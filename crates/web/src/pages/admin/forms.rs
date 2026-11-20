//! 결재 양식 관리와 전체 문서 열람(관리자).

use dioxus::prelude::*;
use pango_api::approval::{DocumentSummary, FormDto};
use pango_api::Page;
use pango_core::id::FormId;
use pango_core::text::{Body, Code, Comment, LabelName};

use crate::api;
use crate::pages::approval::logic::admin_documents_path;
use crate::pages::approval::DocumentRow;
use crate::session::use_session;
use crate::text;
use crate::ui::{
    now, report_error, report_success, resource_view, use_more_pages, Badge, Button, ButtonKind,
    Card, CharCount, Checkbox, EmptyState, FormError, IconName, LoadMore, Modal, PageHeader,
    SelectField, TextArea, TextField, Tone,
};

/// 양식 편집과 문서 필터의 순수 함수.
pub(crate) mod logic {
    use pango_api::approval::{CreateFormRequest, FormDto, UpdateFormRequest};
    use pango_core::approval::{DocumentKind, DocumentStatus};
    use pango_core::text::{self as rules, Body, Code, Comment, LabelName};

    use crate::text;

    /// 편집 중인 양식 입력값(문자열 그대로).
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct FormInput {
        pub code: String,
        pub name: String,
        pub description: String,
        pub body_template: String,
        pub active: bool,
        pub sort_order: String,
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct FormProblems {
        pub code: String,
        pub name: String,
        pub description: String,
        pub body_template: String,
        pub sort_order: String,
    }

    impl FormProblems {
        pub fn is_empty(&self) -> bool {
            self.code.is_empty()
                && self.name.is_empty()
                && self.description.is_empty()
                && self.body_template.is_empty()
                && self.sort_order.is_empty()
        }
    }

    /// 편집 폼의 처음 값. 새 양식은 사용 중·정렬 0으로 시작한다.
    pub fn initial_input(form: Option<&FormDto>) -> FormInput {
        match form {
            None => FormInput {
                active: true,
                sort_order: "0".into(),
                ..FormInput::default()
            },
            Some(form) => FormInput {
                code: form.code.clone(),
                name: form.name.clone(),
                description: form.description.clone(),
                body_template: form.body_template.clone(),
                active: form.active,
                sort_order: form.sort_order.to_string(),
            },
        }
    }

    /// 시스템 양식(휴가·근태 정정)은 연결된 기능이 쓰므로 끌 수 없다.
    pub fn is_system(form: &FormDto) -> bool {
        form.kind != DocumentKind::General
    }

    fn sort_order(raw: &str) -> Result<i64, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(0);
        }
        trimmed
            .parse::<i64>()
            .map_err(|_| "정렬 순서는 정수로 입력해 주세요.".to_owned())
    }

    /// 서버와 같은 규칙으로 검증한다. `creating`일 때만 코드를 본다(코드는 바꿀 수 없다).
    pub fn validate_form(input: &FormInput, creating: bool) -> FormProblems {
        let mut problems = FormProblems::default();
        if creating {
            if let Err(error) = Code::parse(&input.code) {
                problems.code = text::field_error_message("code", &error);
            }
        }
        if let Err(error) = LabelName::parse(&input.name) {
            problems.name = text::field_error_message("name", &error);
        }
        if let Err(error) = rules::optional(Some(&input.description), Comment::parse) {
            problems.description = text::field_error_message("description", &error);
        }
        if let Err(error) = rules::optional(Some(&input.body_template), Body::parse) {
            problems.body_template = text::field_error_message("body_template", &error);
        }
        if let Err(error) = sort_order(&input.sort_order) {
            problems.sort_order = error;
        }
        problems
    }

    pub fn create_form_request(input: &FormInput) -> Result<CreateFormRequest, FormProblems> {
        let problems = validate_form(input, true);
        if !problems.is_empty() {
            return Err(problems);
        }
        Ok(CreateFormRequest {
            code: input.code.clone(),
            name: input.name.trim().to_owned(),
            description: input.description.trim().to_owned(),
            body_template: input.body_template.clone(),
            sort_order: sort_order(&input.sort_order).unwrap_or(0),
        })
    }

    pub fn update_form_request(
        input: &FormInput,
        version: i64,
    ) -> Result<UpdateFormRequest, FormProblems> {
        let problems = validate_form(input, false);
        if !problems.is_empty() {
            return Err(problems);
        }
        Ok(UpdateFormRequest {
            version,
            name: input.name.trim().to_owned(),
            description: input.description.trim().to_owned(),
            body_template: input.body_template.clone(),
            active: input.active,
            sort_order: sort_order(&input.sort_order).unwrap_or(0),
        })
    }

    /// 양식 저장 실패 중 이 화면에서 더 구체적으로 알려 줄 수 있는 것.
    pub fn form_conflict_message(code: &str) -> Option<&'static str> {
        match code {
            "in_use" => Some("시스템 양식은 사용 중지할 수 없습니다."),
            "already_exists" => Some("같은 코드의 양식이 이미 있습니다. 다른 코드를 써 주세요."),
            _ => None,
        }
    }

    /// 전체 문서 상태 필터. 임시저장은 관리자에게도 보이지 않으므로 뺀다.
    pub fn status_options() -> Vec<(String, String)> {
        std::iter::once((String::new(), "전체 상태".to_owned()))
            .chain(
                DocumentStatus::ALL
                    .iter()
                    .filter(|status| **status != DocumentStatus::Draft)
                    .map(|status| {
                        (
                            status.as_str().to_owned(),
                            text::document_status_label(*status).to_owned(),
                        )
                    }),
            )
            .collect()
    }

    /// 필터 값. 비어 있거나 모르는 값이면 전체.
    pub fn parse_status(value: &str) -> Option<DocumentStatus> {
        DocumentStatus::parse(value).ok()
    }
}

