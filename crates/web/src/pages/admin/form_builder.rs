//! 양식의 필드 정의 편집기(양식 관리 화면의 한 부분).
//!
//! 편집 중에는 입력한 글자 그대로([`logic::FieldDraft`]) 들고 있다가, 검증·미리보기·저장 때
//! [`logic::build_schema`]로 `FormSchema`를 만든다. 검증은 서버와 같은 `FormSchema::validate`다.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use pango_core::form_schema::{
    MAX_COLUMNS, MAX_FIELDS, MAX_HELP_CHARS, MAX_KEY_LEN, MAX_LABEL_CHARS,
};

use crate::ui::{
    Badge, Button, ButtonKind, Checkbox, Icon, IconName, SelectField, TextArea, TextField, Tone,
};

use logic::FieldDraft;

/// 필드 정의 편집의 순수 함수: 입력값 ↔ 정의, 키 만들기, 오류 위치 찾기.
pub(crate) mod logic {
    use std::collections::BTreeMap;

    use pango_api::FieldErrorDto;
    use pango_core::form_schema::{ColumnDef, FieldDef, FieldKind, FormSchema, MAX_KEY_LEN};

    use crate::text;

    /// 필드로 쓸 수 있는 종류(`kind` 값), 화면에 보이는 순서.
    pub const KINDS: [&str; 11] = [
        "text",
        "textarea",
        "number",
        "money",
        "date",
        "date_range",
        "time_range",
        "select",
        "checkbox",
        "member",
        "table",
    ];
    /// 표의 열로 쓸 수 있는 종류.
    pub const COLUMN_KINDS: [&str; 5] = ["text", "number", "money", "date", "select"];

