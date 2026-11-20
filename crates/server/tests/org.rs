//! 부서·직책 통합 테스트.

use std::sync::Arc;

use http::StatusCode;
use pango_core::authz::Role;
use pango_core::id::{DepartmentId, MemberId, PositionId};
use pango_core::member::MemberStatus;
use pango_core::org::MAX_DEPTH;
use pango_db::audit::{self, AuditEntry, AuditFilter};
use pango_db::members;
use pango_db::org::{self, Department};
use pango_server::testing::{TestApp, TestMember};
use serde_json::{json, Value};

struct World {
    app: TestApp,
    owner: String,
    admin: TestMember,
    manager: TestMember,
    member: TestMember,
    dept: DepartmentId,
}

/// 최고 관리자, 관리자, 조직장(부서 head), 일반 구성원이 있는 회사.
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

async fn audit_entries(app: &TestApp, action: &str) -> Vec<AuditEntry> {
    let filter = AuditFilter {
        action: Some(action.to_owned()),
        ..Default::default()
    };
    let mut conn = app.state.read().await.expect("read");
    audit::list(&mut conn, &filter, 200).await.expect("audit").0
}

async fn audit_total(app: &TestApp) -> usize {
    let mut conn = app.state.read().await.expect("read");
    audit::list(&mut conn, &AuditFilter::default(), 200)
        .await
        .expect("audit")
        .0
        .len()
}

async fn department_row(app: &TestApp, id: DepartmentId) -> Department {
    let mut conn = app.state.read().await.expect("read");
    org::find_department(&mut conn, id)
        .await
        .expect("find")
        .expect("department")
}

/// 조직 상태 전체(부서·직책 목록 + 감사 로그 수). 거부된 요청이 아무것도 바꾸지 않았는지 비교한다.
async fn org_snapshot(app: &TestApp) -> (Vec<Department>, Vec<org::Position>, usize) {
    let mut conn = app.state.read().await.expect("read");
    let departments = org::list_departments(&mut conn).await.expect("list");
    let positions = org::list_positions(&mut conn).await.expect("list");
    drop(conn);
    (departments, positions, audit_total(app).await)
}

async fn create_department(app: &TestApp, token: &str, body: Value) -> Value {
    app.post("/admin/departments")
        .bearer(token)
        .json(&body)
        .send()
        .await
        .expect(StatusCode::CREATED)
}

