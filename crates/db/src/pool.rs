//! 연결 풀. 쓰기 연결은 하나, 읽기 연결은 여러 개.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};

use crate::error::DbResult;

pub type Conn = sqlx::SqliteConnection;
pub type WriteTx = sqlx::Transaction<'static, sqlx::Sqlite>;
pub type ReadConn = sqlx::pool::PoolConnection<sqlx::Sqlite>;
/// 읽기 연결 위의 읽기 전용 트랜잭션. 버리면 끝난다(쓴 것이 없으므로 롤백과 같다).
pub type ReadSnapshot = sqlx::Transaction<'static, sqlx::Sqlite>;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

#[derive(Debug, Clone)]
pub struct DbOptions {
    pub read_connections: u32,
    /// 쓰기 연결을 기다리는 최대 시간.
    pub write_wait: Duration,
}

impl Default for DbOptions {
    fn default() -> Self {
        Self {
            read_connections: 4,
            write_wait: Duration::from_secs(30),
        }
    }
}

/// 데이터베이스 핸들. 복제는 풀을 공유한다.
#[derive(Debug, Clone)]
pub struct Db {
    reader: SqlitePool,
    writer: SqlitePool,
}

impl Db {
    /// DB 파일을 열고(없으면 만들고) 마이그레이션을 적용한다.
    pub async fn open(path: &Path, options: DbOptions) -> DbResult<Self> {
        let connect = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));

        // 쓰기 연결이 하나뿐이므로 쓰기 트랜잭션은 앱 안에서 직렬화된다.
        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .acquire_timeout(options.write_wait)
            .connect_with(connect.clone())
            .await?;
        MIGRATOR.run(&writer).await?;

        let reader = SqlitePoolOptions::new()
            .max_connections(options.read_connections.max(1))
            .connect_with(connect.read_only(true))
            .await?;
        Ok(Self { reader, writer })
    }

    /// 읽기 전용 조회에 쓰는 풀.
    pub fn reader(&self) -> &SqlitePool {
        &self.reader
    }

    /// 읽기 전용 연결 하나를 빌린다.
    pub async fn read(&self) -> DbResult<ReadConn> {
        Ok(self.reader.acquire().await?)
    }

    /// 읽기 전용 트랜잭션을 시작한다. 그 안의 모든 조회는 **같은 시점**의 데이터를 본다(WAL).
    ///
    /// 조회 여러 번으로 한 결과를 만드는 긴 읽기(내보내기, 전원 집계)에 쓴다. 쓰기 연결을
    /// 쓰지 않으므로 다른 쓰기를 세워 두지 않으면서, 조회 사이에 커밋된 변경이 섞이지 않는다.
    pub async fn read_snapshot(&self) -> DbResult<ReadSnapshot> {
        Ok(self.reader.begin().await?)
    }

    /// 쓰기 트랜잭션을 시작한다. 커밋하지 않고 버리면 롤백된다.
    pub async fn begin_write(&self) -> DbResult<WriteTx> {
        Ok(self.writer.begin().await?)
    }

    pub async fn close(&self) {
        self.reader.close().await;
        self.writer.close().await;
    }
}
