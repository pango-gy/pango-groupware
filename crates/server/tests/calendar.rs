//! 캘린더 통합 테스트.

use std::sync::Arc;

use http::StatusCode;
use pango_core::authz::Role;
use pango_core::id::{DepartmentId, MemberId};
use pango_core::member::MemberStatus;
use pango_db::notifications::{self, Notification};
use pango_server::service::calendar as service;
use pango_server::testing::{denial, ts, TestApp, TestMember, TestResponse};
use serde_json::{json, Value};

const COMPANY: &str = "00000000-0000-7000-8000-000000000001";

// ── 준비 ────────────────────────────────────────────────────────────────

fn ms(rfc3339: &str) -> i64 {
    ts(rfc3339).as_millis()
}

fn timed(title: &str, start: &str, end: &str) -> Value {
    json!({ "title": title, "all_day": false, "start_at": ms(start), "end_at": ms(end) })
}

fn all_day(title: &str, start: &str, end: &str) -> Value {
    json!({ "title": title, "all_day": true, "start_date": start, "end_date": end })
}

fn with(mut body: Value, key: &str, value: Value) -> Value {
    body[key] = value;
    body
}

fn meeting(title: &str) -> Value {
    timed(title, "2026-03-03T01:00:00Z", "2026-03-03T02:00:00Z")
}

async fn create(app: &TestApp, token: &str, body: &Value) -> TestResponse {
    app.post("/calendar/events")
        .bearer(token)
        .json(body)
        .send()
        .await
}

async fn created(app: &TestApp, token: &str, body: &Value) -> Value {
    create(app, token, body).await.expect(StatusCode::CREATED)
}

fn id_of(body: &Value) -> String {
    body["id"].as_str().expect("id").to_owned()
}

async fn detail(app: &TestApp, token: &str, id: &str) -> TestResponse {
    app.get(&format!("/calendar/events/{id}"))
        .bearer(token)
        .send()
        .await
}

async fn patch(app: &TestApp, token: &str, id: &str, version: i64, body: &Value) -> TestResponse {
    app.patch(&format!("/calendar/events/{id}"))
        .bearer(token)
        .json(&with(body.clone(), "version", json!(version)))
        .send()
        .await
}

async fn delete(app: &TestApp, token: &str, id: &str, version: i64) -> TestResponse {
    app.delete(&format!("/calendar/events/{id}?version={version}"))
        .bearer(token)
        .send()
        .await
}

async fn respond(app: &TestApp, token: &str, id: &str, status: &str) -> TestResponse {
    app.post(&format!("/calendar/events/{id}/respond"))
        .bearer(token)
        .json(&json!({ "status": status }))
        .send()
        .await
}

async fn view_response(app: &TestApp, token: &str, query: &str) -> TestResponse {
    app.get(&format!("/calendar/view?{query}"))
        .bearer(token)
        .send()
        .await
}

async fn view(app: &TestApp, token: &str, from: &str, to: &str) -> Value {
    view_response(app, token, &format!("from={from}&to={to}"))
        .await
        .expect(StatusCode::OK)
}

fn event_ids(view: &Value) -> Vec<String> {
    view["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(id_of)
        .collect()
}

fn find_event<'a>(view: &'a Value, id: &str) -> Option<&'a Value> {
    view["events"]
        .as_array()
        .expect("events")
        .iter()
        .find(|e| e["id"] == id)
}

async fn conflicts(app: &TestApp, token: &str, query: &str) -> TestResponse {
    app.get(&format!("/calendar/conflicts?{query}"))
        .bearer(token)
        .send()
        .await
}

async fn notifications_of(app: &TestApp, member_id: MemberId, kind: &str) -> Vec<Notification> {
    let mut conn = app.state.db.read().await.expect("conn");
    notifications::list(&mut conn, member_id, false, None, 200)
        .await
        .expect("list")
        .0
        .into_iter()
        .filter(|n| n.kind == kind)
        .collect()
}

async fn scalar(app: &TestApp, sql: &str) -> i64 {
    let mut conn = app.state.db.read().await.expect("conn");
    sqlx::query_scalar(sql)
        .fetch_one(&mut *conn)
        .await
        .expect("scalar")
}

async fn audit_count(app: &TestApp, action: &str) -> i64 {
    scalar(
        app,
        &format!("SELECT COUNT(*) FROM audit_log WHERE action = '{action}'"),
    )
    .await
}

/// 표마다 (이름, 줄 수, `version` 합계). 거부된 요청이 아무것도 바꾸지 않았는지 비교한다.
/// 세션 표는 인증이 마지막 사용 시각을 갱신하므로 뺀다.
async fn fingerprint(app: &TestApp) -> Vec<(String, i64, i64)> {
    let mut conn = app.state.db.read().await.expect("read");
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_sqlx_%'
           AND name <> 'sessions'
         ORDER BY name",
    )
    .fetch_all(&mut *conn)
    .await
    .expect("tables");
    let mut out = Vec::new();
    for table in tables {
        let versioned: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = 'version'"
        ))
        .fetch_one(&mut *conn)
        .await
        .expect("columns");
        let sum = if versioned > 0 {
            "COALESCE(SUM(version), 0)"
        } else {
            "0"
        };
        let (rows, versions): (i64, i64) =
            sqlx::query_as(&format!("SELECT COUNT(*), {sum} FROM {table}"))
                .fetch_one(&mut *conn)
                .await
                .expect("count");
        out.push((table, rows, versions));
    }
    out
}

async fn set_status(app: &TestApp, id: MemberId, status: MemberStatus) {
    app.alter_member(id, |member| member.status = status).await;
}

/// 본부(조직장 hqhead) ─ 개발팀(조직장 teamhead: alice, bob) / 영업팀(carol), 관리자.
struct Org {
    owner: String,
    admin: TestMember,
    hq: DepartmentId,
    team: DepartmentId,
    other: DepartmentId,
    hq_head: TestMember,
    team_head: TestMember,
    alice: TestMember,
    bob: TestMember,
    carol: TestMember,
}

async fn org(app: &TestApp) -> Org {
    let hq = app.add_department("본부", None, None).await;
    let team = app.add_department("개발팀", Some(hq), None).await;
    let other = app.add_department("영업팀", Some(hq), None).await;
    let hq_head = app.add_member("hqhead", Role::Member, Some(hq)).await;
    let team_head = app.add_member("teamhead", Role::Member, Some(team)).await;
    app.set_department_head(hq, Some(hq_head.id)).await;
    app.set_department_head(team, Some(team_head.id)).await;
    Org {
        owner: app.login_owner().await,
        admin: app.add_member("admin", Role::Admin, None).await,
        hq,
        team,
        other,
        alice: app.add_member("alice", Role::Member, Some(team)).await,
        bob: app.add_member("bob", Role::Member, Some(team)).await,
        carol: app.add_member("carol", Role::Member, Some(other)).await,
        hq_head,
        team_head,
    }
}

