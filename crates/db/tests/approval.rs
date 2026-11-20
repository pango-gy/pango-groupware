//! 전자결재 저장소 통합 테스트.

use std::collections::BTreeSet;

use pango_core::approval::{
    ApprovalLine, Document, DocumentKind, DocumentStatus, EventKind, FormCategory, FormOrigin,
    LineStep, Mailbox, StepKind, StepStatus,
};
use pango_core::authz::Role;
use pango_core::form_schema::FormSchema;
use pango_core::id::{ApprovalEventId, DocumentId, FormId, MemberId};
use pango_core::member::MemberStatus;
use pango_core::text::Comment;
use pango_core::time::Timestamp;
use pango_db::approval::{self, DocumentFilter, DocumentRecord, DocumentState, Event, Form, Scope};
use pango_db::members::{self, Member};
use pango_db::testing::TestDb;
use pango_db::{Conn, Db};

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis).expect("timestamp")
}

async fn add_member(db: &Db, login_id: &str) -> MemberId {
    let member = Member {
        id: MemberId::new(),
        login_id: login_id.to_owned(),
        name: format!("이름-{login_id}"),
        email: None,
        phone: None,
        department_id: None,
        position_id: None,
        role: Role::Member,
        status: MemberStatus::Active,
        hired_on: "2024-03-02".parse().expect("date"),
        retired_on: None,
        must_change_password: false,
        created_at: ts(1),
        version: 1,
    };
    let mut tx = db.begin_write().await.expect("begin");
    members::insert(&mut tx, &member, "hash", ts(1))
        .await
        .expect("member");
    tx.commit().await.expect("commit");
    member.id
}

fn form(code: &str, kind: DocumentKind) -> Form {
    Form {
        description: "설명".into(),
        body_template: "1. 목적\n2. 내용".into(),
        ..Form::plain(FormId::new(), code, &format!("양식-{code}"), kind)
    }
}

async fn add_form(db: &Db, code: &str) -> Form {
    let form = form(code, DocumentKind::General);
    let mut tx = db.begin_write().await.expect("begin");
    approval::insert_form(&mut tx, &form, ts(1))
        .await
        .expect("form");
    tx.commit().await.expect("commit");
    form
}

fn line(drafter: MemberId, approvers: &[MemberId], references: &[MemberId]) -> ApprovalLine {
    let steps = approvers
        .iter()
        .map(|approver| LineStep {
            approver: *approver,
            kind: StepKind::Approve,
        })
        .collect();
    ApprovalLine::new(drafter, steps, references.to_vec()).expect("line")
}

fn record(form: &Form, document: &Document) -> DocumentRecord {
    DocumentRecord {
        id: DocumentId::new(),
        doc_no: None,
        form_id: form.id,
        kind: document.kind(),
        drafter_id: document.drafter(),
        drafter_department_id: None,
        title: "제목".into(),
        body: "본문".into(),
        payload: None,
        status: document.status(),
        submitted_at: document.submitted_at(),
        completed_at: document.completed_at(),
        created_at: ts(10),
        version: 1,
    }
}

/// 임시저장 문서를 넣는다.
async fn add_draft(db: &Db, form: &Form, line: ApprovalLine) -> (DocumentRecord, Document) {
    let document = Document::draft(DocumentKind::General, line);
    let record = record(form, &document);
    let mut tx = db.begin_write().await.expect("begin");
    approval::insert_document(&mut tx, &record)
        .await
        .expect("document");
    approval::replace_line(&mut tx, record.id, document.steps(), document.references())
        .await
        .expect("line");
    tx.commit().await.expect("commit");
    (record, document)
}

async fn save(conn: &mut Conn, id: DocumentId, version: i64, document: &Document) -> bool {
    let doc_no = document
        .submitted_at()
        .map(|_| format!("2026-{:06}", version));
    let state = DocumentState {
        status: document.status(),
        doc_no: doc_no.as_deref(),
        submitted_at: document.submitted_at(),
        completed_at: document.completed_at(),
        steps: document.steps(),
    };
    approval::save_state(conn, id, version, &state, ts(20))
        .await
        .expect("save")
}

