//! 결재 문서 작성. 같은 폼으로 임시저장 문서도 고친다(상세 화면의 "수정").

use dioxus::prelude::*;
use pango_api::approval::{DocumentDetail, FormDto};
use pango_api::Empty;
use pango_core::id::{DocumentId, FormId};
use pango_core::text::{Body, Title};

use crate::api;
use crate::route::Route;
use crate::session::use_session;
use crate::ui::{
    report_error, report_success, ApprovalLineEditor, ApproverEntry, Button, ButtonKind, Card,
    CharCount, FormError, Icon, IconName, MemberPicker, PageHeader, SelectField, TextArea,
    TextField,
};

/// 작성 폼의 순수 함수: 검증, 요청 변환, 참조자 편집.
pub(crate) mod logic {
    use pango_api::approval::{
        CreateDocumentRequest, DocumentDetail, FormDto, UpdateDocumentRequest,
    };
    use pango_core::approval::{ApprovalLine, DocumentKind, LineStep, MAX_REFERENCES, MAX_STEPS};
    use pango_core::error::{DomainError, ValidationReason};
    use pango_core::id::{FormId, MemberId};
    use pango_core::text::{Body, Title};

    use crate::text;
    use crate::ui::approval_line::to_inputs;
    use crate::ui::ApproverEntry;

