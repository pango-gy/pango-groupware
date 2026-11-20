//! 전자결재: 양식, 기안·상신, 승인·반려·회수, 결재함.
//!
//! 상태 전이는 모두 같은 길을 지난다: 트랜잭션 안에서 문서를 다시 읽어
//! [`Document::restore`]로 되살리고 → 전이 함수를 부르고 → [`Transition::Applied`]일 때만
//! 저장·이력·알림·연결 효과를 실행한다([`commit_transition_tx`]). `NoOp`이면 아무것도 쓰지 않는다.
//!
//! 쓰기는 직렬화되고 문서를 트랜잭션 안에서 읽으므로 같은 단계를 두 요청이 함께 처리할 수 없다.
//! 그래도 저장은 `version`이 읽은 값과 같을 때만 하고, 진행 중 단계가 문서당 하나라는
//! 유일 인덱스가 마지막 방어선이다.

use chrono::Datelike;
use pango_api::approval::{
    AdminDocumentQuery, ApprovalCounts, ApproveRequest, CreateDocumentRequest, CreateFormRequest,
    DocumentDetail, DocumentQuery, DocumentSummary, EventDto, FormDto, LineSuggestion,
    ReferenceDto, RejectRequest, StepDto, StepInput, SuggestedStep, UpdateDocumentRequest,
    UpdateFormRequest,
};
use pango_api::{Page, PageQuery};
use pango_core::approval::{
    self as rules, ApprovalLine, Document, DocumentEvent, DocumentKind, DocumentNumber,
    DocumentStatus, LineStep, Participant, StepKind, Transition,
};
use pango_core::authz;
use pango_core::error::{ConflictReason, ValidationReason};
use pango_core::id::{ApprovalEventId, DocumentId, FormId, MemberId};
use pango_core::org::DepartmentTree;
use pango_core::text::{self, Body, Code, Comment, LabelName, Reason, Title};
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::approval::{
    self, DocumentFilter, DocumentListItem, DocumentRecord, DocumentState, Event, Form, Scope,
};
use pango_db::members::{self, Member};
use pango_db::{org, Conn};

use super::leave;
use super::support::{
    audit, found, member_names, notify, page_of, parse_cursor, require_active_members,
    require_updated, Audit, AuthContext, Company, Idempotency, Notify,
};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 휴가 신청 양식의 코드. 휴가 기능이 문서를 만들 때 이 코드로 양식을 찾는다.
pub const LEAVE_FORM_CODE: &str = "leave";
/// 추천 결재선의 최대 단계 수.
const SUGGESTED_STEPS: usize = 3;

/// 문서가 종료되면서 연결된 기능에 알려야 하는 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkedOutcome {
    /// 최종 승인.
    Completed,
    Rejected,
    Withdrawn,
}

impl LinkedOutcome {
    fn from_event(event: &DocumentEvent) -> Option<Self> {
        match event {
            DocumentEvent::Completed => Some(Self::Completed),
            DocumentEvent::Rejected { .. } => Some(Self::Rejected),
            DocumentEvent::Withdrawn => Some(Self::Withdrawn),
            DocumentEvent::Submitted { .. } | DocumentEvent::StepApproved { .. } => None,
        }
    }
}

// ───────────── 초기 데이터 ─────────────

/// 기본 양식을 만든다. 최초 설정과 같은 트랜잭션에서 한 번만 부른다.
pub async fn seed_defaults_tx(conn: &mut Conn, now: Timestamp) -> ServiceResult<()> {
    let defaults = [
        ("general", "품의서", DocumentKind::General),
        (LEAVE_FORM_CODE, "휴가 신청서", DocumentKind::Leave),
        (
            "attendance_correction",
            "근태 정정 신청서",
            DocumentKind::AttendanceCorrection,
        ),
    ];
    for ((code, name, kind), sort_order) in defaults.into_iter().zip(0i64..) {
        let form = Form {
            id: FormId::new(),
            code: code.to_owned(),
            name: name.to_owned(),
            description: String::new(),
            kind,
            body_template: String::new(),
            active: true,
            sort_order,
            version: 1,
        };
        approval::insert_form(conn, &form, now).await?;
    }
    Ok(())
}

// ───────────── 양식 ─────────────

fn form_dto(form: Form) -> FormDto {
    FormDto {
        id: form.id,
        code: form.code,
        name: form.name,
        description: form.description,
        kind: form.kind,
        body_template: form.body_template,
        active: form.active,
        sort_order: form.sort_order,
        version: form.version,
    }
}

