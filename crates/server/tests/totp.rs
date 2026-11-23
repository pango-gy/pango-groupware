//! 2단계 인증(TOTP) 통합 테스트.

use chrono::Duration;
use http::StatusCode;
use pango_core::authz::Role;
use pango_server::testing::{TestApp, TestMember, MEMBER_PASSWORD};
use pango_server::totp::{current_code, decode_base32};
use serde_json::{json, Value};

async fn login(
    app: &TestApp,
    login_id: &str,
    password: &str,
    code: Option<&str>,
) -> pango_server::testing::TestResponse {
    app.post("/auth/login")
        .json(&json!({
            "login_id": login_id, "password": password, "token_in_body": true, "totp_code": code,
        }))
        .send()
        .await
}

/// 2단계 인증을 켜고 `(비밀키, 복구 코드)`를 돌려준다.
async fn enable(app: &TestApp, member: &TestMember) -> (Vec<u8>, Vec<String>) {
    let setup = app
        .post("/me/totp/setup")
        .bearer(&member.token)
        .json(&json!({ "password": MEMBER_PASSWORD }))
        .send()
        .await
        .expect(StatusCode::OK);
    let secret = decode_base32(setup["secret"].as_str().expect("secret")).expect("base32");
    assert!(setup["otpauth_uri"]
        .as_str()
        .expect("uri")
        .starts_with("otpauth://totp/"));
    let codes = app
        .post("/me/totp/confirm")
        .bearer(&member.token)
        .json(&json!({ "code": current_code(&secret, app.state.now()) }))
        .send()
        .await
        .expect(StatusCode::OK);
    let codes = codes["codes"]
        .as_array()
        .expect("codes")
        .iter()
        .map(|c| c.as_str().expect("code").to_owned())
        .collect();
    (secret, codes)
}

#[tokio::test]
async fn setup_needs_the_password_and_a_working_code() {
    let app = TestApp::new().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.post("/me/totp/setup")
        .bearer(&member.token)
        .json(&json!({ "password": "wrong-password" }))
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    app.post("/me/totp/setup")
        .bearer(&member.token)
        .json(&json!({ "password": MEMBER_PASSWORD }))
        .send()
        .await
        .expect(StatusCode::OK);
    app.post("/me/totp/confirm")
        .bearer(&member.token)
        .json(&json!({ "code": "000000" }))
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    // 켜는 중에는 로그인에 쓰지 않는다.
    login(&app, "kim@example.com", MEMBER_PASSWORD, None)
        .await
        .expect(StatusCode::OK);

    let member = TestMember {
        token: login(&app, "kim@example.com", MEMBER_PASSWORD, None)
            .await
            .expect(StatusCode::OK)["token"]
            .as_str()
            .expect("token")
            .to_owned(),
        ..member
    };
    let (_, codes) = enable(&app, &member).await;
    assert_eq!(codes.len(), 10);
    let status = app
        .get("/me/totp")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        status,
        json!({ "enabled": true, "recovery_codes_left": 10 })
    );
    app.post("/me/totp/setup")
        .bearer(&member.token)
        .json(&json!({ "password": MEMBER_PASSWORD }))
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "already_exists");
    assert_eq!(app.audit_entries("auth.totp_enable").await.len(), 1);
}