async fn annual_leave_type(app: &TestApp, token: &str) -> String {
    let types = app
        .get("/leave/types")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK);
    types
        .as_array()
        .expect("array")
        .iter()
        .find(|t| t["code"] == "annual")
        .and_then(|t| t["id"].as_str())
        .expect("annual type")
        .to_owned()
}

/// 휴가를 신청한다(사유: "비밀 사유 입니다", 결재자: `approver`). `approve`면 승인까지 한다.
async fn leave(
    app: &TestApp,
    member: &TestMember,
    approver: &TestMember,
    approve: bool,
    date: &str,
) {
    let leave_type_id = annual_leave_type(app, &member.token).await;
    let request = app
        .post("/leave/requests")
        .bearer(&member.token)
        .json(&json!({
            "leave_type_id": leave_type_id, "start_date": date, "end_date": date,
            "unit": "full_day", "reason": "비밀 사유 입니다",
            "steps": [{ "approver_id": approver.id, "kind": "approve" }],
        }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    if approve {
        let document_id = request["document_id"].as_str().expect("document id");
        app.post(&format!("/approval/documents/{document_id}/approve"))
            .bearer(&approver.token)
            .json(&json!({}))
            .send()
            .await
            .expect(StatusCode::OK);
    }
}

// ── 해피 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn personal_event_is_created_edited_and_deleted_by_its_owner() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let body = with(meeting("주간 회의"), "location", json!(" 3층 회의실 "));
    let event = created(&app, &o.alice.token, &body).await;
    assert_eq!(event["calendar_id"], o.alice.id.to_string());
    assert_eq!(event["calendar_kind"], "personal");
    assert_eq!(event["my_role"], "owner");
    assert_eq!(event["location"], "3층 회의실");
    assert_eq!(event["owner_name"], "alice 님");
    assert_eq!(event["start_date"], Value::Null);
    assert_eq!(event["start_at"], ms("2026-03-03T01:00:00Z"));
    assert_eq!(event["version"], 1);
    let id = id_of(&event);

    let fetched = detail(&app, &o.alice.token, &id)
        .await
        .expect(StatusCode::OK);
    assert_eq!(fetched, event);

    let changed = with(
        all_day("워크숍", "2026-03-05", "2026-03-06"),
        "description",
        json!("준비물: 노트북"),
    );
    let updated = patch(&app, &o.alice.token, &id, 1, &changed)
        .await
        .expect(StatusCode::OK);
    assert_eq!(updated["title"], "워크숍");
    assert_eq!(updated["all_day"], true);
    assert_eq!(updated["start_date"], "2026-03-05");
    assert_eq!(updated["end_date"], "2026-03-06");
    assert_eq!(updated["start_at"], Value::Null);
    assert_eq!(updated["location"], Value::Null);
    assert_eq!(updated["description"], "준비물: 노트북");
    assert_eq!(updated["version"], 2);

    delete(&app, &o.alice.token, &id, 2)
        .await
        .expect(StatusCode::OK);
    detail(&app, &o.alice.token, &id)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    // 개인 일정은 감사 로그에 남기지 않는다.
    assert_eq!(
        scalar(
            &app,
            "SELECT COUNT(*) FROM audit_log WHERE action LIKE 'calendar.%'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn company_event_is_written_by_admins_seen_by_everyone_and_audited() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let body = with(
        all_day("창립 기념일", "2026-03-10", "2026-03-10"),
        "calendar_id",
        json!(COMPANY),
    );
    let event = created(&app, &o.admin.token, &body).await;
    assert_eq!(event["calendar_kind"], "company");
    let id = id_of(&event);
    assert_eq!(audit_count(&app, "calendar.event_create").await, 1);

    for token in [&o.alice.token, &o.carol.token, &o.hq_head.token] {
        let seen = view(&app, token, "2026-03-01", "2026-03-31").await;
        let row = find_event(&seen, &id).expect("company event is visible");
        assert_eq!(row["my_role"], "viewer");
        detail(&app, token, &id).await.expect(StatusCode::OK);
        patch(&app, token, &id, 1, &meeting("바꿈"))
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        delete(&app, token, &id, 1)
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    // 최고 관리자도 쓴다.
    patch(
        &app,
        &o.owner,
        &id,
        1,
        &all_day("창립 기념일(변경)", "2026-03-11", "2026-03-11"),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(audit_count(&app, "calendar.event_update").await, 1);
    delete(&app, &o.admin.token, &id, 2)
        .await
        .expect(StatusCode::OK);
    assert_eq!(audit_count(&app, "calendar.event_delete").await, 1);
}

#[tokio::test]
async fn department_event_is_written_by_heads_and_read_by_members_below() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let team_calendar = o.team.to_string();
    let body = with(
        meeting("개발팀 스탠드업"),
        "calendar_id",
        json!(team_calendar),
    );
    let event = created(&app, &o.team_head.token, &body).await;
    let id = id_of(&event);
    assert_eq!(event["calendar_kind"], "department");
    assert_eq!(event["calendar_id"], team_calendar);
    assert_eq!(audit_count(&app, "calendar.event_create").await, 1);

    // 소속 구성원은 읽기만 한다.
    let seen = view(&app, &o.alice.token, "2026-03-01", "2026-03-31").await;
    assert_eq!(
        find_event(&seen, &id).expect("visible")["my_role"],
        "viewer"
    );
    patch(&app, &o.alice.token, &id, 1, &meeting("바꿈"))
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    // 상위 부서 조직장·관리자·최고 관리자는 쓴다.
    let v = patch(&app, &o.hq_head.token, &id, 1, &meeting("본부장이 고침"))
        .await
        .expect(StatusCode::OK);
    assert_eq!(v["my_role"], "owner");
    patch(&app, &o.admin.token, &id, 2, &meeting("관리자가 고침"))
        .await
        .expect(StatusCode::OK);
    patch(&app, &o.owner, &id, 3, &meeting("대표가 고침"))
        .await
        .expect(StatusCode::OK);
    // 형제 부서 구성원에게는 없는 일정이다.
    detail(&app, &o.carol.token, &id)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    let carol_view = view(&app, &o.carol.token, "2026-03-01", "2026-03-31").await;
    assert!(find_event(&carol_view, &id).is_none());
    view_response(
        &app,
        &o.carol.token,
        &format!("from=2026-03-01&to=2026-03-31&calendars={team_calendar}"),
    )
    .await
    .expect_error(StatusCode::NOT_FOUND, "not_found");
    patch(&app, &o.carol.token, &id, 4, &meeting("남의 부서"))
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    delete(&app, &o.carol.token, &id, 4)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn calendar_list_follows_role_and_organization() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let list = |token: String| {
        let app = &app;
        async move {
            let body = app
                .get("/calendar/calendars")
                .bearer(&token)
                .send()
                .await
                .expect(StatusCode::OK);
            body.as_array()
                .expect("array")
                .iter()
                .map(|c| {
                    (
                        c["kind"].as_str().expect("kind").to_owned(),
                        c["name"].as_str().expect("name").to_owned(),
                        c["can_write"].as_bool().expect("can_write"),
                    )
                })
                .collect::<Vec<_>>()
        }
    };
    let c = |kind: &str, name: &str, can_write: bool| (kind.to_owned(), name.to_owned(), can_write);
    assert_eq!(
        list(o.alice.token.clone()).await,
        [
            c("personal", "내 일정", true),
            c("company", "전사 일정", false),
            c("department", "개발팀", false),
            c("department", "본부", false),
        ]
    );
    assert_eq!(
        list(o.team_head.token.clone()).await,
        [
            c("personal", "내 일정", true),
            c("company", "전사 일정", false),
            c("department", "개발팀", true),
            c("department", "본부", false),
        ]
    );
    let all_departments = [
        c("department", "개발팀", true),
        c("department", "본부", true),
        c("department", "영업팀", true),
    ];
    assert_eq!(list(o.hq_head.token.clone()).await[2..], all_departments);
    assert_eq!(
        list(o.hq_head.token.clone()).await[1],
        c("company", "전사 일정", false)
    );
    let admin = list(o.admin.token.clone()).await;
    assert_eq!(admin[1], c("company", "전사 일정", true));
    assert_eq!(admin[2..], all_departments);
    let carol = list(o.carol.token.clone()).await;
    assert_eq!(
        carol[2..],
        [
            c("department", "본부", false),
            c("department", "영업팀", false)
        ]
    );
    // 부서 없는 구성원은 내 일정과 전사 일정만.
    let loner = app.add_member("loner", Role::Member, None).await;
    assert_eq!(list(loner.token).await.len(), 2);
    let _ = (o.hq, o.other);
}

// ── 권한 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn members_cannot_write_company_or_department_calendars() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let before = fingerprint(&app).await;
    let cases = [
        (COMPANY.to_owned(), StatusCode::FORBIDDEN, "forbidden"),
        (o.team.to_string(), StatusCode::FORBIDDEN, "forbidden"),
        (o.hq.to_string(), StatusCode::FORBIDDEN, "forbidden"),
        (o.other.to_string(), StatusCode::NOT_FOUND, "not_found"),
        // 다른 사람의 내 일정.
        (o.bob.id.to_string(), StatusCode::NOT_FOUND, "not_found"),
    ];
    for (calendar_id, status, code) in cases {
        let body = with(meeting("몰래"), "calendar_id", json!(calendar_id));
        create(&app, &o.alice.token, &body)
            .await
            .expect_error(status, code);
    }
    // 조직장도 관할 밖 부서와 전사 일정에는 쓸 수 없다.
    let body = with(meeting("몰래"), "calendar_id", json!(o.other.to_string()));
    create(&app, &o.team_head.token, &body)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    let body = with(meeting("몰래"), "calendar_id", json!(COMPANY));
    create(&app, &o.team_head.token, &body)
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    // 관리자도 남의 내 일정에 쓸 수 없다.
    let body = with(
        meeting("몰래"),
        "calendar_id",
        json!(o.alice.id.to_string()),
    );
    create(&app, &o.admin.token, &body)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    assert_eq!(fingerprint(&app).await, before);
}

#[tokio::test]
async fn personal_events_are_invisible_to_everyone_else_including_admins() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let body = with(meeting("비밀 면접"), "description", json!("이직 준비"));
    let id = id_of(&created(&app, &o.alice.token, &body).await);
    let others = [
        ("colleague", o.bob.token.clone()),
        ("head", o.team_head.token.clone()),
        ("upper head", o.hq_head.token.clone()),
        ("other department", o.carol.token.clone()),
        ("admin", o.admin.token.clone()),
        ("owner", o.owner.clone()),
    ];
    let before = fingerprint(&app).await;
    for (label, token) in &others {
        detail(&app, token, &id)
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
        let seen = view(&app, token, "2026-03-01", "2026-03-31").await;
        assert!(find_event(&seen, &id).is_none(), "{label} sees the event");
        assert!(!seen.to_string().contains("비밀 면접"), "{label}");
        patch(&app, token, &id, 1, &meeting("고침"))
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
        delete(&app, token, &id, 1)
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
        respond(&app, token, &id, "accepted")
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
        // 남의 내 일정을 캘린더로 골라도 없는 캘린더다.
        view_response(
            &app,
            token,
            &format!("from=2026-03-01&to=2026-03-31&calendars={}", o.alice.id),
        )
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    assert_eq!(fingerprint(&app).await, before);
}

#[tokio::test]
async fn attendees_see_and_respond_but_cannot_edit() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let body = with(
        meeting("기획 회의"),
        "attendee_ids",
        json!([o.bob.id, o.carol.id]),
    );
    let event = created(&app, &o.alice.token, &body).await;
    let id = id_of(&event);
    assert_eq!(event["attendee_count"], 2);
    let names: Vec<_> = event["attendees"]
        .as_array()
        .expect("attendees")
        .iter()
        .map(|a| {
            (
                a["name"].as_str().expect("name").to_owned(),
                a["response"].clone(),
            )
        })
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.iter().all(|(_, response)| response == "pending"));

    let seen = view(&app, &o.carol.token, "2026-03-01", "2026-03-31").await;
    let row = find_event(&seen, &id).expect("invited event is visible");
    assert_eq!(row["my_role"], "attendee");
    assert_eq!(row["my_response"], "pending");
    assert_eq!(row["calendar_kind"], "personal");
    let fetched = detail(&app, &o.carol.token, &id)
        .await
        .expect(StatusCode::OK);
    assert_eq!(fetched["my_role"], "attendee");

    let before = fingerprint(&app).await;
    patch(&app, &o.carol.token, &id, 1, &meeting("참석자가 고침"))
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    delete(&app, &o.carol.token, &id, 1)
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    // 주인은 응답하지 않는다. 초대받지 않은 동료에게는 없는 일정이다.
    respond(&app, &o.alice.token, &id, "accepted")
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    respond(&app, &o.team_head.token, &id, "accepted")
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    assert_eq!(fingerprint(&app).await, before);

    let answered = respond(&app, &o.carol.token, &id, "accepted")
        .await
        .expect(StatusCode::OK);
    assert_eq!(answered["my_response"], "accepted");
    let carol_row = answered["attendees"]
        .as_array()
        .expect("attendees")
        .iter()
        .find(|a| a["member_id"] == o.carol.id.to_string())
        .expect("carol")
        .clone();
    assert_eq!(carol_row["response"], "accepted");
    assert!(carol_row["responded_at"].is_i64());
    assert_eq!(
        notifications_of(&app, o.alice.id, "calendar.responded")
            .await
            .len(),
        1
    );

    // 같은 응답을 다시 보내면 아무것도 바뀌지 않는다(알림도 없다).
    let before = fingerprint(&app).await;
    let again = respond(&app, &o.carol.token, &id, "accepted")
        .await
        .expect(StatusCode::OK);
    assert_eq!(again["attendees"], answered["attendees"]);
    assert_eq!(fingerprint(&app).await, before);
    assert_eq!(
        notifications_of(&app, o.alice.id, "calendar.responded")
            .await
            .len(),
        1
    );

    respond(&app, &o.carol.token, &id, "declined")
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        notifications_of(&app, o.alice.id, "calendar.responded")
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn invitations_changes_and_cancellations_notify_attendees() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let body = with(meeting("1:1"), "attendee_ids", json!([o.bob.id]));
    let id = id_of(&created(&app, &o.alice.token, &body).await);
    let invited = notifications_of(&app, o.bob.id, "calendar.invited").await;
    assert_eq!(invited.len(), 1);
    assert_eq!(invited[0].title, "1:1");
    assert_eq!(
        invited[0].link.as_deref(),
        Some(format!("/calendar/events/{id}").as_str())
    );
    // 주인 자신에게는 알리지 않는다.
    assert!(notifications_of(&app, o.alice.id, "calendar.invited")
        .await
        .is_empty());

    // 제목·설명만 바뀌면 다시 알리지 않는다.
    let renamed = with(meeting("1:1 (안건 추가)"), "description", json!("안건"));
    patch(&app, &o.alice.token, &id, 1, &renamed)
        .await
        .expect(StatusCode::OK);
    assert!(notifications_of(&app, o.bob.id, "calendar.updated")
        .await
        .is_empty());
    // 시간이 바뀌면 알린다. 장소가 바뀌어도 알린다.
    let moved = timed("1:1", "2026-03-04T01:00:00Z", "2026-03-04T02:00:00Z");
    patch(&app, &o.alice.token, &id, 2, &moved)
        .await
        .expect(StatusCode::OK);
    let relocated = with(moved.clone(), "location", json!("카페"));
    patch(&app, &o.alice.token, &id, 3, &relocated)
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        notifications_of(&app, o.bob.id, "calendar.updated")
            .await
            .len(),
        2
    );

    // bob을 빼고 carol을 넣는다.
    let swapped = with(relocated.clone(), "attendee_ids", json!([o.carol.id]));
    let updated = patch(&app, &o.alice.token, &id, 4, &swapped)
        .await
        .expect(StatusCode::OK);
    assert_eq!(updated["attendee_count"], 1);
    assert_eq!(
        notifications_of(&app, o.bob.id, "calendar.canceled")
            .await
            .len(),
        1
    );
    assert_eq!(
        notifications_of(&app, o.carol.id, "calendar.invited")
            .await
            .len(),
        1
    );
    // 새로 들어온 사람에게 변경 알림은 가지 않는다(초대에 지금 내용이 있다).
    assert!(notifications_of(&app, o.carol.id, "calendar.updated")
        .await
        .is_empty());
    // 빠진 사람은 더는 볼 수 없다.
    detail(&app, &o.bob.token, &id)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    let bob_view = view(&app, &o.bob.token, "2026-03-01", "2026-03-31").await;
    assert!(find_event(&bob_view, &id).is_none());

    // attendee_ids를 생략하면 참석자는 그대로다.
    let kept = patch(&app, &o.alice.token, &id, 5, &relocated)
        .await
        .expect(StatusCode::OK);
    assert_eq!(kept["attendee_count"], 1);

    delete(&app, &o.alice.token, &id, 6)
        .await
        .expect(StatusCode::OK);
    let canceled = notifications_of(&app, o.carol.id, "calendar.canceled").await;
    assert_eq!(canceled.len(), 1);
    assert_eq!(canceled[0].link, None);
    assert_eq!(
        scalar(&app, "SELECT COUNT(*) FROM calendar_attendees").await,
        0
    );
}