/// 사용 중인 양식. 시스템 양식도 보이지만 이 API로 기안할 수 있는 것은 일반 양식뿐이다.
pub async fn list_forms(state: &AppState) -> ServiceResult<Vec<FormDto>> {
    let forms = approval::list_forms(&mut *state.read().await?, true).await?;
    Ok(forms.into_iter().map(form_dto).collect())
}

pub async fn admin_list_forms(state: &AppState, ctx: &AuthContext) -> ServiceResult<Vec<FormDto>> {
    authz::require_admin(&ctx.actor)?;
    let forms = approval::list_forms(&mut *state.read().await?, false).await?;
    Ok(forms.into_iter().map(form_dto).collect())
}

/// 양식의 자유 입력란. 비워 둘 수 있다.
fn optional_text(
    raw: &str,
    field: &'static str,
    parse: impl FnOnce(&str) -> Result<String, DomainError>,
) -> ServiceResult<String> {
    let parsed = text::optional(Some(raw), parse).map_err(|error| match error {
        DomainError::Validation { reason, .. } => DomainError::Validation { field, reason },
        other => other,
    })?;
    Ok(parsed.unwrap_or_default())
}

async fn audit_form(
    conn: &mut Conn,
    now: Timestamp,
    ctx: &AuthContext,
    action: &'static str,
    form: &Form,
) -> ServiceResult<()> {
    audit(
        conn,
        now,
        Audit {
            action,
            target_type: "approval_form",
            target_id: Some(form.id.to_string()),
            detail: Some(serde_json::json!({
                "code": form.code, "name": form.name, "active": form.active
            })),
            ctx: Some(ctx),
        },
    )
    .await
}

/// 일반 양식을 만든다. 시스템 양식(휴가·근태 정정)은 연결된 기능이 하나씩만 쓰므로 만들 수 없다.
pub async fn admin_create_form(
    state: &AppState,
    ctx: &AuthContext,
    request: &CreateFormRequest,
) -> ServiceResult<FormDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let form = Form {
        id: FormId::new(),
        code: Code::parse(&request.code)?.into_string(),
        name: LabelName::parse(&request.name)?.into_string(),
        description: optional_text(&request.description, "description", |raw| {
            Comment::parse(raw).map(Comment::into_string)
        })?,
        kind: DocumentKind::General,
        body_template: optional_text(&request.body_template, "body_template", |raw| {
            Body::parse(raw).map(Body::into_string)
        })?,
        active: true,
        sort_order: request.sort_order,
        version: 1,
    };
    state
        .write(async |conn: &mut Conn| {
            if approval::find_form_by_code(conn, &form.code)
                .await?
                .is_some()
            {
                return Err(DomainError::Conflict(ConflictReason::AlreadyExists).into());
            }
            approval::insert_form(conn, &form, now).await?;
            audit_form(conn, now, ctx, "approval_form.create", &form).await
        })
        .await?;
    Ok(form_dto(form))
}

/// 양식 수정. `code`와 종류는 바꿀 수 없고, 시스템 양식은 끌 수 없다(`InUse`).
pub async fn admin_update_form(
    state: &AppState,
    ctx: &AuthContext,
    id: FormId,
    request: &UpdateFormRequest,
) -> ServiceResult<FormDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let name = LabelName::parse(&request.name)?.into_string();
    let description = optional_text(&request.description, "description", |raw| {
        Comment::parse(raw).map(Comment::into_string)
    })?;
    let body_template = optional_text(&request.body_template, "body_template", |raw| {
        Body::parse(raw).map(Body::into_string)
    })?;
    state
        .write(async |conn: &mut Conn| {
            let current = found(approval::find_form(conn, id).await?)?;
            if current.kind != DocumentKind::General && !request.active {
                return Err(DomainError::Conflict(ConflictReason::InUse).into());
            }
            let form = Form {
                name,
                description,
                body_template,
                active: request.active,
                sort_order: request.sort_order,
                version: request.version,
                ..current
            };
            require_updated(approval::update_form(conn, &form, now).await?)?;
            audit_form(conn, now, ctx, "approval_form.update", &form).await?;
            Ok(form_dto(Form {
                version: form.version + 1,
                ..form
            }))
        })
        .await
}

// ───────────── 문서 읽기 ─────────────

/// 저장된 문서와, 그것으로 되살린 상태 기계.
struct Loaded {
    record: DocumentRecord,
    document: Document,
}

