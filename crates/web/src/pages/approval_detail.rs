//! 결재 문서 상세: 머리 정보, 본문, 결재선 타임라인, 참조, 이력, 동작 막대.

use dioxus::prelude::*;
use pango_api::approval::DocumentDetail;
use pango_api::Empty;
use pango_core::approval::{DocumentKind, DocumentStatus, StepStatus};
use pango_core::id::DocumentId;
use pango_core::text::Comment;

use crate::api::{self, ApiError, ApiResult};
use crate::format;
use crate::pages::approval_new::DocumentForm;
use crate::route::Route;
use crate::session::use_session;
use crate::text;
use crate::ui::{
    report_error, report_success, resource_view, Badge, Button, ButtonKind, Card, CharCount,
    ConfirmModal, EmptyState, FormError, IconName, Modal, PageHeader, TextArea, Timeline,
    TimelineItem, Tone,
};

/// 상세 화면의 순수 함수: 동작 판정, 요청 변환, 구조화된 내용 요약.
pub(crate) mod logic {
    use chrono::NaiveDate;
    use chrono_tz::Tz;
    use pango_api::approval::{ApproveRequest, DocumentDetail, EventDto, RejectRequest};
    use pango_core::approval::{DocumentKind, DocumentStatus, EventKind, StepStatus};
    use pango_core::attendance::WorkType;
    use pango_core::id::MemberId;
    use pango_core::leave::LeaveUnit;
    use pango_core::text::{Comment, Reason};
    use pango_core::time::Timestamp;
    use serde_json::{Map, Value};

    use crate::pages::attendance::logic as attendance_logic;
    use crate::{format, text};

