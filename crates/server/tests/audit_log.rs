//! 감사 로그 조회 통합 테스트.

use chrono::Duration;
use http::StatusCode;
use pango_core::authz::Role;
use pango_core::id::MemberId;
use pango_server::testing::{TestApp, MEMBER_PASSWORD, OWNER_PASSWORD};
use serde_json::{json, Value};

fn actions(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|e| e["action"].as_str().expect("action").to_owned())
        .collect()
}

async fn audit_page(app: &TestApp, token: &str, query: &str) -> Value {
    app.get(&format!("/admin/audit-log{query}"))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

/// 공휴일을 하나 등록해 감사 로그 한 줄을 만들고 그 시각을 돌려준다.
async fn holiday_at(app: &TestApp, token: &str, date: &str) -> i64 {
    let at = app.state.now().as_millis();
    app.put(&format!("/admin/holidays/{date}"))
        .bearer(token)
        .json(&json!({ "name": format!("{date} 휴일") }))
        .send()
        .await
        .expect(StatusCode::OK);
    at
}

#[tokio::test]
async fn lists_newest_first_with_actor_names() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    holiday_at(&app, &admin.token, "2026-05-05").await;

    let page = audit_page(&app, &owner, "").await;
    assert_eq!(
        actions(&page),
        vec![
            "holiday.upsert",
            "auth.login",
            "auth.login",
            "system.initialize"
        ]
    );
    let items = page["items"].as_array().expect("items");
    assert_eq!(items[0]["actor_id"], admin.id.to_string());
    assert_eq!(items[0]["actor_name"], "admin1 님");
    assert_eq!(items[0]["target_type"], "holiday");
    assert_eq!(items[0]["target_id"], "2026-05-05");
    assert_eq!(items[3]["actor_id"], Value::Null, "시스템 작업");
    assert_eq!(items[3]["actor_name"], Value::Null);
    let names: Vec<_> = items[1..3]
        .iter()
        .map(|e| e["actor_name"].clone())
        .collect();
    assert!(names.contains(&json!("대표")) && names.contains(&json!("admin1 님")));
}

#[tokio::test]
async fn filters_by_actor_action_and_target() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    holiday_at(&app, &owner, "2026-01-01").await;
    holiday_at(&app, &admin.token, "2026-03-01").await;
    holiday_at(&app, &admin.token, "2026-05-05").await;

    let by_actor = audit_page(&app, &owner, &format!("?actor_id={}", admin.id)).await;
    assert_eq!(
        actions(&by_actor),
        vec!["holiday.upsert", "holiday.upsert", "auth.login"]
    );
    let by_action = audit_page(&app, &owner, "?action=holiday.upsert").await;
    assert_eq!(actions(&by_action).len(), 3);
    let by_target = audit_page(&app, &owner, "?target_type=holiday&target_id=2026-03-01").await;
    assert_eq!(by_target["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(by_target["items"][0]["actor_id"], admin.id.to_string());
    let combined = audit_page(
        &app,
        &owner,
        &format!("?actor_id={}&action=auth.login", app.owner_id),
    )
    .await;
    assert_eq!(actions(&combined), vec!["auth.login"]);
    let nobody = audit_page(&app, &owner, &format!("?actor_id={}", MemberId::new())).await;
    assert!(actions(&nobody).is_empty());
    let injection = audit_page(&app, &owner, "?action=%27%20OR%20%271%27%3D%271").await;
    assert!(actions(&injection).is_empty(), "필터 값은 그대로 비교된다");
}

#[tokio::test]
async fn time_range_is_from_inclusive_until_exclusive() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    app.advance(Duration::seconds(10));
    let t1 = holiday_at(&app, &owner, "2026-01-01").await;
    app.advance(Duration::seconds(1));
    let t2 = holiday_at(&app, &owner, "2026-01-02").await;
    app.advance(Duration::seconds(1));
    let t3 = holiday_at(&app, &owner, "2026-01-03").await;

    let targets = |page: Value| -> Vec<String> {
        page["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|e| e["target_id"].as_str().unwrap_or_default().to_owned())
            .collect()
    };
    let q = |from: i64, until: i64| format!("?action=holiday.upsert&from={from}&until={until}");
    assert_eq!(
        targets(audit_page(&app, &owner, &q(t1, t2)).await),
        vec!["2026-01-01"]
    );
    assert_eq!(
        targets(audit_page(&app, &owner, &q(t1, t3)).await),
        vec!["2026-01-02", "2026-01-01"]
    );
    assert_eq!(
        targets(audit_page(&app, &owner, &q(t1 + 1, t3 + 1)).await),
        vec!["2026-01-03", "2026-01-02"]
    );
    assert!(targets(audit_page(&app, &owner, &q(t2, t2)).await).is_empty());
    assert_eq!(
        targets(audit_page(&app, &owner, &format!("?action=holiday.upsert&from={t3}")).await),
        vec!["2026-01-03"]
    );
    assert_eq!(
        targets(audit_page(&app, &owner, &format!("?action=holiday.upsert&until={t2}")).await),
        vec!["2026-01-01"]
    );
    app.get("/admin/audit-log?from=yesterday")
        .bearer(&owner)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    let reversed = app
        .get(&format!("/admin/audit-log?from={t3}&until={t1}"))
        .bearer(&owner)
        .send()
        .await;
    assert_eq!(reversed.status, StatusCode::BAD_REQUEST);
    assert_eq!(reversed.body["error"]["field"], "until");
}

#[tokio::test]
async fn pages_without_gaps_and_rejects_garbage_cursor() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    for day in 1..=7 {
        holiday_at(&app, &owner, &format!("2026-02-{day:02}")).await;
    }
    let mut seen = Vec::new();
    let mut query = "?action=holiday.upsert&limit=3".to_owned();
    loop {
        let page = audit_page(&app, &owner, &query).await;
        seen.extend(
            page["items"]
                .as_array()
                .expect("items")
                .iter()
                .map(|e| e["target_id"].as_str().expect("target").to_owned()),
        );
        match page["next_cursor"].as_str() {
            Some(cursor) => query = format!("?action=holiday.upsert&limit=3&cursor={cursor}"),
            None => break,
        }
    }
    let expected: Vec<String> = (1..=7).rev().map(|d| format!("2026-02-{d:02}")).collect();
    assert_eq!(seen, expected);

    for cursor in ["nope", "00", "%22x%22"] {
        let response = app
            .get(&format!("/admin/audit-log?cursor={cursor}"))
            .bearer(&owner)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{cursor}");
        assert_eq!(response.body["error"]["field"], "cursor");
    }
    app.get("/admin/audit-log?actor_id=not-a-uuid")
        .bearer(&owner)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
}

