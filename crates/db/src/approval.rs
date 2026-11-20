//! 전자결재: 양식, 문서, 결재 단계, 참조자, 이력, 문서 번호.

use pango_core::approval::{
    DocumentKind, DocumentStatus, EventKind, Mailbox, Step, StepKind, StepStatus,
};
use pango_core::id::{ApprovalEventId, ApprovalStepId, DepartmentId, DocumentId, FormId, MemberId};
use pango_core::text::Comment;
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

// ───────────── 양식 ─────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub id: FormId,
    pub code: String,
    pub name: String,
    pub description: String,
    pub kind: DocumentKind,
    pub body_template: String,
    pub active: bool,
    pub sort_order: i64,
    pub version: i64,
}

#[derive(sqlx::FromRow)]
struct FormRow {
    id: String,
    code: String,
    name: String,
    description: String,
    kind: String,
    body_template: String,
    active: i64,
    sort_order: i64,
    version: i64,
}

impl TryFrom<FormRow> for Form {
    type Error = crate::DbError;

    fn try_from(row: FormRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            code: row.code,
            name: row.name,
            description: row.description,
            kind: codec::parse_with("kind", &row.kind, DocumentKind::parse)?,
            body_template: row.body_template,
            active: row.active != 0,
            sort_order: row.sort_order,
            version: row.version,
        })
    }
}

const FORM_COLUMNS: &str =
    "id, code, name, description, kind, body_template, active, sort_order, version";

/// 정렬 순서대로. `active_only`면 사용 중인 양식만.
pub async fn list_forms(conn: &mut Conn, active_only: bool) -> DbResult<Vec<Form>> {
    sqlx::query_as::<_, FormRow>(&format!(
        "SELECT {FORM_COLUMNS} FROM approval_forms
         WHERE (?1 = 0 OR active = 1) ORDER BY sort_order, name, id"
    ))
    .bind(active_only)
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Form::try_from)
    .collect()
}