/// 문서를 읽어 불변식을 확인한다. 저장된 값이 손상돼 있으면 어떤 동작도 하지 않고 내부 오류다.
async fn load(conn: &mut Conn, id: DocumentId) -> ServiceResult<Loaded> {
    let record = found(approval::find_document(conn, id).await?)?;
    let steps = approval::list_steps(conn, id).await?;
    let references = approval::list_references(conn, id)
        .await?
        .into_iter()
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
    .map_err(ServiceError::internal)?;
    Ok(Loaded { record, document })
}

fn document_link(id: DocumentId) -> String {
    format!("/approval/documents/{id}")
}

async fn detail_tx(
    conn: &mut Conn,
    viewer: MemberId,
    id: DocumentId,
) -> ServiceResult<DocumentDetail> {
    let Loaded { record, document } = load(conn, id).await?;
    let references = approval::list_references(conn, id).await?;
    let events = approval::list_events(conn, id).await?;
    let form = found(approval::find_form(conn, record.form_id).await?)?;
    let department = match record.drafter_department_id {
        Some(department_id) => org::find_department(conn, department_id).await?,
        None => None,
    };
    let people = document
        .steps()
        .iter()
        .map(|step| step.approver)
        .chain(references.iter().map(|reference| reference.member_id))
        .chain(events.iter().map(|event| event.actor_id))
        .chain([record.drafter_id]);
    let names = member_names(conn, people).await?;
    let name_of = |member: MemberId| names.get(&member).cloned().unwrap_or_default();

    Ok(DocumentDetail {
        id: record.id,
        doc_no: record.doc_no,
        form_id: record.form_id,
        form_name: form.name,
        kind: record.kind,
        status: record.status,
        drafter_id: record.drafter_id,
        drafter_name: name_of(record.drafter_id),
        drafter_department_name: department.map(|d| d.name),
        title: record.title,
        body: record.body,
        payload: record.payload,
        steps: document
            .steps()
            .iter()
            .map(|step| StepDto {
                order: step.order,
                approver_id: step.approver,
                approver_name: name_of(step.approver),
                kind: step.kind,
                status: step.status,
                acted_at: step.acted_at,
                comment: step.comment.as_ref().map(|c| c.as_str().to_owned()),
            })
            .collect(),
        references: references
            .iter()
            .map(|reference| ReferenceDto {
                member_id: reference.member_id,
                name: name_of(reference.member_id),
                read_at: reference.read_at,
            })
            .collect(),
        events: events
            .into_iter()
            .map(|event| EventDto {
                kind: event.kind,
                actor_id: event.actor_id,
                actor_name: name_of(event.actor_id),
                step_order: event.step_order,
                comment: event.comment,
                created_at: event.created_at,
            })
            .collect(),
        submitted_at: record.submitted_at,
        completed_at: record.completed_at,
        created_at: record.created_at,
        version: record.version,
        my_turn: document.current_approver() == Some(viewer),
    })
}

/// 문서 상세. 당사자와 관리자만 볼 수 있고 그 밖에는 `NotFound`다.
///
/// 열람이 남기는 기록 두 가지를 함께 처리한다:
/// - 참조자가 처음 열면 열람 시각을 남긴다.
/// - 당사자가 아닌 관리자가 열면 감사 로그(`approval.admin_view`)를 남긴다.
pub async fn get_document(
    state: &AppState,
    ctx: &AuthContext,
    id: DocumentId,
) -> ServiceResult<DocumentDetail> {
    let now = state.now();
    let mut conn = state.read().await?;
    let loaded = load(&mut conn, id).await?;
    rules::view(&ctx.actor, &loaded.document)?;
    let role = loaded.document.participant_role(ctx.member_id());
    drop(conn);

    match role {
        Some(Participant::Reference) => {
            state
                .write(async |conn: &mut Conn| {
                    approval::mark_reference_read(conn, id, ctx.member_id(), now).await?;
                    Ok(())
                })
                .await?;
        }
        // `view`를 통과했는데 맡은 자리가 없으면 관리자 자격으로 연 것이다.
        None => {
            state
                .write(async |conn: &mut Conn| {
                    audit(
                        conn,
                        now,
                        Audit {
                            action: "approval.admin_view",
                            target_type: "approval_document",
                            target_id: Some(id.to_string()),
                            detail: None,
                            ctx: Some(ctx),
                        },
                    )
                    .await
                })
                .await?;
        }
        Some(Participant::Drafter | Participant::Approver { .. }) => {}
    }
    detail_tx(&mut *state.read().await?, ctx.member_id(), id).await
}

