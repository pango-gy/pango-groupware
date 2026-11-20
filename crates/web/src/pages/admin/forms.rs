//! 결재 양식 관리와 전체 문서 열람(관리자).
//!
//! 양식은 분류별로 묶어 보여 주고, 편집 창에서 분류·본문 사용·기본 참조자·입력 항목(필드 정의)을
//! 정한다. 입력 항목은 [`super::form_builder`]가 맡고 미리보기는 기안 화면과 같은 `FormFields`다.

use dioxus::prelude::*;
use pango_api::approval::{DocumentSummary, FormDto};
use pango_api::org::MemberSummary;
use pango_api::{Empty, Page};
use pango_core::id::FormId;
use pango_core::text::{Body, Code, Comment, LabelName};
use serde_json::{Map, Value};

use super::form_builder::{logic as builder, SchemaBuilder};
use crate::api;
use crate::pages::approval::logic::admin_documents_path;
use crate::pages::approval::DocumentRow;
use crate::session::use_session;
use crate::text;
use crate::ui::form_fields::focus_element;
use crate::ui::{
    loaded_or_empty, now, report_error, report_success, resource_view, use_more_pages, Badge,
    Button, ButtonKind, Card, CharCount, Checkbox, EmptyState, FormError, FormFields, Icon,
    IconName, LoadMore, MemberPicker, Meta, Modal, PageHeader, SelectField, TextArea, TextField,
    Tone,
};

/// 양식 편집과 문서 필터의 순수 함수.
pub(crate) mod logic {
    use pango_api::approval::{EditFormRequest, FormDto, NewFormRequest};
    use pango_api::org::MemberSummary;
    use pango_core::approval::{DocumentStatus, FormCategory};
    use pango_core::form_schema::FormSchema;
    use pango_core::id::MemberId;
    use pango_core::text::{self as rules, Body, Code, Comment, LabelName};

    use crate::api::ApiError;
    use crate::pages::admin::form_builder::logic::{self as builder, FieldDraft, SchemaErrors};
    use crate::text;

    /// 양식 하나에 둘 수 있는 기본 참조자 수(서버와 같은 값).
    pub const MAX_DEFAULT_REFERENCES: usize = 30;

    /// 편집 중인 양식 입력값.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FormInput {
        pub code: String,
        pub name: String,
        pub description: String,
        pub body_template: String,
        pub active: bool,
        pub sort_order: String,
        pub category: FormCategory,
        pub use_body: bool,
        pub references: Vec<MemberId>,
        pub fields: Vec<FieldDraft>,
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct FormProblems {
        pub code: String,
        pub name: String,
        pub description: String,
        pub body_template: String,
        pub sort_order: String,
        /// 본문도 필수 필드도 없는 양식.
        pub content: String,
        /// 필드 정의의 위치별 오류.
        pub schema: SchemaErrors,
    }

    impl FormProblems {
        pub fn is_empty(&self) -> bool {
            self.code.is_empty()
                && self.name.is_empty()
                && self.description.is_empty()
                && self.body_template.is_empty()
                && self.sort_order.is_empty()
                && self.content.is_empty()
                && self.schema.is_empty()
        }
    }

    /// 편집 폼의 처음 값. 새 양식은 일반 분류·본문 사용·사용 중·정렬 0으로 시작한다.
    pub fn initial_input(form: Option<&FormDto>) -> FormInput {
        match form {
            None => FormInput {
                code: String::new(),
                name: String::new(),
                description: String::new(),
                body_template: String::new(),
                active: true,
                sort_order: "0".into(),
                category: FormCategory::General,
                use_body: true,
                references: Vec::new(),
                fields: Vec::new(),
            },
            Some(form) => FormInput {
                code: form.code.clone(),
                name: form.name.clone(),
                description: form.description.clone(),
                body_template: form.body_template.clone(),
                active: form.active,
                sort_order: form.sort_order.to_string(),
                category: form.category,
                use_body: form.use_body,
                references: form.default_reference_ids.clone(),
                fields: builder::drafts_from_schema(&form.schema),
            },
        }
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

    /// 본문도 필수 필드도 없으면 빈 문서를 낼 수 있게 되므로 만들 수 없다(서버와 같은 규칙).
    pub fn content_problem(use_body: bool, schema: &FormSchema) -> Option<&'static str> {
        (!use_body && !schema.fields.iter().any(|field| field.required))
            .then_some("본문을 쓰지 않으려면 필수 입력 항목이 하나 이상 있어야 해요.")
    }