#[tokio::test]
async fn only_admins_can_view_and_viewing_is_not_audited() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let dept = app.add_department("개발팀", None, None).await;
    let lead = app.add_member("lead", Role::Member, Some(dept)).await;
    app.set_department_head(dept, Some(lead.id)).await;
    let member = app.add_member("member1", Role::Member, Some(dept)).await;

    for token in [&member.token, &lead.token] {
        app.get("/admin/audit-log")
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    app.get("/admin/audit-log")
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");

    let before = actions(&audit_page(&app, &owner, "").await);
    for _ in 0..3 {
        audit_page(&app, &admin.token, "").await;
    }
    let after = actions(&audit_page(&app, &owner, "").await);
    assert_eq!(before, after, "조회는 감사 로그를 남기지 않는다");
}

#[tokio::test]
async fn entries_never_contain_passwords_or_tokens() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let member = app.add_member("member1", Role::Member, None).await;
    let new_password = "brand-new-password-42";
    app.post("/me/password")
        .bearer(&member.token)
        .json(&json!({ "current_password": MEMBER_PASSWORD, "new_password": new_password }))
        .send()
        .await
        .expect(StatusCode::OK);
    // 실패한 로그인도 섞는다.
    app.post("/auth/login")
        .json(&json!({ "login_id": "member1", "password": "wrong-password-1" }))
        .send()
        .await;

    let response = app
        .get("/admin/audit-log?limit=200")
        .bearer(&owner)
        .send()
        .await;
    let dump = response.body.to_string();
    assert_eq!(response.status, StatusCode::OK);
    assert!(dump.contains("auth.password_change"));
    for secret in [
        owner.as_str(),
        member.token.as_str(),
        OWNER_PASSWORD,
        MEMBER_PASSWORD,
        new_password,
        "wrong-password-1",
        "password_hash",
        "argon2",
    ] {
        assert!(
            !dump.contains(secret),
            "감사 로그에 비밀 값이 있다: {secret}"
        );
    }
}

#[tokio::test]
async fn targets_are_shown_by_name_not_internal_id() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let department = app
        .post("/admin/departments")
        .bearer(&owner)
        .json(&json!({ "name": "개발팀" }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    holiday_at(&app, &owner, "2026-05-05").await;
    let member = app.add_member("hong", Role::Member, None).await;
    app.post(&format!("/admin/members/{}/reset-password", member.id))
        .bearer(&owner)
        .json(&json!({}))
        .send()
        .await
        .expect(StatusCode::OK);

    let page = audit_page(&app, &owner, "").await;
    let name_of = |action: &str| {
        page["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|e| e["action"] == action)
            .unwrap_or_else(|| panic!("{action} missing"))["target_name"]
            .clone()
    };
    assert_eq!(name_of("department.create"), "개발팀");
    assert_eq!(name_of("holiday.upsert"), "2026-05-05");
    assert_eq!(name_of("member.password_reset"), "hong 님");
    // 이름이 없는 대상(로그인 기기)은 비어 있다.
    assert_eq!(name_of("auth.login"), Value::Null);
    assert!(department["id"].is_string());
}