async fn reload(db: &Db, id: DocumentId) -> (DocumentRecord, Document) {
    let mut conn = db.read().await.expect("read");
    let record = approval::find_document(&mut conn, id)
        .await
        .expect("find")
        .expect("document");
    let steps = approval::list_steps(&mut conn, id).await.expect("steps");
    let references = approval::list_references(&mut conn, id)
        .await
        .expect("references")
        .iter()
        .map(|reference| reference.member_id)
        .collect();
    let document = Document::restore(
        record.kind,
        record.status,
        record.drafter_id,
        steps,
        references,
        record.submitted_at,
        record.completed_at,
    )
    .expect("restore");
    (record, document)
}

// ── 양식 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn form_round_trips_and_updates_with_version_check() {
    let test = TestDb::new().await;
    let created = add_form(&test, "expense").await;
    let mut tx = test.begin_write().await.expect("begin");
    assert_eq!(
        approval::find_form(&mut tx, created.id)
            .await
            .expect("find"),
        Some(created.clone())
    );
    assert_eq!(
        approval::find_form_by_code(&mut tx, "expense")
            .await
            .expect("find"),
        Some(created.clone())
    );
    assert_eq!(
        approval::find_form_by_code(&mut tx, "EXPENSE")
            .await
            .expect("find"),
        None
    );

    let edited = Form {
        name: "지출 결의서".into(),
        active: false,
        sort_order: 3,
        code: "ignored".into(),
        kind: DocumentKind::Leave,
        ..created.clone()
    };
    assert!(approval::update_form(&mut tx, &edited, ts(2))
        .await
        .expect("update"));
    assert!(
        !approval::update_form(&mut tx, &edited, ts(3))
            .await
            .expect("update"),
        "같은 버전으로 두 번 고칠 수 없다"
    );
    let stored = approval::find_form(&mut tx, created.id)
        .await
        .expect("find")
        .expect("form");
    assert_eq!(stored.name, "지출 결의서");
    assert_eq!(stored.version, 2);
    assert_eq!(stored.code, "expense", "코드는 바뀌지 않는다");
    assert_eq!(stored.kind, DocumentKind::General, "종류는 바뀌지 않는다");
    assert!(approval::list_forms(&mut tx, true)
        .await
        .expect("list")
        .is_empty());
    assert_eq!(
        approval::list_forms(&mut tx, false).await.expect("list"),
        vec![stored]
    );
}

#[tokio::test]
async fn form_code_is_unique() {
    let test = TestDb::new().await;
    add_form(&test, "expense").await;
    let mut tx = test.begin_write().await.expect("begin");
    let err = approval::insert_form(&mut tx, &form("expense", DocumentKind::General), ts(1))
        .await
        .expect_err("duplicate code");
    assert!(err.is_unique_violation("approval_forms.code"), "{err}");
}

#[tokio::test]
async fn form_schema_columns_round_trip_and_update() {
    let test = TestDb::new().await;
    let reference = add_member(&test, "finance").await;
    let schema: FormSchema = serde_json::from_value(serde_json::json!({ "fields": [
        { "key": "amount", "label": "금액", "kind": "money", "required": true },
        { "key": "items", "label": "내역", "kind": "table", "columns": [
            { "key": "place", "label": "사용처", "kind": "text", "max_len": 200 }
        ]}
    ]}))
    .expect("schema");
    let created = Form {
        category: FormCategory::Accounting,
        schema: schema.clone(),
        schema_version: 3,
        use_body: false,
        default_reference_ids: vec![reference],
        origin: FormOrigin::Catalog,
        ..form("expense", DocumentKind::General)
    };
    let mut tx = test.begin_write().await.expect("begin");
    approval::insert_form(&mut tx, &created, ts(1))
        .await
        .expect("insert");
    let stored = approval::find_form(&mut tx, created.id)
        .await
        .expect("find")
        .expect("form");
    assert_eq!(stored, created);

    // 기본값: 필드 없는 일반 양식.
    let plain = form("plain", DocumentKind::General);
    approval::insert_form(&mut tx, &plain, ts(1))
        .await
        .expect("insert");
    let stored_plain = approval::find_form(&mut tx, plain.id)
        .await
        .expect("find")
        .expect("form");
    assert_eq!(stored_plain.schema, FormSchema::default());
    assert_eq!(
        (
            stored_plain.category,
            stored_plain.use_body,
            stored_plain.origin
        ),
        (FormCategory::General, true, FormOrigin::Custom)
    );
    assert!(stored_plain.default_reference_ids.is_empty());

    let edited = Form {
        category: FormCategory::Welfare,
        schema: FormSchema::default(),
        schema_version: 4,
        use_body: true,
        default_reference_ids: Vec::new(),
        origin: FormOrigin::Custom,
        ..created.clone()
    };
    assert!(approval::update_form(&mut tx, &edited, ts(2))
        .await
        .expect("update"));
    let stored = approval::find_form(&mut tx, created.id)
        .await
        .expect("find")
        .expect("form");
    assert_eq!(
        Form {
            origin: FormOrigin::Catalog,
            version: 2,
            ..edited
        },
        stored,
        "origin은 바뀌지 않는다"
    );
}

