//! 결재 양식의 필드를 그리는 하나뿐인 컴포넌트.
//!
//! 기안 화면(입력), 문서 상세(읽기 전용), 양식 관리의 미리보기가 모두 [`FormFields`]를 쓴다.
//! 값은 서버에 보내는 모양 그대로의 JSON 객체(`{필드 key: 값}`)이고, 입력 문자열과 값 사이의
//! 변환·합계·금액 읽기는 [`logic`]의 순수 함수가 맡는다.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use pango_api::org::MemberSummary;
use pango_core::form_schema::{ColumnDef, FieldDef, FieldKind, FormSchema, MAX_ROWS};
use pango_core::id::MemberId;
use serde_json::{Map, Value};

use super::{Button, ButtonKind, CharCount, Icon, IconName, MemberPicker, PickedMember};
use crate::text;

pub use logic::FieldErrors;

/// 값 변환, 계산, 오류 찾기. 모두 순수 함수다.
pub mod logic {
    use std::collections::BTreeMap;

    use pango_api::FieldErrorDto;
    use pango_core::calendar;
    use pango_core::form_schema::{
        ColumnDef, FieldError, FieldKind, FormSchema, MAX_MONEY, TEXTAREA_MAX_LEN, TEXT_MAX_LEN,
    };
    use pango_core::id::MemberId;
    use serde_json::{Map, Value};

    use crate::format;

    pub type Values = Map<String, Value>;
    pub type Row = Map<String, Value>;

    // ───────────── 입력 문자열 → 값 ─────────────

    /// 글 입력. 공백뿐이면 입력하지 않은 것이다.
    pub fn text_value(raw: &str) -> Option<Value> {
        (!raw.trim().is_empty()).then(|| Value::String(raw.to_owned()))
    }