    /// 지금 이 사람이 할 수 있는 동작. 화면 노출용이며 서버가 매번 다시 판정한다.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct Actions {
        pub approve: bool,
        pub reject: bool,
        pub withdraw: bool,
        pub edit: bool,
        pub delete: bool,
        pub submit: bool,
        /// 관리자의 대리 반려(`POST /admin/approval/documents/{id}/reject`). 동작 막대가 아니라
        /// 따로 떨어진 관리자 영역에 둔다.
        pub admin_reject: bool,
    }

    impl Actions {
        /// 동작 막대에 놓이는 동작(당사자의 동작)이 있는지. 대리 반려는 세지 않는다.
        pub fn any(&self) -> bool {
            self.approve || self.reject || self.withdraw || self.edit || self.delete || self.submit
        }
    }

    /// 동작 판정. 규칙은 `pango_core::approval::Document`의 전이 함수와 같다.
    ///
    /// - 승인·반려: 진행 중 문서에서 내가 현재(대기 중) 단계의 결재자일 때.
    /// - 회수: 내가 기안자이고 진행 중이며 아직 아무도 처리하지 않았을 때.
    /// - 수정·삭제·상신: 내가 기안자이고 임시저장일 때.
    /// - 대리 반려: 관리자(`capabilities.admin`)이고 진행 중이며, 내가 기안하지 않았고 지금
    ///   차례의 결재자도 아닐 때(차례라면 보통의 반려를 쓴다). 서버 `authz::override_document`와
    ///   같은 규칙이다.
    ///
    /// 관리자라도 결재선에 없으면 대리 반려 말고는 열람만 한다.
    pub fn available_actions(me: MemberId, is_admin: bool, detail: &DocumentDetail) -> Actions {
        let drafter = detail.drafter_id == me;
        let in_progress = detail.status == DocumentStatus::InProgress;
        let draft = detail.status == DocumentStatus::Draft;
        let my_turn = in_progress
            && detail
                .steps
                .iter()
                .any(|step| step.status == StepStatus::Pending && step.approver_id == me);
        let untouched = !detail
            .steps
            .iter()
            .any(|step| matches!(step.status, StepStatus::Approved | StepStatus::Rejected));
        Actions {
            approve: my_turn,
            reject: my_turn,
            withdraw: drafter && in_progress && untouched,
            edit: drafter && draft,
            delete: drafter && draft,
            submit: drafter && draft,
            admin_reject: is_admin && in_progress && !drafter && !my_turn,
        }
    }

    /// 지금 차례(대기 중)인 결재자의 이름.
    pub fn pending_approver_name(detail: &DocumentDetail) -> Option<&str> {
        detail
            .steps
            .iter()
            .find(|step| step.status == StepStatus::Pending)
            .map(|step| step.approver_name.as_str())
    }

    /// 반려 이력의 행위자가 그 이력이 가리키는 단계의 결재자가 아니면(관리자가 대신 반려) 그
    /// 단계의 순서. 서버는 단계에는 건너뛴 결재자를, 이력의 행위자에는 관리자를 남긴다. 이력에
    /// 단계 순서가 없으면 반려 상태인 단계로 찾는다.
    fn is_proxy_rejection(detail: &DocumentDetail, event: &EventDto) -> Option<u8> {
        if event.kind != EventKind::Rejected {
            return None;
        }
        let step = match event.step_order {
            Some(order) => detail.steps.iter().find(|step| step.order == order),
            None => detail
                .steps
                .iter()
                .find(|step| step.status == StepStatus::Rejected),
        }?;
        (step.approver_id != event.actor_id).then_some(step.order)
    }

    /// 관리자가 결재자 대신 반려한 단계: `(단계 순서, 관리자 이름)`. 마지막 반려 이력으로 정한다.
    pub fn proxy_rejection(detail: &DocumentDetail) -> Option<(u8, &str)> {
        let event = detail
            .events
            .iter()
            .rev()
            .find(|event| event.kind == EventKind::Rejected)?;
        is_proxy_rejection(detail, event).map(|order| (order, event.actor_name.as_str()))
    }

    /// 이력 한 줄의 동사. 관리자가 대신 반려한 줄이면 "대리 반려".
    pub fn event_verb(detail: &DocumentDetail, event: &EventDto) -> &'static str {
        if is_proxy_rejection(detail, event).is_some() {
            text::PROXY_REJECT_VERB
        } else {
            text::event_kind_label(event.kind)
        }
    }

    /// 다른 사람이 먼저 처리해 상태가 바뀐 경우. 오류가 아니라 정상 흐름이다(알리고 다시 읽는다).
    pub fn is_stale_outcome(code: &str) -> bool {
        matches!(
            code,
            "not_current_approver"
                | "document_already_closed"
                | "withdraw_too_late"
                | "document_not_editable"
                | "version_conflict"
        )
    }

    /// 승인 의견(선택). 비어 있으면 보내지 않는다.
    pub fn approve_request(comment: &str) -> Result<ApproveRequest, String> {
        let Some(trimmed) = format::non_empty(comment) else {
            return Ok(ApproveRequest { comment: None });
        };
        Comment::parse(trimmed)
            .map(|_| ApproveRequest {
                comment: Some(trimmed.to_owned()),
            })
            .map_err(|error| text::field_error_message("comment", &error))
    }

    /// 반려 사유(필수).
    pub fn reject_request(reason: &str) -> Result<RejectRequest, String> {
        Reason::parse(reason)
            .map(|_| RejectRequest {
                reason: reason.trim().to_owned(),
            })
            .map_err(|error| text::field_error_message("reason", &error))
    }

    fn text_of(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
        keys.iter().find_map(|key| match object.get(*key)? {
            Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    }

    fn date_text(raw: &str) -> String {
        pango_core::calendar::parse_date("date", raw)
            .map(format::date_ymd)
            .unwrap_or_else(|_| raw.to_owned())
    }

    /// 정정 요청의 시각 값(`Timestamp`는 밀리초 숫자로 직렬화된다). 근무일 기준으로 보여 준다
    /// (`09:05`, 다음 날이면 `02:10 (+1일)`). 근무일을 모르면 날짜까지 쓴다.
    /// 키가 없거나 해석할 수 없으면 `None`, 값이 `null`이면 `없음`.
    fn instant_text(
        object: &Map<String, Value>,
        key: &str,
        work_date: Option<NaiveDate>,
        tz: Tz,
    ) -> Option<String> {
        match object.get(key)? {
            Value::Null => Some("없음".to_owned()),
            value => {
                let at = Timestamp::from_millis(value.as_i64()?).ok()?;
                Some(match work_date {
                    Some(day) => attendance_logic::clock_text(at, day, tz),
                    None => format::datetime(at, tz),
                })
            }
        }
    }

    fn period(object: &Map<String, Value>) -> Option<String> {
        let start = text_of(object, &["start_date"]);
        let end = text_of(object, &["end_date"]);
        match (start, end) {
            (Some(start), Some(end)) if start == end => Some(date_text(&start)),
            (Some(start), Some(end)) => {
                Some(format!("{} ~ {}", date_text(&start), date_text(&end)))
            }
            (Some(one), None) | (None, Some(one)) => Some(date_text(&one)),
            (None, None) => None,
        }
    }

    /// 시스템 양식 문서의 구조화된 내용을 `(항목, 값)` 줄로. 모르는 종류·필드나 깨진 값은 건너뛴다.
    pub fn payload_rows(
        kind: DocumentKind,
        payload: Option<&Value>,
        tz: Tz,
    ) -> Vec<(&'static str, String)> {
        let Some(Value::Object(object)) = payload else {
            return Vec::new();
        };
        let rows: Vec<(&'static str, Option<String>)> = match kind {
            DocumentKind::General => Vec::new(),
            DocumentKind::Leave => vec![
                ("종류", text_of(object, &["leave_type_name"])),
                ("기간", period(object)),
                (
                    "단위",
                    text_of(object, &["unit"]).map(|raw| {
                        LeaveUnit::parse(&raw)
                            .map(|unit| text::leave_unit_label(unit).to_owned())
                            .unwrap_or(raw)
                    }),
                ),
                (
                    "일수",
                    object
                        .get("amount_q")
                        .and_then(Value::as_i64)
                        .and_then(|q| i32::try_from(q).ok())
                        .map(format::leave_days),
                ),
                ("사유", text_of(object, &["reason"])),
            ],
            // 서버 `attendance::create_correction`이 싣는 모양:
            // work_date, clock_in_at, clock_out_at(없으면 null), work_type, reason,
            // current(요청 당시 기록 또는 null), request_id.
            DocumentKind::AttendanceCorrection => {
                let work_date = text_of(object, &["work_date"]);
                let day = work_date
                    .as_deref()
                    .and_then(|raw| pango_core::calendar::parse_date("work_date", raw).ok());
                vec![
                    ("근무일", work_date.map(|d| date_text(&d))),
                    ("요청 출근", instant_text(object, "clock_in_at", day, tz)),
                    ("요청 퇴근", instant_text(object, "clock_out_at", day, tz)),
                    (
                        "근무 형태",
                        text_of(object, &["work_type"]).map(|raw| {
                            WorkType::parse(&raw)
                                .map(|kind| text::work_type_label(kind).to_owned())
                                .unwrap_or(raw)
                        }),
                    ),
                    (
                        "요청 당시 기록",
                        object.get("current").map(|current| match current {
                            Value::Null => "기록 없음".to_owned(),
                            value => attendance_logic::record_snapshot(value, tz),
                        }),
                    ),
                    ("사유", text_of(object, &["reason"])),
                ]
            }
        };
        rows.into_iter()
            .filter_map(|(label, value)| value.map(|value| (label, value)))
            .collect()
    }

    /// 기안자 표시: `홍길동 · 개발팀`.
    pub fn drafter_line(detail: &DocumentDetail) -> String {
        match detail.drafter_department_name.as_deref().map(str::trim) {
            Some(department) if !department.is_empty() => {
                format!("{} · {department}", detail.drafter_name)
            }
            _ => detail.drafter_name.clone(),
        }
    }
}

use logic::Actions;

/// 확인 단계를 거치는 동작.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Approve,
    Reject,
    Withdraw,
    Delete,
    Submit,
    /// 관리자의 대리 반려.
    AdminReject,
}

