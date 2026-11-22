//! `Db::backup_into`: 운영 중 일관된 사본 만들기.

use pango_db::testing::TestDb;
use pango_db::{Db, DbOptions};

#[tokio::test]
async fn backup_is_a_complete_copy_and_never_overwrites() {
    let source = TestDb::new().await;
    {
        let mut tx = source.begin_write().await.expect("begin");
        sqlx::query(
            "INSERT INTO buzz_settings (id, updated_at) VALUES (1, 1) ON CONFLICT DO NOTHING",
        )
        .execute(&mut *tx)
        .await
        .expect("insert");
        sqlx::query("UPDATE buzz_settings SET enabled = 1")
            .execute(&mut *tx)
            .await
            .expect("update");
        tx.commit().await.expect("commit");
    }
    let dir = tempfile::tempdir().expect("dir");
    let dest = dir.path().join("copy.db");
    pango_db::backup(&source.path(), &dest)
        .await
        .expect("backup");

    let copy = Db::open(&dest, DbOptions::default())
        .await
        .expect("open copy");
    let enabled: bool = sqlx::query_scalar("SELECT enabled FROM buzz_settings")
        .fetch_one(copy.reader())
        .await
        .expect("read");
    assert!(enabled, "커밋된 변경이 사본에 있다");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dest).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "소유자만 읽을 수 있다");
    }

    // 있는 파일은 덮어쓰지 않는다.
    let error = pango_db::backup(&source.path(), &dest)
        .await
        .expect_err("exists");
    assert!(error.to_string().contains("exists"), "{error}");
    assert!(dest.exists(), "있던 사본은 그대로");
}

#[tokio::test]
async fn backup_never_migrates_the_source() {
    let source = TestDb::new().await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(source.reader())
        .await
        .expect("count");
    // 마지막 마이그레이션 기록을 지워 "예전 판"의 DB처럼 만든다.
    {
        let mut tx = source.begin_write().await.expect("begin");
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = (SELECT MAX(version) FROM _sqlx_migrations)")
            .execute(&mut *tx)
            .await
            .expect("delete");
        tx.commit().await.expect("commit");
    }
    let dir = tempfile::tempdir().expect("dir");
    pango_db::backup(&source.path(), &dir.path().join("copy.db"))
        .await
        .expect("backup");
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
        .fetch_one(source.reader())
        .await
        .expect("count");
    assert_eq!(after, before - 1, "백업이 마이그레이션을 돌리지 않았다");

    let missing = pango_db::backup(&dir.path().join("none.db"), &dir.path().join("x.db")).await;
    assert!(missing.is_err());
    assert!(
        !dir.path().join("x.db").exists(),
        "실패하면 빈 사본을 남기지 않는다"
    );
}
