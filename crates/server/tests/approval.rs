//! 전자결재 통합 테스트.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::Duration;
use http::StatusCode;
use pango_core::approval::{
    ApprovalLine, Document, DocumentKind, DocumentStatus, LineStep, StepKind,
};
use pango_core::authz::Role;
use pango_core::id::{DocumentId, MemberId};
use pango_core::member::MemberStatus;
use pango_db::approval::{self as documents, DocumentRecord};
use pango_db::audit::{self, AuditFilter};
use pango_server::testing::{id_of, TestApp, TestMember, TestResponse, MEMBER_PASSWORD};
use serde_json::{json, Value};

/// 기안자 한 명, 결재자 세 명, 참조자, 결재선 밖의 구성원, 관리자(대표).
struct World {
    app: Arc<TestApp>,
    owner: String,
    drafter: TestMember,
    a1: TestMember,
    a2: TestMember,
    a3: TestMember,
    reader: TestMember,
    outsider: TestMember,
    form: String,
}

async fn world() -> World {
    let app = Arc::new(TestApp::new().await);
    let owner = app.login_owner().await;
    let add = |login: &'static str| app.add_member(login, Role::Member, None);
    let (drafter, a1, a2) = (
        add("drafter").await,
        add("first").await,
        add("second").await,
    );
    let (a3, reader, outsider) = (
        add("third").await,
        add("reader").await,
        add("outsider").await,
    );
    let form = form_id(&app, &owner, "general").await;
    World {
        app,
        owner,
        drafter,
        a1,
        a2,
        a3,
        reader,
        outsider,
        form,
    }
}

async fn promote_to_admin(app: &TestApp, member_id: MemberId) {
    let mut tx = app.state.db.begin_write().await.expect("begin");
    let mut member = pango_db::members::find(&mut tx, member_id)
        .await
        .expect("find")
        .expect("member");
    member.role = Role::Admin;
    assert!(pango_db::members::update(&mut tx, &member, app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");
}

async fn form_id(app: &TestApp, token: &str, code: &str) -> String {
    id_of(&app.approval_form(token, code).await)
}

fn step(member: &TestMember, kind: &str) -> Value {
    json!({ "approver_id": member.id, "kind": kind })
}

fn draft(
    form: &str,
    title: &str,
    steps: Vec<Value>,
    references: &[&TestMember],
    submit: bool,
) -> Value {
    json!({
        "form_id": form,
        "title": title,
        "body": "본문입니다.\n둘째 줄.",
        "steps": steps,
        "references": references.iter().map(|m| m.id).collect::<Vec<_>>(),
        "submit": submit,
    })
}

async fn create(app: &TestApp, who: &TestMember, body: &Value) -> Value {
    app.post("/approval/documents")
        .bearer(&who.token)
        .json(body)
        .send()
        .await
        .expect(StatusCode::CREATED)
}

/// 결재자 한 명짜리 문서를 상신해 ID를 돌려준다.
async fn submitted(w: &World, approvers: &[&TestMember]) -> String {
    let steps = approvers.iter().map(|m| step(m, "approve")).collect();
    let body = draft(&w.form, "지출 품의", steps, &[], true);
    id_of(&create(&w.app, &w.drafter, &body).await)
}

async fn act(app: &TestApp, token: &str, id: &str, action: &str, body: Value) -> TestResponse {
    app.post(&format!("/approval/documents/{id}/{action}"))
        .bearer(token)
        .json(&body)
        .send()
        .await
}

async fn approve(app: &TestApp, who: &TestMember, id: &str) -> TestResponse {
    act(app, &who.token, id, "approve", json!({})).await
}

async fn reject(app: &TestApp, who: &TestMember, id: &str) -> TestResponse {
    act(
        app,
        &who.token,
        id,
        "reject",
        json!({ "reason": "근거 부족" }),
    )
    .await
}

async fn withdraw(app: &TestApp, who: &TestMember, id: &str) -> TestResponse {
    act(app, &who.token, id, "withdraw", json!({})).await
}

async fn box_ids(app: &TestApp, who: &TestMember, name: &str) -> Vec<String> {
    let page = app
        .get(&format!("/approval/documents?box={name}&limit=200"))
        .bearer(&who.token)
        .send()
        .await
        .expect(StatusCode::OK);
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(id_of)
        .collect()
}

fn doc_id(id: &str) -> DocumentId {
    id.parse().expect("document id")
}

/// 이력 종류 목록(DB에서 직접 읽는다).
async fn event_kinds(app: &TestApp, id: &str) -> Vec<&'static str> {
    let mut conn = app.state.read().await.expect("read");
    documents::list_events(&mut conn, doc_id(id))
        .await
        .expect("events")
        .iter()
        .map(|event| event.kind.as_str())
        .collect()
}

/// 저장된 문서가 상태 기계의 불변식을 모두 만족하는지 확인하고 되살린다.
async fn restored(app: &TestApp, id: &str) -> Document {
    let mut conn = app.state.read().await.expect("read");
    let id = doc_id(id);
    let record = documents::find_document(&mut conn, id)
        .await
        .expect("find")
        .expect("document");
    let steps = documents::list_steps(&mut conn, id).await.expect("steps");
    let references = documents::list_references(&mut conn, id)
        .await
        .expect("references")
        .iter()
        .map(|r| r.member_id)
        .collect();
    Document::restore(
        record.kind,
        record.status,
        record.drafter_id,
        steps,
        references,
        record.submitted_at,
        record.completed_at,
    )
    .expect("stored document satisfies the invariants")
}

// ── 해피 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn one_step_document_goes_from_submission_to_final_approval() {
    let w = world().await;
    let body = draft(
        &w.form,
        "  지출 품의  ",
        vec![step(&w.a1, "approve")],
        &[],
        true,
    );
    let created = create(&w.app, &w.drafter, &body).await;
    let id = id_of(&created);
    assert_eq!(created["status"], "in_progress");
    assert_eq!(created["doc_no"], "2026-000001");
    assert_eq!(created["title"], "지출 품의", "제목은 앞뒤 공백을 뗀다");
    assert_eq!(created["kind"], "general");
    assert_eq!(created["form_name"], "품의서");
    assert_eq!(created["drafter_name"], "drafter 님");
    assert_eq!(created["steps"][0]["status"], "pending");
    assert_eq!(created["steps"][0]["approver_name"], "first 님");
    assert_eq!(created["my_turn"], false);
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted"]);
    assert_eq!(w.app.notification_count(w.a1.id, "approval.todo").await, 1);

    assert_eq!(w.app.document(&w.a1.token, &id).await["my_turn"], true);
    let approved = act(
        &w.app,
        &w.a1.token,
        &id,
        "approve",
        json!({ "comment": "확인했습니다" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(approved["status"], "approved");
    assert_eq!(approved["steps"][0]["status"], "approved");
    assert_eq!(approved["steps"][0]["comment"], "확인했습니다");
    assert_eq!(approved["completed_at"], approved["steps"][0]["acted_at"]);
    assert_eq!(
        event_kinds(&w.app, &id).await,
        ["submitted", "approved", "completed"]
    );
    assert_eq!(approved["events"][1]["comment"], "확인했습니다");
    assert_eq!(approved["events"][1]["actor_name"], "first 님");
    assert_eq!(
        w.app
            .notification_count(w.drafter.id, "approval.approved")
            .await,
        1
    );
    restored(&w.app, &id).await;
}

#[tokio::test]
async fn three_step_line_hands_over_in_order_and_mailboxes_follow() {
    let w = world().await;
    let steps = vec![
        step(&w.a1, "agree"),
        step(&w.a2, "approve"),
        step(&w.a3, "approve"),
    ];
    let body = draft(&w.form, "신규 장비 구매", steps, &[&w.reader], true);
    let id = id_of(&create(&w.app, &w.drafter, &body).await);
    let only = vec![id.clone()];
    let none: Vec<String> = Vec::new();

    // 상신 직후.
    assert_eq!(box_ids(&w.app, &w.a1, "todo").await, only);
    assert_eq!(box_ids(&w.app, &w.a2, "todo").await, none);
    assert_eq!(box_ids(&w.app, &w.a2, "upcoming").await, only);
    assert_eq!(box_ids(&w.app, &w.a3, "upcoming").await, only);
    assert_eq!(box_ids(&w.app, &w.a1, "upcoming").await, none);
    assert_eq!(box_ids(&w.app, &w.drafter, "drafted").await, only);
    assert_eq!(box_ids(&w.app, &w.reader, "reference").await, only);
    assert_eq!(box_ids(&w.app, &w.a1, "done").await, none);
    for name in ["todo", "upcoming", "drafted", "done", "reference"] {
        assert_eq!(box_ids(&w.app, &w.outsider, name).await, none, "{name}");
    }
    assert_eq!(w.app.notification_count(w.a1.id, "approval.todo").await, 1);
    assert_eq!(w.app.notification_count(w.a2.id, "approval.todo").await, 0);
    assert_eq!(
        w.app
            .notification_count(w.reader.id, "approval.reference")
            .await,
        1
    );
    let counts = |who: &TestMember| {
        let token = who.token.clone();
        let app = w.app.clone();
        async move {
            app.get("/approval/counts")
                .bearer(&token)
                .send()
                .await
                .expect(StatusCode::OK)
        }
    };
    assert_eq!(
        counts(&w.a1).await,
        json!({ "todo": 1, "in_progress_drafted": 0, "unread_reference": 0 })
    );
    assert_eq!(
        counts(&w.drafter).await,
        json!({ "todo": 0, "in_progress_drafted": 1, "unread_reference": 0 })
    );
    assert_eq!(
        counts(&w.reader).await,
        json!({ "todo": 0, "in_progress_drafted": 0, "unread_reference": 1 })
    );

    // 합의(1단계) 처리 → 2단계로 넘어간다.
    let after_first = approve(&w.app, &w.a1, &id).await.expect(StatusCode::OK);
    assert_eq!(after_first["status"], "in_progress");
    assert_eq!(after_first["steps"][0]["status"], "approved");
    assert_eq!(after_first["steps"][1]["status"], "pending");
    assert_eq!(after_first["steps"][2]["status"], "waiting");
    assert_eq!(box_ids(&w.app, &w.a1, "todo").await, none);
    assert_eq!(box_ids(&w.app, &w.a1, "done").await, only);
    assert_eq!(box_ids(&w.app, &w.a2, "todo").await, only);
    assert_eq!(box_ids(&w.app, &w.a2, "upcoming").await, none);
    assert_eq!(box_ids(&w.app, &w.a3, "upcoming").await, only);
    assert_eq!(w.app.notification_count(w.a2.id, "approval.todo").await, 1);
    let todo = app_page(&w.app, &w.a2, "box=todo").await;
    assert_eq!(todo["items"][0]["my_turn"], true);
    assert_eq!(todo["items"][0]["current_approver_name"], "second 님");
    assert_eq!(todo["items"][0]["drafter_name"], "drafter 님");
    assert_eq!(todo["items"][0]["form_name"], "품의서");

    approve(&w.app, &w.a2, &id).await.expect(StatusCode::OK);
    assert_eq!(
        w.app
            .notification_count(w.drafter.id, "approval.approved")
            .await,
        0,
        "마지막 단계 전에는 완료 알림이 없다"
    );
    let done = approve(&w.app, &w.a3, &id).await.expect(StatusCode::OK);
    assert_eq!(done["status"], "approved");
    assert_eq!(
        event_kinds(&w.app, &id).await,
        ["submitted", "approved", "approved", "approved", "completed"]
    );
    for approver in [&w.a1, &w.a2, &w.a3] {
        assert_eq!(box_ids(&w.app, approver, "done").await, only);
        assert_eq!(box_ids(&w.app, approver, "todo").await, none);
        assert_eq!(box_ids(&w.app, approver, "upcoming").await, none);
    }
    assert_eq!(box_ids(&w.app, &w.drafter, "drafted").await, only);
    assert_eq!(
        counts(&w.drafter).await["in_progress_drafted"],
        0,
        "끝난 문서는 진행 중 건수에서 빠진다"
    );
    assert_eq!(
        w.app
            .notification_count(w.drafter.id, "approval.approved")
            .await,
        1
    );
    restored(&w.app, &id).await;
}

async fn app_page(app: &TestApp, who: &TestMember, query: &str) -> Value {
    app.get(&format!("/approval/documents?{query}"))
        .bearer(&who.token)
        .send()
        .await
        .expect(StatusCode::OK)
}

#[tokio::test]
async fn rejection_closes_the_document_and_tells_the_drafter() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1, &w.a2]).await;
    let rejected = reject(&w.app, &w.a1, &id).await.expect(StatusCode::OK);
    assert_eq!(rejected["status"], "rejected");
    assert_eq!(rejected["steps"][0]["status"], "rejected");
    assert_eq!(rejected["steps"][0]["comment"], "근거 부족");
    assert_eq!(rejected["steps"][1]["status"], "waiting");
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted", "rejected"]);
    assert_eq!(
        w.app
            .notification_count(w.drafter.id, "approval.rejected")
            .await,
        1
    );
    assert_eq!(w.app.notification_count(w.a2.id, "approval.todo").await, 0);
    assert_eq!(box_ids(&w.app, &w.a1, "done").await, vec![id.clone()]);
    assert!(box_ids(&w.app, &w.a2, "upcoming").await.is_empty());
    restored(&w.app, &id).await;
}

