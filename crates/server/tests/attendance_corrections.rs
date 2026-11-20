//! 근태 정정 요청 통합 테스트.
//!
//! 시계는 2026-03-02(월) 09:00 KST에서 시작한다. 대부분의 테스트는 03-03(화) 10:00으로 옮겨
//! 전날(03-02) 기록의 정정을 요청한다.

use std::sync::Arc;

use http::StatusCode;
use pango_core::authz::Role;
use pango_core::id::MemberId;
use pango_core::member::MemberStatus;
use pango_db::audit::{self, AuditFilter};
use pango_db::{attendance_corrections, members, notifications};
use pango_server::testing::{TestApp, TestMember, TestResponse};
use serde_json::{json, Value};

// ── 도우미 ──────────────────────────────────────────────────────────────

fn kst(local: &str) -> String {
    format!("{local}+09:00")
}

fn millis(local: &str) -> i64 {
    pango_server::testing::ts(&kst(local)).as_millis()
}

/// 개발팀(팀장 lead, 팀원 kim·lee), 관리자 admin1, 최고 관리자 owner.
struct World {
    app: Arc<TestApp>,
    owner: String,
    lead: TestMember,
    kim: TestMember,
    lee: TestMember,
    admin: TestMember,
}

/// 03-03(화) 10:00의 세계. kim은 전날 출근하지 않았다.
async fn world() -> World {
    let app = Arc::new(TestApp::new().await);
    let owner = app.login_owner().await;
    let dev = app.add_department("개발팀", None, None).await;
    let lead = app.add_member("lead", Role::Member, Some(dev)).await;
    app.set_department_head(dev, Some(lead.id)).await;
    let kim = app.add_member("kim", Role::Member, Some(dev)).await;
    let lee = app.add_member("lee", Role::Member, Some(dev)).await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    app.set_time(&kst("2026-03-03T10:00:00"));
    World {
        app,
        owner,
        lead,
        kim,
        lee,
        admin,
    }
}

/// kim이 03-02 09:40에 출근만 한(지각, 퇴근 누락) 세계.
async fn world_with_late_record() -> (World, Value) {
    let w = world().await;
    w.app.set_time(&kst("2026-03-02T09:40:00"));
    let record = w
        .app
        .post("/attendance/clock-in")
        .bearer(&w.kim.token)
        .json(&json!({ "work_type": "office" }))
        .send()
        .await
        .expect(StatusCode::OK);
    w.app.set_time(&kst("2026-03-03T10:00:00"));
    (w, record)
}

fn body(date: &str, clock_in: &str, clock_out: Option<&str>) -> Value {
    json!({
        "work_date": date,
        "clock_in_at": millis(clock_in),
        "clock_out_at": clock_out.map(millis),
        "work_type": "office",
        "reason": "출근 기록 누락",
    })
}

/// 03-02 09:00–18:00으로 고쳐 달라는 요청.
fn full_day(date: &str) -> Value {
    body(
        date,
        &format!("{date}T09:00:00"),
        Some(&format!("{date}T18:00:00")),
    )
}

async fn request(app: &TestApp, who: &TestMember, body: &Value) -> TestResponse {
    app.post("/attendance/corrections")
        .bearer(&who.token)
        .json(body)
        .send()
        .await
}

/// 요청을 만들어 (요청 ID, 문서 ID)를 돌려준다.
async fn requested(app: &TestApp, who: &TestMember, body: &Value) -> (String, String) {
    let created = request(app, who, body).await.expect(StatusCode::CREATED);
    (
        created["id"].as_str().expect("id").to_owned(),
        created["document_id"]
            .as_str()
            .expect("document")
            .to_owned(),
    )
}

async fn decide(app: &TestApp, token: &str, document: &str, action: &str) -> TestResponse {
    let body = match action {
        "reject" => json!({ "reason": "근거 부족" }),
        _ => json!({}),
    };
    app.post(&format!("/approval/documents/{document}/{action}"))
        .bearer(token)
        .json(&body)
        .send()
        .await
}

async fn cancel(app: &TestApp, token: &str, id: &str) -> TestResponse {
    app.post(&format!("/attendance/corrections/{id}/cancel"))
        .bearer(token)
        .send()
        .await
}

async fn list(app: &TestApp, token: &str, query: &str) -> TestResponse {
    app.get(&format!("/attendance/corrections{query}"))
        .bearer(token)
        .send()
        .await
}

async fn status_of(app: &TestApp, who: &TestMember, id: &str) -> String {
    let page = list(app, &who.token, "").await.expect(StatusCode::OK);
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["id"] == id)
        .unwrap_or_else(|| panic!("correction {id}"))["status"]
        .as_str()
        .expect("status")
        .to_owned()
}

async fn document(app: &TestApp, token: &str, id: &str) -> Value {
    app.get(&format!("/approval/documents/{id}"))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

async fn day(app: &TestApp, token: &str, date: &str) -> Value {
    let records = app
        .get(&format!("/attendance/records?month={}", &date[..7]))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK);
    records["days"]
        .as_array()
        .expect("days")
        .iter()
        .find(|d| d["date"] == date)
        .unwrap_or_else(|| panic!("no entry for {date}"))
        .clone()
}