impl Act {
    const fn done_message(self) -> &'static str {
        match self {
            Self::Approve => "승인했습니다.",
            Self::Reject => "반려했습니다.",
            Self::Withdraw => "회수했습니다.",
            Self::Delete => "삭제했습니다.",
            Self::Submit => "상신했습니다.",
            Self::AdminReject => "관리자 권한으로 반려했습니다.",
        }
    }
}

fn not_found() -> ApiError {
    ApiError {
        status: 404,
        code: "not_found".into(),
        field: None,
        reason: None,
    }
}

#[component]
pub fn ApprovalDetail(id: String) -> Element {
    let session = use_session();
    let nav = navigator();
    let mut doc = use_resource(use_reactive!(|id| async move {
        // 경로 값은 그대로 서버 경로에 넣지 않는다. 형식이 틀리면 없는 문서다.
        let Ok(id) = DocumentId::parse(&id) else {
            return Err(not_found());
        };
        api::get::<DocumentDetail>(&format!("/approval/documents/{id}")).await
    }));
    let mut editing = use_signal(|| false);
    let mut dialog = use_signal(|| None::<Act>);
    let mut busy = use_signal(|| false);
    let mut note = use_signal(String::new);
    let mut dialog_error = use_signal(String::new);

    let me = session.me();
    let tz = format::member_zone(me.as_ref());
    let me_id = me.as_ref().map(|m| m.id);
    let is_admin = me.as_ref().is_some_and(|m| m.capabilities.admin);

    let run = move |act: Act| async move {
        if busy() {
            return;
        }
        let Some(Ok(id)) = doc.peek().as_ref().map(|r| r.as_ref().map(|d| d.id)) else {
            return;
        };
        let path = format!("/approval/documents/{id}");
        let result: ApiResult<()> = match act {
            Act::Approve => match logic::approve_request(&note()) {
                Ok(body) => {
                    busy.set(true);
                    api::post::<_, DocumentDetail>(&format!("{path}/approve"), &body)
                        .await
                        .map(|_| ())
                }
                Err(message) => {
                    dialog_error.set(message);
                    return;
                }
            },
            Act::Reject => match logic::reject_request(&note()) {
                Ok(body) => {
                    busy.set(true);
                    api::post::<_, DocumentDetail>(&format!("{path}/reject"), &body)
                        .await
                        .map(|_| ())
                }
                Err(message) => {
                    dialog_error.set(message);
                    return;
                }
            },
            Act::AdminReject => match logic::reject_request(&note()) {
                Ok(body) => {
                    busy.set(true);
                    api::post::<_, DocumentDetail>(
                        &format!("/admin/approval/documents/{id}/reject"),
                        &body,
                    )
                    .await
                    .map(|_| ())
                }
                Err(message) => {
                    dialog_error.set(message);
                    return;
                }
            },
            Act::Withdraw | Act::Submit => {
                busy.set(true);
                let verb = if act == Act::Withdraw {
                    "withdraw"
                } else {
                    "submit"
                };
                api::post::<_, DocumentDetail>(&format!("{path}/{verb}"), &Empty {})
                    .await
                    .map(|_| ())
            }
            Act::Delete => {
                busy.set(true);
                api::delete::<Empty>(&path).await.map(|_| ())
            }
        };
        busy.set(false);
        match result {
            Ok(()) => {
                dialog.set(None);
                note.set(String::new());
                report_success(act.done_message());
                if act == Act::Delete {
                    nav.replace(Route::ApprovalList {});
                } else {
                    doc.restart();
                }
            }
            // 그사이 다른 사람이 처리했거나 문서가 사라졌다: 알리고 최신 상태를 보여 준다.
            Err(error) if logic::is_stale_outcome(&error.code) || error.status == 404 => {
                dialog.set(None);
                report_error(&error);
                doc.restart();
            }
            Err(error) => dialog_error.set(error.message()),
        }
    };
    let mut open = move |act: Act| {
        note.set(String::new());
        dialog_error.set(String::new());
        dialog.set(Some(act));
    };
    let close = move |_| {
        if !busy() {
            dialog.set(None);
        }
    };

    if let Some(Err(error)) = &*doc.read() {
        if error.status == 404 {
            return rsx! {
                PageHeader { title: "결재 문서" }
                EmptyState {
                    title: "문서를 찾을 수 없습니다.",
                    hint: "삭제되었거나 볼 수 있는 문서가 아닙니다.",
                    icon: IconName::Document,
                    Link { class: "btn btn-secondary btn-sm", to: Route::ApprovalList {}, "결재함으로" }
                }
            };
        }
    }

    let Some(me_id) = me_id else {
        return rsx! {};
    };

    rsx! {
        {resource_view(doc, move |detail: DocumentDetail| {
            if editing() && detail.status == DocumentStatus::Draft {
                return rsx! {
                    PageHeader { title: "임시저장 문서 수정" }
                    DocumentForm {
                        draft: detail,
                        ondone: move |_| {
                            editing.set(false);
                            doc.restart();
                        },
                        oncancel: move |_| editing.set(false),
                    }
                };
            }
            let actions = logic::available_actions(me_id, is_admin, &detail);
            let rows = logic::payload_rows(detail.kind, detail.payload.as_ref(), tz);
            rsx! {
                DocumentView { detail: detail.clone(), rows, tz }
                if actions.admin_reject {
                    Card { title: "관리자 동작",
                        p { class: "field-hint",
                            "차례인 결재자가 퇴사·정지했거나 처리할 수 없어 문서가 멈춰 있을 때만 씁니다."
                        }
                        div { class: "card-actions",
                            Button {
                                kind: ButtonKind::Danger,
                                disabled: busy(),
                                onclick: move |_| open(Act::AdminReject),
                                "관리자 대리 반려"
                            }
                        }
                    }
                }
                if actions.any() {
                    ActionBar {
                        actions,
                        busy: busy(),
                        onact: move |act: Act| open(act),
                        onedit: move |_| editing.set(true),
                    }
                }
            }
        })}
        match dialog() {
            Some(Act::Approve) => rsx! {
                Modal { title: "승인", onclose: close,
                    div { class: "form",
                        FormError { message: dialog_error() }
                        TextArea {
                            label: "의견 (선택)",
                            value: note(),
                            rows: 4,
                            maxlength: Comment::RULE.max_chars as i64,
                            oninput: move |value| note.set(value),
                        }
                        CharCount { count: note().chars().count(), max: Comment::RULE.max_chars }
                        div { class: "modal-actions",
                            Button { kind: ButtonKind::Secondary, disabled: busy(), onclick: move |_| dialog.set(None), "취소" }
                            Button { busy: busy(), onclick: move |_| { spawn(run(Act::Approve)); }, "승인" }
                        }
                    }
                }
            },
            Some(Act::Reject) => rsx! {
                Modal { title: "반려", onclose: close,
                    div { class: "form",
                        p { class: "confirm-message", "반려하면 문서가 바로 종료되고 되돌릴 수 없습니다." }
                        FormError { message: dialog_error() }
                        TextArea {
                            label: "반려 사유",
                            value: note(),
                            rows: 4,
                            required: true,
                            maxlength: pango_core::text::Reason::RULE.max_chars as i64,
                            oninput: move |value| note.set(value),
                        }
                        CharCount { count: note().chars().count(), max: pango_core::text::Reason::RULE.max_chars }
                        div { class: "modal-actions",
                            Button { kind: ButtonKind::Secondary, disabled: busy(), onclick: move |_| dialog.set(None), "취소" }
                            Button { kind: ButtonKind::Danger, busy: busy(), onclick: move |_| { spawn(run(Act::Reject)); }, "반려" }
                        }
                    }
                }
            },
            Some(Act::AdminReject) => {
                let pending = doc
                    .read()
                    .as_ref()
                    .and_then(|loaded| loaded.as_ref().ok())
                    .and_then(|detail| logic::pending_approver_name(detail).map(str::to_owned))
                    .unwrap_or_else(|| "지금 차례의 결재자".to_owned());
                rsx! {
                    Modal { title: "관리자 대리 반려", onclose: close,
                        div { class: "form",
                            p { class: "confirm-message",
                                "{pending}님의 결재 차례에서 문서를 반려로 끝냅니다. 차례인 결재자가 퇴사·정지했거나 처리할 수 없을 때만 사용하세요."
                            }
                            p { class: "field-hint",
                                "되돌릴 수 없습니다. 휴가 신청이면 연차가 복원되고, 기안자와 결재자에게 알림이 가며 감사 로그에 남습니다."
                            }
                            FormError { message: dialog_error() }
                            TextArea {
                                label: "반려 사유",
                                value: note(),
                                rows: 4,
                                required: true,
                                maxlength: pango_core::text::Reason::RULE.max_chars as i64,
                                oninput: move |value| note.set(value),
                            }
                            CharCount { count: note().chars().count(), max: pango_core::text::Reason::RULE.max_chars }
                            div { class: "modal-actions",
                                Button { kind: ButtonKind::Secondary, disabled: busy(), onclick: move |_| dialog.set(None), "취소" }
                                Button { kind: ButtonKind::Danger, busy: busy(), onclick: move |_| { spawn(run(Act::AdminReject)); }, "대리 반려" }
                            }
                        }
                    }
                }
            }
            Some(act @ (Act::Withdraw | Act::Delete | Act::Submit)) => {
                let (title, message, label, danger) = match act {
                    Act::Withdraw => ("문서 회수", "회수하면 결재가 중단되고 문서는 회수 상태로 종료됩니다. 휴가 신청이라면 신청도 함께 취소됩니다.", "회수", true),
                    Act::Delete => ("임시저장 문서 삭제", "삭제한 문서는 되돌릴 수 없습니다.", "삭제", true),
                    _ => ("상신", "상신하면 첫 번째 결재자에게 문서가 전달됩니다. 상신한 뒤에는 내용을 고칠 수 없습니다.", "상신", false),
                };
                rsx! {
                    ConfirmModal {
                        title,
                        message: if dialog_error().is_empty() { message.to_owned() } else { dialog_error() },
                        confirm_label: label,
                        danger,
                        busy: busy(),
                        onconfirm: move |_| { spawn(run(act)); },
                        oncancel: close,
                    }
                }
            }
            None => rsx! {},
        }
    }
}

