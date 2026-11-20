//! 구성원 디렉터리·팀·구성원 관리 통합 테스트.

use std::sync::Arc;

use http::StatusCode;
use pango_core::authz::Role;
use pango_core::id::{DepartmentId, MemberId};
use pango_core::member::MemberStatus;
use pango_db::audit::{self, AuditEntry, AuditFilter};
use pango_db::members::{self, Member, MemberFilter};
use pango_server::testing::{
    TestApp, TestMember, TestResponse, MEMBER_PASSWORD, OWNER_LOGIN, OWNER_PASSWORD,
};
use serde_json::{json, Value};

struct World {
    app: TestApp,
    owner: String,
    admin: TestMember,
    manager: TestMember,
    member: TestMember,
    dept: DepartmentId,
}

/// 최고 관리자, 관리자, 조직장(개발팀 head), 개발팀 구성원이 있는 회사.
async fn world() -> World {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let dept = app.add_department("개발팀", None, None).await;
    let admin = app.add_member("admin", Role::Admin, None).await;
    let manager = app.add_member("manager", Role::Member, Some(dept)).await;
    let member = app.add_member("member", Role::Member, Some(dept)).await;
    app.set_department_head(dept, Some(manager.id)).await;
    World {
        app,
        owner,
        admin,
        manager,
        member,
        dept,
    }
}

async fn row(app: &TestApp, id: MemberId) -> Member {
    let mut conn = app.state.read().await.expect("read");
    members::find(&mut conn, id)
        .await
        .expect("find")
        .expect("member")
}

async fn audit_entries(app: &TestApp, action: &str) -> Vec<AuditEntry> {
    let filter = AuditFilter {
        action: Some(action.to_owned()),
        ..Default::default()
    };
    let mut conn = app.state.read().await.expect("read");
    audit::list(&mut conn, &filter, 200).await.expect("audit").0
}

async fn all_audit(app: &TestApp) -> Vec<AuditEntry> {
    let mut conn = app.state.read().await.expect("read");
    audit::list(&mut conn, &AuditFilter::default(), 200)
        .await
        .expect("audit")
        .0
}

/// 구성원 전체 + 감사 로그 수 + 부서 조직장. 거부된 요청이 아무것도 바꾸지 않았는지 비교한다.
async fn snapshot(app: &TestApp) -> (Vec<Member>, usize, Vec<Option<MemberId>>) {
    let mut conn = app.state.read().await.expect("read");
    let all = members::list(&mut conn, &MemberFilter::default())
        .await
        .expect("list");
    let heads = pango_db::org::list_departments(&mut conn)
        .await
        .expect("departments")
        .into_iter()
        .map(|d| d.head_member_id)
        .collect();
    drop(conn);
    (all, all_audit(app).await.len(), heads)
}