    /// 참조자 한 명(화면 표시용 이름 포함).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ReferenceDraft {
        pub member_id: MemberId,
        pub name: String,
    }

    /// 필드별 오류 문구. 비어 있으면 문제없음.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Problems {
        pub title: String,
        pub body: String,
        pub line: String,
        pub references: String,
    }

    impl Problems {
        pub fn is_empty(&self) -> bool {
            self.title.is_empty()
                && self.body.is_empty()
                && self.line.is_empty()
                && self.references.is_empty()
        }
    }

    fn field_message(error: &DomainError) -> String {
        match error {
            DomainError::Validation { field, reason } => {
                text::error_message("validation_failed", Some(field), Some(reason.code()))
            }
            _ => text::error_message("validation_failed", None, None),
        }
    }

    /// [`ApprovalLine::new`]의 오류를 사람이 고칠 수 있는 문구로. `steps`의 `Inconsistent`는
    /// 두 경우(본인 포함, 마지막이 합의)를 함께 뜻하므로 입력을 보고 어느 쪽인지 가린다.
    pub fn line_message(drafter: MemberId, steps: &[ApproverEntry], error: &DomainError) -> String {
        let DomainError::Validation { field, reason } = error else {
            return field_message(error);
        };
        match (*field, reason) {
            ("steps", ValidationReason::Empty) => "결재자를 지정해 주세요.".into(),
            ("steps", ValidationReason::TooLong { .. }) => {
                format!("결재자는 최대 {MAX_STEPS}명까지 지정할 수 있습니다.")
            }
            ("steps", ValidationReason::Duplicate) => "중복된 결재자가 있습니다.".into(),
            ("steps", ValidationReason::Inconsistent) => {
                if steps.iter().any(|step| step.approver_id == drafter) {
                    "본인은 결재자로 지정할 수 없습니다.".into()
                } else {
                    "마지막 단계는 결재여야 합니다.".into()
                }
            }
            ("references", ValidationReason::TooLong { .. }) => {
                format!("참조자는 최대 {MAX_REFERENCES}명까지 지정할 수 있습니다.")
            }
            ("references", ValidationReason::Duplicate) => "중복된 참조자가 있습니다.".into(),
            ("references", ValidationReason::Inconsistent) => {
                "본인이나 결재자는 참조자로 지정할 수 없습니다.".into()
            }
            _ => field_message(error),
        }
    }

    /// 보내기 전 검증. 서버와 같은 규칙(`Title`, `Body`, `ApprovalLine::new`)을 그대로 부른다.
    pub fn validate(
        drafter: MemberId,
        title: &str,
        body: &str,
        steps: &[ApproverEntry],
        references: &[ReferenceDraft],
    ) -> Problems {
        let mut problems = Problems::default();
        if let Err(error) = Title::parse(title) {
            problems.title = field_message(&error);
        }
        if let Err(error) = Body::parse(body) {
            problems.body = field_message(&error);
        }
        let line_steps = steps
            .iter()
            .map(|step| LineStep {
                approver: step.approver_id,
                kind: step.kind,
            })
            .collect();
        let refs = references.iter().map(|r| r.member_id).collect();
        if let Err(error) = ApprovalLine::new(drafter, line_steps, refs) {
            let message = line_message(drafter, steps, &error);
            match &error {
                DomainError::Validation {
                    field: "references",
                    ..
                } => problems.references = message,
                _ => problems.line = message,
            }
        }
        problems
    }

    /// 이 화면에서 기안할 수 있는 양식: 사용 중인 일반 양식. 시스템 양식(휴가 등)은 그 기능에서 만든다.
    pub fn draftable_forms(forms: &[FormDto]) -> Vec<FormDto> {
        forms
            .iter()
            .filter(|form| form.kind == DocumentKind::General && form.active)
            .cloned()
            .collect()
    }

    /// 양식을 고를 때 본문을 양식의 본문 틀로 채울지. 사용자가 쓴 내용은 덮어쓰지 않는다.
    pub fn prefill_body(current: &str, touched: bool, template: &str) -> Option<String> {
        (!touched || current.trim().is_empty()).then(|| template.to_owned())
    }

    pub fn create_request(
        form_id: FormId,
        title: &str,
        body: &str,
        steps: &[ApproverEntry],
        references: &[ReferenceDraft],
        submit: bool,
    ) -> CreateDocumentRequest {
        CreateDocumentRequest {
            form_id,
            title: title.to_owned(),
            body: body.to_owned(),
            steps: to_inputs(steps),
            references: references.iter().map(|r| r.member_id).collect(),
            submit,
        }
    }

    pub fn update_request(
        version: i64,
        title: &str,
        body: &str,
        steps: &[ApproverEntry],
        references: &[ReferenceDraft],
    ) -> UpdateDocumentRequest {
        UpdateDocumentRequest {
            version,
            title: title.to_owned(),
            body: body.to_owned(),
            steps: to_inputs(steps),
            references: references.iter().map(|r| r.member_id).collect(),
        }
    }

    /// 참조자 선택기에서 뺄 사람: 본인, 결재자, 이미 고른 참조자.
    pub fn reference_exclude(
        drafter: MemberId,
        steps: &[ApproverEntry],
        references: &[ReferenceDraft],
    ) -> Vec<MemberId> {
        std::iter::once(drafter)
            .chain(steps.iter().map(|s| s.approver_id))
            .chain(references.iter().map(|r| r.member_id))
            .collect()
    }

    /// 참조자를 더한다. 본인·결재자·중복·최대 인원 초과는 무시한다.
    pub fn add_reference(
        references: &[ReferenceDraft],
        drafter: MemberId,
        steps: &[ApproverEntry],
        member_id: MemberId,
        name: String,
    ) -> Vec<ReferenceDraft> {
        let mut out = references.to_vec();
        let blocked = reference_exclude(drafter, steps, references).contains(&member_id);
        if !blocked && out.len() < MAX_REFERENCES {
            out.push(ReferenceDraft { member_id, name });
        }
        out
    }

    pub fn remove_reference(
        references: &[ReferenceDraft],
        member_id: MemberId,
    ) -> Vec<ReferenceDraft> {
        references
            .iter()
            .filter(|r| r.member_id != member_id)
            .cloned()
            .collect()
    }

    /// 결재자로 옮겨진 사람은 참조에서 뺀다(한 사람은 한 자리만 가진다).
    pub fn prune_references(
        references: &[ReferenceDraft],
        steps: &[ApproverEntry],
    ) -> Vec<ReferenceDraft> {
        references
            .iter()
            .filter(|r| !steps.iter().any(|s| s.approver_id == r.member_id))
            .cloned()
            .collect()
    }

    pub fn draft_steps(detail: &DocumentDetail) -> Vec<ApproverEntry> {
        detail
            .steps
            .iter()
            .map(|step| ApproverEntry {
                approver_id: step.approver_id,
                approver_name: step.approver_name.clone(),
                kind: step.kind,
            })
            .collect()
    }

    pub fn draft_references(detail: &DocumentDetail) -> Vec<ReferenceDraft> {
        detail
            .references
            .iter()
            .map(|r| ReferenceDraft {
                member_id: r.member_id,
                name: r.name.clone(),
            })
            .collect()
    }
}