/// 열려 있는 편집 대화 상자.
#[derive(Debug, Clone, PartialEq)]
enum Editor {
    New,
    Edit(FormDto),
}

#[component]
pub fn AdminApprovalForms() -> Element {
    let session = use_session();
    let mut forms =
        use_resource(|| async { api::get::<Vec<FormDto>>("/admin/approval/forms").await });
    let mut editor = use_signal(|| None::<Editor>);
    let mut status = use_signal(String::new);
    let more = use_more_pages::<DocumentSummary>();
    let docs = use_resource(move || async move {
        let current = logic::parse_status(&status());
        more.reset();
        api::get::<Page<DocumentSummary>>(&admin_documents_path(current, None)).await
    });

    let tz = session.zone();
    let now = now();

    rsx! {
        PageHeader {
            title: "결재 양식",
            subtitle: "기안할 때 고르는 양식과 본문 틀을 관리합니다.",
            actions: rsx! {
                Button { onclick: move |_| editor.set(Some(Editor::New)), "새 양식" }
            },
        }
        Card {
            {resource_view(forms, move |list: Vec<FormDto>| rsx! {
                if list.is_empty() {
                    EmptyState { title: "양식이 없습니다.", icon: IconName::Document }
                } else {
                    ul { class: "list",
                        for form in list {
                            li { key: "{form.id}", class: "list-row",
                                div { class: "list-row-main",
                                    strong { "{form.name}" }
                                    small {
                                        "{form.code}"
                                        if !form.description.is_empty() {
                                            " · {form.description}"
                                        }
                                    }
                                }
                                div { class: "list-row-side list-row-side-inline",
                                    if logic::is_system(&form) {
                                        Badge { tone: Tone::Info, "시스템 · {text::document_kind_label(form.kind)}" }
                                    }
                                    if form.active {
                                        Badge { tone: Tone::Success, "사용 중" }
                                    } else {
                                        Badge { tone: Tone::Neutral, "사용 안 함" }
                                    }
                                    Button {
                                        kind: ButtonKind::Secondary,
                                        small: true,
                                        onclick: {
                                            let form = form.clone();
                                            move |_| editor.set(Some(Editor::Edit(form.clone())))
                                        },
                                        "수정"
                                    }
                                }
                            }
                        }
                    }
                }
            })}
        }
        Card { title: "전체 문서",
            div { class: "filters filters-narrow",
                SelectField {
                    label: "상태",
                    value: status(),
                    options: logic::status_options(),
                    onchange: move |value: String| status.set(value),
                }
            }
            {resource_view(docs, move |page: Page<DocumentSummary>| {
                let next = more.cursor(&page);
                let has_more = next.is_some();
                let current = logic::parse_status(&status());
                let items: Vec<DocumentSummary> = page.items.into_iter().chain(more.items()).collect();
                rsx! {
                    if items.is_empty() {
                        EmptyState { title: "조건에 맞는 문서가 없습니다.", icon: IconName::Inbox }
                    } else {
                        div { class: "list",
                            for doc in items {
                                DocumentRow { key: "{doc.id}", doc, now, tz }
                            }
                        }
                        LoadMore {
                            has_more,
                            busy: more.busy(),
                            onclick: move |_| {
                                if let Some(cursor) = next.clone() {
                                    spawn(more.load(cursor, move |c| admin_documents_path(current, Some(c))));
                                }
                            },
                        }
                    }
                }
            })}
        }
        if let Some(open) = editor() {
            FormEditor {
                form: match open {
                    Editor::New => None,
                    Editor::Edit(form) => Some(form),
                },
                onclose: move |_| editor.set(None),
                onsaved: move |_| {
                    editor.set(None);
                    forms.restart();
                },
            }
        }
    }
}