async fn set_status(app: &TestApp, id: MemberId, status: MemberStatus) {
    let mut tx = app.state.db.begin_write().await.expect("begin");
    let mut member = members::find(&mut tx, id).await.expect("find").expect("m");
    member.status = status;
    assert!(members::update(&mut tx, &member, app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");
}

async fn try_login(app: &TestApp, login_id: &str, password: &str) -> StatusCode {
    app.post("/auth/login")
        .json(&json!({ "login_id": login_id, "password": password, "token_in_body": true }))
        .send()
        .await
        .status
}

fn names(list: &Value) -> Vec<String> {
    let mut names: Vec<String> = list
        .as_array()
        .expect("array")
        .iter()
        .map(|m| m["name"].as_str().expect("name").to_owned())
        .collect();
    names.sort();
    names
}

fn new_member(login_id: &str) -> Value {
    json!({ "login_id": login_id, "name": "홍길동", "hired_on": "2026-03-02" })
}

async fn create(app: &TestApp, token: &str, body: &Value) -> Value {
    app.post("/admin/members")
        .bearer(token)
        .json(body)
        .send()
        .await
        .expect(StatusCode::CREATED)
}

async fn change_status(app: &TestApp, token: &str, id: MemberId, body: Value) -> TestResponse {
    app.post(&format!("/admin/members/{id}/status"))
        .bearer(token)
        .json(&body)
        .send()
        .await
}

async fn change_role(
    app: &TestApp,
    token: &str,
    id: MemberId,
    version: i64,
    role: &str,
) -> TestResponse {
    app.post(&format!("/admin/members/{id}/role"))
        .bearer(token)
        .json(&json!({ "version": version, "role": role }))
        .send()
        .await
}

async fn reset(app: &TestApp, token: &str, id: MemberId) -> TestResponse {
    app.post(&format!("/admin/members/{id}/reset-password"))
        .bearer(token)
        .send()
        .await
}

fn profile(version: i64, name: &str) -> Value {
    json!({ "version": version, "name": name, "hired_on": "2024-03-02" })
}

async fn update(app: &TestApp, token: &str, id: MemberId, body: &Value) -> TestResponse {
    app.patch(&format!("/admin/members/{id}"))
        .bearer(token)
        .json(body)
        .send()
        .await
}

// ── 디렉터리 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn directory_lists_only_active_members_to_non_admins() {
    let w = world().await;
    let paused = w.app.add_member("paused", Role::Member, Some(w.dept)).await;
    let gone = w.app.add_member("gone", Role::Member, Some(w.dept)).await;
    set_status(&w.app, paused.id, MemberStatus::Suspended).await;
    set_status(&w.app, gone.id, MemberStatus::Retired).await;
    let active = ["admin 님", "manager 님", "member 님", "대표"];

    for token in [&w.member.token, &w.manager.token] {
        for query in ["", "?include_inactive=true"] {
            let list = w
                .app
                .get(&format!("/members{query}"))
                .bearer(token)
                .send()
                .await
                .expect(StatusCode::OK);
            assert_eq!(names(&list), active, "{query}: 관리자가 아니면 무시된다");
        }
    }
    let admin_view = w
        .app
        .get("/members?include_inactive=true")
        .bearer(&w.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(admin_view.as_array().expect("array").len(), 6);
    let admin_default = w
        .app
        .get("/members")
        .bearer(&w.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(names(&admin_default), active);

    // 디렉터리에는 공개 정보만 있다.
    let entry = &admin_view[0];
    for secret in [
        "login_id",
        "role",
        "password_hash",
        "must_change_password",
        "version",
    ] {
        assert!(entry.get(secret).is_none(), "{secret} leaked: {entry}");
    }
    let dev = admin_view
        .as_array()
        .expect("array")
        .iter()
        .find(|m| m["id"] == w.member.id.to_string())
        .expect("member");
    assert_eq!(dev["department_name"], "개발팀");
    assert_eq!(dev["status"], "active");
}

#[tokio::test]
async fn inactive_member_profile_is_hidden_from_non_admins() {
    let w = world().await;
    let gone = w.app.add_member("gone", Role::Member, Some(w.dept)).await;
    set_status(&w.app, gone.id, MemberStatus::Retired).await;
    let path = format!("/members/{}", gone.id);
    for token in [&w.member.token, &w.manager.token] {
        w.app
            .get(&path)
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    let seen = w
        .app
        .get(&path)
        .bearer(&w.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(seen["status"], "retired");
    let colleague = w
        .app
        .get(&format!("/members/{}", w.manager.id))
        .bearer(&w.member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(colleague["name"], "manager 님");
    for bad in [
        MemberId::new().to_string(),
        "not-a-uuid".into(),
        w.member.id.to_string().to_uppercase(),
        "%00".into(),
    ] {
        w.app
            .get(&format!("/members/{bad}"))
            .bearer(&w.member.token)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
}

#[tokio::test]
async fn directory_search_is_literal_and_bounded() {
    let w = world().await;
    create(
        &w.app,
        &w.owner,
        &json!({ "login_id": "wild", "name": "100%_달성", "hired_on": "2026-03-02" }),
    )
    .await;
    let search = |query: String| {
        let app = &w.app;
        let token = w.member.token.clone();
        async move {
            app.get(&format!("/members?{query}"))
                .bearer(&token)
                .send()
                .await
        }
    };
    // `%`와 `_`가 와일드카드로 쓰이면 모두가 나온다.
    for (term, expected) in [
        ("%25", vec!["100%_달성"]),
        ("_", vec!["100%_달성"]),
        ("100%25_", vec!["100%_달성"]),
        ("%27", vec![]),
        ("%27%20OR%201%3D1%20--", vec![]),
        ("%22%3E%3Cscript%3E", vec![]),
        ("manager", vec!["manager 님"]),
        (
            "",
            vec!["100%_달성", "admin 님", "manager 님", "member 님", "대표"],
        ),
    ] {
        let list = search(format!("search={term}"))
            .await
            .expect(StatusCode::OK);
        assert_eq!(names(&list), expected, "{term}");
    }
    for (query, code) in [
        (format!("search={}", "a".repeat(101)), "validation_failed"),
        (
            format!("search={}", "%EA%B0%80".repeat(5_000)),
            "validation_failed",
        ),
        ("search=a%00b".to_owned(), "validation_failed"),
        ("search=%E2%80%AE".to_owned(), "validation_failed"),
        ("department_id=not-a-uuid".to_owned(), "malformed_request"),
        ("include_inactive=maybe".to_owned(), "malformed_request"),
    ] {
        search(query.clone())
            .await
            .expect_error(StatusCode::BAD_REQUEST, code);
    }
    let unknown = search(format!("department_id={}", DepartmentId::new()))
        .await
        .expect(StatusCode::OK);
    assert_eq!(unknown, json!([]));
    let dev = search(format!("department_id={}", w.dept))
        .await
        .expect(StatusCode::OK);
    assert_eq!(names(&dev), ["manager 님", "member 님"]);
}

#[tokio::test]
async fn team_members_follow_department_management() {
    let w = world().await;
    let child = w.app.add_department("백엔드", Some(w.dept), None).await;
    let other = w.app.add_department("영업팀", None, None).await;
    w.app.add_member("child", Role::Member, Some(child)).await;
    w.app.add_member("sales", Role::Member, Some(other)).await;
    let paused = w.app.add_member("paused", Role::Member, Some(w.dept)).await;
    set_status(&w.app, paused.id, MemberStatus::Suspended).await;

    w.app
        .get("/team/members")
        .bearer(&w.member.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    let team = w
        .app
        .get("/team/members")
        .bearer(&w.manager.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(names(&team), ["child 님", "manager 님", "member 님"]);
    let all = w
        .app
        .get("/team/members")
        .bearer(&w.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        names(&all),
        [
            "admin 님",
            "child 님",
            "manager 님",
            "member 님",
            "sales 님",
            "대표"
        ]
    );
    w.app
        .get("/team/members")
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
}

// ── 관리: 권한 행렬 ───────────────────────────────────────────────────────

#[tokio::test]
async fn admin_member_routes_are_forbidden_to_members_and_managers() {
    let w = world().await;
    let before = snapshot(&w.app).await;
    let unknown = MemberId::new();
    for token in [&w.member.token, &w.manager.token] {
        for target in [w.member.id, w.app.owner_id, unknown] {
            let base = format!("/admin/members/{target}");
            let responses = [
                w.app.get("/admin/members").bearer(token).send().await,
                w.app.get(&base).bearer(token).send().await,
                w.app
                    .post("/admin/members")
                    .bearer(token)
                    .json(&new_member("sneaky"))
                    .send()
                    .await,
                update(&w.app, token, target, &profile(1, "탈취")).await,
                change_status(
                    &w.app,
                    token,
                    target,
                    json!({ "version": 1, "status": "retired" }),
                )
                .await,
                change_role(&w.app, token, target, 1, "owner").await,
                reset(&w.app, token, target).await,
            ];
            for response in responses {
                response.expect_error(StatusCode::FORBIDDEN, "forbidden");
            }
        }
    }
    assert_eq!(snapshot(&w.app).await, before);
    // 본인 계정이라도 관리 API로는 권한을 올릴 수 없다.
    change_role(&w.app, &w.member.token, w.member.id, 1, "admin")
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(row(&w.app, w.member.id).await.role, Role::Member);
}

#[tokio::test]
async fn admins_manage_only_member_role_accounts() {
    let w = world().await;
    let other_admin = w.app.add_member("admin2", Role::Admin, None).await;
    let before = snapshot(&w.app).await;
    for target in [other_admin.id, w.app.owner_id, w.admin.id] {
        let version = row(&w.app, target).await.version;
        update(&w.app, &w.admin.token, target, &profile(version, "바뀜"))
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        change_status(
            &w.app,
            &w.admin.token,
            target,
            json!({ "version": version, "status": "suspended" }),
        )
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
        change_role(&w.app, &w.admin.token, target, version, "member")
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        reset(&w.app, &w.admin.token, target)
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    assert_eq!(snapshot(&w.app).await, before);

    // 일반 구성원 계정은 관리할 수 있다(역할 변경만 빼고).
    let updated = update(&w.app, &w.admin.token, w.member.id, &profile(1, "새 이름"))
        .await
        .expect(StatusCode::OK);
    assert_eq!(updated["name"], "새 이름");
    reset(&w.app, &w.admin.token, w.member.id)
        .await
        .expect(StatusCode::OK);
    let version = row(&w.app, w.member.id).await.version;
    change_role(&w.app, &w.admin.token, w.member.id, version, "admin")
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    change_status(
        &w.app,
        &w.admin.token,
        w.member.id,
        json!({ "version": version, "status": "suspended" }),
    )
    .await
    .expect(StatusCode::OK);

    // 관리자는 다른 관리자·최고 관리자의 정보를 볼 수는 있다.
    let owner = w
        .app
        .get(&format!("/admin/members/{}", w.app.owner_id))
        .bearer(&w.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(owner["role"], "owner");
    assert_eq!(owner["login_id"], OWNER_LOGIN);
}

#[tokio::test]
async fn owner_manages_everyone_but_not_own_status_role_or_password() {
    let w = world().await;
    let owner_id = w.app.owner_id;
    let updated = update(&w.app, &w.owner, w.admin.id, &profile(1, "관리자 이름"))
        .await
        .expect(StatusCode::OK);
    assert_eq!(updated["version"], 2);
    change_role(&w.app, &w.owner, w.admin.id, 2, "member")
        .await
        .expect(StatusCode::OK);
    w.app
        .get("/admin/members")
        .bearer(&w.admin.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");

    // 본인 프로필은 고칠 수 있다.
    let me = update(
        &w.app,
        &w.owner,
        owner_id,
        &json!({ "version": 1, "name": "대표이사", "hired_on": "2026-03-02" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(me["name"], "대표이사");
    let before = snapshot(&w.app).await;
    change_status(
        &w.app,
        &w.owner,
        owner_id,
        json!({ "version": 2, "status": "retired" }),
    )
    .await
    .expect_error(StatusCode::FORBIDDEN, "forbidden");
    change_role(&w.app, &w.owner, owner_id, 2, "admin")
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    // 세션을 탈취해도 현재 비밀번호 없이 새 비밀번호를 얻을 수 없다.
    reset(&w.app, &w.owner, owner_id)
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(snapshot(&w.app).await, before);
}

// ── 권한 상승 시도 ────────────────────────────────────────────────────────

#[tokio::test]
async fn admin_cannot_take_over_the_owner_account() {
    let w = world().await;
    reset(&w.app, &w.admin.token, w.app.owner_id)
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(
        try_login(&w.app, OWNER_LOGIN, OWNER_PASSWORD).await,
        StatusCode::OK
    );
    w.app
        .get("/me")
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert!(!row(&w.app, w.app.owner_id).await.must_change_password);
    assert!(audit_entries(&w.app, "member.password_reset")
        .await
        .is_empty());
}

#[tokio::test]
async fn admin_cannot_promote_self_or_others() {
    let w = world().await;
    let before = snapshot(&w.app).await;
    for (target, role) in [
        (w.admin.id, "owner"),
        (w.member.id, "admin"),
        (w.member.id, "owner"),
    ] {
        let version = row(&w.app, target).await.version;
        change_role(&w.app, &w.admin.token, target, version, role)
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    assert_eq!(snapshot(&w.app).await, before);
}

#[tokio::test]
async fn privileged_fields_smuggled_into_bodies_are_ignored() {
    let w = world().await;
    let forged_id = MemberId::new();
    let created = create(
        &w.app,
        &w.admin.token,
        &json!({
            "login_id": "smuggler", "name": "밀수", "hired_on": "2026-03-02",
            "role": "owner", "status": "suspended", "must_change_password": false,
            "password_hash": "$argon2id$forged", "id": forged_id, "version": 99
        }),
    )
    .await;
    let member = &created["member"];
    assert_eq!(member["role"], "member");
    assert_eq!(member["status"], "active");
    assert_eq!(member["must_change_password"], true);
    assert_eq!(member["version"], 1);
    assert_ne!(member["id"], forged_id.to_string());

    let updated = update(
        &w.app,
        &w.admin.token,
        w.member.id,
        &json!({
            "version": 1, "name": "member 님", "hired_on": "2024-03-02",
            "role": "owner", "status": "retired", "login_id": "hijack",
            "retired_on": "2026-01-01", "must_change_password": true
        }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(updated["role"], "member");
    assert_eq!(updated["status"], "active");
    assert_eq!(updated["login_id"], "member");
    assert_eq!(updated["retired_on"], Value::Null);
    let stored = row(&w.app, w.member.id).await;
    assert_eq!(
        (stored.role, stored.status),
        (Role::Member, MemberStatus::Active)
    );
    assert!(!stored.must_change_password);
}

// ── 생성과 임시 비밀번호 ──────────────────────────────────────────────────

#[tokio::test]
async fn created_member_must_change_temporary_password_first() {
    let w = world().await;
    let created = create(
        &w.app,
        &w.admin.token,
        &json!({
            "login_id": "NewHire", "name": " 신입 ", "email": "New@Example.com",
            "phone": "010-1234-5678", "department_id": w.dept, "hired_on": "2026-03-02"
        }),
    )
    .await;
    let temporary = created["temporary_password"]
        .as_str()
        .expect("pw")
        .to_owned();
    assert_eq!(temporary.len(), 16);
    let member = &created["member"];
    assert_eq!(member["login_id"], "newhire");
    assert_eq!(member["name"], "신입");
    assert_eq!(member["email"], "new@example.com");
    assert_eq!(member["phone"], "010-1234-5678");
    assert_eq!(member["department_name"], "개발팀");
    assert_eq!(member["role"], "member");
    assert_eq!(member["hired_on"], "2026-03-02");
    assert_eq!(member["must_change_password"], true);

    let token = w.app.login("newhire", &temporary).await;
    w.app
        .get("/members")
        .bearer(&token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "password_change_required");
    w.app
        .post("/me/password")
        .bearer(&token)
        .json(&json!({ "current_password": temporary, "new_password": "my-own-password-1" }))
        .send()
        .await
        .expect(StatusCode::OK);
    w.app
        .get("/members")
        .bearer(&token)
        .send()
        .await
        .expect(StatusCode::OK);

    // 임시 비밀번호는 응답에서 한 번만 보이고 감사 로그에는 남지 않는다.
    let audits = audit_entries(&w.app, "member.create").await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor_id, Some(w.admin.id));
    let detail = audits[0].detail.as_ref().expect("detail");
    assert_eq!(detail["login_id"], "newhire");
    for entry in all_audit(&w.app).await {
        let text = serde_json::to_string(&entry.detail).expect("json");
        assert!(!text.contains(&temporary), "{}: {text}", entry.action);
    }
    let detail_view = w
        .app
        .get(&format!(
            "/admin/members/{}",
            member["id"].as_str().expect("id")
        ))
        .bearer(&w.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert!(detail_view.get("temporary_password").is_none());
}

#[tokio::test]
async fn duplicate_login_id_is_rejected_without_changes() {
    let w = world().await;
    let before = snapshot(&w.app).await;
    for login in ["member", "MEMBER", "owner"] {
        w.app
            .post("/admin/members")
            .bearer(&w.owner)
            .json(&new_member(login))
            .send()
            .await
            .expect_error(StatusCode::CONFLICT, "login_id_taken");
    }
    assert_eq!(snapshot(&w.app).await, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_with_same_login_id_make_one_account() {
    let w = world().await;
    let app = Arc::new(w.app);
    let tasks: Vec<_> = (0..4)
        .map(|_| {
            let (app, token) = (app.clone(), w.owner.clone());
            tokio::spawn(async move {
                app.post("/admin/members")
                    .bearer(&token)
                    .json(&new_member("twin"))
                    .send()
                    .await
                    .status
            })
        })
        .collect();
    let mut statuses = Vec::new();
    for task in tasks {
        statuses.push(task.await.expect("join"));
    }
    statuses.sort();
    assert_eq!(
        statuses,
        [
            StatusCode::CREATED,
            StatusCode::CONFLICT,
            StatusCode::CONFLICT,
            StatusCode::CONFLICT
        ]
    );
    assert_eq!(audit_entries(&app, "member.create").await.len(), 1);
}

#[tokio::test]
async fn create_member_input_is_validated_without_side_effects() {
    let w = world().await;
    let archived = w.app.add_department("해체팀", None, None).await;
    w.app
        .delete(&format!("/admin/departments/{archived}?version=1"))
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    let position = w
        .app
        .post("/admin/positions")
        .bearer(&w.owner)
        .json(&json!({ "name": "폐지 직책", "level": 1 }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    w.app
        .delete(&format!(
            "/admin/positions/{}?version=1",
            position["id"].as_str().expect("id")
        ))
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    let before = snapshot(&w.app).await;

    let with = |field: &str, value: Value| {
        let mut body = new_member("valid.id");
        body[field] = value;
        body
    };
    let bad = StatusCode::BAD_REQUEST;
    let cases: Vec<(Value, StatusCode, &str, Option<&str>)> = vec![
        (
            with("login_id", json!("ab")),
            bad,
            "validation_failed",
            Some("login_id"),
        ),
        (
            with("login_id", json!("홍길동")),
            bad,
            "validation_failed",
            Some("login_id"),
        ),
        (
            with("login_id", json!("a'--")),
            bad,
            "validation_failed",
            Some("login_id"),
        ),
        (
            with("login_id", json!("x".repeat(10_000))),
            bad,
            "validation_failed",
            Some("login_id"),
        ),
        (
            with("name", json!("")),
            bad,
            "validation_failed",
            Some("name"),
        ),
        (
            with("name", json!("가".repeat(10_000))),
            bad,
            "validation_failed",
            Some("name"),
        ),
        (
            with("name", json!("이름\u{0}")),
            bad,
            "validation_failed",
            Some("name"),
        ),
        (
            with("name", json!("\u{1b}[31m빨강")),
            bad,
            "validation_failed",
            Some("name"),
        ),
        (
            with("name", json!("관리자\u{202E}")),
            bad,
            "validation_failed",
            Some("name"),
        ),
        (
            with("email", json!("not-an-email")),
            bad,
            "validation_failed",
            Some("email"),
        ),
        (
            with("email", json!("a@b.com\n.evil")),
            bad,
            "validation_failed",
            Some("email"),
        ),
        (
            with("phone", json!("call me")),
            bad,
            "validation_failed",
            Some("phone"),
        ),
        (
            with("phone", json!("1".repeat(21))),
            bad,
            "validation_failed",
            Some("phone"),
        ),
        (
            with("hired_on", json!("2026-02-30")),
            bad,
            "malformed_request",
            None,
        ),
        (
            with("hired_on", json!("어제")),
            bad,
            "malformed_request",
            None,
        ),
        (
            with("hired_on", json!(20260302)),
            bad,
            "malformed_request",
            None,
        ),
        (with("name", json!(null)), bad, "malformed_request", None),
        (
            with("name", json!(["배열"])),
            bad,
            "malformed_request",
            None,
        ),
        (
            json!({ "login_id": "nohire", "name": "이름" }),
            bad,
            "malformed_request",
            None,
        ),
        (
            with("department_id", json!("abc")),
            bad,
            "malformed_request",
            None,
        ),
        (
            with("department_id", json!(DepartmentId::new())),
            StatusCode::NOT_FOUND,
            "not_found",
            None,
        ),
        (
            with("department_id", json!(archived)),
            StatusCode::NOT_FOUND,
            "not_found",
            None,
        ),
        (
            with("position_id", json!(position["id"])),
            StatusCode::NOT_FOUND,
            "not_found",
            None,
        ),
        (
            with("position_id", json!(MemberId::new())),
            StatusCode::NOT_FOUND,
            "not_found",
            None,
        ),
    ];
    for (body, status, code, field) in cases {
        let response = w
            .app
            .post("/admin/members")
            .bearer(&w.owner)
            .json(&body)
            .send()
            .await;
        assert_eq!(
            (response.status, response.error_code()),
            (status, code),
            "{body}: {}",
            response.body
        );
        if let Some(field) = field {
            assert_eq!(response.body["error"]["field"], field, "{body}");
        }
    }
    w.app
        .post("/admin/members")
        .bearer(&w.owner)
        .raw_body("application/json", "{\"login_id\": \"x\",")
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    w.app
        .post("/admin/members")
        .bearer(&w.owner)
        .raw_body("text/plain", new_member("plain").to_string())
        .send()
        .await
        .expect_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type");
    assert_eq!(snapshot(&w.app).await, before);
}

#[tokio::test]
async fn blank_optional_contacts_are_stored_as_none() {
    let w = world().await;
    let created = create(
        &w.app,
        &w.owner,
        &json!({ "login_id": "blank", "name": "빈칸", "email": "  ", "phone": "", "hired_on": "2026-03-02" }),
    )
    .await;
    assert_eq!(created["member"]["email"], Value::Null);
    assert_eq!(created["member"]["phone"], Value::Null);
}

// ── 수정과 낙관적 잠금 ────────────────────────────────────────────────────

#[tokio::test]
async fn profile_update_is_audited_with_changed_fields() {
    let w = world().await;
    let position = w
        .app
        .post("/admin/positions")
        .bearer(&w.owner)
        .json(&json!({ "name": "선임", "level": 30 }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    let other = w.app.add_department("영업팀", None, None).await;
    let updated = update(
        &w.app,
        &w.admin.token,
        w.member.id,
        &json!({
            "version": 1, "name": "김구성", "email": "kim@example.com", "phone": "+82 10 1111 2222",
            "department_id": other, "position_id": position["id"], "hired_on": "2025-01-01"
        }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(updated["version"], 2);
    assert_eq!(updated["department_name"], "영업팀");
    assert_eq!(updated["position_name"], "선임");
    assert_eq!(updated["hired_on"], "2025-01-01");

    let audits = audit_entries(&w.app, "member.update").await;
    assert_eq!(audits.len(), 1);
    let changed = &audits[0].detail.as_ref().expect("detail")["changed"];
    assert_eq!(
        changed["name"],
        json!({ "from": "member 님", "to": "김구성" })
    );
    assert_eq!(changed["department_id"]["to"], other.to_string());
    assert!(changed.get("hired_on").is_some());

    // 바뀐 것이 없으면 버전도 감사 로그도 그대로다.
    let same = update(
        &w.app,
        &w.admin.token,
        w.member.id,
        &json!({
            "version": 2, "name": "김구성", "email": "kim@example.com", "phone": "+82 10 1111 2222",
            "department_id": other, "position_id": position["id"], "hired_on": "2025-01-01"
        }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(same["version"], 2);
    assert_eq!(audit_entries(&w.app, "member.update").await.len(), 1);
}

#[tokio::test]
async fn stale_versions_are_rejected_without_changes() {
    let w = world().await;
    let before = snapshot(&w.app).await;
    for version in [0, 2, -5, i64::MAX] {
        update(&w.app, &w.owner, w.member.id, &profile(version, "바뀜"))
            .await
            .expect_error(StatusCode::CONFLICT, "version_conflict");
        change_status(
            &w.app,
            &w.owner,
            w.member.id,
            json!({ "version": version, "status": "suspended" }),
        )
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
        change_role(&w.app, &w.owner, w.member.id, version, "admin")
            .await
            .expect_error(StatusCode::CONFLICT, "version_conflict");
    }
    assert_eq!(snapshot(&w.app).await, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_profile_updates_with_same_version_have_one_winner() {
    let w = world().await;
    let target = w.member.id;
    let app = Arc::new(w.app);
    let tasks: Vec<_> = ["첫째 이름", "둘째 이름"]
        .into_iter()
        .map(|name| {
            let (app, token) = (app.clone(), w.owner.clone());
            tokio::spawn(async move { update(&app, &token, target, &profile(1, name)).await })
        })
        .collect();
    let mut responses = Vec::new();
    for task in tasks {
        responses.push(task.await.expect("join"));
    }
    let mut statuses: Vec<_> = responses.iter().map(|r| r.status).collect();
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::CONFLICT]);
    let winner = responses
        .iter()
        .find(|r| r.status == StatusCode::OK)
        .expect("winner");
    let stored = row(&app, target).await;
    assert_eq!(stored.version, 2);
    assert_eq!(stored.name, winner.body["name"].as_str().expect("name"));
    assert_eq!(audit_entries(&app, "member.update").await.len(), 1);
}

#[tokio::test]
async fn update_validates_references_and_dates() {
    let w = world().await;
    let gone = w.app.add_member("gone", Role::Member, None).await;
    change_status(
        &w.app,
        &w.owner,
        gone.id,
        json!({ "version": 1, "status": "retired", "retired_on": "2025-01-01" }),
    )
    .await
    .expect(StatusCode::OK);
    let before = snapshot(&w.app).await;
    update(
        &w.app,
        &w.owner,
        w.member.id,
        &json!({ "version": 1, "name": "x", "hired_on": "2024-03-02", "department_id": DepartmentId::new() }),
    )
    .await
    .expect_error(StatusCode::NOT_FOUND, "not_found");
    let response = update(
        &w.app,
        &w.owner,
        gone.id,
        &json!({ "version": 2, "name": "gone 님", "hired_on": "2025-06-01" }),
    )
    .await;
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    for body in [
        json!({ "name": "x", "hired_on": "2024-03-02" }),
        json!({ "version": "1", "name": "x", "hired_on": "2024-03-02" }),
        json!({ "version": 1, "name": "x", "hired_on": "2024-13-01" }),
    ] {
        update(&w.app, &w.owner, w.member.id, &body)
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    for bad in ["not-a-uuid", "123"] {
        w.app
            .patch(&format!("/admin/members/{bad}"))
            .bearer(&w.owner)
            .json(&profile(1, "x"))
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    update(&w.app, &w.owner, MemberId::new(), &profile(1, "x"))
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    assert_eq!(snapshot(&w.app).await, before);
}

// ── 상태 변경 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn suspension_revokes_sessions_and_department_headship_immediately() {
    let w = world().await;
    let second_session = w.app.login("manager", MEMBER_PASSWORD).await;
    w.app
        .get("/team/members")
        .bearer(&w.manager.token)
        .send()
        .await
        .expect(StatusCode::OK);

    let suspended = change_status(
        &w.app,
        &w.admin.token,
        w.manager.id,
        json!({ "version": 1, "status": "suspended" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(suspended["status"], "suspended");
    assert_eq!(suspended["retired_on"], Value::Null);
    for token in [&w.manager.token, &second_session] {
        w.app
            .get("/me")
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    assert_eq!(
        try_login(&w.app, "manager", MEMBER_PASSWORD).await,
        StatusCode::UNAUTHORIZED
    );
    let departments = w
        .app
        .get("/departments")
        .bearer(&w.member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(departments[0]["head_member_id"], Value::Null);
    let detail = &audit_entries(&w.app, "member.status").await[0];
    assert_eq!(detail.detail.as_ref().expect("d")["revoked_sessions"], 2);

    // 복직해도 세션과 조직장 지위는 되살아나지 않는다.
    change_status(
        &w.app,
        &w.admin.token,
        w.manager.id,
        json!({ "version": 2, "status": "active" }),
    )
    .await
    .expect(StatusCode::OK);
    let token = w.app.login("manager", MEMBER_PASSWORD).await;
    w.app
        .get("/me")
        .bearer(&w.manager.token)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    w.app
        .get("/team/members")
        .bearer(&token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
}

#[tokio::test]
async fn repeating_a_status_change_is_an_idempotent_success() {
    let w = world().await;
    let body = json!({ "version": 1, "status": "suspended" });
    let first = change_status(&w.app, &w.owner, w.member.id, body.clone())
        .await
        .expect(StatusCode::OK);
    assert_eq!(first["version"], 2);
    // 같은 요청을 다시 보내면(버전은 이제 낡았지만) 이미 그 상태이므로 성공한다.
    let again = change_status(&w.app, &w.owner, w.member.id, body)
        .await
        .expect(StatusCode::OK);
    assert_eq!(again, first);
    assert_eq!(row(&w.app, w.member.id).await.version, 2);
    assert_eq!(audit_entries(&w.app, "member.status").await.len(), 1);
    let unchanged = change_status(
        &w.app,
        &w.owner,
        w.admin.id,
        json!({ "version": 1, "status": "active" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(unchanged["version"], 1);
    assert_eq!(audit_entries(&w.app, "member.status").await.len(), 1);
}

#[tokio::test]
async fn retirement_date_rules() {
    let w = world().await;
    let before = snapshot(&w.app).await;
    let id = w.member.id;
    // 입사일(2024-03-02)보다 이른 퇴사일, 퇴사가 아닌데 퇴사일, 형식 오류.
    for (body, code) in [
        (
            json!({ "version": 1, "status": "retired", "retired_on": "2024-03-01" }),
            "validation_failed",
        ),
        (
            json!({ "version": 1, "status": "suspended", "retired_on": "2026-03-02" }),
            "validation_failed",
        ),
        (
            json!({ "version": 1, "status": "retired", "retired_on": "2026-02-30" }),
            "malformed_request",
        ),
        (
            json!({ "version": 1, "status": "deleted" }),
            "malformed_request",
        ),
        (
            json!({ "version": 1, "status": "ACTIVE" }),
            "malformed_request",
        ),
        (json!({ "status": "retired" }), "malformed_request"),
        (json!({ "version": 1 }), "malformed_request"),
    ] {
        change_status(&w.app, &w.owner, id, body.clone())
            .await
            .expect_error(StatusCode::BAD_REQUEST, code);
    }
    assert_eq!(snapshot(&w.app).await, before);

    // 입사일 당일 퇴사는 허용.
    let retired = change_status(
        &w.app,
        &w.owner,
        id,
        json!({ "version": 1, "status": "retired", "retired_on": "2024-03-02" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(retired["retired_on"], "2024-03-02");
    // 퇴사 상태에서 퇴사일만 바꾸는 것은 새 변경이다.
    let moved = change_status(
        &w.app,
        &w.owner,
        id,
        json!({ "version": 2, "status": "retired", "retired_on": "2026-02-27" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(moved["retired_on"], "2026-02-27");
    let active = change_status(
        &w.app,
        &w.owner,
        id,
        json!({ "version": 3, "status": "active" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(active["retired_on"], Value::Null);
    // 날짜를 생략하면 회사 시간대 기준 오늘(시작 시각 2026-03-02 09:00 KST).
    let today = change_status(
        &w.app,
        &w.owner,
        id,
        json!({ "version": 4, "status": "retired" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(today["retired_on"], "2026-03-02");
    let audits = audit_entries(&w.app, "member.status").await;
    assert_eq!(audits.len(), 4);
    assert_eq!(audits[0].detail.as_ref().expect("d")["to"], "retired");
}

// ── 최고 관리자 보호 ──────────────────────────────────────────────────────

#[tokio::test]
async fn owner_promotes_and_demotes_with_audit() {
    let w = world().await;
    let promoted = change_role(&w.app, &w.owner, w.member.id, 1, "admin")
        .await
        .expect(StatusCode::OK);
    assert_eq!(promoted["role"], "admin");
    w.app
        .get("/admin/members")
        .bearer(&w.member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    // 같은 역할 재요청은 변화 없이 성공.
    let again = change_role(&w.app, &w.owner, w.member.id, 1, "admin")
        .await
        .expect(StatusCode::OK);
    assert_eq!(again["version"], 2);
    change_role(&w.app, &w.owner, w.member.id, 2, "member")
        .await
        .expect(StatusCode::OK);
    w.app
        .get("/admin/members")
        .bearer(&w.member.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    let audits = audit_entries(&w.app, "member.role").await;
    assert_eq!(audits.len(), 2);
    assert_eq!(
        audits[0].detail.as_ref().expect("d"),
        &json!({ "from": "admin", "to": "member" })
    );
    for role in ["root", "Owner", ""] {
        change_role(&w.app, &w.owner, w.member.id, 3, role)
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
}

#[tokio::test]
async fn sole_owner_can_never_be_removed_through_the_api() {
    let w = world().await;
    let owner_id = w.app.owner_id;
    let before = snapshot(&w.app).await;
    // 본인은 자신의 상태·역할을 바꿀 수 없고, 관리자는 최고 관리자를 건드릴 수 없다.
    for token in [&w.owner, &w.admin.token] {
        for status in ["suspended", "retired"] {
            change_status(
                &w.app,
                token,
                owner_id,
                json!({ "version": 1, "status": status }),
            )
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        }
        change_role(&w.app, token, owner_id, 1, "admin")
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    assert_eq!(snapshot(&w.app).await, before);
}

async fn active_owners(app: &TestApp) -> i64 {
    let mut conn = app.state.read().await.expect("read");
    members::count_active_owners(&mut conn)
        .await
        .expect("count")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owners_demoting_each_other_concurrently_leave_one_owner() {
    for round in 0..5 {
        let app = Arc::new(TestApp::new().await);
        let first = app.login_owner().await;
        let second = app.add_member("owner2", Role::Owner, None).await;
        let pairs = [
            (first.clone(), second.id),
            (second.token.clone(), app.owner_id),
        ];
        let tasks: Vec<_> = pairs
            .into_iter()
            .map(|(token, target)| {
                let app = app.clone();
                tokio::spawn(async move { change_role(&app, &token, target, 1, "admin").await })
            })
            .collect();
        let mut statuses = Vec::new();
        for task in tasks {
            statuses.push(task.await.expect("join").status);
        }
        let ok = statuses.iter().filter(|s| **s == StatusCode::OK).count();
        assert_eq!(ok, 1, "round {round}: {statuses:?}");
        assert!(
            statuses
                .iter()
                .all(|s| [StatusCode::OK, StatusCode::FORBIDDEN, StatusCode::CONFLICT].contains(s)),
            "{statuses:?}"
        );
        assert_eq!(active_owners(&app).await, 1, "round {round}");
        assert_eq!(audit_entries(&app, "member.role").await.len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn owners_suspending_each_other_concurrently_leave_one_owner() {
    for round in 0..5 {
        let app = Arc::new(TestApp::new().await);
        let first = app.login_owner().await;
        let second = app.add_member("owner2", Role::Owner, None).await;
        let pairs = [
            (first.clone(), second.id),
            (second.token.clone(), app.owner_id),
        ];
        let tasks: Vec<_> = pairs
            .into_iter()
            .map(|(token, target)| {
                let app = app.clone();
                tokio::spawn(async move {
                    change_status(
                        &app,
                        &token,
                        target,
                        json!({ "version": 1, "status": "suspended" }),
                    )
                    .await
                })
            })
            .collect();
        let mut statuses = Vec::new();
        for task in tasks {
            statuses.push(task.await.expect("join").status);
        }
        let ok = statuses.iter().filter(|s| **s == StatusCode::OK).count();
        assert_eq!(ok, 1, "round {round}: {statuses:?}");
        assert_eq!(active_owners(&app).await, 1, "round {round}");
    }
}

// ── 비밀번호 초기화 ───────────────────────────────────────────────────────

#[tokio::test]
async fn password_reset_revokes_sessions_and_requires_change() {
    let w = world().await;
    let issued = reset(&w.app, &w.admin.token, w.member.id)
        .await
        .expect(StatusCode::OK);
    let temporary = issued["temporary_password"]
        .as_str()
        .expect("pw")
        .to_owned();
    assert_eq!(issued["member"]["must_change_password"], true);
    w.app
        .get("/me")
        .bearer(&w.member.token)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    assert_eq!(
        try_login(&w.app, "member", MEMBER_PASSWORD).await,
        StatusCode::UNAUTHORIZED
    );
    let token = w.app.login("member", &temporary).await;
    w.app
        .get("/members")
        .bearer(&token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "password_change_required");

    // 다시 초기화하면 다른 임시 비밀번호가 나오고 앞의 것은 무효다.
    let second = reset(&w.app, &w.admin.token, w.member.id)
        .await
        .expect(StatusCode::OK);
    assert_ne!(second["temporary_password"], issued["temporary_password"]);
    assert_eq!(
        try_login(&w.app, "member", &temporary).await,
        StatusCode::UNAUTHORIZED
    );

    let audits = audit_entries(&w.app, "member.password_reset").await;
    assert_eq!(audits.len(), 2);
    assert_eq!(audits[1].detail.as_ref().expect("d")["revoked_sessions"], 1);
    for entry in all_audit(&w.app).await {
        let text = serde_json::to_string(&entry.detail).expect("json");
        assert!(!text.contains(&temporary));
        assert!(!text.contains(second["temporary_password"].as_str().expect("pw")));
    }
    reset(&w.app, &w.admin.token, MemberId::new())
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

// ── 비밀 값 노출 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn responses_never_expose_password_hashes() {
    let w = world().await;
    let created = create(&w.app, &w.owner, &new_member("hashcheck")).await;
    let id = created["member"]["id"].as_str().expect("id").to_owned();
    let bodies = [
        created,
        w.app
            .get("/members")
            .bearer(&w.member.token)
            .send()
            .await
            .body,
        w.app
            .get(&format!("/members/{id}"))
            .bearer(&w.member.token)
            .send()
            .await
            .body,
        w.app
            .get("/team/members")
            .bearer(&w.manager.token)
            .send()
            .await
            .body,
        w.app
            .get("/admin/members?include_inactive=true")
            .bearer(&w.owner)
            .send()
            .await
            .body,
        w.app
            .get(&format!("/admin/members/{id}"))
            .bearer(&w.owner)
            .send()
            .await
            .body,
        reset(&w.app, &w.owner, w.member.id).await.body,
        update(&w.app, &w.owner, w.admin.id, &profile(1, "x"))
            .await
            .body,
    ];
    for body in bodies {
        let text = body.to_string();
        assert!(!text.contains("password_hash"), "{text}");
        assert!(!text.contains("$argon2"), "{text}");
    }
}

#[tokio::test]
async fn admin_member_list_filters_and_includes_inactive_on_request() {
    let w = world().await;
    let gone = w.app.add_member("gone", Role::Member, Some(w.dept)).await;
    set_status(&w.app, gone.id, MemberStatus::Retired).await;
    let list = w
        .app
        .get(&format!("/admin/members?department_id={}", w.dept))
        .bearer(&w.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(names(&list), ["manager 님", "member 님"]);
    assert_eq!(list[0]["login_id"], "manager");
    let all = w
        .app
        .get(&format!(
            "/admin/members?department_id={}&include_inactive=true&search=gone",
            w.dept
        ))
        .bearer(&w.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(names(&all), ["gone 님"]);
    assert_eq!(all[0]["status"], "retired");
    w.app
        .get("/admin/members/not-a-uuid")
        .bearer(&w.admin.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn directory_search_cannot_probe_login_ids() {
    let app = TestApp::new().await;
    // 최고 관리자: 아이디 `owner`, 이름 `대표`.
    let searcher = app.add_member("searcher", Role::Member, None).await;
    let search = |path: &'static str, token: String| {
        let app = &app;
        async move {
            app.get(path)
                .bearer(&token)
                .send()
                .await
                .expect(StatusCode::OK)
        }
    };

    // 일반 구성원: 아이디 조각으로는 찾을 수 없고 이름으로만 찾는다.
    let by_login = search("/members?search=owne", searcher.token.clone()).await;
    assert_eq!(by_login.as_array().expect("array").len(), 0);
    let by_name = search("/members?search=%EB%8C%80%ED%91%9C", searcher.token.clone()).await;
    assert_eq!(by_name[0]["id"], app.owner_id.to_string());
    assert!(by_name[0].get("login_id").is_none());

    // 관리자 조회는 아이디로도 찾는다.
    let admin = search("/admin/members?search=owne", app.login_owner().await).await;
    assert_eq!(admin.as_array().expect("array").len(), 1);
}

// ── 날짜 범위 ─────────────────────────────────────────────────────────────

fn field_error(response: TestResponse, field: &str) {
    assert_eq!(
        (
            response.status,
            response.error_code(),
            response.body["error"]["field"].as_str()
        ),
        (StatusCode::BAD_REQUEST, "validation_failed", Some(field)),
        "body: {}",
        response.body
    );
}

async fn listings_work(w: &World) {
    for (path, token) in [
        ("/members", &w.member.token),
        ("/admin/members?include_inactive=true", &w.owner),
        ("/team/members", &w.manager.token),
        ("/admin/leave/balances", &w.owner),
        ("/admin/dashboard", &w.owner),
    ] {
        w.app
            .get(path)
            .bearer(token)
            .send()
            .await
            .expect(StatusCode::OK);
    }
}

/// chrono는 `0202-03-02`와 `+10000-01-01`도 날짜로 읽는다. 저장되면 그 행을 다시 읽지 못해
/// 구성원 목록 전체가 500이 되므로 경계에서 거부해야 한다.
#[tokio::test]
async fn hire_dates_that_cannot_be_read_back_are_rejected_and_listings_keep_working() {
    let w = world().await;
    let before = snapshot(&w.app).await;
    // 오늘은 2026-03-02. 1950-01-01 이전과 1년 뒤(2027-03-02) 이후는 받지 않는다.
    for bad in [
        "0202-03-02",
        "+10000-01-01",
        "-0001-01-01",
        "1899-12-31",
        "1949-12-31",
        "2027-03-03",
        "9999-12-31",
    ] {
        let mut body = new_member("far");
        body["hired_on"] = json!(bad);
        let created = w
            .app
            .post("/admin/members")
            .bearer(&w.owner)
            .json(&body)
            .send()
            .await;
        field_error(created, "hired_on");
        let mut body = profile(1, "member 님");
        body["hired_on"] = json!(bad);
        field_error(
            update(&w.app, &w.owner, w.member.id, &body).await,
            "hired_on",
        );
    }
    assert_eq!(snapshot(&w.app).await, before);
    listings_work(&w).await;

    // 경계값은 받는다.
    for (login_id, ok) in [("oldest", "1950-01-01"), ("newest", "2027-03-02")] {
        let mut body = new_member(login_id);
        body["hired_on"] = json!(ok);
        assert_eq!(
            create(&w.app, &w.owner, &body).await["member"]["hired_on"],
            ok
        );
    }
    let mut body = profile(1, "member 님");
    body["hired_on"] = json!("1950-01-01");
    update(&w.app, &w.owner, w.member.id, &body)
        .await
        .expect(StatusCode::OK);
    listings_work(&w).await;
}

#[tokio::test]
async fn retirement_dates_that_cannot_be_read_back_are_rejected() {
    let w = world().await;
    let before = snapshot(&w.app).await;
    for bad in ["+10000-01-01", "+262142-12-31"] {
        field_error(
            change_status(
                &w.app,
                &w.owner,
                w.admin.id,
                json!({ "version": 1, "status": "retired", "retired_on": bad }),
            )
            .await,
            "retired_on",
        );
    }
    // 입사일(2024-03-02)보다 이른 날은 표기 범위 밖이든 안이든 거부한다.
    for bad in ["0202-03-02", "2024-03-01"] {
        field_error(
            change_status(
                &w.app,
                &w.owner,
                w.admin.id,
                json!({ "version": 1, "status": "retired", "retired_on": bad }),
            )
            .await,
            "retired_on",
        );
    }
    assert_eq!(snapshot(&w.app).await, before);
    listings_work(&w).await;
    change_status(
        &w.app,
        &w.owner,
        w.admin.id,
        json!({ "version": 1, "status": "retired", "retired_on": "9999-12-31" }),
    )
    .await
    .expect(StatusCode::OK);
    listings_work(&w).await;
}

/// 예전에 저장된(지금 기준으로는 창 밖인) 입사일은, 그 값을 그대로 둔 채 다른 항목을 고칠 수 있다.
#[tokio::test]
async fn legacy_hire_date_outside_the_window_can_stay_while_other_fields_change() {
    let w = world().await;
    let mut tx = w.app.state.db.begin_write().await.expect("begin");
    let mut member = members::find(&mut tx, w.member.id)
        .await
        .expect("find")
        .expect("m");
    member.hired_on = "1940-05-01".parse().expect("date");
    assert!(members::update(&mut tx, &member, w.app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");

    let mut body = profile(2, "새 이름");
    body["hired_on"] = json!("1940-05-01");
    let updated = update(&w.app, &w.owner, w.member.id, &body)
        .await
        .expect(StatusCode::OK);
    assert_eq!(updated["name"], "새 이름");
    // 다른 창 밖 날짜로 바꾸는 것은 안 된다.
    let mut body = profile(3, "새 이름");
    body["hired_on"] = json!("1940-05-02");
    field_error(
        update(&w.app, &w.owner, w.member.id, &body).await,
        "hired_on",
    );
}