fn id_of(value: &Value) -> String {
    value["id"].as_str().expect("id").to_owned()
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

// ── 조회 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn departments_show_active_direct_member_count_and_head_name() {
    let w = world().await;
    let child = w.app.add_department("백엔드", Some(w.dept), None).await;
    w.app.add_member("child1", Role::Member, Some(child)).await;
    let suspended = w.app.add_member("paused", Role::Member, Some(w.dept)).await;
    set_status(&w.app, suspended.id, MemberStatus::Suspended).await;

    let list = w
        .app
        .get("/departments")
        .bearer(&w.member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let items = list.as_array().expect("array");
    assert_eq!(items.len(), 2);
    let dev = items
        .iter()
        .find(|d| d["id"] == w.dept.to_string())
        .expect("dev");
    // 조직장·구성원 2명. 정지된 구성원과 하위 부서 구성원은 세지 않는다.
    assert_eq!(dev["member_count"], 2);
    assert_eq!(dev["head_member_id"], w.manager.id.to_string());
    assert_eq!(dev["head_name"], "manager 님");
    assert_eq!(dev["version"], 2, "조직장 지정으로 버전이 올랐다");
    let backend = items
        .iter()
        .find(|d| d["id"] == child.to_string())
        .expect("child");
    assert_eq!(backend["parent_id"], w.dept.to_string());
    assert_eq!(backend["member_count"], 1);
    assert_eq!(backend["head_name"], Value::Null);
}

#[tokio::test]
async fn org_lists_require_authentication() {
    let app = TestApp::new().await;
    for path in ["/departments", "/positions"] {
        app.get(path)
            .send()
            .await
            .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
}

#[tokio::test]
async fn archived_departments_are_not_listed() {
    let w = world().await;
    let empty = w.app.add_department("해체팀", None, None).await;
    w.app
        .delete(&format!("/admin/departments/{empty}?version=1"))
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    let list = w
        .app
        .get("/departments")
        .bearer(&w.member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert!(list
        .as_array()
        .expect("array")
        .iter()
        .all(|d| d["id"] != empty.to_string()));
}

// ── 부서 관리: 권한 ──────────────────────────────────────────────────────

#[tokio::test]
async fn department_admin_routes_are_forbidden_to_members_and_managers() {
    let w = world().await;
    let before = org_snapshot(&w.app).await;
    let path = format!("/admin/departments/{}", w.dept);
    for token in [&w.member.token, &w.manager.token] {
        w.app
            .post("/admin/departments")
            .bearer(token)
            .json(&json!({ "name": "몰래 만든 팀" }))
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        w.app
            .patch(&path)
            .bearer(token)
            .json(&json!({ "version": 2, "name": "탈취" }))
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        w.app
            .delete(&format!("{path}?version=2"))
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        // 없는 부서여도 403: 관리 경로의 존재 여부와 무관하게 권한부터 거부한다.
        w.app
            .patch(&format!("/admin/departments/{}", DepartmentId::new()))
            .bearer(token)
            .json(&json!({ "version": 1, "name": "x" }))
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    w.app
        .post("/admin/departments")
        .json(&json!({ "name": "익명" }))
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    assert_eq!(org_snapshot(&w.app).await, before);
}

#[tokio::test]
async fn admins_and_owner_manage_departments_with_audit() {
    let w = world().await;
    for (token, name) in [(&w.admin.token, "영업팀"), (&w.owner, "인사팀")] {
        let created = create_department(
            &w.app,
            token,
            json!({ "name": format!("  {name} "), "sort_order": 3, "head_member_id": w.member.id }),
        )
        .await;
        assert_eq!(created["name"], name, "앞뒤 공백은 정리된다");
        assert_eq!(created["version"], 1);
        assert_eq!(created["sort_order"], 3);
        assert_eq!(created["member_count"], 0);
        assert_eq!(created["head_name"], "member 님");
        let id = id_of(&created);

        let updated = w
            .app
            .patch(&format!("/admin/departments/{id}"))
            .bearer(token)
            .json(&json!({ "version": 1, "name": format!("{name}2"), "parent_id": w.dept }))
            .send()
            .await
            .expect(StatusCode::OK);
        assert_eq!(updated["version"], 2);
        assert_eq!(updated["parent_id"], w.dept.to_string());
        assert_eq!(
            updated["head_member_id"],
            Value::Null,
            "생략하면 조직장 해제"
        );

        w.app
            .delete(&format!("/admin/departments/{id}?version=2"))
            .bearer(token)
            .send()
            .await
            .expect(StatusCode::OK);
    }
    assert_eq!(audit_entries(&w.app, "department.create").await.len(), 2);
    let updates = audit_entries(&w.app, "department.update").await;
    assert_eq!(updates.len(), 2);
    let detail = updates[0].detail.as_ref().expect("detail");
    assert!(detail["changed"].get("name").is_some(), "{detail}");
    assert!(detail["changed"].get("parent_id").is_some(), "{detail}");
    assert_eq!(audit_entries(&w.app, "department.archive").await.len(), 2);
}

#[tokio::test]
async fn new_department_head_is_a_manager_immediately() {
    let w = world().await;
    let created = create_department(
        &w.app,
        &w.owner,
        json!({ "name": "기획팀", "head_member_id": w.member.id }),
    )
    .await;
    assert_eq!(created["head_member_id"], w.member.id.to_string());
    let me = w
        .app
        .get("/me")
        .bearer(&w.member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(me["capabilities"]["team"], true);
}

// ── 부서 관리: 입력 ──────────────────────────────────────────────────────

#[tokio::test]
async fn department_input_is_validated_without_side_effects() {
    let w = world().await;
    let before = org_snapshot(&w.app).await;
    let huge = "가".repeat(10_000);
    let cases: Vec<(Value, StatusCode, &str)> = vec![
        (
            json!({ "name": "" }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "name": "   " }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "name": huge }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "name": "a".repeat(51) }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "name": "팀\u{0}" }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "name": "팀\n이름" }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "name": "admin\u{202E}txt" }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "name": 123 }),
            StatusCode::BAD_REQUEST,
            "malformed_request",
        ),
        (json!({}), StatusCode::BAD_REQUEST, "malformed_request"),
        (
            json!({ "name": "팀", "sort_order": "1" }),
            StatusCode::BAD_REQUEST,
            "malformed_request",
        ),
        (
            json!({ "name": "팀", "sort_order": 1.5 }),
            StatusCode::BAD_REQUEST,
            "malformed_request",
        ),
        (
            json!({ "name": "팀", "sort_order": 1_000_001 }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "name": "팀", "sort_order": -1_000_001 }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "name": "팀", "sort_order": i64::MAX }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "name": "팀", "sort_order": 1e30 }),
            StatusCode::BAD_REQUEST,
            "malformed_request",
        ),
        (
            json!({ "name": "팀", "parent_id": "not-a-uuid" }),
            StatusCode::BAD_REQUEST,
            "malformed_request",
        ),
        (
            json!({ "name": "팀", "parent_id": DepartmentId::new() }),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
        (
            json!({ "name": "팀", "head_member_id": MemberId::new() }),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
    ];
    for (body, status, code) in cases {
        let response = w
            .app
            .post("/admin/departments")
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
    }
    let response = w
        .app
        .post("/admin/departments")
        .bearer(&w.owner)
        .json(&json!({ "name": "a".repeat(51) }))
        .send()
        .await;
    assert_eq!(response.body["error"]["field"], "name");
    assert_eq!(response.body["error"]["reason"], "too_long");
    assert_eq!(org_snapshot(&w.app).await, before);
}