#[tokio::test]
async fn insert_form_if_absent_never_replaces_an_existing_code() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    let first = form("trip", DocumentKind::General);
    assert!(approval::insert_form_if_absent(&mut tx, &first, ts(1))
        .await
        .expect("insert"));
    // 관리자가 고치고 꺼 둔 뒤에도 같은 코드는 다시 들어가지 않는다.
    let edited = Form {
        name: "고친 이름".into(),
        active: false,
        ..first.clone()
    };
    assert!(approval::update_form(&mut tx, &edited, ts(2))
        .await
        .expect("update"));
    let again = form("trip", DocumentKind::General);
    assert!(!approval::insert_form_if_absent(&mut tx, &again, ts(3))
        .await
        .expect("insert"));
    let all = approval::list_forms(&mut tx, false).await.expect("list");
    assert_eq!(all.len(), 1);
    assert_eq!(
        (all[0].id, all[0].name.as_str(), all[0].active),
        (first.id, "고친 이름", false)
    );
}

#[tokio::test]
async fn form_codes_with_prefix_treats_underscore_literally() {
    let test = TestDb::new().await;
    for code in ["trip", "trip_copy_1", "trip_copy_2", "tripxcopy_3", "other"] {
        add_form(&test, code).await;
    }
    let mut conn = test.read().await.expect("read");
    let mut codes = approval::form_codes_with_prefix(&mut conn, "trip_copy_")
        .await
        .expect("codes");
    codes.sort();
    assert_eq!(codes, ["trip_copy_1", "trip_copy_2"]);
}

#[tokio::test]
async fn documents_filter_by_form_and_category() {
    let test = TestDb::new().await;
    let general = add_form(&test, "general").await;
    let expense = Form {
        category: FormCategory::Accounting,
        ..form("expense", DocumentKind::General)
    };
    let mut tx = test.begin_write().await.expect("begin");
    approval::insert_form(&mut tx, &expense, ts(1))
        .await
        .expect("form");
    tx.commit().await.expect("commit");
    let (drafter, a1) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
    );
    let (plain, _) = add_draft(&test, &general, line(drafter, &[a1], &[])).await;
    let (cost, _) = add_draft(&test, &expense, line(drafter, &[a1], &[])).await;

    let mut conn = test.read().await.expect("read");
    let base = DocumentFilter {
        scope: Scope::Mailbox(drafter, Mailbox::Drafted),
        status: None,
        kind: None,
        drafter_id: None,
        form_id: None,
        category: None,
        before: None,
    };
    let mut ids = async |filter: DocumentFilter| -> Vec<DocumentId> {
        let (items, _) = approval::list_documents(&mut conn, &filter, 50)
            .await
            .expect("list");
        items.into_iter().map(|item| item.id).collect()
    };
    assert_eq!(ids(base).await, [cost.id, plain.id]);
    assert_eq!(
        ids(DocumentFilter {
            form_id: Some(expense.id),
            ..base
        })
        .await,
        [cost.id]
    );
    assert_eq!(
        ids(DocumentFilter {
            category: Some(FormCategory::General),
            ..base
        })
        .await,
        [plain.id]
    );
    assert_eq!(
        ids(DocumentFilter {
            category: Some(FormCategory::Trip),
            ..base
        })
        .await,
        []
    );
    assert_eq!(
        ids(DocumentFilter {
            form_id: Some(general.id),
            category: Some(FormCategory::Accounting),
            ..base
        })
        .await,
        []
    );
}

