//! 인증·세션 통합 테스트.

use chrono::Duration;
use http::StatusCode;
use pango_core::member::MemberStatus;
use pango_db::members;
use pango_server::service::setup::{self, InitInput};
use pango_server::testing::{TestApp, OWNER_LOGIN, OWNER_PASSWORD};
use pango_server::Config;
use serde_json::{json, Value};

fn credentials(login_id: &str, password: &str) -> Value {
    json!({ "login_id": login_id, "password": password, "token_in_body": true })
}

async fn try_login(app: &TestApp, login_id: &str, password: &str) -> StatusCode {
    app.post("/auth/login")
        .json(&credentials(login_id, password))
        .send()
        .await
        .status
}

fn session_cookie(response: &pango_server::testing::TestResponse) -> String {
    let set_cookie = response.headers["set-cookie"]
        .to_str()
        .expect("cookie")
        .to_owned();
    set_cookie.split(';').next().expect("pair").to_owned()
}

async fn set_owner_status(app: &TestApp, status: MemberStatus) {
    let mut tx = app.state.db.begin_write().await.expect("begin");
    let mut owner = members::find(&mut tx, app.owner_id)
        .await
        .expect("find")
        .expect("owner");
    owner.status = status;
    assert!(members::update(&mut tx, &owner, app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");
}

// ── 기본 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_needs_no_auth_and_has_security_headers() {
    let app = TestApp::new().await;
    let response = app.get("/health").send().await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.headers["x-content-type-options"], "nosniff");
    assert_eq!(response.headers["x-frame-options"], "DENY");
    assert_eq!(response.headers["cache-control"], "no-store");
    assert!(response.headers.contains_key("content-security-policy"));
}

#[tokio::test]
async fn unknown_route_is_json_404() {
    let app = TestApp::new().await;
    app.get("/nope")
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn login_returns_token_and_profile() {
    let app = TestApp::new().await;
    let body = app
        .post("/auth/login")
        .json(&credentials(OWNER_LOGIN, OWNER_PASSWORD))
        .send()
        .await
        .expect(StatusCode::OK);
    let token = body["token"].as_str().expect("token");
    assert_eq!(token.len(), 64);
    assert_eq!(body["me"]["login_id"], OWNER_LOGIN);
    assert_eq!(body["me"]["role"], "owner");
    assert_eq!(
        body["me"]["capabilities"],
        json!({ "admin": true, "manage_roles": true, "team": false })
    );
    assert_eq!(body["me"]["timezone"], "Asia/Seoul");
    assert!(body["me"].get("password_hash").is_none());

    let me = app
        .get("/me")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(me, body["me"]);
}

#[tokio::test]
async fn login_id_is_case_insensitive() {
    let app = TestApp::new().await;
    assert_eq!(
        try_login(&app, "OWNER", OWNER_PASSWORD).await,
        StatusCode::OK
    );
}

// ── 실패는 구분되지 않는다 ───────────────────────────────────────────────

#[tokio::test]
async fn every_login_failure_looks_the_same() {
    let app = TestApp::new().await;
    let huge = "x".repeat(100_000);
    let cases = [
        (OWNER_LOGIN, "wrong-password-123"),
        ("nobody", OWNER_PASSWORD),
        ("", OWNER_PASSWORD),
        (OWNER_LOGIN, ""),
        ("' OR '1'='1", "' OR '1'='1"),
        ("owner'--", OWNER_PASSWORD),
        ("owner\0", OWNER_PASSWORD),
        ("오너", OWNER_PASSWORD),
        ("nobody", huge.as_str()),
    ];
    let mut bodies = Vec::new();
    for (login_id, password) in cases {
        let response = app
            .post("/auth/login")
            .json(&credentials(login_id, password))
            .send()
            .await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED, "{login_id:?}");
        assert!(!response.headers.contains_key("set-cookie"));
        bodies.push(response.body);
    }
    assert!(
        bodies.windows(2).all(|pair| pair[0] == pair[1]),
        "응답 본문이 모두 같아야 한다"
    );
    assert_eq!(bodies[0]["error"]["code"], "invalid_credentials");
}