// ── 잘못된 행위자 ────────────────────────────────────────────────────────

#[tokio::test]
async fn non_participant_cannot_see_or_touch_the_document() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1]).await;
    let token = &w.outsider.token;
    w.app
        .get(&format!("/approval/documents/{id}"))
        .bearer(token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    for action in ["approve", "withdraw", "submit"] {
        act(&w.app, token, &id, action, json!({}))
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    act(&w.app, token, &id, "reject", json!({ "reason": "x" }))
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted"]);
    assert_eq!(
        restored(&w.app, &id).await.status(),
        DocumentStatus::InProgress
    );
}

#[tokio::test]
async fn later_approver_cannot_act_before_their_turn() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1, &w.a2]).await;
    approve(&w.app, &w.a2, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "not_current_approver");
    reject(&w.app, &w.a2, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "not_current_approver");
    let current = w.app.document(&w.a2.token, &id).await;
    assert_eq!(current["steps"][0]["status"], "pending");
    assert_eq!(current["steps"][1]["status"], "waiting");
    assert_eq!(current["my_turn"], false);
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted"]);
}

#[tokio::test]
async fn drafter_and_reference_cannot_approve_or_reject() {
    let w = world().await;
    let body = draft(
        &w.form,
        "제목",
        vec![step(&w.a1, "approve")],
        &[&w.reader],
        true,
    );
    let id = id_of(&create(&w.app, &w.drafter, &body).await);
    for who in [&w.drafter, &w.reader] {
        approve(&w.app, who, &id)
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        reject(&w.app, who, &id)
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    // 결재자와 참조자는 회수할 수 없다.
    for who in [&w.a1, &w.reader] {
        withdraw(&w.app, who, &id)
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted"]);
}

#[tokio::test]
async fn admin_off_the_line_can_read_with_an_audit_trail_but_not_decide() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1]).await;
    let seen = w.app.document(&w.owner, &id).await;
    assert_eq!(seen["status"], "in_progress");
    act(&w.app, &w.owner, &id, "approve", json!({}))
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    act(
        &w.app,
        &w.owner,
        &id,
        "reject",
        json!({ "reason": "관리자 반려" }),
    )
    .await
    .expect_error(StatusCode::FORBIDDEN, "forbidden");
    act(&w.app, &w.owner, &id, "withdraw", json!({}))
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted"]);

    // 당사자의 열람은 감사 로그를 남기지 않는다.
    w.app.document(&w.a1.token, &id).await;
    let mut conn = w.app.state.read().await.expect("read");
    let filter = AuditFilter {
        action: Some("approval.admin_view".into()),
        ..AuditFilter::default()
    };
    let (entries, _) = audit::list(&mut conn, &filter, 50).await.expect("audit");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].actor_id, Some(w.app.owner_id));
    assert_eq!(entries[0].target_id.as_deref(), Some(id.as_str()));
}

// ── 순서 역전 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn nothing_can_be_decided_after_a_rejection() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1, &w.a2]).await;
    reject(&w.app, &w.a1, &id).await.expect(StatusCode::OK);
    approve(&w.app, &w.a1, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    approve(&w.app, &w.a2, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    withdraw(&w.app, &w.drafter, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    act(&w.app, &w.drafter.token, &id, "submit", json!({}))
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted", "rejected"]);
    assert_eq!(
        restored(&w.app, &id).await.status(),
        DocumentStatus::Rejected
    );
}

#[tokio::test]
async fn approved_document_cannot_be_rejected_or_withdrawn() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1]).await;
    approve(&w.app, &w.a1, &id).await.expect(StatusCode::OK);
    reject(&w.app, &w.a1, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    withdraw(&w.app, &w.drafter, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    assert_eq!(
        event_kinds(&w.app, &id).await,
        ["submitted", "approved", "completed"]
    );
    assert_eq!(
        w.app
            .notification_count(w.drafter.id, "approval.rejected")
            .await,
        0
    );
}

#[tokio::test]
async fn withdrawal_is_too_late_once_someone_has_acted() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1, &w.a2]).await;
    approve(&w.app, &w.a1, &id).await.expect(StatusCode::OK);
    withdraw(&w.app, &w.drafter, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "withdraw_too_late");
    // 이미 승인한 사람은 그 뒤에 반려로 바꿀 수 없다.
    reject(&w.app, &w.a1, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "not_current_approver");
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted", "approved"]);
    assert_eq!(
        w.app.document(&w.drafter.token, &id).await["status"],
        "in_progress"
    );
}