pub async fn find_form(conn: &mut Conn, id: FormId) -> DbResult<Option<Form>> {
    sqlx::query_as::<_, FormRow>(&format!(
        "SELECT {FORM_COLUMNS} FROM approval_forms WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_optional(conn)
    .await?
    .map(Form::try_from)
    .transpose()
}

pub async fn find_form_by_code(conn: &mut Conn, code: &str) -> DbResult<Option<Form>> {
    sqlx::query_as::<_, FormRow>(&format!(
        "SELECT {FORM_COLUMNS} FROM approval_forms WHERE code = ?"
    ))
    .bind(code)
    .fetch_optional(conn)
    .await?
    .map(Form::try_from)
    .transpose()
}

pub async fn insert_form(conn: &mut Conn, form: &Form, now: Timestamp) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO approval_forms
            (id, code, name, description, kind, body_template, active, sort_order,
             created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(form.id.to_string())
    .bind(&form.code)
    .bind(&form.name)
    .bind(&form.description)
    .bind(form.kind.as_str())
    .bind(&form.body_template)
    .bind(form.active)
    .bind(form.sort_order)
    .bind(now.as_millis())
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

/// `code`와 `kind`는 바꾸지 않는다. `form.version`이 현재 값과 같을 때만 갱신한다.
pub async fn update_form(conn: &mut Conn, form: &Form, now: Timestamp) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE approval_forms SET
            name = ?, description = ?, body_template = ?, active = ?, sort_order = ?,
            updated_at = ?, version = version + 1
         WHERE id = ? AND version = ?",
    )
    .bind(&form.name)
    .bind(&form.description)
    .bind(&form.body_template)
    .bind(form.active)
    .bind(form.sort_order)
    .bind(now.as_millis())
    .bind(form.id.to_string())
    .bind(form.version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

// ───────────── 문서 ─────────────

/// 문서 본체(결재선 제외).
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentRecord {
    pub id: DocumentId,
    pub doc_no: Option<String>,
    pub form_id: FormId,
    pub kind: DocumentKind,
    pub drafter_id: MemberId,
    pub drafter_department_id: Option<DepartmentId>,
    pub title: String,
    pub body: String,
    pub payload: Option<serde_json::Value>,
    pub status: DocumentStatus,
    pub submitted_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub version: i64,
}

#[derive(sqlx::FromRow)]
struct DocumentRow {
    id: String,
    doc_no: Option<String>,
    form_id: String,
    kind: String,
    drafter_id: String,
    drafter_department_id: Option<String>,
    title: String,
    body: String,
    payload: Option<String>,
    status: String,
    submitted_at: Option<i64>,
    completed_at: Option<i64>,
    created_at: i64,
    version: i64,
}

impl TryFrom<DocumentRow> for DocumentRecord {
    type Error = crate::DbError;

    fn try_from(row: DocumentRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            doc_no: row.doc_no,
            form_id: codec::parse("form_id", &row.form_id)?,
            kind: codec::parse_with("kind", &row.kind, DocumentKind::parse)?,
            drafter_id: codec::parse("drafter_id", &row.drafter_id)?,
            drafter_department_id: codec::parse_opt(
                "drafter_department_id",
                row.drafter_department_id.as_deref(),
            )?,
            title: row.title,
            body: row.body,
            payload: row
                .payload
                .as_deref()
                .map(|raw| codec::json("payload", raw))
                .transpose()?,
            status: codec::parse_with("status", &row.status, DocumentStatus::parse)?,
            submitted_at: codec::timestamp_opt("submitted_at", row.submitted_at)?,
            completed_at: codec::timestamp_opt("completed_at", row.completed_at)?,
            created_at: codec::timestamp("created_at", row.created_at)?,
            version: row.version,
        })
    }
}

pub async fn find_document(conn: &mut Conn, id: DocumentId) -> DbResult<Option<DocumentRecord>> {
    sqlx::query_as::<_, DocumentRow>(
        "SELECT id, doc_no, form_id, kind, drafter_id, drafter_department_id, title, body,
                payload, status, submitted_at, completed_at, created_at, version
         FROM approval_documents WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(conn)
    .await?
    .map(DocumentRecord::try_from)
    .transpose()
}

/// 문서 본체를 넣는다. 결재선은 [`replace_line`]으로 따로 넣는다.
pub async fn insert_document(conn: &mut Conn, document: &DocumentRecord) -> DbResult<()> {
    let payload = document
        .payload
        .as_ref()
        .map(|value| codec::json_text("payload", value))
        .transpose()?;
    sqlx::query(
        "INSERT INTO approval_documents
            (id, doc_no, form_id, kind, drafter_id, drafter_department_id, title, body, payload,
             status, submitted_at, completed_at, created_at, updated_at, version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(document.id.to_string())
    .bind(&document.doc_no)
    .bind(document.form_id.to_string())
    .bind(document.kind.as_str())
    .bind(document.drafter_id.to_string())
    .bind(document.drafter_department_id.map(|id| id.to_string()))
    .bind(&document.title)
    .bind(&document.body)
    .bind(payload)
    .bind(document.status.as_str())
    .bind(document.submitted_at.map(Timestamp::as_millis))
    .bind(document.completed_at.map(Timestamp::as_millis))
    .bind(document.created_at.as_millis())
    .bind(document.created_at.as_millis())
    .bind(document.version)
    .execute(conn)
    .await?;
    Ok(())
}

/// 임시저장 문서의 제목·본문을 바꾼다. `version`이 현재 값과 같고 아직 임시저장일 때만.
pub async fn update_draft_content(
    conn: &mut Conn,
    id: DocumentId,
    version: i64,
    title: &str,
    body: &str,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE approval_documents SET title = ?, body = ?, updated_at = ?, version = version + 1
         WHERE id = ? AND version = ? AND status = 'draft'",
    )
    .bind(title)
    .bind(body)
    .bind(now.as_millis())
    .bind(id.to_string())
    .bind(version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 결재선(단계와 참조자)을 통째로 바꾼다. 새 문서에도 쓴다.
pub async fn replace_line(
    conn: &mut Conn,
    id: DocumentId,
    steps: &[Step],
    references: &[MemberId],
) -> DbResult<()> {
    let document_id = id.to_string();
    sqlx::query("DELETE FROM approval_steps WHERE document_id = ?")
        .bind(&document_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM approval_references WHERE document_id = ?")
        .bind(&document_id)
        .execute(&mut *conn)
        .await?;
    for step in steps {
        sqlx::query(
            "INSERT INTO approval_steps
                (id, document_id, step_order, approver_id, kind, status, acted_at, comment)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(ApprovalStepId::new().to_string())
        .bind(&document_id)
        .bind(i64::from(step.order))
        .bind(step.approver.to_string())
        .bind(step.kind.as_str())
        .bind(step.status.as_str())
        .bind(step.acted_at.map(Timestamp::as_millis))
        .bind(step.comment.as_ref().map(Comment::as_str))
        .execute(&mut *conn)
        .await?;
    }
    for member in references {
        sqlx::query("INSERT INTO approval_references (document_id, member_id) VALUES (?, ?)")
            .bind(&document_id)
            .bind(member.to_string())
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// 상태 전이 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentState<'a> {
    pub status: DocumentStatus,
    /// 상신할 때만 새로 정해진다. `None`이면 기존 번호를 유지한다.
    pub doc_no: Option<&'a str>,
    pub submitted_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub steps: &'a [Step],
}

/// 전이 결과(문서 상태와 단계별 상태)를 저장한다. `version`이 현재 값과 같을 때만 저장하고
/// `true`를 돌려준다. 다르면 아무것도 바꾸지 않는다.
///
/// 단계는 순서대로 갱신한다. 승인은 `pending → approved` 다음에 `waiting → pending`이
/// 오므로, 문서당 진행 중 단계가 하나라는 유일 인덱스를 중간에도 어기지 않는다.
pub async fn save_state(
    conn: &mut Conn,
    id: DocumentId,
    version: i64,
    state: &DocumentState<'_>,
    now: Timestamp,
) -> DbResult<bool> {
    let document_id = id.to_string();
    let result = sqlx::query(
        "UPDATE approval_documents SET
            status = ?, doc_no = COALESCE(?, doc_no), submitted_at = ?, completed_at = ?,
            updated_at = ?, version = version + 1
         WHERE id = ? AND version = ?",
    )
    .bind(state.status.as_str())
    .bind(state.doc_no)
    .bind(state.submitted_at.map(Timestamp::as_millis))
    .bind(state.completed_at.map(Timestamp::as_millis))
    .bind(now.as_millis())
    .bind(&document_id)
    .bind(version)
    .execute(&mut *conn)
    .await?;
    if result.rows_affected() != 1 {
        return Ok(false);
    }
    for step in state.steps {
        sqlx::query(
            "UPDATE approval_steps SET status = ?, acted_at = ?, comment = ?
             WHERE document_id = ? AND step_order = ?",
        )
        .bind(step.status.as_str())
        .bind(step.acted_at.map(Timestamp::as_millis))
        .bind(step.comment.as_ref().map(Comment::as_str))
        .bind(&document_id)
        .bind(i64::from(step.order))
        .execute(&mut *conn)
        .await?;
    }
    Ok(true)
}

/// 임시저장 문서를 지운다(단계·참조자 포함). 임시저장이 아니면 지우지 않고 `false`.
pub async fn delete_draft(conn: &mut Conn, id: DocumentId) -> DbResult<bool> {
    let result = sqlx::query("DELETE FROM approval_documents WHERE id = ? AND status = 'draft'")
        .bind(id.to_string())
        .execute(conn)
        .await?;
    Ok(result.rows_affected() == 1)
}

#[derive(sqlx::FromRow)]
struct StepRow {
    step_order: i64,
    approver_id: String,
    kind: String,
    status: String,
    acted_at: Option<i64>,
    comment: Option<String>,
}

/// 순서대로 정렬된 결재 단계.
pub async fn list_steps(conn: &mut Conn, id: DocumentId) -> DbResult<Vec<Step>> {
    let rows = sqlx::query_as::<_, StepRow>(
        "SELECT step_order, approver_id, kind, status, acted_at, comment
         FROM approval_steps WHERE document_id = ? ORDER BY step_order",
    )
    .bind(id.to_string())
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(Step {
                order: step_order(row.step_order)?,
                approver: codec::parse("approver_id", &row.approver_id)?,
                kind: codec::parse_with("kind", &row.kind, StepKind::parse)?,
                status: codec::parse_with("status", &row.status, StepStatus::parse)?,
                acted_at: codec::timestamp_opt("acted_at", row.acted_at)?,
                comment: row
                    .comment
                    .as_deref()
                    .map(|raw| codec::parse_with("comment", raw, Comment::parse))
                    .transpose()?,
            })
        })
        .collect()
}

fn step_order(raw: i64) -> DbResult<u8> {
    u8::try_from(raw).map_err(|_| {
        codec::corrupt("step_order")(pango_core::DomainError::validation(
            "step_order",
            pango_core::ValidationReason::OutOfRange,
        ))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reference {
    pub member_id: MemberId,
    pub read_at: Option<Timestamp>,
}

/// 참조자. 넣은 순서와 무관하게 ID순이다.
pub async fn list_references(conn: &mut Conn, id: DocumentId) -> DbResult<Vec<Reference>> {
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT member_id, read_at FROM approval_references
         WHERE document_id = ? ORDER BY member_id",
    )
    .bind(id.to_string())
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|(member_id, read_at)| {
            Ok(Reference {
                member_id: codec::parse("member_id", &member_id)?,
                read_at: codec::timestamp_opt("read_at", read_at)?,
            })
        })
        .collect()
}

/// 참조자의 첫 열람 시각을 남긴다. 이번에 처음 남겼으면 `true`.
pub async fn mark_reference_read(
    conn: &mut Conn,
    id: DocumentId,
    member_id: MemberId,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE approval_references SET read_at = ?
         WHERE document_id = ? AND member_id = ? AND read_at IS NULL",
    )
    .bind(now.as_millis())
    .bind(id.to_string())
    .bind(member_id.to_string())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

// ───────────── 이력 ─────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: ApprovalEventId,
    pub document_id: DocumentId,
    pub actor_id: MemberId,
    pub kind: EventKind,
    pub step_order: Option<u8>,
    pub comment: Option<String>,
    pub created_at: Timestamp,
}

pub async fn insert_event(conn: &mut Conn, event: &Event) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO approval_events
            (id, document_id, actor_id, kind, step_order, comment, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(event.id.to_string())
    .bind(event.document_id.to_string())
    .bind(event.actor_id.to_string())
    .bind(event.kind.as_str())
    .bind(event.step_order.map(i64::from))
    .bind(&event.comment)
    .bind(event.created_at.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct EventRow {
    id: String,
    document_id: String,
    actor_id: String,
    kind: String,
    step_order: Option<i64>,
    comment: Option<String>,
    created_at: i64,
}

/// 기록된 순서대로. 한 트랜잭션에서 생긴 이력은 시각이 같으므로 삽입 순서로 정렬한다.
pub async fn list_events(conn: &mut Conn, id: DocumentId) -> DbResult<Vec<Event>> {
    let rows = sqlx::query_as::<_, EventRow>(
        "SELECT id, document_id, actor_id, kind, step_order, comment, created_at
         FROM approval_events WHERE document_id = ? ORDER BY rowid",
    )
    .bind(id.to_string())
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(Event {
                id: codec::parse("id", &row.id)?,
                document_id: codec::parse("document_id", &row.document_id)?,
                actor_id: codec::parse("actor_id", &row.actor_id)?,
                kind: codec::parse_with("kind", &row.kind, EventKind::parse)?,
                step_order: row.step_order.map(step_order).transpose()?,
                comment: row.comment,
                created_at: codec::timestamp("created_at", row.created_at)?,
            })
        })
        .collect()
}

// ───────────── 문서 번호 ─────────────

/// 그 해의 다음 일련번호를 발급한다(1부터). 증가와 조회가 한 문장이라 같은 번호가
/// 두 번 나오지 않는다. 트랜잭션이 롤백되면 번호도 되돌아가므로 빈 번호가 생기지 않는다.
pub async fn next_document_seq(conn: &mut Conn, year: i32) -> DbResult<i64> {
    let (seq,): (i64,) = sqlx::query_as(
        "INSERT INTO doc_counters (year, last_seq) VALUES (?, 1)
         ON CONFLICT (year) DO UPDATE SET last_seq = last_seq + 1
         RETURNING last_seq",
    )
    .bind(i64::from(year))
    .fetch_one(conn)
    .await?;
    Ok(seq)
}

// ───────────── 목록 ─────────────

/// 목록의 범위.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// 구성원의 문서함.
    Mailbox(MemberId, Mailbox),
    /// 상신된 모든 문서(임시저장 제외). 관리자용.
    Submitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentFilter {
    pub scope: Scope,
    pub status: Option<DocumentStatus>,
    pub kind: Option<DocumentKind>,
    pub drafter_id: Option<MemberId>,
    /// 이 ID보다 오래된 문서부터(다음 페이지).
    pub before: Option<DocumentId>,
}

/// 목록의 한 줄.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentListItem {
    pub id: DocumentId,
    pub doc_no: Option<String>,
    pub title: String,
    pub form_name: String,
    pub kind: DocumentKind,
    pub status: DocumentStatus,
    pub drafter_id: MemberId,
    pub drafter_name: String,
    pub current_approver_id: Option<MemberId>,
    pub current_approver_name: Option<String>,
    pub submitted_at: Option<Timestamp>,
}

#[derive(sqlx::FromRow)]
struct ListRow {
    id: String,
    doc_no: Option<String>,
    title: String,
    form_name: String,
    kind: String,
    status: String,
    drafter_id: String,
    drafter_name: String,
    current_approver_id: Option<String>,
    current_approver_name: Option<String>,
    submitted_at: Option<i64>,
}

impl TryFrom<ListRow> for DocumentListItem {
    type Error = crate::DbError;

    fn try_from(row: ListRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            doc_no: row.doc_no,
            title: row.title,
            form_name: row.form_name,
            kind: codec::parse_with("kind", &row.kind, DocumentKind::parse)?,
            status: codec::parse_with("status", &row.status, DocumentStatus::parse)?,
            drafter_id: codec::parse("drafter_id", &row.drafter_id)?,
            drafter_name: row.drafter_name,
            current_approver_id: codec::parse_opt(
                "current_approver_id",
                row.current_approver_id.as_deref(),
            )?,
            current_approver_name: row.current_approver_name,
            submitted_at: codec::timestamp_opt("submitted_at", row.submitted_at)?,
        })
    }
}

/// 문서함의 조건. `?1`은 구성원 ID다. 한 사람은 문서에서 한 자리만 가지므로
/// 문서가 두 번 나오지 않는다.
fn scope_condition(scope: Scope) -> &'static str {
    match scope {
        Scope::Mailbox(_, Mailbox::Todo) => {
            "EXISTS (SELECT 1 FROM approval_steps s WHERE s.document_id = d.id
                     AND s.approver_id = ?1 AND s.status = 'pending')"
        }
        Scope::Mailbox(_, Mailbox::Upcoming) => {
            "d.status = 'in_progress'
             AND EXISTS (SELECT 1 FROM approval_steps s WHERE s.document_id = d.id
                         AND s.approver_id = ?1 AND s.status = 'waiting')"
        }
        Scope::Mailbox(_, Mailbox::Drafted) => "d.drafter_id = ?1",
        Scope::Mailbox(_, Mailbox::Done) => {
            "EXISTS (SELECT 1 FROM approval_steps s WHERE s.document_id = d.id
                     AND s.approver_id = ?1 AND s.status IN ('approved', 'rejected'))"
        }
        Scope::Mailbox(_, Mailbox::Reference) => {
            "d.status <> 'draft'
             AND EXISTS (SELECT 1 FROM approval_references r WHERE r.document_id = d.id
                         AND r.member_id = ?1)"
        }
        Scope::Submitted => "?1 IS NULL AND d.status <> 'draft'",
    }
}