/// 양식 만들기·고치기 대화 상자.
#[component]
fn FormEditor(
    form: Option<FormDto>,
    onclose: EventHandler<()>,
    onsaved: EventHandler<()>,
) -> Element {
    let initial = logic::initial_input(form.as_ref());
    let mut code = use_signal(|| initial.code.clone());
    let mut name = use_signal(|| initial.name.clone());
    let mut description = use_signal(|| initial.description.clone());
    let mut body_template = use_signal(|| initial.body_template.clone());
    let mut active = use_signal(|| initial.active);
    let mut sort_order = use_signal(|| initial.sort_order.clone());
    let mut attempted = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let key = use_signal(api::new_idempotency_key);

    let original: Option<(FormId, i64)> = form.as_ref().map(|f| (f.id, f.version));
    let system = form.as_ref().is_some_and(logic::is_system);
    let creating = original.is_none();

    let input = move || logic::FormInput {
        code: code(),
        name: name(),
        description: description(),
        body_template: body_template(),
        active: active(),
        sort_order: sort_order(),
    };
    let problems = if attempted() {
        logic::validate_form(&input(), creating)
    } else {
        logic::FormProblems::default()
    };

    let save = move |_| async move {
        if busy() {
            return;
        }
        attempted.set(true);
        let current = input();
        let result = match original {
            None => match logic::create_form_request(&current) {
                Err(_) => return,
                Ok(request) => {
                    busy.set(true);
                    api::post_once::<_, FormDto>("/admin/approval/forms", &request, &key()).await
                }
            },
            Some((id, version)) => match logic::update_form_request(&current, version) {
                Err(_) => return,
                Ok(request) => {
                    busy.set(true);
                    api::patch::<_, FormDto>(&format!("/admin/approval/forms/{id}"), &request).await
                }
            },
        };
        busy.set(false);
        match result {
            Ok(_) => {
                report_success(if creating {
                    "양식을 만들었습니다."
                } else {
                    "양식을 저장했습니다."
                });
                onsaved.call(());
            }
            Err(failure) if failure.code == "version_conflict" => {
                report_error(&failure);
                onsaved.call(());
            }
            Err(failure) => error.set(
                logic::form_conflict_message(&failure.code)
                    .map(str::to_owned)
                    .unwrap_or_else(|| failure.message()),
            ),
        }
    };

    rsx! {
        Modal { title: if creating { "새 양식" } else { "양식 수정" }, onclose: move |_| onclose.call(()),
            div { class: "form",
                FormError { message: error() }
                if creating {
                    TextField {
                        label: "코드",
                        value: code(),
                        required: true,
                        maxlength: Code::MAX_LEN as i64,
                        hint: "영문 소문자로 시작하고 소문자·숫자·_만 씁니다. 만든 뒤에는 바꿀 수 없습니다.",
                        error: problems.code.clone(),
                        oninput: move |value| code.set(value),
                    }
                } else {
                    div { class: "field",
                        span { class: "field-label", "코드" }
                        p { class: "field-static", "{code}" }
                    }
                }
                TextField {
                    label: "이름",
                    value: name(),
                    required: true,
                    maxlength: LabelName::RULE.max_chars as i64,
                    error: problems.name.clone(),
                    oninput: move |value| name.set(value),
                }
                div { class: "field-with-count",
                    TextArea {
                        label: "설명",
                        value: description(),
                        rows: 2,
                        maxlength: Comment::RULE.max_chars as i64,
                        oninput: move |value| description.set(value),
                    }
                    div { class: "field-foot",
                        span { class: "field-error", "{problems.description}" }
                        CharCount { count: description().chars().count(), max: Comment::RULE.max_chars }
                    }
                }
                div { class: "field-with-count",
                    TextArea {
                        label: "본문 틀",
                        value: body_template(),
                        rows: 8,
                        maxlength: Body::RULE.max_chars as i64,
                        placeholder: "기안할 때 본문에 미리 채워 둘 내용",
                        oninput: move |value| body_template.set(value),
                    }
                    div { class: "field-foot",
                        span { class: "field-error", "{problems.body_template}" }
                        CharCount { count: body_template().chars().count(), max: Body::RULE.max_chars }
                    }
                }
                TextField {
                    label: "정렬 순서",
                    kind: "number",
                    value: sort_order(),
                    hint: "작은 수가 앞에 옵니다.",
                    error: problems.sort_order.clone(),
                    oninput: move |value| sort_order.set(value),
                }
                if !creating {
                    Checkbox {
                        label: "사용 중",
                        checked: active(),
                        disabled: system,
                        hint: if system { "시스템 양식은 연결된 기능이 쓰므로 끌 수 없습니다." } else { "끄면 새 문서에서 고를 수 없습니다. 이미 쓴 문서는 그대로입니다." },
                        onchange: move |value| active.set(value),
                    }
                }
                div { class: "modal-actions",
                    Button { kind: ButtonKind::Secondary, disabled: busy(), onclick: move |_| onclose.call(()), "취소" }
                    Button { busy: busy(), onclick: save, "저장" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::logic::*;
    use pango_api::approval::FormDto;
    use pango_core::approval::{DocumentKind, DocumentStatus};
    use pango_core::id::FormId;

    fn valid() -> FormInput {
        FormInput {
            code: "expense_report".into(),
            name: "지출 결의서".into(),
            description: "".into(),
            body_template: "1. 목적\n2. 금액".into(),
            active: true,
            sort_order: "3".into(),
        }
    }

    #[test]
    fn valid_input_builds_requests() {
        let create = create_form_request(&valid()).expect("create");
        assert_eq!(create.code, "expense_report");
        assert_eq!(create.sort_order, 3);
        assert_eq!(create.body_template, "1. 목적\n2. 금액");
        let update = update_form_request(
            &FormInput {
                active: false,
                ..valid()
            },
            4,
        )
        .expect("update");
        assert_eq!(update.version, 4);
        assert!(!update.active);
        let blank_order = FormInput {
            sort_order: " ".into(),
            ..valid()
        };
        assert_eq!(create_form_request(&blank_order).expect("ok").sort_order, 0);
    }

    #[test]
    fn code_is_checked_only_when_creating() {
        for bad in ["", "a", "Expense", "1abc", "has space", "가나"] {
            let input = FormInput {
                code: bad.into(),
                ..valid()
            };
            assert!(!validate_form(&input, true).code.is_empty(), "{bad:?}");
            assert!(validate_form(&input, false).is_empty(), "{bad:?}");
        }
        let long = FormInput {
            code: "a".repeat(33),
            ..valid()
        };
        assert!(validate_form(&long, true).code.contains("너무 깁니다"));
    }

    #[test]
    fn other_fields_follow_text_rules() {
        let problems = validate_form(
            &FormInput {
                name: " ".into(),
                description: "가".repeat(1001),
                body_template: "\u{0007}".into(),
                sort_order: "1.5".into(),
                ..valid()
            },
            false,
        );
        assert_eq!(problems.name, "이름을(를) 입력해 주세요.");
        assert_eq!(problems.description, "설명이(가) 너무 깁니다.");
        assert!(problems.body_template.contains("본문 양식"));
        assert_eq!(problems.sort_order, "정렬 순서는 정수로 입력해 주세요.");
        assert!(update_form_request(
            &FormInput {
                name: "".into(),
                ..valid()
            },
            1
        )
        .is_err());
        assert!(validate_form(
            &FormInput {
                sort_order: "-2".into(),
                ..valid()
            },
            true
        )
        .is_empty());
    }

    #[test]
    fn initial_values_and_system_forms() {
        let fresh = initial_input(None);
        assert!(fresh.active);
        assert_eq!(fresh.sort_order, "0");
        let form = FormDto {
            id: FormId::new(),
            code: "leave".into(),
            name: "휴가 신청서".into(),
            description: "d".into(),
            kind: DocumentKind::Leave,
            body_template: String::new(),
            active: true,
            sort_order: 1,
            version: 2,
        };
        let loaded = initial_input(Some(&form));
        assert_eq!(loaded.code, "leave");
        assert_eq!(loaded.sort_order, "1");
        assert!(is_system(&form));
        assert!(!is_system(&FormDto {
            kind: DocumentKind::General,
            ..form
        }));
    }

    #[test]
    fn conflicts_and_status_filter() {
        assert!(form_conflict_message("in_use").is_some());
        assert!(form_conflict_message("already_exists").is_some());
        assert!(form_conflict_message("version_conflict").is_none());
        let options = status_options();
        assert_eq!(options[0].0, "");
        assert_eq!(options.len(), DocumentStatus::ALL.len());
        assert!(options.iter().all(|(value, _)| value != "draft"));
        assert_eq!(parse_status(""), None);
        assert_eq!(
            parse_status("in_progress"),
            Some(DocumentStatus::InProgress)
        );
        assert_eq!(parse_status("nope"), None);
    }
}