#[tokio::test]
async fn withdrawn_document_cannot_be_approved() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1]).await;
    let withdrawn = withdraw(&w.app, &w.drafter, &id)
        .await
        .expect(StatusCode::OK);
    assert_eq!(withdrawn["status"], "withdrawn");
    assert_eq!(withdrawn["steps"][0]["status"], "waiting");
    assert_eq!(
        w.app
            .notification_count(w.a1.id, "approval.withdrawn")
            .await,
        1
    );
    approve(&w.app, &w.a1, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    reject(&w.app, &w.a1, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    assert!(box_ids(&w.app, &w.a1, "todo").await.is_empty());
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted", "withdrawn"]);
    restored(&w.app, &id).await;
}

// ── 멱등성 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn repeated_approval_changes_nothing() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1, &w.a2]).await;
    let first = approve(&w.app, &w.a1, &id).await.expect(StatusCode::OK);
    w.app.advance(Duration::minutes(5));
    let again = act(
        &w.app,
        &w.a1.token,
        &id,
        "approve",
        json!({ "comment": "다시" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(again, first, "두 번째 요청은 현재 상태를 그대로 돌려준다");
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted", "approved"]);
    assert_eq!(w.app.notification_count(w.a2.id, "approval.todo").await, 1);

    // 최종 승인 뒤의 반복도 같다.
    let done = approve(&w.app, &w.a2, &id).await.expect(StatusCode::OK);
    assert_eq!(
        approve(&w.app, &w.a2, &id).await.expect(StatusCode::OK),
        done
    );
    assert_eq!(
        approve(&w.app, &w.a1, &id).await.expect(StatusCode::OK),
        done_for(&done, false)
    );
    assert_eq!(
        event_kinds(&w.app, &id).await,
        ["submitted", "approved", "approved", "completed"]
    );
    assert_eq!(
        w.app
            .notification_count(w.drafter.id, "approval.approved")
            .await,
        1
    );
}

/// 보는 사람에 따라 달라지는 필드(`my_turn`)만 맞춘 사본.
fn done_for(detail: &Value, my_turn: bool) -> Value {
    let mut copy = detail.clone();
    copy["my_turn"] = json!(my_turn);
    copy
}

#[tokio::test]
async fn repeated_rejection_keeps_the_first_reason() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1]).await;
    let first = reject(&w.app, &w.a1, &id).await.expect(StatusCode::OK);
    w.app.advance(Duration::minutes(5));
    let again = act(
        &w.app,
        &w.a1.token,
        &id,
        "reject",
        json!({ "reason": "다른 사유" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(again, first);
    assert_eq!(again["steps"][0]["comment"], "근거 부족");
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted", "rejected"]);
    assert_eq!(
        w.app
            .notification_count(w.drafter.id, "approval.rejected")
            .await,
        1
    );
}

#[tokio::test]
async fn repeated_withdrawal_and_submission_change_nothing() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1]).await;
    // 이미 상신한 문서를 다시 상신.
    let current = w.app.document(&w.drafter.token, &id).await;
    let resubmitted = act(&w.app, &w.drafter.token, &id, "submit", json!({}))
        .await
        .expect(StatusCode::OK);
    assert_eq!(resubmitted, current);
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted"]);
    assert_eq!(w.app.notification_count(w.a1.id, "approval.todo").await, 1);

    let first = withdraw(&w.app, &w.drafter, &id)
        .await
        .expect(StatusCode::OK);
    w.app.advance(Duration::minutes(5));
    let again = withdraw(&w.app, &w.drafter, &id)
        .await
        .expect(StatusCode::OK);
    assert_eq!(again, first);
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted", "withdrawn"]);
    assert_eq!(
        w.app
            .notification_count(w.a1.id, "approval.withdrawn")
            .await,
        1
    );
}

