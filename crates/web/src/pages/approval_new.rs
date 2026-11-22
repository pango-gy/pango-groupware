//! 결재 문서 작성: 양식 고르기 → 양식의 필드·본문·결재선을 채워 임시저장하거나 상신한다.
//! 같은 편집기로 임시저장 문서도 고친다(상세 화면의 "수정").

use dioxus::prelude::*;
use pango_api::approval::{DocumentDetail, FormGroup};
use pango_api::Empty;
use pango_core::id::DocumentId;
use pango_core::text::{Body, Title};
use serde_json::{Map, Value};

use crate::api;
use crate::route::Route;
use crate::session::use_session;
use crate::ui::form_fields::{focus_element, focus_field, logic as fields};
use crate::ui::form_picker::logic::{destination, Destination};
use crate::ui::{
    report_error, report_success, resource_view, today, ApprovalLineEditor, ApproverEntry, Badge,
    Button, ButtonKind, Card, CharCount, EmptyState, FieldErrors, FormError, FormFields,
    FormPicker, Icon, IconName, MemberPicker, PageHeader, PickedForm, TextArea, TextField, Tone,
};

/// 작성 폼의 순수 함수: 검증, 요청 변환, 참조자 편집.
pub(crate) mod logic {
    use chrono::NaiveDate;
    use pango_api::approval::{DocumentDetail, EditDocumentRequest, FormGroup, NewDocumentRequest};
    use pango_core::approval::{ApprovalLine, LineStep, MAX_REFERENCES, MAX_STEPS};
    use pango_core::error::{DomainError, ValidationReason};
    use pango_core::form_schema::FormSchema;
    use pango_core::id::{FormId, MemberId};
    use pango_core::text::{Body, Title};
    use serde_json::Value;