#[tokio::test]
async fn attendees_are_only_allowed_on_personal_events() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let body = with(
        with(meeting("전사 행사"), "calendar_id", json!(COMPANY)),
        "attendee_ids",
        json!([o.bob.id]),
    );
    let response = create(&app, &o.admin.token, &body).await;
    assert_eq!(
        response.body["error"]["field"], "attendee_ids",
        "{}",
        response.body
    );
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");

    let id = id_of(
        &created(
            &app,
            &o.admin.token,
            &with(meeting("전사 행사"), "calendar_id", json!(COMPANY)),
        )
        .await,
    );
    let invite = with(meeting("전사 행사"), "attendee_ids", json!([o.bob.id]));
    patch(&app, &o.admin.token, &id, 1, &invite)
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    // 빈 목록은 괜찮다.
    patch(
        &app,
        &o.admin.token,
        &id,
        1,
        &with(meeting("전사 행사"), "attendee_ids", json!([])),
    )
    .await
    .expect(StatusCode::OK);
}

// ── 경계값 ──────────────────────────────────────────────────────────────

async fn expect_field(response: TestResponse, field: &str, reason: &str) {
    assert_eq!(
        (
            response.status,
            response.error_code(),
            response.body["error"]["field"].as_str().unwrap_or_default(),
            response.body["error"]["reason"]
                .as_str()
                .unwrap_or_default(),
        ),
        (StatusCode::BAD_REQUEST, "validation_failed", field, reason),
        "body: {}",
        response.body
    );
}