#[tokio::test]
async fn draft_payload_is_replaced_on_edit_and_refreshed_without_a_version_bump() {
    let test = TestDb::new().await;
    let form = add_form(&test, "general").await;
    let (drafter, a1) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
    );
    let (stored, mut document) = add_draft(&test, &form, line(drafter, &[a1], &[])).await;
    let first = serde_json::json!({ "values": { "amount": 1 } });
    let second = serde_json::json!({ "values": { "amount": 2 } });
    let mut tx = test.begin_write().await.expect("begin");
    assert!(approval::update_draft_content(
        &mut tx,
        stored.id,
        1,
        "제목",
        "",
        Some(&first),
        ts(30)
    )
    .await
    .expect("update"));
    assert!(approval::refresh_draft_payload(&mut tx, stored.id, &second)
        .await
        .expect("refresh"));
    tx.commit().await.expect("commit");
    let (found, _) = reload(&test, stored.id).await;
    assert_eq!((found.version, found.payload.as_ref()), (2, Some(&second)));
    assert_eq!(found.body, "");

    // 상신한 뒤에는 바꿀 수 없다.
    let _ = document.submit(drafter, ts(100)).expect("submit");
    let mut tx = test.begin_write().await.expect("begin");
    assert!(save(&mut tx, stored.id, 2, &document).await);
    assert!(!approval::refresh_draft_payload(&mut tx, stored.id, &first)
        .await
        .expect("refresh"));
    tx.commit().await.expect("commit");
    assert_eq!(reload(&test, stored.id).await.0.payload, Some(second));
}

// ── 문서 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn document_round_trips_with_line_and_payload() {
    let test = TestDb::new().await;
    let form = add_form(&test, "general").await;
    let (drafter, a1, a2) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
        add_member(&test, "second").await,
    );
    let reader = add_member(&test, "reader").await;
    let document = Document::draft(DocumentKind::Leave, line(drafter, &[a1, a2], &[reader]));
    let stored = DocumentRecord {
        payload: Some(serde_json::json!({ "amount_q": 8, "unit": "full_day" })),
        title: "'; DROP TABLE approval_documents;--".into(),
        ..record(&form, &document)
    };
    let mut tx = test.begin_write().await.expect("begin");
    approval::insert_document(&mut tx, &stored)
        .await
        .expect("insert");
    approval::replace_line(&mut tx, stored.id, document.steps(), document.references())
        .await
        .expect("line");
    tx.commit().await.expect("commit");

    let (found, restored) = reload(&test, stored.id).await;
    assert_eq!(found, stored);
    assert_eq!(restored, document);
    let mut conn = test.read().await.expect("read");
    assert_eq!(
        approval::find_document(&mut conn, DocumentId::new())
            .await
            .expect("find"),
        None
    );
}

#[tokio::test]
async fn saving_a_transition_requires_the_version_that_was_read() {
    let test = TestDb::new().await;
    let form = add_form(&test, "general").await;
    let (drafter, a1, a2) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
        add_member(&test, "second").await,
    );
    let (stored, mut document) = add_draft(&test, &form, line(drafter, &[a1, a2], &[])).await;

    let _ = document.submit(drafter, ts(100)).expect("submit");
    let mut tx = test.begin_write().await.expect("begin");
    assert!(save(&mut tx, stored.id, 1, &document).await);
    tx.commit().await.expect("commit");
    let (after_submit, restored) = reload(&test, stored.id).await;
    assert_eq!(after_submit.version, 2);
    assert_eq!(after_submit.doc_no.as_deref(), Some("2026-000001"));
    assert_eq!(restored, document);

    // 예전 버전으로는 저장되지 않고 단계도 바뀌지 않는다.
    let comment = Comment::parse("확인").expect("comment");
    let _ = document
        .approve(a1, ts(200), Some(comment))
        .expect("approve");
    let mut tx = test.begin_write().await.expect("begin");
    assert!(!save(&mut tx, stored.id, 1, &document).await);
    tx.commit().await.expect("commit");
    let (unchanged, restored) = reload(&test, stored.id).await;
    assert_eq!(unchanged, after_submit);
    assert_eq!(restored.acted_count(), 0);

    // 읽은 버전으로 저장하면 단계가 순서대로 넘어간다(진행 중 단계는 늘 하나).
    let mut tx = test.begin_write().await.expect("begin");
    assert!(save(&mut tx, stored.id, 2, &document).await);
    tx.commit().await.expect("commit");
    let (after_approve, restored) = reload(&test, stored.id).await;
    assert_eq!(after_approve.version, 3);
    assert_eq!(
        after_approve.doc_no.as_deref(),
        Some("2026-000002"),
        "save가 넘긴 번호를 쓴다"
    );
    assert_eq!(restored, document);
    assert_eq!(restored.steps()[0].status, StepStatus::Approved);
    assert_eq!(
        restored.steps()[0].comment.as_ref().map(Comment::as_str),
        Some("확인")
    );
    assert_eq!(restored.steps()[1].status, StepStatus::Pending);

    let _ = document.approve(a2, ts(300), None).expect("approve");
    let mut tx = test.begin_write().await.expect("begin");
    assert!(save(&mut tx, stored.id, 3, &document).await);
    tx.commit().await.expect("commit");
    let (done, restored) = reload(&test, stored.id).await;
    assert_eq!(done.status, DocumentStatus::Approved);
    assert_eq!(done.completed_at, Some(ts(300)));
    assert_eq!(restored, document);
}