    /// 편집 중인 필드(또는 표의 열). 숫자 속성은 입력한 글자 그대로 둔다.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct FieldDraft {
        /// 화면에서 줄을 구분하는 번호(순서를 바꿔도 따라간다). 정의에는 들어가지 않는다.
        pub uid: u32,
        pub key: String,
        /// 키를 아직 손으로 고치지 않았는지. 그동안은 이름을 따라 키를 만든다.
        pub auto_key: bool,
        pub label: String,
        /// `kind` 값([`KINDS`]).
        pub kind: String,
        pub required: bool,
        pub help: String,
        pub max_len: String,
        pub min: String,
        pub max: String,
        /// 선택지. 한 줄에 하나.
        pub options: String,
        /// 표의 열(종류가 `table`일 때만 쓴다).
        pub columns: Vec<FieldDraft>,
    }

    /// 속성이 비어 있는 그 종류의 값. 모르는 값은 한 줄 글로 본다.
    pub fn kind_of(code: &str) -> FieldKind {
        match code {
            "textarea" => FieldKind::Textarea { max_len: None },
            "number" => FieldKind::Number {
                min: None,
                max: None,
            },
            "money" => FieldKind::Money,
            "date" => FieldKind::Date,
            "date_range" => FieldKind::DateRange,
            "time_range" => FieldKind::TimeRange,
            "select" => FieldKind::Select {
                options: Vec::new(),
            },
            "checkbox" => FieldKind::Checkbox,
            "member" => FieldKind::Member,
            "table" => FieldKind::Table {
                columns: Vec::new(),
            },
            _ => FieldKind::Text { max_len: None },
        }
    }

    /// 종류 선택 상자의 `(값, 문구)`.
    pub fn kind_options(codes: &[&str]) -> Vec<(String, String)> {
        codes
            .iter()
            .map(|code| {
                (
                    (*code).to_owned(),
                    text::field_kind_label(&kind_of(code)).to_owned(),
                )
            })
            .collect()
    }

    /// 선택지 글을 목록으로. 빈 줄은 건너뛴다.
    pub fn options_list(raw: &str) -> Vec<String> {
        raw.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn draft_of(
        uid: u32,
        key: &str,
        label: &str,
        kind: &FieldKind,
        required: bool,
        help: Option<&str>,
    ) -> FieldDraft {
        let number = |value: &Option<i64>| value.map(|n| n.to_string()).unwrap_or_default();
        let mut draft = FieldDraft {
            uid,
            key: key.to_owned(),
            auto_key: false,
            label: label.to_owned(),
            kind: kind.code().to_owned(),
            required,
            help: help.unwrap_or_default().to_owned(),
            ..FieldDraft::default()
        };
        match kind {
            FieldKind::Text { max_len } | FieldKind::Textarea { max_len } => {
                draft.max_len = max_len.map(|n| n.to_string()).unwrap_or_default();
            }
            FieldKind::Number { min, max } => {
                draft.min = number(min);
                draft.max = number(max);
            }
            FieldKind::Select { options } => draft.options = options.join("\n"),
            _ => {}
        }
        draft
    }

    /// 저장된 정의를 편집할 수 있는 모양으로.
    pub fn drafts_from_schema(schema: &FormSchema) -> Vec<FieldDraft> {
        let mut uid = 0;
        let mut next = || {
            uid += 1;
            uid
        };
        schema
            .fields
            .iter()
            .map(|field| {
                let mut draft = draft_of(
                    next(),
                    &field.key,
                    &field.label,
                    &field.kind,
                    field.required,
                    field.help.as_deref(),
                );
                if let FieldKind::Table { columns } = &field.kind {
                    draft.columns = columns
                        .iter()
                        .map(|column| {
                            draft_of(
                                next(),
                                &column.key,
                                &column.label,
                                &column.kind,
                                column.required,
                                None,
                            )
                        })
                        .collect();
                }
                draft
            })
            .collect()
    }

    /// 정의로 옮길 수 없는 입력(숫자가 아닌 글자) 하나: `(위치, 사유 코드)`.
    pub type Problem = (String, &'static str);

    fn parse_number<T: std::str::FromStr>(
        raw: &str,
        path: String,
        problems: &mut Vec<Problem>,
    ) -> Option<T> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let parsed = trimmed.replace(',', "").parse::<T>().ok();
        if parsed.is_none() {
            problems.push((path, "invalid_format"));
        }
        parsed
    }

    fn build_kind(draft: &FieldDraft, path: &str, problems: &mut Vec<Problem>) -> FieldKind {
        match kind_of(&draft.kind) {
            FieldKind::Text { .. } => FieldKind::Text {
                max_len: parse_number(&draft.max_len, format!("{path}.max_len"), problems),
            },
            FieldKind::Textarea { .. } => FieldKind::Textarea {
                max_len: parse_number(&draft.max_len, format!("{path}.max_len"), problems),
            },
            FieldKind::Number { .. } => FieldKind::Number {
                min: parse_number(&draft.min, format!("{path}.min"), problems),
                max: parse_number(&draft.max, format!("{path}.max"), problems),
            },
            FieldKind::Select { .. } => FieldKind::Select {
                options: options_list(&draft.options),
            },
            FieldKind::Table { .. } => FieldKind::Table {
                columns: draft
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        let path = format!("{path}.columns[{index}]");
                        ColumnDef {
                            key: column.key.trim().to_owned(),
                            label: column.label.clone(),
                            kind: build_kind(column, &path, problems),
                            required: column.required,
                        }
                    })
                    .collect(),
            },
            other => other,
        }
    }

    /// 입력값으로 정의를 만든다. 숫자로 읽을 수 없는 속성은 비워 두고 문제 목록에 적는다.
    pub fn build_schema(drafts: &[FieldDraft]) -> (FormSchema, Vec<Problem>) {
        let mut problems = Vec::new();
        let fields = drafts
            .iter()
            .enumerate()
            .map(|(index, draft)| {
                let path = format!("fields[{index}]");
                FieldDef {
                    key: draft.key.trim().to_owned(),
                    label: draft.label.clone(),
                    kind: build_kind(draft, &path, &mut problems),
                    required: draft.required,
                    help: Some(draft.help.clone()).filter(|help| !help.trim().is_empty()),
                }
            })
            .collect();
        (FormSchema { fields }, problems)
    }

    /// 오류 위치의 마지막 이름: `fields[0].options[2]` → `options`.
    pub fn error_part(path: &str) -> &str {
        let last = path.rsplit('.').next().unwrap_or(path);
        last.split('[').next().unwrap_or(last)
    }

    /// 오류 위치가 가리키는 필드의 순서: `fields[3].label` → `3`.
    pub fn field_index(path: &str) -> Option<usize> {
        path.strip_prefix("fields[")?
            .split(']')
            .next()?
            .parse()
            .ok()
    }

    /// 위치별 오류 문구.
    pub type SchemaErrors = BTreeMap<String, String>;

    /// 화면에서 미리 하는 검증. 서버와 같은 `FormSchema::validate`를 부르고, 숫자가 아닌 속성도 잡는다.
    pub fn schema_errors(drafts: &[FieldDraft]) -> SchemaErrors {
        let (schema, problems) = build_schema(drafts);
        let mut errors: SchemaErrors = problems
            .into_iter()
            .map(|(path, reason)| {
                let message = text::schema_error_message(error_part(&path), reason);
                (path, message)
            })
            .collect();
        if let Err(found) = schema.validate() {
            for error in found {
                let message =
                    text::schema_error_message(error_part(&error.key), error.reason.code());
                errors.entry(error.key).or_insert(message);
            }
        }
        errors
    }

    /// 서버의 `error.fields` 가운데 필드 정의의 오류(`schema.fields[0].key`)를 위치별 문구로.
    /// 다른 위치의 오류와 `schema.` 뒤가 `fields`로 시작하지 않는 것은 버린다.
    pub fn server_schema_errors(fields: &[FieldErrorDto]) -> SchemaErrors {
        fields
            .iter()
            .filter_map(|error| {
                let path = error.key.strip_prefix("schema.")?;
                path.starts_with("fields").then(|| {
                    (
                        path.to_owned(),
                        text::schema_error_message(error_part(path), &error.reason),
                    )
                })
            })
            .collect()
    }

    /// 이 위치의 오류 문구. 목록 속 항목의 오류(`….options[2]`)는 목록(`….options`)에서 보여 준다.
    pub fn error_at(errors: &SchemaErrors, path: &str) -> String {
        if let Some(message) = errors.get(path) {
            return message.clone();
        }
        let item = format!("{path}[");
        errors
            .iter()
            .find(|(key, _)| {
                key.strip_prefix(&item)
                    .is_some_and(|rest| !rest.contains('.'))
            })
            .map(|(_, message)| message.clone())
            .unwrap_or_default()
    }

    /// 이 위치(필드나 열) 아래에 오류가 하나라도 있는지.
    pub fn has_error_under(errors: &SchemaErrors, path: &str) -> bool {
        let below = format!("{path}.");
        errors.keys().any(|key| key.starts_with(&below))
    }

    /// 쓰지 않은 가장 작은 번호의 키: `field_1`, `field_2` …
    pub fn next_key(prefix: &str, taken: &[String]) -> String {
        (1..)
            .map(|n| format!("{prefix}_{n}"))
            .find(|candidate| !taken.contains(candidate))
            .unwrap_or_else(|| format!("{prefix}_1"))
    }

    /// 이름으로 키를 만든다. 영문·숫자 이름이면 `snake_case`로 옮기고(`Unit Price` → `unit_price`),
    /// 한글처럼 옮길 수 없으면 `fallback`(보통 `field_N`)을 쓴다. 이미 쓰는 키와 겹치면 번호를 붙인다.
    pub fn suggest_key(label: &str, taken: &[String], fallback: &str) -> String {
        let mut slug = String::new();
        for c in label.trim().chars() {
            if c.is_ascii_alphanumeric() {
                slug.push(c.to_ascii_lowercase());
            } else if !slug.is_empty() && !slug.ends_with('_') {
                slug.push('_');
            }
        }
        let has_other = label.chars().any(|c| !c.is_ascii() && c.is_alphanumeric());
        let slug: String = slug
            .trim_end_matches('_')
            .chars()
            .take(MAX_KEY_LEN - 4)
            .collect();
        let starts_well = slug.as_bytes().first().is_some_and(u8::is_ascii_lowercase);
        if has_other || !starts_well {
            return fallback.to_owned();
        }
        if !taken.contains(&slug) {
            return slug;
        }
        (2..)
            .map(|n| format!("{slug}_{n}"))
            .find(|candidate| !taken.contains(candidate))
            .unwrap_or(slug)
    }

    fn all_uids(drafts: &[FieldDraft]) -> impl Iterator<Item = u32> + '_ {
        drafts.iter().flat_map(|draft| {
            std::iter::once(draft.uid).chain(draft.columns.iter().map(|column| column.uid))
        })
    }

    /// 아직 쓰지 않은 줄 번호.
    pub fn next_uid(drafts: &[FieldDraft]) -> u32 {
        all_uids(drafts).max().unwrap_or(0) + 1
    }

    fn keys(list: &[FieldDraft]) -> Vec<String> {
        list.iter().map(|draft| draft.key.clone()).collect()
    }

    fn blank(uid: u32, key: String) -> FieldDraft {
        FieldDraft {
            uid,
            key,
            auto_key: true,
            kind: "text".to_owned(),
            ..FieldDraft::default()
        }
    }

    /// 새 필드를 맨 뒤에 더한다. 키는 `field_N`으로 시작한다.
    pub fn add_field(drafts: &[FieldDraft]) -> Vec<FieldDraft> {
        let mut out = drafts.to_vec();
        out.push(blank(next_uid(drafts), next_key("field", &keys(drafts))));
        out
    }

    /// 표 필드에 새 열을 더한다. `uid`는 [`next_uid`]로 받은 값이다.
    pub fn add_column(field: &FieldDraft, uid: u32) -> FieldDraft {
        let mut out = field.clone();
        out.columns
            .push(blank(uid, next_key("col", &keys(&field.columns))));
        out
    }

    pub fn remove_at(list: &[FieldDraft], index: usize) -> Vec<FieldDraft> {
        list.iter()
            .enumerate()
            .filter(|(at, _)| *at != index)
            .map(|(_, draft)| draft.clone())
            .collect()
    }

    /// 한 칸 위(`up`)나 아래로 옮긴다. 끝에서는 그대로다.
    pub fn move_item(list: &[FieldDraft], index: usize, up: bool) -> Vec<FieldDraft> {
        let mut out = list.to_vec();
        let target = if up {
            index.checked_sub(1)
        } else {
            index.checked_add(1).filter(|next| *next < out.len())
        };
        if let Some(target) = target.filter(|_| index < out.len()) {
            out.swap(index, target);
        }
        out
    }

    pub fn replace_at(list: &[FieldDraft], index: usize, draft: FieldDraft) -> Vec<FieldDraft> {
        let mut out = list.to_vec();
        if let Some(slot) = out.get_mut(index) {
            *slot = draft;
        }
        out
    }

    /// 이름을 바꾼다. 키를 손대지 않았으면 이름을 따라 키도 바꾼다(`taken`: 다른 필드의 키).
    pub fn set_label(
        draft: &FieldDraft,
        label: &str,
        taken: &[String],
        prefix: &str,
    ) -> FieldDraft {
        let mut out = draft.clone();
        out.label = label.to_owned();
        if out.auto_key {
            // 옮길 수 없는 이름이면 지금의 `field_N`을 그대로 두고, 그것도 아니면 새 번호를 준다.
            let numbered = draft
                .key
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('_'))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            let fallback = if numbered && !taken.contains(&draft.key) {
                draft.key.clone()
            } else {
                next_key(prefix, taken)
            };
            out.key = suggest_key(label, taken, &fallback);
        }
        out
    }

    /// 키를 손으로 고친다. 그 뒤로는 이름을 바꿔도 키가 따라 바뀌지 않는다.
    pub fn set_key(draft: &FieldDraft, key: &str) -> FieldDraft {
        let mut out = draft.clone();
        out.key = key.to_owned();
        out.auto_key = false;
        out
    }

    /// 종류를 바꾼다. 표로 바꾸면 열을 하나 넣어 둔다(`uid`: [`next_uid`]).
    pub fn set_kind(draft: &FieldDraft, code: &str, uid: u32) -> FieldDraft {
        let mut out = draft.clone();
        out.kind = code.to_owned();
        if code == "table" && out.columns.is_empty() {
            out = add_column(&out, uid);
        }
        out
    }

    /// 다른 줄들이 쓰는 키(자기 자신은 뺀다).
    pub fn other_keys(list: &[FieldDraft], index: usize) -> Vec<String> {
        list.iter()
            .enumerate()
            .filter(|(at, _)| *at != index)
            .map(|(_, draft)| draft.key.clone())
            .collect()
    }
}