async fn summary(app: &TestApp, token: &str, month: &str) -> Value {
    app.get(&format!("/attendance/summary?month={month}"))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

async fn events(app: &TestApp, token: &str, record_id: &str) -> Vec<Value> {
    app.get(&format!("/attendance/records/{record_id}/events"))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
        .as_array()
        .expect("events")
        .clone()
}

/// DB를 직접 센다(API를 거치지 않고 실제로 남은 것을 확인한다).
async fn count(app: &TestApp, sql: &str) -> i64 {
    let mut conn = app.state.db.read().await.expect("conn");
    sqlx::query_scalar(sql)
        .fetch_one(&mut *conn)
        .await
        .expect("count")
}

async fn execute(app: &TestApp, sql: &str) {
    let mut tx = app.state.db.begin_write().await.expect("begin");
    sqlx::query(sql).execute(&mut *tx).await.expect("execute");
    tx.commit().await.expect("commit");
}

async fn stored(
    app: &TestApp,
    member: MemberId,
) -> Vec<attendance_corrections::AttendanceCorrection> {
    let mut conn = app.state.db.read().await.expect("conn");
    attendance_corrections::list_for_member(&mut conn, member, None, 200)
        .await
        .expect("corrections")
        .0
}

async fn notification_count(app: &TestApp, member: MemberId, kind: &str) -> usize {
    let mut conn = app.state.db.read().await.expect("conn");
    let (items, _) = notifications::list(&mut conn, member, false, None, 200)
        .await
        .expect("notifications");
    items.iter().filter(|n| n.kind == kind).count()
}

async fn audit_entries(app: &TestApp, action: &str) -> Vec<audit::AuditEntry> {
    let mut conn = app.state.db.read().await.expect("conn");
    let filter = AuditFilter {
        action: Some(action.to_owned()),
        ..AuditFilter::default()
    };
    audit::list(&mut conn, &filter, 200).await.expect("audit").0
}

const RECORDS: &str = "SELECT COUNT(*) FROM attendance_records";
const CORRECTION_EVENTS: &str = "SELECT COUNT(*) FROM attendance_events WHERE kind = 'correction'";
const CORRECTIONS: &str = "SELECT COUNT(*) FROM attendance_corrections";
const DOCUMENTS: &str =
    "SELECT COUNT(*) FROM approval_documents WHERE kind = 'attendance_correction'";

/// 승인이 실패한 뒤 아무것도 바뀌지 않았는지: 문서는 진행 중, 단계는 대기, 요청은 결재 중.
async fn assert_still_pending(w: &World, id: &str, doc: &str) {
    let detail = document(&w.app, &w.lead.token, doc).await;
    assert_eq!(detail["status"], "in_progress");
    assert_eq!(detail["steps"][0]["status"], "pending");
    assert_eq!(detail["my_turn"], true);
    assert_eq!(
        detail["events"].as_array().expect("events").len(),
        1,
        "상신 이력만 남아 있어야 한다"
    );
    assert_eq!(status_of(&w.app, &w.kim, id).await, "pending");
    assert_eq!(count(&w.app, CORRECTION_EVENTS).await, 0);
    assert_eq!(
        notification_count(&w.app, w.kim.id, "attendance.corrected").await,
        0
    );
    assert_eq!(
        notification_count(&w.app, w.kim.id, "approval.approved").await,
        0
    );
    assert!(audit_entries(&w.app, "attendance.correction")
        .await
        .is_empty());
}

// ── 해피 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn approved_correction_creates_a_record_for_a_day_without_one() {
    let w = world().await;
    let created = request(&w.app, &w.kim, &full_day("2026-03-02"))
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(created["status"], "pending");
    assert_eq!(created["member_id"], w.kim.id.to_string());
    assert_eq!(created["member_name"], "kim 님");
    assert_eq!(created["work_date"], "2026-03-02");
    assert_eq!(created["clock_in_at"], millis("2026-03-02T09:00:00"));
    assert_eq!(created["clock_out_at"], millis("2026-03-02T18:00:00"));
    assert_eq!(created["work_type"], "office");
    assert_eq!(created["reason"], "출근 기록 누락");
    assert_eq!(created["decided_at"], Value::Null);
    let id = created["id"].as_str().expect("id");
    let doc = created["document_id"].as_str().expect("document");

    // 결재 문서: 시스템 양식, 추천 결재선(팀장), 구조화된 내용.
    let detail = document(&w.app, &w.lead.token, doc).await;
    assert_eq!(detail["kind"], "attendance_correction");
    assert_eq!(detail["status"], "in_progress");
    assert_eq!(detail["title"], "근태 정정 요청 (2026-03-02)");
    assert_eq!(detail["body"], "출근 기록 누락");
    assert_eq!(detail["drafter_id"], w.kim.id.to_string());
    assert_eq!(detail["steps"].as_array().expect("steps").len(), 1);
    assert_eq!(detail["steps"][0]["approver_id"], w.lead.id.to_string());
    assert_eq!(detail["payload"]["request_id"], id);
    assert_eq!(detail["payload"]["work_date"], "2026-03-02");
    assert_eq!(
        detail["payload"]["clock_in_at"],
        millis("2026-03-02T09:00:00")
    );
    assert_eq!(detail["payload"]["work_type"], "office");
    assert_eq!(detail["payload"]["current"], Value::Null);
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.todo").await,
        1
    );

    // 승인 전에는 기록이 없다.
    assert_eq!(count(&w.app, RECORDS).await, 0);
    assert_eq!(
        day(&w.app, &w.kim.token, "2026-03-02").await["evaluation"]["status"],
        "absent"
    );

    w.app.set_time(&kst("2026-03-03T11:00:00"));
    let approved = decide(&w.app, &w.lead.token, doc, "approve")
        .await
        .expect(StatusCode::OK);
    assert_eq!(approved["status"], "approved");

    let entry = day(&w.app, &w.kim.token, "2026-03-02").await;
    assert_eq!(entry["record"]["member_id"], w.kim.id.to_string());
    assert_eq!(
        entry["record"]["clock_in_at"],
        millis("2026-03-02T09:00:00")
    );
    assert_eq!(
        entry["record"]["clock_out_at"],
        millis("2026-03-02T18:00:00")
    );
    assert_eq!(entry["record"]["version"], 1);
    assert_eq!(entry["evaluation"]["status"], "normal");
    assert_eq!(entry["evaluation"]["worked_minutes"], 480);

    let record_id = entry["record"]["id"].as_str().expect("record id");
    let history = events(&w.app, &w.kim.token, record_id).await;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["kind"], "correction");
    assert_eq!(history[0]["before"], Value::Null);
    assert_eq!(
        history[0]["after"]["clock_out_at"],
        millis("2026-03-02T18:00:00")
    );
    assert_eq!(history[0]["reason"], "출근 기록 누락");
    assert_eq!(history[0]["actor_id"], w.lead.id.to_string());
    assert_eq!(history[0]["actor_name"], "lead 님");

    let listed = list(&w.app, &w.kim.token, "").await.expect(StatusCode::OK);
    assert_eq!(listed["items"][0]["status"], "approved");
    assert_eq!(
        listed["items"][0]["decided_at"],
        millis("2026-03-03T11:00:00")
    );
    assert_eq!(
        notification_count(&w.app, w.kim.id, "attendance.corrected").await,
        1
    );
    assert_eq!(
        notification_count(&w.app, w.kim.id, "approval.approved").await,
        1
    );
    let audited = audit_entries(&w.app, "attendance.correction").await;
    assert_eq!(audited.len(), 1);
    assert_eq!(audited[0].actor_id, Some(w.lead.id));
    assert_eq!(audited[0].target_id.as_deref(), Some(record_id));
    let requested = audit_entries(&w.app, "attendance.correction_request").await;
    assert_eq!(requested.len(), 1);
    assert_eq!(requested[0].actor_id, Some(w.kim.id));
}