    use crate::ui::form_fields::logic::{self as fields, Values};
    use crate::ui::form_picker::logic as picker;
    use crate::ui::PickedForm;

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
                format!("결재자는 최대 {MAX_STEPS}명까지 지정할 수 있어요.")
            }
            ("steps", ValidationReason::Duplicate) => "중복된 결재자가 있어요.".into(),
            ("steps", ValidationReason::Inconsistent) => {
                if steps.iter().any(|step| step.approver_id == drafter) {
                    "본인은 결재자로 지정할 수 없어요.".into()
                } else {
                    "마지막 단계는 결재여야 해요.".into()
                }
            }
            ("references", ValidationReason::TooLong { .. }) => {
                format!("참조자는 최대 {MAX_REFERENCES}명까지 지정할 수 있어요.")
            }
            ("references", ValidationReason::Duplicate) => "중복된 참조자가 있어요.".into(),
            ("references", ValidationReason::Inconsistent) => {
                "본인이나 결재자는 참조자로 지정할 수 없어요.".into()
            }
            _ => field_message(error),
        }
    }

    /// 보내기 전 검증. 서버와 같은 규칙(`Title`, `Body`, `ApprovalLine::new`)을 그대로 부른다.
    pub fn validate(
        drafter: MemberId,
        title: &str,
        body: &str,
        use_body: bool,
        steps: &[ApproverEntry],
        references: &[ReferenceDraft],
    ) -> Problems {
        let mut problems = Problems::default();
        if let Err(error) = Title::parse(title) {
            problems.title = field_message(&error);
        }
        // 본문을 쓰지 않는 양식은 본문 없이 필드만으로 문서를 낸다.
        if use_body {
            if let Err(error) = Body::parse(body) {
                problems.body = field_message(&error);
            }
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

    /// 문서를 쓸 양식. 새 문서는 고른 양식, 임시저장 문서는 지금의 양식 정의(없으면 저장 당시의 것).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct EditorForm {
        pub id: FormId,
        pub name: String,
        pub category_label: String,
        pub description: String,
        pub use_body: bool,
        pub body_template: String,
        pub schema: FormSchema,
    }

    pub fn editor_form(picked: &PickedForm) -> EditorForm {
        EditorForm {
            id: picked.form.id,
            name: picked.form.name.clone(),
            category_label: picked.category_label.clone(),
            description: picked.form.description.clone(),
            use_body: picked.form.use_body,
            body_template: picked.form.body_template.clone(),
            schema: picked.form.schema.clone(),
        }
    }

    /// 임시저장 문서를 고칠 때의 양식. 상신할 때 서버가 **지금의** 정의로 다시 검증하므로 지금
    /// 정의를 쓰고, 양식이 꺼져 목록에 없으면 문서가 갖고 있는 정의로 보여 준다(그 경우 상신은
    /// 서버가 `form_inactive`로 막는다). 필드 정의가 없던 때의 문서는 본문만 있는 양식이다.
    pub fn editor_form_for_draft(detail: &DocumentDetail, groups: &[FormGroup]) -> EditorForm {
        if let Some(picked) = picker::pick(groups, &detail.form_id.to_string()) {
            return editor_form(&picked);
        }
        let snapshot = detail.form_snapshot.as_ref();
        EditorForm {
            id: detail.form_id,
            name: detail.form_name.clone(),
            category_label: snapshot
                .map(|snapshot| snapshot.category_label.clone())
                .unwrap_or_default(),
            description: String::new(),
            use_body: snapshot.is_none_or(|snapshot| snapshot.use_body),
            body_template: String::new(),
            schema: snapshot
                .map(|snapshot| snapshot.schema.clone())
                .unwrap_or_default(),
        }
    }

    /// 임시저장 문서에 저장된 값. 지금 양식에 없는 키는 버린다.
    pub fn draft_values(detail: &DocumentDetail, schema: &FormSchema) -> Values {
        detail
            .form_snapshot
            .as_ref()
            .map(|snapshot| fields::prune(schema, &snapshot.values))
            .unwrap_or_default()
    }

    /// 새 문서의 기본 제목: `지출결의서 - 2026-11-20`. 제목 길이 제한을 넘으면 양식 이름을 줄인다.
    pub fn default_title(form_name: &str, today: NaiveDate) -> String {
        let suffix = format!(" - {today}");
        let room = Title::RULE.max_chars.saturating_sub(suffix.chars().count());
        let name: String = form_name.trim().chars().take(room).collect();
        format!("{}{suffix}", name.trim_end())
    }

    /// 요청에 실을 값. 필드가 없는 양식이면 보내지 않는다.
    pub fn request_values(schema: &FormSchema, values: &Values) -> Option<Value> {
        (!schema.fields.is_empty()).then(|| Value::Object(fields::prune(schema, values)))
    }

    /// 본문을 쓰지 않는 양식이면 본문을 비워 보낸다.
    fn request_body(form: &EditorForm, body: &str) -> String {
        if form.use_body {
            body.to_owned()
        } else {
            String::new()
        }
    }

    pub fn create_request(
        form: &EditorForm,
        title: &str,
        body: &str,
        values: &Values,
        steps: &[ApproverEntry],
        references: &[ReferenceDraft],
        submit: bool,
    ) -> NewDocumentRequest {
        NewDocumentRequest {
            form_id: form.id,
            title: title.to_owned(),
            body: request_body(form, body),
            values: request_values(&form.schema, values),
            steps: to_inputs(steps),
            references: references.iter().map(|r| r.member_id).collect(),
            submit,
        }
    }

    pub fn update_request(
        form: &EditorForm,
        version: i64,
        title: &str,
        body: &str,
        values: &Values,
        steps: &[ApproverEntry],
        references: &[ReferenceDraft],
    ) -> EditDocumentRequest {
        EditDocumentRequest {
            version,
            title: title.to_owned(),
            body: request_body(form, body),
            values: request_values(&form.schema, values),
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

use logic::{EditorForm, Problems};

/// 지금 진행 중인 저장 동작.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Saving {
    Draft,
    Submit,
}

/// 새 문서. 먼저 양식을 고르고(시스템 양식이면 그 기능의 화면으로 간다), 고른 양식으로 편집기를 연다.
#[component]
pub fn ApprovalNew() -> Element {
    let session = use_session();
    let nav = navigator();
    let forms = use_resource(|| async { api::get::<Vec<FormGroup>>("/approval/forms").await });
    let mut picked = use_signal(|| None::<PickedForm>);
    let mut picking = use_signal(|| true);

    let Some(member) = session.me().map(|me| me.id.to_string()) else {
        return rsx! {};
    };
    let choose = move |form: PickedForm| {
        picking.set(false);
        match destination(form.form.route) {
            Destination::Editor => picked.set(Some(form)),
            Destination::LeaveNew => {
                nav.push(Route::LeaveNew {});
            }
            Destination::Attendance => {
                nav.push(Route::Attendance {});
            }
        }
    };

    rsx! {
        PageHeader {
            title: "새 결재 문서",
            description: "양식을 고르고 내용을 채워 상신해요.",
            actions: rsx! {
                Button { kind: ButtonKind::Weak, onclick: move |_| picking.set(true),
                    if picked().is_some() { "양식 변경" } else { "양식 고르기" }
                }
            },
        }
        if picked().is_none() {
            EmptyState {
                title: "양식을 먼저 골라 주세요.",
                hint: "양식에 따라 입력할 항목이 달라져요.",
                icon: IconName::Document,
                Button { kind: ButtonKind::Weak, onclick: move |_| picking.set(true), "양식 고르기" }
            }
        }
        // 양식이 바뀌면 편집기를 새로 만든다(이전 양식의 값이 섞이지 않게).
        for form in picked().into_iter() {
            DocumentEditor { key: "{form.form.id}", form: logic::editor_form(&form) }
        }
        if picking() {
            {resource_view(forms, move |groups: Vec<FormGroup>| rsx! {
                FormPicker {
                    groups,
                    member: member.clone(),
                    onpick: choose,
                    onclose: move |_| picking.set(false),
                }
            })}
        }
    }
}

/// 임시저장 문서 고치기. 지금의 양식 정의를 읽어 편집기를 연다.
///
/// 저장은 `PATCH`(버전 포함), 상신은 저장 뒤 `submit`. 끝나면 `ondone`을 부르고, 부모(상세 화면)가
/// 문서를 다시 읽는다.
#[component]
pub fn DocumentForm(
    draft: DocumentDetail,
    #[props(default)] ondone: EventHandler<()>,
    #[props(default)] oncancel: EventHandler<()>,
) -> Element {
    let forms = use_resource(|| async { api::get::<Vec<FormGroup>>("/approval/forms").await });
    resource_view(forms, move |groups: Vec<FormGroup>| {
        let form = logic::editor_form_for_draft(&draft, &groups);
        rsx! {
            DocumentEditor { form, draft: draft.clone(), ondone, oncancel }
        }
    })
}

/// 문서 편집기.
///
/// - `draft`가 없으면 새 문서: 저장하면 상세 화면으로 간다.
/// - `draft`가 있으면 그 임시저장 문서를 고친다.
///
/// 보내기 전에 서버와 같은 규칙으로 검증해 모든 오류를 한 번에 보여 주고, 첫 오류로 화면을 옮긴다.
#[component]
fn DocumentEditor(
    form: EditorForm,
    #[props(default)] draft: Option<DocumentDetail>,
    #[props(default)] ondone: EventHandler<()>,
    #[props(default)] oncancel: EventHandler<()>,
) -> Element {
    let session = use_session();
    let nav = navigator();
    let mut title = use_signal(|| match &draft {
        Some(detail) => detail.title.clone(),
        None => logic::default_title(&form.name, today(session.zone())),
    });
    let mut body = use_signal(|| match &draft {
        Some(detail) => detail.body.clone(),
        None => form.body_template.clone(),
    });
    let mut values = use_signal(|| {
        draft
            .as_ref()
            .map(|detail| logic::draft_values(detail, &form.schema))
            .unwrap_or_default()
    });
    let mut steps = use_signal(|| draft.as_ref().map(logic::draft_steps).unwrap_or_default());
    let mut references = use_signal(|| {
        draft
            .as_ref()
            .map(logic::draft_references)
            .unwrap_or_default()
    });
    let mut attempted = use_signal(|| false);
    // 서버가 돌려준 필드 오류(구성원이 재직 중이 아님 등 화면에서 알 수 없는 것).
    let mut server_errors = use_signal(FieldErrors::default);
    let mut error = use_signal(String::new);
    let mut saving = use_signal(|| None::<Saving>);
    // 생성 요청의 멱등성 키. 폼을 열 때 만들고, 저장에 성공하면 새로 만든다.
    let mut key = use_signal(api::new_idempotency_key);
    // 저장에 쓰는 양식. 이 편집기는 양식마다 새로 만들어지므로 처음 값 그대로다.
    let form_ref = use_hook(|| CopyValue::new(form.clone()));

    let editing: Option<(DocumentId, i64)> = draft.as_ref().map(|d| (d.id, d.version));
    // 첨부파일. 임시저장 문서를 고치는 중이면 고르는 즉시 올리고, 새 문서면 모아 두었다가
    // 임시저장으로 만든 뒤 올리고 나서 상신한다(상신한 문서의 첨부는 바꿀 수 없다).
    let uploaded = use_signal(|| {
        draft
            .as_ref()
            .map(|detail| detail.attachments.clone())
            .unwrap_or_default()
    });
    let mut pending = use_signal(Vec::<crate::ui::attachments::PendingFile>::new);
    let is_edit = editing.is_some();
    let member_names = draft
        .as_ref()
        .and_then(|detail| detail.form_snapshot.as_ref())
        .map(|snapshot| snapshot.member_names.clone())
        .unwrap_or_default();

    let me = session.me();
    let Some(drafter) = me.as_ref().map(|me| me.id) else {
        return rsx! {};
    };

    let (problems, field_errors) = if attempted() {
        let found = fields::validate(&form.schema, &fields::prune(&form.schema, &values()));
        (
            logic::validate(
                drafter,
                &title(),
                &body(),
                form.use_body,
                &steps(),
                &references(),
            ),
            if found.is_empty() {
                server_errors()
            } else {
                found
            },
        )
    } else {
        (Problems::default(), server_errors())
    };
    let current_saving = saving();

    let save = move |submit: bool| async move {
        if saving().is_some() {
            return;
        }
        let form = form_ref.cloned();
        attempted.set(true);
        server_errors.set(FieldErrors::default());
        // 빈 행·빈 칸을 치우고 검증한다(오류의 행 번호가 화면과 맞도록 화면의 값도 같이 바꾼다).
        let cleaned = fields::prune(&form.schema, &values());
        values.set(cleaned.clone());
        let found = logic::validate(
            drafter,
            &title(),
            &body(),
            form.use_body,
            &steps(),
            &references(),
        );
        let field_problems = fields::validate(&form.schema, &cleaned);
        if !found.is_empty() || !field_problems.is_empty() {
            error.set("입력한 내용을 확인해 주세요. 고칠 곳을 표시해 뒀어요.".into());
            if !found.title.is_empty() {
                focus_element("doc-title");
            } else if let Some(first) = field_problems.first_key(&form.schema) {
                focus_field(&first);
            } else if !found.body.is_empty() {
                focus_element("doc-body");
            } else {
                focus_element("doc-line");
            }
            return;
        }
        error.set(String::new());
        saving.set(Some(if submit {
            Saving::Submit
        } else {
            Saving::Draft
        }));
        let done_message = if submit {
            "상신했어요."
        } else {
            "임시 저장했어요."
        };
        // 서버가 필드별 오류를 돌려주면 그 필드에 붙이고 첫 오류로 옮긴다.
        let mut show_failure = move |failure: api::ApiError| {
            if failure.fields.is_empty() {
                error.set(failure.message());
            } else {
                let mapped = FieldErrors::from_api(&failure.fields);
                error.set("입력한 내용을 확인해 주세요. 고칠 곳을 표시해 뒀어요.".into());
                if let Some(first) = mapped.first_key(&form_ref.read().schema) {
                    focus_field(&first);
                }
                server_errors.set(mapped);
            }
        };
        match editing {
            None => {
                // 고른 파일이 있으면 임시저장으로 만들고, 파일을 올린 뒤에 상신한다.
                let files = pending.read().clone();
                let request = logic::create_request(
                    &form,
                    &title(),
                    &body(),
                    &cleaned,
                    &steps(),
                    &references(),
                    submit && files.is_empty(),
                );
                let result =
                    api::post_once::<_, DocumentDetail>("/approval/documents", &request, &key())
                        .await;
                let detail = match result {
                    Ok(detail) => detail,
                    Err(failure) => {
                        saving.set(None);
                        show_failure(failure);
                        return;
                    }
                };
                key.set(api::new_idempotency_key());
                let path = format!("/approval/documents/{}", detail.id);
                let (left, problems) = if files.is_empty() {
                    (Vec::new(), Vec::new())
                } else {
                    crate::ui::attachments::upload_pending(&format!("{path}/attachments"), files)
                        .await
                };
                pending.set(left);
                let outcome = if !problems.is_empty() {
                    Err(format!(
                        "문서는 임시 저장했지만 파일 {}개를 올리지 못했어요. 문서에서 다시 올린 뒤 상신해 주세요.\n{}",
                        problems.len(),
                        problems.join("\n")
                    ))
                } else if submit && detail.status == pango_core::approval::DocumentStatus::Draft {
                    api::post::<_, DocumentDetail>(&format!("{path}/submit"), &Empty {})
                        .await
                        .map(|_| ())
                        .map_err(|failure| {
                            format!(
                                "임시 저장은 했지만 상신하지 못했어요. {}",
                                failure.message()
                            )
                        })
                } else {
                    Ok(())
                };
                saving.set(None);
                match outcome {
                    Ok(()) => report_success(done_message),
                    Err(message) => crate::ui::report_failure(message),
                }
                nav.push(Route::ApprovalDetail {
                    id: detail.id.to_string(),
                });
            }
            Some((id, version)) => {
                let request = logic::update_request(
                    &form,
                    version,
                    &title(),
                    &body(),
                    &cleaned,
                    &steps(),
                    &references(),
                );
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
                    Err((failure, _)) => show_failure(failure),
                }
            }
        }
    };

    rsx! {
        div { class: "page-form",
            FormError { message: error() }
            Card {
                div { class: "doc-form-head",
                    div { class: "row-title",
                        strong { class: "doc-form-name", "{form.name}" }
                        if !form.category_label.is_empty() {
                            Badge { tone: Tone::Neutral, "{form.category_label}" }
                        }
                    }
                    if !form.description.is_empty() {
                        p { class: "text-sub", "{form.description}" }
                    }
                }
                div { class: "form",
                    div { id: "doc-title", class: "field-with-count",
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
                    FormFields {
                        schema: form.schema.clone(),
                        values: values(),
                        errors: field_errors,
                        member_names,
                        onchange: move |next: Map<String, Value>| {
                            values.set(next);
                            if !server_errors.peek().is_empty() {
                                server_errors.set(FieldErrors::default());
                            }
                        },
                    }
                    if form.use_body {
                        div { id: "doc-body", class: "field-with-count",
                            TextArea {
                                label: "내용",
                                value: body(),
                                rows: if form.schema.fields.is_empty() { 12 } else { 6 },
                                required: true,
                                maxlength: Body::RULE.max_chars as i64,
                                oninput: move |value| body.set(value),
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
                    crate::ui::AttachmentEditor {
                        base: editing.map(|(id, _)| format!("/approval/documents/{id}/attachments")),
                        uploaded,
                        pending,
                    }
                }
            }
            Card { title: "결재선",
                div { id: "doc-line",
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
            }
            Card { title: "참조",
                if references().is_empty() {
                    p { class: "field-hint panel-note", "참조자는 문서를 열람만 해요. 없어도 돼요." }
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
                        kind: ButtonKind::Grey,
                        disabled: current_saving.is_some(),
                        onclick: move |_| oncancel.call(()),
                        "취소"
                    }
                } else {
                    Link { class: "btn btn-grey", to: Route::ApprovalList {}, "취소" }
                }
                Button {
                    kind: ButtonKind::Weak,
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
    use chrono::NaiveDate;
    use pango_api::approval::{
        DocumentDetail, FormGroup, FormListItem, FormRoute, FormSnapshot, ReferenceDto, StepDto,
    };
    use pango_core::approval::{
        DocumentKind, DocumentStatus, FormCategory, StepKind, StepStatus, MAX_REFERENCES, MAX_STEPS,
    };
    use pango_core::form_schema::{FieldDef, FieldKind, FormSchema};
    use pango_core::id::{DocumentId, FormId, MemberId};
    use pango_core::text::Title;
    use pango_core::time::Timestamp;
    use serde_json::json;

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
            true,
            &[step(StepKind::Agree), step(StepKind::Approve)],
            &[reference()],
        );
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn line_rules_come_from_the_core_validator() {
        let me = MemberId::new();
        let check = |steps: &[ApproverEntry], refs: &[ReferenceDraft]| {
            validate(me, "t", "b", true, steps, refs)
        };

        assert_eq!(check(&[], &[]).line, "결재자를 지정해 주세요.");
        assert_eq!(
            check(&[step(StepKind::Approve), step(StepKind::Agree)], &[]).line,
            "마지막 단계는 결재여야 해요."
        );
        let mine = ApproverEntry {
            approver_id: me,
            ..step(StepKind::Approve)
        };
        assert_eq!(
            check(&[mine], &[]).line,
            "본인은 결재자로 지정할 수 없어요."
        );
        let twice = step(StepKind::Approve);
        assert_eq!(
            check(&[twice.clone(), twice], &[]).line,
            "중복된 결재자가 있어요."
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
            "본인이나 결재자는 참조자로 지정할 수 없어요."
        );
        let r = reference();
        assert_eq!(
            check(&[step(StepKind::Approve)], &[r.clone(), r]).references,
            "중복된 참조자가 있어요."
        );
    }

    #[test]
    fn title_and_body_use_text_rules() {
        let me = MemberId::new();
        let ok = [step(StepKind::Approve)];
        let problems = validate(me, "  ", "", true, &ok, &[]);
        assert_eq!(problems.title, "제목을(를) 입력해 주세요.");
        assert_eq!(problems.body, "내용을(를) 입력해 주세요.");
        let long = "가".repeat(pango_core::text::Title::RULE.max_chars + 1);
        assert_eq!(
            validate(me, &long, "b", true, &ok, &[]).title,
            "제목이(가) 너무 깁니다."
        );
        assert!(validate(me, "a\u{0007}", "b", true, &ok, &[])
            .title
            .contains("쓸 수 없는"));
        assert!(validate(me, "t", "줄\n바꿈\t허용", true, &ok, &[]).is_empty());
        // 본문을 쓰지 않는 양식은 본문이 비어도 된다.
        assert!(validate(me, "t", "", false, &ok, &[]).is_empty());
        assert!(validate(me, "t", "\u{0007}", false, &ok, &[])
            .body
            .is_empty());
    }

    fn schema() -> FormSchema {
        FormSchema {
            fields: vec![
                FieldDef {
                    key: "amount".into(),
                    label: "금액".into(),
                    kind: FieldKind::Money,
                    required: true,
                    help: None,
                },
                FieldDef {
                    key: "memo".into(),
                    label: "메모".into(),
                    kind: FieldKind::Text { max_len: None },
                    required: false,
                    help: None,
                },
            ],
        }
    }

    fn editor(use_body: bool, schema: FormSchema) -> EditorForm {
        EditorForm {
            id: FormId::new(),
            name: "지출결의서".into(),
            category_label: "회계".into(),
            description: String::new(),
            use_body,
            body_template: String::new(),
            schema,
        }
    }

    fn values(value: serde_json::Value) -> crate::ui::form_fields::logic::Values {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn default_title_names_the_form_and_the_day() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 20).expect("date");
        assert_eq!(default_title("지출결의서", day), "지출결의서 - 2026-11-20");
        assert_eq!(default_title("  품의서 ", day), "품의서 - 2026-11-20");
        assert_eq!(default_title("", day), " - 2026-11-20");
        // 아주 긴 양식 이름도 제목 길이 제한 안에 든다.
        let long = default_title(&"가".repeat(500), day);
        assert_eq!(long.chars().count(), Title::RULE.max_chars);
        assert!(long.ends_with(" - 2026-11-20"));
        assert!(Title::parse(&long).is_ok());
    }

    #[test]
    fn requests_carry_values_line_and_references() {
        let steps = [step(StepKind::Agree), step(StepKind::Approve)];
        let refs = [reference()];
        let form = editor(false, schema());
        let input = values(json!({"amount": 1000, "memo": "  ", "stale": "x"}));
        let create = create_request(&form, "t", "남은 본문", &input, &steps, &refs, true);
        assert!(create.submit);
        assert_eq!(create.form_id, form.id);
        // 본문을 쓰지 않는 양식은 본문을 비워 보내고, 값은 정리해서 보낸다.
        assert_eq!(create.body, "");
        assert_eq!(create.values, Some(json!({"amount": 1000})));
        assert_eq!(create.steps.len(), 2);
        assert_eq!(create.steps[0].kind, StepKind::Agree);
        assert_eq!(create.references, vec![refs[0].member_id]);
        let update = update_request(&form, 7, "t", "b", &input, &steps, &refs);
        assert_eq!(update.version, 7);
        assert_eq!(update.steps, create.steps);
        assert_eq!(update.values, create.values);

        // 필드가 없는 양식은 값을 보내지 않고 본문을 그대로 보낸다.
        let plain = editor(true, FormSchema::default());
        let create = create_request(&plain, "t", "본문", &input, &steps, &refs, false);
        assert_eq!(create.values, None);
        assert_eq!(create.body, "본문");
        assert!(!create.submit);
        assert_eq!(
            request_values(&schema(), &values(json!({}))),
            Some(json!({}))
        );
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
            form_snapshot: Some(FormSnapshot {
                category: FormCategory::Accounting,
                category_label: "회계".into(),
                use_body: false,
                schema_version: 2,
                schema: schema(),
                values: values(json!({"amount": 5000, "removed_field": "x"})),
                computed: serde_json::Map::new(),
                member_names: std::collections::BTreeMap::new(),
            }),
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
            attachments: vec![],
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

        // 양식이 목록에 없으면 문서가 갖고 있는 정의로 연다. 지금 양식에 없는 값은 버린다.
        let stored = editor_form_for_draft(&detail, &[]);
        assert_eq!(stored.id, detail.form_id);
        assert_eq!(stored.name, "품의서");
        assert_eq!(stored.category_label, "회계");
        assert!(!stored.use_body);
        assert_eq!(stored.schema, schema());
        assert_eq!(
            serde_json::Value::Object(draft_values(&detail, &stored.schema)),
            json!({"amount": 5000})
        );

        // 양식이 목록에 있으면 지금의 정의를 쓴다.
        let current = FormListItem {
            id: detail.form_id,
            code: "c".into(),
            name: "새 이름".into(),
            description: "설명".into(),
            kind: DocumentKind::General,
            body_template: "틀".into(),
            use_body: true,
            schema: FormSchema::default(),
            schema_version: 3,
            system: false,
            route: FormRoute::ApprovalNew,
        };
        let groups = [FormGroup {
            category: FormCategory::General,
            category_label: "일반".into(),
            forms: vec![current],
        }];
        let live = editor_form_for_draft(&detail, &groups);
        assert_eq!(
            (live.name.as_str(), live.category_label.as_str()),
            ("새 이름", "일반")
        );
        assert!(live.use_body);
        assert!(draft_values(&detail, &live.schema).is_empty());

        // 필드 정의가 없던 때의 문서는 본문만 있는 양식이다.
        let legacy = DocumentDetail {
            form_snapshot: None,
            ..detail
        };
        let old = editor_form_for_draft(&legacy, &[]);
        assert!(old.use_body && old.schema.fields.is_empty());
        assert!(draft_values(&legacy, &old.schema).is_empty());
    }
}