#[tokio::test]
async fn event_span_and_title_boundaries() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let t = &o.alice.token;
    // 종일 하루(끝 날짜 생략 = 같은 날).
    let single = created(
        &app,
        t,
        &json!({ "title": "하루", "all_day": true, "start_date": "2026-03-02" }),
    )
    .await;
    assert_eq!(single["end_date"], "2026-03-02");
    created(&app, t, &all_day("31일", "2026-03-01", "2026-03-31")).await;
    expect_field(
        create(&app, t, &all_day("32일", "2026-03-01", "2026-04-01")).await,
        "end_date",
        "out_of_range",
    )
    .await;
    expect_field(
        create(&app, t, &all_day("거꾸로", "2026-03-02", "2026-03-01")).await,
        "end_date",
        "inconsistent",
    )
    .await;
    // 시간 일정: 끝 = 시작은 거부, 1밀리초는 허용, 31일 정확히는 허용, 1밀리초 넘으면 거부.
    let start = ms("2026-03-02T00:00:00Z");
    let at = |end: i64| json!({ "title": "t", "all_day": false, "start_at": start, "end_at": end });
    expect_field(create(&app, t, &at(start)).await, "end_at", "inconsistent").await;
    created(&app, t, &at(start + 1)).await;
    created(&app, t, &at(start + 31 * 86_400_000)).await;
    expect_field(
        create(&app, t, &at(start + 31 * 86_400_000 + 1)).await,
        "end_at",
        "out_of_range",
    )
    .await;
    // 제목 1~200자.
    created(&app, t, &meeting(&"가".repeat(200))).await;
    expect_field(
        create(&app, t, &meeting(&"가".repeat(201))).await,
        "title",
        "too_long",
    )
    .await;
    expect_field(create(&app, t, &meeting("   ")).await, "title", "empty").await;
}