use logic::Problems;

/// 지금 진행 중인 저장 동작.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Saving {
    Draft,
    Submit,
}

#[component]
pub fn ApprovalNew() -> Element {
    rsx! {
        PageHeader { title: "새 결재 문서" }
        DocumentForm {}
    }
}

/// 문서 작성·수정 폼.
///
/// - `draft`가 없으면 새 문서: 저장하면 상세 화면으로 간다.
/// - `draft`가 있으면 그 임시저장 문서를 고친다: `PATCH`(버전 포함), 상신은 저장 뒤 `submit`.
///   끝나면 `ondone`을 부르고, 부모(상세 화면)가 문서를 다시 읽는다.
#[component]
pub fn DocumentForm(
    #[props(default)] draft: Option<DocumentDetail>,
    #[props(default)] ondone: EventHandler<()>,
    #[props(default)] oncancel: EventHandler<()>,
) -> Element {
    let session = use_session();
    let nav = navigator();
    let mut form_id = use_signal(|| {
        draft
            .as_ref()
            .map(|d| d.form_id.to_string())
            .unwrap_or_default()
    });
    let mut title = use_signal(|| draft.as_ref().map(|d| d.title.clone()).unwrap_or_default());
    let mut body = use_signal(|| draft.as_ref().map(|d| d.body.clone()).unwrap_or_default());
    // 사용자가 본문을 직접 고쳤는지. 고쳤으면 양식을 바꿔도 본문을 덮어쓰지 않는다.
    let mut touched = use_signal(|| draft.is_some());
    let mut steps = use_signal(|| draft.as_ref().map(logic::draft_steps).unwrap_or_default());
    let mut references = use_signal(|| {
        draft
            .as_ref()
            .map(logic::draft_references)
            .unwrap_or_default()
    });
    let mut attempted = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut saving = use_signal(|| None::<Saving>);
    // 생성 요청의 멱등성 키. 폼을 열 때 만들고, 저장에 성공하면 새로 만든다.
    let mut key = use_signal(api::new_idempotency_key);
    let forms = use_resource(|| async { api::get::<Vec<FormDto>>("/approval/forms").await });

    let editing: Option<(DocumentId, i64)> = draft.as_ref().map(|d| (d.id, d.version));
    let is_edit = editing.is_some();

    // 새 문서는 첫 번째 양식을 기본으로 고르고, 본문이 비어 있으면 그 양식의 본문 틀로 채운다.
    use_effect(move || {
        if is_edit {
            return;
        }
        if let Some(Ok(list)) = &*forms.read() {
            if form_id.peek().is_empty() {
                if let Some(first) = logic::draftable_forms(list).first() {
                    form_id.set(first.id.to_string());
                    let fill =
                        logic::prefill_body(&body.peek(), *touched.peek(), &first.body_template);
                    if let Some(next) = fill {
                        body.set(next);
                    }
                }
            }
        }
    });

    let me = session.me();
    let Some(drafter) = me.as_ref().map(|me| me.id) else {
        return rsx! {};
    };

    let draftable = match &*forms.read() {
        Some(Ok(list)) => logic::draftable_forms(list),
        _ => Vec::new(),
    };
    let forms_failed = matches!(&*forms.read(), Some(Err(_)));
    let form_options: Vec<(String, String)> = draftable
        .iter()
        .map(|form| (form.id.to_string(), form.name.clone()))
        .collect();
    let selected_description = draftable
        .iter()
        .find(|form| form.id.to_string() == form_id())
        .map(|form| form.description.clone())
        .unwrap_or_default();

    let problems = if attempted() {
        logic::validate(drafter, &title(), &body(), &steps(), &references())
    } else {
        Problems::default()
    };
    let current_saving = saving();

    let save = move |submit: bool| async move {
        if saving().is_some() {
            return;
        }
        attempted.set(true);
        let found = logic::validate(drafter, &title(), &body(), &steps(), &references());
        if !found.is_empty() {
            error.set("입력 내용을 확인해 주세요.".into());
            return;
        }
        error.set(String::new());
        saving.set(Some(if submit {
            Saving::Submit
        } else {
            Saving::Draft
        }));
        let done_message = if submit {
            "상신했습니다."
        } else {
            "임시 저장했습니다."
        };
        match editing {
            None => {
                let Ok(form) = FormId::parse(&form_id()) else {
                    error.set("양식을 골라 주세요.".into());
                    saving.set(None);
                    return;
                };
                let request =
                    logic::create_request(form, &title(), &body(), &steps(), &references(), submit);
                let result =
                    api::post_once::<_, DocumentDetail>("/approval/documents", &request, &key())
                        .await;
                saving.set(None);
                match result {
                    Ok(detail) => {
                        key.set(api::new_idempotency_key());
                        report_success(done_message);
                        nav.push(Route::ApprovalDetail {
                            id: detail.id.to_string(),
                        });
                    }
                    Err(failure) => error.set(failure.message()),
                }
            }
            Some((id, version)) => {
                let request =
                    logic::update_request(version, &title(), &body(), &steps(), &references());
                let path = format!("/approval/documents/{id}");
                // 실패 시 저장(PATCH)까지는 됐는지 함께 돌려준다.
                let result = match api::patch::<_, DocumentDetail>(&path, &request).await {
                    Err(failure) => Err((failure, false)),
                    Ok(_) if submit => {
                        api::post::<_, DocumentDetail>(&format!("{path}/submit"), &Empty {})
                            .await
                            .map(|_| ())
                            .map_err(|failure| (failure, true))
                    }
                    Ok(_) => Ok(()),
                };
                saving.set(None);
                match result {
                    Ok(()) => {
                        report_success(done_message);
                        ondone.call(());
                    }
                    // 저장은 됐지만 상신에 실패했거나, 다른 곳에서 먼저 고친 경우: 알리고 다시 읽는다.
                    Err((failure, saved)) if saved || failure.code == "version_conflict" => {
                        report_error(&failure);
                        ondone.call(());
                    }
                    Err((failure, _)) => error.set(failure.message()),
                }
            }
        }
    };

    rsx! {
        div { class: "form doc-form",
            FormError { message: error() }
            Card { title: "문서",
                div { class: "form",
                    if let Some(detail) = &draft {
                        div { class: "field",
                            span { class: "field-label", "양식" }
                            p { class: "field-static", "{detail.form_name}" }
                        }
                    } else if forms_failed {
                        p { class: "form-error", role: "alert", "양식을 불러오지 못했습니다. 새로 고쳐 주세요." }
                    } else if draftable.is_empty() && forms.read().is_some() {
                        p { class: "form-error", role: "alert", "기안할 수 있는 양식이 없습니다. 관리자에게 문의해 주세요." }
                    } else {
                        SelectField {
                            label: "양식",
                            value: form_id(),
                            options: form_options,
                            required: true,
                            onchange: move |value: String| {
                                let template = match &*forms.read() {
                                    Some(Ok(list)) => list
                                        .iter()
                                        .find(|form| form.id.to_string() == value)
                                        .map(|form| form.body_template.clone()),
                                    _ => None,
                                };
                                form_id.set(value);
                                if let Some(template) = template {
                                    if let Some(next) = logic::prefill_body(&body(), touched(), &template) {
                                        body.set(next);
                                        touched.set(false);
                                    }
                                }
                            },
                        }
                        if !selected_description.is_empty() {
                            p { class: "field-hint", "{selected_description}" }
                        }
                    }
                    div { class: "field-with-count",
                        TextField {
                            label: "제목",
                            value: title(),
                            required: true,
                            maxlength: Title::RULE.max_chars as i64,
                            error: problems.title.clone(),
                            oninput: move |value| title.set(value),
                        }
                        CharCount { count: title().chars().count(), max: Title::RULE.max_chars }
                    }
                    div { class: "field-with-count",
                        TextArea {
                            label: "내용",
                            value: body(),
                            rows: 12,
                            required: true,
                            maxlength: Body::RULE.max_chars as i64,
                            oninput: move |value| {
                                body.set(value);
                                touched.set(true);
                            },
                        }
                        div { class: "field-foot",
                            if problems.body.is_empty() {
                                span {}
                            } else {
                                span { class: "field-error", role: "alert", "{problems.body}" }
                            }
                            CharCount { count: body().chars().count(), max: Body::RULE.max_chars }
                        }
                    }
                }
            }
            Card { title: "결재선",
                ApprovalLineEditor {
                    steps: steps(),
                    exclude: vec![drafter],
                    error: problems.line.clone(),
                    onchange: move |next: Vec<ApproverEntry>| {
                        references.set(logic::prune_references(&references(), &next));
                        steps.set(next);
                    },
                }
            }
            Card { title: "참조",
                if references().is_empty() {
                    p { class: "field-hint", "참조자는 문서를 열람만 합니다. 없어도 됩니다." }
                } else {
                    ul { class: "chips",
                        for reference in references() {
                            li { key: "{reference.member_id}", class: "chip",
                                span { "{reference.name}" }
                                button {
                                    class: "icon-btn chip-remove",
                                    r#type: "button",
                                    "aria-label": "{reference.name} 참조 빼기",
                                    onclick: move |_| {
                                        references.set(logic::remove_reference(&references(), reference.member_id));
                                    },
                                    Icon { name: IconName::Close }
                                }
                            }
                        }
                    }
                }
                if references().len() < pango_core::approval::MAX_REFERENCES {
                    MemberPicker {
                        label: "참조자 추가",
                        exclude: logic::reference_exclude(drafter, &steps(), &references()),
                        onpick: move |member: pango_api::org::MemberSummary| {
                            references.set(logic::add_reference(&references(), drafter, &steps(), member.id, member.name));
                        },
                    }
                }
                if !problems.references.is_empty() {
                    p { class: "field-error", role: "alert", "{problems.references}" }
                }
            }
            div { class: "form-actions",
                if is_edit {
                    Button {
                        kind: ButtonKind::Ghost,
                        disabled: current_saving.is_some(),
                        onclick: move |_| oncancel.call(()),
                        "취소"
                    }
                } else {
                    Link { class: "btn btn-ghost", to: Route::ApprovalList {}, "취소" }
                }
                Button {
                    kind: ButtonKind::Secondary,
                    busy: current_saving == Some(Saving::Draft),
                    disabled: current_saving.is_some(),
                    onclick: move |_| {
                        spawn(save(false));
                    },
                    "임시 저장"
                }
                Button {
                    busy: current_saving == Some(Saving::Submit),
                    disabled: current_saving.is_some(),
                    onclick: move |_| {
                        spawn(save(true));
                    },
                    "상신"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::logic::*;
    use pango_api::approval::{DocumentDetail, FormDto, ReferenceDto, StepDto};
    use pango_core::approval::{
        DocumentKind, DocumentStatus, StepKind, StepStatus, MAX_REFERENCES, MAX_STEPS,
    };
    use pango_core::id::{DocumentId, FormId, MemberId};
    use pango_core::time::Timestamp;

    use crate::ui::ApproverEntry;

    fn step(kind: StepKind) -> ApproverEntry {
        ApproverEntry {
            approver_id: MemberId::new(),
            approver_name: "결재자".into(),
            kind,
        }
    }

    fn reference() -> ReferenceDraft {
        ReferenceDraft {
            member_id: MemberId::new(),
            name: "참조자".into(),
        }
    }

    #[test]
    fn valid_input_has_no_problems() {
        let me = MemberId::new();
        let problems = validate(
            me,
            "제목",
            "내용",
            &[step(StepKind::Agree), step(StepKind::Approve)],
            &[reference()],
        );
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn line_rules_come_from_the_core_validator() {
        let me = MemberId::new();
        let check =
            |steps: &[ApproverEntry], refs: &[ReferenceDraft]| validate(me, "t", "b", steps, refs);

        assert_eq!(check(&[], &[]).line, "결재자를 지정해 주세요.");
        assert_eq!(
            check(&[step(StepKind::Approve), step(StepKind::Agree)], &[]).line,
            "마지막 단계는 결재여야 합니다."
        );
        let mine = ApproverEntry {
            approver_id: me,
            ..step(StepKind::Approve)
        };
        assert_eq!(
            check(&[mine], &[]).line,
            "본인은 결재자로 지정할 수 없습니다."
        );
        let twice = step(StepKind::Approve);
        assert_eq!(
            check(&[twice.clone(), twice], &[]).line,
            "중복된 결재자가 있습니다."
        );
        let many: Vec<_> = (0..=MAX_STEPS).map(|_| step(StepKind::Approve)).collect();
        assert!(check(&many, &[]).line.contains("최대"));

        let approver = step(StepKind::Approve);
        let as_reference = ReferenceDraft {
            member_id: approver.approver_id,
            name: "x".into(),
        };
        let problems = check(&[approver], &[as_reference]);
        assert!(problems.line.is_empty());
        assert_eq!(
            problems.references,
            "본인이나 결재자는 참조자로 지정할 수 없습니다."
        );
        let r = reference();
        assert_eq!(
            check(&[step(StepKind::Approve)], &[r.clone(), r]).references,
            "중복된 참조자가 있습니다."
        );
    }

    #[test]
    fn title_and_body_use_text_rules() {
        let me = MemberId::new();
        let ok = [step(StepKind::Approve)];
        let problems = validate(me, "  ", "", &ok, &[]);
        assert_eq!(problems.title, "제목을(를) 입력해 주세요.");
        assert_eq!(problems.body, "내용을(를) 입력해 주세요.");
        let long = "가".repeat(pango_core::text::Title::RULE.max_chars + 1);
        assert_eq!(
            validate(me, &long, "b", &ok, &[]).title,
            "제목이(가) 너무 깁니다."
        );
        assert!(validate(me, "a\u{0007}", "b", &ok, &[])
            .title
            .contains("쓸 수 없는"));
        assert!(validate(me, "t", "줄\n바꿈\t허용", &ok, &[]).is_empty());
    }

    fn form(kind: DocumentKind, active: bool, template: &str) -> FormDto {
        FormDto {
            id: FormId::new(),
            code: "c".into(),
            name: "n".into(),
            description: String::new(),
            kind,
            body_template: template.into(),
            active,
            sort_order: 0,
            version: 1,
        }
    }

    #[test]
    fn only_active_general_forms_are_draftable() {
        let forms = vec![
            form(DocumentKind::General, true, ""),
            form(DocumentKind::General, false, ""),
            form(DocumentKind::Leave, true, ""),
            form(DocumentKind::AttendanceCorrection, true, ""),
        ];
        let draftable = draftable_forms(&forms);
        assert_eq!(draftable, vec![forms[0].clone()]);
        assert!(draftable_forms(&[]).is_empty());
    }

    #[test]
    fn template_fills_only_untouched_or_empty_bodies() {
        assert_eq!(prefill_body("", false, "틀"), Some("틀".into()));
        assert_eq!(
            prefill_body("이전 틀", false, "새 틀"),
            Some("새 틀".into())
        );
        assert_eq!(prefill_body("  \n", true, "틀"), Some("틀".into()));
        assert_eq!(prefill_body("내가 쓴 글", true, "틀"), None);
    }

    #[test]
    fn requests_carry_line_and_references() {
        let steps = [step(StepKind::Agree), step(StepKind::Approve)];
        let refs = [reference()];
        let form_id = FormId::new();
        let create = create_request(form_id, "t", "b", &steps, &refs, true);
        assert!(create.submit);
        assert_eq!(create.form_id, form_id);
        assert_eq!(create.steps.len(), 2);
        assert_eq!(create.steps[0].kind, StepKind::Agree);
        assert_eq!(create.references, vec![refs[0].member_id]);
        let update = update_request(7, "t", "b", &steps, &refs);
        assert_eq!(update.version, 7);
        assert_eq!(update.steps, create.steps);
    }

    #[test]
    fn references_cannot_overlap_the_line() {
        let me = MemberId::new();
        let approver = step(StepKind::Approve);
        let steps = [approver.clone()];
        let r = reference();
        let one = add_reference(&[], me, &steps, r.member_id, r.name.clone());
        assert_eq!(one, vec![r.clone()]);
        assert_eq!(
            add_reference(&one, me, &steps, r.member_id, "x".into()),
            one
        );
        assert_eq!(add_reference(&one, me, &steps, me, "me".into()), one);
        assert_eq!(
            add_reference(&one, me, &steps, approver.approver_id, "a".into()),
            one
        );
        let full: Vec<_> = (0..MAX_REFERENCES).map(|_| reference()).collect();
        assert_eq!(
            add_reference(&full, me, &steps, MemberId::new(), "n".into()).len(),
            MAX_REFERENCES
        );
        assert!(remove_reference(&one, r.member_id).is_empty());
        assert_eq!(remove_reference(&one, MemberId::new()), one);

        let moved = ApproverEntry {
            approver_id: r.member_id,
            ..step(StepKind::Approve)
        };
        assert!(prune_references(&one, &[moved]).is_empty());
        assert_eq!(prune_references(&one, &steps), one);
        assert_eq!(
            reference_exclude(me, &steps, &one),
            vec![me, approver.approver_id, r.member_id]
        );
    }

    #[test]
    fn drafts_are_loaded_into_the_editor() {
        let approver = MemberId::new();
        let referee = MemberId::new();
        let at = Timestamp::from_millis(0).expect("ts");
        let detail = DocumentDetail {
            id: DocumentId::new(),
            doc_no: None,
            form_id: FormId::new(),
            form_name: "품의서".into(),
            kind: DocumentKind::General,
            status: DocumentStatus::Draft,
            drafter_id: MemberId::new(),
            drafter_name: "기안자".into(),
            drafter_department_name: None,
            title: "t".into(),
            body: "b".into(),
            payload: None,
            steps: vec![StepDto {
                order: 1,
                approver_id: approver,
                approver_name: "결재자".into(),
                kind: StepKind::Agree,
                status: StepStatus::Waiting,
                acted_at: None,
                comment: None,
            }],
            references: vec![ReferenceDto {
                member_id: referee,
                name: "참조자".into(),
                read_at: None,
            }],
            events: vec![],
            submitted_at: None,
            completed_at: None,
            created_at: at,
            version: 3,
            my_turn: false,
        };
        let steps = draft_steps(&detail);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].approver_id, approver);
        assert_eq!(steps[0].kind, StepKind::Agree);
        assert_eq!(draft_references(&detail)[0].member_id, referee);
    }
}