#[component]
fn DocumentView(
    detail: DocumentDetail,
    rows: Vec<(&'static str, String)>,
    tz: chrono_tz::Tz,
) -> Element {
    let drafter = logic::drafter_line(&detail);
    let proxy = logic::proxy_rejection(&detail)
        .map(|(order, admin)| (order, text::proxy_rejection_note(admin)));
    let verbs: Vec<&'static str> = detail
        .events
        .iter()
        .map(|event| logic::event_verb(&detail, event))
        .collect();
    rsx! {
        PageHeader {
            title: detail.title.clone(),
            subtitle: detail.doc_no.clone().unwrap_or_else(|| "문서 번호는 상신할 때 붙습니다.".into()),
        }
        Card {
            div { class: "doc-head",
                Badge { tone: text::document_status_tone(detail.status), {text::document_status_label(detail.status)} }
                if detail.my_turn {
                    Badge { tone: Tone::Warning, "내 차례" }
                }
                if detail.kind != DocumentKind::General {
                    Badge { tone: Tone::Info, {text::document_kind_label(detail.kind)} }
                }
            }
            dl { class: "detail-list",
                dt { "양식" }
                dd { "{detail.form_name}" }
                dt { "기안자" }
                dd { "{drafter}" }
                dt { "작성" }
                dd { {format::datetime(detail.created_at, tz)} }
                if let Some(at) = detail.submitted_at {
                    dt { "상신" }
                    dd { {format::datetime(at, tz)} }
                }
                if let Some(at) = detail.completed_at {
                    dt { "완료" }
                    dd { {format::datetime(at, tz)} }
                }
            }
        }
        if !rows.is_empty() {
            Card { title: "신청 내용",
                dl { class: "detail-list detail-list-flush",
                    for (label , value) in rows {
                        div { key: "{label}", class: "detail-pair",
                            dt { "{label}" }
                            dd { "{value}" }
                        }
                    }
                }
            }
        }
        Card { title: "내용",
            // 본문은 사용자가 쓴 글이므로 텍스트로만 넣는다. 줄바꿈은 CSS(pre-wrap)로 살린다.
            div { class: "doc-body", "{detail.body}" }
        }
        Card { title: "결재선",
            Timeline {
                for step in detail.steps.iter() {
                    TimelineItem {
                        key: "{step.order}",
                        tone: text::step_status_tone(step.status),
                        current: step.status == StepStatus::Pending,
                        div { class: "timeline-head",
                            strong { "{step.order}. {step.approver_name}" }
                            span { class: "timeline-meta", {text::step_kind_label(step.kind)} }
                            Badge { tone: text::step_status_tone(step.status), {text::step_status_label(step.status)} }
                        }
                        if let Some(at) = step.acted_at {
                            small { class: "timeline-meta", {format::datetime(at, tz)} }
                        }
                        if let Some((_, note)) = proxy.as_ref().filter(|(order, _)| *order == step.order) {
                            small { class: "timeline-meta", "{note}" }
                        }
                        if let Some(comment) = step.comment.as_ref().filter(|c| !c.trim().is_empty()) {
                            p { class: "timeline-comment", "{comment}" }
                        }
                    }
                }
            }
        }
        if !detail.references.is_empty() {
            Card { title: "참조",
                ul { class: "list",
                    for reference in detail.references.iter() {
                        li { key: "{reference.member_id}", class: "list-row",
                            div { class: "list-row-main", strong { "{reference.name}" } }
                            div { class: "list-row-side",
                                if let Some(at) = reference.read_at {
                                    Badge { tone: Tone::Success, "읽음" }
                                    small { {format::datetime(at, tz)} }
                                } else {
                                    Badge { tone: Tone::Neutral, "읽지 않음" }
                                }
                            }
                        }
                    }
                }
            }
        }
        if !detail.events.is_empty() {
            Card { title: "이력",
                Timeline {
                    for (index , (event , verb)) in detail.events.iter().zip(verbs).enumerate() {
                        TimelineItem { key: "{index}",
                            div { class: "timeline-head",
                                strong { "{event.actor_name}" }
                                span { class: "timeline-meta", "{verb}" }
                            }
                            small { class: "timeline-meta", {format::datetime(event.created_at, tz)} }
                            if let Some(comment) = event.comment.as_ref().filter(|c| !c.trim().is_empty()) {
                                p { class: "timeline-comment", "{comment}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 화면 아래에 붙는 동작 막대. 모바일에서는 하단 탭 바로 위에 붙는다.
#[component]
fn ActionBar(
    actions: Actions,
    busy: bool,
    onact: EventHandler<Act>,
    onedit: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "action-bar", role: "toolbar", "aria-label": "문서 동작",
            if actions.delete {
                Button { kind: ButtonKind::Ghost, disabled: busy, onclick: move |_| onact.call(Act::Delete), "삭제" }
            }
            if actions.withdraw {
                Button { kind: ButtonKind::Secondary, disabled: busy, onclick: move |_| onact.call(Act::Withdraw), "회수" }
            }
            if actions.edit {
                Button { kind: ButtonKind::Secondary, disabled: busy, onclick: move |_| onedit.call(()), "수정" }
            }
            if actions.submit {
                Button { disabled: busy, onclick: move |_| onact.call(Act::Submit), "상신" }
            }
            if actions.reject {
                Button { kind: ButtonKind::Danger, disabled: busy, onclick: move |_| onact.call(Act::Reject), "반려" }
            }
            if actions.approve {
                Button { disabled: busy, onclick: move |_| onact.call(Act::Approve), "승인" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::logic::*;
    use pango_api::approval::{DocumentDetail, EventDto, ReferenceDto, StepDto};
    use pango_core::approval::{DocumentKind, DocumentStatus, EventKind, StepKind, StepStatus};
    use pango_core::id::{DocumentId, FormId, MemberId};
    use pango_core::time::Timestamp;
    use serde_json::json;

    use crate::format;

    struct People {
        drafter: MemberId,
        first: MemberId,
        second: MemberId,
        reference: MemberId,
        admin: MemberId,
    }

    fn people() -> People {
        People {
            drafter: MemberId::new(),
            first: MemberId::new(),
            second: MemberId::new(),
            reference: MemberId::new(),
            admin: MemberId::new(),
        }
    }

    fn step(order: u8, approver: MemberId, status: StepStatus) -> StepDto {
        StepDto {
            order,
            approver_id: approver,
            approver_name: "결재자".into(),
            kind: StepKind::Approve,
            status,
            acted_at: None,
            comment: None,
        }
    }

    fn detail(p: &People, status: DocumentStatus, steps: [StepStatus; 2]) -> DocumentDetail {
        DocumentDetail {
            id: DocumentId::new(),
            doc_no: None,
            form_id: FormId::new(),
            form_name: "품의서".into(),
            kind: DocumentKind::General,
            status,
            drafter_id: p.drafter,
            drafter_name: "기안자".into(),
            drafter_department_name: Some("개발팀".into()),
            title: "t".into(),
            body: "b".into(),
            payload: None,
            steps: vec![step(1, p.first, steps[0]), step(2, p.second, steps[1])],
            references: vec![ReferenceDto {
                member_id: p.reference,
                name: "참조자".into(),
                read_at: None,
            }],
            events: vec![],
            submitted_at: None,
            completed_at: None,
            created_at: Timestamp::from_millis(0).expect("ts"),
            version: 1,
            my_turn: false,
        }
    }

    const NONE: Actions = Actions {
        approve: false,
        reject: false,
        withdraw: false,
        edit: false,
        delete: false,
        submit: false,
        admin_reject: false,
    };
    const DRAFT_OWNER: Actions = Actions {
        edit: true,
        delete: true,
        submit: true,
        ..NONE
    };
    const DECIDE: Actions = Actions {
        approve: true,
        reject: true,
        ..NONE
    };
    const WITHDRAW: Actions = Actions {
        withdraw: true,
        ..NONE
    };
    const OVERRIDE: Actions = Actions {
        admin_reject: true,
        ..NONE
    };

    #[test]
    fn action_matrix_by_role_and_status() {
        use DocumentStatus as D;
        use StepStatus as S;
        let p = people();
        // (문서 상태, 단계 상태들, [기안자, 1단계, 2단계, 참조자, 관리자]) — 관리자 권한 없이.
        let cases: [(D, [S; 2], [Actions; 5]); 7] = [
            (
                D::Draft,
                [S::Waiting, S::Waiting],
                [DRAFT_OWNER, NONE, NONE, NONE, NONE],
            ),
            (
                D::InProgress,
                [S::Pending, S::Waiting],
                [WITHDRAW, DECIDE, NONE, NONE, NONE],
            ),
            (
                D::InProgress,
                [S::Approved, S::Pending],
                [NONE, NONE, DECIDE, NONE, NONE],
            ),
            (D::Approved, [S::Approved, S::Approved], [NONE; 5]),
            (D::Rejected, [S::Rejected, S::Waiting], [NONE; 5]),
            (D::Rejected, [S::Approved, S::Rejected], [NONE; 5]),
            (D::Withdrawn, [S::Waiting, S::Waiting], [NONE; 5]),
        ];
        for (status, steps, expected) in cases {
            let doc = detail(&p, status, steps);
            let who = [p.drafter, p.first, p.second, p.reference, p.admin];
            for (member, want) in who.iter().zip(expected) {
                assert_eq!(
                    available_actions(*member, false, &doc),
                    want,
                    "{status:?} {steps:?}"
                );
            }
        }
    }

    #[test]
    fn admin_matrix_adds_override_only_where_the_server_allows_it() {
        use DocumentStatus as D;
        use StepStatus as S;
        let p = people();
        // 같은 사람들이 모두 관리자 권한을 가졌을 때. 대리 반려는 진행 중 문서에서 기안자도 지금
        // 차례의 결재자도 아닌 관리자에게만 열린다(기안자는 403, 차례인 결재자는 보통의 반려).
        let cases: [(D, [S; 2], [Actions; 5]); 7] = [
            (
                D::Draft,
                [S::Waiting, S::Waiting],
                [DRAFT_OWNER, NONE, NONE, NONE, NONE],
            ),
            (
                D::InProgress,
                [S::Pending, S::Waiting],
                [WITHDRAW, DECIDE, OVERRIDE, OVERRIDE, OVERRIDE],
            ),
            (
                D::InProgress,
                [S::Approved, S::Pending],
                [NONE, OVERRIDE, DECIDE, OVERRIDE, OVERRIDE],
            ),
            (D::Approved, [S::Approved, S::Approved], [NONE; 5]),
            (D::Rejected, [S::Rejected, S::Waiting], [NONE; 5]),
            (D::Rejected, [S::Approved, S::Rejected], [NONE; 5]),
            (D::Withdrawn, [S::Waiting, S::Waiting], [NONE; 5]),
        ];
        for (status, steps, expected) in cases {
            let doc = detail(&p, status, steps);
            let who = [p.drafter, p.first, p.second, p.reference, p.admin];
            for (member, want) in who.iter().zip(expected) {
                assert_eq!(
                    available_actions(*member, true, &doc),
                    want,
                    "{status:?} {steps:?}"
                );
            }
        }
        // 대리 반려는 동작 막대에 세지 않는다.
        assert!(!OVERRIDE.any());
    }

    fn event(kind: EventKind, actor: MemberId, name: &str, order: Option<u8>) -> EventDto {
        EventDto {
            kind,
            actor_id: actor,
            actor_name: name.into(),
            step_order: order,
            comment: None,
            created_at: Timestamp::from_millis(0).expect("ts"),
        }
    }

    #[test]
    fn pending_approver_is_the_current_step() {
        let p = people();
        let mut doc = detail(
            &p,
            DocumentStatus::InProgress,
            [StepStatus::Approved, StepStatus::Pending],
        );
        doc.steps[1].approver_name = "이차례".into();
        assert_eq!(pending_approver_name(&doc), Some("이차례"));
        let closed = detail(
            &p,
            DocumentStatus::Approved,
            [StepStatus::Approved, StepStatus::Approved],
        );
        assert_eq!(pending_approver_name(&closed), None);
    }

    #[test]
    fn proxy_rejection_is_detected_from_the_event_actor() {
        let p = people();
        let mut doc = detail(
            &p,
            DocumentStatus::Rejected,
            [StepStatus::Approved, StepStatus::Rejected],
        );
        doc.events = vec![
            event(EventKind::Submitted, p.drafter, "기안자", None),
            event(EventKind::Approved, p.first, "결재자", Some(1)),
            event(EventKind::Rejected, p.admin, "김관리", Some(2)),
        ];
        assert_eq!(proxy_rejection(&doc), Some((2, "김관리")));
        assert_eq!(event_verb(&doc, &doc.events[2]), "대리 반려");
        assert_eq!(event_verb(&doc, &doc.events[1]), "승인");
        assert_eq!(event_verb(&doc, &doc.events[0]), "상신");

        // 결재자가 직접 반려했으면 대리 반려가 아니다.
        doc.events[2] = event(EventKind::Rejected, p.second, "결재자", Some(2));
        assert_eq!(proxy_rejection(&doc), None);
        assert_eq!(event_verb(&doc, &doc.events[2]), "반려");

        // 이력에 단계 순서가 없으면 반려된 단계로 찾는다.
        doc.events[2] = event(EventKind::Rejected, p.admin, "김관리", None);
        assert_eq!(proxy_rejection(&doc), Some((2, "김관리")));

        // 반려 이력이 없거나, 가리키는 단계가 없으면 아무것도 붙이지 않는다.
        doc.events.truncate(2);
        assert_eq!(proxy_rejection(&doc), None);
        doc.events
            .push(event(EventKind::Rejected, p.admin, "김관리", Some(9)));
        assert_eq!(proxy_rejection(&doc), None);
        assert_eq!(event_verb(&doc, &doc.events[2]), "반려");
    }

    #[test]
    fn inconsistent_step_state_does_not_grant_actions() {
        let p = people();
        // 진행 중이 아닌데 대기 단계가 남은(손상된) 문서에서는 승인할 수 없다.
        let doc = detail(
            &p,
            DocumentStatus::Approved,
            [StepStatus::Pending, StepStatus::Waiting],
        );
        assert_eq!(available_actions(p.first, false, &doc), NONE);
        assert_eq!(
            available_actions(p.admin, true, &doc),
            NONE,
            "끝난 문서는 대리 반려도 없다"
        );
        assert!(!NONE.any());
        assert!(WITHDRAW.any());
    }

    #[test]
    fn stale_outcomes_are_recognised() {
        for code in [
            "not_current_approver",
            "document_already_closed",
            "withdraw_too_late",
        ] {
            assert!(is_stale_outcome(code), "{code}");
        }
        assert!(!is_stale_outcome("validation_failed"));
        assert!(!is_stale_outcome("network_error"));
    }

    #[test]
    fn comment_is_optional_and_reason_is_required() {
        assert_eq!(approve_request("  ").expect("ok").comment, None);
        assert_eq!(
            approve_request(" 좋습니다 ")
                .expect("ok")
                .comment
                .as_deref(),
            Some("좋습니다")
        );
        assert!(approve_request(&"가".repeat(1001)).is_err());
        assert_eq!(reject_request("").unwrap_err(), "사유을(를) 입력해 주세요.");
        assert!(reject_request("\u{0007}").is_err());
        assert_eq!(
            reject_request(" 금액 확인 ").expect("ok").reason,
            "금액 확인"
        );
    }

    #[test]
    fn leave_payload_is_summarised() {
        let tz = format::zone("Asia/Seoul");
        let payload = json!({
            "leave_type_name": "연차",
            "start_date": "2026-03-02",
            "end_date": "2026-03-03",
            "unit": "full_day",
            "amount_q": 8,
            "reason": "가족 여행",
        });
        assert_eq!(
            payload_rows(DocumentKind::Leave, Some(&payload), tz),
            vec![
                ("종류", "연차".to_owned()),
                ("기간", "2026. 3. 2. ~ 2026. 3. 3.".to_owned()),
                ("단위", "종일".to_owned()),
                ("일수", "2일".to_owned()),
                ("사유", "가족 여행".to_owned()),
            ]
        );
        let half = json!({ "start_date": "2026-03-02", "end_date": "2026-03-02", "unit": "am_half", "amount_q": 2 });
        assert_eq!(
            payload_rows(DocumentKind::Leave, Some(&half), tz),
            vec![
                ("기간", "2026. 3. 2.".to_owned()),
                ("단위", "오전 반차".to_owned()),
                ("일수", "0.5일".to_owned()),
            ]
        );
    }

    #[test]
    fn malformed_payloads_do_not_panic() {
        let tz = format::zone("Asia/Seoul");
        let cases = [
            json!(null),
            json!([1, 2, 3]),
            json!("문자열"),
            json!(42),
            json!({}),
            json!({ "start_date": 5, "unit": null, "amount_q": "8", "reason": {"x": 1} }),
            json!({ "amount_q": 1e300, "end_date": "  " }),
            json!({ "amount_q": i64::MAX }),
        ];
        for payload in &cases {
            for kind in DocumentKind::ALL {
                let _ = payload_rows(*kind, Some(payload), tz);
            }
        }
        assert!(payload_rows(DocumentKind::Leave, None, tz).is_empty());
        assert!(payload_rows(DocumentKind::General, Some(&json!({"reason": "x"})), tz).is_empty());

        let odd = json!({ "start_date": "어제", "unit": "lunch", "amount_q": i64::MAX });
        assert_eq!(
            payload_rows(DocumentKind::Leave, Some(&odd), tz),
            vec![("기간", "어제".to_owned()), ("단위", "lunch".to_owned())],
            "모르는 값은 그대로, 범위를 넘는 수는 건너뛴다"
        );
    }

    #[test]
    fn attendance_correction_payload_matches_the_server_shape() {
        let tz = format::zone("Asia/Seoul");
        // 2026-03-02 09:05 KST, 다음 날 02:10 KST.
        let clock_in = 1_772_409_900_000_i64;
        let clock_out = clock_in + (17 * 60 + 5) * 60_000;
        let payload = json!({
            "request_id": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
            "work_date": "2026-03-02",
            "clock_in_at": clock_in,
            "clock_out_at": clock_out,
            "work_type": "remote",
            "reason": " 지문 인식 실패 ",
            "current": {
                "id": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5c",
                "member_id": "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5d",
                "work_date": "2026-03-02",
                "clock_in_at": clock_in + 3_600_000,
                "clock_out_at": null,
                "work_type": "office",
                "note": null,
                "version": 2,
            },
        });
        assert_eq!(
            payload_rows(DocumentKind::AttendanceCorrection, Some(&payload), tz),
            vec![
                ("근무일", "2026. 3. 2.".to_owned()),
                ("요청 출근", "09:05".to_owned()),
                ("요청 퇴근", "02:10 (+1일)".to_owned()),
                ("근무 형태", "재택".to_owned()),
                (
                    "요청 당시 기록",
                    "출근 10:05 · 퇴근 없음 · 사무실".to_owned()
                ),
                ("사유", "지문 인식 실패".to_owned()),
            ]
        );
    }

    #[test]
    fn attendance_correction_payload_without_record_or_clock_out() {
        let tz = format::zone("Asia/Seoul");
        let payload = json!({
            "work_date": "2026-03-02",
            "clock_in_at": 1_772_409_900_000_i64,
            "clock_out_at": null,
            "work_type": "field",
            "reason": "외근 후 미기록",
            "current": null,
        });
        let rows = payload_rows(DocumentKind::AttendanceCorrection, Some(&payload), tz);
        assert_eq!(rows[2], ("요청 퇴근", "없음".to_owned()));
        assert_eq!(rows[3], ("근무 형태", "외근".to_owned()));
        assert_eq!(rows[4], ("요청 당시 기록", "기록 없음".to_owned()));
    }

    #[test]
    fn attendance_correction_payload_skips_broken_values() {
        let tz = format::zone("Asia/Seoul");
        let broken = json!({
            "work_date": "어제",
            "clock_in_at": "2026-03-02T00:05:00Z",
            "clock_out_at": i64::MAX,
            "work_type": "moon",
            "current": { "unexpected": true },
        });
        assert_eq!(
            payload_rows(DocumentKind::AttendanceCorrection, Some(&broken), tz),
            vec![
                ("근무일", "어제".to_owned()),
                ("근무 형태", "moon".to_owned()),
                ("요청 당시 기록", "내용을 표시할 수 없습니다.".to_owned()),
            ],
            "시각은 밀리초 숫자만, 범위를 넘는 값은 건너뛴다"
        );
        // 근무일이 깨져도 시각은 날짜까지 보여 준다.
        let no_day = json!({ "work_date": null, "clock_in_at": 1_772_409_900_000_i64 });
        assert_eq!(
            payload_rows(DocumentKind::AttendanceCorrection, Some(&no_day), tz),
            vec![("요청 출근", "2026. 3. 2. 09:05".to_owned())]
        );
    }

    #[test]
    fn drafter_line_includes_department_when_present() {
        let p = people();
        let mut doc = detail(&p, DocumentStatus::Draft, [StepStatus::Waiting; 2]);
        assert_eq!(drafter_line(&doc), "기안자 · 개발팀");
        doc.drafter_department_name = Some(" ".into());
        assert_eq!(drafter_line(&doc), "기안자");
        doc.drafter_department_name = None;
        assert_eq!(drafter_line(&doc), "기안자");
    }
}
