//! 휴가 통합 테스트.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::NaiveDate;
use http::StatusCode;
use pango_api::leave::BalanceDto;
use pango_core::authz::Role;
use pango_core::calendar::DateRange;
use pango_core::id::{DepartmentId, LeaveRequestId, MemberId};
use pango_core::leave::{Balance, LeaveUnit, LedgerEntry, LedgerEntryKind};
use pango_core::member::MemberStatus;
use pango_db::audit::{self, AuditFilter};
use pango_db::leave::{self as ledger, LedgerRecord};
use pango_db::settings::{self, Holiday};
use pango_db::{approval as documents, members, notifications};
use pango_server::service::leave as leave_service;
use pango_server::testing::{TestApp, TestMember, TestResponse, MEMBER_PASSWORD};
use serde_json::{json, Value};

/// 개발팀(팀장 lead, 팀원 kim)과 관리자(대표). 입사 2년차라 연차 15일(60)이 있다.
struct World {
    app: Arc<TestApp>,
    owner: String,
    dev: DepartmentId,
    lead: TestMember,
    kim: TestMember,
    annual: String,
}

/// 연차 15일 = 60(1/4일 단위).
const FULL: i64 = 60;

async fn world() -> World {
    let app = Arc::new(TestApp::new().await);
    let owner = app.login_owner().await;
    let dev = app.add_department("개발팀", None, None).await;
    let lead = app.add_member("lead", Role::Member, Some(dev)).await;
    app.set_department_head(dev, Some(lead.id)).await;
    let kim = app.add_member("kim", Role::Member, Some(dev)).await;
    let annual = type_id(&app, &owner, "annual").await;
    World {
        app,
        owner,
        dev,
        lead,
        kim,
        annual,
    }
}

async fn type_id(app: &TestApp, token: &str, code: &str) -> String {
    let types = app
        .get("/admin/leave/types")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK);
    types
        .as_array()
        .expect("types")
        .iter()
        .find(|leave_type| leave_type["code"] == code)
        .unwrap_or_else(|| panic!("leave type {code}"))["id"]
        .as_str()
        .expect("id")
        .to_owned()
}

fn leave(leave_type: &str, start: &str, end: &str, unit: &str) -> Value {
    json!({
        "leave_type_id": leave_type, "start_date": start, "end_date": end,
        "unit": unit, "reason": "개인 사정",
    })
}

async fn request(app: &TestApp, who: &TestMember, body: &Value) -> TestResponse {
    app.post("/leave/requests")
        .bearer(&who.token)
        .json(body)
        .send()
        .await
}

/// 종일 연차를 신청해 (신청 ID, 문서 ID)를 돌려준다.
async fn requested(w: &World, who: &TestMember, start: &str, end: &str) -> (String, String) {
    let created = request(&w.app, who, &leave(&w.annual, start, end, "full_day"))
        .await
        .expect(StatusCode::CREATED);
    ids(&created)
}

fn ids(request: &Value) -> (String, String) {
    (
        request["id"].as_str().expect("id").to_owned(),
        request["document_id"]
            .as_str()
            .expect("document")
            .to_owned(),
    )
}

async fn decide(app: &TestApp, token: &str, document: &str, action: &str) -> TestResponse {
    let body = match action {
        "reject" => json!({ "reason": "일정 조정 필요" }),
        _ => json!({}),
    };
    app.post(&format!("/approval/documents/{document}/{action}"))
        .bearer(token)
        .json(&body)
        .send()
        .await
}

async fn cancel(app: &TestApp, token: &str, id: &str) -> TestResponse {
    app.post(&format!("/leave/requests/{id}/cancel"))
        .bearer(token)
        .send()
        .await
}

async fn admin_cancel(app: &TestApp, token: &str, id: &str) -> TestResponse {
    app.post(&format!("/admin/leave/requests/{id}/cancel"))
        .bearer(token)
        .send()
        .await
}

async fn balance_of(app: &TestApp, token: &str) -> BalanceDto {
    let body = app
        .get("/leave/balance")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK);
    serde_json::from_value(body).expect("balance")
}

async fn remaining(app: &TestApp, who: &TestMember) -> i64 {
    i64::from(
        balance_of(app, &who.token)
            .await
            .balance
            .remaining
            .quarters(),
    )
}

async fn status_of(app: &TestApp, who: &TestMember, id: &str) -> String {
    let requests = app
        .get("/leave/requests?year=2026")
        .bearer(&who.token)
        .send()
        .await
        .expect(StatusCode::OK);
    requests
        .as_array()
        .expect("requests")
        .iter()
        .find(|request| request["id"] == id)
        .unwrap_or_else(|| panic!("request {id}"))["status"]
        .as_str()
        .expect("status")
        .to_owned()
}

async fn stored_ledger(app: &TestApp, member: MemberId) -> Vec<LedgerRecord> {
    let mut conn = app.state.read().await.expect("read");
    ledger::list_ledger(&mut conn, member)
        .await
        .expect("ledger")
}

/// 원장의 (사용, 복원) 줄 수.
async fn reservations(app: &TestApp, member: MemberId) -> (usize, usize) {
    let records = stored_ledger(app, member).await;
    let count = |wanted: fn(&LedgerEntryKind) -> bool| {
        records.iter().filter(|r| wanted(&r.entry.kind)).count()
    };
    (
        count(|kind| matches!(kind, LedgerEntryKind::Use { .. })),
        count(|kind| matches!(kind, LedgerEntryKind::Restore { .. })),
    )
}

/// 돈처럼 다루는 불변식을 DB에서 직접 확인한다.
///
/// - 신청 1건에 사용은 최대 1줄, 복원은 최대 1줄이고 복원에는 짝이 되는 사용이 있다.
/// - 저장된 원장으로 계산한 잔여가 API의 답과 같다.
/// - (음수 조정이 없으면) 잔여는 음수가 아니다.
async fn assert_ledger_invariants(app: &TestApp, who: &TestMember) -> Balance {
    let api = balance_of(app, &who.token).await;
    let records = stored_ledger(app, who.id).await;
    let mut uses: BTreeMap<LeaveRequestId, usize> = BTreeMap::new();
    let mut restores: BTreeMap<LeaveRequestId, usize> = BTreeMap::new();
    for record in &records {
        match record.entry.kind {
            LedgerEntryKind::Use { request, .. } => *uses.entry(request).or_default() += 1,
            LedgerEntryKind::Restore { request } => *restores.entry(request).or_default() += 1,
            LedgerEntryKind::Grant { .. } | LedgerEntryKind::Adjust { .. } => {}
        }
    }
    assert!(uses.values().all(|count| *count == 1), "사용은 신청당 1줄");
    assert!(
        restores.values().all(|count| *count == 1),
        "복원은 신청당 1줄"
    );
    assert!(
        restores.keys().all(|request| uses.contains_key(request)),
        "짝 없는 복원이 없다"
    );
    let entries: Vec<LedgerEntry> = records.iter().map(|record| record.entry).collect();
    let computed = Balance::compute(&entries, api.as_of).expect("balance");
    assert_eq!(computed, api.balance, "원장 계산과 API 응답이 같다");
    let negative_adjustment = entries.iter().any(|entry| {
        matches!(entry.kind, LedgerEntryKind::Adjust { .. }) && entry.amount.is_negative()
    });
    assert!(negative_adjustment || !computed.remaining.is_negative());
    computed
}

async fn notification_count(app: &TestApp, member: MemberId, kind: &str) -> usize {
    let mut conn = app.state.read().await.expect("read");
    let (items, _) = notifications::list(&mut conn, member, false, None, 200)
        .await
        .expect("notifications");
    items.iter().filter(|n| n.kind == kind).count()
}

async fn document(app: &TestApp, token: &str, id: &str) -> Value {
    app.get(&format!("/approval/documents/{id}"))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

async fn adjust(app: &TestApp, token: &str, member: MemberId, amount_q: i64) -> TestResponse {
    app.post("/admin/leave/adjustments")
        .bearer(token)
        .json(&json!({
            "member_id": member, "amount_q": amount_q,
            "effective_on": "2026-03-02", "memo": "테스트 조정",
        }))
        .send()
        .await
}

async fn audit_count(app: &TestApp, action: &str) -> usize {
    let mut conn = app.state.read().await.expect("read");
    let filter = AuditFilter {
        action: Some(action.to_owned()),
        ..AuditFilter::default()
    };
    audit::list(&mut conn, &filter, 200)
        .await
        .expect("audit")
        .0
        .len()
}

fn date(value: &str) -> NaiveDate {
    value.parse().expect("date")
}

// ── 해피 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn request_reserves_balance_and_approval_confirms_it() {
    let w = world().await;
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);

    let body = leave(&w.annual, "2026-03-10", "2026-03-11", "full_day");
    let created = request(&w.app, &w.kim, &body)
        .await
        .expect(StatusCode::CREATED);
    let (id, doc) = ids(&created);
    assert_eq!(created["status"], "pending");
    assert_eq!(created["amount"], 8);
    assert_eq!(created["leave_type_name"], "연차");
    assert_eq!(created["member_name"], "kim 님");
    assert_eq!(created["decided_at"], Value::Null);
    assert_eq!(
        remaining(&w.app, &w.kim).await,
        FULL - 8,
        "신청 즉시 예약된다"
    );

    // 결재 문서: 추천 결재선(팀장)으로 바로 상신된다.
    let detail = document(&w.app, &w.kim.token, &doc).await;
    assert_eq!(detail["kind"], "leave");
    assert_eq!(detail["form_name"], "휴가 신청서");
    assert_eq!(detail["status"], "in_progress");
    assert_eq!(detail["doc_no"], "2026-000001");
    assert_eq!(detail["title"], "휴가 신청 (2026-03-10 ~ 2026-03-11, 2일)");
    assert_eq!(detail["body"], "개인 사정");
    assert_eq!(detail["steps"].as_array().expect("steps").len(), 1);
    assert_eq!(detail["steps"][0]["approver_id"], json!(w.lead.id));
    assert_eq!(detail["payload"]["request_id"], id.as_str());
    assert_eq!(detail["payload"]["amount_q"], 8);
    assert_eq!(detail["payload"]["unit"], "full_day");
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.todo").await,
        1
    );

    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "approved");
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 8);
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 0));
    let balance = assert_ledger_invariants(&w.app, &w.kim).await;
    assert_eq!(balance.used.quarters(), 8);

    // 승인의 반복은 아무것도 바꾸지 않는다.
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 0));
    assert_eq!(audit_count(&w.app, "leave.request").await, 1);
}