#[tokio::test]
async fn approved_correction_updates_an_existing_record_and_keeps_history() {
    let (w, record) = world_with_late_record().await;
    let record_id = record["id"].as_str().expect("id");
    let (id, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;

    // 문서에는 요청 당시의 기록이 함께 실린다.
    let detail = document(&w.app, &w.lead.token, &doc).await;
    assert_eq!(detail["payload"]["current"]["id"], record_id);
    assert_eq!(
        detail["payload"]["current"]["clock_in_at"],
        millis("2026-03-02T09:40:00")
    );
    assert_eq!(detail["payload"]["current"]["clock_out_at"], Value::Null);

    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "approved");

    let entry = day(&w.app, &w.kim.token, "2026-03-02").await;
    assert_eq!(entry["record"]["id"], record_id, "같은 기록을 고친다");
    assert_eq!(entry["record"]["version"], 2);
    assert_eq!(
        entry["record"]["clock_in_at"],
        millis("2026-03-02T09:00:00")
    );
    assert_eq!(count(&w.app, RECORDS).await, 1);

    let history = events(&w.app, &w.kim.token, record_id).await;
    let kinds: Vec<&str> = history
        .iter()
        .map(|e| e["kind"].as_str().expect("kind"))
        .collect();
    assert_eq!(kinds, ["clock_in", "correction"]);
    let last = &history[1];
    assert_eq!(last["before"]["clock_in_at"], millis("2026-03-02T09:40:00"));
    assert_eq!(last["before"]["clock_out_at"], Value::Null);
    assert_eq!(last["before"]["version"], 1);
    assert_eq!(last["after"]["clock_in_at"], millis("2026-03-02T09:00:00"));
    assert_eq!(last["after"]["clock_out_at"], millis("2026-03-02T18:00:00"));
    assert_eq!(last["after"]["version"], 2);
    assert_eq!(last["actor_id"], w.lead.id.to_string());
    assert_eq!(last["reason"], "출근 기록 누락");
}

#[tokio::test]
async fn corrected_day_is_reflected_in_records_and_summary() {
    let (w, _) = world_with_late_record().await;
    // 정정 전: 09:40 출근(지각 40분), 퇴근 없음.
    let before = day(&w.app, &w.kim.token, "2026-03-02").await;
    assert_eq!(before["evaluation"]["late_minutes"], 40);
    assert_eq!(before["evaluation"]["worked_minutes"], 0);
    let before = summary(&w.app, &w.kim.token, "2026-03").await;
    assert_eq!(before["normal_days"], 0);
    assert_eq!(before["total_late_minutes"], 40);

    // 재택 08:50–19:00으로 정정한다.
    let mut request_body = body(
        "2026-03-02",
        "2026-03-02T08:50:00",
        Some("2026-03-02T19:00:00"),
    );
    request_body["work_type"] = json!("remote");
    let (_, doc) = requested(&w.app, &w.kim, &request_body).await;
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);

    let after = day(&w.app, &w.kim.token, "2026-03-02").await;
    assert_eq!(after["record"]["work_type"], "remote");
    assert_eq!(after["evaluation"]["status"], "normal");
    assert_eq!(after["evaluation"]["late_minutes"], 0);
    assert_eq!(after["evaluation"]["overtime_minutes"], 60);
    // 10시간 10분 − 휴게 60분.
    assert_eq!(after["evaluation"]["worked_minutes"], 550);
    let after = summary(&w.app, &w.kim.token, "2026-03").await;
    assert_eq!(after["normal_days"], 1);
    assert_eq!(after["late_days"], 0);
    assert_eq!(after["missing_clock_out_days"], 0);
    assert_eq!(after["total_late_minutes"], 0);
    assert_eq!(after["total_worked_minutes"], 550);
    assert_eq!(after["total_overtime_minutes"], 60);
    // 팀장이 보는 값도 같다.
    let seen_by_lead = w
        .app
        .get(&format!(
            "/attendance/summary?month=2026-03&member_id={}",
            w.kim.id
        ))
        .bearer(&w.lead.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(seen_by_lead["total_worked_minutes"], 550);
}

// ── 반려·회수·취소 ──────────────────────────────────────────────────────