#[tokio::test]
async fn sort_order_boundaries_are_accepted() {
    let w = world().await;
    for sort_order in [-1_000_000, 0, 1_000_000] {
        let created = create_department(
            &w.app,
            &w.owner,
            json!({ "name": format!("팀{sort_order}"), "sort_order": sort_order }),
        )
        .await;
        assert_eq!(created["sort_order"], sort_order);
    }
}

#[tokio::test]
async fn markup_in_names_is_stored_as_plain_text() {
    let w = world().await;
    for name in ["<script>alert(1)</script>", "'; DROP TABLE departments;--"] {
        let created = create_department(&w.app, &w.owner, json!({ "name": name })).await;
        assert_eq!(created["name"], name);
    }
}

#[tokio::test]
async fn department_head_must_be_an_active_member() {
    let w = world().await;
    let retired = w.app.add_member("retired", Role::Member, None).await;
    set_status(&w.app, retired.id, MemberStatus::Retired).await;
    let suspended = w.app.add_member("suspended", Role::Member, None).await;
    set_status(&w.app, suspended.id, MemberStatus::Suspended).await;
    let before = org_snapshot(&w.app).await;
    for head in [retired.id, suspended.id] {
        w.app
            .post("/admin/departments")
            .bearer(&w.owner)
            .json(&json!({ "name": "새 팀", "head_member_id": head }))
            .send()
            .await
            .expect_error(StatusCode::CONFLICT, "member_inactive");
        w.app
            .patch(&format!("/admin/departments/{}", w.dept))
            .bearer(&w.owner)
            .json(&json!({ "version": 2, "name": "개발팀", "head_member_id": head }))
            .send()
            .await
            .expect_error(StatusCode::CONFLICT, "member_inactive");
    }
    assert_eq!(org_snapshot(&w.app).await, before);
}