fn summary(item: DocumentListItem, viewer: MemberId) -> DocumentSummary {
    DocumentSummary {
        my_turn: item.current_approver_id == Some(viewer),
        id: item.id,
        doc_no: item.doc_no,
        title: item.title,
        form_name: item.form_name,
        kind: item.kind,
        status: item.status,
        drafter_id: item.drafter_id,
        drafter_name: item.drafter_name,
        current_approver_id: item.current_approver_id,
        current_approver_name: item.current_approver_name,
        submitted_at: item.submitted_at,
    }
}

async fn list_page(
    state: &AppState,
    viewer: MemberId,
    filter: DocumentFilter,
    page: &PageQuery,
) -> ServiceResult<Page<DocumentSummary>> {
    let filter = DocumentFilter {
        before: parse_cursor(page)?,
        ..filter
    };
    let (items, has_more) =
        approval::list_documents(&mut *state.read().await?, &filter, page.limit()).await?;
    let items = items
        .into_iter()
        .map(|item| summary(item, viewer))
        .collect();
    Ok(page_of(items, has_more, |item: &DocumentSummary| {
        item.id.to_string()
    }))
}

/// 내 문서함. 범위가 본인으로 고정되므로 따로 권한을 확인하지 않는다.
pub async fn list_documents(
    state: &AppState,
    ctx: &AuthContext,
    query: &DocumentQuery,
) -> ServiceResult<Page<DocumentSummary>> {
    let filter = DocumentFilter {
        scope: Scope::Mailbox(ctx.member_id(), query.mailbox),
        status: query.status,
        kind: None,
        drafter_id: None,
        before: None,
    };
    list_page(state, ctx.member_id(), filter, &query.page()).await
}

/// 상신된 모든 문서(관리자). 임시저장 문서는 관리자에게도 보이지 않는다.
pub async fn admin_list_documents(
    state: &AppState,
    ctx: &AuthContext,
    query: &AdminDocumentQuery,
) -> ServiceResult<Page<DocumentSummary>> {
    authz::require_admin(&ctx.actor)?;
    let filter = DocumentFilter {
        scope: Scope::Submitted,
        status: query.status,
        kind: query.kind,
        drafter_id: query.drafter_id,
        before: None,
    };
    list_page(state, ctx.member_id(), filter, &query.page()).await
}

pub async fn counts(state: &AppState, ctx: &AuthContext) -> ServiceResult<ApprovalCounts> {
    let counts = approval::mailbox_counts(&mut *state.read().await?, ctx.member_id()).await?;
    let clamp = |count: i64| u32::try_from(count).unwrap_or(u32::MAX);
    Ok(ApprovalCounts {
        todo: clamp(counts.todo),
        in_progress_drafted: clamp(counts.in_progress_drafted),
        unread_reference: clamp(counts.unread_reference),
    })
}

// ───────────── 결재선 ─────────────

/// 추천 결재자: 소속 부서에서 위로 올라가며 만나는 조직장(가까운 순).
/// 본인과 재직 중이 아닌 사람은 건너뛰고, 같은 사람은 한 번만, 최대 [`SUGGESTED_STEPS`]명.
pub async fn suggested_approvers_tx(
    conn: &mut Conn,
    member: &Member,
) -> ServiceResult<Vec<Member>> {
    let Some(department_id) = member.department_id else {
        return Ok(Vec::new());
    };
    let departments = org::list_departments(conn).await?;
    let tree = DepartmentTree::new(departments.iter().map(|d| d.node()))
        .map_err(ServiceError::internal)?;
    let mut heads: Vec<MemberId> = Vec::new();
    for ancestor in tree.ancestors_inclusive(department_id) {
        let head = departments
            .iter()
            .find(|department| department.id == ancestor)
            .and_then(|department| department.head_member_id);
        if let Some(head) = head.filter(|head| *head != member.id && !heads.contains(head)) {
            heads.push(head);
        }
    }
    let found = members::find_many(conn, &heads).await?;
    Ok(heads
        .iter()
        .filter_map(|head| found.iter().find(|m| m.id == *head))
        .filter(|head| head.status.require_active().is_ok())
        .take(SUGGESTED_STEPS)
        .cloned()
        .collect())
}

pub async fn line_suggestion(state: &AppState, ctx: &AuthContext) -> ServiceResult<LineSuggestion> {
    let approvers = suggested_approvers_tx(&mut *state.read().await?, &ctx.member).await?;
    Ok(LineSuggestion {
        steps: approvers
            .into_iter()
            .map(|approver| SuggestedStep {
                approver_id: approver.id,
                approver_name: approver.name,
                kind: StepKind::Approve,
            })
            .collect(),
    })
}