#[tokio::test]
async fn view_and_conflict_range_boundaries() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let t = &o.alice.token;
    view(&app, t, "2026-03-01", "2026-05-01").await; // 62일
    expect_field(
        view_response(&app, t, "from=2026-03-01&to=2026-05-02").await,
        "to",
        "out_of_range",
    )
    .await;
    expect_field(
        view_response(&app, t, "from=2026-03-02&to=2026-03-01").await,
        "to",
        "inconsistent",
    )
    .await;
    view(&app, t, "2026-03-01", "2026-03-01").await;
    expect_field(
        view_response(&app, t, "from=2026-3-01&to=2026-03-31").await,
        "from",
        "invalid_format",
    )
    .await;
    expect_field(
        view_response(&app, t, "from=2026-03-01&to=2026-02-30").await,
        "to",
        "invalid_format",
    )
    .await;
    view_response(&app, t, "from=2026-03-01")
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");

    let me = o.bob.id;
    conflicts(
        &app,
        t,
        &format!("member_ids={me}&from=2026-03-01&to=2026-03-07"),
    )
    .await
    .expect(StatusCode::OK);
    expect_field(
        conflicts(
            &app,
            t,
            &format!("member_ids={me}&from=2026-03-01&to=2026-03-08"),
        )
        .await,
        "to",
        "out_of_range",
    )
    .await;
}

#[tokio::test]
async fn attendee_list_limits_and_validation() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let mut many = Vec::new();
    for n in 0..51 {
        many.push(
            app.add_member(&format!("guest{n}"), Role::Member, None)
                .await
                .id,
        );
    }
    let fifty = with(meeting("큰 회의"), "attendee_ids", json!(many[..50]));
    let event = created(&app, &o.alice.token, &fifty).await;
    assert_eq!(event["attendee_count"], 50);
    set_status(&app, o.carol.id, MemberStatus::Suspended).await;
    let before = fingerprint(&app).await;
    let t = &o.alice.token;
    let invite = |ids: Value| with(meeting("회의"), "attendee_ids", ids);
    expect_field(
        create(&app, t, &invite(json!(many))).await,
        "attendee_ids",
        "out_of_range",
    )
    .await;
    expect_field(
        create(&app, t, &invite(json!([o.bob.id, o.bob.id]))).await,
        "attendee_ids",
        "duplicate",
    )
    .await;
    expect_field(
        create(&app, t, &invite(json!([o.alice.id]))).await,
        "attendee_ids",
        "inconsistent",
    )
    .await;
    create(&app, t, &invite(json!([MemberId::new()])))
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    // 정지된 구성원은 일반 구성원에게 보이지 않으므로 없는 구성원과 같다. 관리자에게는 409.
    create(&app, t, &invite(json!([o.bob.id, o.carol.id])))
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    create(&app, &o.admin.token, &invite(json!([o.carol.id])))
        .await
        .expect_error(StatusCode::CONFLICT, "member_inactive");
    create(&app, t, &invite(json!(["not-a-uuid"])))
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    assert_eq!(fingerprint(&app).await, before);
}

// ── 엣지·코너 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn all_day_dates_are_plain_dates_and_timed_events_use_company_midnight() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let t = &o.alice.token;
    let last_day = id_of(&created(&app, t, &all_day("말일", "2026-03-31", "2026-03-31")).await);
    // 2026-03-31T15:30Z = 2026-04-01 00:30 KST: UTC로는 3월이지만 회사 달력으로는 4월이다.
    let after_midnight = id_of(
        &created(
            &app,
            t,
            &timed("새벽", "2026-03-31T15:30:00Z", "2026-03-31T16:00:00Z"),
        )
        .await,
    );
    // 2026-03-31T14:30Z = 2026-03-31 23:30 KST.
    let before_midnight = id_of(
        &created(
            &app,
            t,
            &timed("밤", "2026-03-31T14:30:00Z", "2026-03-31T14:45:00Z"),
        )
        .await,
    );
    let march = view(&app, t, "2026-03-01", "2026-03-31").await;
    let april = view(&app, t, "2026-04-01", "2026-04-30").await;
    assert_eq!(
        event_ids(&march),
        [last_day.clone(), before_midnight.clone()]
    );
    assert_eq!(event_ids(&april), [after_midnight]);
    let row = find_event(&march, &last_day).expect("all-day");
    assert_eq!(
        (row["start_date"].clone(), row["end_date"].clone()),
        (json!("2026-03-31"), json!("2026-03-31"))
    );
    assert_eq!(row["start_at"], Value::Null);
}

#[tokio::test]
async fn events_crossing_boundaries_appear_once_in_each_view() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let t = &o.alice.token;
    let crossing = id_of(&created(&app, t, &all_day("출장", "2026-03-30", "2026-04-02")).await);
    // 2026-03-10 23:00 KST ~ 2026-03-11 01:00 KST.
    let overnight = id_of(
        &created(
            &app,
            t,
            &timed("야간 점검", "2026-03-10T14:00:00Z", "2026-03-10T16:00:00Z"),
        )
        .await,
    );
    assert_eq!(
        event_ids(&view(&app, t, "2026-03-01", "2026-03-31").await),
        [overnight.clone(), crossing.clone()]
    );
    assert_eq!(
        event_ids(&view(&app, t, "2026-04-01", "2026-04-30").await),
        [crossing.as_str()]
    );
    assert_eq!(
        event_ids(&view(&app, t, "2026-03-01", "2026-04-30").await),
        [overnight.clone(), crossing.clone()]
    );
    assert_eq!(
        event_ids(&view(&app, t, "2026-03-10", "2026-03-10").await),
        [overnight.as_str()]
    );
    assert_eq!(
        event_ids(&view(&app, t, "2026-03-11", "2026-03-11").await),
        [overnight]
    );
    assert!(event_ids(&view(&app, t, "2026-03-12", "2026-03-29").await).is_empty());
}

#[tokio::test]
async fn view_overlays_holidays_and_leaves_without_private_details() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    app.put("/admin/holidays/2026-03-01")
        .bearer(&o.owner)
        .json(&json!({ "name": "삼일절" }))
        .send()
        .await
        .expect(StatusCode::OK);
    leave(&app, &o.bob, &o.team_head, true, "2026-03-04").await;
    leave(&app, &o.carol, &o.hq_head, false, "2026-03-05").await;

    let alice = view(&app, &o.alice.token, "2026-03-01", "2026-03-31").await;
    assert_eq!(
        alice["holidays"],
        json!([{ "date": "2026-03-01", "name": "삼일절" }])
    );
    let leaves = alice["leaves"].as_array().expect("leaves");
    // 같은 부서 동료의 휴가만 보이고, 사유·종류는 없다.
    assert_eq!(leaves.len(), 1);
    assert_eq!(leaves[0]["member_id"], o.bob.id.to_string());
    assert_eq!(leaves[0]["status"], "approved");
    let mut keys: Vec<_> = leaves[0]
        .as_object()
        .expect("object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "end_date",
            "member_id",
            "member_name",
            "start_date",
            "status",
            "unit"
        ]
    );
    assert!(!alice.to_string().contains("비밀 사유"));
    // 관리자는 결재 중인 휴가까지 모두 본다.
    let admin = view(&app, &o.admin.token, "2026-03-01", "2026-03-31").await;
    let statuses: Vec<_> = admin["leaves"]
        .as_array()
        .expect("leaves")
        .iter()
        .map(|l| l["status"].clone())
        .collect();
    assert_eq!(statuses, [json!("approved"), json!("pending")]);
    assert!(!admin.to_string().contains("비밀 사유"));
    // 공휴일·휴가는 일정 표에 복사되지 않는다.
    assert_eq!(
        scalar(&app, "SELECT COUNT(*) FROM calendar_events").await,
        0
    );
}