#[tokio::test]
async fn idempotency_key_replays_the_first_creation() {
    let w = world().await;
    let body = draft(&w.form, "한 번만", vec![step(&w.a1, "approve")], &[], true);
    let send = |body: Value| {
        let app = w.app.clone();
        let token = w.drafter.token.clone();
        async move {
            app.post("/approval/documents")
                .bearer(&token)
                .header("idempotency-key", "create-1")
                .json(&body)
                .send()
                .await
        }
    };
    let first = send(body.clone()).await.expect(StatusCode::CREATED);
    let replay = send(body.clone()).await.expect(StatusCode::CREATED);
    assert_eq!(replay, first);
    assert_eq!(box_ids(&w.app, &w.drafter, "drafted").await.len(), 1);
    assert_eq!(w.app.notification_count(w.a1.id, "approval.todo").await, 1);

    let different = draft(
        &w.form,
        "다른 제목",
        vec![step(&w.a1, "approve")],
        &[],
        true,
    );
    send(different)
        .await
        .expect_error(StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused");
    assert_eq!(box_ids(&w.app, &w.drafter, "drafted").await.len(), 1);

    // 같은 키라도 다른 사람의 요청은 별개다.
    let other = w
        .app
        .post("/approval/documents")
        .bearer(&w.a2.token)
        .header("idempotency-key", "create-1")
        .json(&body)
        .send()
        .await
        .expect(StatusCode::CREATED);
    assert_ne!(other["id"], first["id"]);
    assert_eq!(other["doc_no"], "2026-000002");
}

// ── 동시성 ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_approvals_by_the_same_approver_apply_once() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1, &w.a2]).await;
    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let (app, token, id) = (w.app.clone(), w.a1.token.clone(), id.clone());
            tokio::spawn(async move { act(&app, &token, &id, "approve", json!({})).await.status })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.expect("join"), StatusCode::OK);
    }
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted", "approved"]);
    assert_eq!(w.app.notification_count(w.a2.id, "approval.todo").await, 1);
    let document = restored(&w.app, &id).await;
    assert_eq!(document.acted_count(), 1);
    assert_eq!(document.current_approver(), Some(w.a2.id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approval_racing_with_withdrawal_has_exactly_one_winner() {
    let w = world().await;
    for _ in 0..8 {
        let id = submitted(&w, &[&w.a1]).await;
        let approving = {
            let (app, token, id) = (w.app.clone(), w.a1.token.clone(), id.clone());
            tokio::spawn(async move { act(&app, &token, &id, "approve", json!({})).await })
        };
        let withdrawing = {
            let (app, token, id) = (w.app.clone(), w.drafter.token.clone(), id.clone());
            tokio::spawn(async move { act(&app, &token, &id, "withdraw", json!({})).await })
        };
        let approved = approving.await.expect("join");
        let withdrawn = withdrawing.await.expect("join");
        let document = restored(&w.app, &id).await;
        match document.status() {
            DocumentStatus::Approved => {
                assert_eq!(approved.status, StatusCode::OK);
                withdrawn.expect_error(StatusCode::CONFLICT, "document_already_closed");
                assert_eq!(
                    event_kinds(&w.app, &id).await,
                    ["submitted", "approved", "completed"]
                );
            }
            DocumentStatus::Withdrawn => {
                assert_eq!(withdrawn.status, StatusCode::OK);
                approved.expect_error(StatusCode::CONFLICT, "document_already_closed");
                assert_eq!(event_kinds(&w.app, &id).await, ["submitted", "withdrawn"]);
            }
            other => panic!("unexpected final status {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_submissions_get_distinct_consecutive_numbers() {
    let w = world().await;
    let mut tasks = Vec::new();
    // 절반은 임시저장 후 상신, 절반은 기안과 동시에 상신.
    for index in 0..20 {
        let (app, token) = (w.app.clone(), w.drafter.token.clone());
        let body = draft(
            &w.form,
            &format!("문서 {index}"),
            vec![step(&w.a1, "approve")],
            &[],
            index % 2 == 0,
        );
        let pending = if index % 2 == 0 {
            None
        } else {
            Some(id_of(&create(&w.app, &w.drafter, &body).await))
        };
        tasks.push(tokio::spawn(async move {
            let response = match pending {
                Some(id) => act(&app, &token, &id, "submit", json!({})).await,
                None => {
                    app.post("/approval/documents")
                        .bearer(&token)
                        .json(&body)
                        .send()
                        .await
                }
            };
            assert!(response.status.is_success(), "{}", response.body);
            response.body["doc_no"].as_str().expect("doc_no").to_owned()
        }));
    }
    let mut numbers = BTreeSet::new();
    for task in tasks {
        numbers.insert(task.await.expect("join"));
    }
    let expected: BTreeSet<String> = (1..=20).map(|seq| format!("2026-{seq:06}")).collect();
    assert_eq!(numbers, expected);
}

// ── 부분 실패 ────────────────────────────────────────────────────────────

/// 연결된 휴가 신청이 없는 휴가 문서를 DB에 직접 만든다(연결 효과가 실패하는 상황).
async fn orphan_leave_document(w: &World) -> String {
    let now = w.app.state.now();
    let line = ApprovalLine::new(
        w.drafter.id,
        vec![LineStep {
            approver: w.a1.id,
            kind: StepKind::Approve,
        }],
        Vec::new(),
    )
    .expect("line");
    let mut document = Document::draft(DocumentKind::Leave, line);
    assert!(!document
        .submit(w.drafter.id, now)
        .expect("submit")
        .is_noop());
    let mut tx = w.app.state.db.begin_write().await.expect("begin");
    let form = documents::find_form_by_code(&mut tx, "leave")
        .await
        .expect("find")
        .expect("leave form");
    let record = DocumentRecord {
        id: DocumentId::new(),
        doc_no: Some("2026-900001".into()),
        form_id: form.id,
        kind: DocumentKind::Leave,
        drafter_id: w.drafter.id,
        drafter_department_id: None,
        title: "휴가 신청".into(),
        body: "사유".into(),
        payload: None,
        status: document.status(),
        submitted_at: document.submitted_at(),
        completed_at: None,
        created_at: now,
        version: 1,
    };
    documents::insert_document(&mut tx, &record)
        .await
        .expect("insert");
    documents::replace_line(&mut tx, record.id, document.steps(), document.references())
        .await
        .expect("line");
    tx.commit().await.expect("commit");
    record.id.to_string()
}

#[tokio::test]
async fn failing_linked_effect_rolls_back_the_whole_transition() {
    let w = world().await;
    // 권한 검사에서 걸리지 않고 연결 효과까지 가도록, 결재자를 관리자로 둔다.
    promote_to_admin(&w.app, w.a1.id).await;
    let id = orphan_leave_document(&w).await;
    let before = w.app.document(&w.a1.token, &id).await;

    approve(&w.app, &w.a1, &id)
        .await
        .expect_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    reject(&w.app, &w.a1, &id)
        .await
        .expect_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    withdraw(&w.app, &w.drafter, &id)
        .await
        .expect_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");

    let after = w.app.document(&w.a1.token, &id).await;
    assert_eq!(after, before, "문서가 전혀 바뀌지 않았다");
    assert_eq!(after["status"], "in_progress");
    assert_eq!(after["steps"][0]["status"], "pending");
    assert_eq!(after["steps"][0]["acted_at"], Value::Null);
    assert!(event_kinds(&w.app, &id).await.is_empty());
    for kind in ["approval.approved", "approval.rejected"] {
        assert_eq!(w.app.notification_count(w.drafter.id, kind).await, 0);
    }
    assert_eq!(
        w.app
            .notification_count(w.a1.id, "approval.withdrawn")
            .await,
        0
    );
    assert_eq!(box_ids(&w.app, &w.a1, "todo").await, vec![id.clone()]);
    restored(&w.app, &id).await;
}

// ── 결재선의 구성원 ──────────────────────────────────────────────────────

#[tokio::test]
async fn unknown_or_inactive_people_cannot_be_put_on_the_line() {
    let w = world().await;
    let ghost = json!({ "approver_id": MemberId::new(), "kind": "approve" });
    let send = |body: Value| {
        let (app, token) = (w.app.clone(), w.drafter.token.clone());
        async move {
            app.post("/approval/documents")
                .bearer(&token)
                .json(&body)
                .send()
                .await
        }
    };
    for submit in [false, true] {
        send(draft(&w.form, "제목", vec![ghost.clone()], &[], submit))
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    let mut with_ghost_reference = draft(&w.form, "제목", vec![step(&w.a1, "approve")], &[], true);
    with_ghost_reference["references"] = json!([MemberId::new()]);
    send(with_ghost_reference)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");

    for status in [MemberStatus::Suspended, MemberStatus::Retired] {
        w.app.set_status(w.a2.id, status).await;
        let as_approver = draft(
            &w.form,
            "제목",
            vec![step(&w.a2, "agree"), step(&w.a1, "approve")],
            &[],
            true,
        );
        send(as_approver)
            .await
            .expect_error(StatusCode::CONFLICT, "member_inactive");
        let as_reference = draft(
            &w.form,
            "제목",
            vec![step(&w.a1, "approve")],
            &[&w.a2],
            false,
        );
        send(as_reference)
            .await
            .expect_error(StatusCode::CONFLICT, "member_inactive");
    }
    assert!(box_ids(&w.app, &w.drafter, "drafted").await.is_empty());
    assert_eq!(w.app.notification_count(w.a1.id, "approval.todo").await, 0);
}

#[tokio::test]
async fn draft_cannot_be_submitted_after_an_approver_left() {
    let w = world().await;
    let body = draft(&w.form, "제목", vec![step(&w.a1, "approve")], &[], false);
    let id = id_of(&create(&w.app, &w.drafter, &body).await);
    w.app.set_status(w.a1.id, MemberStatus::Retired).await;
    act(&w.app, &w.drafter.token, &id, "submit", json!({}))
        .await
        .expect_error(StatusCode::CONFLICT, "member_inactive");
    let current = w.app.document(&w.drafter.token, &id).await;
    assert_eq!(current["status"], "draft");
    assert_eq!(current["doc_no"], Value::Null);
    assert!(event_kinds(&w.app, &id).await.is_empty());

    // 번호는 실패한 상신에 쓰이지 않았다.
    let next = submitted(&w, &[&w.a2]).await;
    assert_eq!(
        w.app.document(&w.drafter.token, &next).await["doc_no"],
        "2026-000001"
    );
}

// ── 임시저장 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn draft_is_private_to_the_drafter_until_submitted() {
    let w = world().await;
    let body = draft(
        &w.form,
        "초안",
        vec![step(&w.a1, "approve")],
        &[&w.reader],
        false,
    );
    let created = create(&w.app, &w.drafter, &body).await;
    let id = id_of(&created);
    assert_eq!(created["status"], "draft");
    assert_eq!(created["doc_no"], Value::Null);
    assert_eq!(created["submitted_at"], Value::Null);
    assert_eq!(created["steps"][0]["status"], "waiting");
    assert!(event_kinds(&w.app, &id).await.is_empty());

    // 결재선의 사람, 참조자, 관리자 누구에게도 보이지 않는다.
    for token in [&w.a1.token, &w.reader.token, &w.outsider.token, &w.owner] {
        w.app
            .get(&format!("/approval/documents/{id}"))
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    assert!(box_ids(&w.app, &w.a1, "todo").await.is_empty());
    assert!(box_ids(&w.app, &w.a1, "upcoming").await.is_empty());
    assert!(box_ids(&w.app, &w.reader, "reference").await.is_empty());
    let all = w
        .app
        .get("/admin/approval/documents")
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(all["items"], json!([]));
    assert_eq!(w.app.notification_count(w.a1.id, "approval.todo").await, 0);
    approve(&w.app, &w.a1, &id)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    withdraw(&w.app, &w.drafter, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_not_editable");

    assert_eq!(
        box_ids(&w.app, &w.drafter, "drafted").await,
        vec![id.clone()]
    );
    let drafts = app_page(&w.app, &w.drafter, "box=drafted&status=draft").await;
    assert_eq!(drafts["items"].as_array().expect("items").len(), 1);
    let in_progress = app_page(&w.app, &w.drafter, "box=drafted&status=in_progress").await;
    assert_eq!(in_progress["items"], json!([]));

    let sent = act(&w.app, &w.drafter.token, &id, "submit", json!({}))
        .await
        .expect(StatusCode::OK);
    assert_eq!(sent["status"], "in_progress");
    assert_eq!(sent["doc_no"], "2026-000001");
    assert_eq!(w.app.document(&w.a1.token, &id).await["my_turn"], true);
    assert_eq!(box_ids(&w.app, &w.reader, "reference").await, vec![id]);
}

#[tokio::test]
async fn draft_can_be_edited_with_version_check_and_deleted_by_the_drafter_only() {
    let w = world().await;
    let body = draft(&w.form, "초안", vec![step(&w.a1, "approve")], &[], false);
    let created = create(&w.app, &w.drafter, &body).await;
    let id = id_of(&created);
    let path = format!("/approval/documents/{id}");
    let edit = |version: i64| {
        json!({
            "version": version,
            "title": "고친 제목",
            "body": "고친 본문",
            "steps": [step(&w.a2, "agree"), step(&w.a3, "approve")],
            "references": [w.reader.id],
        })
    };

    // 다른 사람에게는 문서가 없다.
    w.app
        .patch(&path)
        .bearer(&w.a1.token)
        .json(&edit(1))
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    w.app
        .delete(&path)
        .bearer(&w.a1.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");

    let edited = w
        .app
        .patch(&path)
        .bearer(&w.drafter.token)
        .json(&edit(1))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(edited["title"], "고친 제목");
    assert_eq!(edited["body"], "고친 본문");
    assert_eq!(edited["version"], 2);
    assert_eq!(edited["steps"][0]["approver_id"], json!(w.a2.id));
    assert_eq!(edited["steps"][0]["kind"], "agree");
    assert_eq!(edited["steps"][1]["approver_id"], json!(w.a3.id));
    assert_eq!(edited["references"][0]["member_id"], json!(w.reader.id));

    // 예전 버전으로는 고칠 수 없다.
    w.app
        .patch(&path)
        .bearer(&w.drafter.token)
        .json(&edit(1))
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    // 잘못된 결재선은 아무것도 바꾸지 않는다.
    let mut broken = edit(2);
    broken["steps"] = json!([step(&w.drafter, "approve")]);
    w.app
        .patch(&path)
        .bearer(&w.drafter.token)
        .json(&broken)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    assert_eq!(w.app.document(&w.drafter.token, &id).await, edited);

    w.app
        .delete(&path)
        .bearer(&w.drafter.token)
        .send()
        .await
        .expect(StatusCode::OK);
    w.app
        .get(&path)
        .bearer(&w.drafter.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    w.app
        .delete(&path)
        .bearer(&w.drafter.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    assert!(box_ids(&w.app, &w.drafter, "drafted").await.is_empty());
}

#[tokio::test]
async fn submitted_document_cannot_be_edited_or_deleted() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1]).await;
    let path = format!("/approval/documents/{id}");
    let before = w.app.document(&w.drafter.token, &id).await;
    let edit = json!({
        "version": before["version"], "title": "바꿈", "body": "바꿈",
        "steps": [step(&w.a2, "approve")], "references": [],
    });
    w.app
        .patch(&path)
        .bearer(&w.drafter.token)
        .json(&edit)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "document_not_editable");
    w.app
        .delete(&path)
        .bearer(&w.drafter.token)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "document_not_editable");
    // 결재자는 문서가 보이지만 고치거나 지울 수 없다.
    w.app
        .patch(&path)
        .bearer(&w.a1.token)
        .json(&edit)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    w.app
        .delete(&path)
        .bearer(&w.a1.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(w.app.document(&w.drafter.token, &id).await, before);
}

// ── 참조 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn reference_read_time_is_recorded_once() {
    let w = world().await;
    let body = draft(
        &w.form,
        "공유",
        vec![step(&w.a1, "approve")],
        &[&w.reader],
        true,
    );
    let id = id_of(&create(&w.app, &w.drafter, &body).await);
    // 다른 당사자가 열어도 참조자의 열람으로 기록되지 않는다.
    let seen_by_drafter = w.app.document(&w.drafter.token, &id).await;
    assert_eq!(seen_by_drafter["references"][0]["read_at"], Value::Null);
    assert_eq!(seen_by_drafter["references"][0]["name"], "reader 님");

    w.app.advance(Duration::minutes(10));
    let first = w.app.document(&w.reader.token, &id).await;
    let read_at = first["references"][0]["read_at"].clone();
    assert_eq!(read_at, json!(w.app.state.now()));

    w.app.advance(Duration::minutes(10));
    let second = w.app.document(&w.reader.token, &id).await;
    assert_eq!(second["references"][0]["read_at"], read_at);
    let counts = w
        .app
        .get("/approval/counts")
        .bearer(&w.reader.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(counts["unread_reference"], 0);
    // 참조자는 열람만 한다.
    assert_eq!(first["my_turn"], false);
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted"]);
}

// ── 목록 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn cursor_paging_has_no_duplicates_or_gaps() {
    let w = world().await;
    let mut created = Vec::new();
    for _ in 0..7 {
        created.push(submitted(&w, &[&w.a1]).await);
    }
    created.reverse(); // 최신순
    for (name, who) in [("drafted", &w.drafter), ("todo", &w.a1)] {
        let mut seen = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let query = match &cursor {
                Some(cursor) => format!("box={name}&limit=3&cursor={cursor}"),
                None => format!("box={name}&limit=3"),
            };
            let page = app_page(&w.app, who, &query).await;
            let items = page["items"].as_array().expect("items");
            assert!(items.len() <= 3);
            seen.extend(items.iter().map(id_of));
            pages += 1;
            match page["next_cursor"].as_str() {
                Some(next) => cursor = Some(next.to_owned()),
                None => break,
            }
        }
        assert_eq!(pages, 3, "{name}");
        assert_eq!(seen, created, "{name}");
    }
    // limit=0은 1로, 너무 큰 값은 최대로 잘린다.
    let one = app_page(&w.app, &w.drafter, "box=drafted&limit=0").await;
    assert_eq!(one["items"].as_array().expect("items").len(), 1);
    let all = app_page(&w.app, &w.drafter, "box=drafted&limit=100000").await;
    assert_eq!(all["items"].as_array().expect("items").len(), 7);
    assert_eq!(all["next_cursor"], Value::Null);
}

#[tokio::test]
async fn admin_list_shows_every_submitted_document_with_filters() {
    let w = world().await;
    let first = submitted(&w, &[&w.a1]).await;
    let second = submitted(&w, &[&w.a1]).await;
    approve(&w.app, &w.a1, &second).await.expect(StatusCode::OK);
    let other_body = draft(
        &w.form,
        "다른 기안",
        vec![step(&w.a1, "approve")],
        &[],
        true,
    );
    let third = id_of(&create(&w.app, &w.a2, &other_body).await);
    let draft_only = draft(&w.form, "초안", vec![step(&w.a1, "approve")], &[], false);
    create(&w.app, &w.drafter, &draft_only).await;

    let list = |query: String| {
        let (app, owner) = (w.app.clone(), w.owner.clone());
        async move {
            let page = app
                .get(&format!("/admin/approval/documents{query}"))
                .bearer(&owner)
                .send()
                .await
                .expect(StatusCode::OK);
            page["items"]
                .as_array()
                .expect("items")
                .iter()
                .map(id_of)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        list(String::new()).await,
        [third.clone(), second.clone(), first.clone()]
    );
    assert_eq!(list("?status=approved".into()).await, vec![second.clone()]);
    assert_eq!(list("?status=draft".into()).await, Vec::<String>::new());
    assert_eq!(list("?kind=leave".into()).await, Vec::<String>::new());
    assert_eq!(
        list(format!("?drafter_id={}", w.a2.id)).await,
        vec![third.clone()]
    );
    assert_eq!(list("?limit=2".into()).await, [third, second]);

    for token in [&w.drafter.token, &w.a1.token] {
        w.app
            .get("/admin/approval/documents")
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
}

// ── 문서 번호 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn document_number_year_follows_company_local_time() {
    let mut w = world().await;
    // 2026-12-31 23:59 KST. 그동안 세션이 만료됐으므로 다시 로그인한다.
    w.app.set_time("2026-12-31T14:59:00Z");
    w.drafter.token = w.app.login(&w.drafter.login_id, MEMBER_PASSWORD).await;
    let last = submitted(&w, &[&w.a1]).await;
    assert_eq!(
        w.app.document(&w.drafter.token, &last).await["doc_no"],
        "2026-000001"
    );
    // UTC로는 아직 2026년이지만 회사 시간으로는 2027-01-01 00:30.
    w.app.set_time("2026-12-31T15:30:00Z");
    let first = submitted(&w, &[&w.a1]).await;
    let second = submitted(&w, &[&w.a1]).await;
    assert_eq!(
        w.app.document(&w.drafter.token, &first).await["doc_no"],
        "2027-000001"
    );
    assert_eq!(
        w.app.document(&w.drafter.token, &second).await["doc_no"],
        "2027-000002"
    );
}

// ── 결재선 추천 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn line_suggestion_walks_up_the_department_tree() {
    let app = TestApp::new().await;
    let company = app.add_department("회사", None, None).await;
    let division = app.add_department("본부", Some(company), None).await;
    let team = app.add_department("팀", Some(division), None).await;
    let part = app.add_department("파트", Some(team), None).await;
    let ceo = app.add_member("ceo", Role::Member, Some(company)).await;
    let director = app
        .add_member("director", Role::Member, Some(division))
        .await;
    let lead = app.add_member("lead", Role::Member, Some(team)).await;
    let staff = app.add_member("staff", Role::Member, Some(part)).await;
    let loner = app.add_member("loner", Role::Member, None).await;
    app.set_department_head(company, Some(ceo.id)).await;
    app.set_department_head(division, Some(director.id)).await;
    app.set_department_head(team, Some(lead.id)).await;
    // 파트장은 팀장이 겸한다(같은 사람은 한 번만 나온다).
    app.set_department_head(part, Some(lead.id)).await;

    let suggest = |who: &TestMember| {
        let token = who.token.clone();
        let app = &app;
        async move {
            let body = app
                .get("/approval/line-suggestion")
                .bearer(&token)
                .send()
                .await
                .expect(StatusCode::OK);
            body["steps"]
                .as_array()
                .expect("steps")
                .iter()
                .map(|step| {
                    assert_eq!(step["kind"], "approve");
                    step["approver_name"].as_str().expect("name").to_owned()
                })
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(suggest(&staff).await, ["lead 님", "director 님", "ceo 님"]);
    assert_eq!(
        suggest(&lead).await,
        ["director 님", "ceo 님"],
        "본인은 건너뛴다"
    );
    assert_eq!(suggest(&ceo).await, Vec::<String>::new());
    assert_eq!(suggest(&loner).await, Vec::<String>::new());

    // 재직 중이 아닌 조직장은 건너뛴다.
    app.set_status(director.id, MemberStatus::Suspended).await;
    assert_eq!(suggest(&staff).await, ["lead 님", "ceo 님"]);

    // 최대 3명.
    let group = app.add_department("그룹", None, None).await;
    let chair = app.add_member("chair", Role::Member, Some(group)).await;
    app.set_department_head(group, Some(chair.id)).await;
    app.set_status(director.id, MemberStatus::Active).await;
    let mut tx = app.state.db.begin_write().await.expect("begin");
    let mut top = pango_db::org::find_department(&mut tx, company)
        .await
        .expect("find")
        .expect("company");
    top.parent_id = Some(group);
    assert!(
        pango_db::org::update_department(&mut tx, &top, app.state.now())
            .await
            .expect("update")
    );
    tx.commit().await.expect("commit");
    assert_eq!(suggest(&staff).await, ["lead 님", "director 님", "ceo 님"]);
}

// ── 양식 관리 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn default_forms_are_seeded() {
    let w = world().await;
    let forms = w.app.approval_forms(&w.drafter.token).await;
    let find = |code: &str| {
        let form = forms
            .iter()
            .find(|form| form["code"] == code)
            .unwrap_or_else(|| panic!("form {code}"));
        (
            form["name"].as_str().expect("name").to_owned(),
            form["kind"].as_str().expect("kind").to_owned(),
        )
    };
    // 처음부터 있던 세 양식은 코드·이름·종류가 그대로다(나머지 기본 양식은 approval_forms.rs).
    assert_eq!(find("general"), ("품의서".to_owned(), "general".to_owned()));
    assert_eq!(
        find("leave"),
        ("휴가 신청서".to_owned(), "leave".to_owned())
    );
    assert_eq!(
        find("attendance_correction"),
        (
            "근태 정정 신청서".to_owned(),
            "attendance_correction".to_owned()
        )
    );
}

#[tokio::test]
async fn admin_manages_forms_and_system_forms_stay_on() {
    let w = world().await;
    let new_form = json!({
        "code": "expense", "name": "지출 결의서", "description": "비용 지출",
        "body_template": "1. 금액\n2. 사유", "sort_order": 5
    });
    for token in [&w.drafter.token, &w.a1.token] {
        w.app
            .post("/admin/approval/forms")
            .bearer(token)
            .json(&new_form)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        w.app
            .get("/admin/approval/forms")
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    let created = w
        .app
        .post("/admin/approval/forms")
        .bearer(&w.owner)
        .json(&new_form)
        .send()
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(created["kind"], "general");
    assert_eq!(created["active"], true);
    assert_eq!(created["version"], 1);
    let id = created["id"].as_str().expect("id").to_owned();
    w.app
        .post("/admin/approval/forms")
        .bearer(&w.owner)
        .json(&new_form)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "already_exists");
    for bad_code in ["", "Expense", "지출", "a b", "1abc", &"a".repeat(33)] {
        let mut body = new_form.clone();
        body["code"] = json!(bad_code);
        w.app
            .post("/admin/approval/forms")
            .bearer(&w.owner)
            .json(&body)
            .send()
            .await
            .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }

    // 새 양식으로 기안할 수 있다.
    let body = draft(&id, "지출", vec![step(&w.a1, "approve")], &[], true);
    assert_eq!(
        create(&w.app, &w.drafter, &body).await["form_name"],
        "지출 결의서"
    );

    // 끄면 목록에서 빠지고 기안할 수 없다. `code`는 보내도 바뀌지 않는다.
    let path = format!("/admin/approval/forms/{id}");
    let off = json!({
        "version": 1, "name": "지출 결의서(구)", "description": "", "body_template": "",
        "active": false, "sort_order": 9, "code": "renamed"
    });
    let updated = w
        .app
        .patch(&path)
        .bearer(&w.owner)
        .json(&off)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(updated["version"], 2);
    assert_eq!(updated["code"], "expense");
    assert_eq!(updated["name"], "지출 결의서(구)");
    assert_eq!(updated["active"], false);
    w.app
        .patch(&path)
        .bearer(&w.owner)
        .json(&off)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    w.app
        .patch(&path)
        .bearer(&w.drafter.token)
        .json(&off)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    w.app
        .post("/approval/documents")
        .bearer(&w.drafter.token)
        .json(&body)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "form_inactive");
    let visible = w.app.approval_forms(&w.drafter.token).await;
    assert!(visible.iter().all(|form| form["code"] != "expense"));
    let all = w
        .app
        .get("/admin/approval/forms")
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    let all = all.as_array().expect("forms");
    assert!(all.iter().any(|form| form["code"] == "expense"));
    assert_eq!(
        i64::try_from(all.len()).expect("count"),
        w.app.count("SELECT COUNT(*) FROM approval_forms").await,
        "꺼 둔 양식도 관리 목록에는 나온다"
    );

    // 시스템 양식은 끌 수 없다.
    let leave_form = form_id(&w.app, &w.owner, "leave").await;
    let system_off = json!({ "version": 1, "name": "휴가 신청서", "active": false });
    w.app
        .patch(&format!("/admin/approval/forms/{leave_form}"))
        .bearer(&w.owner)
        .json(&system_off)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "in_use");
    w.app
        .patch(&format!("/admin/approval/forms/{}", MemberId::new()))
        .bearer(&w.owner)
        .json(&system_off)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn draft_of_a_deactivated_form_cannot_be_submitted() {
    let w = world().await;
    let created = w
        .app
        .post("/admin/approval/forms")
        .bearer(&w.owner)
        .json(&json!({ "code": "temp_form", "name": "임시 양식" }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    let form = created["id"].as_str().expect("id");
    let body = draft(form, "초안", vec![step(&w.a1, "approve")], &[], false);
    let id = id_of(&create(&w.app, &w.drafter, &body).await);
    w.app
        .patch(&format!("/admin/approval/forms/{form}"))
        .bearer(&w.owner)
        .json(&json!({ "version": 1, "name": "임시 양식", "active": false }))
        .send()
        .await
        .expect(StatusCode::OK);
    act(&w.app, &w.drafter.token, &id, "submit", json!({}))
        .await
        .expect_error(StatusCode::CONFLICT, "form_inactive");
    assert_eq!(
        w.app.document(&w.drafter.token, &id).await["status"],
        "draft"
    );
}

// ── 기형·악의적 입력 ─────────────────────────────────────────────────────

/// (설명, 단계, 참조자, 오류 필드, 오류 사유)
type LineCase = (
    &'static str,
    Vec<Value>,
    Vec<MemberId>,
    &'static str,
    &'static str,
);

#[tokio::test]
async fn malformed_lines_are_rejected_before_anything_is_stored() {
    let w = world().await;
    let ghost = || json!({ "approver_id": MemberId::new(), "kind": "approve" });
    let cases: Vec<LineCase> = vec![
        ("단계 없음", vec![], vec![], "steps", "empty"),
        (
            "11단계",
            (0..11).map(|_| ghost()).collect(),
            vec![],
            "steps",
            "too_long",
        ),
        (
            "같은 결재자 두 번",
            vec![step(&w.a1, "agree"), step(&w.a1, "approve")],
            vec![],
            "steps",
            "duplicate",
        ),
        (
            "본인을 결재자로",
            vec![step(&w.drafter, "approve")],
            vec![],
            "steps",
            "inconsistent",
        ),
        (
            "마지막이 합의",
            vec![step(&w.a1, "approve"), step(&w.a2, "agree")],
            vec![],
            "steps",
            "inconsistent",
        ),
        (
            "참조자 중복",
            vec![step(&w.a1, "approve")],
            vec![w.reader.id, w.reader.id],
            "references",
            "duplicate",
        ),
        (
            "결재자를 참조자로",
            vec![step(&w.a1, "approve")],
            vec![w.a1.id],
            "references",
            "inconsistent",
        ),
        (
            "참조자 31명",
            vec![step(&w.a1, "approve")],
            (0..31).map(|_| MemberId::new()).collect(),
            "references",
            "too_long",
        ),
    ];
    for (name, steps, references, field, reason) in cases {
        for submit in [false, true] {
            let mut body = draft(&w.form, "제목", steps.clone(), &[], submit);
            body["references"] = json!(references);
            let response = w
                .app
                .post("/approval/documents")
                .bearer(&w.drafter.token)
                .json(&body)
                .send()
                .await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST, "{name}");
            assert_eq!(response.body["error"]["field"], field, "{name}");
            assert_eq!(response.body["error"]["reason"], reason, "{name}");
        }
    }
    assert!(box_ids(&w.app, &w.drafter, "drafted").await.is_empty());
}

#[tokio::test]
async fn malformed_text_is_rejected() {
    let w = world().await;
    let long_title = "가".repeat(201);
    let long_body = "나".repeat(20_001);
    let cases: [(&str, &str, &str); 9] = [
        ("title", "", "empty"),
        ("title", "   \t ", "empty"),
        ("title", long_title.as_str(), "too_long"),
        ("title", "줄\n바꿈", "invalid_character"),
        ("title", "널\0문자", "invalid_character"),
        ("title", "방향\u{202E}뒤집기", "invalid_character"),
        ("body", "", "empty"),
        ("body", long_body.as_str(), "too_long"),
        ("body", "벨\u{7}문자", "invalid_character"),
    ];
    for (field, value, reason) in cases {
        let mut body = draft(&w.form, "제목", vec![step(&w.a1, "approve")], &[], true);
        body[field] = json!(value);
        let response = w
            .app
            .post("/approval/documents")
            .bearer(&w.drafter.token)
            .json(&body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{field} {reason}");
        assert_eq!(response.body["error"]["field"], field);
        assert_eq!(response.body["error"]["reason"], reason);
    }
    // 경계값과 무해한 특수문자는 그대로 저장된다.
    let mut edge = draft(&w.form, "제목", vec![step(&w.a1, "approve")], &[], true);
    edge["title"] = json!("가".repeat(200));
    edge["body"] = json!("<script>alert(1)</script>'; DROP TABLE approval_documents;--");
    let stored = create(&w.app, &w.drafter, &edge).await;
    assert_eq!(stored["body"], edge["body"]);
    assert_eq!(
        stored["title"].as_str().expect("title").chars().count(),
        200
    );

    // 의견·사유.
    let id = id_of(&stored);
    let long = "다".repeat(1_001);
    act(
        &w.app,
        &w.a1.token,
        &id,
        "approve",
        json!({ "comment": long }),
    )
    .await
    .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    for reason in ["", "   ", long.as_str(), "널\0"] {
        act(
            &w.app,
            &w.a1.token,
            &id,
            "reject",
            json!({ "reason": reason }),
        )
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    act(&w.app, &w.a1.token, &id, "reject", json!({}))
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted"]);
}

#[tokio::test]
async fn malformed_requests_are_rejected() {
    let w = world().await;
    let post = |body: Value| {
        let (app, token) = (w.app.clone(), w.drafter.token.clone());
        async move {
            app.post("/approval/documents")
                .bearer(&token)
                .json(&body)
                .send()
                .await
        }
    };
    let valid = || draft(&w.form, "제목", vec![step(&w.a1, "approve")], &[], true);

    // 없는 양식, 시스템 양식.
    let mut unknown_form = valid();
    unknown_form["form_id"] = json!(MemberId::new());
    post(unknown_form)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    for code in ["leave", "attendance_correction"] {
        let mut system = valid();
        system["form_id"] = json!(form_id(&w.app, &w.owner, code).await);
        let response = post(system).await;
        assert_eq!(response.body["error"]["field"], "form_id", "{code}");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }

    // 형식이 틀린 본문.
    let mut bad_kind = valid();
    bad_kind["steps"][0]["kind"] = json!("sign");
    let mut bad_uuid = valid();
    bad_uuid["steps"][0]["approver_id"] = json!("not-a-uuid");
    let mut upper_uuid = valid();
    upper_uuid["form_id"] = json!(w.form.to_uppercase());
    let mut wrong_type = valid();
    wrong_type["submit"] = json!("yes");
    let mut missing_title = valid();
    missing_title
        .as_object_mut()
        .expect("object")
        .remove("title");
    for body in [bad_kind, bad_uuid, upper_uuid, wrong_type, missing_title] {
        post(body)
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    w.app
        .post("/approval/documents")
        .bearer(&w.drafter.token)
        .raw_body("application/json", "{\"form_id\": ")
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    w.app
        .post("/approval/documents")
        .bearer(&w.drafter.token)
        .raw_body("text/plain", valid().to_string())
        .send()
        .await
        .expect_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type");
    w.app
        .post("/approval/documents")
        .json(&valid())
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    assert!(box_ids(&w.app, &w.drafter, "drafted").await.is_empty());

    // 형식이 틀린 경로·쿼리.
    let id = id_of(&create(&w.app, &w.drafter, &valid()).await);
    for bad in ["not-a-uuid", "1", &id.to_uppercase(), "' OR 1=1 --"] {
        let path = format!("/approval/documents/{}", bad.replace(' ', "%20"));
        w.app
            .get(&path)
            .bearer(&w.drafter.token)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
        act(
            &w.app,
            &w.a1.token,
            &bad.replace(' ', "%20"),
            "approve",
            json!({}),
        )
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    w.app
        .get(&format!("/approval/documents/{}", DocumentId::new()))
        .bearer(&w.drafter.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    for query in [
        "",
        "?box=everything",
        "?box=",
        "?box=todo&status=done",
        "?box=todo&limit=many",
        "?box=todo&limit=-1",
    ] {
        w.app
            .get(&format!("/approval/documents{query}"))
            .bearer(&w.drafter.token)
            .send()
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    for cursor in ["garbage", "0", "%27%3B--", &id.to_uppercase()] {
        let response = w
            .app
            .get(&format!("/approval/documents?box=drafted&cursor={cursor}"))
            .bearer(&w.drafter.token)
            .send()
            .await;
        assert_eq!(response.body["error"]["field"], "cursor", "{cursor}");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    w.app
        .get("/admin/approval/documents?kind=memo")
        .bearer(&w.owner)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
}

// ── 관리자의 대리 반려 ────────────────────────────────────────────────────

async fn admin_reject(app: &TestApp, token: &str, id: &str) -> TestResponse {
    app.post(&format!("/admin/approval/documents/{id}/reject"))
        .bearer(token)
        .json(&json!({ "reason": "결재자 부재로 종료" }))
        .send()
        .await
}

/// 한 문서의 상태·이력과 그 문서에 딸린 알림·감사 로그의 줄 수. 거부된 요청이 아무것도
/// 바꾸지 않았는지 비교한다. 관리자의 열람(`approval.admin_view`)은 세지 않는다.
async fn footprint(app: &TestApp, id: &str) -> (Document, Vec<&'static str>, i64, i64) {
    let document = restored(app, id).await;
    let events = event_kinds(app, id).await;
    let mut conn = app.state.db.read().await.expect("read");
    let notifications: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE link = ?")
            .bind(format!("/approval/documents/{id}"))
            .fetch_one(&mut *conn)
            .await
            .expect("count");
    let audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_log WHERE target_id = ? AND action <> 'approval.admin_view'",
    )
    .bind(id)
    .fetch_one(&mut *conn)
    .await
    .expect("count");
    (document, events, notifications, audits)
}

/// [합의 a1, 결재 a2]에서 a1이 처리한 뒤 a2가 정지되면 아무도 문서를 끝낼 수 없었다.
#[tokio::test]
async fn admin_reject_ends_a_document_stranded_on_a_deactivated_approver() {
    let w = world().await;
    let steps = vec![
        step(&w.a1, "agree"),
        step(&w.a2, "approve"),
        step(&w.a3, "approve"),
    ];
    let body = draft(&w.form, "지출 품의", steps, &[&w.reader], true);
    let id = id_of(&create(&w.app, &w.drafter, &body).await);
    approve(&w.app, &w.a1, &id).await.expect(StatusCode::OK);
    w.app.set_status(w.a2.id, MemberStatus::Suspended).await;

    // 기존 길은 모두 막혀 있다.
    withdraw(&w.app, &w.drafter, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "withdraw_too_late");
    act(
        &w.app,
        &w.owner,
        &id,
        "reject",
        json!({ "reason": "관리자 반려" }),
    )
    .await
    .expect_error(StatusCode::FORBIDDEN, "forbidden");
    approve(&w.app, &w.a3, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "not_current_approver");

    w.app.advance(Duration::minutes(5));
    let closed = admin_reject(&w.app, &w.owner, &id)
        .await
        .expect(StatusCode::OK);
    assert_eq!(closed["status"], "rejected");
    assert_eq!(closed["my_turn"], false);
    let statuses: Vec<&str> = closed["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .map(|s| s["status"].as_str().expect("status"))
        .collect();
    assert_eq!(statuses, ["approved", "rejected", "waiting"]);
    assert_eq!(closed["steps"][1]["comment"], "결재자 부재로 종료");
    assert_eq!(closed["steps"][1]["acted_at"], closed["completed_at"]);
    assert_eq!(closed["steps"][2]["acted_at"], Value::Null);
    // 이력의 행위자는 건너뛴 결재자가 아니라 관리자다.
    let events = closed["events"].as_array().expect("events");
    assert_eq!(
        event_kinds(&w.app, &id).await,
        ["submitted", "approved", "rejected"]
    );
    let last = events.last().expect("event");
    assert_eq!(last["kind"], "rejected");
    assert_eq!(last["actor_id"], json!(w.app.owner_id));
    assert_eq!(last["step_order"], 2);
    assert_eq!(last["comment"], "결재자 부재로 종료");
    // 저장된 문서는 상태 기계의 불변식을 만족한다.
    let stored = restored(&w.app, &id).await;
    assert_eq!(stored.status(), DocumentStatus::Rejected);
    assert_eq!(stored.current_approver(), None);

    // 기안자에게 반려를, 건너뛴 결재자에게 할 일이 사라졌음을 알린다. 다음 결재자는 모른다.
    assert_eq!(
        w.app
            .notification_count(w.drafter.id, "approval.rejected")
            .await,
        1
    );
    assert_eq!(
        w.app
            .notification_count(w.a2.id, "approval.admin_rejected")
            .await,
        1
    );
    assert_eq!(w.app.notification_count(w.a3.id, "approval.todo").await, 0);
    let audits = w.app.audit_entries("approval.admin_reject").await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor_id, Some(w.app.owner_id));
    assert_eq!(audits[0].target_id.as_deref(), Some(id.as_str()));
    let detail = audits[0].detail.as_ref().expect("detail");
    assert_eq!(detail["step_order"], 2);
    assert_eq!(detail["pending_approver_id"], json!(w.a2.id));
    assert_eq!(detail["drafter_id"], json!(w.drafter.id));
    assert_eq!(detail["reason"], "결재자 부재로 종료");
    assert_eq!(detail["kind"], "general");

    // 결재함에서도 끝난 문서다.
    assert!(box_ids(&w.app, &w.a3, "todo").await.is_empty());
    let page = app_page(&w.app, &w.drafter, "box=drafted&status=rejected").await;
    assert_eq!(page["items"].as_array().expect("items").len(), 1);

    // 같은 요청을 다시 보내도(다른 사유여도) 아무것도 더 생기지 않는다.
    let before = footprint(&w.app, &id).await;
    w.app.advance(Duration::minutes(5));
    let again = w
        .app
        .post(&format!("/admin/approval/documents/{id}/reject"))
        .bearer(&w.owner)
        .json(&json!({ "reason": "다른 사유" }))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(again["steps"][1]["comment"], "결재자 부재로 종료");
    assert_eq!(again["completed_at"], closed["completed_at"]);
    assert_eq!(footprint(&w.app, &id).await, before);

    // 복직한 결재자의 뒤늦은 승인, 기안자의 회수는 거부된다.
    w.app.set_status(w.a2.id, MemberStatus::Active).await;
    let a2 = w.app.login(&w.a2.login_id, MEMBER_PASSWORD).await;
    act(&w.app, &a2, &id, "approve", json!({}))
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    withdraw(&w.app, &w.drafter, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");
    assert_eq!(footprint(&w.app, &id).await, before);
}

/// 결재자가 재직 중이어도, 아무도 처리하기 전이어도 쓸 수 있다.
#[tokio::test]
async fn admin_reject_works_whoever_the_pending_approver_is() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1, &w.a2]).await;
    let closed = admin_reject(&w.app, &w.owner, &id)
        .await
        .expect(StatusCode::OK);
    assert_eq!(closed["status"], "rejected");
    assert_eq!(closed["steps"][0]["status"], "rejected");
    assert_eq!(closed["steps"][1]["status"], "waiting");
    assert_eq!(event_kinds(&w.app, &id).await, ["submitted", "rejected"]);
    assert_eq!(
        w.app
            .notification_count(w.a1.id, "approval.admin_rejected")
            .await,
        1
    );
    approve(&w.app, &w.a1, &id)
        .await
        .expect_error(StatusCode::CONFLICT, "document_already_closed");

    // 관리자(admin 역할)도 쓸 수 있다. 결재선에 있는 관리자여도 마찬가지다.
    promote_to_admin(&w.app, w.a2.id).await;
    let other = submitted(&w, &[&w.a1, &w.a2]).await;
    let closed = admin_reject(&w.app, &w.a2.token, &other)
        .await
        .expect(StatusCode::OK);
    assert_eq!(closed["steps"][0]["status"], "rejected");
    let audits = w.app.audit_entries("approval.admin_reject").await;
    assert_eq!(audits.len(), 2);
    assert!(audits.iter().any(|a| a.actor_id == Some(w.a2.id)));
}

#[tokio::test]
async fn admin_reject_is_refused_for_wrong_actors_and_wrong_states() {
    let w = world().await;
    let id = submitted(&w, &[&w.a1, &w.a2]).await;
    approve(&w.app, &w.a1, &id).await.expect(StatusCode::OK);
    let before = footprint(&w.app, &id).await;

    // 관리자가 아닌 사람: 기안자, 처리한 결재자, 차례인 결재자, 뒤 결재자, 외부인.
    for who in [&w.drafter, &w.a1, &w.a2, &w.outsider] {
        admin_reject(&w.app, &who.token, &id)
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    w.app
        .post(&format!("/admin/approval/documents/{id}/reject"))
        .json(&json!({ "reason": "x" }))
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    // 사유는 반려와 같은 규칙이다.
    for body in [
        json!({ "reason": "" }),
        json!({ "reason": "  " }),
        json!({}),
    ] {
        let response = w
            .app
            .post(&format!("/admin/approval/documents/{id}/reject"))
            .bearer(&w.owner)
            .json(&body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{body}");
    }
    admin_reject(&w.app, &w.owner, &DocumentId::new().to_string())
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    assert_eq!(footprint(&w.app, &id).await, before);

    // 본인이 기안한 문서에는 관리자도, 최고 관리자도 쓸 수 없다.
    promote_to_admin(&w.app, w.drafter.id).await;
    admin_reject(&w.app, &w.drafter.token, &id)
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    let owner = TestMember {
        id: w.app.owner_id,
        login_id: "owner".into(),
        token: w.owner.clone(),
    };
    let steps = vec![step(&w.a1, "approve")];
    let own = id_of(
        &create(
            &w.app,
            &owner,
            &draft(&w.form, "대표 품의", steps, &[], true),
        )
        .await,
    );
    let own_before = footprint(&w.app, &own).await;
    admin_reject(&w.app, &w.owner, &own)
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(footprint(&w.app, &own).await, own_before);
    assert_eq!(footprint(&w.app, &id).await, before);
    // 다른 관리자는 그 문서를 끝낼 수 있다.
    admin_reject(&w.app, &w.drafter.token, &own)
        .await
        .expect(StatusCode::OK);

    // 임시저장: 남의 것은 보이지 않고(404), 본인 것은 본인 문서라 403.
    let steps = vec![step(&w.a1, "approve")];
    let draft_body = draft(&w.form, "임시", steps, &[], false);
    let others_draft = id_of(&create(&w.app, &w.outsider, &draft_body).await);
    admin_reject(&w.app, &w.owner, &others_draft)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    let own_draft = id_of(&create(&w.app, &owner, &draft_body).await);
    admin_reject(&w.app, &w.owner, &own_draft)
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(event_kinds(&w.app, &others_draft).await, Vec::<&str>::new());

    // 승인·회수로 끝난 문서.
    let approved = submitted(&w, &[&w.a1]).await;
    approve(&w.app, &w.a1, &approved)
        .await
        .expect(StatusCode::OK);
    let withdrawn = submitted(&w, &[&w.a1]).await;
    withdraw(&w.app, &w.drafter, &withdrawn)
        .await
        .expect(StatusCode::OK);
    for closed in [&approved, &withdrawn] {
        let closed_before = footprint(&w.app, closed).await;
        admin_reject(&w.app, &w.owner, closed)
            .await
            .expect_error(StatusCode::CONFLICT, "document_already_closed");
        assert_eq!(footprint(&w.app, closed).await, closed_before);
    }
    assert_eq!(w.app.audit_entries("approval.admin_reject").await.len(), 1);
}

/// 차례인 결재자의 승인과 관리자의 대리 반려가 동시에 오면 하나만 반영된다.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_reject_racing_with_the_final_approval_has_exactly_one_winner() {
    let w = world().await;
    let mut outcomes = BTreeSet::new();
    for _ in 0..12 {
        let id = submitted(&w, &[&w.a1]).await;
        let approving = {
            let (app, token, id) = (w.app.clone(), w.a1.token.clone(), id.clone());
            tokio::spawn(async move { act(&app, &token, &id, "approve", json!({})).await })
        };
        let rejecting = {
            let (app, token, id) = (w.app.clone(), w.owner.clone(), id.clone());
            tokio::spawn(async move { admin_reject(&app, &token, &id).await })
        };
        let approved = approving.await.expect("join");
        let rejected = rejecting.await.expect("join");
        let document = restored(&w.app, &id).await;
        match document.status() {
            DocumentStatus::Approved => {
                assert_eq!(approved.status, StatusCode::OK);
                rejected.expect_error(StatusCode::CONFLICT, "document_already_closed");
                assert_eq!(
                    event_kinds(&w.app, &id).await,
                    ["submitted", "approved", "completed"]
                );
            }
            DocumentStatus::Rejected => {
                assert_eq!(rejected.status, StatusCode::OK);
                approved.expect_error(StatusCode::CONFLICT, "document_already_closed");
                assert_eq!(event_kinds(&w.app, &id).await, ["submitted", "rejected"]);
            }
            other => panic!("unexpected final status {other:?}"),
        }
        outcomes.insert(document.status());
    }
    // 반영된 대리 반려마다 감사 로그가 정확히 한 줄이다.
    let mut conn = w.app.state.read().await.expect("read");
    let filter = documents::DocumentFilter {
        scope: documents::Scope::Submitted,
        status: Some(DocumentStatus::Rejected),
        kind: None,
        drafter_id: None,
        form_id: None,
        category: None,
        before: None,
    };
    let (rejected_documents, _) = documents::list_documents(&mut conn, &filter, 200)
        .await
        .expect("list");
    drop(conn);
    assert_eq!(
        w.app.audit_entries("approval.admin_reject").await.len(),
        rejected_documents.len()
    );
    assert!(!outcomes.is_empty());
}

#[tokio::test]
async fn admin_form_create_honours_idempotency_key() {
    let w = world().await;
    let send = |name: &str| {
        w.app
            .post("/admin/approval/forms")
            .bearer(&w.owner)
            .header("idempotency-key", "form-create-1")
            .json(&json!({ "code": "expense", "name": name }))
            .send()
    };
    let first = send("지출 결의서").await.expect(StatusCode::CREATED);
    let again = send("지출 결의서").await.expect(StatusCode::CREATED);
    assert_eq!(first, again, "같은 키는 첫 응답을 그대로 돌려준다");
    send("다른 양식")
        .await
        .expect_error(StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused");
    assert_eq!(
        w.app
            .count("SELECT COUNT(*) FROM approval_forms WHERE code = 'expense'")
            .await,
        1
    );
    assert_eq!(w.app.audit_entries("approval_form.create").await.len(), 1);
}