/// 최신순 목록. `(항목, 다음 페이지 존재 여부)`.
pub async fn list_documents(
    conn: &mut Conn,
    filter: &DocumentFilter,
    limit: u32,
) -> DbResult<(Vec<DocumentListItem>, bool)> {
    let member_id = match filter.scope {
        Scope::Mailbox(member_id, _) => Some(member_id.to_string()),
        Scope::Submitted => None,
    };
    let sql = format!(
        "SELECT d.id, d.doc_no, d.title, f.name AS form_name, d.kind, d.status,
                d.drafter_id, m.name AS drafter_name,
                p.approver_id AS current_approver_id, pm.name AS current_approver_name,
                d.submitted_at
         FROM approval_documents d
         JOIN approval_forms f ON f.id = d.form_id
         JOIN members m ON m.id = d.drafter_id
         LEFT JOIN approval_steps p ON p.document_id = d.id AND p.status = 'pending'
         LEFT JOIN members pm ON pm.id = p.approver_id
         WHERE {}
           AND (?2 IS NULL OR d.status = ?2)
           AND (?3 IS NULL OR d.kind = ?3)
           AND (?4 IS NULL OR d.drafter_id = ?4)
           AND (?5 IS NULL OR d.id < ?5)
         ORDER BY d.id DESC LIMIT ?6",
        scope_condition(filter.scope)
    );
    let rows = sqlx::query_as::<_, ListRow>(&sql)
        .bind(member_id)
        .bind(filter.status.map(DocumentStatus::as_str))
        .bind(filter.kind.map(DocumentKind::as_str))
        .bind(filter.drafter_id.map(|id| id.to_string()))
        .bind(filter.before.map(|id| id.to_string()))
        .bind(codec::limit_plus_one(limit))
        .fetch_all(conn)
        .await?;
    let items = rows
        .into_iter()
        .map(DocumentListItem::try_from)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(codec::split_page(items, limit))
}