#[tokio::test]
async fn selecting_calendars_limits_the_events() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let mine = id_of(&created(&app, &o.alice.token, &meeting("내 것")).await);
    let invited = id_of(
        &created(
            &app,
            &o.bob.token,
            &with(meeting("초대"), "attendee_ids", json!([o.alice.id])),
        )
        .await,
    );
    let company = id_of(
        &created(
            &app,
            &o.admin.token,
            &with(meeting("전사"), "calendar_id", json!(COMPANY)),
        )
        .await,
    );
    let team = id_of(
        &created(
            &app,
            &o.team_head.token,
            &with(meeting("팀"), "calendar_id", json!(o.team.to_string())),
        )
        .await,
    );
    let ids = |query: String| {
        let app = &app;
        let token = o.alice.token.clone();
        async move {
            let mut ids = event_ids(
                &view_response(app, &token, &query)
                    .await
                    .expect(StatusCode::OK),
            );
            ids.sort();
            ids
        }
    };
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    let base = "from=2026-03-01&to=2026-03-31";
    assert_eq!(
        ids(base.to_owned()).await,
        sorted(vec![
            mine.clone(),
            invited.clone(),
            company.clone(),
            team.clone()
        ])
    );
    assert_eq!(
        ids(format!("{base}&calendars={COMPANY}")).await,
        [company.as_str()]
    );
    assert_eq!(
        ids(format!("{base}&calendars={}", o.alice.id)).await,
        sorted(vec![mine, invited])
    );
    assert_eq!(
        ids(format!("{base}&calendars={},{COMPANY}", o.team)).await,
        sorted(vec![company, team])
    );
    // 형식이 틀리거나 겹치는 목록은 입력 오류, 없는 캘린더는 404.
    expect_field(
        view_response(&app, &o.alice.token, &format!("{base}&calendars=abc")).await,
        "calendars",
        "invalid_format",
    )
    .await;
    expect_field(
        view_response(
            &app,
            &o.alice.token,
            &format!("{base}&calendars={COMPANY},{COMPANY}"),
        )
        .await,
        "calendars",
        "duplicate",
    )
    .await;
    expect_field(
        view_response(&app, &o.alice.token, &format!("{base}&calendars=")).await,
        "calendars",
        "empty",
    )
    .await;
    view_response(
        &app,
        &o.alice.token,
        &format!("{base}&calendars={}", MemberId::new()),
    )
    .await
    .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn archived_department_calendar_disappears() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let body = with(meeting("팀 회의"), "calendar_id", json!(o.team.to_string()));
    let id = id_of(&created(&app, &o.team_head.token, &body).await);
    {
        let mut tx = app.state.db.begin_write().await.expect("begin");
        sqlx::query("UPDATE departments SET archived_at = 1 WHERE id = ?")
            .bind(o.team.to_string())
            .execute(&mut *tx)
            .await
            .expect("archive");
        tx.commit().await.expect("commit");
    }
    detail(&app, &o.admin.token, &id)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    create(&app, &o.admin.token, &body)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

// ── 멱등성 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn create_replay_with_the_same_key_makes_one_event_and_one_invitation() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let body = with(meeting("재전송"), "attendee_ids", json!([o.bob.id]));
    let send = |body: Value| {
        let app = &app;
        let token = o.alice.token.clone();
        async move {
            app.post("/calendar/events")
                .bearer(&token)
                .header("idempotency-key", "calendar-key-1")
                .json(&body)
                .send()
                .await
        }
    };
    let first = send(body.clone()).await.expect(StatusCode::CREATED);
    let second = send(body.clone()).await.expect(StatusCode::CREATED);
    assert_eq!(first, second);
    assert_eq!(
        scalar(&app, "SELECT COUNT(*) FROM calendar_events").await,
        1
    );
    assert_eq!(
        notifications_of(&app, o.bob.id, "calendar.invited")
            .await
            .len(),
        1
    );
    send(meeting("다른 본문"))
        .await
        .expect_error(StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused");
    assert_eq!(
        scalar(&app, "SELECT COUNT(*) FROM calendar_events").await,
        1
    );
}

// ── 동시성 ──────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creates_with_one_key_make_one_event() {
    let app = Arc::new(TestApp::new().await);
    let o = org(&app).await;
    let body = with(meeting("동시"), "attendee_ids", json!([o.bob.id]));
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (app, token, body) = (app.clone(), o.alice.token.clone(), body.clone());
            tokio::spawn(async move {
                let response = app
                    .post("/calendar/events")
                    .bearer(&token)
                    .header("idempotency-key", "same-key")
                    .json(&body)
                    .send()
                    .await;
                (response.status, response.body["id"].clone())
            })
        })
        .collect();
    let mut ids = Vec::new();
    for task in tasks {
        let (status, id) = task.await.expect("join");
        assert_eq!(status, StatusCode::CREATED);
        ids.push(id);
    }
    ids.dedup();
    assert_eq!(ids.len(), 1);
    assert_eq!(
        scalar(&app, "SELECT COUNT(*) FROM calendar_events").await,
        1
    );
    assert_eq!(
        scalar(&app, "SELECT COUNT(*) FROM calendar_attendees").await,
        1
    );
    assert_eq!(
        notifications_of(&app, o.bob.id, "calendar.invited")
            .await
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_edits_with_one_version_let_one_win() {
    let app = Arc::new(TestApp::new().await);
    let o = org(&app).await;
    let id = id_of(&created(&app, &o.alice.token, &meeting("원래")).await);
    let tasks: Vec<_> = [("A안", o.bob.id), ("B안", o.carol.id)]
        .into_iter()
        .map(|(title, guest)| {
            let (app, token, id) = (app.clone(), o.alice.token.clone(), id.clone());
            tokio::spawn(async move {
                let body = with(meeting(title), "attendee_ids", json!([guest]));
                (title, patch(&app, &token, &id, 1, &body).await)
            })
        })
        .collect();
    let mut winners = Vec::new();
    for task in tasks {
        let (title, response) = task.await.expect("join");
        match response.status {
            StatusCode::OK => winners.push(title),
            _ => response.expect_error(StatusCode::CONFLICT, "version_conflict"),
        }
    }
    assert_eq!(winners.len(), 1);
    let stored = detail(&app, &o.alice.token, &id)
        .await
        .expect(StatusCode::OK);
    assert_eq!(stored["title"], winners[0]);
    assert_eq!(stored["version"], 2);
    // 진 쪽의 참석자는 들어가지 않았고, 알림도 하나뿐이다.
    assert_eq!(stored["attendee_count"], 1);
    let invited = notifications_of(&app, o.bob.id, "calendar.invited")
        .await
        .len()
        + notifications_of(&app, o.carol.id, "calendar.invited")
            .await
            .len();
    assert_eq!(invited, 1);
}

// ── 순서 역전 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn actions_after_removal_or_deletion_find_nothing() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let body = with(meeting("취소될 회의"), "attendee_ids", json!([o.bob.id]));
    let id = id_of(&created(&app, &o.alice.token, &body).await);
    patch(
        &app,
        &o.alice.token,
        &id,
        1,
        &with(meeting("취소될 회의"), "attendee_ids", json!([])),
    )
    .await
    .expect(StatusCode::OK);
    respond(&app, &o.bob.token, &id, "accepted")
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    delete(&app, &o.alice.token, &id, 2)
        .await
        .expect(StatusCode::OK);
    patch(&app, &o.alice.token, &id, 3, &meeting("부활"))
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    delete(&app, &o.alice.token, &id, 3)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    respond(&app, &o.bob.token, &id, "declined")
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    // 지운 뒤에는 남은 참석자 줄도 없다.
    assert_eq!(
        scalar(&app, "SELECT COUNT(*) FROM calendar_attendees").await,
        0
    );
}