    /// 서버와 같은 규칙으로 검증한다. `creating`일 때만 코드를 본다(코드는 바꿀 수 없다).
    /// 시스템 양식(`system`)은 분류·본문 사용·입력 항목을 바꿀 수 없으므로 그 부분은 보지 않는다.
    pub fn validate_form(input: &FormInput, creating: bool, system: bool) -> FormProblems {
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
        if !system {
            problems.schema = builder::schema_errors(&input.fields);
            let (schema, _) = builder::build_schema(&input.fields);
            if let Some(message) = content_problem(input.use_body, &schema) {
                problems.content = message.to_owned();
            }
        }
        problems
    }

    pub fn create_form_request(input: &FormInput) -> Result<NewFormRequest, Box<FormProblems>> {
        let problems = validate_form(input, true, false);
        if !problems.is_empty() {
            return Err(Box::new(problems));
        }
        Ok(NewFormRequest {
            code: input.code.clone(),
            name: input.name.trim().to_owned(),
            description: input.description.trim().to_owned(),
            body_template: input.body_template.clone(),
            sort_order: sort_order(&input.sort_order).unwrap_or(0),
            category: Some(input.category),
            schema: Some(builder::build_schema(&input.fields).0),
            use_body: Some(input.use_body),
            default_reference_ids: input.references.clone(),
        })
    }

    /// 수정 요청. 시스템 양식은 분류·입력 항목·본문 사용·기본 참조자를 보내지 않는다(서버가 거부한다).
    pub fn update_form_request(
        input: &FormInput,
        version: i64,
        system: bool,
    ) -> Result<EditFormRequest, Box<FormProblems>> {
        let problems = validate_form(input, false, system);
        if !problems.is_empty() {
            return Err(Box::new(problems));
        }
        Ok(EditFormRequest {
            version,
            name: input.name.trim().to_owned(),
            description: input.description.trim().to_owned(),
            body_template: input.body_template.clone(),
            active: input.active,
            sort_order: sort_order(&input.sort_order).unwrap_or(0),
            category: (!system).then_some(input.category),
            schema: (!system).then(|| builder::build_schema(&input.fields).0),
            use_body: (!system).then_some(input.use_body),
            default_reference_ids: (!system).then(|| input.references.clone()),
        })
    }