    /// 정수 입력. 쉼표는 무시한다. 숫자로 읽을 수 없으면 입력한 글자를 그대로 둬서(문자열)
    /// 검증이 형식 오류로 잡게 한다.
    pub fn number_value(raw: &str) -> Option<Value> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        let digits: String = trimmed.chars().filter(|c| *c != ',').collect();
        Some(match digits.parse::<i64>() {
            Ok(number) => Value::from(number),
            Err(_) => Value::String(raw.to_owned()),
        })
    }

    /// 금액 입력. 쉼표·공백·끝의 `원`은 무시하고 숫자만 정수로 읽는다. 그 밖의 글자가 섞였거나
    /// 너무 큰 수면 입력한 글자를 그대로 둔다(검증이 형식 오류로 잡는다).
    pub fn money_value(raw: &str) -> Option<Value> {
        let cleaned: String = raw
            .trim()
            .trim_end_matches('원')
            .chars()
            .filter(|c| *c != ',' && !c.is_whitespace())
            .collect();
        if cleaned.is_empty() {
            return None;
        }
        let all_digits = cleaned.bytes().all(|b| b.is_ascii_digit());
        Some(match cleaned.parse::<i64>() {
            Ok(amount) if all_digits => Value::from(amount),
            _ => Value::String(raw.to_owned()),
        })
    }

    /// 날짜(`YYYY-MM-DD`)·시각(`HH:MM`)·선택지처럼 브라우저가 모양을 정해 주는 입력.
    pub fn plain_value(raw: &str) -> Option<Value> {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| Value::String(trimmed.to_owned()))
    }

    /// 기간·시간대. 둘 다 비었으면 입력하지 않은 것이고, 하나만 있으면 그대로 둬서 검증이 잡는다.
    pub fn pair_value(start: &str, end: &str) -> Option<Value> {
        let (start, end) = (start.trim(), end.trim());
        if start.is_empty() && end.is_empty() {
            return None;
        }
        let mut object = Map::new();
        object.insert("start".to_owned(), Value::String(start.to_owned()));
        object.insert("end".to_owned(), Value::String(end.to_owned()));
        Some(Value::Object(object))
    }

    /// 체크. 체크하지 않은 것은 입력하지 않은 것이다.
    pub fn checkbox_value(checked: bool) -> Option<Value> {
        checked.then_some(Value::Bool(true))
    }

    /// 표의 칸(열 종류에 따라).
    pub fn cell_value(kind: &FieldKind, raw: &str) -> Option<Value> {
        match kind {
            FieldKind::Number { .. } => number_value(raw),
            FieldKind::Money => money_value(raw),
            FieldKind::Date | FieldKind::Select { .. } => plain_value(raw),
            _ => text_value(raw),
        }
    }

    // ───────────── 값 → 입력 문자열 ─────────────

    /// 글·날짜·선택지 입력란에 넣을 문자열.
    pub fn str_text(value: Option<&Value>) -> String {
        match value {
            Some(Value::String(text)) => text.clone(),
            _ => String::new(),
        }
    }

    /// 정수 입력란의 문자열. 읽지 못한 입력은 쓴 그대로 돌려준다.
    pub fn number_text(value: Option<&Value>) -> String {
        match value {
            Some(Value::Number(number)) => number.to_string(),
            Some(Value::String(raw)) => raw.clone(),
            _ => String::new(),
        }
    }

    /// `1234567` → `1,234,567`.
    pub fn group_thousands(number: i64) -> String {
        let digits = number.unsigned_abs().to_string();
        let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
        if number < 0 {
            out.push('-');
        }
        for (index, digit) in digits.chars().enumerate() {
            if index > 0 && (digits.len() - index) % 3 == 0 {
                out.push(',');
            }
            out.push(digit);
        }
        out
    }

    /// 금액 입력란의 문자열(천 단위 쉼표). 읽지 못한 입력은 쓴 그대로 돌려준다.
    pub fn money_text(value: Option<&Value>) -> String {
        match value {
            Some(Value::Number(number)) => number
                .as_i64()
                .map(group_thousands)
                .unwrap_or_else(|| number.to_string()),
            Some(Value::String(raw)) => raw.clone(),
            _ => String::new(),
        }
    }

    /// 서버가 받는 금액(0~[`MAX_MONEY`]의 정수)이면 그 값.
    pub fn money_amount(value: Option<&Value>) -> Option<i64> {
        value?
            .as_i64()
            .filter(|amount| (0..=MAX_MONEY).contains(amount))
    }

    /// 금액을 한글로 읽는다: `123000` → `십이만 삼천 원`. 음수는 빈 문자열이다.
    ///
    /// 네 자리씩 끊어 만·억·조·경을 붙이고, 십·백·천 앞의 `일`은 읽지 않는다(`일십` → `십`).
    /// 만 이상의 단위 앞에서는 `일`을 읽는다(`10000` → `일만 원`).
    pub fn korean_amount(amount: i64) -> String {
        const DIGITS: [&str; 10] = ["", "일", "이", "삼", "사", "오", "육", "칠", "팔", "구"];
        const SMALL: [&str; 4] = ["", "십", "백", "천"];
        const LARGE: [&str; 5] = ["", "만", "억", "조", "경"];
        if amount < 0 {
            return String::new();
        }
        if amount == 0 {
            return "영 원".to_owned();
        }
        let mut groups = Vec::new();
        let mut rest = amount;
        let mut unit = 0;
        while rest > 0 {
            let group = (rest % 10_000) as usize;
            if group > 0 {
                let mut word = String::new();
                for place in (0..4).rev() {
                    let digit = group / 10_usize.pow(place as u32) % 10;
                    if digit == 0 {
                        continue;
                    }
                    if digit > 1 || place == 0 {
                        word.push_str(DIGITS[digit]);
                    }
                    word.push_str(SMALL[place]);
                }
                word.push_str(LARGE.get(unit).copied().unwrap_or(""));
                groups.push(word);
            }
            rest /= 10_000;
            unit += 1;
        }
        groups.reverse();
        format!("{} 원", groups.join(" "))
    }

    /// 금액 입력란 아래에 보여 줄 읽기. 받을 수 없는 값이면 없다.
    pub fn money_reading(value: Option<&Value>) -> Option<String> {
        money_amount(value).map(korean_amount)
    }

    /// 기간·시간대의 한쪽 끝(`start`/`end`).
    pub fn pair_part(value: Option<&Value>, part: &str) -> String {
        value
            .and_then(Value::as_object)
            .and_then(|object| object.get(part))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }

    /// 기간의 일수(양 끝 포함). 서버의 계산(`computed.days`)과 같다. 읽을 수 없거나 끝이 시작보다
    /// 빠르면 없다.
    pub fn range_days(value: Option<&Value>) -> Option<i64> {
        let date = |part: &str| calendar::parse_date("value", pair_part(value, part).trim()).ok();
        let (start, end) = (date("start")?, date("end")?);
        (end >= start).then(|| (end - start).num_days() + 1)
    }

    pub fn is_checked(value: Option<&Value>) -> bool {
        value.and_then(Value::as_bool).unwrap_or(false)
    }

    // ───────────── 값 객체 편집 ─────────────

    /// 필드 하나의 값을 바꾼 새 객체. `None`이면 키를 지운다(입력하지 않음).
    pub fn set_value(values: &Values, key: &str, value: Option<Value>) -> Values {
        let mut out = values.clone();
        match value {
            Some(value) => {
                out.insert(key.to_owned(), value);
            }
            None => {
                out.remove(key);
            }
        }
        out
    }

    /// 표의 행들. 배열이 아니면 빈 표, 객체가 아닌 행은 빈 행으로 읽는다.
    pub fn table_rows(value: Option<&Value>) -> Vec<Row> {
        value
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|row| row.as_object().cloned().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 행들을 값으로. 행이 없으면 입력하지 않은 것이다.
    pub fn rows_value(rows: Vec<Row>) -> Option<Value> {
        (!rows.is_empty()).then(|| Value::Array(rows.into_iter().map(Value::Object).collect()))
    }

    /// 빈 행을 하나 더한다. 최대 행 수를 넘기지 않는다.
    pub fn add_row(rows: &[Row], max_rows: usize) -> Vec<Row> {
        let mut out = rows.to_vec();
        if out.len() < max_rows {
            out.push(Row::new());
        }
        out
    }

    pub fn remove_row(rows: &[Row], index: usize) -> Vec<Row> {
        rows.iter()
            .enumerate()
            .filter(|(at, _)| *at != index)
            .map(|(_, row)| row.clone())
            .collect()
    }

    /// 한 칸을 바꾼다. 없는 행이면 그대로다.
    pub fn set_cell(rows: &[Row], index: usize, column: &str, value: Option<Value>) -> Vec<Row> {
        let mut out = rows.to_vec();
        if let Some(row) = out.get_mut(index) {
            *row = set_value(row, column, value);
        }
        out
    }

    /// 금액 열의 합계 `(열 key, 합계)`. 서버와 같은 규칙으로, 받을 수 있는 금액(0~[`MAX_MONEY`]의
    /// 정수)만 더한다. 금액 열이 없으면 비어 있다.
    pub fn table_totals(columns: &[ColumnDef], rows: &[Row]) -> Vec<(String, i64)> {
        columns
            .iter()
            .filter(|column| column.kind == FieldKind::Money)
            .map(|column| {
                let sum = rows
                    .iter()
                    .filter_map(|row| money_amount(row.get(&column.key)))
                    .sum();
                (column.key.clone(), sum)
            })
            .collect()
    }

    fn is_blank(value: &Value) -> bool {
        match value {
            Value::Null => true,
            Value::String(text) => text.trim().is_empty(),
            Value::Array(rows) => rows.is_empty(),
            Value::Bool(checked) => !checked,
            _ => false,
        }
    }

    /// 보내기 전 정리: 양식에 없는 키, 비어 있는 값·칸, 통째로 빈 행을 뺀다.
    /// 양식이 바뀌어 남은 옛 키 때문에 문서를 못 내는 일이 없게 한다.
    pub fn prune(schema: &FormSchema, values: &Values) -> Values {
        let mut out = Values::new();
        for field in &schema.fields {
            let Some(value) = values.get(&field.key) else {
                continue;
            };
            let kept = match &field.kind {
                FieldKind::Table { columns } => {
                    let rows: Vec<Row> = table_rows(Some(value))
                        .into_iter()
                        .map(|row| {
                            row.into_iter()
                                .filter(|(key, cell)| {
                                    !is_blank(cell)
                                        && columns.iter().any(|column| column.key == *key)
                                })
                                .collect::<Row>()
                        })
                        .filter(|row| !row.is_empty())
                        .collect();
                    rows_value(rows)
                }
                FieldKind::DateRange | FieldKind::TimeRange => pair_value(
                    &pair_part(Some(value), "start"),
                    &pair_part(Some(value), "end"),
                )
                .or_else(|| (!value.is_object() && !is_blank(value)).then(|| value.clone())),
                _ => (!is_blank(value)).then(|| value.clone()),
            };
            if let Some(kept) = kept {
                out.insert(field.key.clone(), kept);
            }
        }
        out
    }

    /// 글 필드의 최대 글자 수(정의에 없으면 종류의 기본값).
    pub fn max_chars(kind: &FieldKind) -> usize {
        match kind {
            FieldKind::Textarea { max_len } => max_len
                .unwrap_or(TEXTAREA_MAX_LEN)
                .clamp(1, TEXTAREA_MAX_LEN),
            FieldKind::Text { max_len } => max_len.unwrap_or(TEXT_MAX_LEN).clamp(1, TEXT_MAX_LEN),
            _ => TEXT_MAX_LEN,
        }
    }

    // ───────────── 읽기 전용 표시 ─────────────

    fn date_text(raw: &str) -> String {
        calendar::parse_date("value", raw.trim())
            .map(format::date_ymd)
            .unwrap_or_else(|_| raw.to_owned())
    }

    /// 읽기 전용 화면에 보여 줄 값. 입력하지 않았으면 `None`.
    pub fn display_value(
        kind: &FieldKind,
        value: Option<&Value>,
        member_names: &BTreeMap<MemberId, String>,
    ) -> Option<String> {
        let value = value.filter(|value| !matches!(value, Value::Null))?;
        let text = match kind {
            FieldKind::Money => match value.as_i64() {
                Some(amount) => format!("{}원", group_thousands(amount)),
                None => number_text(Some(value)),
            },
            FieldKind::Number { .. } => match value.as_i64() {
                Some(number) => group_thousands(number),
                None => number_text(Some(value)),
            },
            FieldKind::Date => date_text(value.as_str()?),
            FieldKind::DateRange => {
                let (start, end) = (
                    pair_part(Some(value), "start"),
                    pair_part(Some(value), "end"),
                );
                let span = if start == end {
                    date_text(&start)
                } else {
                    format!("{} ~ {}", date_text(&start), date_text(&end))
                };
                match range_days(Some(value)) {
                    Some(days) => format!("{span} ({days}일)"),
                    None => span,
                }
            }
            FieldKind::TimeRange => format!(
                "{} ~ {}",
                pair_part(Some(value), "start"),
                pair_part(Some(value), "end")
            ),
            FieldKind::Checkbox => {
                let label = if is_checked(Some(value)) {
                    "예"
                } else {
                    "아니요"
                };
                label.to_owned()
            }
            FieldKind::Member => {
                let raw = value.as_str()?;
                MemberId::parse(raw.trim())
                    .ok()
                    .and_then(|id| member_names.get(&id).cloned())
                    .unwrap_or_else(|| "알 수 없는 구성원".to_owned())
            }
            FieldKind::Text { .. } | FieldKind::Textarea { .. } | FieldKind::Select { .. } => {
                value.as_str()?.to_owned()
            }
            FieldKind::Table { .. } => return None,
        };
        (!text.trim().is_empty()).then_some(text)
    }

    // ───────────── 오류 ─────────────

    /// 오류 하나. 표의 칸이면 `row`·`column`이 있다.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Issue {
        pub key: String,
        pub row: Option<usize>,
        pub column: Option<String>,
        /// 사유 코드(`empty`, `too_long` …). 문구는 `text::form_value_error`가 고른다.
        pub reason: String,
    }

    /// 필드별 오류 묶음. 화면 검증(`FormSchema::validate_values`)과 서버 응답(`error.fields`)을
    /// 같은 모양으로 담는다.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct FieldErrors(Vec<Issue>);

    impl FieldErrors {
        pub fn from_core(errors: &[FieldError]) -> Self {
            Self(
                errors
                    .iter()
                    .map(|error| Issue {
                        key: error.key.clone(),
                        row: error.row,
                        column: error.column.clone(),
                        reason: error.reason.code().to_owned(),
                    })
                    .collect(),
            )
        }

        pub fn from_api(errors: &[FieldErrorDto]) -> Self {
            Self(
                errors
                    .iter()
                    .map(|error| Issue {
                        key: error.key.clone(),
                        row: error.row.and_then(|row| usize::try_from(row).ok()),
                        column: error.column.clone(),
                        reason: error.reason.clone(),
                    })
                    .collect(),
            )
        }

        pub fn is_empty(&self) -> bool {
            self.0.is_empty()
        }

        /// 필드 전체의 오류(표의 칸 오류는 제외).
        pub fn field(&self, key: &str) -> Option<&str> {
            self.0
                .iter()
                .find(|issue| issue.key == key && issue.row.is_none())
                .map(|issue| issue.reason.as_str())
        }

        /// 표의 한 칸의 오류.
        pub fn cell(&self, key: &str, row: usize, column: &str) -> Option<&str> {
            self.0
                .iter()
                .find(|issue| {
                    issue.key == key
                        && issue.row == Some(row)
                        && issue.column.as_deref() == Some(column)
                })
                .map(|issue| issue.reason.as_str())
        }

        /// 표의 한 행에서 화면의 칸에 붙일 수 없는 오류: 행 자체의 오류이거나 양식에 없는 열의 오류.
        pub fn row(&self, key: &str, row: usize, columns: &[ColumnDef]) -> Option<&str> {
            self.0
                .iter()
                .find(|issue| {
                    issue.key == key
                        && issue.row == Some(row)
                        && issue
                            .column
                            .as_deref()
                            .is_none_or(|name| !columns.iter().any(|column| column.key == name))
                })
                .map(|issue| issue.reason.as_str())
        }

        /// 이 필드에 (칸 오류를 포함해) 오류가 있는지.
        pub fn touches(&self, key: &str) -> bool {
            self.0.iter().any(|issue| issue.key == key)
        }

        /// 화면 순서로 첫 번째 오류가 있는 필드의 키.
        pub fn first_key(&self, schema: &FormSchema) -> Option<String> {
            schema
                .fields
                .iter()
                .find(|field| self.touches(&field.key))
                .map(|field| field.key.clone())
        }

        /// 화면의 어느 필드에도 붙일 수 없는 오류의 수(양식에 없는 키 등).
        pub fn unplaced(&self, schema: &FormSchema) -> usize {
            self.0
                .iter()
                .filter(|issue| !schema.fields.iter().any(|field| field.key == issue.key))
                .count()
        }
    }

    /// 화면에서 미리 하는 검증. 서버와 같은 함수(`FormSchema::validate_values`)를 그대로 부른다.
    pub fn validate(schema: &FormSchema, values: &Values) -> FieldErrors {
        match schema.validate_values(&Value::Object(values.clone())) {
            Ok(_) => FieldErrors::default(),
            Err(errors) => FieldErrors::from_core(&errors),
        }
    }

    /// 필드를 감싼 요소의 DOM id. 첫 오류로 화면을 옮길 때 쓴다.
    pub fn field_dom_id(key: &str) -> String {
        format!("ff-{key}")
    }
}