#[tokio::test]
async fn inactive_accounts_cannot_login_and_look_like_wrong_password() {
    let app = TestApp::new().await;
    for status in [MemberStatus::Suspended, MemberStatus::Retired] {
        set_owner_status(&app, status).await;
        app.post("/auth/login")
            .json(&credentials(OWNER_LOGIN, OWNER_PASSWORD))
            .send()
            .await
            .expect_error(StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
}

#[tokio::test]
async fn deactivation_invalidates_existing_sessions_immediately() {
    let app = TestApp::new().await;
    let token = app.login_owner().await;
    set_owner_status(&app, MemberStatus::Suspended).await;
    app.get("/me")
        .bearer(&token)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
}

// ── 시도 제한 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn account_locks_after_repeated_failures_even_for_correct_password() {
    let app = TestApp::new().await;
    for _ in 0..5 {
        assert_eq!(
            try_login(&app, OWNER_LOGIN, "wrong-password-123").await,
            StatusCode::UNAUTHORIZED
        );
    }
    let locked = app
        .post("/auth/login")
        .json(&credentials(OWNER_LOGIN, OWNER_PASSWORD))
        .send()
        .await;
    assert_eq!(locked.headers["retry-after"], "900");
    locked.expect_error(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");

    // 다른 계정은 영향받지 않는다.
    assert_eq!(
        try_login(&app, "someone-else", "x").await,
        StatusCode::UNAUTHORIZED
    );

    // 창이 끝나기 직전까지는 잠겨 있고, 끝나면 풀린다.
    app.advance(Duration::minutes(15) - Duration::milliseconds(1));
    assert_eq!(
        try_login(&app, OWNER_LOGIN, OWNER_PASSWORD).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    app.advance(Duration::milliseconds(1));
    assert_eq!(
        try_login(&app, OWNER_LOGIN, OWNER_PASSWORD).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn lockout_boundary_is_exactly_the_limit() {
    let app = TestApp::new().await;
    for _ in 0..4 {
        try_login(&app, OWNER_LOGIN, "wrong-password-123").await;
    }
    assert_eq!(
        try_login(&app, OWNER_LOGIN, OWNER_PASSWORD).await,
        StatusCode::OK,
        "4회 실패까지는 허용"
    );
    // 성공하면 실패 횟수가 초기화된다.
    for _ in 0..4 {
        try_login(&app, OWNER_LOGIN, "wrong-password-123").await;
    }
    assert_eq!(
        try_login(&app, OWNER_LOGIN, OWNER_PASSWORD).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn unknown_accounts_lock_the_same_way_so_existence_is_not_revealed() {
    let app = TestApp::new().await;
    let mut known = Vec::new();
    let mut unknown = Vec::new();
    for _ in 0..7 {
        known.push(try_login(&app, OWNER_LOGIN, "wrong-password-123").await);
        unknown.push(try_login(&app, "ghost", "wrong-password-123").await);
    }
    assert_eq!(known, unknown);
    assert_eq!(known[5], StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn ip_rate_limit_applies_across_accounts() {
    let config = Config {
        trust_proxy: true,
        ..Config::for_tests()
    };
    let app = TestApp::with_config(config).await;
    let attempt = |ip: &'static str, n: usize| {
        let app = &app;
        async move {
            app.post("/auth/login")
                .header("x-forwarded-for", ip)
                .json(&credentials(&format!("user{n}"), "wrong-password-123"))
                .send()
                .await
                .status
        }
    };
    for n in 0..20 {
        assert_eq!(
            attempt("203.0.113.9", n).await,
            StatusCode::UNAUTHORIZED,
            "attempt {n}"
        );
    }
    assert_eq!(
        attempt("203.0.113.9", 20).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        attempt("203.0.113.10", 21).await,
        StatusCode::UNAUTHORIZED,
        "다른 IP는 무관"
    );

    app.advance(Duration::minutes(1));
    assert_eq!(attempt("203.0.113.9", 22).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn forwarded_header_is_ignored_unless_proxy_is_trusted() {
    let app = TestApp::new().await;
    // 신뢰하지 않으면 헤더를 바꿔 가며 제한을 피할 수도, 남의 IP를 막을 수도 없다(IP 없음으로 처리).
    for n in 0..30 {
        let status = app
            .post("/auth/login")
            .header("x-forwarded-for", "203.0.113.9")
            .json(&credentials(&format!("user{n}"), "wrong-password-123"))
            .send()
            .await
            .status;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn spoofed_forwarded_chain_uses_last_hop() {
    let config = Config {
        trust_proxy: true,
        login_ip_limit: 2,
        ..Config::for_tests()
    };
    let app = TestApp::with_config(config).await;
    let attempt = |chain: String| {
        let app = &app;
        async move {
            app.post("/auth/login")
                .header("x-forwarded-for", &chain)
                .json(&credentials("ghost-user", "wrong-password-123"))
                .send()
                .await
                .status
        }
    };
    // 클라이언트가 앞에 가짜 IP를 붙여도, 프록시가 붙인 마지막 값으로 센다.
    assert_eq!(
        attempt("1.1.1.1, 198.51.100.7".into()).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        attempt("2.2.2.2, 198.51.100.7".into()).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        attempt("3.3.3.3, 198.51.100.7".into()).await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

// ── 토큰과 세션 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn requests_without_valid_token_are_unauthenticated() {
    let app = TestApp::new().await;
    app.get("/me")
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    let forged = "a".repeat(64);
    for token in ["garbage", "", forged.as_str(), "../../etc/passwd"] {
        app.get("/me")
            .header("authorization", &format!("Bearer {token}"))
            .send()
            .await
            .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    app.get("/me")
        .header("authorization", "Basic b3duZXI6cGFzcw==")
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
}

#[tokio::test]
async fn logout_revokes_the_session_and_is_not_repeatable() {
    let app = TestApp::new().await;
    let token = app.login_owner().await;
    app.post("/auth/logout")
        .bearer(&token)
        .send()
        .await
        .expect(StatusCode::OK);
    app.get("/me")
        .bearer(&token)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    app.post("/auth/logout")
        .bearer(&token)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
}

#[tokio::test]
async fn idle_sessions_expire_but_activity_extends_them() {
    let app = TestApp::new().await;
    let idle = app.login_owner().await;
    let active = app.login_owner().await;

    for _ in 0..3 {
        app.advance(Duration::days(13));
        app.get("/me")
            .bearer(&active)
            .send()
            .await
            .expect(StatusCode::OK);
    }
    // idle 토큰은 39일 동안 쓰지 않았다.
    app.get("/me")
        .bearer(&idle)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    app.get("/me")
        .bearer(&active)
        .send()
        .await
        .expect(StatusCode::OK);
}

#[tokio::test]
async fn idle_expiry_boundary() {
    let app = TestApp::new().await;
    let token = app.login_owner().await;
    app.advance(Duration::days(14) - Duration::milliseconds(1));
    // 이 조회는 마지막 사용 시각을 갱신한다.
    app.get("/me")
        .bearer(&token)
        .send()
        .await
        .expect(StatusCode::OK);

    let other = app.login_owner().await;
    app.advance(Duration::days(14));
    app.get("/me")
        .bearer(&other)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
}

#[tokio::test]
async fn absolute_expiry_applies_even_to_active_sessions() {
    let app = TestApp::new().await;
    let token = app.login_owner().await;
    for _ in 0..8 {
        app.advance(Duration::days(11));
        app.get("/me")
            .bearer(&token)
            .send()
            .await
            .expect(StatusCode::OK);
    }
    app.advance(Duration::days(2)); // 총 90일
    app.get("/me")
        .bearer(&token)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
}

#[tokio::test]
async fn sessions_can_be_listed_and_revoked_individually() {
    let app = TestApp::new().await;
    let current = app.login_owner().await;
    let other = app.login_owner().await;

    let sessions = app
        .get("/me/sessions")
        .bearer(&current)
        .send()
        .await
        .expect(StatusCode::OK);
    let sessions = sessions.as_array().expect("array");
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions.iter().filter(|s| s["current"] == true).count(), 1);
    let other_id = sessions
        .iter()
        .find(|s| s["current"] == false)
        .expect("other")["id"]
        .as_str()
        .expect("id")
        .to_owned();

    let path = format!("/me/sessions/{other_id}");
    app.delete(&path)
        .bearer(&current)
        .send()
        .await
        .expect(StatusCode::OK);
    app.get("/me")
        .bearer(&other)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    // 다시 끊어도 성공(멱등).
    app.delete(&path)
        .bearer(&current)
        .send()
        .await
        .expect(StatusCode::OK);
    app.get("/me")
        .bearer(&current)
        .send()
        .await
        .expect(StatusCode::OK);
}

#[tokio::test]
async fn revoking_unknown_or_malformed_session_is_not_found() {
    let app = TestApp::new().await;
    let token = app.login_owner().await;
    let unknown = pango_core::id::SessionId::new();
    for id in [
        unknown.to_string(),
        "not-a-uuid".into(),
        "1%20OR%201=1".into(),
    ] {
        app.delete(&format!("/me/sessions/{id}"))
            .bearer(&token)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_logins_each_get_a_distinct_working_session() {
    let app = std::sync::Arc::new(TestApp::new().await);
    let tasks: Vec<_> = (0..12)
        .map(|_| {
            let app = app.clone();
            tokio::spawn(async move { app.login_owner().await })
        })
        .collect();
    let mut tokens = std::collections::HashSet::new();
    for task in tasks {
        tokens.insert(task.await.expect("join"));
    }
    assert_eq!(tokens.len(), 12);
    for token in &tokens {
        app.get("/me")
            .bearer(token)
            .send()
            .await
            .expect(StatusCode::OK);
    }
}

// ── 쿠키와 CSRF ──────────────────────────────────────────────────────────

#[tokio::test]
async fn cookie_login_sets_hardened_cookie_and_hides_token() {
    let app = TestApp::new().await;
    let response = app
        .post("/auth/login")
        .json(&json!({ "login_id": OWNER_LOGIN, "password": OWNER_PASSWORD }))
        .send()
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert!(
        response.body.get("token").is_none(),
        "쿠키 방식에서는 본문에 토큰을 싣지 않는다"
    );
    let set_cookie = response.headers["set-cookie"].to_str().expect("cookie");
    for attribute in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age=7776000"] {
        assert!(
            set_cookie.contains(attribute),
            "{attribute} missing in {set_cookie}"
        );
    }

    let cookie = session_cookie(&response);
    app.get("/me")
        .header("cookie", &cookie)
        .send()
        .await
        .expect(StatusCode::OK);
}

#[tokio::test]
async fn secure_config_uses_host_prefixed_secure_cookie() {
    let config = Config {
        cookie_secure: true,
        ..Config::for_tests()
    };
    let app = TestApp::with_config(config).await;
    let response = app
        .post("/auth/login")
        .json(&json!({ "login_id": OWNER_LOGIN, "password": OWNER_PASSWORD }))
        .send()
        .await;
    let set_cookie = response.headers["set-cookie"].to_str().expect("cookie");
    assert!(set_cookie.starts_with("__Host-pango_session="));
    assert!(set_cookie.contains("; Secure"));
}

#[tokio::test]
async fn cookie_authenticated_writes_require_same_origin() {
    let app = TestApp::new().await;
    let login = app
        .post("/auth/login")
        .json(&json!({ "login_id": OWNER_LOGIN, "password": OWNER_PASSWORD }))
        .send()
        .await;
    let cookie = session_cookie(&login);
    let logout = |headers: Vec<(&'static str, &'static str)>| {
        let (app, cookie) = (&app, cookie.clone());
        async move {
            let mut request = app.post("/auth/logout").header("cookie", &cookie);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            request.send().await
        }
    };

    logout(vec![])
        .await
        .expect_error(StatusCode::FORBIDDEN, "cross_origin_rejected");
    logout(vec![
        ("origin", "https://evil.example"),
        ("host", "erp.example.com"),
    ])
    .await
    .expect_error(StatusCode::FORBIDDEN, "cross_origin_rejected");
    logout(vec![("sec-fetch-site", "cross-site")])
        .await
        .expect_error(StatusCode::FORBIDDEN, "cross_origin_rejected");
    // 거부된 요청은 아무것도 바꾸지 않았다.
    app.get("/me")
        .header("cookie", &cookie)
        .send()
        .await
        .expect(StatusCode::OK);

    logout(vec![
        ("origin", "https://erp.example.com"),
        ("host", "erp.example.com"),
    ])
    .await
    .expect(StatusCode::OK);
    app.get("/me")
        .header("cookie", &cookie)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
}

#[tokio::test]
async fn bearer_requests_are_not_subject_to_origin_checks() {
    let app = TestApp::new().await;
    let token = app.login_owner().await;
    app.post("/auth/logout")
        .bearer(&token)
        .send()
        .await
        .expect(StatusCode::OK);
}

#[tokio::test]
async fn cross_site_login_is_rejected() {
    let app = TestApp::new().await;
    app.post("/auth/login")
        .header("sec-fetch-site", "cross-site")
        .json(&credentials(OWNER_LOGIN, OWNER_PASSWORD))
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "cross_origin_rejected");
}

// ── 비밀번호 변경 ────────────────────────────────────────────────────────

fn change(current: &str, new: &str) -> Value {
    json!({ "current_password": current, "new_password": new })
}

#[tokio::test]
async fn changing_password_revokes_other_sessions_and_old_password() {
    let app = TestApp::new().await;
    let current = app.login_owner().await;
    let other = app.login_owner().await;
    let new_password = "brand-new-password-456";

    app.post("/me/password")
        .bearer(&current)
        .json(&change(OWNER_PASSWORD, new_password))
        .send()
        .await
        .expect(StatusCode::OK);

    app.get("/me")
        .bearer(&current)
        .send()
        .await
        .expect(StatusCode::OK);
    app.get("/me")
        .bearer(&other)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    assert_eq!(
        try_login(&app, OWNER_LOGIN, OWNER_PASSWORD).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        try_login(&app, OWNER_LOGIN, new_password).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn password_change_validations() {
    let app = TestApp::new().await;
    let token = app.login_owner().await;
    let attempt = |body: Value| {
        let (app, token) = (&app, token.clone());
        async move {
            app.post("/me/password")
                .bearer(&token)
                .json(&body)
                .send()
                .await
        }
    };

    let wrong = attempt(change("not-my-password", "brand-new-password-456")).await;
    assert_eq!(wrong.body["error"]["field"], "current_password");
    wrong.expect_error(StatusCode::BAD_REQUEST, "validation_failed");

    for weak in ["short", "aaaaaaaaaaaa", "my-owner-password", ""] {
        let response = attempt(change(OWNER_PASSWORD, weak)).await;
        assert_eq!(response.body["error"]["field"], "new_password", "{weak:?}");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }

    attempt(change(OWNER_PASSWORD, OWNER_PASSWORD))
        .await
        .expect_error(StatusCode::CONFLICT, "password_reused");

    // 실패한 시도는 아무것도 바꾸지 않았다.
    assert_eq!(
        try_login(&app, OWNER_LOGIN, OWNER_PASSWORD).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn temporary_password_blocks_everything_until_changed() {
    let app = TestApp::uninitialized(Config::for_tests()).await;
    let output = setup::initialize(
        &app.state,
        InitInput {
            company_name: "팡고".into(),
            timezone: "Asia/Seoul".into(),
            owner_login_id: "owner".into(),
            owner_name: "대표".into(),
            owner_password: None,
        },
    )
    .await
    .expect("initialize");
    let temporary = output.temporary_password.expect("temporary password");
    let token = app.login("owner", &temporary).await;

    let me = app
        .get("/me")
        .bearer(&token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(me["must_change_password"], true);
    app.get("/me/sessions")
        .bearer(&token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "password_change_required");

    app.post("/me/password")
        .bearer(&token)
        .json(&change(&temporary, "my-real-password-789"))
        .send()
        .await
        .expect(StatusCode::OK);
    let me = app
        .get("/me")
        .bearer(&token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(me["must_change_password"], false);
    app.get("/me/sessions")
        .bearer(&token)
        .send()
        .await
        .expect(StatusCode::OK);
}

// ── 초기화 ───────────────────────────────────────────────────────────────

fn init_input() -> InitInput {
    InitInput {
        company_name: "팡고".into(),
        timezone: "Asia/Seoul".into(),
        owner_login_id: "second".into(),
        owner_name: "두번째".into(),
        owner_password: Some("valid-password-123".into()),
    }
}

#[tokio::test]
async fn initialization_runs_once() {
    let app = TestApp::new().await;
    assert!(setup::is_initialized(&app.state).await.expect("check"));
    let error = setup::initialize(&app.state, init_input())
        .await
        .err()
        .expect("must fail");
    assert_eq!(error.code(), "already_exists");
    assert_eq!(
        try_login(&app, "second", "valid-password-123").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn initialization_validates_input_without_side_effects() {
    let app = TestApp::uninitialized(Config::for_tests()).await;
    let bad_inputs = [
        InitInput {
            timezone: "Mars/Olympus".into(),
            ..init_input()
        },
        InitInput {
            owner_login_id: "x".into(),
            ..init_input()
        },
        InitInput {
            owner_password: Some("short".into()),
            ..init_input()
        },
        InitInput {
            company_name: "  ".into(),
            ..init_input()
        },
        InitInput {
            owner_name: "a\u{202E}b".into(),
            ..init_input()
        },
    ];
    for input in bad_inputs {
        let error = setup::initialize(&app.state, input)
            .await
            .err()
            .expect("must fail");
        assert_eq!(error.code(), "validation_failed");
    }
    assert!(!setup::is_initialized(&app.state).await.expect("check"));
    setup::initialize(&app.state, init_input())
        .await
        .expect("valid input still works");
}

#[tokio::test]
async fn uninitialized_server_rejects_logins() {
    let app = TestApp::uninitialized(Config::for_tests()).await;
    assert_eq!(
        try_login(&app, OWNER_LOGIN, OWNER_PASSWORD).await,
        StatusCode::UNAUTHORIZED
    );
}

// ── 기형 요청 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn malformed_bodies_are_rejected_with_specific_codes() {
    let app = TestApp::new().await;
    let post = |content_type: &'static str, body: Vec<u8>| {
        let app = &app;
        async move {
            app.post("/auth/login")
                .raw_body(content_type, body)
                .send()
                .await
        }
    };

    post("application/json", b"{not json".to_vec())
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    post("application/json", b"[]".to_vec())
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    post("application/json", b"{}".to_vec())
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    post("application/json", vec![0xff, 0xfe, 0x00])
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    post(
        "application/json",
        br#"{"login_id":1,"password":["a"]}"#.to_vec(),
    )
    .await
    .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    post(
        "text/plain",
        br#"{"login_id":"owner","password":"x"}"#.to_vec(),
    )
    .await
    .expect_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type");
    post(
        "application/x-www-form-urlencoded",
        b"login_id=owner&password=x".to_vec(),
    )
    .await
    .expect_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type");

    let nested = format!("{}1{}", "[".repeat(50_000), "]".repeat(50_000));
    post("application/json", nested.into_bytes())
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
}

#[tokio::test]
async fn oversized_body_is_rejected() {
    let app = TestApp::new().await;
    let big = json!({ "login_id": "owner", "password": "x".repeat(300 * 1024) });
    app.post("/auth/login")
        .json(&big)
        .send()
        .await
        .expect_error(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large");
}

#[tokio::test]
async fn audit_log_records_logins_without_secrets() {
    let app = TestApp::new().await;
    let token = app.login_owner().await;
    let mut conn = app.state.db.read().await.expect("conn");
    let (entries, _) = pango_db::audit::list(&mut conn, &Default::default(), 50)
        .await
        .expect("list");
    let actions: Vec<_> = entries.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, vec!["auth.login", "system.initialize"]);
    let dump = format!("{entries:?}");
    assert!(!dump.contains(&token) && !dump.contains(OWNER_PASSWORD));
}