#[tokio::test]
async fn stale_version_edit_and_delete_change_nothing() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let body = with(meeting("버전"), "attendee_ids", json!([o.bob.id]));
    let id = id_of(&created(&app, &o.alice.token, &body).await);
    patch(&app, &o.alice.token, &id, 1, &meeting("한 번"))
        .await
        .expect(StatusCode::OK);
    let before = fingerprint(&app).await;
    patch(
        &app,
        &o.alice.token,
        &id,
        1,
        &with(meeting("낡은 버전"), "attendee_ids", json!([o.carol.id])),
    )
    .await
    .expect_error(StatusCode::CONFLICT, "version_conflict");
    delete(&app, &o.alice.token, &id, 1)
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    assert_eq!(fingerprint(&app).await, before);
}

// ── 기형·악의적 입력 ────────────────────────────────────────────────────

#[tokio::test]
async fn malformed_and_hostile_input_is_rejected_without_changes() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let t = &o.alice.token;
    let id = id_of(&created(&app, t, &meeting("기준")).await);
    let before = fingerprint(&app).await;
    let bad_json = [
        json!({ "title": "t", "all_day": false, "start_at": "2026-03-02T00:00:00Z", "end_at": 1 }),
        json!({ "title": "t", "all_day": "yes", "start_date": "2026-03-02" }),
        json!({ "title": "t", "all_day": true, "start_date": "2026-13-02" }),
        json!({ "title": "t", "all_day": true, "start_date": "2026-03-02", "calendar_id": "NOT-A-UUID" }),
        json!({ "title": "t", "all_day": false, "start_at": 1e30, "end_at": 2 }),
        json!({ "all_day": true, "start_date": "2026-03-02" }),
    ];
    for body in bad_json {
        create(&app, t, &body)
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    // 날짜와 시각을 함께 보내면 의도를 알 수 없다.
    let both = json!({
        "title": "t", "all_day": true, "start_date": "2026-03-02",
        "start_at": ms("2026-03-02T00:00:00Z"), "end_at": ms("2026-03-02T01:00:00Z"),
    });
    expect_field(create(&app, t, &both).await, "start_at", "inconsistent").await;
    let both = with(meeting("t"), "start_date", json!("2026-03-02"));
    expect_field(create(&app, t, &both).await, "start_date", "inconsistent").await;
    expect_field(
        create(&app, t, &all_day("옛날", "1899-12-31", "1899-12-31")).await,
        "start_date",
        "out_of_range",
    )
    .await;
    expect_field(
        create(&app, t, &json!({ "title": "t", "all_day": false, "start_at": -3_000_000_000_000_i64, "end_at": 0 })).await,
        "start_at",
        "out_of_range",
    )
    .await;
    // 제어문자·방향 제어 문자·거대한 입력.
    expect_field(
        create(&app, t, &meeting("제목\u{0007}")).await,
        "title",
        "invalid_character",
    )
    .await;
    expect_field(
        create(
            &app,
            t,
            &with(meeting("t"), "location", json!("회의실\u{202E}")),
        )
        .await,
        "location",
        "invalid_character",
    )
    .await;
    expect_field(
        create(
            &app,
            t,
            &with(meeting("t"), "description", json!("a".repeat(5_001))),
        )
        .await,
        "description",
        "too_long",
    )
    .await;
    create(
        &app,
        t,
        &with(meeting("t"), "description", json!("a".repeat(300_000))),
    )
    .await
    .expect_error(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large");
    // 없는 캘린더, 형식이 틀린 경로 ID.
    create(
        &app,
        t,
        &with(meeting("t"), "calendar_id", json!(MemberId::new())),
    )
    .await
    .expect_error(StatusCode::NOT_FOUND, "not_found");
    for path in [
        "/calendar/events/not-a-uuid",
        "/calendar/events/1' OR '1'='1",
    ] {
        app.get(&path.replace(' ', "%20").replace('\'', "%27"))
            .bearer(t)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    detail(&app, t, &id.to_uppercase())
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    // 응답 값은 수락·거절뿐이다.
    let invited = id_of(
        &created(
            &app,
            &o.bob.token,
            &with(meeting("초대"), "attendee_ids", json!([o.alice.id])),
        )
        .await,
    );
    let before_respond = fingerprint(&app).await;
    expect_field(
        respond(&app, t, &invited, "pending").await,
        "status",
        "invalid_format",
    )
    .await;
    respond(&app, t, &invited, "maybe")
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    assert_eq!(fingerprint(&app).await, before_respond);
    // SQL 조각이 든 제목은 그대로 저장되고 표에는 영향이 없다.
    let sneaky = created(&app, t, &meeting("'); DROP TABLE calendar_events; --")).await;
    assert_eq!(sneaky["title"], "'); DROP TABLE calendar_events; --");
    assert!(before.len() == fingerprint(&app).await.len());
}

// ── 참석자 일정 확인(개인정보) ─────────────────────────────────────────

#[tokio::test]
async fn conflicts_show_busy_times_without_leaking_private_details() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let secret = with(
        timed("비밀 면접", "2026-03-03T01:00:00Z", "2026-03-03T02:00:00Z"),
        "description",
        json!("극비 설명"),
    );
    created(
        &app,
        &o.alice.token,
        &with(secret, "location", json!("비밀 장소")),
    )
    .await;
    let shared = created(
        &app,
        &o.alice.token,
        &with(
            timed("팀 회의", "2026-03-04T01:00:00Z", "2026-03-04T02:00:00Z"),
            "attendee_ids",
            json!([o.bob.id]),
        ),
    )
    .await;
    let declined = created(
        &app,
        &o.carol.token,
        &with(
            all_day("영업 미팅", "2026-03-05", "2026-03-05"),
            "attendee_ids",
            json!([o.alice.id]),
        ),
    )
    .await;
    respond(&app, &o.alice.token, &id_of(&declined), "declined")
        .await
        .expect(StatusCode::OK);
    // 회사 일정은 개인의 바쁜 시간이 아니다.
    created(
        &app,
        &o.admin.token,
        &with(
            all_day("전사", "2026-03-06", "2026-03-06"),
            "calendar_id",
            json!(COMPANY),
        ),
    )
    .await;
    leave(&app, &o.alice, &o.team_head, true, "2026-03-06").await;

    let query = format!(
        "member_ids={},{}&from=2026-03-02&to=2026-03-08",
        o.alice.id, o.bob.id
    );
    let report = conflicts(&app, &o.bob.token, &query)
        .await
        .expect(StatusCode::OK);
    let text = report.to_string();
    for secret in [
        "비밀 면접",
        "극비 설명",
        "비밀 장소",
        "비밀 사유",
        "영업 미팅",
        "annual",
    ] {
        assert!(!text.contains(secret), "leaked {secret}: {text}");
    }
    let members = report["members"].as_array().expect("members");
    assert_eq!(members[0]["member_id"], o.alice.id.to_string());
    assert_eq!(members[1]["member_id"], o.bob.id.to_string());
    let alice: Vec<_> = members[0]["busy"]
        .as_array()
        .expect("busy")
        .iter()
        .map(|b| {
            (
                b["kind"].clone(),
                b["title"].clone(),
                b["event_id"].clone(),
                b["leave_unit"].clone(),
            )
        })
        .collect();
    assert_eq!(
        alice,
        [
            (json!("busy"), Value::Null, Value::Null, Value::Null),
            (
                json!("event"),
                json!("팀 회의"),
                shared["id"].clone(),
                Value::Null
            ),
            (json!("leave"), Value::Null, Value::Null, json!("full_day")),
        ]
    );
    // 거절한 초대는 바쁜 시간이 아니고, bob에게는 초대받은 팀 회의가 보인다.
    let bob: Vec<_> = members[1]["busy"]
        .as_array()
        .expect("busy")
        .iter()
        .map(|b| b["title"].clone())
        .collect();
    assert_eq!(bob, [json!("팀 회의")]);

    // 다른 부서에서 보면 휴가도 그냥 바쁜 날이고, 팀 회의 제목도 없다.
    let outsider = conflicts(
        &app,
        &o.carol.token,
        &format!("member_ids={}&from=2026-03-02&to=2026-03-08", o.alice.id),
    )
    .await
    .expect(StatusCode::OK);
    let kinds: Vec<_> = outsider["members"][0]["busy"]
        .as_array()
        .expect("busy")
        .iter()
        .map(|b| b["kind"].clone())
        .collect();
    assert_eq!(kinds, [json!("busy"), json!("busy"), json!("busy")]);
    assert!(!outsider.to_string().contains("팀 회의"));
    // 관리자도 남의 개인 일정 제목은 보지 못한다.
    let admin = conflicts(&app, &o.admin.token, &query)
        .await
        .expect(StatusCode::OK);
    assert!(!admin.to_string().contains("비밀 면접"));
    assert!(!admin.to_string().contains("팀 회의"));
}

#[tokio::test]
async fn conflicts_validate_the_requested_members() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let t = &o.alice.token;
    let range = "from=2026-03-02&to=2026-03-08";
    conflicts(&app, t, &format!("member_ids={}&{range}", MemberId::new()))
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    expect_field(
        conflicts(&app, t, &format!("member_ids=abc&{range}")).await,
        "member_ids",
        "invalid_format",
    )
    .await;
    expect_field(
        conflicts(&app, t, &format!("member_ids={0},{0}&{range}", o.bob.id)).await,
        "member_ids",
        "duplicate",
    )
    .await;
    let many: Vec<String> = (0..51).map(|_| MemberId::new().to_string()).collect();
    expect_field(
        conflicts(&app, t, &format!("member_ids={}&{range}", many.join(","))).await,
        "member_ids",
        "out_of_range",
    )
    .await;
    set_status(&app, o.carol.id, MemberStatus::Retired).await;
    conflicts(&app, t, &format!("member_ids={}&{range}", o.carol.id))
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    conflicts(
        &app,
        &o.admin.token,
        &format!("member_ids={}&{range}", o.carol.id),
    )
    .await
    .expect_error(StatusCode::CONFLICT, "member_inactive");
    // 본인도 확인할 수 있다.
    conflicts(&app, t, &format!("member_ids={}&{range}", o.alice.id))
        .await
        .expect(StatusCode::OK);
}