fn line_from_input(
    drafter: MemberId,
    steps: &[StepInput],
    references: &[MemberId],
) -> Result<ApprovalLine, DomainError> {
    let steps = steps
        .iter()
        .map(|step| LineStep {
            approver: step.approver_id,
            kind: step.kind,
        })
        .collect();
    ApprovalLine::new(drafter, steps, references.to_vec())
}

/// 입력받은 결재선을 검증한다: 모양([`ApprovalLine::new`]) 다음에 구성원의 존재와 재직 여부.
/// 모양을 먼저 보므로 구성원 조회는 최대 40명으로 묶인다.
pub async fn build_line_tx(
    conn: &mut Conn,
    drafter: MemberId,
    steps: &[StepInput],
    references: &[MemberId],
) -> ServiceResult<ApprovalLine> {
    let line = line_from_input(drafter, steps, references)?;
    let approvers = line.steps().iter().map(|step| step.approver);
    require_line_active_tx(conn, approvers, line.references()).await?;
    Ok(line)
}

async fn require_line_active_tx(
    conn: &mut Conn,
    approvers: impl Iterator<Item = MemberId>,
    references: &[MemberId],
) -> ServiceResult<()> {
    let people: Vec<MemberId> = approvers.chain(references.iter().copied()).collect();
    require_active_members(conn, &people).await
}

// ───────────── 전이 저장 ─────────────

/// 적용된 전이를 저장하고, 이력·알림·연결 효과까지 같은 트랜잭션에서 실행한다.
///
/// `loaded.record`는 전이 전에 읽은 값(특히 `version`), `loaded.document`는 전이 후 상태다.
/// `actor_id`는 이력에 남길 실제 요청자다.
async fn commit_transition_tx(
    conn: &mut Conn,
    actor_id: MemberId,
    loaded: &Loaded,
    events: &[DocumentEvent],
    now: Timestamp,
) -> ServiceResult<()> {
    let Loaded { record, document } = loaded;
    let submitted = events
        .iter()
        .any(|event| matches!(event, DocumentEvent::Submitted { .. }));
    let doc_no = if submitted {
        Some(prepare_submission_tx(conn, record, document, now).await?)
    } else {
        None
    };

    let state = DocumentState {
        status: document.status(),
        doc_no: doc_no.as_deref(),
        submitted_at: document.submitted_at(),
        completed_at: document.completed_at(),
        steps: document.steps(),
    };
    require_updated(approval::save_state(conn, record.id, record.version, &state, now).await?)?;

    for event in events {
        let step_order = match event {
            DocumentEvent::StepApproved { order, .. } | DocumentEvent::Rejected { order, .. } => {
                Some(*order)
            }
            _ => None,
        };
        let comment = step_order
            .and_then(|order| document.steps().iter().find(|step| step.order == order))
            .and_then(|step| step.comment.as_ref())
            .map(|comment| comment.as_str().to_owned())
            .filter(|comment| !comment.is_empty());
        let entry = Event {
            id: ApprovalEventId::new(),
            document_id: record.id,
            actor_id,
            kind: event.kind(),
            step_order,
            comment,
            created_at: now,
        };
        approval::insert_event(conn, &entry).await?;
        notify_event_tx(conn, record, document, event, now).await?;
        if let Some(outcome) = LinkedOutcome::from_event(event) {
            run_linked_effect_tx(conn, record, outcome, actor_id, now).await?;
        }
    }
    Ok(())
}

/// 상신 직전 확인과 문서 번호 발급.
///
/// 임시저장해 둔 사이에 양식이 꺼졌거나 결재선의 누군가가 퇴사했을 수 있으므로 다시 확인한다.
/// 번호의 연도는 상신 시점의 회사 시간대 기준이다.
async fn prepare_submission_tx(
    conn: &mut Conn,
    record: &DocumentRecord,
    document: &Document,
    now: Timestamp,
) -> ServiceResult<String> {
    let form = found(approval::find_form(conn, record.form_id).await?)?;
    if !form.active {
        return Err(DomainError::Conflict(ConflictReason::FormInactive).into());
    }
    let approvers = document.steps().iter().map(|step| step.approver);
    require_line_active_tx(conn, approvers, document.references()).await?;

    let year = Company::load(conn).await?.today(now).year();
    let seq = approval::next_document_seq(conn, year).await?;
    let seq = u32::try_from(seq).map_err(ServiceError::internal)?;
    Ok(DocumentNumber::format(year, seq)?)
}