/// 메뉴 배지용 건수.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxCounts {
    pub todo: i64,
    pub in_progress_drafted: i64,
    pub unread_reference: i64,
}

pub async fn mailbox_counts(conn: &mut Conn, member_id: MemberId) -> DbResult<MailboxCounts> {
    let (todo, in_progress_drafted, unread_reference): (i64, i64, i64) = sqlx::query_as(
        "SELECT
            (SELECT COUNT(*) FROM approval_steps
              WHERE approver_id = ?1 AND status = 'pending'),
            (SELECT COUNT(*) FROM approval_documents
              WHERE drafter_id = ?1 AND status = 'in_progress'),
            (SELECT COUNT(*) FROM approval_references r
              JOIN approval_documents d ON d.id = r.document_id
              WHERE r.member_id = ?1 AND r.read_at IS NULL AND d.status <> 'draft')",
    )
    .bind(member_id.to_string())
    .fetch_one(conn)
    .await?;
    Ok(MailboxCounts {
        todo,
        in_progress_drafted,
        unread_reference,
    })
}

/// 결재가 진행 중인 문서 수(관리 대시보드).
pub async fn count_in_progress(conn: &mut Conn) -> DbResult<i64> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM approval_documents WHERE status = 'in_progress'")
            .fetch_one(conn)
            .await?;
    Ok(count)
}