/// 오류가 있는 필드로 화면을 옮기고 그 안의 첫 입력란에 초점을 준다. 이벤트 핸들러에서 부른다.
pub fn focus_field(key: &str) {
    focus_element(&logic::field_dom_id(key));
}

pub use crate::platform::focus_element;

/// 양식의 필드들.
///
/// - `values`: `{필드 key: 값}`. 바뀌면 새 객체를 `onchange`로 올린다.
/// - `errors`: 필드·칸에 붙일 오류(화면 검증 또는 서버의 `error.fields`).
/// - `readonly`: 값만 보여 주는 문서 모양(상세 화면). 이때 `member_names`로 구성원 이름을 준다.
#[component]
pub fn FormFields(
    schema: FormSchema,
    values: Map<String, Value>,
    #[props(default)] errors: FieldErrors,
    #[props(default)] onchange: EventHandler<Map<String, Value>>,
    #[props(default)] readonly: bool,
    /// `member` 필드에 이미 들어 있는 구성원의 이름.
    #[props(default)]
    member_names: BTreeMap<MemberId, String>,
) -> Element {
    // 이 화면에서 새로 고른 구성원의 이름(값에는 ID만 들어간다).
    let mut picked = use_signal(BTreeMap::<MemberId, String>::new);
    if schema.fields.is_empty() {
        return rsx! {};
    }
    let mut names = member_names.clone();
    names.extend(picked().into_iter());

    if readonly {
        return rsx! {
            dl { class: "detail-list ff-read",
                for field in schema.fields.iter() {
                    ReadField {
                        key: "{field.key}",
                        field: field.clone(),
                        value: values.get(&field.key).cloned(),
                        names: names.clone(),
                    }
                }
            }
        };
    }

    let unplaced = errors.unplaced(&schema);
    rsx! {
        div { class: "form ff",
            if unplaced > 0 {
                p { class: "form-error", role: "alert", {text::FORM_CHANGED_HINT} }
            }
            for field in schema.fields.iter() {
                EditField {
                    key: "{field.key}",
                    field: field.clone(),
                    value: values.get(&field.key).cloned(),
                    errors: errors.clone(),
                    names: names.clone(),
                    onvalue: {
                        let values = values.clone();
                        let key = field.key.clone();
                        move |next: Option<Value>| onchange.call(logic::set_value(&values, &key, next))
                    },
                    onname: move |(id, name): (MemberId, String)| {
                        picked.write().insert(id, name);
                    },
                }
            }
        }
    }
}

/// 라벨 옆의 필수 표시.
#[component]
fn RequiredMark(required: bool) -> Element {
    if !required {
        return rsx! {};
    }
    rsx! {
        span { class: "field-required", "aria-hidden": "true", " *" }
        span { class: "sr-only", " (필수)" }
    }
}