#[tokio::test]
async fn rejection_restores_the_reserved_balance_exactly_once() {
    let w = world().await;
    let (id, doc) = requested(&w, &w.kim, "2026-03-10", "2026-03-12").await;
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 12);
    decide(&w.app, &w.lead.token, &doc, "reject")
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "rejected");
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 1));

    decide(&w.app, &w.lead.token, &doc, "reject")
        .await
        .expect(StatusCode::OK);
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 1));
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    let balance = assert_ledger_invariants(&w.app, &w.kim).await;
    assert_eq!(balance.used.quarters(), 0);

    // 끝난 신청은 취소할 수 없다.
    cancel(&w.app, &w.kim.token, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 1));
}

#[tokio::test]
async fn withdrawing_the_document_cancels_the_request_and_restores() {
    let w = world().await;
    let (id, doc) = requested(&w, &w.kim, "2026-03-10", "2026-03-10").await;
    let withdrawn = decide(&w.app, &w.kim.token, &doc, "withdraw")
        .await
        .expect(StatusCode::OK);
    assert_eq!(withdrawn["status"], "withdrawn");
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "canceled");
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 1));

    // 회수된 문서는 승인할 수 없고, 잔여도 다시 줄지 않는다.
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    decide(&w.app, &w.kim.token, &doc, "withdraw")
        .await
        .expect(StatusCode::OK);
    cancel(&w.app, &w.kim.token, &id)
        .await
        .expect(StatusCode::OK);
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 1));
    assert_ledger_invariants(&w.app, &w.kim).await;
}

#[tokio::test]
async fn canceling_a_pending_request_withdraws_its_document() {
    let w = world().await;
    let (id, doc) = requested(&w, &w.kim, "2026-03-10", "2026-03-11").await;
    let canceled = cancel(&w.app, &w.kim.token, &id)
        .await
        .expect(StatusCode::OK);
    assert_eq!(canceled["status"], "canceled");
    assert_eq!(canceled["decided_at"], json!(w.app.state.now()));
    let detail = document(&w.app, &w.kim.token, &doc).await;
    assert_eq!(detail["status"], "withdrawn");
    let kinds: Vec<&str> = detail["events"]
        .as_array()
        .expect("events")
        .iter()
        .map(|event| event["kind"].as_str().expect("kind"))
        .collect();
    assert_eq!(kinds, ["submitted", "withdrawn"]);
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.withdrawn").await,
        1
    );

    // 취소의 반복: 성공으로 답하되 아무것도 다시 하지 않는다.
    let again = cancel(&w.app, &w.kim.token, &id)
        .await
        .expect(StatusCode::OK);
    assert_eq!(again, canceled);
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 1));
    assert_eq!(audit_count(&w.app, "leave.cancel").await, 1);
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.withdrawn").await,
        1
    );
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    assert_ledger_invariants(&w.app, &w.kim).await;

    // 같은 날짜로 다시 신청할 수 있다.
    requested(&w, &w.kim, "2026-03-10", "2026-03-11").await;
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 8);
}

#[tokio::test]
async fn pending_request_cannot_be_canceled_once_someone_approved() {
    let w = world().await;
    let mut body = leave(&w.annual, "2026-03-10", "2026-03-10", "full_day");
    body["steps"] = json!([
        { "approver_id": w.lead.id, "kind": "agree" },
        { "approver_id": w.app.owner_id, "kind": "approve" },
    ]);
    let (id, doc) = ids(&request(&w.app, &w.kim, &body)
        .await
        .expect(StatusCode::CREATED));
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);

    // 문서를 회수할 수 없으므로 본인의 취소는 거부된다. 신청과 문서는 그대로 진행 중이다.
    // (관리자의 취소는 대리 반려로 끝낼 수 있다: `admin_cancel_closes_a_stranded_request`.)
    cancel(&w.app, &w.kim.token, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "withdraw_too_late");
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "pending");
    assert_eq!(
        document(&w.app, &w.kim.token, &doc).await["status"],
        "in_progress"
    );
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 4);
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 0));

    // 남은 결재자가 반려하면 잔여가 돌아온다.
    decide(&w.app, &w.owner, &doc, "reject")
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "rejected");
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_ledger_invariants(&w.app, &w.kim).await;
}

// ── 경계값 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn request_equal_to_the_balance_passes_and_a_quarter_more_fails() {
    let w = world().await;
    // 3주(근무일 15일) = 잔여 전부.
    let (_, doc) = requested(&w, &w.kim, "2026-03-09", "2026-03-27").await;
    assert_eq!(remaining(&w.app, &w.kim).await, 0);
    let quarter = leave(&w.annual, "2026-03-30", "2026-03-30", "am_quarter");
    request(&w.app, &w.kim, &quarter)
        .await
        .expect_error(StatusCode::CONFLICT, "insufficient_leave_balance");
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 0));
    assert_eq!(remaining(&w.app, &w.kim).await, 0);
    assert_ledger_invariants(&w.app, &w.kim).await;

    // 반려로 복원되면 다시 쓸 수 있다.
    decide(&w.app, &w.lead.token, &doc, "reject")
        .await
        .expect(StatusCode::OK);
    request(&w.app, &w.kim, &quarter)
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 1);

    // 잔여보다 1/4일 많은 신청은 거부되고 아무것도 예약하지 않는다.
    let all = leave(&w.annual, "2026-04-06", "2026-04-24", "full_day");
    request(&w.app, &w.kim, &all)
        .await
        .expect_error(StatusCode::CONFLICT, "insufficient_leave_balance");
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 1);
    assert_eq!(reservations(&w.app, w.kim.id).await, (2, 1));
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.todo").await,
        2
    );
    assert_ledger_invariants(&w.app, &w.kim).await;
}

#[tokio::test]
async fn partial_units_share_a_day_only_when_their_hours_do_not_collide() {
    let w = world().await;
    let unit = |day: &str, unit: &str| leave(&w.annual, day, day, unit);
    let ok = |body: Value| {
        let (app, kim) = (w.app.clone(), w.kim.clone());
        async move { request(&app, &kim, &body).await.expect(StatusCode::CREATED) }
    };
    let overlap = |body: Value| {
        let (app, kim) = (w.app.clone(), w.kim.clone());
        async move {
            request(&app, &kim, &body)
                .await
                .expect_error(StatusCode::CONFLICT, "leave_overlap")
        }
    };

    assert_eq!(ok(unit("2026-03-10", "am_half")).await["amount"], 2);
    assert_eq!(ok(unit("2026-03-10", "pm_half")).await["amount"], 2);
    overlap(unit("2026-03-10", "am_quarter")).await;
    overlap(unit("2026-03-10", "pm_quarter")).await;
    overlap(unit("2026-03-10", "full_day")).await;
    overlap(leave(&w.annual, "2026-03-09", "2026-03-11", "full_day")).await;

    assert_eq!(ok(unit("2026-03-11", "am_quarter")).await["amount"], 1);
    assert_eq!(ok(unit("2026-03-11", "pm_half")).await["amount"], 2);
    overlap(unit("2026-03-11", "am_half")).await;
    overlap(unit("2026-03-11", "pm_quarter")).await;
    overlap(unit("2026-03-11", "am_quarter")).await;

    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 7);
    assert_eq!(reservations(&w.app, w.kim.id).await, (4, 0));

    // 반차는 하루짜리 신청에만, 그리고 허용하는 종류에만 쓸 수 있다.
    let multi_day = leave(&w.annual, "2026-03-12", "2026-03-13", "am_half");
    let response = request(&w.app, &w.kim, &multi_day).await;
    assert_eq!(response.body["error"]["field"], "unit");
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    let sick = type_id(&w.app, &w.owner, "sick").await;
    let response = request(
        &w.app,
        &w.kim,
        &leave(&sick, "2026-03-12", "2026-03-12", "pm_half"),
    )
    .await;
    assert_eq!(response.body["error"]["field"], "unit");
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    assert_eq!(reservations(&w.app, w.kim.id).await, (4, 0));
    assert_ledger_invariants(&w.app, &w.kim).await;
}

// ── 엣지·코너 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn weekends_and_holidays_are_not_counted() {
    let w = world().await;
    // 금~월: 주말을 빼고 2일.
    let created = request(
        &w.app,
        &w.kim,
        &leave(&w.annual, "2026-03-06", "2026-03-09", "full_day"),
    )
    .await
    .expect(StatusCode::CREATED);
    assert_eq!(created["amount"], 8);

    let mut tx = w.app.state.db.begin_write().await.expect("begin");
    let holiday = Holiday {
        date: date("2026-03-11"),
        name: "창립기념일".into(),
    };
    settings::upsert_holiday(&mut tx, &holiday)
        .await
        .expect("holiday");
    tx.commit().await.expect("commit");
    // 화~목인데 수요일이 공휴일: 2일.
    let created = request(
        &w.app,
        &w.kim,
        &leave(&w.annual, "2026-03-10", "2026-03-12", "full_day"),
    )
    .await
    .expect(StatusCode::CREATED);
    assert_eq!(created["amount"], 8);
    let detail = document(&w.app, &w.kim.token, &ids(&created).1).await;
    assert_eq!(detail["title"], "휴가 신청 (2026-03-10 ~ 2026-03-12, 2일)");
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 16);

    // 근무일이 하루도 없는 신청.
    for (start, end) in [
        ("2026-03-14", "2026-03-15"),
        ("2026-03-21", "2026-03-21"),
        ("2026-03-11", "2026-03-11"),
    ] {
        request(&w.app, &w.kim, &leave(&w.annual, start, end, "full_day"))
            .await
            .expect_error(StatusCode::CONFLICT, "leave_no_working_days");
    }
    request(
        &w.app,
        &w.kim,
        &leave(&w.annual, "2026-03-11", "2026-03-11", "am_half"),
    )
    .await
    .expect_error(StatusCode::CONFLICT, "leave_no_working_days");
    assert_eq!(reservations(&w.app, w.kim.id).await, (2, 0));
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 16);
    assert_ledger_invariants(&w.app, &w.kim).await;
}