#[tokio::test]
async fn doc_no_is_kept_when_a_transition_does_not_assign_one() {
    let test = TestDb::new().await;
    let form = add_form(&test, "general").await;
    let (drafter, a1) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
    );
    let (stored, mut document) = add_draft(&test, &form, line(drafter, &[a1], &[])).await;
    let _ = document.submit(drafter, ts(100)).expect("submit");
    let mut tx = test.begin_write().await.expect("begin");
    assert!(save(&mut tx, stored.id, 1, &document).await);
    let _ = document.withdraw(drafter, ts(200)).expect("withdraw");
    let state = DocumentState {
        status: document.status(),
        doc_no: None,
        submitted_at: document.submitted_at(),
        completed_at: document.completed_at(),
        steps: document.steps(),
    };
    assert!(approval::save_state(&mut tx, stored.id, 2, &state, ts(200))
        .await
        .expect("save"));
    tx.commit().await.expect("commit");
    let (withdrawn, restored) = reload(&test, stored.id).await;
    assert_eq!(withdrawn.doc_no.as_deref(), Some("2026-000001"));
    assert_eq!(restored.status(), DocumentStatus::Withdrawn);
}

#[tokio::test]
async fn database_rejects_two_pending_steps_and_duplicate_approvers() {
    let test = TestDb::new().await;
    let form = add_form(&test, "general").await;
    let (drafter, a1, a2) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
        add_member(&test, "second").await,
    );
    let (stored, mut document) = add_draft(&test, &form, line(drafter, &[a1, a2], &[])).await;
    let _ = document.submit(drafter, ts(100)).expect("submit");
    let mut tx = test.begin_write().await.expect("begin");
    assert!(save(&mut tx, stored.id, 1, &document).await);
    tx.commit().await.expect("commit");

    // 두 번째 단계까지 진행 중으로 만들려는 시도.
    let mut tx = test.begin_write().await.expect("begin");
    let forced = sqlx::query(
        "UPDATE approval_steps SET status = 'pending' WHERE document_id = ? AND step_order = 2",
    )
    .bind(stored.id.to_string())
    .execute(&mut *tx)
    .await;
    assert!(forced.is_err(), "문서당 진행 중 단계는 하나뿐이다");
    drop(tx);

    // 같은 사람을 두 단계에.
    let mut tx = test.begin_write().await.expect("begin");
    let duplicate = sqlx::query(
        "UPDATE approval_steps SET approver_id = ? WHERE document_id = ? AND step_order = 2",
    )
    .bind(a1.to_string())
    .bind(stored.id.to_string())
    .execute(&mut *tx)
    .await;
    assert!(duplicate.is_err());
    drop(tx);

    // 없는 구성원은 결재선에 넣을 수 없다(외래 키).
    let ghost = Document::draft(
        DocumentKind::General,
        line(drafter, &[MemberId::new()], &[]),
    );
    let mut tx = test.begin_write().await.expect("begin");
    let err = approval::replace_line(&mut tx, stored.id, ghost.steps(), ghost.references())
        .await
        .expect_err("unknown approver");
    assert!(err.is_foreign_key_violation(), "{err}");
    drop(tx);

    let (_, restored) = reload(&test, stored.id).await;
    assert_eq!(restored, document, "실패한 시도는 아무것도 바꾸지 않았다");
}