/// 입력란 아래의 오류 또는 안내.
#[component]
fn FieldNote(
    #[props(into, default)] error: String,
    #[props(into, default)] help: String,
) -> Element {
    rsx! {
        if !error.is_empty() {
            span { class: "field-error", role: "alert", "{error}" }
        } else if !help.is_empty() {
            span { class: "field-hint", "{help}" }
        }
    }
}

fn input_class(invalid: bool) -> &'static str {
    if invalid {
        "input input-error"
    } else {
        "input"
    }
}

#[component]
fn EditField(
    field: FieldDef,
    value: Option<Value>,
    errors: FieldErrors,
    names: BTreeMap<MemberId, String>,
    onvalue: EventHandler<Option<Value>>,
    onname: EventHandler<(MemberId, String)>,
) -> Element {
    let id = logic::field_dom_id(&field.key);
    let error = errors
        .field(&field.key)
        .map(|reason| text::form_value_error(&field.kind, reason))
        .unwrap_or_default();
    let invalid = !error.is_empty();
    let help = field.help.clone().unwrap_or_default();
    let label = field.label.clone();
    let required = field.required;
    let current = value.as_ref();

    match &field.kind {
        FieldKind::Text { .. } => rsx! {
            label { id, class: "field",
                span { class: "field-label", "{label}" RequiredMark { required } }
                input {
                    class: input_class(invalid),
                    r#type: "text",
                    value: logic::str_text(current),
                    maxlength: logic::max_chars(&field.kind) as i64,
                    "aria-invalid": invalid,
                    oninput: move |event| onvalue.call(logic::text_value(&event.value())),
                }
                FieldNote { error, help }
            }
        },
        FieldKind::Textarea { .. } => {
            let max = logic::max_chars(&field.kind);
            let count = logic::str_text(current).chars().count();
            rsx! {
                div { id, class: "field-with-count",
                    label { class: "field",
                        span { class: "field-label", "{label}" RequiredMark { required } }
                        textarea {
                            class: if invalid { "input textarea input-error" } else { "input textarea" },
                            rows: 4,
                            value: logic::str_text(current),
                            maxlength: max as i64,
                            "aria-invalid": invalid,
                            oninput: move |event| onvalue.call(logic::text_value(&event.value())),
                        }
                    }
                    div { class: "field-foot",
                        span { FieldNote { error, help } }
                        CharCount { count, max }
                    }
                }
            }
        }
        FieldKind::Number { min, max } => {
            let range = text::number_range_hint(*min, *max);
            let help = [help, range]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            rsx! {
                label { id, class: "field ff-narrow",
                    span { class: "field-label", "{label}" RequiredMark { required } }
                    input {
                        class: "{input_class(invalid)} num ff-num",
                        r#type: "text",
                        inputmode: "numeric",
                        autocomplete: "off",
                        value: logic::number_text(current),
                        maxlength: 20,
                        "aria-invalid": invalid,
                        oninput: move |event| onvalue.call(logic::number_value(&event.value())),
                    }
                    FieldNote { error, help }
                }
            }
        }
        FieldKind::Money => {
            let reading = logic::money_reading(current).unwrap_or_default();
            rsx! {
                label { id, class: "field ff-narrow",
                    span { class: "field-label", "{label}" RequiredMark { required } }
                    span { class: "ff-money",
                        input {
                            class: "{input_class(invalid)} num ff-num",
                            r#type: "text",
                            inputmode: "numeric",
                            autocomplete: "off",
                            placeholder: "0",
                            value: logic::money_text(current),
                            maxlength: 20,
                            "aria-invalid": invalid,
                            oninput: move |event| onvalue.call(logic::money_value(&event.value())),
                        }
                        span { class: "ff-unit", "aria-hidden": "true", "원" }
                    }
                    if !reading.is_empty() && !invalid {
                        span { class: "ff-reading", "{reading}" }
                    }
                    FieldNote { error, help }
                }
            }
        }
        FieldKind::Date => rsx! {
            label { id, class: "field ff-narrow",
                span { class: "field-label", "{label}" RequiredMark { required } }
                input {
                    class: input_class(invalid),
                    r#type: "date",
                    value: logic::str_text(current),
                    "aria-invalid": invalid,
                    oninput: move |event| onvalue.call(logic::plain_value(&event.value())),
                }
                FieldNote { error, help }
            }
        },
        FieldKind::DateRange | FieldKind::TimeRange => {
            let is_date = field.kind == FieldKind::DateRange;
            let start = logic::pair_part(current, "start");
            let end = logic::pair_part(current, "end");
            let days = if is_date {
                logic::range_days(current)
            } else {
                None
            };
            let kind = if is_date { "date" } else { "time" };
            let (start_name, end_name) = if is_date {
                ("시작일", "종료일")
            } else {
                ("시작 시각", "종료 시각")
            };
            rsx! {
                fieldset { id, class: "fieldset",
                    legend { class: "field-label", "{label}" RequiredMark { required } }
                    div { class: "ff-range",
                        input {
                            class: input_class(invalid),
                            r#type: kind,
                            value: "{start}",
                            "aria-label": "{label} {start_name}",
                            "aria-invalid": invalid,
                            oninput: {
                                let end = end.clone();
                                move |event: FormEvent| onvalue.call(logic::pair_value(&event.value(), &end))
                            },
                        }
                        span { class: "ff-range-sep", "aria-hidden": "true", "~" }
                        input {
                            class: input_class(invalid),
                            r#type: kind,
                            value: "{end}",
                            min: if is_date { start.clone() } else { String::new() },
                            "aria-label": "{label} {end_name}",
                            "aria-invalid": invalid,
                            oninput: {
                                let start = start.clone();
                                move |event: FormEvent| onvalue.call(logic::pair_value(&start, &event.value()))
                            },
                        }
                        if let Some(days) = days {
                            span { class: "ff-days tnum", "aria-live": "polite", "총 {days}일" }
                        }
                    }
                    FieldNote { error, help }
                }
            }
        }
        FieldKind::Select { options } => {
            let chosen = logic::str_text(current);
            rsx! {
                label { id, class: "field ff-narrow",
                    span { class: "field-label", "{label}" RequiredMark { required } }
                    span { class: "select",
                        select {
                            class: input_class(invalid),
                            "aria-invalid": invalid,
                            onchange: move |event| onvalue.call(logic::plain_value(&event.value())),
                            option { value: "", selected: chosen.is_empty(), "선택해 주세요" }
                            for choice in options.iter() {
                                option { key: "{choice}", value: "{choice}", selected: *choice == chosen, "{choice}" }
                            }
                        }
                    }
                    FieldNote { error, help }
                }
            }
        }
        FieldKind::Checkbox => rsx! {
            div { id, class: "field",
                label { class: "check",
                    input {
                        class: "check-input",
                        r#type: "checkbox",
                        checked: logic::is_checked(current),
                        "aria-invalid": invalid,
                        onchange: move |event| onvalue.call(logic::checkbox_value(event.checked())),
                    }
                    span { class: "check-text",
                        span { class: "check-label", "{label}" RequiredMark { required } }
                    }
                }
                FieldNote { error, help }
            }
        },
        FieldKind::Member => {
            let chosen = logic::str_text(current);
            let name = MemberId::parse(chosen.trim())
                .ok()
                .and_then(|member| names.get(&member).cloned())
                .unwrap_or_else(|| "알 수 없는 구성원".to_owned());
            rsx! {
                div { id, class: "field ff-narrow",
                    if chosen.is_empty() {
                        MemberPicker {
                            label: if required { format!("{label} *") } else { label.clone() },
                            onpick: move |member: MemberSummary| {
                                onname.call((member.id, member.name.clone()));
                                onvalue.call(Some(Value::String(member.id.to_string())));
                            },
                        }
                    } else {
                        span { class: "field-label", "{label}" RequiredMark { required } }
                        PickedMember {
                            name,
                            clear_label: "{label} 다시 고르기",
                            onclear: move |_| onvalue.call(None),
                        }
                    }
                    FieldNote { error, help }
                }
            }
        }
        FieldKind::Table { columns } => rsx! {
            EditTable {
                field: field.clone(),
                columns: columns.clone(),
                value: value.clone(),
                errors,
                onvalue,
            }
        },
    }
}