#[tokio::test]
async fn overlap_is_checked_against_pending_and_approved_only() {
    let w = world().await;
    let (_, pending_doc) = requested(&w, &w.kim, "2026-03-10", "2026-03-12").await;
    let overlapping = leave(&w.annual, "2026-03-12", "2026-03-13", "full_day");
    request(&w.app, &w.kim, &overlapping)
        .await
        .expect_error(StatusCode::CONFLICT, "leave_overlap");
    decide(&w.app, &w.lead.token, &pending_doc, "approve")
        .await
        .expect(StatusCode::OK);
    request(&w.app, &w.kim, &overlapping)
        .await
        .expect_error(StatusCode::CONFLICT, "leave_overlap");
    // 주말만 공유해도 겹침이다.
    let (_, weekend_doc) = requested(&w, &w.kim, "2026-03-20", "2026-03-21").await;
    request(
        &w.app,
        &w.kim,
        &leave(&w.annual, "2026-03-21", "2026-03-23", "full_day"),
    )
    .await
    .expect_error(StatusCode::CONFLICT, "leave_overlap");
    assert_eq!(reservations(&w.app, w.kim.id).await, (2, 0));

    // 반려·취소된 신청과는 겹쳐도 된다.
    decide(&w.app, &w.lead.token, &weekend_doc, "reject")
        .await
        .expect(StatusCode::OK);
    let (canceled, _) = requested(&w, &w.kim, "2026-03-20", "2026-03-23").await;
    cancel(&w.app, &w.kim.token, &canceled)
        .await
        .expect(StatusCode::OK);
    requested(&w, &w.kim, "2026-03-23", "2026-03-23").await;
    // 다른 구성원의 휴가와는 무관하다.
    requested(&w, &w.lead_peer().await, "2026-03-10", "2026-03-12").await;
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 12 - 4);
    assert_ledger_invariants(&w.app, &w.kim).await;
}

impl World {
    /// 같은 팀의 또 다른 팀원.
    async fn lead_peer(&self) -> TestMember {
        self.app
            .add_member("park", Role::Member, Some(self.dev))
            .await
    }
}

#[tokio::test]
async fn backdated_request_is_limited_to_thirty_days() {
    let w = world().await;
    // 오늘은 2026-03-02. 30일 전은 01-31.
    let too_old = request(
        &w.app,
        &w.kim,
        &leave(&w.annual, "2026-01-30", "2026-01-30", "full_day"),
    )
    .await;
    assert_eq!(too_old.body["error"]["field"], "start_date");
    too_old.expect_error(StatusCode::BAD_REQUEST, "validation_failed");

    // 경계(01-31 토요일 시작)는 통과하고, 그날 유효했던 부여(지금은 소멸)에서 차감된다.
    let created = request(
        &w.app,
        &w.kim,
        &leave(&w.annual, "2026-01-31", "2026-02-02", "full_day"),
    )
    .await
    .expect(StatusCode::CREATED);
    assert_eq!(created["amount"], 4);
    assert_eq!(
        remaining(&w.app, &w.kim).await,
        FULL,
        "올해 부여는 줄지 않는다"
    );
    let balance = assert_ledger_invariants(&w.app, &w.kim).await;
    assert_eq!(balance.used.quarters(), 4);
}

// ── 동시성 ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_requests_cannot_spend_the_same_balance_twice() {
    let w = world().await;
    // 하루(4)만 남긴다.
    adjust(&w.app, &w.owner, w.kim.id, 4 - FULL)
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(remaining(&w.app, &w.kim).await, 4);

    let days = [
        "2026-03-09",
        "2026-03-10",
        "2026-03-11",
        "2026-03-12",
        "2026-03-13",
        "2026-03-16",
        "2026-03-17",
        "2026-03-18",
    ];
    let tasks: Vec<_> = days
        .into_iter()
        .map(|day| {
            let (app, kim) = (w.app.clone(), w.kim.clone());
            let body = leave(&w.annual, day, day, "full_day");
            tokio::spawn(async move { request(&app, &kim, &body).await })
        })
        .collect();
    let mut created = 0;
    for task in tasks {
        let response = task.await.expect("join");
        if response.status == StatusCode::CREATED {
            created += 1;
        } else {
            response.expect_error(StatusCode::CONFLICT, "insufficient_leave_balance");
        }
    }
    assert_eq!(created, 1, "정확히 한 건만 성공한다");
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 0));
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.todo").await,
        1
    );
    let balance = assert_ledger_invariants(&w.app, &w.kim).await;
    assert_eq!(balance.remaining.quarters(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_requests_and_cancellations_keep_the_ledger_consistent() {
    let w = world().await;
    let (id, doc) = requested(&w, &w.kim, "2026-03-10", "2026-03-10").await;
    // 승인, 회수, 취소가 한꺼번에 온다.
    let approving = {
        let (app, token, doc) = (w.app.clone(), w.lead.token.clone(), doc.clone());
        tokio::spawn(async move { decide(&app, &token, &doc, "approve").await.status })
    };
    let cancels: Vec<_> = (0..4)
        .map(|_| {
            let (app, token, id) = (w.app.clone(), w.kim.token.clone(), id.clone());
            tokio::spawn(async move { cancel(&app, &token, &id).await.status })
        })
        .collect();
    let approved = approving.await.expect("join");
    for task in cancels {
        // 승인이 먼저면 시작 전 취소로, 회수가 먼저면 반복 취소로 모두 성공한다.
        assert_eq!(task.await.expect("join"), StatusCode::OK);
    }
    assert!(approved == StatusCode::OK || approved == StatusCode::CONFLICT);
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "canceled");
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 1));
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_ledger_invariants(&w.app, &w.kim).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_accrual_runs_insert_each_grant_once() {
    let w = world().await;
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (app, owner) = (w.app.clone(), w.owner.clone());
            tokio::spawn(async move {
                app.post("/admin/leave/accrual/run")
                    .bearer(&owner)
                    .send()
                    .await
                    .expect(StatusCode::OK)
            })
        })
        .collect();
    let mut inserted = 0;
    for task in tasks {
        let result = task.await.expect("join");
        assert_eq!(result["members"], 3);
        inserted += result["grants_inserted"].as_i64().expect("count");
    }
    // 2024-03-02 입사: 월 부여 11건 + 1·2주년 부여 2건. 대표는 오늘 입사라 없다.
    assert_eq!(inserted, 13 * 2);
    for member in [&w.kim, &w.lead] {
        let records = stored_ledger(&w.app, member.id).await;
        let keys: Vec<&str> = records
            .iter()
            .filter_map(|record| record.grant_key.as_deref())
            .collect();
        let unique: std::collections::BTreeSet<&str> = keys.iter().copied().collect();
        assert_eq!(keys.len(), 13);
        assert_eq!(unique.len(), 13);
        assert!(unique.contains("annual:2"));
        assert!(unique.contains("monthly:1"));
    }
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_eq!(stored_ledger(&w.app, w.kim.id).await.len(), 13);

    let again = w
        .app
        .post("/admin/leave/accrual/run")
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(again, json!({ "members": 3, "grants_inserted": 0 }));
    w.app
        .post("/admin/leave/accrual/run")
        .bearer(&w.kim.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
}

// ── 자동 부여 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn new_grant_appears_on_the_anniversary_and_the_old_one_expires() {
    let mut w = world().await;
    let (_, doc) = requested(&w, &w.kim, "2026-03-10", "2026-03-11").await;
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    let before = balance_of(&w.app, &w.kim.token).await;
    assert_eq!(before.balance.remaining.quarters(), 52);
    assert_eq!(before.expiring_soon.quarters(), 0);
    assert_eq!(before.as_of, date("2026-03-02"));

    // 소멸 2주 전: 남은 13일이 곧 사라진다고 알려 준다.
    w.app.set_time("2027-02-15T00:00:00Z");
    w.kim.token = w.app.login("kim", MEMBER_PASSWORD).await;
    let soon = balance_of(&w.app, &w.kim.token).await;
    assert_eq!(soon.balance.remaining.quarters(), 52);
    assert_eq!(soon.expiring_soon.quarters(), 52);

    // 소멸일(2027-03-01) 당일까지는 유효하다.
    w.app.set_time("2027-03-01T14:59:59Z");
    w.kim.token = w.app.login("kim", MEMBER_PASSWORD).await;
    let last_day = balance_of(&w.app, &w.kim.token).await;
    assert_eq!(last_day.balance.remaining.quarters(), 52);
    assert_eq!(last_day.balance.expired, before.balance.expired);

    // 3주년: 16일이 새로 생기고 지난해 남은 13일은 소멸한다.
    w.app.set_time("2027-03-01T15:00:00Z");
    let after = balance_of(&w.app, &w.kim.token).await;
    assert_eq!(after.as_of, date("2027-03-02"));
    assert_eq!(after.balance.remaining.quarters(), 64);
    assert_eq!(after.expiring_soon.quarters(), 0);
    assert_eq!(
        after.balance.expired.quarters() - before.balance.expired.quarters(),
        52
    );
    let records = stored_ledger(&w.app, w.kim.id).await;
    let new_grant: Vec<&LedgerRecord> = records
        .iter()
        .filter(|record| record.grant_key.as_deref() == Some("annual:3"))
        .collect();
    assert_eq!(new_grant.len(), 1);
    assert_eq!(new_grant[0].entry.amount.quarters(), 64);
    assert_eq!(new_grant[0].actor_id, None, "자동 부여는 시스템이 넣는다");
    assert_ledger_invariants(&w.app, &w.kim).await;

    // 다시 조회해도 부여가 늘지 않는다.
    balance_of(&w.app, &w.kim.token).await;
    assert_eq!(stored_ledger(&w.app, w.kim.id).await.len(), records.len());
}