/// 사건별 알림. 같은 사건의 알림은 `dedupe_key`로 한 번만 만들어진다.
async fn notify_event_tx(
    conn: &mut Conn,
    record: &DocumentRecord,
    document: &Document,
    event: &DocumentEvent,
    now: Timestamp,
) -> ServiceResult<()> {
    let id = record.id;
    let mut messages: Vec<(MemberId, &'static str, &'static str, String)> = Vec::new();
    let todo = |approver: MemberId, order: u8| {
        (
            approver,
            "approval.todo",
            "결재할 문서가 도착했습니다",
            format!("approval.todo:{id}:{order}"),
        )
    };
    match *event {
        DocumentEvent::Submitted { first_approver } => {
            messages.push(todo(first_approver, 1));
            messages.extend(document.references().iter().map(|reference| {
                (
                    *reference,
                    "approval.reference",
                    "참조 문서가 도착했습니다",
                    format!("approval.reference:{id}"),
                )
            }));
        }
        DocumentEvent::StepApproved {
            order,
            next_approver,
            ..
        } => messages.extend(next_approver.map(|next| todo(next, order.saturating_add(1)))),
        DocumentEvent::Completed => messages.push((
            record.drafter_id,
            "approval.approved",
            "문서가 최종 승인되었습니다",
            format!("approval.approved:{id}"),
        )),
        DocumentEvent::Rejected { .. } => messages.push((
            record.drafter_id,
            "approval.rejected",
            "문서가 반려되었습니다",
            format!("approval.rejected:{id}"),
        )),
        // 결재 요청을 받았던 사람(첫 결재자)에게 할 일이 사라졌음을 알린다.
        DocumentEvent::Withdrawn => messages.extend(document.steps().first().map(|step| {
            (
                step.approver,
                "approval.withdrawn",
                "기안자가 문서를 회수했습니다",
                format!("approval.withdrawn:{id}"),
            )
        })),
    }
    for (to, kind, title, dedupe_key) in messages {
        notify(
            conn,
            now,
            Notify {
                to,
                kind,
                title: title.to_owned(),
                body: record.title.clone(),
                link: Some(document_link(id)),
                dedupe_key: Some(&dedupe_key),
            },
        )
        .await?;
    }
    Ok(())
}

/// 종료된 문서의 연결 효과를 문서 종류에 따라 실행한다. 효과가 실패하면 전이 전체가 롤백된다.
async fn run_linked_effect_tx(
    conn: &mut Conn,
    record: &DocumentRecord,
    outcome: LinkedOutcome,
    actor_id: MemberId,
    now: Timestamp,
) -> ServiceResult<()> {
    match record.kind {
        DocumentKind::General => Ok(()),
        DocumentKind::Leave => {
            leave::on_document_closed_tx(conn, record.id, outcome, actor_id, now).await
        }
        DocumentKind::AttendanceCorrection => {
            attendance_correction_effect_tx(conn, record, outcome, actor_id, now).await
        }
    }
}

/// 근태 정정 문서가 종료됐을 때의 연결 효과가 들어갈 **자리**다. 지금은 아무것도 하지 않는다.
///
/// 근태 정정 기능이 생기면 여기서 그 기능의 `*_tx` 함수 하나를 부른다
/// (최종 승인 → 기록 정정, 반려·회수 → 요청 종료). 오류를 돌려주면 결재 전이 전체가 롤백된다.
async fn attendance_correction_effect_tx(
    _conn: &mut Conn,
    _record: &DocumentRecord,
    _outcome: LinkedOutcome,
    _actor_id: MemberId,
    _now: Timestamp,
) -> ServiceResult<()> {
    Ok(())
}

// ───────────── 기안·상신 ─────────────

/// 저장할 새 문서의 내용.
struct NewDocument {
    kind: DocumentKind,
    form_id: FormId,
    title: Title,
    body: Body,
    payload: Option<serde_json::Value>,
    line: ApprovalLine,
}

/// 임시저장 문서를 넣는다.
async fn insert_draft_tx(
    conn: &mut Conn,
    drafter: &Member,
    new: NewDocument,
    now: Timestamp,
) -> ServiceResult<DocumentId> {
    let document = Document::draft(new.kind, new.line);
    let record = DocumentRecord {
        id: DocumentId::new(),
        doc_no: None,
        form_id: new.form_id,
        kind: new.kind,
        drafter_id: drafter.id,
        drafter_department_id: drafter.department_id,
        title: new.title.into_string(),
        body: new.body.into_string(),
        payload: new.payload,
        status: document.status(),
        submitted_at: None,
        completed_at: None,
        created_at: now,
        version: 1,
    };
    approval::insert_document(conn, &record).await?;
    approval::replace_line(conn, record.id, document.steps(), document.references()).await?;
    Ok(record.id)
}