/// 필드 목록 편집기. 필드를 더하고·빼고·옮기고, 펼쳐서 속성을 고친다.
#[component]
pub fn SchemaBuilder(
    fields: Vec<FieldDraft>,
    /// 위치(`fields[0].label` 등)별 오류 문구.
    #[props(default)]
    errors: BTreeMap<String, String>,
    onchange: EventHandler<Vec<FieldDraft>>,
) -> Element {
    // 펼쳐 둔 필드(줄 번호). 한 번에 하나만 펼친다.
    let mut open = use_signal(|| None::<u32>);
    let count = fields.len();
    let list_error = logic::error_at(&errors, "fields");

    rsx! {
        div { class: "fb",
            if fields.is_empty() {
                p { class: "line-empty", "아직 필드가 없어요. 필드를 추가해 입력 항목을 만들어 보세요." }
            } else {
                ol { class: "fb-list",
                    for (index , field) in fields.iter().enumerate() {
                        li { key: "{field.uid}", class: "fb-field",
                            div { class: "fb-head",
                                button {
                                    class: "fb-toggle",
                                    r#type: "button",
                                    "aria-expanded": open() == Some(field.uid),
                                    "aria-label": "{index + 1}번 필드 {field.label} 속성 펼치기",
                                    onclick: {
                                        let uid = field.uid;
                                        move |_| open.set(if open() == Some(uid) { None } else { Some(uid) })
                                    },
                                    span { class: "line-step-order", "aria-hidden": "true", "{index + 1}" }
                                    span { class: "fb-name",
                                        if field.label.trim().is_empty() { "이름 없는 필드" } else { "{field.label}" }
                                    }
                                    span { class: "badge-row",
                                        Badge { tone: Tone::Neutral, {crate::text::field_kind_label(&logic::kind_of(&field.kind))} }
                                        if field.required {
                                            Badge { tone: Tone::Info, "필수" }
                                        }
                                        if logic::has_error_under(&errors, &format!("fields[{index}]")) {
                                            Badge { tone: Tone::Danger, "확인 필요" }
                                        }
                                    }
                                }
                                div { class: "line-step-actions",
                                    button {
                                        class: "icon-btn line-step-up",
                                        r#type: "button",
                                        "aria-label": "{index + 1}번 필드 위로",
                                        disabled: index == 0,
                                        onclick: {
                                            let fields = fields.clone();
                                            move |_| onchange.call(logic::move_item(&fields, index, true))
                                        },
                                        Icon { name: IconName::Chevron }
                                    }
                                    button {
                                        class: "icon-btn line-step-down",
                                        r#type: "button",
                                        "aria-label": "{index + 1}번 필드 아래로",
                                        disabled: index + 1 == count,
                                        onclick: {
                                            let fields = fields.clone();
                                            move |_| onchange.call(logic::move_item(&fields, index, false))
                                        },
                                        Icon { name: IconName::Chevron }
                                    }
                                    button {
                                        class: "icon-btn",
                                        r#type: "button",
                                        "aria-label": "{index + 1}번 필드 지우기",
                                        onclick: {
                                            let fields = fields.clone();
                                            move |_| onchange.call(logic::remove_at(&fields, index))
                                        },
                                        Icon { name: IconName::Close }
                                    }
                                }
                            }
                            if open() == Some(field.uid) {
                                div { class: "fb-body",
                                    DraftEditor {
                                        draft: field.clone(),
                                        path: format!("fields[{index}]"),
                                        taken: logic::other_keys(&fields, index),
                                        next_uid: logic::next_uid(&fields),
                                        errors: errors.clone(),
                                        onchange: {
                                            let fields = fields.clone();
                                            move |next: FieldDraft| onchange.call(logic::replace_at(&fields, index, next))
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if !list_error.is_empty() {
                p { class: "field-error", role: "alert", "{list_error}" }
            }
            div { class: "fb-foot",
                Button {
                    kind: ButtonKind::Weak,
                    small: true,
                    disabled: count >= MAX_FIELDS,
                    onclick: {
                        let fields = fields.clone();
                        move |_| {
                            let next = logic::add_field(&fields);
                            open.set(next.last().map(|field| field.uid));
                            onchange.call(next);
                        }
                    },
                    Icon { name: IconName::Plus }
                    "필드 추가"
                }
                span { class: "meta tnum", "{count} / {MAX_FIELDS}개" }
            }
        }
    }
}

/// 필드 하나(또는 표의 열 하나)의 속성 편집.
#[component]
fn DraftEditor(
    draft: FieldDraft,
    /// 이 줄의 위치(`fields[0]`, `fields[0].columns[1]`). 오류를 찾는 열쇠다.
    #[props(into)]
    path: String,
    /// 같은 묶음의 다른 줄이 쓰는 키.
    taken: Vec<String>,
    /// 새 열에 줄 번호.
    #[props(default)]
    next_uid: u32,
    /// 표의 열인지(열에는 안내 문구와 일부 종류가 없다).
    #[props(default)]
    column: bool,
    errors: BTreeMap<String, String>,
    onchange: EventHandler<FieldDraft>,
) -> Element {
    let error = |part: &str| logic::error_at(&errors, &format!("{path}.{part}"));
    let kinds: &[&str] = if column {
        &logic::COLUMN_KINDS
    } else {
        &logic::KINDS
    };
    let prefix = if column { "col" } else { "field" };
    let column_count = draft.columns.len();
    // 속성 하나를 바꾼 새 값을 올린다.
    let edit = {
        let draft = draft.clone();
        move |change: fn(&mut FieldDraft, String), value: String| {
            let mut next = draft.clone();
            change(&mut next, value);
            onchange.call(next);
        }
    };

    rsx! {
        div { class: "form fb-editor",
            div { class: "form-row",
                TextField {
                    label: "이름",
                    value: draft.label.clone(),
                    required: true,
                    maxlength: MAX_LABEL_CHARS as i64,
                    error: error("label"),
                    oninput: {
                        let draft = draft.clone();
                        let taken = taken.clone();
                        move |value: String| onchange.call(logic::set_label(&draft, &value, &taken, prefix))
                    },
                }
                TextField {
                    label: "키",
                    value: draft.key.clone(),
                    required: true,
                    maxlength: MAX_KEY_LEN as i64,
                    hint: "영문 소문자로 시작하고 소문자·숫자·_만 써요.",
                    error: error("key"),
                    oninput: {
                        let draft = draft.clone();
                        move |value: String| onchange.call(logic::set_key(&draft, &value))
                    },
                }
            }
            div { class: "form-row",
                SelectField {
                    label: "종류",
                    value: draft.kind.clone(),
                    options: logic::kind_options(kinds),
                    onchange: {
                        let draft = draft.clone();
                        move |value: String| onchange.call(logic::set_kind(&draft, &value, next_uid))
                    },
                }
                div { class: "fb-required",
                    Checkbox {
                        label: "필수",
                        checked: draft.required,
                        hint: if draft.kind == "checkbox" { "꼭 체크해야 문서를 낼 수 있어요." } else { "" },
                        onchange: {
                            let draft = draft.clone();
                            move |checked: bool| {
                                let mut next = draft.clone();
                                next.required = checked;
                                onchange.call(next);
                            }
                        },
                    }
                }
            }
            if !error("kind").is_empty() {
                p { class: "field-error", role: "alert", {error("kind")} }
            }
            if !column {
                TextField {
                    label: "안내 문구",
                    value: draft.help.clone(),
                    maxlength: MAX_HELP_CHARS as i64,
                    hint: "입력란 아래에 보여 줄 설명이에요. 없어도 돼요.",
                    error: error("help"),
                    oninput: {
                        let edit = edit.clone();
                        move |value: String| edit(|draft, value| draft.help = value, value)
                    },
                }
            }
            match draft.kind.as_str() {
                "text" | "textarea" => rsx! {
                    TextField {
                        label: "최대 글자 수",
                        value: draft.max_len.clone(),
                        maxlength: 5,
                        hint: if draft.kind == "text" { "비워 두면 200자예요." } else { "비워 두면 5,000자예요." },
                        error: error("max_len"),
                        oninput: {
                            let edit = edit.clone();
                            move |value: String| edit(|draft, value| draft.max_len = value, value)
                        },
                    }
                },
                "number" => rsx! {
                    div { class: "form-row",
                        TextField {
                            label: "최솟값",
                            value: draft.min.clone(),
                            maxlength: 20,
                            hint: "비워 두면 제한이 없어요.",
                            error: error("min"),
                            oninput: {
                                let edit = edit.clone();
                                move |value: String| edit(|draft, value| draft.min = value, value)
                            },
                        }
                        TextField {
                            label: "최댓값",
                            value: draft.max.clone(),
                            maxlength: 20,
                            error: error("max"),
                            oninput: {
                                let edit = edit.clone();
                                move |value: String| edit(|draft, value| draft.max = value, value)
                            },
                        }
                    }
                },
                "select" => rsx! {
                    div { class: "field-with-count",
                        TextArea {
                            label: "선택지",
                            value: draft.options.clone(),
                            rows: 4,
                            required: true,
                            maxlength: 2000,
                            placeholder: "한 줄에 하나씩 적어 주세요",
                            oninput: {
                                let edit = edit.clone();
                                move |value: String| edit(|draft, value| draft.options = value, value)
                            },
                        }
                        if error("options").is_empty() {
                            span { class: "field-hint", "한 줄에 하나씩, 30개까지 적을 수 있어요." }
                        } else {
                            span { class: "field-error", role: "alert", {error("options")} }
                        }
                    }
                },
                "table" if !column => rsx! {
                    div { class: "fieldset",
                        span { class: "field-label", "열 ({column_count} / {MAX_COLUMNS})" }
                        ol { class: "fb-columns",
                            for (index , item) in draft.columns.iter().enumerate() {
                                li { key: "{item.uid}", class: "fb-column",
                                    div { class: "fb-column-head",
                                        strong { "{index + 1}번 열" }
                                        div { class: "line-step-actions",
                                            button {
                                                class: "icon-btn line-step-up",
                                                r#type: "button",
                                                "aria-label": "{index + 1}번 열 앞으로",
                                                disabled: index == 0,
                                                onclick: {
                                                    let draft = draft.clone();
                                                    move |_| {
                                                        let mut next = draft.clone();
                                                        next.columns = logic::move_item(&draft.columns, index, true);
                                                        onchange.call(next);
                                                    }
                                                },
                                                Icon { name: IconName::Chevron }
                                            }
                                            button {
                                                class: "icon-btn line-step-down",
                                                r#type: "button",
                                                "aria-label": "{index + 1}번 열 뒤로",
                                                disabled: index + 1 == column_count,
                                                onclick: {
                                                    let draft = draft.clone();
                                                    move |_| {
                                                        let mut next = draft.clone();
                                                        next.columns = logic::move_item(&draft.columns, index, false);
                                                        onchange.call(next);
                                                    }
                                                },
                                                Icon { name: IconName::Chevron }
                                            }
                                            button {
                                                class: "icon-btn",
                                                r#type: "button",
                                                "aria-label": "{index + 1}번 열 지우기",
                                                onclick: {
                                                    let draft = draft.clone();
                                                    move |_| {
                                                        let mut next = draft.clone();
                                                        next.columns = logic::remove_at(&draft.columns, index);
                                                        onchange.call(next);
                                                    }
                                                },
                                                Icon { name: IconName::Close }
                                            }
                                        }
                                    }
                                    DraftEditor {
                                        draft: item.clone(),
                                        path: format!("{path}.columns[{index}]"),
                                        taken: logic::other_keys(&draft.columns, index),
                                        column: true,
                                        errors: errors.clone(),
                                        onchange: {
                                            let draft = draft.clone();
                                            move |changed: FieldDraft| {
                                                let mut next = draft.clone();
                                                next.columns = logic::replace_at(&draft.columns, index, changed);
                                                onchange.call(next);
                                            }
                                        },
                                    }
                                }
                            }
                        }
                        if !error("columns").is_empty() {
                            p { class: "field-error", role: "alert", {error("columns")} }
                        }
                        div { class: "fb-foot",
                            Button {
                                kind: ButtonKind::Grey,
                                small: true,
                                disabled: column_count >= MAX_COLUMNS,
                                onclick: {
                                    let draft = draft.clone();
                                    move |_| onchange.call(logic::add_column(&draft, next_uid))
                                },
                                Icon { name: IconName::Plus }
                                "열 추가"
                            }
                            span { class: "field-hint", "금액 열은 합계를 자동으로 보여 줘요." }
                        }
                    }
                },
                _ => rsx! {},
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::logic::*;
    use pango_api::FieldErrorDto;
    use pango_core::form_schema::{ColumnDef, FieldDef, FieldKind, FormSchema, MAX_FIELDS};

    fn draft(key: &str, label: &str, kind: &str) -> FieldDraft {
        FieldDraft {
            uid: 1,
            key: key.into(),
            label: label.into(),
            kind: kind.into(),
            ..FieldDraft::default()
        }
    }

    fn sample_schema() -> FormSchema {
        FormSchema {
            fields: vec![
                FieldDef {
                    key: "title".into(),
                    label: "제목".into(),
                    kind: FieldKind::Text { max_len: Some(50) },
                    required: true,
                    help: Some("짧게 써 주세요".into()),
                },
                FieldDef {
                    key: "count".into(),
                    label: "수량".into(),
                    kind: FieldKind::Number {
                        min: Some(1),
                        max: Some(99),
                    },
                    required: false,
                    help: None,
                },
                FieldDef {
                    key: "pay".into(),
                    label: "결제".into(),
                    kind: FieldKind::Select {
                        options: vec!["카드".into(), "현금".into()],
                    },
                    required: true,
                    help: None,
                },
                FieldDef {
                    key: "items".into(),
                    label: "내역".into(),
                    kind: FieldKind::Table {
                        columns: vec![
                            ColumnDef {
                                key: "name".into(),
                                label: "품목".into(),
                                kind: FieldKind::Text { max_len: Some(200) },
                                required: true,
                            },
                            ColumnDef {
                                key: "amount".into(),
                                label: "금액".into(),
                                kind: FieldKind::Money,
                                required: true,
                            },
                        ],
                    },
                    required: true,
                    help: None,
                },
                FieldDef {
                    key: "who".into(),
                    label: "담당".into(),
                    kind: FieldKind::Member,
                    required: false,
                    help: None,
                },
            ],
        }
    }

    #[test]
    fn schema_round_trips_through_the_editor() {
        let schema = sample_schema();
        let drafts = drafts_from_schema(&schema);
        assert_eq!(drafts.len(), 5);
        assert_eq!(drafts[1].min, "1");
        assert_eq!(drafts[2].options, "카드\n현금");
        assert_eq!(drafts[3].columns.len(), 2);
        assert!(
            drafts.iter().all(|draft| !draft.auto_key),
            "저장된 키는 이름을 따라 바뀌지 않는다"
        );
        // 줄 번호는 필드와 열을 통틀어 겹치지 않는다.
        let mut uids: Vec<u32> = drafts
            .iter()
            .flat_map(|d| std::iter::once(d.uid).chain(d.columns.iter().map(|c| c.uid)))
            .collect();
        uids.sort_unstable();
        uids.dedup();
        assert_eq!(uids.len(), 7);
        assert_eq!(next_uid(&drafts), 8);
        let (built, problems) = build_schema(&drafts);
        assert!(problems.is_empty());
        assert_eq!(built, schema);
        assert!(schema_errors(&drafts).is_empty());
        assert!(drafts_from_schema(&FormSchema::default()).is_empty());
        assert_eq!(next_uid(&[]), 1);
    }

    #[test]
    fn every_kind_is_known_and_labelled() {
        for code in KINDS {
            assert_eq!(kind_of(code).code(), code);
        }
        for code in COLUMN_KINDS {
            assert!(KINDS.contains(&code));
        }
        assert_eq!(kind_of("nope").code(), "text");
        let options = kind_options(&KINDS);
        assert_eq!(options.len(), KINDS.len());
        assert_eq!(options[3], ("money".into(), "금액".into()));
        assert!(options.iter().all(|(_, label)| !label.is_empty()));
    }

    #[test]
    fn options_are_one_per_line() {
        assert_eq!(
            options_list(" 카드 \n\n현금\r\n  \n계좌 이체"),
            vec!["카드", "현금", "계좌 이체"]
        );
        assert!(options_list("").is_empty());
        assert!(options_list(" \n ").is_empty());
    }

    #[test]
    fn numbers_that_do_not_parse_are_reported_in_place() {
        let mut text = draft("a", "가", "text");
        text.max_len = "많이".into();
        let mut number = draft("b", "나", "number");
        number.min = "1.5".into();
        number.max = " 1,000 ".into();
        let mut table = draft("c", "다", "table");
        let mut column = draft("x", "열", "text");
        column.max_len = "-3".into();
        table.columns = vec![column];
        let (schema, problems) = build_schema(&[text, number, table]);
        assert_eq!(
            problems,
            vec![
                ("fields[0].max_len".to_owned(), "invalid_format"),
                ("fields[1].min".to_owned(), "invalid_format"),
                ("fields[2].columns[0].max_len".to_owned(), "invalid_format"),
            ]
        );
        assert_eq!(
            schema.fields[1].kind,
            FieldKind::Number {
                min: None,
                max: Some(1000)
            }
        );
        assert_eq!(schema.fields[0].kind, FieldKind::Text { max_len: None });
    }

    #[test]
    fn client_validation_uses_the_core_rules() {
        let mut select = draft("Bad Key", "", "select");
        select.options = "가\n가".into();
        let mut number = draft("n", "숫자", "number");
        number.min = "5".into();
        number.max = "1".into();
        let twin = draft("n", "또 숫자", "money");
        let table = draft("t", "표", "table");
        let errors = schema_errors(&[select, number, twin, table]);
        assert_eq!(
            errors["fields[0].key"],
            "키는 영문 소문자·숫자·밑줄(_)만 쓸 수 있어요."
        );
        assert_eq!(errors["fields[0].label"], "이름을 입력해 주세요.");
        assert_eq!(
            error_at(&errors, "fields[0].options"),
            "같은 선택지가 두 번 들어 있어요."
        );
        assert_eq!(errors["fields[1].min"], "최솟값이 최댓값보다 커요.");
        assert_eq!(
            errors["fields[2].key"],
            "같은 키가 이미 있어요. 다른 키를 써 주세요."
        );
        assert_eq!(errors["fields[3].columns"], "열을 하나 이상 만들어 주세요.");
        assert!(has_error_under(&errors, "fields[0]"));
        assert!(!has_error_under(&errors, "fields[9]"));
        // `fields[1]`의 오류가 `fields[10]`으로 번지지 않는다.
        let tenth = SchemaErrors::from([("fields[10].key".to_owned(), "x".to_owned())]);
        assert!(!has_error_under(&tenth, "fields[1]"));
        assert!(has_error_under(&tenth, "fields[10]"));
        assert_eq!(error_at(&errors, "fields[9].label"), "");

        let many: Vec<FieldDraft> = (0..=MAX_FIELDS)
            .map(|n| draft(&format!("f{n}"), "이름", "text"))
            .collect();
        assert_eq!(
            error_at(&schema_errors(&many), "fields"),
            "필드는 40개까지 만들 수 있어요."
        );
        // 숫자가 아닌 속성의 문구가 검증 문구보다 먼저다.
        let mut text = draft("a", "가", "text");
        text.max_len = "x".into();
        assert_eq!(
            schema_errors(&[text])["fields[0].max_len"],
            "최대 글자 수는 숫자로 입력해 주세요."
        );
    }

    #[test]
    fn server_error_keys_map_back_onto_builder_rows() {
        let dto = |key: &str, reason: &str| FieldErrorDto {
            key: key.into(),
            row: None,
            column: None,
            reason: reason.into(),
        };
        let errors = server_schema_errors(&[
            dto("schema.fields[2].key", "duplicate"),
            dto("schema.fields[0].columns[1].options[3]", "too_long"),
            dto("schema.fields", "too_long"),
            dto("name", "empty"),
            dto("schema.evil", "empty"),
            dto("schema.", "empty"),
        ]);
        assert_eq!(errors.len(), 3);
        assert_eq!(
            errors["fields[2].key"],
            "같은 키가 이미 있어요. 다른 키를 써 주세요."
        );
        assert_eq!(
            error_at(&errors, "fields[0].columns[1].options"),
            "선택지는 30개까지, 하나에 50자까지 쓸 수 있어요."
        );
        // 열의 오류는 표 필드의 `columns` 자리에 잘못 붙지 않는다.
        assert_eq!(error_at(&errors, "fields[0].columns"), "");
        assert!(has_error_under(&errors, "fields[0]"));
        assert!(has_error_under(&errors, "fields[0].columns[1]"));
        assert_eq!(
            error_at(&errors, "fields"),
            "필드는 40개까지 만들 수 있어요."
        );
        assert!(server_schema_errors(&[]).is_empty());
    }

    #[test]
    fn error_paths_are_parsed_defensively() {
        assert_eq!(error_part("fields[0].options[2]"), "options");
        assert_eq!(error_part("fields[0].key"), "key");
        assert_eq!(error_part("fields"), "fields");
        assert_eq!(error_part(""), "");
        assert_eq!(field_index("fields[3].label"), Some(3));
        assert_eq!(field_index("fields[12].columns[1].key"), Some(12));
        assert_eq!(field_index("fields"), None);
        assert_eq!(field_index("fields[x].key"), None);
        assert_eq!(field_index("fields[-1]"), None);
        assert_eq!(field_index("other[1]"), None);
        assert_eq!(field_index("fields[99999999999999999999999]"), None);
    }

    #[test]
    fn keys_are_generated_from_labels() {
        let none: Vec<String> = Vec::new();
        assert_eq!(suggest_key("Unit Price", &none, "field_1"), "unit_price");
        assert_eq!(suggest_key("  total(KRW) ", &none, "field_1"), "total_krw");
        assert_eq!(suggest_key("a--b__c", &none, "field_1"), "a_b_c");
        // 한글이 섞였거나 소문자로 시작하지 못하면 번호 키를 쓴다.
        assert_eq!(suggest_key("금액", &none, "field_3"), "field_3");
        assert_eq!(suggest_key("금액 amount", &none, "field_3"), "field_3");
        assert_eq!(suggest_key("2nd", &none, "field_3"), "field_3");
        assert_eq!(suggest_key("", &none, "field_3"), "field_3");
        assert_eq!(suggest_key("!!!", &none, "field_3"), "field_3");
        // 겹치면 번호를 붙인다.
        let taken = vec!["amount".to_owned(), "amount_2".to_owned()];
        assert_eq!(suggest_key("Amount", &taken, "field_1"), "amount_3");
        let long = suggest_key(&"a".repeat(200), &none, "field_1");
        assert!(long.len() <= 40);
        assert_eq!(next_key("field", &none), "field_1");
        assert_eq!(
            next_key("field", &["field_1".to_owned(), "field_3".to_owned()]),
            "field_2"
        );
    }

    #[test]
    fn label_edits_follow_until_the_key_is_touched() {
        let fields = add_field(&[]);
        assert_eq!(fields[0].key, "field_1");
        assert!(fields[0].auto_key);
        assert_eq!(fields[0].kind, "text");
        let none: Vec<String> = Vec::new();
        // 한글 이름은 번호 키를 그대로 둔다.
        let korean = set_label(&fields[0], "지출 금액", &none, "field");
        assert_eq!(
            (korean.label.as_str(), korean.key.as_str()),
            ("지출 금액", "field_1")
        );
        // 영문 이름은 키로 옮겨지고, 다시 한글로 바꾸면 번호 키로 돌아온다.
        let english = set_label(&korean, "Amount", &none, "field");
        assert_eq!(english.key, "amount");
        let back = set_label(&english, "금액", &["field_1".to_owned()], "field");
        assert_eq!(back.key, "field_2");
        // 키를 손으로 고친 뒤에는 이름을 바꿔도 그대로다.
        let manual = set_key(&english, "my_key");
        assert!(!manual.auto_key);
        assert_eq!(set_label(&manual, "Other", &none, "field").key, "my_key");

        let two = add_field(&fields);
        assert_eq!(two[1].key, "field_2");
        assert_ne!(two[0].uid, two[1].uid);
    }

    #[test]
    fn list_edits_keep_everything_else() {
        let list = vec![
            draft("a", "가", "text"),
            draft("b", "나", "text"),
            draft("c", "다", "text"),
        ];
        let keys = |list: &[FieldDraft]| {
            list.iter()
                .map(|d| d.key.clone())
                .collect::<Vec<_>>()
                .join("")
        };
        assert_eq!(keys(&move_item(&list, 1, true)), "bac");
        assert_eq!(keys(&move_item(&list, 1, false)), "acb");
        assert_eq!(keys(&move_item(&list, 0, true)), "abc");
        assert_eq!(keys(&move_item(&list, 2, false)), "abc");
        assert_eq!(keys(&move_item(&list, 9, true)), "abc");
        assert_eq!(keys(&move_item(&list, usize::MAX, false)), "abc");
        assert_eq!(keys(&remove_at(&list, 1)), "ac");
        assert_eq!(keys(&remove_at(&list, 9)), "abc");
        assert_eq!(keys(&replace_at(&list, 0, draft("z", "새", "text"))), "zbc");
        assert_eq!(keys(&replace_at(&list, 9, draft("z", "새", "text"))), "abc");
        assert_eq!(other_keys(&list, 1), vec!["a", "c"]);
    }

    #[test]
    fn switching_to_a_table_starts_with_one_column() {
        let field = draft("items", "내역", "text");
        let table = set_kind(&field, "table", 7);
        assert_eq!(table.kind, "table");
        assert_eq!(table.columns.len(), 1);
        assert_eq!(
            (table.columns[0].uid, table.columns[0].key.as_str()),
            (7, "col_1")
        );
        // 이미 열이 있으면 그대로 둔다.
        assert_eq!(set_kind(&table, "table", 8).columns.len(), 1);
        let more = add_column(&table, 8);
        assert_eq!(more.columns[1].key, "col_2");
        // 다른 종류로 바꿔도 입력해 둔 열은 지우지 않는다(다시 표로 돌아올 수 있게). 정의에는 안 들어간다.
        let money = set_kind(&more, "money", 9);
        assert_eq!(money.columns.len(), 2);
        assert_eq!(build_schema(&[money]).0.fields[0].kind, FieldKind::Money);
    }
}
