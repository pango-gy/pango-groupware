//! 정리 작업 테스트.

use chrono::Duration;
use http::StatusCode;
use pango_server::service::maintenance::{self, Cleanup};
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
