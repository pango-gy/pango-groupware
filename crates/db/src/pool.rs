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

    /// 읽기 풀이 DB에 닿는지 가볍게 확인한다(헬스체크용). 쓰기 락과 경합하지 않는다.
    pub async fn ping(&self) -> DbResult<()> {
        let mut conn = self.reader.acquire().await?;
        sqlx::query("SELECT 1").execute(&mut *conn).await?;
        Ok(())
    }

    pub async fn close(&self) {
        self.reader.close().await;
        self.writer.close().await;
    }
}

/// `source` DB 파일의 같은 시점 사본을 `dest`에 만든다(`VACUUM INTO`). 서버가 도는 중에도 되고
/// 쓰기를 막지 않는다.
///
/// - 마이그레이션을 돌리지 않는 읽기 전용 연결 하나만 쓴다. 판올림 전에 새 실행 파일로 백업해도
///   운영 중인 DB를 바꾸지 않고, 예전 실행 파일로도 더 새 DB를 백업할 수 있다.
/// - 사본에는 개인정보·첨부파일·2단계 인증 비밀키가 모두 들어 있으므로, 유닉스에서는 처음부터
///   소유자만 읽고 쓸 수 있는(0600) 빈 파일을 만든 뒤 그 안에 쓴다. `dest`가 이미 있으면 덮어쓰지
///   않고 실패한다.
pub async fn backup(source: &Path, dest: &Path) -> DbResult<()> {
    let io =
        |kind, message: &str| crate::DbError::Io(std::io::Error::new(kind, message.to_owned()));
    let target = dest.to_str().ok_or_else(|| {
        io(
            std::io::ErrorKind::InvalidInput,
            "backup path is not valid UTF-8",
        )
    })?;
    if !source.exists() {
        return Err(io(std::io::ErrorKind::NotFound, "database file not found"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // 이미 있으면 `AlreadyExists`. SQLite의 `VACUUM INTO`는 빈 파일에는 쓸 수 있다.
    options.open(dest).map_err(crate::DbError::Io)?;
    let connect = SqliteConnectOptions::new()
        .filename(source)
        .read_only(true)
        .busy_timeout(Duration::from_secs(5));
    let result = async {
        let mut conn = sqlx::ConnectOptions::connect(&connect).await?;
        sqlx::query("VACUUM INTO ?")
            .bind(target)
            .execute(&mut conn)
            .await?;
        sqlx::Connection::close(conn).await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;
    if let Err(error) = result {
        // 반쯤 쓴 사본을 남기지 않는다.
        let _ = std::fs::remove_file(dest);
        return Err(error.into());
    }
    Ok(())
}