#[tokio::test]
async fn rejected_withdrawn_and_canceled_requests_leave_the_record_untouched() {
    let (w, record) = world_with_late_record().await;
    let record_id = record["id"].as_str().expect("id");

    // 반려.
    let (rejected, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    decide(&w.app, &w.lead.token, &doc, "reject")
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&w.app, &w.kim, &rejected).await, "rejected");

    // 결재 화면에서 회수.
    let (withdrawn, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    decide(&w.app, &w.kim.token, &doc, "withdraw")
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&w.app, &w.kim, &withdrawn).await, "canceled");

    // 근태 화면에서 취소: 문서도 함께 회수된다.
    let (canceled, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    let response = cancel(&w.app, &w.kim.token, &canceled)
        .await
        .expect(StatusCode::OK);
    assert_eq!(response["status"], "canceled");
    assert_eq!(response["decided_at"], millis("2026-03-03T10:00:00"));
    assert_eq!(
        document(&w.app, &w.kim.token, &doc).await["status"],
        "withdrawn"
    );
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.withdrawn").await,
        2
    );

    // 취소는 자연 멱등이다: 다시 보내도 성공하고 아무것도 더 남기지 않는다.
    let audits = audit_entries(&w.app, "attendance.correction_cancel")
        .await
        .len();
    assert_eq!(audits, 1);
    let again = cancel(&w.app, &w.kim.token, &canceled)
        .await
        .expect(StatusCode::OK);
    assert_eq!(again, response);
    assert_eq!(
        audit_entries(&w.app, "attendance.correction_cancel")
            .await
            .len(),
        audits
    );
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.withdrawn").await,
        2
    );

    // 기록은 그대로다.
    let entry = day(&w.app, &w.kim.token, "2026-03-02").await;
    assert_eq!(entry["record"]["version"], 1);
    assert_eq!(
        entry["record"]["clock_in_at"],
        millis("2026-03-02T09:40:00")
    );
    assert_eq!(entry["record"]["clock_out_at"], Value::Null);
    assert_eq!(events(&w.app, &w.kim.token, record_id).await.len(), 1);
    assert_eq!(count(&w.app, CORRECTION_EVENTS).await, 0);
    assert_eq!(
        notification_count(&w.app, w.kim.id, "attendance.corrected").await,
        0
    );
    assert!(audit_entries(&w.app, "attendance.correction")
        .await
        .is_empty());
    assert_eq!(stored(&w.app, w.kim.id).await.len(), 3);
}

#[tokio::test]
async fn duplicate_pending_request_for_the_same_day_is_a_conflict_until_it_is_closed() {
    let w = world().await;
    let (first, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;

    // 같은 날: 값이 달라도 거부한다.
    request(&w.app, &w.kim, &full_day("2026-03-02"))
        .await
        .expect_error(StatusCode::CONFLICT, "already_exists");
    let other_times = body("2026-03-02", "2026-03-02T10:00:00", None);
    request(&w.app, &w.kim, &other_times)
        .await
        .expect_error(StatusCode::CONFLICT, "already_exists");
    assert_eq!(count(&w.app, CORRECTIONS).await, 1);
    assert_eq!(count(&w.app, DOCUMENTS).await, 1, "문서도 남지 않는다");
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.todo").await,
        1
    );

    // 다른 날, 다른 사람은 상관없다.
    requested(&w.app, &w.kim, &full_day("2026-02-27")).await;
    requested(&w.app, &w.lee, &full_day("2026-03-02")).await;

    // 반려되면 다시 요청할 수 있다.
    decide(&w.app, &w.lead.token, &doc, "reject")
        .await
        .expect(StatusCode::OK);
    let (second, _) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    assert_ne!(first, second);
    assert_eq!(status_of(&w.app, &w.kim, &first).await, "rejected");
    assert_eq!(status_of(&w.app, &w.kim, &second).await, "pending");
    request(&w.app, &w.kim, &full_day("2026-03-02"))
        .await
        .expect_error(StatusCode::CONFLICT, "already_exists");
}

// ── 멱등성·동시성 ───────────────────────────────────────────────────────