#[tokio::test]
async fn archived_or_unknown_parent_is_not_found() {
    let w = world().await;
    let gone = w.app.add_department("해체팀", None, None).await;
    w.app
        .delete(&format!("/admin/departments/{gone}?version=1"))
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    let before = org_snapshot(&w.app).await;
    for parent in [gone, DepartmentId::new()] {
        w.app
            .post("/admin/departments")
            .bearer(&w.owner)
            .json(&json!({ "name": "하위", "parent_id": parent }))
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
        w.app
            .patch(&format!("/admin/departments/{}", w.dept))
            .bearer(&w.owner)
            .json(&json!({ "version": 2, "name": "개발팀", "parent_id": parent, "head_member_id": w.manager.id }))
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    assert_eq!(org_snapshot(&w.app).await, before);
}

// ── 부서 트리 ─────────────────────────────────────────────────────────────

/// 루트부터 `len`단계 깊이의 부서 사슬을 API로 만든다.
async fn chain(app: &TestApp, token: &str, len: usize) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for depth in 1..=len {
        let parent = ids.last().cloned();
        let created = create_department(
            app,
            token,
            json!({ "name": format!("깊이{depth}"), "parent_id": parent }),
        )
        .await;
        ids.push(id_of(&created));
    }
    ids
}

#[tokio::test]
async fn depth_limit_is_exact() {
    let w = world().await;
    let ids = chain(&w.app, &w.owner, MAX_DEPTH).await;
    assert_eq!(ids.len(), MAX_DEPTH, "정확히 최대 깊이까지는 만들 수 있다");
    let before = org_snapshot(&w.app).await;
    w.app
        .post("/admin/departments")
        .bearer(&w.owner)
        .json(&json!({ "name": "너무 깊음", "parent_id": ids[MAX_DEPTH - 1] }))
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "department_too_deep");
    assert_eq!(org_snapshot(&w.app).await, before);
}

#[tokio::test]
async fn moving_a_subtree_respects_depth_and_cycles() {
    let w = world().await;
    let ids = chain(&w.app, &w.owner, MAX_DEPTH - 1).await;
    // 높이 2인 서브트리: dept ─ child
    let child = w.app.add_department("하위", Some(w.dept), None).await;
    let before = org_snapshot(&w.app).await;
    let patch = |id: String, version: i64, parent: Value| {
        let app = &w.app;
        let owner = w.owner.clone();
        async move {
            app.patch(&format!("/admin/departments/{id}"))
                .bearer(&owner)
                .json(&json!({ "version": version, "name": "이동", "parent_id": parent }))
                .send()
                .await
        }
    };
    // 깊이 MAX-1 아래에 높이 2를 붙이면 MAX+1.
    patch(w.dept.to_string(), 2, json!(ids[MAX_DEPTH - 2]))
        .await
        .expect_error(StatusCode::CONFLICT, "department_too_deep");
    // 자기 하위 부서 밑으로, 또는 자기 자신 밑으로.
    patch(w.dept.to_string(), 2, json!(child))
        .await
        .expect_error(StatusCode::CONFLICT, "department_cycle");
    patch(w.dept.to_string(), 2, json!(w.dept))
        .await
        .expect_error(StatusCode::CONFLICT, "department_cycle");
    patch(ids[0].clone(), 1, json!(ids[MAX_DEPTH - 2]))
        .await
        .expect_error(StatusCode::CONFLICT, "department_cycle");
    assert_eq!(org_snapshot(&w.app).await, before);

    // 높이 1인 잎은 붙일 수 있다(정확히 MAX).
    let moved = patch(child.to_string(), 1, json!(ids[MAX_DEPTH - 2]))
        .await
        .expect(StatusCode::OK);
    assert_eq!(moved["parent_id"], ids[MAX_DEPTH - 2].as_str());
    // 루트로 올리는 것도 가능.
    let root = patch(child.to_string(), 2, Value::Null)
        .await
        .expect(StatusCode::OK);
    assert_eq!(root["parent_id"], Value::Null);
}