/// 상신. 이미 진행 중이면 아무것도 하지 않는다.
async fn submit_tx(
    conn: &mut Conn,
    actor_id: MemberId,
    id: DocumentId,
    now: Timestamp,
) -> ServiceResult<()> {
    let mut loaded = load(conn, id).await?;
    if let Transition::Applied(events) = loaded.document.submit(actor_id, now)? {
        commit_transition_tx(conn, actor_id, &loaded, &events, now).await?;
    }
    Ok(())
}

/// 일반 양식의 문서를 기안한다. `submit`이면 같은 트랜잭션에서 상신까지 한다.
pub async fn create_document(
    state: &AppState,
    ctx: &AuthContext,
    request: &CreateDocumentRequest,
    idempotency: Option<&Idempotency>,
) -> ServiceResult<DocumentDetail> {
    let now = state.now();
    let title = Title::parse(&request.title)?;
    let body = Body::parse(&request.body)?;
    state
        .write_idempotent(ctx.member_id(), idempotency, async |conn: &mut Conn| {
            let form = found(approval::find_form(conn, request.form_id).await?)?;
            // 시스템 양식의 문서는 그 기능(휴가 신청 등)만 만든다. 내용이 정해진 구조여야 하기 때문이다.
            if form.kind != DocumentKind::General {
                return Err(
                    DomainError::validation("form_id", ValidationReason::Inconsistent).into(),
                );
            }
            if !form.active {
                return Err(DomainError::Conflict(ConflictReason::FormInactive).into());
            }
            let line =
                build_line_tx(conn, ctx.member_id(), &request.steps, &request.references).await?;
            let new = NewDocument {
                kind: form.kind,
                form_id: form.id,
                title,
                body,
                payload: None,
                line,
            };
            let id = insert_draft_tx(conn, &ctx.member, new, now).await?;
            if request.submit {
                submit_tx(conn, ctx.member_id(), id, now).await?;
            }
            detail_tx(conn, ctx.member_id(), id).await
        })
        .await
}

/// 시스템 양식의 문서 내용. 연결된 기능(휴가 신청 등)이 만든다.
pub struct SystemDocument {
    pub kind: DocumentKind,
    pub form_code: &'static str,
    pub title: Title,
    pub body: Body,
    pub payload: serde_json::Value,
    pub line: ApprovalLine,
}

/// 시스템 양식의 문서를 만들어 바로 상신한다. 호출한 쪽의 트랜잭션에 묶인다.
pub async fn create_submitted_tx(
    conn: &mut Conn,
    drafter: &Member,
    document: SystemDocument,
    now: Timestamp,
) -> ServiceResult<DocumentId> {
    let form = approval::find_form_by_code(conn, document.form_code)
        .await?
        .ok_or_else(|| ServiceError::internal("system approval form is missing"))?;
    let new = NewDocument {
        kind: document.kind,
        form_id: form.id,
        title: document.title,
        body: document.body,
        payload: Some(document.payload),
        line: document.line,
    };
    let id = insert_draft_tx(conn, drafter, new, now).await?;
    submit_tx(conn, drafter.id, id, now).await?;
    Ok(id)
}

/// 임시저장 문서 수정(기안자만). 제목·본문·결재선을 통째로 바꾼다.
pub async fn update_document(
    state: &AppState,
    ctx: &AuthContext,
    id: DocumentId,
    request: &UpdateDocumentRequest,
) -> ServiceResult<DocumentDetail> {
    let now = state.now();
    let title = Title::parse(&request.title)?;
    let body = Body::parse(&request.body)?;
    state
        .write(async |conn: &mut Conn| {
            let mut loaded = load(conn, id).await?;
            rules::view(&ctx.actor, &loaded.document)?;
            // 모양(400) → 기안자·상태(403/409) → 구성원 조회(404/409) 순으로 확인한다.
            let line = line_from_input(ctx.member_id(), &request.steps, &request.references)?;
            let replaced = loaded.document.replace_line(ctx.member_id(), line)?;
            let approvers = loaded.document.steps().iter().map(|step| step.approver);
            require_line_active_tx(conn, approvers, loaded.document.references()).await?;
            let updated = approval::update_draft_content(
                conn,
                id,
                request.version,
                title.as_str(),
                body.as_str(),
                now,
            )
            .await?;
            require_updated(updated)?;
            if !replaced.is_noop() {
                let document = &loaded.document;
                approval::replace_line(conn, id, document.steps(), document.references()).await?;
            }
            detail_tx(conn, ctx.member_id(), id).await
        })
        .await
}