#[tokio::test]
async fn only_drafts_can_be_deleted_and_the_line_goes_with_them() {
    let test = TestDb::new().await;
    let form = add_form(&test, "general").await;
    let (drafter, a1, reader) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
        add_member(&test, "reader").await,
    );
    let (draft, _) = add_draft(&test, &form, line(drafter, &[a1], &[reader])).await;
    let (sent, mut document) = add_draft(&test, &form, line(drafter, &[a1], &[reader])).await;
    let _ = document.submit(drafter, ts(100)).expect("submit");

    let mut tx = test.begin_write().await.expect("begin");
    assert!(save(&mut tx, sent.id, 1, &document).await);
    assert!(!approval::delete_draft(&mut tx, sent.id)
        .await
        .expect("delete"));
    assert!(approval::delete_draft(&mut tx, draft.id)
        .await
        .expect("delete"));
    assert!(!approval::delete_draft(&mut tx, draft.id)
        .await
        .expect("delete"));
    assert_eq!(
        approval::find_document(&mut tx, draft.id)
            .await
            .expect("find"),
        None
    );
    assert!(approval::list_steps(&mut tx, draft.id)
        .await
        .expect("steps")
        .is_empty());
    assert!(approval::list_references(&mut tx, draft.id)
        .await
        .expect("references")
        .is_empty());
    assert_eq!(
        approval::list_steps(&mut tx, sent.id)
            .await
            .expect("steps")
            .len(),
        1
    );
}

#[tokio::test]
async fn replacing_the_line_swaps_steps_and_references() {
    let test = TestDb::new().await;
    let form = add_form(&test, "general").await;
    let (drafter, a1, a2, reader) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
        add_member(&test, "second").await,
        add_member(&test, "reader").await,
    );
    let (stored, mut document) = add_draft(&test, &form, line(drafter, &[a1], &[reader])).await;
    let _ = document
        .replace_line(drafter, line(drafter, &[a2, a1], &[]))
        .expect("replace");
    let mut tx = test.begin_write().await.expect("begin");
    approval::replace_line(&mut tx, stored.id, document.steps(), document.references())
        .await
        .expect("line");
    assert!(approval::update_draft_content(
        &mut tx,
        stored.id,
        1,
        "새 제목",
        "새 본문",
        None,
        ts(30)
    )
    .await
    .expect("update"));
    assert!(
        !approval::update_draft_content(&mut tx, stored.id, 1, "또", "또", None, ts(31))
            .await
            .expect("update")
    );
    tx.commit().await.expect("commit");
    let (found, restored) = reload(&test, stored.id).await;
    assert_eq!(restored, document);
    assert_eq!(
        (found.title.as_str(), found.body.as_str()),
        ("새 제목", "새 본문")
    );
    assert_eq!(found.version, 2);

    // 상신한 뒤에는 내용을 바꿀 수 없다.
    let _ = document.submit(drafter, ts(100)).expect("submit");
    let mut tx = test.begin_write().await.expect("begin");
    assert!(save(&mut tx, stored.id, 2, &document).await);
    assert!(
        !approval::update_draft_content(&mut tx, stored.id, 3, "늦음", "늦음", None, ts(40))
            .await
            .expect("update")
    );
}

// ── 참조·이력 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn reference_read_time_is_set_once() {
    let test = TestDb::new().await;
    let form = add_form(&test, "general").await;
    let (drafter, a1, reader) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
        add_member(&test, "reader").await,
    );
    let (stored, _) = add_draft(&test, &form, line(drafter, &[a1], &[reader])).await;
    let mut tx = test.begin_write().await.expect("begin");
    let mut mark = async |member: MemberId, at: Timestamp| {
        approval::mark_reference_read(&mut tx, stored.id, member, at)
            .await
            .expect("mark")
    };
    assert!(mark(reader, ts(50)).await);
    assert!(!mark(reader, ts(60)).await);
    assert!(!mark(a1, ts(60)).await, "참조자가 아니면 기록되지 않는다");
    let references = approval::list_references(&mut tx, stored.id)
        .await
        .expect("references");
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].read_at, Some(ts(50)));
}