// ── 낙관적 잠금 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn stale_department_version_is_rejected() {
    let w = world().await;
    let before = department_row(&w.app, w.dept).await;
    for version in [1, 3, -1, i64::MAX] {
        w.app
            .patch(&format!("/admin/departments/{}", w.dept))
            .bearer(&w.owner)
            .json(
                &json!({ "version": version, "name": "바뀐 이름", "head_member_id": w.manager.id }),
            )
            .send()
            .await
            .expect_error(StatusCode::CONFLICT, "version_conflict");
        w.app
            .delete(&format!("/admin/departments/{}?version={version}", w.dept))
            .bearer(&w.owner)
            .send()
            .await
            .expect_error(StatusCode::CONFLICT, "version_conflict");
    }
    assert_eq!(department_row(&w.app, w.dept).await, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_department_edits_with_same_version_have_one_winner() {
    let World {
        app,
        owner,
        manager,
        dept,
        ..
    } = world().await;
    let app = Arc::new(app);
    let tasks: Vec<_> = ["가 이름", "나 이름"]
        .into_iter()
        .map(|name| {
            let (app, owner) = (app.clone(), owner.clone());
            let body = json!({ "version": 2, "name": name, "head_member_id": manager.id });
            tokio::spawn(async move {
                app.patch(&format!("/admin/departments/{dept}"))
                    .bearer(&owner)
                    .json(&body)
                    .send()
                    .await
            })
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
    let row = department_row(&app, dept).await;
    assert_eq!(row.version, 3);
    assert_eq!(row.name, winner.body["name"].as_str().expect("name"));
    assert_eq!(audit_entries(&app, "department.update").await.len(), 1);
}

#[tokio::test]
async fn unchanged_patch_is_a_no_op() {
    let w = world().await;
    let audits = audit_total(&w.app).await;
    let same = w
        .app
        .patch(&format!("/admin/departments/{}", w.dept))
        .bearer(&w.owner)
        .json(&json!({ "version": 2, "name": "개발팀", "head_member_id": w.manager.id }))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(same["version"], 2);
    assert_eq!(audit_total(&w.app).await, audits);
}

// ── 보관 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn archive_is_refused_while_members_or_children_remain() {
    let w = world().await;
    let before = org_snapshot(&w.app).await;
    let archive = |id: DepartmentId, version: i64| {
        w.app
            .delete(&format!("/admin/departments/{id}?version={version}"))
            .bearer(&w.owner)
            .send()
    };
    archive(w.dept, 2)
        .await
        .expect_error(StatusCode::CONFLICT, "department_not_empty");
    assert_eq!(org_snapshot(&w.app).await, before);

    // 정지된 구성원만 남아도 비어 있지 않다.
    let paused_dept = w.app.add_department("휴직팀", None, None).await;
    let paused = w
        .app
        .add_member("paused", Role::Member, Some(paused_dept))
        .await;
    set_status(&w.app, paused.id, MemberStatus::Suspended).await;
    archive(paused_dept, 1)
        .await
        .expect_error(StatusCode::CONFLICT, "department_not_empty");

    // 퇴사자만 남으면 보관할 수 있다.
    set_status(&w.app, paused.id, MemberStatus::Retired).await;
    archive(paused_dept, 1).await.expect(StatusCode::OK);

    // 보관되지 않은 하위 부서가 있으면 거부.
    let parent = w.app.add_department("상위", None, None).await;
    let child = w.app.add_department("하위", Some(parent), None).await;
    archive(parent, 1)
        .await
        .expect_error(StatusCode::CONFLICT, "department_not_empty");
    archive(child, 1).await.expect(StatusCode::OK);
    archive(parent, 1).await.expect(StatusCode::OK);
    assert!(department_row(&w.app, parent).await.archived_at.is_some());

    // 이미 보관된 부서는 없는 것으로 취급한다.
    archive(parent, 2)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    w.app
        .patch(&format!("/admin/departments/{parent}"))
        .bearer(&w.owner)
        .json(&json!({ "version": 2, "name": "되살리기" }))
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn archive_requires_a_numeric_version_and_valid_id() {
    let w = world().await;
    let empty = w.app.add_department("빈 팀", None, None).await;
    let before = org_snapshot(&w.app).await;
    for query in ["", "?version=", "?version=abc", "?version=1.0", "?ver=1"] {
        w.app
            .delete(&format!("/admin/departments/{empty}{query}"))
            .bearer(&w.owner)
            .send()
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    for bad in [
        "not-a-uuid",
        "1",
        "../../members",
        &empty.to_string().to_uppercase(),
    ] {
        let response = w
            .app
            .delete(&format!("/admin/departments/{bad}?version=1"))
            .bearer(&w.owner)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::NOT_FOUND, "{bad}");
    }
    w.app
        .delete(&format!(
            "/admin/departments/{}?version=1",
            DepartmentId::new()
        ))
        .bearer(&w.owner)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    assert_eq!(org_snapshot(&w.app).await, before);
}

#[tokio::test]
async fn department_create_honours_idempotency_key() {
    let w = world().await;
    let send = |name: &str| {
        w.app
            .post("/admin/departments")
            .bearer(&w.owner)
            .header("idempotency-key", "dept-create-1")
            .json(&json!({ "name": name }))
            .send()
    };
    let first = send("재무팀").await.expect(StatusCode::CREATED);
    let again = send("재무팀").await.expect(StatusCode::CREATED);
    assert_eq!(first, again);
    send("다른 팀")
        .await
        .expect_error(StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused");
    assert_eq!(audit_entries(&w.app, "department.create").await.len(), 1);
}

// ── 직책 ─────────────────────────────────────────────────────────────────

async fn create_position(app: &TestApp, token: &str, name: &str, level: i64) -> Value {
    app.post("/admin/positions")
        .bearer(token)
        .json(&json!({ "name": name, "level": level }))
        .send()
        .await
        .expect(StatusCode::CREATED)
}

#[tokio::test]
async fn positions_are_listed_by_level_for_everyone() {
    let w = world().await;
    create_position(&w.app, &w.owner, "사원", 10).await;
    create_position(&w.app, &w.admin.token, "팀장", 50).await;
    let list = w
        .app
        .get("/positions")
        .bearer(&w.member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let names: Vec<_> = list
        .as_array()
        .expect("array")
        .iter()
        .map(|p| p["name"].as_str().expect("name").to_owned())
        .collect();
    assert_eq!(names, ["팀장", "사원"]);
    assert_eq!(list[0]["version"], 1);
}

#[tokio::test]
async fn position_admin_routes_are_forbidden_to_non_admins() {
    let w = world().await;
    let position = create_position(&w.app, &w.owner, "사원", 10).await;
    let id = id_of(&position);
    let before = org_snapshot(&w.app).await;
    for token in [&w.member.token, &w.manager.token] {
        w.app
            .post("/admin/positions")
            .bearer(token)
            .json(&json!({ "name": "회장", "level": 1000 }))
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        w.app
            .patch(&format!("/admin/positions/{id}"))
            .bearer(token)
            .json(&json!({ "version": 1, "name": "회장", "level": 1000 }))
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        w.app
            .delete(&format!("/admin/positions/{id}?version=1"))
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    assert_eq!(org_snapshot(&w.app).await, before);
}

#[tokio::test]
async fn position_level_boundaries_and_types() {
    let w = world().await;
    create_position(&w.app, &w.owner, "최하", 0).await;
    create_position(&w.app, &w.owner, "최상", 1000).await;
    let before = org_snapshot(&w.app).await;
    let cases: Vec<(Value, &str)> = vec![
        (json!({ "name": "음수", "level": -1 }), "validation_failed"),
        (
            json!({ "name": "초과", "level": 1001 }),
            "validation_failed",
        ),
        (
            json!({ "name": "거대", "level": i64::MIN }),
            "validation_failed",
        ),
        (
            json!({ "name": "문자", "level": "10" }),
            "malformed_request",
        ),
        (
            json!({ "name": "범위 밖", "level": 1e20 }),
            "malformed_request",
        ),
        (json!({ "level": 10 }), "malformed_request"),
        (json!({ "name": "", "level": 10 }), "validation_failed"),
        (
            json!({ "name": "x".repeat(10_000), "level": 10 }),
            "validation_failed",
        ),
        (
            json!({ "name": "\u{200B}", "level": 10 }),
            "validation_failed",
        ),
    ];
    for (body, code) in cases {
        w.app
            .post("/admin/positions")
            .bearer(&w.owner)
            .json(&body)
            .send()
            .await
            .expect_error(StatusCode::BAD_REQUEST, code);
    }
    let response = w
        .app
        .post("/admin/positions")
        .bearer(&w.owner)
        .json(&json!({ "name": "초과", "level": 1001 }))
        .send()
        .await;
    assert_eq!(response.body["error"]["field"], "level");
    assert_eq!(org_snapshot(&w.app).await, before);
}

#[tokio::test]
async fn duplicate_active_position_name_conflicts() {
    let w = world().await;
    let lead = create_position(&w.app, &w.owner, "팀장", 50).await;
    let staff = create_position(&w.app, &w.owner, "사원", 10).await;
    let before = org_snapshot(&w.app).await;
    w.app
        .post("/admin/positions")
        .bearer(&w.owner)
        .json(&json!({ "name": " 팀장 ", "level": 40 }))
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "already_exists");
    w.app
        .patch(&format!("/admin/positions/{}", id_of(&staff)))
        .bearer(&w.owner)
        .json(&json!({ "version": 1, "name": "팀장", "level": 10 }))
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "already_exists");
    assert_eq!(org_snapshot(&w.app).await, before);

    // 보관하면 같은 이름을 다시 쓸 수 있다.
    w.app
        .delete(&format!("/admin/positions/{}?version=1", id_of(&lead)))
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    create_position(&w.app, &w.owner, "팀장", 60).await;
    assert_eq!(audit_entries(&w.app, "position.create").await.len(), 3);
    assert_eq!(audit_entries(&w.app, "position.archive").await.len(), 1);
}

#[tokio::test]
async fn position_in_use_cannot_be_archived_until_holders_retire() {
    let w = world().await;
    let position = create_position(&w.app, &w.owner, "주임", 20).await;
    let id: PositionId = id_of(&position).parse().expect("id");
    let mut tx = w.app.state.db.begin_write().await.expect("begin");
    let mut holder = members::find(&mut tx, w.member.id)
        .await
        .expect("find")
        .expect("m");
    holder.position_id = Some(id);
    assert!(members::update(&mut tx, &holder, w.app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");

    let path = format!("/admin/positions/{id}?version=1");
    for status in [MemberStatus::Active, MemberStatus::Suspended] {
        set_status(&w.app, w.member.id, status).await;
        w.app
            .delete(&path)
            .bearer(&w.owner)
            .send()
            .await
            .expect_error(StatusCode::CONFLICT, "in_use");
    }
    set_status(&w.app, w.member.id, MemberStatus::Retired).await;
    w.app
        .delete(&path)
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    let list = w
        .app
        .get("/positions")
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn position_update_versioning_and_audit() {
    let w = world().await;
    let position = create_position(&w.app, &w.owner, "대리", 30).await;
    let path = format!("/admin/positions/{}", id_of(&position));
    let updated = w
        .app
        .patch(&path)
        .bearer(&w.admin.token)
        .json(&json!({ "version": 1, "name": "선임", "level": 35 }))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(updated["version"], 2);
    assert_eq!(updated["level"], 35);
    w.app
        .patch(&path)
        .bearer(&w.admin.token)
        .json(&json!({ "version": 1, "name": "책임", "level": 40 }))
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    w.app
        .patch(&format!("/admin/positions/{}", PositionId::new()))
        .bearer(&w.admin.token)
        .json(&json!({ "version": 1, "name": "책임", "level": 40 }))
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    let audits = audit_entries(&w.app, "position.update").await;
    assert_eq!(audits.len(), 1);
    let detail = audits[0].detail.as_ref().expect("detail");
    assert_eq!(
        detail["changed"]["name"],
        json!({ "from": "대리", "to": "선임" })
    );
    assert_eq!(detail["changed"]["level"], json!({ "from": 30, "to": 35 }));
}