/// 임시저장 문서 삭제(기안자만). 상신한 문서는 지울 수 없다.
pub async fn delete_document(
    state: &AppState,
    ctx: &AuthContext,
    id: DocumentId,
) -> ServiceResult<()> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let loaded = load(conn, id).await?;
            rules::view(&ctx.actor, &loaded.document)?;
            if loaded.document.drafter() != ctx.member_id() {
                return Err(DomainError::Forbidden.into());
            }
            if loaded.document.status() != DocumentStatus::Draft
                || !approval::delete_draft(conn, id).await?
            {
                return Err(DomainError::Conflict(ConflictReason::DocumentNotEditable).into());
            }
            audit(
                conn,
                now,
                Audit {
                    action: "approval.draft_delete",
                    target_type: "approval_document",
                    target_id: Some(id.to_string()),
                    detail: None,
                    ctx: Some(ctx),
                },
            )
            .await
        })
        .await
}

// ───────────── 상태 전이 ─────────────

/// 전이 하나를 실행하는 공통 경로. 문서가 보이지 않는 사람에게는 전이 판정 전에 `NotFound`다.
async fn transition(
    state: &AppState,
    ctx: &AuthContext,
    id: DocumentId,
    apply: impl FnOnce(&mut Document, Timestamp) -> Result<Transition, DomainError>,
) -> ServiceResult<DocumentDetail> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let mut loaded = load(conn, id).await?;
            rules::view(&ctx.actor, &loaded.document)?;
            if let Transition::Applied(events) = apply(&mut loaded.document, now)? {
                commit_transition_tx(conn, ctx.member_id(), &loaded, &events, now).await?;
            }
            detail_tx(conn, ctx.member_id(), id).await
        })
        .await
}

pub async fn submit_document(
    state: &AppState,
    ctx: &AuthContext,
    id: DocumentId,
) -> ServiceResult<DocumentDetail> {
    let actor = ctx.member_id();
    transition(state, ctx, id, |document, now| document.submit(actor, now)).await
}

/// 승인(결재·합의). 같은 요청이 다시 오면 현재 상태를 그대로 돌려준다.
pub async fn approve(
    state: &AppState,
    ctx: &AuthContext,
    id: DocumentId,
    request: &ApproveRequest,
) -> ServiceResult<DocumentDetail> {
    let comment = text::optional(request.comment.as_deref(), Comment::parse)?;
    let actor = ctx.member_id();
    transition(state, ctx, id, |document, now| {
        document.approve(actor, now, comment)
    })
    .await
}

pub async fn reject(
    state: &AppState,
    ctx: &AuthContext,
    id: DocumentId,
    request: &RejectRequest,
) -> ServiceResult<DocumentDetail> {
    let reason = Reason::parse(&request.reason)?;
    let actor = ctx.member_id();
    transition(state, ctx, id, |document, now| {
        document.reject(actor, now, reason)
    })
    .await
}

/// 회수. 휴가 신청 문서라면 연결된 신청도 같은 트랜잭션에서 취소된다.
pub async fn withdraw(
    state: &AppState,
    ctx: &AuthContext,
    id: DocumentId,
) -> ServiceResult<DocumentDetail> {
    let actor = ctx.member_id();
    transition(state, ctx, id, |document, now| {
        document.withdraw(actor, now)
    })
    .await
}

/// 연결된 기능이 문서를 회수한다(휴가 신청 취소). 호출한 쪽의 트랜잭션에 묶인다.
///
/// 회수는 기안자의 동작이므로 상태 기계에는 기안자의 이름으로 요청하고, 이력에는 실제
/// 요청자(`actor_id`: 본인 또는 대신 취소한 관리자)를 남긴다. 권한은 호출한 쪽이 이미
/// 확인했어야 한다. 누군가 결재한 뒤라면 `WithdrawTooLate`로 실패한다.
pub async fn withdraw_linked_tx(
    conn: &mut Conn,
    actor_id: MemberId,
    id: DocumentId,
    now: Timestamp,
) -> ServiceResult<()> {
    let mut loaded = load(conn, id).await?;
    let drafter = loaded.document.drafter();
    if let Transition::Applied(events) = loaded.document.withdraw(drafter, now)? {
        commit_transition_tx(conn, actor_id, &loaded, &events, now).await?;
    }
    Ok(())
}