    /// 양식 저장 실패 중 이 화면에서 더 구체적으로 알려 줄 수 있는 것.
    pub fn form_conflict_message(error: &ApiError) -> Option<&'static str> {
        match (error.code.as_str(), error.field.as_deref()) {
            ("in_use", _) => Some("시스템 양식은 끄거나 내용을 바꿀 수 없어요."),
            ("already_exists", _) => Some("같은 코드의 양식이 이미 있어요. 다른 코드를 써 주세요."),
            ("validation_failed", Some("use_body")) => {
                Some("본문을 쓰지 않으려면 필수 입력 항목이 하나 이상 있어야 해요.")
            }
            ("validation_failed", Some("default_reference_ids")) => Some(
                "기본 참조자를 다시 확인해 주세요. 재직 중인 구성원만, 30명까지 넣을 수 있어요.",
            ),
            _ => None,
        }
    }

    /// 입력 항목에 오류가 있을 때 폼 위에 띄울 한 줄: 몇 번째 항목을 봐야 하는지.
    pub fn schema_error_hint(errors: &SchemaErrors) -> Option<String> {
        let first = errors
            .keys()
            .filter_map(|path| builder::field_index(path))
            .min();
        match first {
            Some(index) => Some(format!("입력 항목 {}번을 확인해 주세요.", index + 1)),
            None => (!errors.is_empty()).then(|| "입력 항목을 확인해 주세요.".to_owned()),
        }
    }

    /// 분류별로 묶는다(분류 순서대로, 양식이 없는 분류는 빠진다). 묶음 안의 순서는 받은 그대로다.
    pub fn group_forms(forms: &[FormDto]) -> Vec<(FormCategory, Vec<FormDto>)> {
        FormCategory::ALL
            .iter()
            .filter_map(|category| {
                let group: Vec<FormDto> = forms
                    .iter()
                    .filter(|form| form.category == *category)
                    .cloned()
                    .collect();
                (!group.is_empty()).then_some((*category, group))
            })
            .collect()
    }

    /// 분류 선택 상자의 `(값, 문구)`.
    pub fn category_options() -> Vec<(String, String)> {
        FormCategory::ALL
            .iter()
            .map(|category| {
                (
                    category.as_str().to_owned(),
                    text::form_category_label(*category).to_owned(),
                )
            })
            .collect()
    }

    /// 기본 참조자의 `(ID, 이름)`. 구성원 목록에 없으면(퇴사 등) 그렇다고 적는다.
    pub fn reference_names(ids: &[MemberId], members: &[MemberSummary]) -> Vec<(MemberId, String)> {
        ids.iter()
            .map(|id| {
                let name = members
                    .iter()
                    .find(|member| member.id == *id)
                    .map(|member| member.name.clone())
                    .unwrap_or_else(|| "재직 중이 아닌 구성원".to_owned());
                (*id, name)
            })
            .collect()
    }

    /// 기본 참조자를 더한다. 중복과 최대 인원 초과는 무시한다.
    pub fn add_reference(ids: &[MemberId], id: MemberId) -> Vec<MemberId> {
        let mut out = ids.to_vec();
        if !out.contains(&id) && out.len() < MAX_DEFAULT_REFERENCES {
            out.push(id);
        }
        out
    }

    pub fn remove_reference(ids: &[MemberId], id: MemberId) -> Vec<MemberId> {
        ids.iter().copied().filter(|other| *other != id).collect()
    }

    /// 목록 한 줄의 보조 정보: 코드, 설명, 입력 항목 수.
    pub fn form_meta(form: &FormDto) -> Vec<String> {
        let fields = form.schema.fields.len();
        vec![
            form.code.clone(),
            form.description.clone(),
            if fields > 0 {
                format!("입력 항목 {fields}개")
            } else {
                String::new()
            },
        ]
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

/// 편집 창의 맨 위(오류 안내가 놓이는 곳)의 DOM id.
const EDITOR_TOP_ID: &str = "form-editor-top";

/// 열려 있는 편집 대화 상자.
#[derive(Debug, Clone, PartialEq)]
enum Editor {
    New,
    Edit(Box<FormDto>),
}

#[component]
pub fn AdminApprovalForms() -> Element {
    let session = use_session();
    let mut forms =
        use_resource(|| async { api::get::<Vec<FormDto>>("/admin/approval/forms").await });
    let mut editor = use_signal(|| None::<Editor>);
    let mut status = use_signal(String::new);
    // 복제 중인 양식. 끝날 때까지 다른 복제를 막는다.
    let mut copying = use_signal(|| None::<FormId>);
    let more = use_more_pages::<DocumentSummary>();
    let docs = use_resource(move || async move {
        let current = logic::parse_status(&status());
        more.reset();
        api::get::<Page<DocumentSummary>>(&admin_documents_path(current, None, None)).await
    });

    let tz = session.zone();
    let now = now();

    let duplicate = move |id: FormId| async move {
        if copying().is_some() {
            return;
        }
        copying.set(Some(id));
        let result = api::post_once::<_, FormDto>(
            &format!("/admin/approval/forms/{id}/duplicate"),
            &Empty {},
            &api::new_idempotency_key(),
        )
        .await;
        copying.set(None);
        match result {
            Ok(copy) => {
                report_success("사본을 만들었어요. 꺼진 상태이니 고친 뒤 켜 주세요.");
                forms.restart();
                editor.set(Some(Editor::Edit(Box::new(copy))));
            }
            Err(failure) => report_error(&failure),
        }
    };

    rsx! {
        PageHeader {
            title: "결재 양식",
            description: "기안할 때 고르는 양식과 입력 항목을 관리해요.",
            actions: rsx! {
                Button { onclick: move |_| editor.set(Some(Editor::New)), "새 양식" }
            },
        }
        {resource_view(forms, move |list: Vec<FormDto>| rsx! {
            if list.is_empty() {
                EmptyState { title: "양식이 없어요.", icon: IconName::Document }
            }
            for (category , group) in logic::group_forms(&list) {
                Card {
                    key: "{category.as_str()}",
                    title: format!("{} {}", text::form_category_label(category), group.len()),
                    ul { class: "list",
                        for form in group {
                            li { key: "{form.id}", class: "list-row list-row-wrap",
                                div { class: "list-row-main",
                                    strong { "{form.name}" }
                                    Meta { items: logic::form_meta(&form) }
                                }
                                div { class: "list-row-side list-row-side-inline",
                                    Badge { tone: Tone::Neutral, {text::form_origin_label(form.origin)} }
                                    if form.system {
                                        Badge { tone: Tone::Info, "시스템" }
                                    }
                                    if form.active {
                                        Badge { tone: Tone::Success, "사용 중" }
                                    } else {
                                        Badge { tone: Tone::Neutral, "사용 안 함" }
                                    }
                                    if !form.system {
                                        Button {
                                            kind: ButtonKind::Grey,
                                            small: true,
                                            busy: copying() == Some(form.id),
                                            disabled: copying().is_some(),
                                            onclick: {
                                                let id = form.id;
                                                move |_| {
                                                    spawn(duplicate(id));
                                                }
                                            },
                                            "복제"
                                        }
                                    }
                                    Button {
                                        kind: ButtonKind::Grey,
                                        small: true,
                                        onclick: {
                                            let form = form.clone();
                                            move |_| editor.set(Some(Editor::Edit(Box::new(form.clone()))))
                                        },
                                        "수정"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })}
        Card { title: "전체 문서",
            div { class: "toolbar",
                SelectField {
                    label: "상태",
                    value: status(),
                    options: logic::status_options(),
                    onchange: move |value: String| status.set(value),
                }
            }
            p { class: "field-hint",
                "차례인 결재자가 퇴사·정지해 멈춘 진행 중 문서는 문서를 열어 관리자 대리 반려로 끝낼 수 있어요. 본인이 기안한 문서는 대리 반려할 수 없어요."
            }
            {resource_view(docs, move |page: Page<DocumentSummary>| {
                let next = more.cursor(&page);
                let has_more = next.is_some();
                let current = logic::parse_status(&status());
                let items: Vec<DocumentSummary> = more.all(page.items, |item| item.id);
                rsx! {
                    if items.is_empty() {
                        EmptyState { title: "조건에 맞는 문서가 없어요.", icon: IconName::Inbox }
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
                                    spawn(more.load(cursor, move |c| admin_documents_path(current, None, Some(c))));
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
                    Editor::Edit(form) => Some(*form),
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

/// 양식 만들기·고치기 대화 상자. 왼쪽에서 고치고 오른쪽 미리보기로 기안 화면의 모양을 확인한다.
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
    let mut category = use_signal(|| initial.category);
    let mut use_body = use_signal(|| initial.use_body);
    let mut references = use_signal(|| initial.references.clone());
    let mut fields = use_signal(|| initial.fields.clone());
    // 미리보기에 넣어 본 값. 저장되지 않는다.
    let mut preview = use_signal(Map::<String, Value>::new);
    let mut attempted = use_signal(|| false);
    let mut error = use_signal(String::new);
    // 서버가 돌려준 입력 항목 오류(`schema.fields[i]…`).
    let mut server_schema = use_signal(builder::SchemaErrors::new);
    let mut busy = use_signal(|| false);
    // 창을 열 때 한 번 만들고 성공하면 바꾼다. 재시도해도 양식은 하나만 생긴다.
    let mut key = use_signal(api::new_idempotency_key);
    // 기본 참조자의 이름을 보여 주려고 읽는다. 못 읽어도 편집은 할 수 있다.
    let members = use_resource(|| async { api::get::<Vec<MemberSummary>>("/members").await });

    let original: Option<(FormId, i64)> = form.as_ref().map(|f| (f.id, f.version));
    let system = form.as_ref().is_some_and(|f| f.system);
    let creating = original.is_none();

    let input = move || logic::FormInput {
        code: code(),
        name: name(),
        description: description(),
        body_template: body_template(),
        active: active(),
        sort_order: sort_order(),
        category: category(),
        use_body: use_body(),
        references: references(),
        fields: fields(),
    };
    let problems = if attempted() {
        logic::validate_form(&input(), creating, system)
    } else {
        logic::FormProblems::default()
    };
    let schema_errors = if problems.schema.is_empty() {
        server_schema()
    } else {
        problems.schema.clone()
    };
    let (preview_schema, _) = builder::build_schema(&fields());
    let reference_names = logic::reference_names(&references(), &loaded_or_empty(&members));

    let save = move |_| async move {
        if busy() {
            return;
        }
        attempted.set(true);
        server_schema.set(builder::SchemaErrors::new());
        let current = input();
        let built = match original {
            None => logic::create_form_request(&current).map(|request| (None, Some(request), None)),
            Some((id, version)) => logic::update_form_request(&current, version, system)
                .map(|request| (Some(id), None, Some(request))),
        };
        let (id, create, update) = match built {
            Ok(request) => request,
            Err(found) => {
                error.set(
                    logic::schema_error_hint(&found.schema)
                        .unwrap_or_else(|| "입력한 내용을 확인해 주세요.".to_owned()),
                );
                // 저장 버튼은 창 맨 아래에 있으므로 안내가 보이게 위로 올린다.
                focus_element(EDITOR_TOP_ID);
                return;
            }
        };
        error.set(String::new());
        busy.set(true);
        let result = match (id, create, update) {
            (Some(id), _, Some(request)) => {
                api::patch::<_, FormDto>(&format!("/admin/approval/forms/{id}"), &request).await
            }
            (_, Some(request), _) => {
                api::post_once::<_, FormDto>("/admin/approval/forms", &request, &key()).await
            }
            _ => return busy.set(false),
        };
        busy.set(false);
        match result {
            Ok(_) => {
                key.set(api::new_idempotency_key());
                report_success(if creating {
                    "양식을 만들었어요."
                } else {
                    "양식을 저장했어요."
                });
                onsaved.call(());
            }
            Err(failure) if failure.code == "version_conflict" => {
                report_error(&failure);
                onsaved.call(());
            }
            Err(failure) => {
                // 입력 항목의 오류는 그 항목에 붙이고, 폼 위에는 몇 번째인지 알린다.
                let mapped = builder::server_schema_errors(&failure.fields);
                let message = logic::schema_error_hint(&mapped)
                    .or_else(|| logic::form_conflict_message(&failure).map(str::to_owned))
                    .unwrap_or_else(|| failure.message());
                server_schema.set(mapped);
                error.set(message);
                focus_element(EDITOR_TOP_ID);
            }
        }
    };

    rsx! {
        Modal {
            title: if creating { "새 양식" } else { "양식 수정" },
            wide: !system,
            onclose: move |_| onclose.call(()),
            div { class: if system { "form" } else { "form-editor" },
                div { id: EDITOR_TOP_ID, class: "form form-editor-main",
                    FormError { message: error() }
                    if system {
                        p { class: "callout callout-info",
                            "시스템 양식은 휴가·근태 기능이 내용을 정해요. 이름·설명·정렬 순서만 바꿀 수 있고, 끄거나 입력 항목을 고칠 수 없어요."
                        }
                    }
                    div { class: "form-row",
                        if creating {
                            TextField {
                                label: "코드",
                                value: code(),
                                required: true,
                                maxlength: Code::MAX_LEN as i64,
                                hint: "영문 소문자로 시작하고 소문자·숫자·_만 써요. 만든 뒤에는 바꿀 수 없어요.",
                                error: problems.code.clone(),
                                oninput: move |value| code.set(value),
                            }
                        } else {
                            div { class: "field",
                                span { class: "field-label", "코드" }
                                p { class: "field-static mono", "{code}" }
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
                    }
                    div { class: "form-row",
                        SelectField {
                            label: "분류",
                            value: category().as_str(),
                            options: logic::category_options(),
                            disabled: system,
                            onchange: move |value: String| {
                                if let Ok(next) = pango_core::approval::FormCategory::parse(&value) {
                                    category.set(next);
                                }
                            },
                        }
                        TextField {
                            label: "정렬 순서",
                            kind: "number",
                            value: sort_order(),
                            hint: "작은 수가 앞에 와요.",
                            error: problems.sort_order.clone(),
                            oninput: move |value| sort_order.set(value),
                        }
                    }
                    div { class: "field-with-count",
                        TextArea {
                            label: "설명",
                            value: description(),
                            rows: 2,
                            maxlength: Comment::RULE.max_chars as i64,
                            placeholder: "양식을 고를 때 이름 아래에 보여요",
                            oninput: move |value| description.set(value),
                        }
                        div { class: "field-foot",
                            span { class: "field-error", "{problems.description}" }
                            CharCount { count: description().chars().count(), max: Comment::RULE.max_chars }
                        }
                    }
                    if !creating {
                        Checkbox {
                            label: "사용 중",
                            checked: active(),
                            disabled: system,
                            hint: if system { "시스템 양식은 연결된 기능이 쓰므로 끌 수 없어요." } else { "끄면 새 문서에서 고를 수 없어요. 이미 쓴 문서는 그대로예요." },
                            onchange: move |value| active.set(value),
                        }
                    }
                    if !system {
                        h3 { class: "sub-title", "입력 항목" }
                        p { class: "field-hint form-editor-note",
                            "기안할 때 채우는 칸이에요. 항목을 고쳐도 이미 낸 문서는 낼 때의 모양 그대로 보여요."
                        }
                        SchemaBuilder {
                            fields: fields(),
                            errors: schema_errors,
                            onchange: move |next: Vec<builder::FieldDraft>| {
                                fields.set(next);
                                if !server_schema.peek().is_empty() {
                                    server_schema.set(builder::SchemaErrors::new());
                                }
                            },
                        }
                        h3 { class: "sub-title", "본문" }
                        Checkbox {
                            label: "자유 입력 본문 쓰기",
                            checked: use_body(),
                            hint: "켜면 본문이 필수예요. 끄면 입력 항목만으로 문서를 내요.",
                            onchange: move |value| use_body.set(value),
                        }
                        if !problems.content.is_empty() {
                            p { class: "field-error", role: "alert", "{problems.content}" }
                        }
                        if use_body() {
                            div { class: "field-with-count",
                                TextArea {
                                    label: "본문 틀",
                                    value: body_template(),
                                    rows: 5,
                                    maxlength: Body::RULE.max_chars as i64,
                                    placeholder: "기안할 때 본문에 미리 채워 둘 내용",
                                    oninput: move |value| body_template.set(value),
                                }
                                div { class: "field-foot",
                                    span { class: "field-error", "{problems.body_template}" }
                                    CharCount { count: body_template().chars().count(), max: Body::RULE.max_chars }
                                }
                            }
                        }
                        h3 { class: "sub-title", "기본 참조자" }
                        p { class: "field-hint form-editor-note",
                            "이 양식으로 상신하면 자동으로 참조에 들어가요. 예: 회계 양식에 재무 담당자."
                        }
                        if !reference_names.is_empty() {
                            ul { class: "chips",
                                for (id , member_name) in reference_names {
                                    li { key: "{id}", class: "chip",
                                        span { "{member_name}" }
                                        button {
                                            class: "icon-btn chip-remove",
                                            r#type: "button",
                                            "aria-label": "{member_name} 기본 참조에서 빼기",
                                            onclick: move |_| references.set(logic::remove_reference(&references(), id)),
                                            Icon { name: IconName::Close }
                                        }
                                    }
                                }
                            }
                        }
                        if references().len() < logic::MAX_DEFAULT_REFERENCES {
                            MemberPicker {
                                label: "기본 참조자 추가",
                                exclude: references(),
                                onpick: move |member: MemberSummary| {
                                    references.set(logic::add_reference(&references(), member.id));
                                },
                            }
                        }
                    }
                }
                if !system {
                    aside { class: "form-editor-preview", "aria-label": "미리보기",
                        h3 { class: "sub-title", "미리보기" }
                        p { class: "field-hint", "기안 화면에서 이렇게 보여요. 값을 넣어 봐도 저장되지 않아요." }
                        div { class: "form-editor-sheet",
                            strong { class: "doc-form-name",
                                if name().trim().is_empty() { "양식 이름" } else { "{name}" }
                            }
                            if preview_schema.fields.is_empty() && !use_body() {
                                p { class: "text-sub", "입력 항목을 추가하면 여기에 보여요." }
                            }
                            FormFields {
                                schema: preview_schema,
                                values: preview(),
                                onchange: move |next: Map<String, Value>| preview.set(next),
                            }
                            if use_body() {
                                div { class: "field",
                                    span { class: "field-label", "내용" }
                                    div { class: "form-editor-body doc-body",
                                        if body_template().trim().is_empty() { "본문을 자유롭게 써요." } else { "{body_template}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "modal-actions",
                Button { kind: ButtonKind::Grey, disabled: busy(), onclick: move |_| onclose.call(()), "취소" }
                Button { busy: busy(), onclick: save, "저장" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::logic::*;
    use pango_api::approval::{FormDto, FormRoute};
    use pango_api::org::MemberSummary;
    use pango_core::approval::{DocumentKind, DocumentStatus, FormCategory, FormOrigin};
    use pango_core::form_schema::{FieldDef, FieldKind, FormSchema};
    use pango_core::id::{FormId, MemberId};
    use pango_core::member::MemberStatus;

    use crate::api::ApiError;
    use crate::pages::admin::form_builder::logic::{add_field, set_label, SchemaErrors};

    fn valid() -> FormInput {
        FormInput {
            code: "expense_report".into(),
            name: "지출 결의서".into(),
            description: "".into(),
            body_template: "1. 목적\n2. 금액".into(),
            active: true,
            sort_order: "3".into(),
            category: FormCategory::Accounting,
            use_body: true,
            references: Vec::new(),
            fields: Vec::new(),
        }
    }

    fn required_field() -> FormSchema {
        FormSchema {
            fields: vec![FieldDef {
                key: "amount".into(),
                label: "금액".into(),
                kind: FieldKind::Money,
                required: true,
                help: None,
            }],
        }
    }

    fn form(kind: DocumentKind, category: FormCategory, name: &str) -> FormDto {
        FormDto {
            id: FormId::new(),
            code: "leave".into(),
            name: name.into(),
            description: "d".into(),
            kind,
            body_template: String::new(),
            active: true,
            sort_order: 1,
            version: 2,
            category,
            category_label: category.label().to_owned(),
            schema: required_field(),
            schema_version: 4,
            use_body: false,
            default_reference_ids: vec![MemberId::new()],
            origin: FormOrigin::Catalog,
            system: kind != DocumentKind::General,
            route: FormRoute::of(kind),
        }
    }

    fn api_error(code: &str, field: Option<&str>) -> ApiError {
        ApiError {
            status: 400,
            code: code.into(),
            field: field.map(str::to_owned),
            reason: None,
            fields: Vec::new(),
        }
    }

    #[test]
    fn valid_input_builds_requests() {
        let reference = MemberId::new();
        let mut input = FormInput {
            references: vec![reference],
            ..valid()
        };
        input.fields = add_field(&[]);
        input.fields[0] = set_label(&input.fields[0], "Amount", &[], "field");
        input.fields[0].kind = "money".into();
        let create = create_form_request(&input).expect("create");
        assert_eq!(create.code, "expense_report");
        assert_eq!(create.sort_order, 3);
        assert_eq!(create.body_template, "1. 목적\n2. 금액");
        assert_eq!(create.category, Some(FormCategory::Accounting));
        assert_eq!(create.use_body, Some(true));
        assert_eq!(create.default_reference_ids, vec![reference]);
        let schema = create.schema.expect("schema");
        assert_eq!(schema.fields[0].key, "amount");
        assert_eq!(schema.fields[0].kind, FieldKind::Money);

        let update = update_form_request(
            &FormInput {
                active: false,
                ..input.clone()
            },
            4,
            false,
        )
        .expect("update");
        assert_eq!(update.version, 4);
        assert!(!update.active);
        assert_eq!(update.schema, Some(schema));
        assert_eq!(update.default_reference_ids, Some(vec![reference]));
        let blank_order = FormInput {
            sort_order: " ".into(),
            ..valid()
        };
        assert_eq!(create_form_request(&blank_order).expect("ok").sort_order, 0);
    }

    #[test]
    fn system_forms_send_only_what_they_may_change() {
        // 시스템 양식은 입력 항목이 엉망이어도 검사하지 않고, 바꿀 수 없는 것은 보내지 않는다.
        let mut input = valid();
        input.use_body = false;
        input.fields = add_field(&[]);
        assert!(update_form_request(&input, 1, false).is_err());
        let update = update_form_request(&input, 1, true).expect("system update");
        assert_eq!(
            (
                update.category,
                update.schema,
                update.use_body,
                update.default_reference_ids
            ),
            (None, None, None, None)
        );
        assert_eq!(update.name, "지출 결의서");
    }

    #[test]
    fn a_form_needs_a_body_or_a_required_field() {
        assert!(content_problem(true, &FormSchema::default()).is_none());
        assert!(content_problem(false, &required_field()).is_none());
        assert!(content_problem(false, &FormSchema::default()).is_some());
        let mut optional = required_field();
        optional.fields[0].required = false;
        assert!(content_problem(false, &optional).is_some());

        let empty = FormInput {
            use_body: false,
            ..valid()
        };
        let problems = validate_form(&empty, true, false);
        assert!(!problems.content.is_empty());
        assert!(create_form_request(&empty).is_err());
    }

    #[test]
    fn field_definition_errors_block_saving_and_point_at_the_row() {
        let mut input = valid();
        input.fields = add_field(&add_field(&[]));
        input.fields[1].key = "Bad".into();
        let problems = validate_form(&input, true, false);
        assert!(problems.schema.contains_key("fields[0].label"));
        assert!(problems.schema.contains_key("fields[1].key"));
        assert!(!problems.is_empty());
        assert_eq!(
            schema_error_hint(&problems.schema),
            Some("입력 항목 1번을 확인해 주세요.".into())
        );
        let only_list = SchemaErrors::from([("fields".to_owned(), "x".to_owned())]);
        assert_eq!(
            schema_error_hint(&only_list),
            Some("입력 항목을 확인해 주세요.".into())
        );
        assert_eq!(schema_error_hint(&SchemaErrors::new()), None);
    }

    #[test]
    fn code_is_checked_only_when_creating() {
        for bad in ["", "a", "Expense", "1abc", "has space", "가나"] {
            let input = FormInput {
                code: bad.into(),
                ..valid()
            };
            assert!(
                !validate_form(&input, true, false).code.is_empty(),
                "{bad:?}"
            );
            assert!(validate_form(&input, false, false).is_empty(), "{bad:?}");
        }
        let long = FormInput {
            code: "a".repeat(33),
            ..valid()
        };
        assert!(validate_form(&long, true, false)
            .code
            .contains("너무 깁니다"));
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
            1,
            false
        )
        .is_err());
        assert!(validate_form(
            &FormInput {
                sort_order: "-2".into(),
                ..valid()
            },
            true,
            false
        )
        .is_empty());
    }

    #[test]
    fn initial_values_come_from_the_form() {
        let fresh = initial_input(None);
        assert!(fresh.active && fresh.use_body);
        assert_eq!(fresh.sort_order, "0");
        assert_eq!(fresh.category, FormCategory::General);
        assert!(fresh.fields.is_empty());
        let form = form(DocumentKind::General, FormCategory::Welfare, "경조금");
        let loaded = initial_input(Some(&form));
        assert_eq!(loaded.code, "leave");
        assert_eq!(loaded.sort_order, "1");
        assert_eq!(loaded.category, FormCategory::Welfare);
        assert!(!loaded.use_body);
        assert_eq!(loaded.references, form.default_reference_ids);
        assert_eq!(loaded.fields.len(), 1);
        assert_eq!(loaded.fields[0].key, "amount");
        // 읽어 온 그대로 저장하면 정의가 바뀌지 않는다.
        let update = update_form_request(&loaded, form.version, false).expect("update");
        assert_eq!(update.schema, Some(form.schema.clone()));
        assert_eq!(
            form_meta(&form),
            vec![
                "leave".to_owned(),
                "d".to_owned(),
                "입력 항목 1개".to_owned()
            ]
        );
    }

    #[test]
    fn forms_are_grouped_in_category_order() {
        let forms = vec![
            form(DocumentKind::General, FormCategory::Welfare, "경조금"),
            form(DocumentKind::Leave, FormCategory::Hr, "휴가"),
            form(DocumentKind::General, FormCategory::General, "품의서"),
            form(DocumentKind::General, FormCategory::Hr, "사직서"),
        ];
        let groups = group_forms(&forms);
        let shape: Vec<(FormCategory, Vec<&str>)> = groups
            .iter()
            .map(|(category, forms)| (*category, forms.iter().map(|f| f.name.as_str()).collect()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (FormCategory::General, vec!["품의서"]),
                (FormCategory::Hr, vec!["휴가", "사직서"]),
                (FormCategory::Welfare, vec!["경조금"]),
            ]
        );
        assert!(group_forms(&[]).is_empty());
        let options = category_options();
        assert_eq!(options.len(), FormCategory::ALL.len());
        assert_eq!(options[0], ("general".into(), "일반".into()));
        for (value, _) in &options {
            assert!(FormCategory::parse(value).is_ok());
        }
    }

    #[test]
    fn default_references_are_unique_capped_and_named() {
        let (a, b) = (MemberId::new(), MemberId::new());
        let one = add_reference(&[], a);
        assert_eq!(add_reference(&one, a), one);
        assert_eq!(add_reference(&one, b), vec![a, b]);
        assert!(remove_reference(&one, a).is_empty());
        assert_eq!(remove_reference(&one, b), one);
        let full: Vec<MemberId> = (0..MAX_DEFAULT_REFERENCES)
            .map(|_| MemberId::new())
            .collect();
        assert_eq!(add_reference(&full, a).len(), MAX_DEFAULT_REFERENCES);

        let members = [MemberSummary {
            id: a,
            name: "홍길동".into(),
            email: None,
            phone: None,
            department_id: None,
            department_name: None,
            position_id: None,
            position_name: None,
            status: MemberStatus::Active,
        }];
        assert_eq!(
            reference_names(&[a, b], &members),
            vec![
                (a, "홍길동".to_owned()),
                (b, "재직 중이 아닌 구성원".to_owned())
            ]
        );
        assert!(reference_names(&[], &members).is_empty());
    }

    #[test]
    fn conflicts_and_status_filter() {
        assert!(form_conflict_message(&api_error("in_use", None)).is_some());
        assert!(form_conflict_message(&api_error("already_exists", None)).is_some());
        assert!(form_conflict_message(&api_error("validation_failed", Some("use_body"))).is_some());
        assert!(form_conflict_message(&api_error("validation_failed", Some("name"))).is_none());
        assert!(form_conflict_message(&api_error("version_conflict", None)).is_none());
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