#[tokio::test]
async fn events_keep_insertion_order_and_cannot_be_changed() {
    let test = TestDb::new().await;
    let form = add_form(&test, "general").await;
    let (drafter, a1) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
    );
    let (stored, _) = add_draft(&test, &form, line(drafter, &[a1], &[])).await;
    let event = |kind, actor, step_order, comment: Option<&str>| Event {
        id: ApprovalEventId::new(),
        document_id: stored.id,
        actor_id: actor,
        kind,
        step_order,
        comment: comment.map(str::to_owned),
        // 한 트랜잭션에서 생긴 이력은 시각이 같다.
        created_at: ts(100),
    };
    let events = [
        event(EventKind::Submitted, drafter, None, None),
        event(EventKind::Approved, a1, Some(1), Some("확인")),
        event(EventKind::Completed, a1, None, None),
    ];
    let mut tx = test.begin_write().await.expect("begin");
    for entry in &events {
        approval::insert_event(&mut tx, entry).await.expect("event");
    }
    assert_eq!(
        approval::list_events(&mut tx, stored.id)
            .await
            .expect("events"),
        events
    );
    let update = sqlx::query("UPDATE approval_events SET comment = 'tampered'")
        .execute(&mut *tx)
        .await;
    assert!(update.is_err());
    let delete = sqlx::query("DELETE FROM approval_events")
        .execute(&mut *tx)
        .await;
    assert!(delete.is_err());
    let unknown_kind = sqlx::query(
        "INSERT INTO approval_events (id, document_id, actor_id, kind, created_at)
         VALUES ('x', ?, ?, 'edited', 1)",
    )
    .bind(stored.id.to_string())
    .bind(drafter.to_string())
    .execute(&mut *tx)
    .await;
    assert!(unknown_kind.is_err());
}