#[tokio::test]
async fn request_uses_grants_that_became_due_since_the_last_look() {
    let w = world().await;
    // 잔여를 한 번도 조회하지 않아 원장이 비어 있어도 신청 시점에 부여가 반영된다.
    assert!(stored_ledger(&w.app, w.kim.id).await.is_empty());
    requested(&w, &w.kim, "2026-03-10", "2026-03-10").await;
    assert_eq!(stored_ledger(&w.app, w.kim.id).await.len(), 14);
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 4);
}

// ── 취소 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn approved_leave_can_be_canceled_by_the_member_only_before_it_starts() {
    let w = world().await;
    let (first, first_doc) = requested(&w, &w.kim, "2026-03-12", "2026-03-12").await;
    let (second, second_doc) = requested(&w, &w.kim, "2026-03-13", "2026-03-16").await;
    for doc in [&first_doc, &second_doc] {
        decide(&w.app, &w.lead.token, doc, "approve")
            .await
            .expect(StatusCode::OK);
    }
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 12);

    // 시작 전날 23:59:59(KST): 취소할 수 있다. 문서는 승인된 기록으로 남는다.
    w.app.set_time("2026-03-11T14:59:59Z");
    let canceled = cancel(&w.app, &w.kim.token, &first)
        .await
        .expect(StatusCode::OK);
    assert_eq!(canceled["status"], "canceled");
    assert_eq!(
        document(&w.app, &w.kim.token, &first_doc).await["status"],
        "approved"
    );
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 8);
    assert_eq!(
        notification_count(&w.app, w.lead.id, "leave.canceled").await,
        1
    );
    assert_eq!(
        notification_count(&w.app, w.kim.id, "leave.canceled").await,
        0
    );
    let again = cancel(&w.app, &w.kim.token, &first)
        .await
        .expect(StatusCode::OK);
    assert_eq!(again, canceled);
    assert_eq!(reservations(&w.app, w.kim.id).await, (2, 1));
    assert_eq!(
        notification_count(&w.app, w.lead.id, "leave.canceled").await,
        1
    );

    // 시작일 00:00(KST)부터 본인은 취소할 수 없다.
    w.app.set_time("2026-03-12T15:00:00Z");
    cancel(&w.app, &w.kim.token, &second)
        .await
        .expect_error(StatusCode::CONFLICT, "leave_already_started");
    assert_eq!(status_of(&w.app, &w.kim, &second).await, "approved");
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 8);

    // 남의 신청은 없는 것으로 보이고, 관리 경로는 관리자만 쓴다.
    cancel(&w.app, &w.lead.token, &second)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    for token in [&w.kim.token, &w.lead.token] {
        admin_cancel(&w.app, token, &second)
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    assert_eq!(reservations(&w.app, w.kim.id).await, (2, 1));

    // 관리자는 시작한 뒤에도 취소할 수 있다.
    let by_admin = admin_cancel(&w.app, &w.owner, &second)
        .await
        .expect(StatusCode::OK);
    assert_eq!(by_admin["status"], "canceled");
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_eq!(
        notification_count(&w.app, w.kim.id, "leave.canceled").await,
        1
    );
    assert_eq!(
        notification_count(&w.app, w.lead.id, "leave.canceled").await,
        2
    );
    assert_eq!(audit_count(&w.app, "leave.admin_cancel").await, 1);
    assert_eq!(
        admin_cancel(&w.app, &w.owner, &second)
            .await
            .expect(StatusCode::OK),
        by_admin
    );
    assert_eq!(audit_count(&w.app, "leave.admin_cancel").await, 1);
    assert_eq!(reservations(&w.app, w.kim.id).await, (2, 2));
    assert_ledger_invariants(&w.app, &w.kim).await;

    for bad in ["not-a-uuid", &MemberId::new().to_string()] {
        cancel(&w.app, &w.kim.token, bad)
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
        admin_cancel(&w.app, &w.owner, bad)
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
}

#[tokio::test]
async fn admin_can_cancel_a_pending_request_on_behalf_of_the_member() {
    let w = world().await;
    let (id, doc) = requested(&w, &w.kim, "2026-03-10", "2026-03-10").await;
    let canceled = admin_cancel(&w.app, &w.owner, &id)
        .await
        .expect(StatusCode::OK);
    assert_eq!(canceled["status"], "canceled");
    let detail = document(&w.app, &w.kim.token, &doc).await;
    assert_eq!(detail["status"], "withdrawn");
    // 이력에는 실제로 취소한 사람이 남는다.
    assert_eq!(detail["events"][1]["kind"], "withdrawn");
    assert_eq!(detail["events"][1]["actor_id"], json!(w.app.owner_id));
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_ledger_invariants(&w.app, &w.kim).await;
}

// ── 멱등성 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn idempotency_key_replay_does_not_reserve_twice() {
    let w = world().await;
    let send = |body: Value| {
        let (app, token) = (w.app.clone(), w.kim.token.clone());
        async move {
            app.post("/leave/requests")
                .bearer(&token)
                .header("idempotency-key", "leave-1")
                .json(&body)
                .send()
                .await
        }
    };
    let body = leave(&w.annual, "2026-03-10", "2026-03-11", "full_day");
    let first = send(body.clone()).await.expect(StatusCode::CREATED);
    let replay = send(body.clone()).await.expect(StatusCode::CREATED);
    assert_eq!(replay, first);
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 0));
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 8);
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.todo").await,
        1
    );

    send(leave(&w.annual, "2026-03-17", "2026-03-17", "full_day"))
        .await
        .expect_error(StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused");
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 0));

    // 키 없이 같은 본문을 다시 보내면 겹침으로 거부된다(두 번 예약되지 않는다).
    request(&w.app, &w.kim, &body)
        .await
        .expect_error(StatusCode::CONFLICT, "leave_overlap");
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 8);
    assert_ledger_invariants(&w.app, &w.kim).await;
}

// ── 차감하지 않는 종류 ───────────────────────────────────────────────────

#[tokio::test]
async fn non_deducting_type_never_touches_the_ledger() {
    let w = world().await;
    let sick = type_id(&w.app, &w.owner, "sick").await;
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    let grants = stored_ledger(&w.app, w.kim.id).await.len();

    let mut ids_by_outcome = Vec::new();
    for day in ["2026-03-10", "2026-03-11", "2026-03-12", "2026-03-13"] {
        let created = request(&w.app, &w.kim, &leave(&sick, day, day, "full_day"))
            .await
            .expect(StatusCode::CREATED);
        assert_eq!(created["amount"], 4);
        ids_by_outcome.push(ids(&created));
    }
    let approve = |doc: String| {
        let (app, token) = (w.app.clone(), w.lead.token.clone());
        async move {
            decide(&app, &token, &doc, "approve")
                .await
                .expect(StatusCode::OK)
        }
    };
    approve(ids_by_outcome[0].1.clone()).await;
    decide(&w.app, &w.lead.token, &ids_by_outcome[1].1, "reject")
        .await
        .expect(StatusCode::OK);
    cancel(&w.app, &w.kim.token, &ids_by_outcome[2].0)
        .await
        .expect(StatusCode::OK);
    approve(ids_by_outcome[3].1.clone()).await;
    cancel(&w.app, &w.kim.token, &ids_by_outcome[3].0)
        .await
        .expect(StatusCode::OK);

    let statuses: Vec<String> = {
        let mut statuses = Vec::new();
        for (id, _) in &ids_by_outcome {
            statuses.push(status_of(&w.app, &w.kim, id).await);
        }
        statuses
    };
    assert_eq!(statuses, ["approved", "rejected", "canceled", "canceled"]);
    assert_eq!(stored_ledger(&w.app, w.kim.id).await.len(), grants);
    assert_eq!(reservations(&w.app, w.kim.id).await, (0, 0));
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);

    // 잔여가 없어도 신청할 수 있다.
    adjust(&w.app, &w.owner, w.kim.id, -FULL)
        .await
        .expect(StatusCode::CREATED);
    request(
        &w.app,
        &w.kim,
        &leave(&sick, "2026-03-17", "2026-03-18", "full_day"),
    )
    .await
    .expect(StatusCode::CREATED);
    request(
        &w.app,
        &w.kim,
        &leave(&w.annual, "2026-03-19", "2026-03-19", "am_quarter"),
    )
    .await
    .expect_error(StatusCode::CONFLICT, "insufficient_leave_balance");
    assert_ledger_invariants(&w.app, &w.kim).await;
}

