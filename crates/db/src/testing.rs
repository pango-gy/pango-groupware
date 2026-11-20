//! 테스트용 임시 데이터베이스.

use crate::pool::{Db, DbOptions};

/// 임시 디렉터리에 만든 실제 SQLite 파일. 값이 사라지면 파일도 지워진다.
///
/// 메모리 DB가 아니라 파일을 쓰는 이유: WAL, 읽기/쓰기 연결 분리 등 운영과 같은 조건에서
/// 동시성 테스트를 하기 위해서다.
pub struct TestDb {
    pub db: Db,
    dir: tempfile::TempDir,
}

impl TestDb {
    pub async fn new() -> Self {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db = Db::open(&dir.path().join("test.db"), DbOptions::default())
            .await
            .expect("open test database");
        Self { db, dir }
    }

    pub fn path(&self) -> std::path::PathBuf {
        self.dir.path().join("test.db")
    }
}

impl std::ops::Deref for TestDb {
    type Target = Db;

    fn deref(&self) -> &Db {
        &self.db
    }
}