// ── 문서 번호 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn document_sequence_counts_per_year_and_rolls_back() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    let mut next = Vec::new();
    for year in [2026, 2026, 2027, 2026, 2027] {
        next.push(
            approval::next_document_seq(&mut tx, year)
                .await
                .expect("seq"),
        );
    }
    assert_eq!(next, [1, 2, 1, 3, 2]);
    tx.commit().await.expect("commit");

    // 롤백된 발급은 번호를 쓰지 않는다.
    let mut tx = test.begin_write().await.expect("begin");
    assert_eq!(
        approval::next_document_seq(&mut tx, 2026)
            .await
            .expect("seq"),
        4
    );
    drop(tx);
    let mut tx = test.begin_write().await.expect("begin");
    assert_eq!(
        approval::next_document_seq(&mut tx, 2026)
            .await
            .expect("seq"),
        4
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_document_sequences_never_repeat() {
    let test = TestDb::new().await;
    let tasks: Vec<_> = (0..30)
        .map(|_| {
            let db = test.db.clone();
            tokio::spawn(async move {
                let mut tx = db.begin_write().await.expect("begin");
                let seq = approval::next_document_seq(&mut tx, 2026)
                    .await
                    .expect("seq");
                tx.commit().await.expect("commit");
                seq
            })
        })
        .collect();
    let mut seen = BTreeSet::new();
    for task in tasks {
        seen.insert(task.await.expect("join"));
    }
    assert_eq!(seen, (1..=30).collect::<BTreeSet<i64>>());
}

// ── 목록 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn mailboxes_and_counts_follow_document_state() {
    let test = TestDb::new().await;
    let form = add_form(&test, "general").await;
    let (drafter, a1, a2, reader) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
        add_member(&test, "second").await,
        add_member(&test, "reader").await,
    );
    let (draft, _) = add_draft(&test, &form, line(drafter, &[a1], &[reader])).await;
    let (sent, mut document) = add_draft(&test, &form, line(drafter, &[a1, a2], &[reader])).await;
    let _ = document.submit(drafter, ts(100)).expect("submit");
    let mut tx = test.begin_write().await.expect("begin");
    assert!(save(&mut tx, sent.id, 1, &document).await);
    tx.commit().await.expect("commit");

    let mut conn = test.read().await.expect("read");
    let list = |scope: Scope, status: Option<DocumentStatus>| DocumentFilter {
        scope,
        status,
        kind: None,
        drafter_id: None,
        form_id: None,
        category: None,
        before: None,
    };
    let mut ids = async |filter: DocumentFilter| -> Vec<DocumentId> {
        let (items, has_more) = approval::list_documents(&mut conn, &filter, 50)
            .await
            .expect("list");
        assert!(!has_more);
        items.iter().map(|item| item.id).collect()
    };
    let mailbox = |member, mailbox| list(Scope::Mailbox(member, mailbox), None);
    assert_eq!(ids(mailbox(a1, Mailbox::Todo)).await, [sent.id]);
    assert_eq!(ids(mailbox(a2, Mailbox::Todo)).await, []);
    assert_eq!(ids(mailbox(a2, Mailbox::Upcoming)).await, [sent.id]);
    assert_eq!(
        ids(mailbox(a1, Mailbox::Upcoming)).await,
        [],
        "임시저장 문서의 결재선은 문서함에 나오지 않는다"
    );
    assert_eq!(
        ids(mailbox(drafter, Mailbox::Drafted)).await,
        [sent.id, draft.id]
    );
    assert_eq!(ids(mailbox(reader, Mailbox::Reference)).await, [sent.id]);
    assert_eq!(ids(mailbox(a1, Mailbox::Done)).await, []);
    assert_eq!(ids(list(Scope::Submitted, None)).await, [sent.id]);
    assert_eq!(
        ids(list(
            Scope::Mailbox(drafter, Mailbox::Drafted),
            Some(DocumentStatus::Draft)
        ))
        .await,
        [draft.id]
    );
    assert_eq!(
        ids(DocumentFilter {
            kind: Some(DocumentKind::Leave),
            ..list(Scope::Submitted, None)
        })
        .await,
        []
    );
    assert_eq!(
        ids(DocumentFilter {
            drafter_id: Some(a1),
            ..list(Scope::Submitted, None)
        })
        .await,
        []
    );

    let (items, _) = approval::list_documents(&mut conn, &mailbox(a1, Mailbox::Todo), 50)
        .await
        .expect("list");
    assert_eq!(items[0].current_approver_id, Some(a1));
    assert_eq!(
        items[0].current_approver_name.as_deref(),
        Some("이름-first")
    );
    assert_eq!(items[0].drafter_name, "이름-drafter");
    assert_eq!(items[0].form_name, "양식-general");
    assert_eq!(items[0].doc_no.as_deref(), Some("2026-000001"));

    let counts = approval::mailbox_counts(&mut conn, a1)
        .await
        .expect("counts");
    assert_eq!(
        (
            counts.todo,
            counts.in_progress_drafted,
            counts.unread_reference
        ),
        (1, 0, 0)
    );
    let counts = approval::mailbox_counts(&mut conn, drafter)
        .await
        .expect("counts");
    assert_eq!(
        (
            counts.todo,
            counts.in_progress_drafted,
            counts.unread_reference
        ),
        (0, 1, 0)
    );
    let counts = approval::mailbox_counts(&mut conn, reader)
        .await
        .expect("counts");
    assert_eq!(
        (
            counts.todo,
            counts.in_progress_drafted,
            counts.unread_reference
        ),
        (0, 0, 1),
        "임시저장 문서의 참조는 세지 않는다"
    );
}

#[tokio::test]
async fn document_list_pages_by_id_without_overlap() {
    let test = TestDb::new().await;
    let form = add_form(&test, "general").await;
    let (drafter, a1) = (
        add_member(&test, "drafter").await,
        add_member(&test, "first").await,
    );
    let mut created = Vec::new();
    for _ in 0..5 {
        created.push(
            add_draft(&test, &form, line(drafter, &[a1], &[]))
                .await
                .0
                .id,
        );
    }
    created.sort();
    created.reverse();

    let mut conn = test.read().await.expect("read");
    let mut seen = Vec::new();
    let mut before = None;
    loop {
        let filter = DocumentFilter {
            scope: Scope::Mailbox(drafter, Mailbox::Drafted),
            status: None,
            kind: None,
            drafter_id: None,
            form_id: None,
            category: None,
            before,
        };
        let (items, has_more) = approval::list_documents(&mut conn, &filter, 2)
            .await
            .expect("list");
        seen.extend(items.iter().map(|item| item.id));
        before = items.last().map(|item| item.id);
        if !has_more {
            break;
        }
    }
    assert_eq!(seen, created);
}
