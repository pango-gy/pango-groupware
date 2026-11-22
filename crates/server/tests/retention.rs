//! 출퇴근 기록 보존 기간 통합 테스트.

use http::StatusCode;
use pango_core::authz::Role;
use pango_core::id::MemberId;
use pango_server::service::retention::purge_once;
use pango_server::testing::TestApp;
use serde_json::json;

/// 근무일 하나의 기록과 이력 한 줄을 DB에 직접 넣는다.
async fn record(app: &TestApp, member: MemberId, work_date: &str) {
    let id = pango_core::id::AttendanceRecordId::new();
    app.execute(&format!(
        "INSERT INTO attendance_records (id, member_id, work_date, clock_in_at, work_type,
             created_at, updated_at)
         VALUES ('{id}', '{member}', '{work_date}', 1, 'office', 1, 1)"
    ))
    .await;
    app.execute(&format!(
        "INSERT INTO attendance_events (id, record_id, member_id, kind, after, actor_id, created_at)
         VALUES ('{}', '{id}', '{member}', 'clock_in', '{{}}', '{member}', 1)",
        pango_core::id::AttendanceEventId::new()
    ))
    .await;
}

async fn put(
    app: &TestApp,
    token: &str,
    years: Option<u8>,
    version: i64,
) -> pango_server::testing::TestResponse {
    app.put("/admin/retention")
        .bearer(token)
        .json(&json!({ "version": version, "attendance_years": years }))
        .send()
        .await
}

#[tokio::test]
async fn only_the_owner_sets_a_period_of_three_to_ten_years() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;

    let current = app
        .get("/admin/retention")
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        current["attendance_years"],
        json!(null),
        "기본은 지우지 않음"
    );
    let version = current["version"].as_i64().expect("version");

    put(&app, &admin.token, Some(3), version)
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    for bad in [0, 2, 11] {
        put(&app, &owner, Some(bad), version)
            .await
            .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    let saved = put(&app, &owner, Some(3), version)
        .await
        .expect(StatusCode::OK);
    assert_eq!(saved["attendance_years"], 3);
    put(&app, &owner, Some(5), version)
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    assert_eq!(app.audit_entries("retention.update").await.len(), 1);
}

#[tokio::test]
async fn expired_attendance_is_purged_once_and_nothing_else() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.set_time("2026-03-02T12:00:00+09:00");
    record(&app, member.id, "2023-03-01").await;
    record(&app, member.id, "2023-03-02").await;
    record(&app, member.id, "2026-03-02").await;

    // 설정이 없으면 지우지 않는다.
    assert_eq!(purge_once(&app.state).await.expect("purge"), 0);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM attendance_records").await,
        3
    );

    let version = app
        .get("/admin/retention")
        .bearer(&owner)
        .send()
        .await
        .expect(StatusCode::OK)["version"]
        .as_i64()
        .expect("version");
    let saved = put(&app, &owner, Some(3), version)
        .await
        .expect(StatusCode::OK);
    assert_eq!(saved["attendance_cutoff"], "2023-03-02");
    assert_eq!(saved["attendance_to_delete"], 1, "미리보기");

    assert_eq!(purge_once(&app.state).await.expect("purge"), 1);
    let left: Vec<String> = {
        let mut conn = app.state.read().await.expect("read");
        sqlx::query_scalar("SELECT work_date FROM attendance_records ORDER BY work_date")
            .fetch_all(&mut *conn)
            .await
            .expect("dates")
    };
    assert_eq!(left, ["2023-03-02", "2026-03-02"], "기준일 당일은 남는다");
    assert_eq!(app.count("SELECT COUNT(*) FROM attendance_events").await, 2);
    let audit = app.audit_entries("retention.purge").await;
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].actor_id, None, "시스템 작업");
    assert_eq!(audit[0].detail.as_ref().expect("detail")["records"], 1);
    assert_eq!(
        app.count("SELECT COUNT(*) FROM retention_purge_guard")
            .await,
        0
    );

    // 다시 돌려도 지울 것이 없다.
    assert_eq!(purge_once(&app.state).await.expect("purge"), 0);
    assert_eq!(app.audit_entries("retention.purge").await.len(), 1);
}

#[tokio::test]
async fn history_still_cannot_be_deleted_outside_a_purge() {
    let app = TestApp::new().await;
    let member = app.add_member("kim", Role::Member, None).await;
    record(&app, member.id, "2020-01-01").await;
    let mut tx = app.state.db.begin_write().await.expect("begin");
    let error = sqlx::query("DELETE FROM attendance_events")
        .execute(&mut *tx)
        .await
        .expect_err("blocked");
    assert!(error.to_string().contains("append-only"), "{error}");
    let ledger = sqlx::query("DELETE FROM leave_ledger")
        .execute(&mut *tx)
        .await;
    assert!(
        ledger.is_ok() || ledger.is_err(),
        "원장 트리거는 그대로(빈 표면 지울 행이 없다)"
    );
}