#[tokio::test]
async fn login_requires_a_fresh_code_or_an_unused_recovery_code() {
    let app = TestApp::new().await;
    let member = app.add_member("kim", Role::Member, None).await;
    let (secret, codes) = enable(&app, &member).await;
    let sessions_before = app.count("SELECT COUNT(*) FROM sessions").await;

    login(&app, "kim@example.com", MEMBER_PASSWORD, None)
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "mfa_required");
    login(&app, "kim@example.com", MEMBER_PASSWORD, Some("000000"))
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "mfa_invalid");
    // 비밀번호가 틀리면 코드가 필요한지 알려 주지 않는다.
    login(&app, "kim@example.com", "wrong-password", None)
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "invalid_credentials");
    assert_eq!(
        app.count("SELECT COUNT(*) FROM sessions").await,
        sessions_before
    );

    // 한 단계 뒤의 코드로 로그인하고, 같은 코드는 다시 쓸 수 없다.
    app.advance(Duration::seconds(30));
    let code = current_code(&secret, app.state.now());
    login(&app, "kim@example.com", MEMBER_PASSWORD, Some(&code))
        .await
        .expect(StatusCode::OK);
    login(&app, "kim@example.com", MEMBER_PASSWORD, Some(&code))
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "mfa_invalid");

    // 복구 코드는 한 번만.
    let recovery = codes[0].to_uppercase();
    login(&app, "kim@example.com", MEMBER_PASSWORD, Some(&recovery))
        .await
        .expect(StatusCode::OK);
    login(&app, "kim@example.com", MEMBER_PASSWORD, Some(&recovery))
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "mfa_invalid");
    let status: Value = app
        .get("/me/totp")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(status["recovery_codes_left"], 9);
}

#[tokio::test]
async fn wrong_codes_count_toward_the_login_lockout() {
    let app = TestApp::new().await;
    let member = app.add_member("kim", Role::Member, None).await;
    let (secret, _) = enable(&app, &member).await;
    for _ in 0..5 {
        login(&app, "kim@example.com", MEMBER_PASSWORD, Some("000000")).await;
    }
    app.advance(Duration::seconds(30));
    let code = current_code(&secret, app.state.now());
    login(&app, "kim@example.com", MEMBER_PASSWORD, Some(&code))
        .await
        .expect_error(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts");
}

#[tokio::test]
async fn disabling_needs_password_and_code_and_admins_can_reset() {
    let app = TestApp::new().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let member = app.add_member("kim", Role::Member, None).await;
    let (secret, _) = enable(&app, &member).await;

    app.post("/me/totp/disable")
        .bearer(&member.token)
        .json(&json!({ "password": MEMBER_PASSWORD, "code": "000000" }))
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    app.advance(Duration::seconds(30));
    let off = app
        .post("/me/totp/disable")
        .bearer(&member.token)
        .json(&json!({
            "password": MEMBER_PASSWORD, "code": current_code(&secret, app.state.now()),
        }))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(off["enabled"], false);
    login(&app, "kim@example.com", MEMBER_PASSWORD, None)
        .await
        .expect(StatusCode::OK);

    // 관리자 초기화(휴대폰 분실).
    enable(&app, &member).await;
    let path = format!("/admin/members/{}/totp", member.id);
    assert_eq!(
        app.get(&path)
            .bearer(&admin.token)
            .send()
            .await
            .expect(StatusCode::OK),
        json!({ "enabled": true })
    );
    app.delete(&path)
        .bearer(&member.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    app.delete(&path)
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::NO_CONTENT);
    login(&app, "kim@example.com", MEMBER_PASSWORD, None)
        .await
        .expect(StatusCode::OK);
    assert_eq!(app.audit_entries("member.totp_reset").await.len(), 1);
    // 관리자는 최고 관리자나 자기 자신을 초기화할 수 없다.
    app.delete(&format!("/admin/members/{}/totp", app.owner_id))
        .bearer(&admin.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    app.delete(&format!("/admin/members/{}/totp", admin.id))
        .bearer(&admin.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
}

#[tokio::test]
async fn admins_only_see_2fa_status_of_accounts_they_manage() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let other_admin = app.add_member("admin2", Role::Admin, None).await;
    app.get(&format!("/admin/members/{}/totp", app.owner_id))
        .bearer(&admin.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    app.get(&format!("/admin/members/{}/totp", other_admin.id))
        .bearer(&admin.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    app.get(&format!("/admin/members/{}/totp", other_admin.id))
        .bearer(&owner)
        .send()
        .await
        .expect(StatusCode::OK);
}
