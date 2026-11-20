//! 정리 작업과 하루 한 번의 연차 자동 부여 테스트.

use chrono::Duration;
use http::StatusCode;
use pango_api::leave::AccrualRunResult;
use pango_core::authz::Role;
use pango_core::id::MemberId;
use pango_core::member::MemberStatus;
use pango_db::audit::{self, AuditFilter};
use pango_db::members;
use pango_server::config::Config;
use pango_server::service::maintenance::{self, Cleanup, DailyAccrual};
use pango_server::testing::{TestApp, OWNER_LOGIN, OWNER_PASSWORD};
use serde_json::json;

async fn failed_login(app: &TestApp) -> StatusCode {
    let body =
        json!({ "login_id": OWNER_LOGIN, "password": "wrong-password-123", "token_in_body": true });
    app.post("/auth/login").json(&body).send().await.status
}

#[tokio::test]
async fn nothing_to_clean_on_a_fresh_server() {
    let app = TestApp::new().await;
    assert_eq!(
        maintenance::run_once(&app.state).await.expect("run"),
        Cleanup::default()
    );
}

#[tokio::test]
async fn active_sessions_survive_and_long_dead_ones_are_removed() {
    let app = TestApp::new().await;
    let revoked = app.login_owner().await;
    app.post("/auth/logout")
        .bearer(&revoked)
        .send()
        .await
        .expect(StatusCode::OK);

    app.advance(Duration::days(29));
    let active = app.login_owner().await;
    assert_eq!(
        maintenance::run_once(&app.state)
            .await
            .expect("run")
            .sessions,
        0,
        "보관 기간 안"
    );

    app.advance(Duration::days(2));
    assert_eq!(
        maintenance::run_once(&app.state)
            .await
            .expect("run")
            .sessions,
        1
    );
    app.get("/me")
        .bearer(&active)
        .send()
        .await
        .expect(StatusCode::OK);
    // 다시 실행해도 더 지울 것이 없다.
    assert_eq!(
        maintenance::run_once(&app.state)
            .await
            .expect("run")
            .sessions,
        0
    );
}

#[tokio::test]
async fn cleanup_never_unlocks_an_account_early() {
    let app = TestApp::new().await;
    for _ in 0..5 {
        assert_eq!(failed_login(&app).await, StatusCode::UNAUTHORIZED);
    }
    app.advance(Duration::minutes(14));
    maintenance::run_once(&app.state).await.expect("run");
    assert_eq!(
        failed_login(&app).await,
        StatusCode::TOO_MANY_REQUESTS,
        "잠금 창 안에서는 기록이 남아야 한다"
    );

    // 한도를 넘긴 시도(14분 시점)부터 15분 동안 잠긴다.
    app.advance(Duration::minutes(14));
    maintenance::run_once(&app.state).await.expect("run");
    assert_eq!(
        failed_login(&app).await,
        StatusCode::TOO_MANY_REQUESTS,
        "28분: 아직 잠금"
    );

    app.advance(Duration::minutes(2));
    assert_eq!(
        maintenance::run_once(&app.state)
            .await
            .expect("run")
            .throttle_buckets,
        1
    );
    assert_eq!(failed_login(&app).await, StatusCode::UNAUTHORIZED);
    let ok = json!({ "login_id": OWNER_LOGIN, "password": OWNER_PASSWORD, "token_in_body": true });
    app.post("/auth/login")
        .json(&ok)
        .send()
        .await
        .expect(StatusCode::OK);
}

// ── 하루 한 번의 연차 자동 부여 ─────────────────────────────────────────

async fn accrual_audits(app: &TestApp) -> Vec<audit::AuditEntry> {
    let mut conn = app.state.db.read().await.expect("conn");
    let filter = AuditFilter {
        action: Some("leave.accrual_run".to_owned()),
        ..AuditFilter::default()
    };
    audit::list(&mut conn, &filter, 200).await.expect("audit").0
}

async fn ledger_rows(app: &TestApp) -> i64 {
    app.count("SELECT COUNT(*) FROM leave_ledger").await
}