// ── 권한 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn personal_records_are_visible_to_self_manager_and_admin_only() {
    let w = world().await;
    let park = w.lead_peer().await;
    let sales = w.app.add_department("영업팀", None, None).await;
    let boss = w.app.add_member("boss", Role::Member, Some(sales)).await;
    w.app.set_department_head(sales, Some(boss.id)).await;
    let lee = w.app.add_member("lee", Role::Member, Some(sales)).await;

    let mut body = leave(&w.annual, "2026-03-10", "2026-03-11", "full_day");
    body["reason"] = json!("가족 여행 비밀");
    request(&w.app, &w.kim, &body)
        .await
        .expect(StatusCode::CREATED);
    requested(&w, &lee, "2026-03-17", "2026-03-17").await;

    let kim = w.kim.id;
    let paths = [
        format!("/leave/balance?member_id={kim}"),
        format!("/leave/ledger?member_id={kim}"),
        format!("/leave/requests?year=2026&member_id={kim}"),
    ];
    for path in &paths {
        for (who, token) in [
            ("본인", &w.kim.token),
            ("팀장", &w.lead.token),
            ("관리자", &w.owner),
        ] {
            let response = w.app.get(path).bearer(token).send().await;
            assert_eq!(response.status, StatusCode::OK, "{who} {path}");
        }
        for (who, token) in [
            ("같은 팀 동료", &park.token),
            ("다른 팀 팀장", &boss.token),
            ("다른 팀 팀원", &lee.token),
        ] {
            let response = w.app.get(path).bearer(token).send().await;
            assert_eq!(response.status, StatusCode::NOT_FOUND, "{who} {path}");
            assert!(
                !response.body.to_string().contains("가족"),
                "거부 응답에 내용이 섞이지 않는다"
            );
        }
        w.app
            .get(path)
            .send()
            .await
            .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    // 없는 구성원은 관리자에게도 404이고, 형식이 틀린 ID는 400이다.
    for path in ["/leave/balance", "/leave/ledger", "/leave/requests"] {
        w.app
            .get(&format!("{path}?member_id={}", MemberId::new()))
            .bearer(&w.owner)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
        w.app
            .get(&format!("{path}?member_id=kim"))
            .bearer(&w.owner)
            .send()
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }

    // 팀장이 보는 잔여는 본인이 보는 것과 같다.
    let own = balance_of(&w.app, &w.kim.token).await;
    let seen: BalanceDto = serde_json::from_value(
        w.app
            .get(&paths[0])
            .bearer(&w.lead.token)
            .send()
            .await
            .expect(StatusCode::OK),
    )
    .expect("balance");
    assert_eq!(seen, own);
    assert_eq!(own.member_id, kim);
    let requests = w
        .app
        .get(&paths[2])
        .bearer(&w.lead.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(requests[0]["reason"], "가족 여행 비밀");
    assert_eq!(requests.as_array().expect("requests").len(), 1);
}

#[tokio::test]
async fn calendar_shows_who_is_off_without_reason_or_type() {
    let w = world().await;
    let park = w.lead_peer().await;
    let sales = w.app.add_department("영업팀", None, None).await;
    let boss = w.app.add_member("boss", Role::Member, Some(sales)).await;
    w.app.set_department_head(sales, Some(boss.id)).await;
    let lee = w.app.add_member("lee", Role::Member, Some(sales)).await;

    let mut body = leave(&w.annual, "2026-03-10", "2026-03-11", "full_day");
    body["reason"] = json!("가족 여행 비밀");
    let (_, approved_doc) = ids(&request(&w.app, &w.kim, &body)
        .await
        .expect(StatusCode::CREATED));
    decide(&w.app, &w.lead.token, &approved_doc, "approve")
        .await
        .expect(StatusCode::OK);
    let mut half = leave(&w.annual, "2026-03-20", "2026-03-20", "pm_half");
    half["reason"] = json!("병원 진료 비밀");
    request(&w.app, &w.kim, &half)
        .await
        .expect(StatusCode::CREATED);
    let (rejected, rejected_doc) = requested(&w, &w.kim, "2026-03-24", "2026-03-24").await;
    decide(&w.app, &w.lead.token, &rejected_doc, "reject")
        .await
        .expect(StatusCode::OK);
    requested(&w, &w.kim, "2026-04-01", "2026-04-01").await;
    requested(&w, &lee, "2026-03-17", "2026-03-17").await;

    let calendar = |token: &str, month: &str| {
        let (app, token) = (w.app.clone(), token.to_owned());
        let path = format!("/leave/calendar?month={month}");
        async move {
            app.get(&path)
                .bearer(&token)
                .send()
                .await
                .expect(StatusCode::OK)
        }
    };
    let names = |entries: &Value| -> Vec<(String, String, String)> {
        entries
            .as_array()
            .expect("entries")
            .iter()
            .map(|entry| {
                let text = |key: &str| entry[key].as_str().expect("text").to_owned();
                (text("member_name"), text("start_date"), text("status"))
            })
            .collect()
    };
    let kims = [
        (
            "kim 님".to_owned(),
            "2026-03-10".to_owned(),
            "approved".to_owned(),
        ),
        (
            "kim 님".to_owned(),
            "2026-03-20".to_owned(),
            "pending".to_owned(),
        ),
    ];
    let lees = [(
        "lee 님".to_owned(),
        "2026-03-17".to_owned(),
        "pending".to_owned(),
    )];

    // 본인, 같은 팀 동료, 팀장은 개발팀 일정만 본다.
    for token in [&w.kim.token, &park.token, &w.lead.token] {
        let entries = calendar(token, "2026-03").await;
        assert_eq!(names(&entries), kims);
        let raw = entries.to_string();
        for leaked in [
            "비밀",
            "reason",
            "leave_type",
            "연차",
            "document_id",
            "amount",
            &rejected,
        ] {
            assert!(!raw.contains(leaked), "달력에 {leaked}이(가) 노출됐다");
        }
        assert_eq!(entries[1]["unit"], "pm_half");
        assert_eq!(entries[0]["end_date"], "2026-03-11");
    }
    // 다른 팀은 자기 팀 일정만, 관리자는 전부.
    for token in [&boss.token, &lee.token] {
        assert_eq!(names(&calendar(token, "2026-03").await), lees);
    }
    let everything = calendar(&w.owner, "2026-03").await;
    assert_eq!(everything.as_array().expect("entries").len(), 3);
    assert_eq!(names(&calendar(&w.kim.token, "2026-04").await).len(), 1);
    assert_eq!(calendar(&w.kim.token, "2026-05").await, json!([]));

    // 팀 휴가: 조직장과 관리자만, 종류와 사유까지.
    for token in [&w.kim.token, &park.token, &lee.token] {
        w.app
            .get("/team/leave?month=2026-03")
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    let team = |token: &str| {
        let (app, token) = (w.app.clone(), token.to_owned());
        async move {
            app.get("/team/leave?month=2026-03")
                .bearer(&token)
                .send()
                .await
                .expect(StatusCode::OK)
        }
    };
    let dev_team = team(&w.lead.token).await;
    assert_eq!(dev_team.as_array().expect("team").len(), 2);
    assert_eq!(dev_team[0]["reason"], "가족 여행 비밀");
    assert_eq!(dev_team[0]["leave_type_name"], "연차");
    assert_eq!(dev_team[1]["reason"], "병원 진료 비밀");
    let sales_team = team(&boss.token).await;
    assert_eq!(sales_team.as_array().expect("team").len(), 1);
    assert_eq!(sales_team[0]["member_name"], "lee 님");
    assert!(!sales_team.to_string().contains("비밀"));
    assert_eq!(team(&w.owner).await.as_array().expect("team").len(), 3);

    for month in [
        "",
        "2026",
        "2026-3",
        "2026-13",
        "2026-03-01",
        "garbage",
        "%27%3B--",
    ] {
        for path in ["/leave/calendar", "/team/leave"] {
            let response = w
                .app
                .get(&format!("{path}?month={month}"))
                .bearer(&w.owner)
                .send()
                .await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST, "{path} {month:?}");
        }
    }
    w.app
        .get("/leave/calendar")
        .bearer(&w.owner)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
}

// ── 관리 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn admin_adjustment_is_bounded_and_audited() {
    let w = world().await;
    for token in [&w.kim.token, &w.lead.token] {
        adjust(&w.app, token, w.kim.id, 4)
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    for amount in [0, 401, -401, i64::from(i32::MAX), i64::from(i32::MIN)] {
        let response = adjust(&w.app, &w.owner, w.kim.id, amount).await;
        assert_eq!(response.body["error"]["field"], "amount_q", "{amount}");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    for amount in [i64::from(i32::MAX) + 1, i64::MIN] {
        adjust(&w.app, &w.owner, w.kim.id, amount)
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    let send = |body: Value| {
        let (app, owner) = (w.app.clone(), w.owner.clone());
        async move {
            app.post("/admin/leave/adjustments")
                .bearer(&owner)
                .json(&body)
                .send()
                .await
        }
    };
    let valid = json!({
        "member_id": w.kim.id, "amount_q": 2, "effective_on": "2026-03-02", "memo": "포상"
    });
    let long_memo = "가".repeat(1_001);
    for (field, value, status) in [
        ("memo", json!(""), StatusCode::BAD_REQUEST),
        ("memo", json!("   "), StatusCode::BAD_REQUEST),
        ("memo", json!(long_memo), StatusCode::BAD_REQUEST),
        ("amount_q", json!(1.5), StatusCode::BAD_REQUEST),
        ("amount_q", json!("4"), StatusCode::BAD_REQUEST),
        ("effective_on", json!("2026-02-30"), StatusCode::BAD_REQUEST),
        (
            "effective_on",
            json!("+10000-01-01"),
            StatusCode::BAD_REQUEST,
        ),
        ("member_id", json!("kim"), StatusCode::BAD_REQUEST),
        ("member_id", json!(MemberId::new()), StatusCode::NOT_FOUND),
    ] {
        let mut body = valid.clone();
        body[field] = value;
        assert_eq!(send(body).await.status, status, "{field}");
    }
    assert!(stored_ledger(&w.app, w.kim.id).await.is_empty());
    assert_eq!(audit_count(&w.app, "leave.adjust").await, 0);

    // 경계값 ±400은 통과한다.
    let plus = adjust(&w.app, &w.owner, w.kim.id, 400)
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(plus["balance"]["remaining"], FULL + 400);
    let minus = adjust(&w.app, &w.owner, w.kim.id, -400)
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(minus["balance"]["remaining"], FULL);
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_eq!(audit_count(&w.app, "leave.adjust").await, 2);

    let ledger = w
        .app
        .get("/leave/ledger")
        .bearer(&w.kim.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let adjustments: Vec<&Value> = ledger
        .as_array()
        .expect("ledger")
        .iter()
        .filter(|entry| entry["kind"] == "adjust")
        .collect();
    assert_eq!(adjustments.len(), 2);
    assert_eq!(adjustments[0]["amount"], 400);
    assert_eq!(adjustments[0]["memo"], "테스트 조정");
    assert_eq!(adjustments[1]["amount"], -400);
    assert_eq!(adjustments[1]["effective_on"], "2026-03-02");

    // 음수 조정은 잔여를 음수로 만들 수 있고, 그동안은 신청할 수 없다.
    adjust(&w.app, &w.owner, w.kim.id, -(FULL + 4))
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(remaining(&w.app, &w.kim).await, -4);
    request(
        &w.app,
        &w.kim,
        &leave(&w.annual, "2026-03-10", "2026-03-10", "am_quarter"),
    )
    .await
    .expect_error(StatusCode::CONFLICT, "insufficient_leave_balance");
    assert_ledger_invariants(&w.app, &w.kim).await;
}

#[tokio::test]
async fn admin_sees_all_balances_and_requests() {
    let w = world().await;
    requested(&w, &w.kim, "2026-03-10", "2026-03-10").await;
    let (rejected, doc) = requested(&w, &w.kim, "2026-03-11", "2026-03-11").await;
    decide(&w.app, &w.lead.token, &doc, "reject")
        .await
        .expect(StatusCode::OK);
    let mut explicit = leave(&w.annual, "2026-03-12", "2026-03-12", "full_day");
    explicit["steps"] = json!([{ "approver_id": w.app.owner_id, "kind": "approve" }]);
    request(&w.app, &w.lead, &explicit)
        .await
        .expect(StatusCode::CREATED);

    let balances = w
        .app
        .get("/admin/leave/balances")
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    let by_name: BTreeMap<&str, i64> = balances
        .as_array()
        .expect("balances")
        .iter()
        .map(|row| {
            (
                row["member_name"].as_str().expect("name"),
                row["balance"]["remaining"].as_i64().expect("remaining"),
            )
        })
        .collect();
    assert_eq!(
        by_name,
        BTreeMap::from([("kim 님", FULL - 4), ("lead 님", FULL - 4), ("대표", 0)])
    );
    assert_eq!(balances[0]["department_name"], "개발팀");

    // 퇴사자는 목록에서 빠진다.
    let mut tx = w.app.state.db.begin_write().await.expect("begin");
    let mut kim = members::find(&mut tx, w.kim.id)
        .await
        .expect("find")
        .expect("kim");
    kim.status = MemberStatus::Retired;
    assert!(members::update(&mut tx, &kim, w.app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");
    let balances = w
        .app
        .get("/admin/leave/balances")
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(balances.as_array().expect("balances").len(), 2);

    let list = |query: String| {
        let (app, owner) = (w.app.clone(), w.owner.clone());
        async move {
            app.get(&format!("/admin/leave/requests{query}"))
                .bearer(&owner)
                .send()
                .await
                .expect(StatusCode::OK)
        }
    };
    let all = list(String::new()).await;
    assert_eq!(all["items"].as_array().expect("items").len(), 3);
    assert_eq!(all["next_cursor"], Value::Null);
    let first_page = list("?limit=2".into()).await;
    let cursor = first_page["next_cursor"]
        .as_str()
        .expect("cursor")
        .to_owned();
    let second_page = list(format!("?limit=2&cursor={cursor}")).await;
    let mut paged: Vec<Value> = first_page["items"].as_array().expect("items").clone();
    paged.extend(second_page["items"].as_array().expect("items").clone());
    assert_eq!(json!(paged), all["items"]);
    assert_eq!(second_page["next_cursor"], Value::Null);
    let only_rejected = list("?status=rejected".into()).await;
    assert_eq!(only_rejected["items"][0]["id"], rejected.as_str());
    assert_eq!(only_rejected["items"].as_array().expect("items").len(), 1);
    let of_lead = list(format!("?member_id={}", w.lead.id)).await;
    assert_eq!(of_lead["items"].as_array().expect("items").len(), 1);

    for path in [
        "/admin/leave/balances",
        "/admin/leave/requests",
        "/admin/leave/types",
    ] {
        w.app
            .get(path)
            .bearer(&w.lead.token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    w.app
        .get("/admin/leave/requests?cursor=garbage")
        .bearer(&w.owner)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    w.app
        .get("/admin/leave/requests?status=done")
        .bearer(&w.owner)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
}

#[tokio::test]
async fn admin_manages_leave_types() {
    let w = world().await;
    let types = w
        .app
        .get("/leave/types")
        .bearer(&w.kim.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let seeded: Vec<(&str, &str, bool, bool)> = types
        .as_array()
        .expect("types")
        .iter()
        .map(|t| {
            (
                t["code"].as_str().expect("code"),
                t["name"].as_str().expect("name"),
                t["deducts_balance"].as_bool().expect("deducts"),
                t["allow_partial"].as_bool().expect("partial"),
            )
        })
        .collect();
    assert_eq!(
        seeded,
        [
            ("annual", "연차", true, true),
            ("sick", "병가", false, false),
            ("family_event", "경조사", false, false),
            ("official", "공가", false, false),
        ]
    );

    let reward = json!({
        "code": "reward", "name": "포상휴가", "deducts_balance": false, "allow_partial": true
    });
    w.app
        .post("/admin/leave/types")
        .bearer(&w.kim.token)
        .json(&reward)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    let created = w
        .app
        .post("/admin/leave/types")
        .bearer(&w.owner)
        .json(&reward)
        .send()
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(created["active"], true);
    w.app
        .post("/admin/leave/types")
        .bearer(&w.owner)
        .json(&reward)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "already_exists");
    for (field, value) in [
        ("code", "Reward"),
        ("code", "a"),
        ("code", "포상"),
        ("name", ""),
        ("name", "줄\n바꿈"),
    ] {
        let mut body = reward.clone();
        body[field] = json!(value);
        let response = w
            .app
            .post("/admin/leave/types")
            .bearer(&w.owner)
            .json(&body)
            .send()
            .await;
        assert_eq!(response.body["error"]["field"], field);
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }

    // 수정: 버전 확인, 코드와 차감 여부는 바뀌지 않는다.
    let id = created["id"].as_str().expect("id");
    let path = format!("/admin/leave/types/{id}");
    let edit = json!({
        "version": 1, "name": "포상", "allow_partial": false, "active": true, "sort_order": 7,
        "code": "renamed", "deducts_balance": true
    });
    let updated = w
        .app
        .patch(&path)
        .bearer(&w.owner)
        .json(&edit)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(updated["version"], 2);
    assert_eq!(updated["name"], "포상");
    assert_eq!(updated["code"], "reward");
    assert_eq!(updated["deducts_balance"], false);
    assert_eq!(updated["allow_partial"], false);
    w.app
        .patch(&path)
        .bearer(&w.owner)
        .json(&edit)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    w.app
        .patch(&path)
        .bearer(&w.lead.token)
        .json(&edit)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    w.app
        .patch(&format!("/admin/leave/types/{}", MemberId::new()))
        .bearer(&w.owner)
        .json(&edit)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");

    // 결재 중인 신청이 있으면 끌 수 없다.
    let (_, doc) = requested(&w, &w.kim, "2026-03-10", "2026-03-10").await;
    let annual_path = format!("/admin/leave/types/{}", w.annual);
    let off = json!({ "version": 1, "name": "연차", "allow_partial": true, "active": false });
    w.app
        .patch(&annual_path)
        .bearer(&w.owner)
        .json(&off)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "in_use");
    decide(&w.app, &w.lead.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    w.app
        .patch(&annual_path)
        .bearer(&w.owner)
        .json(&off)
        .send()
        .await
        .expect(StatusCode::OK);

    // 꺼진 종류로는 신청할 수 없고 목록에서 빠진다. 이미 승인된 휴가는 그대로다.
    request(
        &w.app,
        &w.kim,
        &leave(&w.annual, "2026-03-17", "2026-03-17", "full_day"),
    )
    .await
    .expect_error(StatusCode::CONFLICT, "leave_type_inactive");
    let visible = w
        .app
        .get("/leave/types")
        .bearer(&w.kim.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert!(visible
        .as_array()
        .expect("types")
        .iter()
        .all(|t| t["code"] != "annual"));
    let all = w
        .app
        .get("/admin/leave/types")
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(all.as_array().expect("types").len(), 5);
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 4);
    assert_ledger_invariants(&w.app, &w.kim).await;
}

// ── 결재선 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn request_needs_an_approver_and_validates_an_explicit_line() {
    let w = world().await;
    let solo = w.app.add_member("solo", Role::Member, None).await;
    let body = leave(&w.annual, "2026-03-10", "2026-03-10", "full_day");

    // 조직장이 없으면 결재선을 직접 지정해야 한다.
    let response = request(&w.app, &solo, &body).await;
    assert_eq!(response.body["error"]["field"], "steps");
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    // 팀장 본인도 위에 조직장이 없으면 마찬가지다.
    request(&w.app, &w.lead, &body)
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    assert_eq!(reservations(&w.app, solo.id).await, (0, 0));

    let with_steps = |steps: Value, references: Value| {
        let mut body = body.clone();
        body["steps"] = steps;
        body["references"] = references;
        body
    };
    let lead_step = json!({ "approver_id": w.lead.id, "kind": "approve" });
    let bad_lines = [
        (json!([]), json!([]), StatusCode::BAD_REQUEST),
        (
            json!([{ "approver_id": solo.id, "kind": "approve" }]),
            json!([]),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!([lead_step, lead_step]),
            json!([]),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!([{ "approver_id": w.lead.id, "kind": "agree" }]),
            json!([]),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!([lead_step]),
            json!([w.lead.id]),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!([{ "approver_id": MemberId::new(), "kind": "approve" }]),
            json!([]),
            StatusCode::NOT_FOUND,
        ),
        (
            json!([lead_step]),
            json!([MemberId::new()]),
            StatusCode::NOT_FOUND,
        ),
    ];
    for (steps, references, status) in bad_lines {
        let response = request(&w.app, &solo, &with_steps(steps.clone(), references)).await;
        assert_eq!(response.status, status, "{steps}");
    }

    // 재직 중이 아닌 결재자.
    let mut tx = w.app.state.db.begin_write().await.expect("begin");
    let mut kim = members::find(&mut tx, w.kim.id)
        .await
        .expect("find")
        .expect("kim");
    kim.status = MemberStatus::Suspended;
    assert!(members::update(&mut tx, &kim, w.app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");
    let inactive = json!([{ "approver_id": w.kim.id, "kind": "approve" }]);
    request(&w.app, &solo, &with_steps(inactive, json!([])))
        .await
        .expect_error(StatusCode::CONFLICT, "member_inactive");

    // 거부된 신청은 아무것도 남기지 않았다.
    assert_eq!(reservations(&w.app, solo.id).await, (0, 0));
    assert_eq!(remaining(&w.app, &solo).await, FULL);
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.todo").await,
        0
    );
    let mut conn = w.app.state.read().await.expect("read");
    assert_eq!(
        documents::mailbox_counts(&mut conn, solo.id)
            .await
            .expect("counts")
            .in_progress_drafted,
        0
    );
    drop(conn);

    // 소속 부서가 없는 구성원의 신청은 관리자만 최종 승인할 수 있다:
    // 다른 부서의 조직장을 최종 결재자로 지정하면 거부된다.
    request(
        &w.app,
        &solo,
        &with_steps(json!([lead_step.clone()]), json!([])),
    )
    .await
    .expect_error(StatusCode::BAD_REQUEST, "validation_failed");

    let owner_step = json!({ "approver_id": w.app.owner_id, "kind": "approve" });
    let created = request(
        &w.app,
        &solo,
        &with_steps(json!([owner_step]), json!([w.lead.id])),
    )
    .await
    .expect(StatusCode::CREATED);
    let detail = document(&w.app, &w.owner, &ids(&created).1).await;
    assert_eq!(detail["references"][0]["member_id"], json!(w.lead.id));
    assert_eq!(remaining(&w.app, &solo).await, FULL - 4);
    assert_ledger_invariants(&w.app, &solo).await;
}

// ── 기형·악의적 입력 ─────────────────────────────────────────────────────

#[tokio::test]
async fn malformed_requests_are_rejected_without_side_effects() {
    let w = world().await;
    let valid = leave(&w.annual, "2026-03-10", "2026-03-10", "full_day");
    let long_reason = "가".repeat(1_001);
    let malformed: Vec<(&str, Value)> = vec![
        ("start_date", json!("2026-13-01")),
        ("start_date", json!("2026-02-30")),
        ("start_date", json!("03/10/2026")),
        ("start_date", json!(20260310)),
        ("end_date", json!("")),
        ("end_date", Value::Null),
        ("unit", json!("all_day")),
        ("unit", json!("")),
        ("leave_type_id", json!("annual")),
        ("leave_type_id", json!(w.annual.to_uppercase())),
        ("steps", json!("lead")),
    ];
    for (field, value) in malformed {
        let mut body = valid.clone();
        body[field] = value.clone();
        request(&w.app, &w.kim, &body)
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    let invalid: Vec<(&str, Value, &str)> = vec![
        ("end_date", json!("2026-03-09"), "date_range"),
        ("end_date", json!("2027-04-14"), "date_range"),
        ("end_date", json!("+10000-01-01"), "end_date"),
        ("start_date", json!("1899-12-31"), "start_date"),
        ("reason", json!(""), "reason"),
        ("reason", json!(" \n "), "reason"),
        ("reason", json!(long_reason), "reason"),
        ("reason", json!("널\0문자"), "reason"),
    ];
    for (field, value, error_field) in invalid {
        let mut body = valid.clone();
        body[field] = value.clone();
        let response = request(&w.app, &w.kim, &body).await;
        assert_eq!(
            response.body["error"]["field"], error_field,
            "{field} {value}"
        );
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    let mut unknown_type = valid.clone();
    unknown_type["leave_type_id"] = json!(MemberId::new());
    request(&w.app, &w.kim, &unknown_type)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    w.app
        .post("/leave/requests")
        .json(&valid)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    for query in ["?year=1999", "?year=10000", "?year=-1"] {
        w.app
            .get(&format!("/leave/requests{query}"))
            .bearer(&w.kim.token)
            .send()
            .await
            .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    w.app
        .get("/leave/requests?year=twenty")
        .bearer(&w.kim.token)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");

    // 아무것도 만들어지지 않았다.
    assert_eq!(reservations(&w.app, w.kim.id).await, (0, 0));
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.todo").await,
        0
    );
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    let requests = w
        .app
        .get("/leave/requests")
        .bearer(&w.kim.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(requests, json!([]));

    // 사유의 특수문자는 그대로 저장될 뿐이다.
    let mut odd = valid.clone();
    odd["reason"] = json!("<b>굵게</b>'; DROP TABLE leave_ledger;--");
    let created = request(&w.app, &w.kim, &odd)
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(created["reason"], odd["reason"]);
    assert_ledger_invariants(&w.app, &w.kim).await;
}

// ── 근태 연동 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn units_by_date_reports_approved_working_days_with_the_larger_unit() {
    let w = world().await;
    let submit = |start: &'static str, end: &'static str, unit: &'static str, approve: bool| {
        let (app, kim, lead) = (w.app.clone(), w.kim.clone(), w.lead.token.clone());
        let body = leave(&w.annual, start, end, unit);
        async move {
            let created = request(&app, &kim, &body).await.expect(StatusCode::CREATED);
            if approve {
                decide(&app, &lead, &ids(&created).1, "approve")
                    .await
                    .expect(StatusCode::OK);
            }
        }
    };
    submit("2026-03-06", "2026-03-10", "full_day", true).await; // 금, (주말), 월, 화
    submit("2026-03-11", "2026-03-11", "am_quarter", true).await;
    submit("2026-03-11", "2026-03-11", "pm_half", true).await;
    submit("2026-03-12", "2026-03-12", "pm_half", true).await;
    submit("2026-03-12", "2026-03-12", "am_quarter", true).await;
    submit("2026-03-13", "2026-03-13", "full_day", false).await; // 대기 중은 제외

    let range = DateRange::new(date("2026-03-09"), date("2026-03-31")).expect("range");
    let mut conn = w.app.state.read().await.expect("read");
    let units = leave_service::units_by_date(&mut conn, w.kim.id, range)
        .await
        .expect("units");
    let expected = BTreeMap::from([
        (date("2026-03-09"), LeaveUnit::FullDay),
        (date("2026-03-10"), LeaveUnit::FullDay),
        (date("2026-03-11"), LeaveUnit::PmHalf),
        (date("2026-03-12"), LeaveUnit::PmHalf),
    ]);
    assert_eq!(units, expected);
    let whole = DateRange::new(date("2026-03-01"), date("2026-03-08")).expect("range");
    let units = leave_service::units_by_date(&mut conn, w.kim.id, whole)
        .await
        .expect("units");
    assert_eq!(
        units,
        BTreeMap::from([(date("2026-03-06"), LeaveUnit::FullDay)])
    );
    let other = leave_service::units_by_date(&mut conn, w.lead.id, range)
        .await
        .expect("units");
    assert!(other.is_empty());
}

// ── 입사일 정정과 자동 부여 ───────────────────────────────────────────────

async fn set_hired_on(w: &World, member: MemberId, hired_on: &str) {
    let current = w
        .app
        .get(&format!("/admin/members/{member}"))
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    let mut body = json!({
        "version": current["version"], "name": current["name"], "hired_on": hired_on,
    });
    body["department_id"] = current["department_id"].clone();
    w.app
        .patch(&format!("/admin/members/{member}"))
        .bearer(&w.owner)
        .json(&body)
        .send()
        .await
        .expect(StatusCode::OK);
}

fn grant_keys(records: &[LedgerRecord]) -> Vec<String> {
    records
        .iter()
        .filter_map(|record| record.grant_key.clone())
        .collect()
}

/// 부여 키가 날짜였을 때는 입사일을 하루만 고쳐도 모든 키가 새것이 되어 한 벌이 더 들어갔다
/// (잔여 15일 → 30일). 키는 차수이므로 이미 받은 차수는 다시 들어가지 않는다.
#[tokio::test]
async fn correcting_the_hire_date_does_not_duplicate_grants() {
    let w = world().await;
    // 전원의 부여를 먼저 반영해 둔다(아래에서 "더 들어간 것이 없다"를 세기 위해).
    w.app
        .post("/admin/leave/accrual/run")
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    let granted = stored_ledger(&w.app, w.kim.id).await;
    assert_eq!(grant_keys(&granted).len(), 13);

    for hired_on in ["2024-03-01", "2024-03-03", "2024-02-29", "2024-03-02"] {
        set_hired_on(&w, w.kim.id, hired_on).await;
        // 본인 조회, 관리자 조회, 전원 부여 실행 모두 아무것도 더 넣지 않는다.
        assert_eq!(remaining(&w.app, &w.kim).await, FULL, "hired_on {hired_on}");
        let run = w
            .app
            .post("/admin/leave/accrual/run")
            .bearer(&w.owner)
            .send()
            .await
            .expect(StatusCode::OK);
        assert_eq!(run["grants_inserted"], 0, "hired_on {hired_on}");
        let balances = w
            .app
            .get("/admin/leave/balances")
            .bearer(&w.owner)
            .send()
            .await
            .expect(StatusCode::OK);
        let kim = balances
            .as_array()
            .expect("rows")
            .iter()
            .find(|row| row["member_id"] == json!(w.kim.id))
            .expect("kim");
        assert_eq!(kim["balance"]["remaining"], FULL);
        assert_eq!(kim["hired_on"], hired_on);
        // 이미 들어간 부여는 날짜·만료일까지 그대로다.
        assert_eq!(stored_ledger(&w.app, w.kim.id).await, granted);
    }
    let keys = grant_keys(&stored_ledger(&w.app, w.kim.id).await);
    let unique: std::collections::BTreeSet<&String> = keys.iter().collect();
    assert_eq!(unique.len(), keys.len(), "차수가 겹치지 않는다");
    let expected: std::collections::BTreeSet<String> = (1..=11)
        .map(|m| format!("monthly:{m}"))
        .chain(["annual:1".to_owned(), "annual:2".to_owned()])
        .collect();
    assert_eq!(
        keys.into_iter().collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    assert_ledger_invariants(&w.app, &w.kim).await;
}

/// 입사일을 앞당겨 새 차수가 생기면 그 차수만 새 입사일 기준으로 들어간다.
#[tokio::test]
async fn moving_the_hire_date_earlier_adds_only_the_missing_ordinals() {
    let w = world().await;
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    let before = stored_ledger(&w.app, w.kim.id).await;
    // 2024-03-02 → 2023-03-02: 만 3년 차수(annual:3) 하나가 새로 생긴다.
    set_hired_on(&w, w.kim.id, "2023-03-02").await;
    balance_of(&w.app, &w.kim.token).await;
    let after = stored_ledger(&w.app, w.kim.id).await;
    assert_eq!(after[..before.len()], before[..], "기존 줄은 그대로");
    assert_eq!(grant_keys(&after[before.len()..]), ["annual:3"]);
    assert_eq!(
        after[before.len()].entry.kind,
        LedgerEntryKind::Grant {
            valid_from: date("2026-03-02"),
            expires_on: Some(date("2027-03-01")),
        }
    );
    // 다시 조회해도 더 들어가지 않는다.
    balance_of(&w.app, &w.kim.token).await;
    assert_eq!(stored_ledger(&w.app, w.kim.id).await, after);
}

// ── 결재자가 사라진 신청 ──────────────────────────────────────────────────

async fn set_member_status(app: &TestApp, member: MemberId, status: MemberStatus) {
    let mut tx = app.state.db.begin_write().await.expect("begin");
    let mut found = members::find(&mut tx, member)
        .await
        .expect("find")
        .expect("member");
    found.status = status;
    assert!(members::update(&mut tx, &found, app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");
}

/// [합의 lee, 결재 lead]로 올린 kim의 연차 1일. lee가 합의한 뒤 lead가 정지됐다.
async fn stranded(w: &World) -> (TestMember, String, String) {
    let lee = w.app.add_member("lee", Role::Member, Some(w.dev)).await;
    let mut body = leave(&w.annual, "2026-03-10", "2026-03-10", "full_day");
    body["steps"] = json!([
        { "approver_id": lee.id, "kind": "agree" },
        { "approver_id": w.lead.id, "kind": "approve" },
    ]);
    let (id, doc) = ids(&request(&w.app, &w.kim, &body)
        .await
        .expect(StatusCode::CREATED));
    decide(&w.app, &lee.token, &doc, "approve")
        .await
        .expect(StatusCode::OK);
    set_member_status(&w.app, w.lead.id, MemberStatus::Suspended).await;
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 4);
    (lee, id, doc)
}

async fn admin_reject(app: &TestApp, token: &str, document: &str) -> TestResponse {
    app.post(&format!("/admin/approval/documents/{document}/reject"))
        .bearer(token)
        .json(&json!({ "reason": "결재자 부재" }))
        .send()
        .await
}

/// 결재자가 정지되면 본인도 관리자도 신청을 끝낼 수 없어 잔여가 영영 예약돼 있었다.
#[tokio::test]
async fn admin_reject_restores_the_balance_of_a_stranded_request() {
    let w = world().await;
    let (lee, id, doc) = stranded(&w).await;
    // 기존 길은 막혀 있다.
    cancel(&w.app, &w.kim.token, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "withdraw_too_late");
    decide(&w.app, &w.owner, &doc, "reject")
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    // 관리자가 아닌 사람은 대리 반려를 쓸 수 없다.
    for token in [&w.kim.token, &lee.token] {
        admin_reject(&w.app, token, &doc)
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "pending");
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 0));

    let closed = admin_reject(&w.app, &w.owner, &doc)
        .await
        .expect(StatusCode::OK);
    assert_eq!(closed["status"], "rejected");
    assert_eq!(status_of(&w.app, &w.kim, &id).await, "rejected");
    assert_eq!(remaining(&w.app, &w.kim).await, FULL, "예약이 풀린다");
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 1));
    assert_eq!(audit_count(&w.app, "approval.admin_reject").await, 1);
    assert_eq!(
        notification_count(&w.app, w.kim.id, "approval.rejected").await,
        1
    );
    assert_eq!(
        notification_count(&w.app, w.lead.id, "approval.admin_rejected").await,
        1
    );

    // 반복해도 복원은 한 번뿐이다.
    admin_reject(&w.app, &w.owner, &doc)
        .await
        .expect(StatusCode::OK);
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 1));
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_eq!(audit_count(&w.app, "approval.admin_reject").await, 1);
    assert_ledger_invariants(&w.app, &w.kim).await;

    // 같은 날짜로 다시 신청할 수 있다(결재선은 재직 중인 사람으로).
    let mut again = leave(&w.annual, "2026-03-10", "2026-03-10", "full_day");
    again["steps"] = json!([{ "approver_id": w.app.owner_id, "kind": "approve" }]);
    request(&w.app, &w.kim, &again)
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 4);
}

/// 관리자의 취소는 회수할 수 없는 문서도 대리 반려로 끝낸다(`withdraw_too_late`가 아니다).
#[tokio::test]
async fn admin_cancel_closes_a_stranded_request() {
    let w = world().await;
    let (_lee, id, doc) = stranded(&w).await;
    let canceled = admin_cancel(&w.app, &w.owner, &id)
        .await
        .expect(StatusCode::OK);
    // 문서는 반려로 끝나고, 신청도 문서와 같이 반려로 끝난다.
    assert_eq!(canceled["status"], "rejected");
    assert!(canceled["decided_at"].is_number());
    let detail = document(&w.app, &w.kim.token, &doc).await;
    assert_eq!(detail["status"], "rejected");
    assert_eq!(detail["steps"][0]["status"], "approved");
    assert_eq!(detail["steps"][1]["status"], "rejected");
    let last = detail["events"]
        .as_array()
        .expect("events")
        .last()
        .expect("e")
        .clone();
    assert_eq!(last["kind"], "rejected");
    assert_eq!(last["actor_id"], json!(w.app.owner_id));
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 1));
    // 취소와 대리 반려 양쪽이 감사 로그에 남는다.
    assert_eq!(audit_count(&w.app, "leave.admin_cancel").await, 1);
    assert_eq!(audit_count(&w.app, "approval.admin_reject").await, 1);
    assert_eq!(
        notification_count(&w.app, w.kim.id, "approval.rejected").await,
        1
    );
    assert_ledger_invariants(&w.app, &w.kim).await;

    // 이미 끝난 신청이다: 다시 취소하면 409이고 아무것도 더 생기지 않는다.
    admin_cancel(&w.app, &w.owner, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    assert_eq!(reservations(&w.app, w.kim.id).await, (1, 1));
    assert_eq!(audit_count(&w.app, "leave.admin_cancel").await, 1);
    assert_eq!(audit_count(&w.app, "approval.admin_reject").await, 1);
}

/// 결재자가 재직 중이어도 관리자의 취소는 진행 중인 문서를 끝낸다. 아무도 결재하지 않은
/// 문서는 예전처럼 회수로 끝난다.
#[tokio::test]
async fn admin_cancel_withdraws_untouched_documents_and_rejects_touched_ones() {
    let w = world().await;
    let (untouched, untouched_doc) = requested(&w, &w.kim, "2026-03-10", "2026-03-10").await;
    let canceled = admin_cancel(&w.app, &w.owner, &untouched)
        .await
        .expect(StatusCode::OK);
    assert_eq!(canceled["status"], "canceled");
    assert_eq!(
        document(&w.app, &w.kim.token, &untouched_doc).await["status"],
        "withdrawn"
    );
    assert_eq!(audit_count(&w.app, "approval.admin_reject").await, 0);

    let mut body = leave(&w.annual, "2026-03-11", "2026-03-11", "full_day");
    body["steps"] = json!([
        { "approver_id": w.lead.id, "kind": "agree" },
        { "approver_id": w.app.owner_id, "kind": "approve" },
    ]);
    let (touched, touched_doc) = ids(&request(&w.app, &w.kim, &body)
        .await
        .expect(StatusCode::CREATED));
    decide(&w.app, &w.lead.token, &touched_doc, "approve")
        .await
        .expect(StatusCode::OK);
    // 조직장은 관리자가 아니므로 관리자 취소를 쓸 수 없다.
    admin_cancel(&w.app, &w.lead.token, &touched)
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(status_of(&w.app, &w.kim, &touched).await, "pending");
    let canceled = admin_cancel(&w.app, &w.owner, &touched)
        .await
        .expect(StatusCode::OK);
    assert_eq!(canceled["status"], "rejected");
    assert_eq!(
        document(&w.app, &w.kim.token, &touched_doc).await["status"],
        "rejected"
    );
    assert_eq!(remaining(&w.app, &w.kim).await, FULL);
    assert_ledger_invariants(&w.app, &w.kim).await;
}

/// 차례인 결재자의 최종 승인과 관리자의 대리 반려가 동시에 오면 하나만 반영되고,
/// 신청·문서·원장이 서로 어긋나지 않는다.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_reject_racing_with_the_final_approval_keeps_leave_consistent() {
    let w = world().await;
    for day in 9..=13 {
        let date = format!("2026-03-{day:02}");
        let mut body = leave(&w.annual, &date, &date, "full_day");
        body["steps"] = json!([
            { "approver_id": w.app.owner_id, "kind": "agree" },
            { "approver_id": w.lead.id, "kind": "approve" },
        ]);
        let (id, doc) = ids(&request(&w.app, &w.kim, &body)
            .await
            .expect(StatusCode::CREATED));
        decide(&w.app, &w.owner, &doc, "approve")
            .await
            .expect(StatusCode::OK);
        let approving = {
            let (app, token, doc) = (w.app.clone(), w.lead.token.clone(), doc.clone());
            tokio::spawn(async move { decide(&app, &token, &doc, "approve").await })
        };
        let rejecting = {
            let (app, token, doc) = (w.app.clone(), w.owner.clone(), doc.clone());
            tokio::spawn(async move { admin_reject(&app, &token, &doc).await })
        };
        let approved = approving.await.expect("join");
        let rejected = rejecting.await.expect("join");
        let status = status_of(&w.app, &w.kim, &id).await;
        let detail = document(&w.app, &w.kim.token, &doc).await;
        assert_eq!(detail["status"], status.as_str(), "신청과 문서가 같은 결과");
        match status.as_str() {
            "approved" => {
                assert_eq!(approved.status, StatusCode::OK);
                rejected.expect_error(StatusCode::CONFLICT, "document_already_closed");
            }
            "rejected" => {
                assert_eq!(rejected.status, StatusCode::OK);
                approved.expect_error(StatusCode::CONFLICT, "document_already_closed");
            }
            other => panic!("unexpected status {other}"),
        }
    }
    // 승인된 것만 차감돼 있다.
    let requests = w
        .app
        .get("/leave/requests?year=2026")
        .bearer(&w.kim.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let approved = requests
        .as_array()
        .expect("requests")
        .iter()
        .filter(|request| request["status"] == "approved")
        .count() as i64;
    assert_eq!(remaining(&w.app, &w.kim).await, FULL - 4 * approved);
    assert_ledger_invariants(&w.app, &w.kim).await;
}
