//! 원격 MCP와 OAuth 인가 서버 통합 테스트. claude.ai 커넥터와 같은 순서로 부른다:
//! 발견 → 동적 등록 → 동의(로그인) → 토큰 → `/mcp`.

use base64::Engine as _;
use http::{Method, StatusCode};
use pango_core::authz::Role;
use pango_server::config::{Config, McpConfig};
use pango_server::testing::{TestApp, TestMember, TestResponse, MEMBER_PASSWORD};
use serde_json::{json, Value};
use sha2::Digest as _;

const ISSUER: &str = "https://gw.example.com";
const CALLBACK: &str = "https://claude.ai/api/mcp/auth_callback";
const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk-verifier-for-tests";

async fn app() -> TestApp {
    TestApp::with_config(Config {
        mcp: Some(McpConfig::from_public_origin(Some(ISSUER), false).expect("mcp")),
        ..Config::for_tests()
    })
    .await
}

fn challenge() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(VERIFIER))
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

async fn register(app: &TestApp) -> String {
    let created = app
        .root(Method::POST, "/oauth/register")
        .json(&json!({ "client_name": "Claude", "redirect_uris": [CALLBACK], "token_endpoint_auth_method": "none" }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    created["client_id"].as_str().expect("client_id").to_owned()
}

/// 동의 화면을 연다. `(요청 ID, nonce 쿠키)`.
async fn open_consent(app: &TestApp, client_id: &str, scope: &str) -> (String, String) {
    let query = form(&[
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", CALLBACK),
        ("scope", scope),
        ("state", "st-1"),
        ("code_challenge", &challenge()),
        ("code_challenge_method", "S256"),
        ("resource", &format!("{ISSUER}/mcp")),
    ]);
    let page = app
        .root(Method::GET, &format!("/oauth/authorize?{query}"))
        .send()
        .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert_eq!(page.headers["referrer-policy"], "same-origin");
    let html = page.body.as_str().expect("html").to_owned();
    let request_id = html
        .split("name=\"request_id\" value=\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("request id")
        .to_owned();
    let cookie = page.headers["set-cookie"]
        .to_str()
        .expect("cookie")
        .to_owned();
    assert!(
        cookie.contains("Path=/oauth/authorize")
            && cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Lax")
    );
    let nonce = cookie
        .split(';')
        .next()
        .and_then(|pair| pair.split_once('='))
        .map(|(_, v)| v.to_owned())
        .expect("nonce");
    (request_id, nonce)
}

async fn decide(
    app: &TestApp,
    request_id: &str,
    nonce: &str,
    fields: &[(&str, &str)],
) -> TestResponse {
    let mut pairs = vec![("request_id", request_id)];
    pairs.extend_from_slice(fields);
    app.root(Method::POST, "/oauth/authorize")
        .header("origin", ISSUER)
        .header("cookie", &format!("pango_oauth_nonce={nonce}"))
        .raw_body("application/x-www-form-urlencoded", form(&pairs))
        .send()
        .await
}

fn query_of(location: &str) -> std::collections::BTreeMap<String, String> {
    location
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.replace("%3A", ":").replace("%2F", "/")))
        .collect()
}

async fn token(app: &TestApp, fields: &[(&str, &str)]) -> TestResponse {
    app.root(Method::POST, "/oauth/token")
        .raw_body("application/x-www-form-urlencoded", form(fields))
        .send()
        .await
}

/// 승인부터 토큰까지. `(접근 토큰, 갱신 토큰, 받은 scope)`.
async fn connect(app: &TestApp, member: &TestMember, scope: &str) -> (String, String, String) {
    let client_id = register(app).await;
    let (request_id, nonce) = open_consent(app, &client_id, scope).await;
    let approved = decide(
        app,
        &request_id,
        &nonce,
        &[
            ("action", "approve"),
            ("login_id", &member.login_id),
            ("password", MEMBER_PASSWORD),
        ],
    )
    .await;
    assert_eq!(approved.status, StatusCode::SEE_OTHER, "{}", approved.body);
    let location = approved.headers["location"].to_str().expect("location");
    assert!(location.starts_with(CALLBACK));
    let params = query_of(location);
    assert_eq!(params["state"], "st-1");
    assert_eq!(params["iss"], ISSUER);
    let tokens = token(
        app,
        &[
            ("grant_type", "authorization_code"),
            ("code", &params["code"]),
            ("redirect_uri", CALLBACK),
            ("client_id", &client_id),
            ("code_verifier", VERIFIER),
            ("resource", &format!("{ISSUER}/mcp")),
        ],
    )
    .await
    .expect(StatusCode::OK);
    (
        tokens["access_token"].as_str().expect("access").to_owned(),
        tokens["refresh_token"]
            .as_str()
            .expect("refresh")
            .to_owned(),
        tokens["scope"].as_str().expect("scope").to_owned(),
    )
}

async fn rpc(app: &TestApp, access: &str, method: &str, params: Value) -> TestResponse {
    app.root(Method::POST, "/mcp")
        .bearer(access)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .send()
        .await
}

async fn call(app: &TestApp, access: &str, name: &str, arguments: Value) -> TestResponse {
    rpc(
        app,
        access,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
    .await
}

#[tokio::test]
async fn discovery_and_the_401_challenge_point_clients_to_oauth() {
    let app = app().await;
    let unauthorized = app
        .root(Method::POST, "/mcp")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }))
        .send()
        .await;
    assert_eq!(unauthorized.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthorized.headers["www-authenticate"],
        format!("Bearer resource_metadata=\"{ISSUER}/.well-known/oauth-protected-resource\", scope=\"groupware:read groupware:write groupware:admin\"")
    );
    for path in [
        "/.well-known/oauth-protected-resource",
        "/.well-known/oauth-protected-resource/mcp",
    ] {
        let meta = app
            .root(Method::GET, path)
            .send()
            .await
            .expect(StatusCode::OK);
        assert_eq!(meta["resource"], format!("{ISSUER}/mcp"));
        assert_eq!(meta["authorization_servers"], json!([ISSUER]));
    }
    let server = app
        .root(Method::GET, "/.well-known/oauth-authorization-server")
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(server["issuer"], ISSUER);
    assert_eq!(server["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(
        server["registration_endpoint"],
        format!("{ISSUER}/oauth/register")
    );
    assert_eq!(
        server["authorization_response_iss_parameter_supported"],
        true
    );

    app.root(Method::GET, "/mcp")
        .send()
        .await
        .expect(StatusCode::METHOD_NOT_ALLOWED);
    // 허용하지 않은 출처의 브라우저 요청은 막는다.
    app.root(Method::POST, "/mcp")
        .header("origin", "https://evil.example")
        .json(&json!({}))
        .send()
        .await
        .expect(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn mcp_is_unavailable_until_enabled() {
    let app = TestApp::new().await;
    app.root(Method::POST, "/mcp")
        .json(&json!({}))
        .send()
        .await
        .expect(StatusCode::SERVICE_UNAVAILABLE);
    let missing = app
        .root(Method::POST, "/oauth/register")
        .json(&json!({}))
        .send()
        .await;
    assert_ne!(missing.status, StatusCode::CREATED);
}

#[tokio::test]
async fn registration_only_accepts_known_callbacks_and_is_deterministic() {
    let app = app().await;
    let first = register(&app).await;
    assert_eq!(register(&app).await, first, "같은 등록은 같은 ID");
    for bad in [
        "https://evil.example/callback",
        "http://localhost:3000/callback",
    ] {
        app.root(Method::POST, "/oauth/register")
            .json(&json!({ "redirect_uris": [bad] }))
            .send()
            .await
            .expect(StatusCode::BAD_REQUEST);
    }
    app.root(Method::POST, "/oauth/register")
        .json(&json!({ "redirect_uris": [CALLBACK], "token_endpoint_auth_method": "client_secret_basic" }))
        .send()
        .await
        .expect(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_member_connects_and_works_through_tools_within_the_granted_scope() {
    let app = app().await;
    let approver = app.add_member("boss", Role::Admin, None).await;
    let member = app.add_member("kim", Role::Member, None).await;
    let (access, _, scope) = connect(&app, &member, "groupware:read groupware:write").await;
    assert_eq!(scope, "groupware:read groupware:write");

    let init = rpc(
        &app,
        &access,
        "initialize",
        json!({ "protocolVersion": "2025-06-18" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    let listed = rpc(&app, &access, "tools/list", json!({}))
        .await
        .expect(StatusCode::OK);
    assert!(listed["result"]["tools"].as_array().expect("tools").len() > 20);

    let types = call(&app, &access, "list_leave_types", json!({}))
        .await
        .expect(StatusCode::OK);
    let annual = types["result"]["structuredContent"]["result"]
        .as_array()
        .expect("types")
        .iter()
        .find(|t| t["deducts_balance"] == true)
        .expect("annual")["id"]
        .as_str()
        .expect("id")
        .to_owned();
    app.set_time("2026-03-02T09:00:00+09:00");
    let leave = call(
        &app,
        &access,
        "request_leave",
        json!({
            "leave_type_id": annual, "start_date": "2026-03-10", "end_date": "2026-03-10",
            "unit": "full_day", "reason": "개인 사유",
            "steps": [{ "approver_id": approver.id, "kind": "approve" }],
            "request_id": "leave-1",
        }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(leave["result"]["isError"], false, "{}", leave);
    // 같은 request_id로 다시 보내도 하나만 생긴다.
    call(
        &app,
        &access,
        "request_leave",
        json!({
            "leave_type_id": annual, "start_date": "2026-03-10", "end_date": "2026-03-10",
            "unit": "full_day", "reason": "개인 사유",
            "steps": [{ "approver_id": approver.id, "kind": "approve" }],
            "request_id": "leave-1",
        }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(app.count("SELECT COUNT(*) FROM leave_requests").await, 1);
    // 기록은 웹과 같이 본인 이름으로 남는다.
    assert_eq!(
        app.audit_entries("leave.request").await[0].actor_id,
        Some(member.id)
    );

    // 서비스가 거부한 일은 도구 오류로 돌아온다(관리자 작업 권한이 없는 공지 게시 등).
    let bad = call(
        &app,
        &access,
        "get_approval_document",
        json!({ "id": "nope" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(bad["result"]["isError"], true);

    // 관리 도구는 구성원에게 보이지 않고, 불러도 도구 오류로 끝난다(다시 로그인시키지 않는다).
    let listed = rpc(&app, &access, "tools/list", json!({}))
        .await
        .expect(StatusCode::OK);
    assert!(!listed["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .any(|t| t["name"] == "create_notice"));
    let refused = call(
        &app,
        &access,
        "create_notice",
        json!({ "title": "공지", "body": "본문", "audience": "all" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(refused["result"]["isError"], true);
    assert_eq!(app.count("SELECT COUNT(*) FROM notices").await, 0);
}

#[tokio::test]
async fn read_only_by_default_and_admin_scope_only_for_admins() {
    let app = app().await;
    let member = app.add_member("kim", Role::Member, None).await;
    let admin = app.add_member("admin1", Role::Admin, None).await;

    let (access, _, scope) = connect(&app, &member, "").await;
    assert_eq!(scope, "groupware:read", "scope를 안 보내면 조회만");
    call(&app, &access, "clock_out", json!({}))
        .await
        .expect(StatusCode::FORBIDDEN);

    let (_, _, scope) = connect(&app, &member, "groupware:read groupware:admin").await;
    assert_eq!(
        scope, "groupware:read",
        "구성원에게는 관리 scope를 주지 않는다"
    );

    let (access, _, scope) = connect(&app, &admin, "groupware:read groupware:admin").await;
    assert_eq!(scope, "groupware:read groupware:admin");
    let notice = call(
        &app,
        &access,
        "create_notice",
        json!({ "title": "MCP 공지", "body": "본문", "audience": "all" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(notice["result"]["isError"], false, "{notice}");
}

#[tokio::test]
async fn consent_rejections_keep_the_flow_safe() {
    let app = app().await;
    let member = app.add_member("kim", Role::Member, None).await;
    let client_id = register(&app).await;

    // 거부는 로그인 없이, 클라이언트로 error=access_denied와 함께 돌아간다.
    let (request_id, nonce) = open_consent(&app, &client_id, "groupware:read").await;
    let denied = decide(&app, &request_id, &nonce, &[("action", "deny")]).await;
    assert_eq!(denied.status, StatusCode::SEE_OTHER);
    let params = query_of(denied.headers["location"].to_str().expect("location"));
    assert_eq!(params["error"], "access_denied");
    assert_eq!(params["state"], "st-1");

    // 틀린 비밀번호는 같은 화면을 다시 그린다(입력한 아이디는 남긴다).
    let (request_id, nonce) = open_consent(&app, &client_id, "groupware:read").await;
    let wrong = decide(
        &app,
        &request_id,
        &nonce,
        &[
            ("action", "approve"),
            ("login_id", "kim"),
            ("password", "wrong-password"),
        ],
    )
    .await;
    assert_eq!(wrong.status, StatusCode::OK);
    let html = wrong.body.as_str().expect("html");
    assert!(html.contains("아이디 또는 비밀번호가 맞지 않아요") && html.contains("value=\"kim\""));

    // 다른 브라우저(쿠키 없음)나 Origin: null은 승인하지 못한다.
    let no_cookie = app
        .root(Method::POST, "/oauth/authorize")
        .header("origin", ISSUER)
        .raw_body(
            "application/x-www-form-urlencoded",
            form(&[
                ("request_id", &request_id),
                ("action", "approve"),
                ("login_id", "kim"),
                ("password", MEMBER_PASSWORD),
            ]),
        )
        .send()
        .await;
    assert_eq!(no_cookie.status, StatusCode::BAD_REQUEST);
    let null_origin = app
        .root(Method::POST, "/oauth/authorize")
        .header("origin", "null")
        .header("cookie", &format!("pango_oauth_nonce={nonce}"))
        .raw_body(
            "application/x-www-form-urlencoded",
            form(&[("request_id", &request_id), ("action", "deny")]),
        )
        .send()
        .await;
    assert_eq!(null_origin.status, StatusCode::FORBIDDEN);

    // PKCE가 없거나 모르는 클라이언트면 화면을 열지 않는다.
    let no_pkce = app
        .root(
            Method::GET,
            &format!(
                "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri={}&state=s",
                encode(CALLBACK)
            ),
        )
        .send()
        .await;
    assert_eq!(no_pkce.status, StatusCode::SEE_OTHER);
    assert!(no_pkce.headers["location"]
        .to_str()
        .expect("loc")
        .contains("error=invalid_request"));
    let unknown = app
        .root(
            Method::GET,
            &format!(
                "/oauth/authorize?client_id=pgw_x&redirect_uri={}",
                encode(CALLBACK)
            ),
        )
        .send()
        .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    let _ = member;
}

#[tokio::test]
async fn codes_and_refresh_tokens_are_single_use_and_sessions_cut_access() {
    let app = app().await;
    let member = app.add_member("kim", Role::Member, None).await;
    let client_id = register(&app).await;
    let (request_id, nonce) = open_consent(&app, &client_id, "groupware:read").await;
    let approved = decide(
        &app,
        &request_id,
        &nonce,
        &[
            ("action", "approve"),
            ("login_id", "kim"),
            ("password", MEMBER_PASSWORD),
        ],
    )
    .await;
    let code = query_of(approved.headers["location"].to_str().expect("loc"))["code"].clone();
    let exchange = |verifier: &'static str| {
        let code = code.clone();
        let client_id = client_id.clone();
        let app = &app;
        async move {
            token(
                app,
                &[
                    ("grant_type", "authorization_code"),
                    ("code", &code),
                    ("redirect_uri", CALLBACK),
                    ("client_id", &client_id),
                    ("code_verifier", verifier),
                ],
            )
            .await
        }
    };
    // 틀린 verifier는 거부되고, 그 코드는 이미 쓴 것이 된다.
    exchange("wrong-verifier-wrong-verifier-wrong-verifier-123")
        .await
        .expect(StatusCode::BAD_REQUEST);
    exchange(VERIFIER).await.expect(StatusCode::BAD_REQUEST);

    let (access, refresh, _) = connect(&app, &member, "groupware:read").await;
    let renewed = token(
        &app,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh),
            ("client_id", &client_id),
        ],
    )
    .await
    .expect(StatusCode::OK);
    let new_access = renewed["access_token"].as_str().expect("access").to_owned();
    rpc(&app, &new_access, "ping", json!({}))
        .await
        .expect(StatusCode::OK);
    let reuse_fields = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh.as_str()),
        ("client_id", client_id.as_str()),
    ];
    let reuse = || token(&app, &reuse_fields);
    // 30초 안의 재사용(병렬 갱신·재시도)은 거절만 하고 연결은 그대로 둔다.
    reuse().await.expect(StatusCode::BAD_REQUEST);
    rpc(&app, &new_access, "ping", json!({}))
        .await
        .expect(StatusCode::OK);
    // 그 뒤에 쓴 갱신 토큰이 다시 오면(탈취) 세션을 끊어 모든 토큰이 죽는다.
    app.advance(chrono::Duration::seconds(31));
    reuse().await.expect(StatusCode::BAD_REQUEST);
    rpc(&app, &new_access, "ping", json!({}))
        .await
        .expect(StatusCode::UNAUTHORIZED);
    rpc(&app, &access, "ping", json!({}))
        .await
        .expect(StatusCode::UNAUTHORIZED);
    // 앞의 코드 재사용과 이번 갱신 토큰 재사용, 두 번 연결을 끊었다.
    assert_eq!(app.audit_entries("oauth.token_reuse").await.len(), 2);

    // 연결은 "로그인된 기기"에 보이고, 기기 로그아웃하면 끊긴다.
    let (access, _, _) = connect(&app, &member, "groupware:read").await;
    let sessions = app
        .get("/me/sessions")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let mcp_session = sessions
        .as_array()
        .expect("sessions")
        .iter()
        .rev()
        .find(|s| {
            s["user_agent"]
                .as_str()
                .is_some_and(|ua| ua.starts_with("AI 연결(MCP)"))
        })
        .expect("mcp session")["id"]
        .as_str()
        .expect("id")
        .to_owned();
    app.delete(&format!("/me/sessions/{mcp_session}"))
        .bearer(&member.token)
        .send()
        .await;
    rpc(&app, &access, "ping", json!({}))
        .await
        .expect(StatusCode::UNAUTHORIZED);

    // 접근 토큰은 10분이면 만료된다.
    let (access, _, _) = connect(&app, &member, "groupware:read").await;
    app.advance(chrono::Duration::minutes(10));
    rpc(&app, &access, "ping", json!({}))
        .await
        .expect(StatusCode::UNAUTHORIZED);
}

/// 승인을 받은 클라이언트는 연결이 모두 끝나도 남아, claude.ai가 저장해 둔 client_id로 다시
/// 연결할 수 있다. 등록만 한 클라이언트는 정리된다.
#[tokio::test]
async fn cleanup_keeps_clients_that_were_granted() {
    let app = app().await;
    let member = app.add_member("kim", Role::Member, None).await;
    let granted = register(&app).await;
    connect(&app, &member, "groupware:read").await;
    let probe = app
        .root(Method::POST, "/oauth/register")
        .json(&json!({ "client_name": "Probe", "redirect_uris": [CALLBACK], "token_endpoint_auth_method": "none" }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    let probe = probe["client_id"].as_str().expect("client_id").to_owned();

    // 토큰이 만료되고(갱신 토큰 포함) 그 뒤 하루가 넘게 지난다.
    app.advance(chrono::Duration::days(100));
    pango_server::service::maintenance::run_once(&app.state)
        .await
        .expect("cleanup");

    let mut conn = app.state.read().await.expect("read");
    assert!(pango_db::oauth::find_client(&mut conn, &granted)
        .await
        .expect("find")
        .is_some());
    assert!(pango_db::oauth::find_client(&mut conn, &probe)
        .await
        .expect("find")
        .is_none());
}

#[tokio::test]
async fn token_endpoint_is_rate_limited_per_ip_and_recovers_after_window() {
    let app = TestApp::with_config(Config {
        trust_proxy: true,
        mcp: Some(McpConfig::from_public_origin(Some(ISSUER), false).expect("mcp")),
        ..Config::for_tests()
    })
    .await;
    let body = form(&[
        ("grant_type", "authorization_code"),
        ("code", "0".repeat(64).as_str()),
        ("client_id", "pgw_test"),
    ]);
    let send_from = |ip: &'static str| {
        let app = &app;
        let body = body.clone();
        async move {
            app.root(Method::POST, "/oauth/token")
                .header("x-forwarded-for", ip)
                .raw_body("application/x-www-form-urlencoded", body)
                .send()
                .await
        }
    };
    for _ in 0..120 {
        assert_eq!(
            send_from("203.0.113.10").await.status,
            StatusCode::BAD_REQUEST
        );
    }
    let limited = send_from("203.0.113.10").await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.body["error"], "slow_down");
    // 다른 IP는 영향받지 않는다.
    assert_eq!(
        send_from("203.0.113.11").await.status,
        StatusCode::BAD_REQUEST
    );
    // 1분 창이 지나면 다시 받을 수 있다.
    app.advance(chrono::Duration::seconds(60));
    assert_eq!(
        send_from("203.0.113.10").await.status,
        StatusCode::BAD_REQUEST
    );
}