/// kim(입사 2024-03-02)과 newbie(입사 2026-02-10).
async fn accrual_members(app: &TestApp) -> (MemberId, MemberId) {
    let kim = app.add_member("kim", Role::Member, None).await;
    let newbie = app.add_member("newbie", Role::Member, None).await;
    let mut tx = app.state.db.begin_write().await.expect("begin");
    let mut member = members::find(&mut tx, newbie.id)
        .await
        .expect("find")
        .expect("member");
    member.hired_on = "2026-02-10".parse().expect("date");
    assert!(members::update(&mut tx, &member, app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");
    (kim.id, newbie.id)
}

#[tokio::test]
async fn daily_accrual_runs_once_per_company_day() {
    let app = TestApp::new().await;
    let (kim, newbie) = accrual_members(&app).await;
    let mut accrual = DailyAccrual::default();

    // 시작 직후 한 번: kim의 월 부여 11건 + 1·2주년 부여 2건.
    let first = accrual.run_if_due(&app.state).await.expect("run");
    assert_eq!(
        first,
        Some(AccrualRunResult {
            members: 3,
            grants_inserted: 13
        })
    );
    assert_eq!(app.grant_keys(kim).await.len(), 13);
    assert!(app.grant_keys(newbie).await.is_empty());
    let audits = accrual_audits(&app).await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor_id, None, "시스템 작업");

    // 같은 날에는 몇 번을 불러도 다시 실행하지 않는다.
    for hours in [0, 1, 13] {
        app.advance(Duration::hours(hours));
        assert_eq!(accrual.run_if_due(&app.state).await.expect("run"), None);
    }
    assert_eq!(ledger_rows(&app).await, 13);

    // 회사 시간대(KST)의 날짜가 바뀌면 한 번 실행한다. UTC로는 아직 03-02 15:00이다.
    app.set_time("2026-03-03T00:00:01+09:00");
    let next_day = accrual.run_if_due(&app.state).await.expect("run");
    assert_eq!(
        next_day,
        Some(AccrualRunResult {
            members: 3,
            grants_inserted: 0
        })
    );
    assert_eq!(accrual.run_if_due(&app.state).await.expect("run"), None);
    assert_eq!(
        ledger_rows(&app).await,
        13,
        "반복 실행은 아무것도 넣지 않는다"
    );
    assert_eq!(
        accrual_audits(&app).await.len(),
        1,
        "바뀐 것이 없는 날은 감사 로그도 남기지 않는다"
    );
}

#[tokio::test]
async fn daily_accrual_picks_up_new_grants_as_days_pass() {
    let app = TestApp::new().await;
    let (kim, newbie) = accrual_members(&app).await;
    let mut accrual = DailyAccrual::default();
    accrual.run_if_due(&app.state).await.expect("run");

    // 03-09까지는 newbie에게 부여할 것이 없다.
    app.set_time("2026-03-09T23:59:59+09:00");
    let none = accrual.run_if_due(&app.state).await.expect("run");
    assert_eq!(none.map(|result| result.grants_inserted), Some(0));
    // 03-10: 입사 한 달째의 월 부여 1건.
    app.set_time("2026-03-10T00:00:00+09:00");
    let monthly = accrual.run_if_due(&app.state).await.expect("run");
    assert_eq!(monthly.map(|result| result.grants_inserted), Some(1));
    assert_eq!(app.grant_keys(newbie).await, ["monthly:1"]);
    assert_eq!(accrual_audits(&app).await.len(), 2);

    // 서버를 다시 띄우면(새 상태) 같은 날 한 번 더 돌지만 아무것도 바꾸지 않는다.
    let rows = ledger_rows(&app).await;
    let restarted = DailyAccrual::default()
        .run_if_due(&app.state)
        .await
        .expect("run");
    assert_eq!(restarted.map(|result| result.grants_inserted), Some(0));
    assert_eq!(ledger_rows(&app).await, rows);

    // 1년 뒤 kim의 3주년: 며칠을 건너뛰어도 그사이의 부여가 모두 들어간다.
    app.set_time("2027-03-02T00:30:00+09:00");
    let anniversary = accrual.run_if_due(&app.state).await.expect("run");
    assert!(anniversary.is_some_and(|result| result.grants_inserted >= 2));
    let keys = app.grant_keys(kim).await;
    assert_eq!(keys.len(), 14);
    assert!(keys.contains(&"annual:3".to_owned()));
    let newbie_keys = app.grant_keys(newbie).await;
    assert!(newbie_keys.contains(&"monthly:11".to_owned()));
    assert!(newbie_keys.contains(&"annual:1".to_owned()));
    // 잔여 조회가 보는 값과 같다(조회가 새로 넣을 부여가 남아 있지 않다).
    let rows = ledger_rows(&app).await;
    let token = app
        .login("kim", pango_server::testing::MEMBER_PASSWORD)
        .await;
    app.get("/leave/balance")
        .bearer(&token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(ledger_rows(&app).await, rows);
}

#[tokio::test]
async fn daily_accrual_skips_inactive_members_and_an_uninitialized_server() {
    let app = TestApp::new().await;
    let (kim, _) = accrual_members(&app).await;
    let mut tx = app.state.db.begin_write().await.expect("begin");
    let mut member = members::find(&mut tx, kim)
        .await
        .expect("find")
        .expect("member");
    member.status = MemberStatus::Suspended;
    assert!(members::update(&mut tx, &member, app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");
    let result = DailyAccrual::default()
        .run_if_due(&app.state)
        .await
        .expect("run");
    assert_eq!(
        result,
        Some(AccrualRunResult {
            members: 2,
            grants_inserted: 0
        })
    );
    assert!(app.grant_keys(kim).await.is_empty());

    // 초기화 전에는 회사 시간대를 모르므로 아무것도 하지 않고, 다음 주기에 다시 본다.
    let blank = TestApp::uninitialized(Config::for_tests()).await;
    let mut accrual = DailyAccrual::default();
    assert_eq!(accrual.run_if_due(&blank.state).await.expect("run"), None);
    assert_eq!(accrual.run_if_due(&blank.state).await.expect("run"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_daily_accrual_runs_insert_each_grant_once() {
    let app = std::sync::Arc::new(TestApp::new().await);
    let (kim, _) = accrual_members(&app).await;
    let tasks: Vec<_> = (0..6)
        .map(|_| {
            let app = app.clone();
            tokio::spawn(async move {
                DailyAccrual::default()
                    .run_if_due(&app.state)
                    .await
                    .expect("run")
                    .expect("due")
                    .grants_inserted
            })
        })
        .collect();
    let mut inserted = 0;
    for task in tasks {
        inserted += task.await.expect("join");
    }
    assert_eq!(inserted, 13);
    let keys = app.grant_keys(kim).await;
    let unique: std::collections::BTreeSet<&String> = keys.iter().collect();
    assert_eq!((keys.len(), unique.len()), (13, 13));
    assert_eq!(accrual_audits(&app).await.len(), 1);
}