// ── 계정·권한 변화 ─────────────────────────────────────────────────────

#[tokio::test]
async fn deactivated_attendee_stops_seeing_the_event_and_cannot_be_reinvited() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let body = with(meeting("정지 전 초대"), "attendee_ids", json!([o.bob.id]));
    let id = id_of(&created(&app, &o.alice.token, &body).await);
    detail(&app, &o.bob.token, &id).await.expect(StatusCode::OK);
    set_status(&app, o.bob.id, MemberStatus::Suspended).await;
    detail(&app, &o.bob.token, &id)
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    view_response(&app, &o.bob.token, "from=2026-03-01&to=2026-03-31")
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    // 이미 초대된 사람은 목록에 남길 수 있지만, 새로 부를 수는 없다.
    patch(
        &app,
        &o.alice.token,
        &id,
        1,
        &with(meeting("유지"), "attendee_ids", json!([o.bob.id])),
    )
    .await
    .expect(StatusCode::OK);
    patch(
        &app,
        &o.alice.token,
        &id,
        2,
        &with(meeting("빼기"), "attendee_ids", json!([])),
    )
    .await
    .expect(StatusCode::OK);
    patch(
        &app,
        &o.alice.token,
        &id,
        3,
        &with(meeting("다시"), "attendee_ids", json!([o.bob.id])),
    )
    .await
    .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn writes_use_permissions_read_inside_the_transaction() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let ctx = app.auth_context(&o.team_head.token).await;
    // 인증 뒤에 조직장에서 물러났다.
    app.set_department_head(o.team, None).await;
    let input = serde_json::from_value(with(
        meeting("팀 회의"),
        "calendar_id",
        json!(o.team.to_string()),
    ))
    .expect("request");
    let result = service::create_event(&app.state, &ctx, &input, None).await;
    assert_eq!(denial(&result), "forbidden");
    assert_eq!(
        scalar(&app, "SELECT COUNT(*) FROM calendar_events").await,
        0
    );

    let admin_ctx = app.auth_context(&o.admin.token).await;
    app.alter_member(o.admin.id, |m| m.role = Role::Member)
        .await;
    let input = serde_json::from_value(with(meeting("전사"), "calendar_id", json!(COMPANY)))
        .expect("request");
    let result = service::create_event(&app.state, &admin_ctx, &input, None).await;
    assert_eq!(denial(&result), "forbidden");
}