#[tokio::test]
async fn idempotency_key_replay_returns_the_same_request() {
    let w = world().await;
    let request_body = full_day("2026-03-02");
    let send = |body: Value| {
        w.app
            .post("/attendance/corrections")
            .bearer(&w.kim.token)
            .header("idempotency-key", "fix-0302")
            .json(&body)
            .send()
    };
    let first = send(request_body.clone()).await.expect(StatusCode::CREATED);
    let second = send(request_body.clone()).await.expect(StatusCode::CREATED);
    assert_eq!(first, second, "저장해 둔 첫 응답을 그대로 돌려준다");
    assert_eq!(count(&w.app, CORRECTIONS).await, 1);
    assert_eq!(count(&w.app, DOCUMENTS).await, 1);
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.todo").await,
        1
    );

    // 같은 키로 다른 본문은 거부한다.
    send(full_day("2026-02-27"))
        .await
        .expect_error(StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused");
    assert_eq!(count(&w.app, CORRECTIONS).await, 1);

    // 키가 없는 재전송은 새 요청이고, 결재 중인 요청과 부딪힌다.
    request(&w.app, &w.kim, &request_body)
        .await
        .expect_error(StatusCode::CONFLICT, "already_exists");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_duplicate_submissions_create_exactly_one_pending_request() {
    let w = world().await;
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (app, kim) = (w.app.clone(), w.kim.clone());
            tokio::spawn(async move { request(&app, &kim, &full_day("2026-03-02")).await })
        })
        .collect();
    let mut created = 0;
    for task in tasks {
        let response = task.await.expect("join");
        if response.status == StatusCode::CREATED {
            created += 1;
        } else {
            response.expect_error(StatusCode::CONFLICT, "already_exists");
        }
    }
    assert_eq!(created, 1, "정확히 한 건만 성공한다");
    let corrections = stored(&w.app, w.kim.id).await;
    assert_eq!(corrections.len(), 1);
    assert_eq!(count(&w.app, DOCUMENTS).await, 1);
    assert_eq!(
        count(
            &w.app,
            "SELECT COUNT(*) FROM approval_steps WHERE status = 'pending'"
        )
        .await,
        1
    );
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.todo").await,
        1
    );
    assert_eq!(
        audit_entries(&w.app, "attendance.correction_request")
            .await
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn double_approve_writes_one_event_row() {
    let w = world().await;
    let (id, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    let tasks: Vec<_> = (0..6)
        .map(|_| {
            let (app, token, doc) = (w.app.clone(), w.lead.token.clone(), doc.clone());
            tokio::spawn(async move { decide(&app, &token, &doc, "approve").await.status })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.expect("join"), StatusCode::OK);
    }
    // 끝난 뒤에 다시 보내도 같다.
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);

    assert_eq!(status_of(&w.app, &w.kim, &id).await, "approved");
    assert_eq!(count(&w.app, RECORDS).await, 1);
    assert_eq!(count(&w.app, CORRECTION_EVENTS).await, 1);
    assert_eq!(
        day(&w.app, &w.kim.token, "2026-03-02").await["record"]["version"],
        1
    );
    assert_eq!(
        notification_count(&w.app, w.kim.id, "attendance.corrected").await,
        1
    );
    assert_eq!(
        audit_entries(&w.app, "attendance.correction").await.len(),
        1
    );
}

// ── 부분 실패(롤백) ─────────────────────────────────────────────────────

#[tokio::test]
async fn approval_fails_and_rolls_back_when_the_record_changed_since_the_request() {
    let (w, record) = world_with_late_record().await;
    let record_id = record["id"].as_str().expect("id");
    let (id, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;

    // 그사이 관리자가 기록을 직접 고쳤다.
    let edited = w
        .app
        .patch(&format!("/admin/attendance/{record_id}"))
        .bearer(&w.admin.token)
        .json(&json!({
            "version": 1,
            "clock_in_at": millis("2026-03-02T09:40:00"),
            "clock_out_at": millis("2026-03-02T20:00:00"),
            "work_type": "field",
            "reason": "출장 복귀 확인",
        }))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(edited["version"], 2);

    // 결재자가 본 "현재 기록"이 더는 사실이 아니므로 승인은 실패하고 아무것도 바뀌지 않는다.
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    assert_still_pending(&w, &id, &doc).await;
    let entry = day(&w.app, &w.kim.token, "2026-03-02").await;
    assert_eq!(entry["record"], edited, "관리자가 고친 값 그대로");

    // 결재자는 반려로 끝낼 수 있고, 요청자는 새 기록을 보고 다시 요청할 수 있다.
    decide(&w.app, &w.lead.token, &doc, "reject")
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "rejected");
    let (_, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    let entry = day(&w.app, &w.kim.token, "2026-03-02").await;
    assert_eq!(entry["record"]["version"], 3);
    assert_eq!(
        entry["record"]["clock_out_at"],
        millis("2026-03-02T18:00:00")
    );
}

#[tokio::test]
async fn approval_fails_when_a_record_appeared_for_a_day_that_had_none() {
    let w = world().await;
    let (id, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    w.app
        .post("/admin/attendance")
        .bearer(&w.admin.token)
        .json(&json!({
            "member_id": w.kim.id, "work_date": "2026-03-02",
            "clock_in_at": millis("2026-03-02T10:00:00"),
            "clock_out_at": millis("2026-03-02T19:00:00"),
            "work_type": "office", "reason": "누락 보정",
        }))
        .send()
        .await
        .expect(StatusCode::OK);

    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    assert_still_pending(&w, &id, &doc).await;
    assert_eq!(count(&w.app, RECORDS).await, 1);
    assert_eq!(
        day(&w.app, &w.kim.token, "2026-03-02").await["record"]["clock_in_at"],
        millis("2026-03-02T10:00:00")
    );
}

#[tokio::test]
async fn approval_revalidates_the_requested_times_and_rolls_back_on_failure() {
    let w = world().await;
    let (id, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    // 요청할 때는 맞았던 값이 승인 시점에는 규칙에 어긋난다(저장된 값이 DB에서 직접 바뀌었다).
    // 출근 시각이 더는 그 근무일에 속하지 않는다.
    execute(
        &w.app,
        &format!(
            "UPDATE attendance_corrections SET clock_in_at = {}, clock_out_at = {}",
            millis("2026-03-01T09:00:00"),
            millis("2026-03-01T18:00:00")
        ),
    )
    .await;

    let response = decide(&w.app, &w.lead.token, &doc, "approve").await;
    assert_eq!(response.body["error"]["field"], "work_date");
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    assert_still_pending(&w, &id, &doc).await;
    assert_eq!(count(&w.app, RECORDS).await, 0);

    // 길이가 24시간을 넘는 값도 마찬가지다.
    execute(
        &w.app,
        &format!(
            "UPDATE attendance_corrections SET clock_in_at = {}, clock_out_at = {}",
            millis("2026-03-02T09:00:00"),
            millis("2026-03-03T09:00:01")
        ),
    )
    .await;
    let response = decide(&w.app, &w.lead.token, &doc, "approve").await;
    assert_eq!(response.body["error"]["field"], "clock_out_at");
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    assert_still_pending(&w, &id, &doc).await;
    assert_eq!(count(&w.app, RECORDS).await, 0);

    // 반려로 끝낼 수 있다.
    decide(&w.app, &w.lead.token, &doc, "reject")
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "rejected");
}

#[tokio::test]
async fn failure_in_the_middle_of_the_effect_rolls_back_the_whole_approval() {
    let (w, _) = world_with_late_record().await;
    let (id, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    // 기록을 고친 다음 단계(이력 추가)가 실패하게 만든다.
    execute(
        &w.app,
        "CREATE TRIGGER fail_correction_event BEFORE INSERT ON attendance_events
         WHEN NEW.kind = 'correction'
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END",
    )
    .await;

    let response = decide(&w.app, &w.lead.token, &doc, "approve").await;
    assert_eq!(response.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_still_pending(&w, &id, &doc).await;
    let entry = day(&w.app, &w.kim.token, "2026-03-02").await;
    assert_eq!(entry["record"]["version"], 1, "기록 수정도 되돌려졌다");
    assert_eq!(
        entry["record"]["clock_in_at"],
        millis("2026-03-02T09:40:00")
    );

    // 장애가 사라지면 같은 승인이 그대로 성공한다.
    execute(&w.app, "DROP TRIGGER fail_correction_event").await;
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "approved");
    assert_eq!(count(&w.app, CORRECTION_EVENTS).await, 1);
}

// ── 권한 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn colleagues_cannot_see_or_cancel_and_only_the_line_can_decide() {
    let w = world().await;
    let (id, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    let kim_query = format!("?member_id={}", w.kim.id);

    // 동료: 목록도 취소도 문서도 없는 것으로 보인다.
    list(&w.app, &w.lee.token, &kim_query)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    cancel(&w.app, &w.lee.token, &id)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    for action in ["approve", "reject", "withdraw"] {
        decide(&w.app, &w.lee.token, &doc, action)
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    let own = list(&w.app, &w.lee.token, "").await.expect(StatusCode::OK);
    assert_eq!(own["items"], json!([]), "남의 요청이 섞이지 않는다");

    // 팀장·관리자는 볼 수 있지만 취소는 요청자만 한다.
    for token in [&w.lead.token, &w.admin.token, &w.owner] {
        let seen = list(&w.app, token, &kim_query).await.expect(StatusCode::OK);
        assert_eq!(seen["items"][0]["id"], id);
        cancel(&w.app, token, &id)
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }

    // 결재선에 없는 관리자와 요청자 본인은 승인·반려할 수 없다.
    for token in [&w.admin.token, &w.owner, &w.kim.token] {
        for action in ["approve", "reject"] {
            decide(&w.app, token, &doc, action)
                .await
                .expect_error(StatusCode::FORBIDDEN, "forbidden");
        }
    }
    // 결재자는 회수할 수 없다.
    decide(&w.app, &w.lead.token, &doc, "withdraw")
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");

    // 아무것도 바뀌지 않았다.
    assert_still_pending(&w, &id, &doc).await;
    assert_eq!(count(&w.app, RECORDS).await, 0);

    // 없는 요청·형식이 틀린 ID.
    let unknown = pango_core::id::AttendanceCorrectionId::new();
    cancel(&w.app, &w.kim.token, &unknown.to_string())
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    for bad in ["1", "not-a-uuid", &id.to_uppercase()] {
        cancel(&w.app, &w.kim.token, bad)
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    let unknown_member = format!("?member_id={}", MemberId::new());
    list(&w.app, &w.lead.token, &unknown_member)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn approval_line_rules_apply_to_corrections() {
    let w = world().await;
    let with_steps = |steps: Value| {
        let mut request_body = full_day("2026-03-02");
        request_body["steps"] = steps;
        request_body
    };
    // 자기 자신을 결재자로 넣을 수 없다.
    let own = with_steps(json!([{ "approver_id": w.kim.id, "kind": "approve" }]));
    let response = request(&w.app, &w.kim, &own).await;
    assert_eq!(response.body["error"]["field"], "steps");
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    // 빈 결재선, 없는 결재자.
    request(&w.app, &w.kim, &with_steps(json!([])))
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    let ghost = with_steps(json!([{ "approver_id": MemberId::new(), "kind": "approve" }]));
    request(&w.app, &w.kim, &ghost)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    // 추천할 결재자가 없으면(부서 없는 대표) 직접 지정해야 한다.
    let token = TestMember {
        id: w.app.owner_id,
        login_id: "owner".into(),
        token: w.owner.clone(),
    };
    let response = request(&w.app, &token, &full_day("2026-03-02")).await;
    assert_eq!(response.body["error"]["field"], "steps");
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    assert_eq!(count(&w.app, CORRECTIONS).await, 0);
    assert_eq!(count(&w.app, DOCUMENTS).await, 0);

    // 직접 지정한 2단계 결재선과 참조자.
    let mut two_steps = with_steps(json!([
        { "approver_id": w.lead.id, "kind": "agree" },
        { "approver_id": w.admin.id, "kind": "approve" },
    ]));
    two_steps["references"] = json!([w.lee.id]);
    let (id, doc) = requested(&w.app, &w.kim, &two_steps).await;
    assert_eq!(
        notification_count(&w.app, w.lee.id, "approval.reference").await,
        1
    );

    // 첫 단계만 승인되면 아직 반영되지 않는다.
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "pending");
    assert_eq!(count(&w.app, RECORDS).await, 0);
    // 누군가 결재한 뒤에는 취소(회수)할 수 없다.
    cancel(&w.app, &w.kim.token, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "withdraw_too_late");
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "pending");

    // 최종 승인자가 이력의 행위자다.
    decide(&w.app, &w.admin.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "approved");
    let entry = day(&w.app, &w.kim.token, "2026-03-02").await;
    let record_id = entry["record"]["id"].as_str().expect("record");
    let history = events(&w.app, &w.kim.token, record_id).await;
    assert_eq!(history[0]["actor_id"], w.admin.id.to_string());
}

// ── 순서 역전 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn cancel_after_a_decision_is_a_conflict() {
    let w = world().await;
    let (approved, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    cancel(&w.app, &w.kim.token, &approved)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    decide(&w.app, &w.kim.token, &doc, "withdraw")
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    decide(&w.app, &w.lead.token, &doc, "reject")
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    assert_eq!(status_of(&w.app, &w.kim, &approved).await, "approved");
    assert_eq!(
        day(&w.app, &w.kim.token, "2026-03-02").await["evaluation"]["status"],
        "normal",
        "정정된 기록은 그대로다"
    );

    let (rejected, doc) = requested(&w.app, &w.kim, &full_day("2026-02-27")).await;
    decide(&w.app, &w.lead.token, &doc, "reject")
        .await
        .expect(StatusCode::OK);
    cancel(&w.app, &w.kim.token, &rejected)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    assert_eq!(status_of(&w.app, &w.kim, &rejected).await, "rejected");
    assert_eq!(count(&w.app, RECORDS).await, 1);
    assert_eq!(
        audit_entries(&w.app, "attendance.correction_cancel")
            .await
            .len(),
        0
    );
}

#[tokio::test]
async fn decisions_after_cancel_are_conflicts_and_change_nothing() {
    let w = world().await;
    let (id, doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    cancel(&w.app, &w.kim.token, &id)
        .await
        .expect(StatusCode::OK);
    for action in ["approve", "reject"] {
        decide(&w.app, &w.lead.token, &doc, action)
            .await
            .expect_error(StatusCode::CONFLICT, "document_already_closed");
    }
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "canceled");
    assert_eq!(count(&w.app, RECORDS).await, 0);
    assert_eq!(count(&w.app, CORRECTION_EVENTS).await, 0);
    assert_eq!(
        notification_count(&w.app, w.kim.id, "attendance.corrected").await,
        0
    );
}

// ── 경계값 ──────────────────────────────────────────────────────────────

/// 요청이 `field`의 검증 오류로 거부되는지 확인한다.
async fn assert_invalid(w: &World, request_body: &Value, field: &str) {
    let response = request(&w.app, &w.kim, request_body).await;
    assert_eq!(
        response.body["error"]["field"], field,
        "body: {request_body}, response: {}",
        response.body
    );
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
}

#[tokio::test]
async fn work_date_window_is_31_days_back_and_never_the_future() {
    // 지금은 03-03 10:00. 31일 전은 01-31, 32일 전은 01-30이다.
    let w = world().await;
    requested(&w.app, &w.kim, &full_day("2026-01-31")).await;
    assert_invalid(&w, &full_day("2026-01-30"), "work_date").await;
    assert_invalid(&w, &full_day("2025-03-02"), "work_date").await;

    // 내일은 안 된다(시각을 과거로 적어도 근무일이 미래다).
    assert_invalid(
        &w,
        &body("2026-03-04", "2026-03-03T09:00:00", None),
        "work_date",
    )
    .await;
    assert_invalid(&w, &full_day("2026-03-04"), "work_date").await;

    // 오늘은 된다(아직 퇴근 전이라 출근 시각만).
    requested(
        &w.app,
        &w.kim,
        &body("2026-03-03", "2026-03-03T09:00:00", None),
    )
    .await;
    assert_eq!(count(&w.app, CORRECTIONS).await, 2);

    // 근무일 전환(05:00) 전인 새벽에는 "오늘"이 아직 전날이다.
    w.app.set_time(&kst("2026-03-04T04:59:00"));
    assert_invalid(
        &w,
        &body("2026-03-04", "2026-03-04T04:00:00", None),
        "work_date",
    )
    .await;
}

#[tokio::test]
async fn requested_times_follow_the_edit_rules() {
    let w = world().await;
    // 퇴근이 출근보다 빠르거나 같다.
    assert_invalid(
        &w,
        &body(
            "2026-03-02",
            "2026-03-02T18:00:00",
            Some("2026-03-02T09:00:00"),
        ),
        "clock_out_at",
    )
    .await;
    assert_invalid(
        &w,
        &body(
            "2026-03-02",
            "2026-03-02T09:00:00",
            Some("2026-03-02T09:00:00"),
        ),
        "clock_out_at",
    )
    .await;
    // 24시간 + 1초.
    assert_invalid(
        &w,
        &body(
            "2026-03-01",
            "2026-03-01T09:00:00",
            Some("2026-03-02T09:00:01"),
        ),
        "clock_out_at",
    )
    .await;
    // 미래 시각(시계 오차 5분까지는 허용).
    assert_invalid(
        &w,
        &body("2026-03-03", "2026-03-03T10:05:01", None),
        "clock_in_at",
    )
    .await;
    assert_invalid(
        &w,
        &body(
            "2026-03-03",
            "2026-03-03T09:00:00",
            Some("2026-03-03T18:00:00"),
        ),
        "clock_out_at",
    )
    .await;
    // 출근 시각이 그 근무일에 속하지 않는다(04:59는 전날 근무일).
    assert_invalid(
        &w,
        &body("2026-03-02", "2026-03-02T04:59:59", None),
        "work_date",
    )
    .await;
    assert_invalid(
        &w,
        &body("2026-03-02", "2026-03-01T09:00:00", None),
        "work_date",
    )
    .await;
    assert_eq!(count(&w.app, CORRECTIONS).await, 0);
    assert_eq!(count(&w.app, DOCUMENTS).await, 0);

    // 경계 안쪽: 정확히 24시간, 근무일 전환 시각 정각 출근, 5분 뒤 시각.
    requested(
        &w.app,
        &w.kim,
        &body(
            "2026-03-01",
            "2026-03-01T09:00:00",
            Some("2026-03-02T09:00:00"),
        ),
    )
    .await;
    requested(
        &w.app,
        &w.kim,
        &body("2026-03-02", "2026-03-02T05:00:00", None),
    )
    .await;
    requested(
        &w.app,
        &w.kim,
        &body("2026-03-03", "2026-03-03T10:05:00", None),
    )
    .await;
}

// ── 기형·악의적 입력 ────────────────────────────────────────────────────

#[tokio::test]
async fn malformed_requests_create_nothing() {
    let w = world().await;
    let with = |field: &str, value: Value| {
        let mut request_body = full_day("2026-03-02");
        request_body[field] = value;
        request_body
    };

    // 사유: 비어 있음, 공백뿐, 너무 김, 제어 문자.
    for reason in ["", "   \n\t ", "\u{0}", "사유\u{202e}"] {
        assert_invalid(&w, &with("reason", json!(reason)), "reason").await;
    }
    assert_invalid(&w, &with("reason", json!("가".repeat(1_001))), "reason").await;
    assert_invalid(&w, &with("reason", json!("x".repeat(100_000))), "reason").await;

    // 날짜 표기.
    for date in [
        "2026-3-2",
        "20260302",
        "",
        "2026-02-30",
        "2026-03-02T00:00:00",
    ] {
        assert_invalid(&w, &with("work_date", json!(date)), "work_date").await;
    }

    // 형식이 틀린 본문은 역직렬화에서 거부된다.
    let malformed = [
        with("work_type", json!("home")),
        with("work_type", json!("Office")),
        with("work_type", json!(1)),
        with("clock_in_at", json!("2026-03-02T09:00:00+09:00")),
        with("clock_in_at", json!(null)),
        with("clock_in_at", json!(1.5)),
        with("clock_in_at", json!(i64::MAX)),
        with("clock_out_at", json!("내일")),
        with("clock_out_at", json!(-9_000_000_000_000_000_i64)),
        with("work_date", json!(20260302)),
        with("reason", json!(null)),
        with("steps", json!("lead")),
        with(
            "steps",
            json!([{ "approver_id": "not-a-uuid", "kind": "approve" }]),
        ),
        with(
            "steps",
            json!([{ "approver_id": w.lead.id.to_string().to_uppercase(), "kind": "approve" }]),
        ),
        with(
            "steps",
            json!([{ "approver_id": w.lead.id, "kind": "sign" }]),
        ),
        with("references", json!(["1' OR '1'='1"])),
        json!({}),
        json!([]),
        json!("2026-03-02"),
    ];
    for request_body in &malformed {
        request(&w.app, &w.kim, request_body)
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    w.app
        .post("/attendance/corrections")
        .bearer(&w.kim.token)
        .raw_body("application/json", "{\"work_date\": ")
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");

    // 결재선 모양: 같은 사람 두 번, 11단계.
    let twice = json!([
        { "approver_id": w.lead.id, "kind": "approve" },
        { "approver_id": w.lead.id, "kind": "agree" },
    ]);
    assert_invalid(&w, &with("steps", twice), "steps").await;

    assert_eq!(count(&w.app, CORRECTIONS).await, 0);
    assert_eq!(count(&w.app, DOCUMENTS).await, 0);
    assert_eq!(count(&w.app, RECORDS).await, 0);
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.todo").await,
        0
    );

    // 스크립트·SQL 조각은 사유로 그대로 저장될 뿐이다.
    let hostile = "<script>alert(1)</script>'; DROP TABLE attendance_records;--";
    let created = request(&w.app, &w.kim, &with("reason", json!(hostile)))
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(created["reason"], hostile);
    assert_eq!(count(&w.app, RECORDS).await, 0);

    // 목록 쿼리.
    list(&w.app, &w.kim.token, "?member_id=abc")
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    list(&w.app, &w.kim.token, "?cursor=%00%00")
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let w = world().await;
    let (id, _) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    w.app
        .post("/attendance/corrections")
        .json(&full_day("2026-02-27"))
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    w.app
        .get("/attendance/corrections")
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    w.app
        .post(&format!("/attendance/corrections/{id}/cancel"))
        .bearer("not-a-token")
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "pending");
    assert_eq!(count(&w.app, CORRECTIONS).await, 1);
}

// ── 엣지 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn suspended_requesters_pending_request_can_still_be_decided() {
    let w = world().await;
    let (rejected, reject_doc) = requested(&w.app, &w.kim, &full_day("2026-03-02")).await;
    let (approved, approve_doc) = requested(&w.app, &w.kim, &full_day("2026-02-27")).await;

    // 요청자의 계정이 정지됐다.
    let mut tx = w.app.state.db.begin_write().await.expect("begin");
    let mut member = members::find(&mut tx, w.kim.id)
        .await
        .expect("find")
        .expect("member");
    member.status = MemberStatus::Suspended;
    assert!(members::update(&mut tx, &member, w.app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");
    list(&w.app, &w.kim.token, "")
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");

    // 결재자는 남은 문서를 반려하거나 승인해 끝낼 수 있다.
    decide(&w.app, &w.lead.token, &reject_doc, "reject")
        .await
        .expect(StatusCode::OK);
    decide(&w.app, &w.lead.token, &approve_doc, "approve")
        .await
        .expect(StatusCode::OK);
    let kim_query = format!("?member_id={}", w.kim.id);
    let seen = list(&w.app, &w.admin.token, &kim_query)
        .await
        .expect(StatusCode::OK);
    let status = |id: &str| {
        seen["items"]
            .as_array()
            .expect("items")
            .iter()
            .find(|item| item["id"] == id)
            .expect("correction")["status"]
            .clone()
    };
    assert_eq!(status(&rejected), "rejected");
    assert_eq!(status(&approved), "approved");
    assert_eq!(count(&w.app, RECORDS).await, 1);
    assert_eq!(count(&w.app, CORRECTION_EVENTS).await, 1);
    assert_eq!(
        count(
            &w.app,
            "SELECT COUNT(*) FROM approval_documents WHERE status = 'in_progress'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn night_shift_correction_belongs_to_the_previous_work_date() {
    let w = world().await;
    // 03-02 22:00 출근, 03-03 04:00 퇴근: 근무일은 03-02다.
    let night = body(
        "2026-03-02",
        "2026-03-02T22:00:00",
        Some("2026-03-03T04:00:00"),
    );
    let (_, doc) = requested(&w.app, &w.kim, &night).await;
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    let entry = day(&w.app, &w.kim.token, "2026-03-02").await;
    assert_eq!(
        entry["record"]["clock_out_at"],
        millis("2026-03-03T04:00:00")
    );
    assert_eq!(
        day(&w.app, &w.kim.token, "2026-03-03").await["record"],
        Value::Null
    );
    // 같은 퇴근 시각을 03-03의 출근으로 적으면 근무일이 맞지 않는다.
    assert_invalid(
        &w,
        &body("2026-03-03", "2026-03-03T04:00:00", None),
        "work_date",
    )
    .await;
}

#[tokio::test]
async fn list_is_newest_first_and_paginated() {
    let w = world().await;
    let mut ids = Vec::new();
    for date in ["2026-02-25", "2026-02-26", "2026-02-27"] {
        ids.push(requested(&w.app, &w.kim, &full_day(date)).await.0);
    }
    requested(&w.app, &w.lee, &full_day("2026-02-27")).await;

    let first = list(&w.app, &w.kim.token, "?limit=2")
        .await
        .expect(StatusCode::OK);
    let page_ids = |page: &Value| -> Vec<String> {
        page["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|item| item["id"].as_str().expect("id").to_owned())
            .collect()
    };
    assert_eq!(page_ids(&first), [ids[2].clone(), ids[1].clone()]);
    let cursor = first["next_cursor"].as_str().expect("cursor");
    let second = list(&w.app, &w.kim.token, &format!("?limit=2&cursor={cursor}"))
        .await
        .expect(StatusCode::OK);
    assert_eq!(page_ids(&second), [ids[0].clone()]);
    assert_eq!(second["next_cursor"], Value::Null);
    // 본인을 명시해도 같다.
    let own = list(&w.app, &w.kim.token, &format!("?member_id={}", w.kim.id))
        .await
        .expect(StatusCode::OK);
    assert_eq!(page_ids(&own).len(), 3);
}