/// 표 필드의 편집 모양. 넓은 화면에서는 표, 좁은 화면에서는 행마다 쌓인다.
#[component]
fn EditTable(
    field: FieldDef,
    columns: Vec<ColumnDef>,
    value: Option<Value>,
    errors: FieldErrors,
    onvalue: EventHandler<Option<Value>>,
) -> Element {
    let id = logic::field_dom_id(&field.key);
    let rows = logic::table_rows(value.as_ref());
    let totals = logic::table_totals(&columns, &rows);
    let error = errors
        .field(&field.key)
        .map(|reason| text::form_value_error(&field.kind, reason))
        .unwrap_or_default();
    let help = field.help.clone().unwrap_or_default();
    let label = field.label.clone();
    let full = rows.len() >= MAX_ROWS;
    let count = rows.len();

    rsx! {
        fieldset { id, class: "fieldset ff-table-field",
            legend { class: "field-label", "{label}" RequiredMark { required: field.required } }
            if rows.is_empty() {
                p { class: "ff-table-empty", "아직 입력한 행이 없어요." }
            } else {
                div { class: "ff-table-wrap",
                    table { class: "table table-stack ff-table",
                        thead {
                            tr {
                                th { class: "ff-table-no", scope: "col", "번호" }
                                for column in columns.iter() {
                                    th {
                                        key: "{column.key}",
                                        scope: "col",
                                        class: if matches!(column.kind, FieldKind::Money | FieldKind::Number { .. }) { "num" },
                                        "{column.label}"
                                        RequiredMark { required: column.required }
                                    }
                                }
                                th { class: "ff-table-act", scope: "col", span { class: "sr-only", "행 지우기" } }
                            }
                        }
                        tbody {
                            for (index , row) in rows.iter().enumerate() {
                                tr { key: "{index}",
                                    td { class: "ff-table-no tnum", "data-label": "번호", "{index + 1}" }
                                    for column in columns.iter() {
                                        td { key: "{column.key}", "data-label": "{column.label}",
                                            CellInput {
                                                column: column.clone(),
                                                value: row.get(&column.key).cloned(),
                                                name: "{label} {index + 1}행 {column.label}",
                                                error: errors
                                                    .cell(&field.key, index, &column.key)
                                                    .map(|reason| text::form_value_error(&column.kind, reason))
                                                    .unwrap_or_default(),
                                                onvalue: {
                                                    let rows = rows.clone();
                                                    let column_key = column.key.clone();
                                                    move |next: Option<Value>| {
                                                        onvalue.call(logic::rows_value(logic::set_cell(&rows, index, &column_key, next)))
                                                    }
                                                },
                                            }
                                        }
                                    }
                                    td { class: "ff-table-act",
                                        button {
                                            class: "icon-btn",
                                            r#type: "button",
                                            "aria-label": "{label} {index + 1}행 지우기",
                                            onclick: {
                                                let rows = rows.clone();
                                                move |_| onvalue.call(logic::rows_value(logic::remove_row(&rows, index)))
                                            },
                                            Icon { name: IconName::Close }
                                        }
                                        if let Some(reason) = errors.row(&field.key, index, &columns) {
                                            span { class: "field-error", role: "alert", {text::form_row_error(reason)} }
                                        }
                                    }
                                }
                            }
                        }
                        if !totals.is_empty() {
                            tfoot {
                                tr { class: "ff-total",
                                    td { class: "ff-table-no", "data-label": "", "합계" }
                                    for column in columns.iter() {
                                        td {
                                            key: "{column.key}",
                                            class: "num tnum",
                                            "data-label": "{column.label}",
                                            "aria-live": "polite",
                                            if let Some((_, sum)) = totals.iter().find(|(key, _)| *key == column.key) {
                                                span { class: "nowrap",
                                                    strong { {logic::group_thousands(*sum)} }
                                                    span { class: "ff-unit-inline", "원" }
                                                }
                                            }
                                        }
                                    }
                                    td { class: "ff-table-act" }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "ff-table-foot",
                Button {
                    kind: ButtonKind::Grey,
                    small: true,
                    disabled: full,
                    onclick: {
                        let rows = rows.clone();
                        move |_| onvalue.call(logic::rows_value(logic::add_row(&rows, MAX_ROWS)))
                    },
                    Icon { name: IconName::Plus }
                    "행 추가"
                }
                span { class: "meta tnum", "{count} / {MAX_ROWS}행" }
            }
            FieldNote { error, help }
        }
    }
}

/// 표의 한 칸. 열 종류에 맞는 입력란을 그린다.
#[component]
fn CellInput(
    column: ColumnDef,
    value: Option<Value>,
    #[props(into)] name: String,
    #[props(into, default)] error: String,
    onvalue: EventHandler<Option<Value>>,
) -> Element {
    let invalid = !error.is_empty();
    let current = value.as_ref();
    let kind = column.kind.clone();
    let input = match &column.kind {
        FieldKind::Select { options } => {
            let chosen = logic::str_text(current);
            rsx! {
                span { class: "select",
                    select {
                        class: input_class(invalid),
                        "aria-label": "{name}",
                        "aria-invalid": invalid,
                        onchange: move |event| onvalue.call(logic::plain_value(&event.value())),
                        option { value: "", selected: chosen.is_empty(), "선택" }
                        for choice in options.iter() {
                            option { key: "{choice}", value: "{choice}", selected: *choice == chosen, "{choice}" }
                        }
                    }
                }
            }
        }
        FieldKind::Date => rsx! {
            input {
                class: input_class(invalid),
                r#type: "date",
                value: logic::str_text(current),
                "aria-label": "{name}",
                "aria-invalid": invalid,
                oninput: move |event| onvalue.call(logic::plain_value(&event.value())),
            }
        },
        FieldKind::Money | FieldKind::Number { .. } => {
            let shown = if column.kind == FieldKind::Money {
                logic::money_text(current)
            } else {
                logic::number_text(current)
            };
            rsx! {
                input {
                    class: "{input_class(invalid)} num ff-num",
                    r#type: "text",
                    inputmode: "numeric",
                    autocomplete: "off",
                    value: "{shown}",
                    maxlength: 20,
                    "aria-label": "{name}",
                    "aria-invalid": invalid,
                    oninput: move |event| onvalue.call(logic::cell_value(&kind, &event.value())),
                }
            }
        }
        _ => rsx! {
            input {
                class: input_class(invalid),
                r#type: "text",
                value: logic::str_text(current),
                maxlength: logic::max_chars(&column.kind) as i64,
                "aria-label": "{name}",
                "aria-invalid": invalid,
                oninput: move |event| onvalue.call(logic::text_value(&event.value())),
            }
        },
    };
    rsx! {
        div { class: "ff-cell",
            {input}
            if invalid {
                span { class: "field-error", role: "alert", "{error}" }
            }
        }
    }
}

/// 읽기 전용 한 줄(표는 표로).
#[component]
fn ReadField(field: FieldDef, value: Option<Value>, names: BTreeMap<MemberId, String>) -> Element {
    let current = value.as_ref();
    let rows = logic::table_rows(current);
    if let FieldKind::Table { columns } = &field.kind {
        let totals = logic::table_totals(columns, &rows);
        return rsx! {
            div { class: if rows.is_empty() { "detail-pair" } else { "ff-read-table" },
                dt { "{field.label}" }
                dd {
                    if rows.is_empty() {
                        span { class: "text-sub", "-" }
                    } else {
                        div { class: "ff-table-wrap",
                            table { class: "table table-stack ff-table ff-table-read",
                                thead {
                                    tr {
                                        th { class: "ff-table-no", scope: "col", "번호" }
                                        for column in columns.iter() {
                                            th {
                                                key: "{column.key}",
                                                scope: "col",
                                                class: if matches!(column.kind, FieldKind::Money | FieldKind::Number { .. }) { "num" },
                                                "{column.label}"
                                            }
                                        }
                                    }
                                }
                                tbody {
                                    for (index , row) in rows.iter().enumerate() {
                                        tr { key: "{index}",
                                            td { class: "ff-table-no tnum", "data-label": "번호", "{index + 1}" }
                                            for column in columns.iter() {
                                                td {
                                                    key: "{column.key}",
                                                    class: if matches!(column.kind, FieldKind::Money | FieldKind::Number { .. }) { "num tnum" },
                                                    "data-label": "{column.label}",
                                                    {logic::display_value(&column.kind, row.get(&column.key), &names).unwrap_or_else(|| "-".to_owned())}
                                                }
                                            }
                                        }
                                    }
                                }
                                if !totals.is_empty() {
                                    tfoot {
                                        tr { class: "ff-total",
                                            td { class: "ff-table-no", "data-label": "", "합계" }
                                            for column in columns.iter() {
                                                td { key: "{column.key}", class: "num tnum", "data-label": "{column.label}",
                                                    if let Some((_, sum)) = totals.iter().find(|(key, _)| *key == column.key) {
                                                        span { class: "nowrap",
                                                            strong { {logic::group_thousands(*sum)} }
                                                            span { class: "ff-unit-inline", "원" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };
    }
    let shown = logic::display_value(&field.kind, current, &names);
    let reading = match &field.kind {
        FieldKind::Money => logic::money_reading(current),
        _ => None,
    };
    rsx! {
        div { class: "detail-pair",
            dt { "{field.label}" }
            dd { class: if matches!(field.kind, FieldKind::Textarea { .. }) { "doc-body" },
                match shown {
                    // 사용자가 쓴 값은 텍스트 노드로만 넣는다.
                    Some(shown) => rsx! {
                        span { class: if matches!(field.kind, FieldKind::Money | FieldKind::Number { .. }) { "tnum ff-read-strong" }, "{shown}" }
                        if let Some(reading) = reading {
                            span { class: "ff-reading ff-reading-inline", "{reading}" }
                        }
                    },
                    None => rsx! { span { class: "text-sub", "-" } },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::logic::*;
    use pango_api::FieldErrorDto;
    use pango_core::form_schema::{
        ColumnDef, FieldDef, FieldKind, FormSchema, MAX_MONEY, MAX_ROWS,
    };
    use pango_core::id::MemberId;
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    fn field(key: &str, kind: FieldKind, required: bool) -> FieldDef {
        FieldDef {
            key: key.into(),
            label: key.into(),
            kind,
            required,
            help: None,
        }
    }

    fn column(key: &str, kind: FieldKind, required: bool) -> ColumnDef {
        ColumnDef {
            key: key.into(),
            label: key.into(),
            kind,
            required,
        }
    }

    fn expense_columns() -> Vec<ColumnDef> {
        vec![
            column("item", FieldKind::Text { max_len: None }, true),
            column("amount", FieldKind::Money, true),
            column("tax", FieldKind::Money, false),
            column(
                "count",
                FieldKind::Number {
                    min: None,
                    max: None,
                },
                false,
            ),
        ]
    }

    fn schema() -> FormSchema {
        FormSchema {
            fields: vec![
                field("title", FieldKind::Text { max_len: Some(10) }, true),
                field("amount", FieldKind::Money, false),
                field("period", FieldKind::DateRange, false),
                field("time", FieldKind::TimeRange, false),
                field("agree", FieldKind::Checkbox, false),
                field(
                    "items",
                    FieldKind::Table {
                        columns: expense_columns(),
                    },
                    false,
                ),
            ],
        }
    }

    fn values(value: Value) -> Values {
        value.as_object().cloned().expect("object")
    }

    #[test]
    fn blank_inputs_are_absent() {
        for blank in ["", " ", "\t\n"] {
            assert_eq!(text_value(blank), None, "{blank:?}");
            assert_eq!(number_value(blank), None);
            assert_eq!(money_value(blank), None);
            assert_eq!(plain_value(blank), None);
        }
        assert_eq!(pair_value(" ", ""), None);
        assert_eq!(checkbox_value(false), None);
        assert_eq!(checkbox_value(true), Some(json!(true)));
        assert_eq!(text_value(" 가 "), Some(json!(" 가 ")));
        assert_eq!(plain_value(" 2026-01-02 "), Some(json!("2026-01-02")));
    }

    #[test]
    fn numbers_parse_or_keep_the_typed_text() {
        assert_eq!(number_value("12"), Some(json!(12)));
        assert_eq!(number_value(" -3 "), Some(json!(-3)));
        assert_eq!(number_value("1,234"), Some(json!(1234)));
        assert_eq!(number_value("0"), Some(json!(0)));
        // 읽을 수 없는 입력은 문자열로 남아 검증에서 형식 오류가 된다.
        for bad in ["-", "1.5", "abc", "1e3", "99999999999999999999"] {
            assert_eq!(number_value(bad), Some(json!(bad)), "{bad:?}");
            assert_eq!(number_text(number_value(bad).as_ref()), bad);
        }
        assert_eq!(number_text(Some(&json!(-7))), "-7");
        assert_eq!(number_text(None), "");
        assert_eq!(number_text(Some(&json!(true))), "");
    }

    #[test]
    fn money_parses_commas_and_formats_back() {
        assert_eq!(money_value("123,000"), Some(json!(123_000)));
        assert_eq!(money_value(" 1 000 원"), Some(json!(1000)));
        assert_eq!(money_value("0"), Some(json!(0)));
        assert_eq!(money_value(","), None);
        assert_eq!(money_value("원"), None);
        for bad in ["-5", "1.5", "12a", "만원", "99999999999999999999"] {
            assert_eq!(money_value(bad), Some(json!(bad)), "{bad:?}");
            assert_eq!(money_text(money_value(bad).as_ref()), bad);
        }
        assert_eq!(money_text(Some(&json!(1_234_567))), "1,234,567");
        assert_eq!(money_text(Some(&json!(999))), "999");
        assert_eq!(money_text(None), "");
        // 입력 → 값 → 다시 입력란 문자열이 안정적이다(쉼표가 자리를 잡는다).
        let typed = money_text(money_value("1234").as_ref());
        assert_eq!(typed, "1,234");
        assert_eq!(money_value(&typed), Some(json!(1234)));
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(100), "100");
        assert_eq!(group_thousands(1000), "1,000");
        assert_eq!(group_thousands(-1_234_567), "-1,234,567");
        assert_eq!(group_thousands(MAX_MONEY), "1,000,000,000,000");
        assert_eq!(group_thousands(i64::MIN), "-9,223,372,036,854,775,808");
    }

    #[test]
    fn amounts_are_read_in_korean() {
        assert_eq!(korean_amount(123_000), "십이만 삼천 원");
        assert_eq!(korean_amount(0), "영 원");
        assert_eq!(korean_amount(1), "일 원");
        assert_eq!(korean_amount(10), "십 원");
        assert_eq!(korean_amount(11), "십일 원");
        assert_eq!(korean_amount(110), "백십 원");
        assert_eq!(korean_amount(1000), "천 원");
        assert_eq!(korean_amount(10_000), "일만 원");
        assert_eq!(korean_amount(11_111), "일만 천백십일 원");
        assert_eq!(korean_amount(100_000_000), "일억 원");
        assert_eq!(korean_amount(100_010_000), "일억 일만 원");
        assert_eq!(korean_amount(2_050_300), "이백오만 삼백 원");
        assert_eq!(korean_amount(MAX_MONEY), "일조 원");
        assert_eq!(
            korean_amount(987_654_321),
            "구억 팔천칠백육십오만 사천삼백이십일 원"
        );
        assert_eq!(korean_amount(-1), "");
        assert!(korean_amount(i64::MAX).starts_with("구백이십이경"));
    }

    #[test]
    fn reading_only_for_acceptable_amounts() {
        assert_eq!(money_reading(Some(&json!(5000))), Some("오천 원".into()));
        assert_eq!(money_reading(Some(&json!(MAX_MONEY + 1))), None);
        assert_eq!(money_reading(Some(&json!(-1))), None);
        assert_eq!(money_reading(Some(&json!("12a"))), None);
        assert_eq!(money_reading(Some(&json!(1.5))), None);
        assert_eq!(money_reading(None), None);
    }

    #[test]
    fn ranges_keep_partial_input_and_count_days() {
        let partial = pair_value("2026-03-01", "");
        assert_eq!(partial, Some(json!({"start": "2026-03-01", "end": ""})));
        assert_eq!(pair_part(partial.as_ref(), "start"), "2026-03-01");
        assert_eq!(pair_part(partial.as_ref(), "end"), "");
        assert_eq!(range_days(partial.as_ref()), None);
        let one = pair_value("2026-03-01", "2026-03-01");
        assert_eq!(range_days(one.as_ref()), Some(1));
        let leap = pair_value("2028-02-28", "2028-03-01");
        assert_eq!(range_days(leap.as_ref()), Some(3));
        let backwards = pair_value("2026-03-02", "2026-03-01");
        assert_eq!(range_days(backwards.as_ref()), None);
        assert_eq!(range_days(Some(&json!("2026-03-01"))), None);
        assert_eq!(range_days(Some(&json!({"start": 1, "end": 2}))), None);
        assert_eq!(pair_part(Some(&json!([1])), "start"), "");
    }

    /// 화면의 일수·합계는 서버가 계산하는 값(`computed`)과 같다.
    #[test]
    fn computed_values_match_the_server_rules() {
        let schema = schema();
        let input = values(json!({
            "title": "제목",
            "period": {"start": "2026-12-30", "end": "2027-01-02"},
            "items": [
                {"item": "택시", "amount": 12_000, "tax": 1200},
                {"item": "식대", "amount": 30_000},
                {"item": "숙박", "amount": 0, "count": 2},
            ],
        }));
        let normalized = schema
            .validate_values(&Value::Object(input.clone()))
            .expect("valid");
        assert_eq!(
            normalized.computed["period"]["days"],
            json!(range_days(input.get("period")).expect("days"))
        );
        let totals = table_totals(&expense_columns(), &table_rows(input.get("items")));
        assert_eq!(
            totals,
            vec![("amount".into(), 42_000), ("tax".into(), 1200)]
        );
        for (key, sum) in &totals {
            assert_eq!(normalized.computed["items"]["totals"][key], json!(sum));
        }
    }

    #[test]
    fn totals_skip_cells_the_server_would_reject() {
        let rows = table_rows(Some(&json!([
            {"amount": 1000},
            {"amount": "12a"},
            {"amount": -5},
            {"amount": MAX_MONEY + 1},
            {"amount": 1.5},
            {},
            {"amount": MAX_MONEY},
        ])));
        assert_eq!(
            table_totals(&expense_columns(), &rows),
            vec![("amount".into(), MAX_MONEY + 1000), ("tax".into(), 0)]
        );
        let no_money = [column("item", FieldKind::Text { max_len: None }, false)];
        assert!(table_totals(&no_money, &rows).is_empty());
        assert_eq!(
            table_totals(&expense_columns(), &[]),
            vec![("amount".into(), 0), ("tax".into(), 0)]
        );
    }

    #[test]
    fn rows_are_edited_without_touching_others() {
        let rows = add_row(&[], MAX_ROWS);
        assert_eq!(rows.len(), 1);
        let rows = set_cell(&rows, 0, "item", text_value("택시"));
        let rows = add_row(&rows, MAX_ROWS);
        let rows = set_cell(&rows, 1, "amount", money_value("3,000"));
        assert_eq!(
            rows_value(rows.clone()),
            Some(json!([{"item": "택시"}, {"amount": 3000}]))
        );
        // 없는 행은 그대로, 칸을 비우면 키가 사라진다.
        assert_eq!(set_cell(&rows, 9, "item", text_value("x")), rows);
        let cleared = set_cell(&rows, 0, "item", None);
        assert!(cleared[0].is_empty());
        assert_eq!(remove_row(&rows, 0), vec![rows[1].clone()]);
        assert_eq!(remove_row(&rows, 9), rows);
        assert_eq!(rows_value(Vec::new()), None);
        let full: Vec<Row> = (0..MAX_ROWS).map(|_| Row::new()).collect();
        assert_eq!(add_row(&full, MAX_ROWS).len(), MAX_ROWS);
    }

    #[test]
    fn malformed_tables_read_as_empty() {
        assert!(table_rows(None).is_empty());
        assert!(table_rows(Some(&json!("x"))).is_empty());
        assert!(table_rows(Some(&json!({"a": 1}))).is_empty());
        let mixed = table_rows(Some(&json!([1, null, {"a": 1}])));
        assert_eq!(mixed.len(), 3);
        assert!(mixed[0].is_empty() && mixed[1].is_empty());
        assert_eq!(mixed[2]["a"], json!(1));
    }

    #[test]
    fn cells_convert_by_column_kind() {
        assert_eq!(cell_value(&FieldKind::Money, "1,500"), Some(json!(1500)));
        assert_eq!(
            cell_value(
                &FieldKind::Number {
                    min: None,
                    max: None
                },
                "-2"
            ),
            Some(json!(-2))
        );
        assert_eq!(
            cell_value(&FieldKind::Date, "2026-01-01"),
            Some(json!("2026-01-01"))
        );
        assert_eq!(
            cell_value(&FieldKind::Select { options: vec![] }, " 가 "),
            Some(json!("가"))
        );
        assert_eq!(
            cell_value(&FieldKind::Text { max_len: None }, "1,500"),
            Some(json!("1,500"))
        );
        assert_eq!(cell_value(&FieldKind::Money, " "), None);
    }

    #[test]
    fn set_value_inserts_and_removes() {
        let start = values(json!({"a": 1}));
        assert_eq!(
            set_value(&start, "b", Some(json!("x"))),
            values(json!({"a": 1, "b": "x"}))
        );
        assert_eq!(set_value(&start, "a", None), values(json!({})));
        assert_eq!(set_value(&start, "zz", None), start);
    }

    #[test]
    fn prune_drops_blanks_stale_keys_and_empty_rows() {
        let pruned = prune(
            &schema(),
            &values(json!({
                "title": "  ",
                "amount": 0,
                "period": {"start": "", "end": ""},
                "time": {"start": "09:00", "end": ""},
                "agree": false,
                "old_field": "예전 값",
                "items": [
                    {"item": " ", "amount": null},
                    {"item": "택시", "gone_column": 3, "amount": 1000},
                    {},
                ],
            })),
        );
        assert_eq!(
            Value::Object(pruned),
            json!({
                "amount": 0,
                "time": {"start": "09:00", "end": ""},
                "items": [{"item": "택시", "amount": 1000}],
            })
        );
        // 모두 빈 표는 키째 사라져 필수 검증(`empty`)에 걸린다.
        let empty = prune(&schema(), &values(json!({"items": [{}, {"item": ""}]})));
        assert!(empty.is_empty());
        // 모양이 틀린 값은 지우지 않고 검증에 맡긴다.
        let odd = prune(
            &schema(),
            &values(json!({"period": "2026-01-01", "items": "x"})),
        );
        assert_eq!(Value::Object(odd), json!({"period": "2026-01-01"}));
    }

    #[test]
    fn validation_reports_every_field_and_cell() {
        let schema = FormSchema {
            fields: vec![
                field("title", FieldKind::Text { max_len: Some(3) }, true),
                field("amount", FieldKind::Money, true),
                field("period", FieldKind::DateRange, true),
                field(
                    "items",
                    FieldKind::Table {
                        columns: expense_columns(),
                    },
                    true,
                ),
            ],
        };
        let errors = validate(
            &schema,
            &values(json!({
                "amount": "12a",
                "period": {"start": "2026-03-02", "end": "2026-03-01"},
                "items": [{"amount": -1}, {"item": "택시", "amount": 10}],
                "stale": 1,
            })),
        );
        assert_eq!(errors.field("title"), Some("empty"));
        assert_eq!(errors.field("amount"), Some("invalid_format"));
        assert_eq!(errors.field("period"), Some("inconsistent"));
        assert_eq!(errors.field("items"), None);
        assert_eq!(errors.cell("items", 0, "item"), Some("empty"));
        assert_eq!(errors.cell("items", 0, "amount"), Some("out_of_range"));
        assert_eq!(errors.cell("items", 1, "amount"), None);
        assert!(errors.touches("items"));
        assert_eq!(errors.first_key(&schema), Some("title".into()));
        assert_eq!(errors.unplaced(&schema), 1);
        assert!(validate(
            &schema,
            &values(json!({
                "title": "가나다", "amount": 5,
                "period": {"start": "2026-03-01", "end": "2026-03-01"},
                "items": [{"item": "택시", "amount": 10}],
            }))
        )
        .is_empty());
        assert!(FieldErrors::default().first_key(&schema).is_none());
    }

    #[test]
    fn server_errors_map_onto_fields_rows_and_columns() {
        let errors = FieldErrors::from_api(&[
            FieldErrorDto {
                key: "handover_to".into(),
                row: None,
                column: None,
                reason: "member_inactive".into(),
            },
            FieldErrorDto {
                key: "items".into(),
                row: Some(1),
                column: Some("amount".into()),
                reason: "out_of_range".into(),
            },
            FieldErrorDto {
                key: "items".into(),
                row: Some(2),
                column: None,
                reason: "invalid_format".into(),
            },
            FieldErrorDto {
                key: "items".into(),
                row: Some(0),
                column: Some("ghost".into()),
                reason: "unknown_field".into(),
            },
        ]);
        let columns = expense_columns();
        assert_eq!(errors.field("handover_to"), Some("member_inactive"));
        assert_eq!(errors.cell("items", 1, "amount"), Some("out_of_range"));
        assert_eq!(errors.row("items", 2, &columns), Some("invalid_format"));
        assert_eq!(errors.row("items", 0, &columns), Some("unknown_field"));
        assert_eq!(errors.row("items", 1, &columns), None);
        assert_eq!(errors.field("items"), None);
        assert!(FieldErrors::from_api(&[]).is_empty());
    }

    #[test]
    fn read_only_values_are_formatted() {
        let me = MemberId::new();
        let names = BTreeMap::from([(me, "홍길동".to_owned())]);
        let show = |kind: FieldKind, value: Value| display_value(&kind, Some(&value), &names);
        assert_eq!(
            show(FieldKind::Money, json!(123_000)),
            Some("123,000원".into())
        );
        assert_eq!(
            show(
                FieldKind::Number {
                    min: None,
                    max: None
                },
                json!(-1500)
            ),
            Some("-1,500".into())
        );
        assert_eq!(
            show(FieldKind::Date, json!("2026-03-02")),
            Some("2026. 3. 2.".into())
        );
        assert_eq!(show(FieldKind::Date, json!("어제")), Some("어제".into()));
        assert_eq!(
            show(
                FieldKind::DateRange,
                json!({"start": "2026-03-02", "end": "2026-03-04"})
            ),
            Some("2026. 3. 2. ~ 2026. 3. 4. (3일)".into())
        );
        assert_eq!(
            show(
                FieldKind::DateRange,
                json!({"start": "2026-03-02", "end": "2026-03-02"})
            ),
            Some("2026. 3. 2. (1일)".into())
        );
        assert_eq!(
            show(
                FieldKind::TimeRange,
                json!({"start": "09:00", "end": "18:00"})
            ),
            Some("09:00 ~ 18:00".into())
        );
        assert_eq!(show(FieldKind::Checkbox, json!(true)), Some("예".into()));
        assert_eq!(
            show(FieldKind::Checkbox, json!(false)),
            Some("아니요".into())
        );
        assert_eq!(
            show(FieldKind::Member, json!(me.to_string())),
            Some("홍길동".into())
        );
        assert_eq!(
            show(FieldKind::Member, json!(MemberId::new().to_string())),
            Some("알 수 없는 구성원".into())
        );
        assert_eq!(
            show(FieldKind::Member, json!("<b>")),
            Some("알 수 없는 구성원".into())
        );
        assert_eq!(
            show(FieldKind::Text { max_len: None }, json!("<b>x</b>")),
            Some("<b>x</b>".into())
        );
        assert_eq!(show(FieldKind::Text { max_len: None }, json!("  ")), None);
        assert_eq!(show(FieldKind::Text { max_len: None }, json!(null)), None);
        assert_eq!(show(FieldKind::Text { max_len: None }, json!(3)), None);
        assert_eq!(display_value(&FieldKind::Money, None, &names), None);
    }

    #[test]
    fn text_limits_follow_the_definition() {
        assert_eq!(max_chars(&FieldKind::Text { max_len: None }), 200);
        assert_eq!(max_chars(&FieldKind::Text { max_len: Some(20) }), 20);
        assert_eq!(
            max_chars(&FieldKind::Text {
                max_len: Some(9999)
            }),
            200
        );
        assert_eq!(max_chars(&FieldKind::Text { max_len: Some(0) }), 1);
        assert_eq!(max_chars(&FieldKind::Textarea { max_len: None }), 5000);
        assert_eq!(field_dom_id("spent_on"), "ff-spent_on");
    }
}
