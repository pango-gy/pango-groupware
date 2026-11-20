//! 결재 양식의 구조(필드 정의)와 제출 값 검증.
//!
//! 양식은 필드 목록([`FormSchema`])을 가진다. 관리자가 양식을 만들거나 고칠 때는
//! [`FormSchema::normalized`]로 정의를 검증하고, 구성원이 문서를 낼 때는
//! [`FormSchema::validate_values`]로 값을 검증한다. 두 함수 모두 첫 오류에서 멈추지 않고
//! **모든 오류를 모아** 돌려준다(화면이 필드마다 표시할 수 있도록).
//!
//! 값의 JSON 모양(필드 종류별):
//!
//! | 종류 | 값 |
//! | --- | --- |
//! | `text`, `textarea` | 문자열 |
//! | `number`, `money` | 정수(JSON 숫자, 소수·문자열 불가) |
//! | `date` | `"YYYY-MM-DD"` |
//! | `date_range` | `{"start": "YYYY-MM-DD", "end": "YYYY-MM-DD"}` (양 끝 포함) |
//! | `time_range` | `{"start": "HH:MM", "end": "HH:MM"}` (같은 날, 시작 < 끝) |
//! | `select` | 선택지 중 하나(문자열) |
//! | `checkbox` | 불리언 |
//! | `member` | 구성원 ID(문자열) |
//! | `table` | 행의 배열. 행은 `{열 key: 값}` |
//!
//! 없는 키, `null`, 공백뿐인 문자열, 빈 표는 모두 "입력하지 않음"이다.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::calendar::{self, DateRange};
use crate::error::{DomainError, DomainResult, ValidationReason};
use crate::id::MemberId;
use crate::text::TextRule;
use crate::time::MinuteOfDay;

/// 양식 하나에 둘 수 있는 최대 필드 수.
pub const MAX_FIELDS: usize = 40;
/// 필드·열 `key`의 최대 길이(ASCII).
pub const MAX_KEY_LEN: usize = 40;
pub const MAX_LABEL_CHARS: usize = 50;
pub const MAX_HELP_CHARS: usize = 200;
/// `text`의 기본이자 최대 글자 수.
pub const TEXT_MAX_LEN: usize = 200;
/// `textarea`의 기본이자 최대 글자 수.
pub const TEXTAREA_MAX_LEN: usize = 5_000;
pub const MAX_OPTIONS: usize = 30;
pub const MAX_OPTION_CHARS: usize = 50;
pub const MAX_COLUMNS: usize = 8;
pub const MAX_ROWS: usize = 50;
/// `money`의 최댓값(원). 최솟값은 0이다.
pub const MAX_MONEY: i64 = 1_000_000_000_000;
/// `number`가 가질 수 있는 절댓값의 상한. 브라우저의 숫자(2^53)로 손실 없이 오간다.
pub const MAX_NUMBER: i64 = 1_000_000_000_000_000;
/// 한 번에 돌려주는 오류의 최대 개수. 거대한 입력이 거대한 오류 응답이 되지 않게 한다.
pub const MAX_ERRORS: usize = 100;

/// 양식의 필드 목록. 순서가 곧 화면의 순서다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormSchema {
    #[serde(default)]
    pub fields: Vec<FieldDef>,
}

/// 필드 하나. JSON에서는 `kind`와 그 종류의 속성이 같은 객체에 놓인다:
/// `{"key": "amount", "label": "금액", "kind": "money", "required": true}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDef {
    /// 값 객체의 키. 소문자 영문으로 시작하는 `snake_case`, 양식 안에서 유일.
    pub key: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    /// 입력란 아래의 안내 문구.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

/// 표의 열. 종류는 `text`·`number`·`money`·`date`·`select`만 쓸 수 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDef {
    /// 행 객체의 키. 표 안에서 유일.
    pub key: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldKind {
    /// 한 줄 글. `max_len`을 생략하면 [`TEXT_MAX_LEN`].
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_len: Option<usize>,
    },
    /// 여러 줄 글. `max_len`을 생략하면 [`TEXTAREA_MAX_LEN`].
    Textarea {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_len: Option<usize>,
    },
    /// 정수. `min`·`max`는 양 끝 포함.
    Number {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<i64>,
    },
    /// 금액(원). 0 이상 [`MAX_MONEY`] 이하의 정수.
    Money,
    Date,
    /// 기간. 양 끝 포함, 최대 366일.
    DateRange,
    /// 같은 날의 시간대.
    TimeRange,
    Select {
        #[serde(default)]
        options: Vec<String>,
    },
    /// 체크. 필수면 반드시 체크해야 한다(확인·동의).
    Checkbox,
    /// 구성원 한 명. 재직 여부는 서비스 계층이 확인한다([`NormalizedValues::members`]).
    Member,
    Table {
        #[serde(default)]
        columns: Vec<ColumnDef>,
    },
}

impl FieldKind {
    /// JSON의 `kind` 값.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Textarea { .. } => "textarea",
            Self::Number { .. } => "number",
            Self::Money => "money",
            Self::Date => "date",
            Self::DateRange => "date_range",
            Self::TimeRange => "time_range",
            Self::Select { .. } => "select",
            Self::Checkbox => "checkbox",
            Self::Member => "member",
            Self::Table { .. } => "table",
        }
    }

    const fn allowed_in_table(&self) -> bool {
        matches!(
            self,
            Self::Text { .. }
                | Self::Number { .. }
                | Self::Money
                | Self::Date
                | Self::Select { .. }
        )
    }
}

/// 오류의 사유. `code()`가 API에 실리는 값이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldReason {
    /// 값이 규칙에 맞지 않음(`empty`, `too_long`, `invalid_format`, `out_of_range` …).
    /// 필수 값이 없으면 `empty`다.
    Invalid(ValidationReason),
    /// 양식에 없는 키(표에서는 없는 열).
    UnknownField,
    /// 선택지에 없는 값.
    NotAnOption,
}

impl FieldReason {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Invalid(reason) => reason.code(),
            Self::UnknownField => "unknown_field",
            Self::NotAnOption => "not_an_option",
        }
    }
}

impl From<ValidationReason> for FieldReason {
    fn from(reason: ValidationReason) -> Self {
        Self::Invalid(reason)
    }
}

/// 필드 하나의 오류.
///
/// - 값 검증: `key`는 필드 키, 표의 칸이면 `row`(0부터)와 `column`(열 키)이 함께 온다.
///   행 자체가 잘못됐으면 `row`만 온다. 값 전체가 객체가 아니면 `key`는 `values`다.
/// - 정의 검증: `key`는 정의 안의 위치다(`fields[2].label`, `fields[0].columns[1].options`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub key: String,
    pub row: Option<usize>,
    pub column: Option<String>,
    pub reason: FieldReason,
}

impl FieldError {
    fn field(key: &str, reason: impl Into<FieldReason>) -> Self {
        Self {
            key: key.to_owned(),
            row: None,
            column: None,
            reason: reason.into(),
        }
    }

    fn cell(key: &str, row: usize, column: Option<&str>, reason: impl Into<FieldReason>) -> Self {
        Self {
            key: key.to_owned(),
            row: Some(row),
            column: column.map(str::to_owned),
            reason: reason.into(),
        }
    }
}

/// 검증을 통과한 제출 값.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NormalizedValues {
    /// 정규화된 값. 입력하지 않은 필드는 들어 있지 않다.
    pub values: Map<String, Value>,
    /// 값에서 계산한 것. 금액 열이 있는 표는 `{"totals": {열 key: 합계}}`,
    /// 기간은 `{"days": 일수}`. 키는 필드 키다.
    pub computed: Map<String, Value>,
    /// `member` 필드에 지정된 구성원. 존재와 재직 여부는 호출한 쪽이 확인한다.
    pub members: Vec<(String, MemberId)>,
}

fn reason_of(error: DomainError) -> ValidationReason {
    match error {
        DomainError::Validation { reason, .. } => reason,
        // 이 모듈이 부르는 검증 함수는 입력 오류만 낸다.
        _ => ValidationReason::InvalidFormat,
    }
}

fn normalize_text(
    raw: &str,
    max_chars: usize,
    multiline: bool,
) -> Result<String, ValidationReason> {
    TextRule {
        field: "value",
        min_chars: 1,
        max_chars,
        multiline,
    }
    .normalize(raw)
    .map_err(reason_of)
}

/// 오류가 너무 많이 쌓이지 않게 막는 수집기.
#[derive(Default)]
struct Errors(Vec<FieldError>);

impl Errors {
    fn push(&mut self, error: FieldError) {
        if self.0.len() < MAX_ERRORS {
            self.0.push(error);
        }
    }

    fn is_full(&self) -> bool {
        self.0.len() >= MAX_ERRORS
    }

    fn finish<T>(self, value: T) -> Result<T, Vec<FieldError>> {
        if self.0.is_empty() {
            Ok(value)
        } else {
            Err(self.0)
        }
    }
}

// ───────────── 정의 검증 ─────────────

fn check_key(raw: &str) -> Result<(), ValidationReason> {
    if raw.is_empty() {
        return Err(ValidationReason::Empty);
    }
    if raw.len() > MAX_KEY_LEN {
        return Err(ValidationReason::TooLong { max: MAX_KEY_LEN });
    }
    let valid_char = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_';
    if !raw.bytes().all(valid_char) {
        return Err(ValidationReason::InvalidCharacter);
    }
    if !raw.as_bytes()[0].is_ascii_lowercase() {
        return Err(ValidationReason::InvalidFormat);
    }
    Ok(())
}

/// 키와 이름을 확인한다. 이름은 정규화해 제자리에 다시 쓴다.
fn check_identity(
    path: &str,
    key: &str,
    label: &mut String,
    seen: &mut BTreeSet<String>,
    errors: &mut Errors,
) {
    match check_key(key) {
        Err(reason) => errors.push(FieldError::field(&format!("{path}.key"), reason)),
        Ok(()) if !seen.insert(key.to_owned()) => errors.push(FieldError::field(
            &format!("{path}.key"),
            ValidationReason::Duplicate,
        )),
        Ok(()) => {}
    }
    match normalize_text(label, MAX_LABEL_CHARS, false) {
        Ok(normalized) => *label = normalized,
        Err(reason) => errors.push(FieldError::field(&format!("{path}.label"), reason)),
    }
}

fn check_max_len(path: &str, max_len: &mut Option<usize>, limit: usize, errors: &mut Errors) {
    match *max_len {
        None => *max_len = Some(limit),
        Some(value) if (1..=limit).contains(&value) => {}
        Some(_) => errors.push(FieldError::field(
            &format!("{path}.max_len"),
            ValidationReason::OutOfRange,
        )),
    }
}

fn check_kind(path: &str, kind: &mut FieldKind, in_table: bool, errors: &mut Errors) {
    if in_table && !kind.allowed_in_table() {
        errors.push(FieldError::field(
            &format!("{path}.kind"),
            ValidationReason::InvalidFormat,
        ));
        return;
    }
    match kind {
        FieldKind::Text { max_len } => check_max_len(path, max_len, TEXT_MAX_LEN, errors),
        FieldKind::Textarea { max_len } => check_max_len(path, max_len, TEXTAREA_MAX_LEN, errors),
        FieldKind::Number { min, max } => {
            let in_range = |bound: &Option<i64>| {
                bound.is_none_or(|value| (-MAX_NUMBER..=MAX_NUMBER).contains(&value))
            };
            for (name, bound) in [("min", &*min), ("max", &*max)] {
                if !in_range(bound) {
                    errors.push(FieldError::field(
                        &format!("{path}.{name}"),
                        ValidationReason::OutOfRange,
                    ));
                }
            }
            if let (Some(min), Some(max)) = (*min, *max) {
                if min > max {
                    errors.push(FieldError::field(
                        &format!("{path}.min"),
                        ValidationReason::Inconsistent,
                    ));
                }
            }
        }
        FieldKind::Select { options } => check_options(path, options, errors),
        FieldKind::Table { columns } => check_columns(path, columns, errors),
        FieldKind::Money
        | FieldKind::Date
        | FieldKind::DateRange
        | FieldKind::TimeRange
        | FieldKind::Checkbox
        | FieldKind::Member => {}
    }
}

fn check_options(path: &str, options: &mut [String], errors: &mut Errors) {
    let at = format!("{path}.options");
    if options.is_empty() {
        return errors.push(FieldError::field(&at, ValidationReason::Empty));
    }
    if options.len() > MAX_OPTIONS {
        return errors.push(FieldError::field(
            &at,
            ValidationReason::TooLong { max: MAX_OPTIONS },
        ));
    }
    let mut seen = BTreeSet::new();
    for (index, option) in options.iter_mut().enumerate() {
        let at = format!("{at}[{index}]");
        match normalize_text(option, MAX_OPTION_CHARS, false) {
            Ok(normalized) if !seen.insert(normalized.clone()) => {
                errors.push(FieldError::field(&at, ValidationReason::Duplicate));
            }
            Ok(normalized) => *option = normalized,
            Err(reason) => errors.push(FieldError::field(&at, reason)),
        }
    }
}

fn check_columns(path: &str, columns: &mut [ColumnDef], errors: &mut Errors) {
    let at = format!("{path}.columns");
    if columns.is_empty() {
        return errors.push(FieldError::field(&at, ValidationReason::Empty));
    }
    if columns.len() > MAX_COLUMNS {
        return errors.push(FieldError::field(
            &at,
            ValidationReason::TooLong { max: MAX_COLUMNS },
        ));
    }
    let mut seen = BTreeSet::new();
    for (index, column) in columns.iter_mut().enumerate() {
        let at = format!("{at}[{index}]");
        check_identity(&at, &column.key, &mut column.label, &mut seen, errors);
        check_kind(&at, &mut column.kind, true, errors);
    }
}

impl FormSchema {
    /// 정의를 검증한다. 오류는 정의 안의 위치(`fields[0].key` 등)를 `key`로 갖는다.
    pub fn validate(&self) -> Result<(), Vec<FieldError>> {
        self.clone().normalized().map(|_| ())
    }

    /// 정의를 검증하고 저장할 모양으로 다듬는다: 이름·안내 문구·선택지를 정규화하고(NFC,
    /// 앞뒤 공백 제거), 생략한 `max_len`에 기본값을 채운다. 다듬은 결과는 다시 넣어도 같다.
    ///
    /// | 규칙 | 위치 | 사유 |
    /// | --- | --- | --- |
    /// | 필드 [`MAX_FIELDS`]개 이하 | `fields` | `too_long` |
    /// | `key`는 소문자 영문으로 시작하는 `[a-z0-9_]`, [`MAX_KEY_LEN`]자 이하 | `….key` | `empty`·`too_long`·`invalid_character`·`invalid_format` |
    /// | `key`는 양식(열은 표) 안에서 유일 | `….key` | `duplicate` |
    /// | `label` 1~[`MAX_LABEL_CHARS`]자, `help` [`MAX_HELP_CHARS`]자 이하 | `….label`, `….help` | 글 규칙 |
    /// | `max_len`은 1~기본값 | `….max_len` | `out_of_range` |
    /// | `min`·`max`는 ±[`MAX_NUMBER`] 안, `min ≤ max` | `….min`, `….max` | `out_of_range`·`inconsistent` |
    /// | 선택지 1~[`MAX_OPTIONS`]개, 각 [`MAX_OPTION_CHARS`]자 이하, 중복 없음 | `….options`, `….options[i]` | `empty`·`too_long`·`duplicate` |
    /// | 열 1~[`MAX_COLUMNS`]개, 종류는 text·number·money·date·select | `….columns`, `….columns[i].kind` | `empty`·`too_long`·`invalid_format` |
    pub fn normalized(mut self) -> Result<Self, Vec<FieldError>> {
        let mut errors = Errors::default();
        if self.fields.len() > MAX_FIELDS {
            errors.push(FieldError::field(
                "fields",
                ValidationReason::TooLong { max: MAX_FIELDS },
            ));
            return errors.finish(self);
        }
        let mut seen = BTreeSet::new();
        for (index, field) in self.fields.iter_mut().enumerate() {
            let path = format!("fields[{index}]");
            check_identity(&path, &field.key, &mut field.label, &mut seen, &mut errors);
            let help = field.help.take().filter(|help| !help.trim().is_empty());
            field.help = match help.map(|help| normalize_text(&help, MAX_HELP_CHARS, false)) {
                None => None,
                Some(Ok(normalized)) => Some(normalized),
                Some(Err(reason)) => {
                    errors.push(FieldError::field(&format!("{path}.help"), reason));
                    None
                }
            };
            check_kind(&path, &mut field.kind, false, &mut errors);
        }
        errors.finish(self)
    }
}

// ───────────── 값 검증 ─────────────

/// 입력하지 않은 것으로 볼 값.
fn is_blank(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.trim().is_empty(),
        Some(Value::Array(rows)) => rows.is_empty(),
        Some(_) => false,
    }
}

/// 오류에 실을 키. 입력에서 온 문자열이므로 길이를 자른다.
fn echo_key(key: &str) -> String {
    key.chars().take(MAX_KEY_LEN).collect()
}

fn integer(value: &Value, min: i64, max: i64) -> Result<Value, FieldReason> {
    // 소수(`1.5`, `1.0`)와 i64를 넘는 수는 `as_i64`가 `None`이다.
    let number = value.as_i64().ok_or(ValidationReason::InvalidFormat)?;
    if !(min..=max).contains(&number) {
        return Err(ValidationReason::OutOfRange.into());
    }
    Ok(Value::from(number))
}

fn date(value: &Value) -> Result<NaiveDate, FieldReason> {
    let raw = value.as_str().ok_or(ValidationReason::InvalidFormat)?;
    calendar::parse_date("value", raw.trim()).map_err(|error| reason_of(error).into())
}

/// `{"start": …, "end": …}`만 받는다(다른 키가 있으면 형식 오류).
fn pair(value: &Value) -> Result<(&Value, &Value), FieldReason> {
    let object = value.as_object().ok_or(ValidationReason::InvalidFormat)?;
    match (object.get("start"), object.get("end")) {
        (Some(start), Some(end)) if object.len() == 2 => Ok((start, end)),
        _ => Err(ValidationReason::InvalidFormat.into()),
    }
}

fn pair_value(start: String, end: String) -> Value {
    let mut object = Map::new();
    object.insert("start".to_owned(), Value::String(start));
    object.insert("end".to_owned(), Value::String(end));
    Value::Object(object)
}

/// 표의 칸에도 쓸 수 있는 종류의 값 하나를 검증한다.
fn scalar(kind: &FieldKind, value: &Value) -> Result<Value, FieldReason> {
    let text = |max_len: &Option<usize>, default: usize, multiline: bool| {
        let raw = value.as_str().ok_or(ValidationReason::InvalidFormat)?;
        // 정의가 다듬어지지 않았더라도 기본 상한을 넘기지 않는다.
        let max_chars = max_len.unwrap_or(default).clamp(1, default);
        normalize_text(raw, max_chars, multiline)
            .map(Value::String)
            .map_err(FieldReason::from)
    };
    match kind {
        FieldKind::Text { max_len } => text(max_len, TEXT_MAX_LEN, false),
        FieldKind::Textarea { max_len } => text(max_len, TEXTAREA_MAX_LEN, true),
        FieldKind::Number { min, max } => integer(
            value,
            min.unwrap_or(-MAX_NUMBER).max(-MAX_NUMBER),
            max.unwrap_or(MAX_NUMBER).min(MAX_NUMBER),
        ),
        FieldKind::Money => integer(value, 0, MAX_MONEY),
        FieldKind::Date => date(value).map(|date| Value::String(date.to_string())),
        FieldKind::Select { options } => {
            let raw = value.as_str().ok_or(ValidationReason::InvalidFormat)?;
            let chosen = normalize_text(raw, MAX_OPTION_CHARS, false)
                .map_err(|_| FieldReason::NotAnOption)?;
            if options.contains(&chosen) {
                Ok(Value::String(chosen))
            } else {
                Err(FieldReason::NotAnOption)
            }
        }
        // 표의 칸이 아닌 종류는 `validate_field`가 처리한다.
        FieldKind::DateRange
        | FieldKind::TimeRange
        | FieldKind::Checkbox
        | FieldKind::Member
        | FieldKind::Table { .. } => Err(ValidationReason::InvalidFormat.into()),
    }
}

fn validate_table(
    field: &FieldDef,
    columns: &[ColumnDef],
    value: &Value,
    out: &mut NormalizedValues,
    errors: &mut Errors,
) {
    let key = field.key.as_str();
    let Some(rows) = value.as_array() else {
        return errors.push(FieldError::field(key, ValidationReason::InvalidFormat));
    };
    if rows.len() > MAX_ROWS {
        return errors.push(FieldError::field(
            key,
            ValidationReason::TooLong { max: MAX_ROWS },
        ));
    }
    let failed_before = errors.0.len();
    let mut totals: Map<String, Value> = columns
        .iter()
        .filter(|column| column.kind == FieldKind::Money)
        .map(|column| (column.key.clone(), Value::from(0)))
        .collect();
    let mut normalized_rows = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        if errors.is_full() {
            return;
        }
        let Some(cells) = row.as_object() else {
            errors.push(FieldError::cell(
                key,
                index,
                None,
                ValidationReason::InvalidFormat,
            ));
            continue;
        };
        let mut normalized = Map::new();
        for column in columns {
            let cell = cells.get(&column.key);
            if is_blank(cell) {
                if column.required {
                    errors.push(FieldError::cell(
                        key,
                        index,
                        Some(&column.key),
                        ValidationReason::Empty,
                    ));
                }
                continue;
            }
            match cell.map(|cell| scalar(&column.kind, cell)) {
                Some(Ok(value)) => {
                    if let Some(total) = totals.get_mut(&column.key) {
                        // 행 50개 × 최대 금액은 i64 안에 든다.
                        let sum = total.as_i64().unwrap_or(0) + value.as_i64().unwrap_or(0);
                        *total = Value::from(sum);
                    }
                    normalized.insert(column.key.clone(), value);
                }
                Some(Err(reason)) => {
                    errors.push(FieldError::cell(key, index, Some(&column.key), reason));
                }
                None => {}
            }
        }
        for unknown in cells
            .keys()
            .filter(|name| !columns.iter().any(|column| column.key == **name))
        {
            errors.push(FieldError::cell(
                key,
                index,
                Some(&echo_key(unknown)),
                FieldReason::UnknownField,
            ));
        }
        normalized_rows.push(Value::Object(normalized));
    }
    if errors.0.len() > failed_before {
        return;
    }
    out.values
        .insert(field.key.clone(), Value::Array(normalized_rows));
    if !totals.is_empty() {
        let mut computed = Map::new();
        computed.insert("totals".to_owned(), Value::Object(totals));
        out.computed
            .insert(field.key.clone(), Value::Object(computed));
    }
}

fn validate_field(
    field: &FieldDef,
    value: &Value,
    out: &mut NormalizedValues,
    errors: &mut Errors,
) {
    let key = field.key.as_str();
    let result: Result<Value, FieldReason> = match &field.kind {
        FieldKind::Table { columns } => {
            return validate_table(field, columns, value, out, errors);
        }
        FieldKind::DateRange => pair(value).and_then(|(start, end)| {
            let range = DateRange::new(date(start)?, date(end)?)
                .map_err(|error| FieldReason::from(reason_of(error)))?;
            let days = (range.end() - range.start()).num_days() + 1;
            let mut computed = Map::new();
            computed.insert("days".to_owned(), Value::from(days));
            out.computed
                .insert(field.key.clone(), Value::Object(computed));
            Ok(pair_value(
                range.start().to_string(),
                range.end().to_string(),
            ))
        }),
        FieldKind::TimeRange => pair(value).and_then(|(start, end)| {
            let time = |value: &Value| {
                let raw = value.as_str().ok_or(ValidationReason::InvalidFormat)?;
                MinuteOfDay::parse(raw.trim())
                    .map(|_| raw.trim().to_owned())
                    .map_err(|error| FieldReason::from(reason_of(error)))
            };
            let (start, end) = (time(start)?, time(end)?);
            // `HH:MM` 고정 폭이므로 문자열 비교가 곧 시각 비교다.
            if start >= end {
                return Err(ValidationReason::Inconsistent.into());
            }
            Ok(pair_value(start, end))
        }),
        FieldKind::Checkbox => match value.as_bool() {
            Some(false) if field.required => Err(ValidationReason::Empty.into()),
            Some(checked) => Ok(Value::Bool(checked)),
            None => Err(ValidationReason::InvalidFormat.into()),
        },
        FieldKind::Member => value
            .as_str()
            .and_then(|raw| MemberId::parse(raw.trim()).ok())
            .map(|member| {
                out.members.push((field.key.clone(), member));
                Value::String(member.to_string())
            })
            .ok_or(ValidationReason::InvalidFormat.into()),
        kind => scalar(kind, value),
    };
    match result {
        Ok(value) => {
            out.values.insert(field.key.clone(), value);
        }
        Err(reason) => {
            out.computed.remove(key);
            errors.push(FieldError::field(key, reason));
        }
    }
}

impl FormSchema {
    /// 제출 값을 검증하고 정규화한다. 오류는 필드 순서대로, 모르는 키는 맨 뒤에 모아 돌려준다.
    ///
    /// - `values`는 객체여야 한다(`null`은 빈 객체로 본다).
    /// - 양식에 없는 키는 `unknown_field`, 필수인데 입력하지 않았으면 `empty`.
    /// - 글은 [`crate::text`]의 규칙으로 정규화한다(NFC, 제어·방향 제어 문자 거부).
    /// - 필수 표는 1~[`MAX_ROWS`]행, 아니면 0~[`MAX_ROWS`]행.
    ///
    /// 통과한 결과([`NormalizedValues::values`])를 다시 넣으면 같은 결과가 나온다.
    pub fn validate_values(&self, values: &Value) -> Result<NormalizedValues, Vec<FieldError>> {
        let empty = Map::new();
        let object = match values {
            Value::Null => &empty,
            Value::Object(object) => object,
            _ => {
                return Err(vec![FieldError::field(
                    "values",
                    ValidationReason::InvalidFormat,
                )])
            }
        };
        let mut out = NormalizedValues::default();
        let mut errors = Errors::default();
        for field in &self.fields {
            if errors.is_full() {
                break;
            }
            match object.get(&field.key) {
                value if is_blank(value) => {
                    if field.required {
                        errors.push(FieldError::field(&field.key, ValidationReason::Empty));
                    }
                }
                Some(value) => validate_field(field, value, &mut out, &mut errors),
                None => {}
            }
        }
        for unknown in object
            .keys()
            .filter(|name| !self.fields.iter().any(|field| field.key == **name))
        {
            errors.push(FieldError::field(
                &echo_key(unknown),
                FieldReason::UnknownField,
            ));
        }
        errors.finish(out)
    }
}

/// 저장된 정의(JSON 문자열)를 읽는다. 정의는 저장 전에 검증하므로 다시 검증하지 않는다.
pub fn parse_stored(raw: &str) -> DomainResult<FormSchema> {
    serde_json::from_str(raw)
        .map_err(|_| DomainError::validation("schema", ValidationReason::InvalidFormat))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    fn schema(fields: Value) -> FormSchema {
        serde_json::from_value::<FormSchema>(json!({ "fields": fields }))
            .expect("schema json")
            .normalized()
            .expect("valid schema")
    }

    fn raw_schema(fields: Value) -> FormSchema {
        serde_json::from_value(json!({ "fields": fields })).expect("schema json")
    }

    /// 모든 종류가 하나씩 있는 양식.
    fn full() -> FormSchema {
        schema(json!([
            { "key": "subject", "label": "제목", "kind": "text", "required": true, "max_len": 10 },
            { "key": "detail", "label": "내용", "kind": "textarea" },
            { "key": "count", "label": "수량", "kind": "number", "min": -5, "max": 5 },
            { "key": "amount", "label": "금액", "kind": "money" },
            { "key": "day", "label": "일자", "kind": "date" },
            { "key": "period", "label": "기간", "kind": "date_range" },
            { "key": "hours", "label": "시간", "kind": "time_range" },
            { "key": "method", "label": "구분", "kind": "select", "options": ["법인카드", "개인경비"] },
            { "key": "agree", "label": "확인", "kind": "checkbox" },
            { "key": "handover", "label": "인수인계자", "kind": "member" },
            { "key": "items", "label": "내역", "kind": "table", "columns": [
                { "key": "on", "label": "일자", "kind": "date", "required": true },
                { "key": "place", "label": "사용처", "kind": "text" },
                { "key": "account", "label": "계정", "kind": "select", "options": ["식대", "교통비"] },
                { "key": "qty", "label": "수량", "kind": "number", "min": 1 },
                { "key": "price", "label": "금액", "kind": "money", "required": true },
                { "key": "tax", "label": "세액", "kind": "money" }
            ]}
        ]))
    }

    fn errors_of(
        schema: &FormSchema,
        values: Value,
    ) -> Vec<(String, Option<usize>, Option<String>, &'static str)> {
        schema
            .validate_values(&values)
            .expect_err("should fail")
            .into_iter()
            .map(|e| (e.key, e.row, e.column, e.reason.code()))
            .collect()
    }

    fn one_error(schema: &FormSchema, values: Value) -> (String, &'static str) {
        let errors = errors_of(schema, values);
        assert_eq!(errors.len(), 1, "{errors:?}");
        (errors[0].0.clone(), errors[0].3)
    }

    fn single(kind: Value) -> FormSchema {
        let mut field = json!({ "key": "f", "label": "값" });
        for (name, value) in kind.as_object().expect("object") {
            field[name] = value.clone();
        }
        schema(json!([field]))
    }

    fn accept(schema: &FormSchema, value: Value) -> Value {
        schema
            .validate_values(&json!({ "f": value }))
            .unwrap_or_else(|errors| panic!("should pass: {errors:?}"))
            .values
            .remove("f")
            .unwrap_or(Value::Null)
    }

    fn reject(schema: &FormSchema, value: Value) -> &'static str {
        let (key, reason) = one_error(schema, json!({ "f": value }));
        assert_eq!(key, "f");
        reason
    }

    // ── 값: 해피 ──

    #[test]
    fn every_kind_is_accepted_and_normalized() {
        let member = MemberId::new();
        let out = full()
            .validate_values(&json!({
                "subject": "  출장 정산 ",
                "detail": "첫 줄\r\n둘째 줄",
                "count": -5,
                "amount": 120000,
                "day": "2026-03-02",
                "period": { "start": "2026-03-02", "end": "2026-03-04" },
                "hours": { "start": "09:00", "end": "18:30" },
                "method": "법인카드",
                "agree": true,
                "handover": member.to_string(),
                "items": [
                    { "on": "2026-03-02", "place": "식당", "account": "식대", "qty": 2, "price": 30000, "tax": 3000 },
                    { "on": "2026-03-03", "price": 12000 }
                ]
            }))
            .expect("valid");
        assert_eq!(
            Value::Object(out.values),
            json!({
                "subject": "출장 정산",
                "detail": "첫 줄\n둘째 줄",
                "count": -5,
                "amount": 120000,
                "day": "2026-03-02",
                "period": { "start": "2026-03-02", "end": "2026-03-04" },
                "hours": { "start": "09:00", "end": "18:30" },
                "method": "법인카드",
                "agree": true,
                "handover": member.to_string(),
                "items": [
                    { "on": "2026-03-02", "place": "식당", "account": "식대", "qty": 2, "price": 30000, "tax": 3000 },
                    { "on": "2026-03-03", "price": 12000 }
                ]
            })
        );
        assert_eq!(
            Value::Object(out.computed),
            json!({
                "period": { "days": 3 },
                "items": { "totals": { "price": 42000, "tax": 3000 } }
            })
        );
        assert_eq!(out.members, vec![("handover".to_owned(), member)]);
    }

    #[test]
    fn only_required_fields_are_needed_and_blank_inputs_count_as_missing() {
        let schema = full();
        let out = schema
            .validate_values(&json!({
                "subject": "제목", "detail": "   ", "count": null, "day": "", "items": []
            }))
            .expect("valid");
        assert_eq!(Value::Object(out.values), json!({ "subject": "제목" }));
        assert!(out.computed.is_empty());
        assert!(out.members.is_empty());
        // 필드가 없는 양식은 값이 없어도 된다.
        let none = FormSchema::default();
        for values in [Value::Null, json!({})] {
            assert_eq!(
                none.validate_values(&values),
                Ok(NormalizedValues::default())
            );
        }
    }

    // ── 값: 필수·모르는 키·형식 ──

    #[test]
    fn missing_required_values_are_reported() {
        let schema = full();
        for values in [
            json!({}),
            Value::Null,
            json!({ "subject": null }),
            json!({ "subject": " \n " }),
        ] {
            assert_eq!(one_error(&schema, values), ("subject".to_owned(), "empty"));
        }
        let table = single(json!({ "kind": "table", "required": true, "columns": [
            { "key": "name", "label": "이름", "kind": "text" }
        ]}));
        assert_eq!(reject(&table, json!([])), "empty");
        assert_eq!(accept(&table, json!([{}])), json!([{}]));
        let checkbox = single(json!({ "kind": "checkbox", "required": true }));
        assert_eq!(reject(&checkbox, json!(false)), "empty");
        assert_eq!(accept(&checkbox, json!(true)), json!(true));
        let optional = single(json!({ "kind": "checkbox" }));
        assert_eq!(accept(&optional, json!(false)), json!(false));
    }

    #[test]
    fn unknown_keys_are_rejected_and_echoed_truncated() {
        let schema = full();
        let long = "k".repeat(500);
        let errors = errors_of(
            &schema,
            json!({ "subject": "제목", "extra": 1, long.clone(): "x", "items": [
                { "on": "2026-03-02", "price": 1, "memo": "?" }
            ]}),
        );
        assert_eq!(
            errors,
            vec![
                (
                    "items".to_owned(),
                    Some(0),
                    Some("memo".to_owned()),
                    "unknown_field"
                ),
                ("extra".to_owned(), None, None, "unknown_field"),
                ("k".repeat(MAX_KEY_LEN), None, None, "unknown_field"),
            ]
        );
    }

    #[test]
    fn values_must_be_an_object() {
        for values in [
            json!([]),
            json!("x"),
            json!(1),
            json!(true),
            json!([{ "subject": "a" }]),
        ] {
            assert_eq!(
                one_error(&full(), values),
                ("values".to_owned(), "invalid_format")
            );
        }
    }

    #[test]
    fn wrong_json_types_are_format_errors() {
        let cases: [(Value, Vec<Value>); 10] = [
            (
                json!({ "kind": "text" }),
                vec![json!(1), json!(true), json!(["a"]), json!({ "a": 1 })],
            ),
            (json!({ "kind": "textarea" }), vec![json!(1), json!([1])]),
            (
                json!({ "kind": "number" }),
                vec![
                    json!("3"),
                    json!(1.5),
                    json!(1.0),
                    json!(true),
                    json!(1e30),
                    json!({}),
                ],
            ),
            (
                json!({ "kind": "money" }),
                vec![json!("1000"), json!(0.5), json!([1])],
            ),
            (
                json!({ "kind": "date" }),
                vec![
                    json!(20260302),
                    json!("2026-3-2"),
                    json!("2026-02-30"),
                    json!("0202-03-02"),
                    json!("내일"),
                    json!({}),
                ],
            ),
            (
                json!({ "kind": "date_range" }),
                vec![
                    json!("2026-03-02"),
                    json!(["2026-03-02", "2026-03-03"]),
                    json!({ "start": "2026-03-02" }),
                    json!({ "start": "2026-03-02", "end": "2026-03-03", "x": 1 }),
                    json!({ "start": 1, "end": 2 }),
                    json!({ "start": "2026-03-02", "end": null }),
                ],
            ),
            (
                json!({ "kind": "time_range" }),
                vec![
                    json!("09:00-18:00"),
                    json!({ "start": "9:00", "end": "18:00" }),
                    json!({ "start": "09:00", "end": "18:00:00" }),
                    json!({ "start": 9, "end": 18 }),
                    json!({ "start": "09:00" }),
                ],
            ),
            (
                json!({ "kind": "select", "options": ["a"] }),
                vec![json!(0), json!(["a"]), json!(true)],
            ),
            (
                json!({ "kind": "checkbox" }),
                vec![json!("true"), json!(1), json!({})],
            ),
            (
                json!({ "kind": "member" }),
                vec![
                    json!(1),
                    json!("abc"),
                    json!("018F2B6E-7C1A-7000-8000-000000000001"),
                    json!({ "id": "x" }),
                    json!("' OR 1=1 --"),
                ],
            ),
        ];
        for (kind, values) in cases {
            let schema = single(kind.clone());
            for value in values {
                assert_eq!(
                    reject(&schema, value.clone()),
                    "invalid_format",
                    "{kind} ← {value}"
                );
            }
        }
        let table = single(json!({ "kind": "table", "columns": [
            { "key": "n", "label": "수", "kind": "number" }
        ]}));
        assert_eq!(reject(&table, json!({ "n": 1 })), "invalid_format");
        assert_eq!(reject(&table, json!("rows")), "invalid_format");
        assert_eq!(
            errors_of(&table, json!({ "f": [1, { "n": "x" }, [], { "n": 2 }] })),
            vec![
                ("f".to_owned(), Some(0), None, "invalid_format"),
                (
                    "f".to_owned(),
                    Some(1),
                    Some("n".to_owned()),
                    "invalid_format"
                ),
                ("f".to_owned(), Some(2), None, "invalid_format"),
            ]
        );
    }

    #[test]
    fn deeply_nested_garbage_is_rejected_without_panicking() {
        let mut nested = json!("leaf");
        for depth in 0..100 {
            nested = if depth % 2 == 0 {
                json!({ "start": nested, "end": "x" })
            } else {
                json!([nested])
            };
        }
        let schema = full();
        for key in [
            "subject", "count", "period", "hours", "items", "handover", "agree", "method",
        ] {
            let errors = errors_of(&schema, json!({ "subject": "제목", key: nested.clone() }));
            assert!(errors.iter().all(|e| e.0 == key), "{errors:?}");
        }
    }

    // ── 값: 경계 ──

    #[test]
    fn text_length_boundaries() {
        let text = single(json!({ "kind": "text", "max_len": 5 }));
        assert_eq!(accept(&text, json!("가나다라마")), json!("가나다라마"));
        assert_eq!(reject(&text, json!("가나다라마바")), "too_long");
        let default = single(json!({ "kind": "text" }));
        assert_eq!(
            accept(&default, json!("a".repeat(200))),
            json!("a".repeat(200))
        );
        assert_eq!(reject(&default, json!("a".repeat(201))), "too_long");
        let area = single(json!({ "kind": "textarea" }));
        assert_eq!(
            accept(&area, json!("a".repeat(5000))),
            json!("a".repeat(5000))
        );
        assert_eq!(reject(&area, json!("a".repeat(5001))), "too_long");
        assert_eq!(reject(&area, json!("a".repeat(1_000_000))), "too_long");
    }

    #[test]
    fn text_is_normalized_and_deceptive_characters_are_rejected() {
        let text = single(json!({ "kind": "text" }));
        // 분해형(NFD) 한글은 조합형(NFC)으로.
        assert_eq!(
            accept(&text, json!("\u{1112}\u{1161}\u{11AB}")),
            json!("한")
        );
        for bad in [
            "a\u{0000}b",
            "a\nb",
            "a\tb",
            "a\u{202E}b",
            "a\u{200B}b",
            "\u{FEFF}a",
            "a\u{0007}",
        ] {
            assert_eq!(reject(&text, json!(bad)), "invalid_character", "{bad:?}");
        }
        let area = single(json!({ "kind": "textarea" }));
        assert_eq!(accept(&area, json!("a\r\nb\tc")), json!("a\nb\tc"));
        assert_eq!(reject(&area, json!("a\u{202E}b")), "invalid_character");
        assert_eq!(reject(&area, json!("a\u{001B}[31m")), "invalid_character");
        // 표의 칸도 같은 규칙.
        let table = single(json!({ "kind": "table", "columns": [
            { "key": "t", "label": "글", "kind": "text" }
        ]}));
        assert_eq!(
            errors_of(
                &table,
                json!({ "f": [{ "t": "\u{1112}\u{1161}\u{11AB}" }, { "t": "x\u{202E}" }] })
            ),
            vec![(
                "f".to_owned(),
                Some(1),
                Some("t".to_owned()),
                "invalid_character"
            )]
        );
    }

    #[test]
    fn number_boundaries() {
        let ranged = single(json!({ "kind": "number", "min": -5, "max": 5 }));
        for ok in [-5, 0, 5] {
            assert_eq!(accept(&ranged, json!(ok)), json!(ok));
        }
        for bad in [-6, 6, i64::MIN, i64::MAX] {
            assert_eq!(reject(&ranged, json!(bad)), "out_of_range");
        }
        let open = single(json!({ "kind": "number" }));
        assert_eq!(accept(&open, json!(MAX_NUMBER)), json!(MAX_NUMBER));
        assert_eq!(accept(&open, json!(-MAX_NUMBER)), json!(-MAX_NUMBER));
        assert_eq!(reject(&open, json!(MAX_NUMBER + 1)), "out_of_range");
        assert_eq!(reject(&open, json!(-MAX_NUMBER - 1)), "out_of_range");
        assert_eq!(reject(&open, json!(u64::MAX)), "invalid_format");
        let min_only = single(json!({ "kind": "number", "min": 1 }));
        assert_eq!(reject(&min_only, json!(0)), "out_of_range");
        assert_eq!(accept(&min_only, json!(1)), json!(1));
    }

    #[test]
    fn money_boundaries() {
        let money = single(json!({ "kind": "money" }));
        assert_eq!(accept(&money, json!(0)), json!(0));
        assert_eq!(accept(&money, json!(MAX_MONEY)), json!(MAX_MONEY));
        assert_eq!(reject(&money, json!(MAX_MONEY + 1)), "out_of_range");
        assert_eq!(reject(&money, json!(-1)), "out_of_range");
    }

    #[test]
    fn date_range_boundaries() {
        let range = single(json!({ "kind": "date_range" }));
        let out = range
            .validate_values(&json!({ "f": { "start": "2026-01-01", "end": "2027-01-01" } }))
            .expect("366 days");
        assert_eq!(out.computed["f"]["days"], 366);
        assert_eq!(
            reject(
                &range,
                json!({ "start": "2026-01-01", "end": "2027-01-02" })
            ),
            "out_of_range"
        );
        let single_day = range
            .validate_values(&json!({ "f": { "start": "2024-02-29", "end": "2024-02-29" } }))
            .expect("one day");
        assert_eq!(single_day.computed["f"]["days"], 1);
        assert_eq!(
            reject(
                &range,
                json!({ "start": "2026-03-02", "end": "2026-03-01" })
            ),
            "inconsistent"
        );
        // 실패한 기간은 계산값을 남기지 않는다.
        let errors = range.validate_values(&json!({ "f": { "start": "2026-03-02", "end": "x" } }));
        assert_eq!(
            errors.expect_err("fails")[0].reason.code(),
            "invalid_format"
        );
    }

    #[test]
    fn time_range_boundaries() {
        let range = single(json!({ "kind": "time_range" }));
        assert_eq!(
            accept(&range, json!({ "start": "00:00", "end": "23:59" })),
            json!({ "start": "00:00", "end": "23:59" })
        );
        assert_eq!(
            reject(&range, json!({ "start": "09:00", "end": "09:00" })),
            "inconsistent"
        );
        assert_eq!(
            reject(&range, json!({ "start": "18:00", "end": "09:00" })),
            "inconsistent"
        );
        assert_eq!(
            reject(&range, json!({ "start": "09:00", "end": "24:00" })),
            "out_of_range"
        );
        assert_eq!(
            reject(&range, json!({ "start": "09:60", "end": "10:00" })),
            "out_of_range"
        );
    }

    #[test]
    fn select_accepts_only_listed_options() {
        let select = single(json!({ "kind": "select", "options": ["법인카드", "개인경비", "한"] }));
        assert_eq!(accept(&select, json!(" 법인카드 ")), json!("법인카드"));
        assert_eq!(
            accept(&select, json!("\u{1112}\u{1161}\u{11AB}")),
            json!("한")
        );
        for bad in ["현금", "법인", "법인카드\u{202E}", &"가".repeat(500)] {
            assert_eq!(reject(&select, json!(bad)), "not_an_option", "{bad:?}");
        }
    }

    #[test]
    fn table_row_boundaries_and_totals() {
        let table = single(json!({ "kind": "table", "columns": [
            { "key": "price", "label": "금액", "kind": "money", "required": true }
        ]}));
        let rows = |n: usize| Value::Array(vec![json!({ "price": MAX_MONEY }); n]);
        let out = table
            .validate_values(&json!({ "f": rows(50) }))
            .expect("50 rows");
        assert_eq!(out.computed["f"]["totals"]["price"], 50 * MAX_MONEY);
        assert_eq!(reject(&table, rows(51)), "too_long");
        // 금액 열이 없는 표는 계산값이 없다.
        let plain = single(json!({ "kind": "table", "columns": [
            { "key": "name", "label": "이름", "kind": "text" }
        ]}));
        let out = plain
            .validate_values(&json!({ "f": [{ "name": "김" }] }))
            .expect("valid");
        assert!(out.computed.is_empty());
    }

    #[test]
    fn all_errors_are_returned_together_in_field_order() {
        let errors = errors_of(
            &full(),
            json!({
                "detail": 5,
                "count": 9,
                "amount": -1,
                "day": "어제",
                "period": { "start": "2026-03-05", "end": "2026-03-01" },
                "hours": { "start": "10:00", "end": "09:00" },
                "method": "수표",
                "agree": "yes",
                "handover": "nobody",
                "items": [
                    { "on": "2026-03-02", "price": 100 },
                    { "place": "식당", "account": "숙박", "qty": 0, "price": "만원" },
                    "행",
                ],
                "zzz": true
            }),
        );
        let cell = |row, column: &str, reason| {
            (
                "items".to_owned(),
                Some(row),
                Some(column.to_owned()),
                reason,
            )
        };
        assert_eq!(
            errors,
            vec![
                ("subject".to_owned(), None, None, "empty"),
                ("detail".to_owned(), None, None, "invalid_format"),
                ("count".to_owned(), None, None, "out_of_range"),
                ("amount".to_owned(), None, None, "out_of_range"),
                ("day".to_owned(), None, None, "invalid_format"),
                ("period".to_owned(), None, None, "inconsistent"),
                ("hours".to_owned(), None, None, "inconsistent"),
                ("method".to_owned(), None, None, "not_an_option"),
                ("agree".to_owned(), None, None, "invalid_format"),
                ("handover".to_owned(), None, None, "invalid_format"),
                cell(1, "on", "empty"),
                cell(1, "account", "not_an_option"),
                cell(1, "qty", "out_of_range"),
                cell(1, "price", "invalid_format"),
                ("items".to_owned(), Some(2), None, "invalid_format"),
                ("zzz".to_owned(), None, None, "unknown_field"),
            ]
        );
    }

    #[test]
    fn error_count_is_capped() {
        let table = single(json!({ "kind": "table", "columns": [
            { "key": "a", "label": "가", "kind": "money", "required": true },
            { "key": "b", "label": "나", "kind": "money", "required": true },
            { "key": "c", "label": "다", "kind": "money", "required": true }
        ]}));
        let errors = table
            .validate_values(&json!({ "f": vec![json!({ "x": 1 }); 50] }))
            .expect_err("fails");
        assert_eq!(errors.len(), MAX_ERRORS);
        let mut many = Map::new();
        for index in 0..5_000 {
            many.insert(format!("unknown_{index}"), json!(1));
        }
        let errors = full()
            .validate_values(&Value::Object(many))
            .expect_err("fails");
        assert_eq!(errors.len(), MAX_ERRORS);
    }

    // ── 정의 ──

    fn schema_errors(fields: Value) -> Vec<(String, &'static str)> {
        raw_schema(fields)
            .normalized()
            .expect_err("should fail")
            .into_iter()
            .map(|e| (e.key, e.reason.code()))
            .collect()
    }

    fn text_field(key: &str) -> Value {
        json!({ "key": key, "label": "값", "kind": "text" })
    }

    #[test]
    fn normalized_fills_defaults_and_is_idempotent() {
        let schema = raw_schema(json!([
            { "key": "a", "label": "  \u{1112}\u{1161}\u{11AB} ", "kind": "text", "help": "  " },
            { "key": "b", "label": "나", "kind": "textarea", "help": " 자세히 " },
            { "key": "c", "label": "다", "kind": "select", "options": [" 가 ", "나"] }
        ]))
        .normalized()
        .expect("valid");
        assert_eq!(
            serde_json::to_value(&schema).expect("json"),
            json!({ "fields": [
                { "key": "a", "label": "한", "kind": "text", "max_len": 200, "required": false },
                { "key": "b", "label": "나", "kind": "textarea", "max_len": 5000, "required": false, "help": "자세히" },
                { "key": "c", "label": "다", "kind": "select", "options": ["가", "나"], "required": false }
            ]})
        );
        assert_eq!(schema.clone().normalized(), Ok(schema.clone()));
        assert_eq!(schema.validate(), Ok(()));
        // 저장한 JSON을 다시 읽어도 같다.
        let stored = serde_json::to_string(&schema).expect("json");
        assert_eq!(parse_stored(&stored), Ok(schema));
        assert!(parse_stored("{\"fields\": 1}").is_err());
        assert_eq!(parse_stored("{}"), Ok(FormSchema::default()));
    }

    #[test]
    fn field_count_boundary() {
        let fields =
            |n: usize| Value::Array((0..n).map(|i| text_field(&format!("f{i}"))).collect());
        assert!(raw_schema(fields(40)).validate().is_ok());
        assert_eq!(
            schema_errors(fields(41)),
            vec![("fields".to_owned(), "too_long")]
        );
        assert!(FormSchema::default().validate().is_ok());
    }

    #[test]
    fn key_rules() {
        let cases = [
            ("", "empty"),
            ("Amount", "invalid_character"),
            ("금액", "invalid_character"),
            ("a b", "invalid_character"),
            ("a-b", "invalid_character"),
            ("1st", "invalid_format"),
            ("_a", "invalid_format"),
        ];
        for (key, reason) in cases {
            assert_eq!(
                schema_errors(json!([text_field(key)])),
                vec![("fields[0].key".to_owned(), reason)],
                "{key:?}"
            );
        }
        assert!(raw_schema(json!([text_field(&"a".repeat(40))]))
            .validate()
            .is_ok());
        assert_eq!(
            schema_errors(json!([text_field(&"a".repeat(41))])),
            vec![("fields[0].key".to_owned(), "too_long")]
        );
        assert_eq!(
            schema_errors(json!([text_field("a"), text_field("b"), text_field("a")])),
            vec![("fields[2].key".to_owned(), "duplicate")]
        );
    }

    #[test]
    fn label_and_help_rules() {
        let field = |label: &str, help: &str| json!([{ "key": "a", "label": label, "kind": "date", "help": help }]);
        assert!(raw_schema(field(&"가".repeat(50), &"나".repeat(200)))
            .validate()
            .is_ok());
        assert_eq!(
            schema_errors(field(&"가".repeat(51), &"나".repeat(201))),
            vec![
                ("fields[0].label".to_owned(), "too_long"),
                ("fields[0].help".to_owned(), "too_long"),
            ]
        );
        assert_eq!(
            schema_errors(field(" ", "")),
            vec![("fields[0].label".to_owned(), "empty")]
        );
        assert_eq!(
            schema_errors(field("a\u{202E}b", "줄\n바꿈")),
            vec![
                ("fields[0].label".to_owned(), "invalid_character"),
                ("fields[0].help".to_owned(), "invalid_character"),
            ]
        );
    }

    #[test]
    fn max_len_and_number_bound_rules() {
        let text = |kind: &str, max_len: usize| json!([{ "key": "a", "label": "가", "kind": kind, "max_len": max_len }]);
        assert!(raw_schema(text("text", 1)).validate().is_ok());
        assert!(raw_schema(text("text", 200)).validate().is_ok());
        assert!(raw_schema(text("textarea", 5000)).validate().is_ok());
        for bad in [text("text", 0), text("text", 201), text("textarea", 5001)] {
            assert_eq!(
                schema_errors(bad),
                vec![("fields[0].max_len".to_owned(), "out_of_range")]
            );
        }
        let number = |min: i64, max: i64| json!([{ "key": "a", "label": "가", "kind": "number", "min": min, "max": max }]);
        assert!(raw_schema(number(3, 3)).validate().is_ok());
        assert!(raw_schema(number(-MAX_NUMBER, MAX_NUMBER))
            .validate()
            .is_ok());
        assert_eq!(
            schema_errors(number(4, 3)),
            vec![("fields[0].min".to_owned(), "inconsistent")]
        );
        assert_eq!(
            schema_errors(number(-MAX_NUMBER - 1, MAX_NUMBER + 1)),
            vec![
                ("fields[0].min".to_owned(), "out_of_range"),
                ("fields[0].max".to_owned(), "out_of_range"),
            ]
        );
    }

    #[test]
    fn option_rules() {
        let select = |options: Value| json!([{ "key": "a", "label": "가", "kind": "select", "options": options }]);
        let numbered =
            |n: usize| Value::Array((0..n).map(|i| json!(format!("선택 {i}"))).collect());
        assert!(raw_schema(select(numbered(1))).validate().is_ok());
        assert!(raw_schema(select(numbered(30))).validate().is_ok());
        assert!(raw_schema(select(json!(["가".repeat(50)])))
            .validate()
            .is_ok());
        assert_eq!(
            schema_errors(select(numbered(31))),
            vec![("fields[0].options".to_owned(), "too_long")]
        );
        assert_eq!(
            schema_errors(select(json!([]))),
            vec![("fields[0].options".to_owned(), "empty")]
        );
        // `options`를 아예 보내지 않아도 같은 오류다.
        assert_eq!(
            schema_errors(json!([{ "key": "a", "label": "가", "kind": "select" }])),
            vec![("fields[0].options".to_owned(), "empty")]
        );
        assert_eq!(
            schema_errors(select(json!([
                "가",
                " ",
                "가".repeat(51),
                "가 ",
                "a\u{200B}"
            ]))),
            vec![
                ("fields[0].options[1]".to_owned(), "empty"),
                ("fields[0].options[2]".to_owned(), "too_long"),
                ("fields[0].options[3]".to_owned(), "duplicate"),
                ("fields[0].options[4]".to_owned(), "invalid_character"),
            ]
        );
    }

    #[test]
    fn column_rules() {
        let table = |columns: Value| json!([{ "key": "t", "label": "표", "kind": "table", "columns": columns }]);
        let columns = |n: usize| {
            Value::Array(
                (0..n)
                    .map(|i| json!({ "key": format!("c{i}"), "label": "열", "kind": "text" }))
                    .collect(),
            )
        };
        assert!(raw_schema(table(columns(1))).validate().is_ok());
        assert!(raw_schema(table(columns(8))).validate().is_ok());
        assert_eq!(
            schema_errors(table(columns(9))),
            vec![("fields[0].columns".to_owned(), "too_long")]
        );
        assert_eq!(
            schema_errors(table(json!([]))),
            vec![("fields[0].columns".to_owned(), "empty")]
        );
        assert_eq!(
            schema_errors(json!([{ "key": "t", "label": "표", "kind": "table" }])),
            vec![("fields[0].columns".to_owned(), "empty")]
        );
        for kind in ["textarea", "date_range", "time_range", "checkbox", "member"] {
            assert_eq!(
                schema_errors(table(json!([{ "key": "c", "label": "열", "kind": kind }]))),
                vec![("fields[0].columns[0].kind".to_owned(), "invalid_format")],
                "{kind}"
            );
        }
        let nested = json!([{ "key": "c", "label": "열", "kind": "table", "columns": [
            { "key": "d", "label": "열", "kind": "text" }
        ]}]);
        assert_eq!(
            schema_errors(table(nested)),
            vec![("fields[0].columns[0].kind".to_owned(), "invalid_format")]
        );
        assert_eq!(
            schema_errors(table(json!([
                { "key": "c", "label": "열", "kind": "select", "options": [] },
                { "key": "c", "label": "", "kind": "number", "min": 2, "max": 1 },
                { "key": "D", "label": "열", "kind": "text", "max_len": 999 }
            ]))),
            vec![
                ("fields[0].columns[0].options".to_owned(), "empty"),
                ("fields[0].columns[1].key".to_owned(), "duplicate"),
                ("fields[0].columns[1].label".to_owned(), "empty"),
                ("fields[0].columns[1].min".to_owned(), "inconsistent"),
                ("fields[0].columns[2].key".to_owned(), "invalid_character"),
                ("fields[0].columns[2].max_len".to_owned(), "out_of_range"),
            ]
        );
        // 열 키는 필드 키와 겹쳐도 된다(표 안에서만 유일).
        assert!(raw_schema(json!([
            text_field("name"),
            { "key": "t", "label": "표", "kind": "table", "columns": [text_field("name")] }
        ]))
        .validate()
        .is_ok());
    }

    #[test]
    fn unknown_kind_or_wrong_shape_does_not_deserialize() {
        for bad in [
            json!({ "fields": [{ "key": "a", "label": "가", "kind": "file" }] }),
            json!({ "fields": [{ "key": "a", "label": "가" }] }),
            json!({ "fields": [{ "key": "a", "kind": "text" }] }),
            json!({ "fields": [{ "key": "a", "label": "가", "kind": "select", "options": "가" }] }),
            json!({ "fields": [{ "key": "a", "label": "가", "kind": "text", "max_len": -1 }] }),
            json!({ "fields": "none" }),
            json!("fields"),
        ] {
            assert!(
                serde_json::from_value::<FormSchema>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn field_kind_codes_match_serialization() {
        let kinds = [
            FieldKind::Text { max_len: None },
            FieldKind::Textarea { max_len: None },
            FieldKind::Number {
                min: None,
                max: None,
            },
            FieldKind::Money,
            FieldKind::Date,
            FieldKind::DateRange,
            FieldKind::TimeRange,
            FieldKind::Select { options: vec![] },
            FieldKind::Checkbox,
            FieldKind::Member,
            FieldKind::Table { columns: vec![] },
        ];
        for kind in kinds {
            let json = serde_json::to_value(&kind).expect("json");
            assert_eq!(json["kind"], kind.code());
        }
    }

    // ── 속성 테스트 ──

    fn arb_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            any::<f64>()
                .prop_map(|f| serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)),
            any::<String>().prop_map(Value::from),
            "[0-9]{4}-[0-9]{2}-[0-9]{2}".prop_map(Value::from),
            "[0-2][0-9]:[0-5][0-9]".prop_map(Value::from),
            prop_oneof![Just("법인카드"), Just("식대"), Just("교통비")].prop_map(Value::from),
            Just(Value::from(MemberId::new().to_string())),
        ];
        leaf.prop_recursive(4, 48, 6, |inner| {
            let key = prop_oneof![
                any::<String>(),
                prop_oneof![
                    Just("start"),
                    Just("end"),
                    Just("on"),
                    Just("place"),
                    Just("account"),
                    Just("qty"),
                    Just("price"),
                    Just("tax")
                ]
                .prop_map(str::to_owned),
            ];
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::btree_map(key, inner, 0..6)
                    .prop_map(|map| Value::Object(map.into_iter().collect())),
            ]
        })
    }

    /// 양식의 키를 자주 쓰는 값 객체.
    fn arb_values() -> impl Strategy<Value = Value> {
        let key = prop_oneof![
            4 => prop_oneof![
                Just("subject"), Just("detail"), Just("count"), Just("amount"), Just("day"),
                Just("period"), Just("hours"), Just("method"), Just("agree"), Just("handover"),
                Just("items")
            ]
            .prop_map(str::to_owned),
            1 => any::<String>(),
        ];
        prop_oneof![
            8 => prop::collection::btree_map(key, arb_json(), 0..12)
                .prop_map(|map| Value::Object(map.into_iter().collect())),
            1 => arb_json(),
        ]
    }

    proptest! {
        #[test]
        fn validate_values_never_panics_and_output_revalidates(values in arb_values()) {
            let schema = full();
            match schema.validate_values(&values) {
                Ok(out) => {
                    let again = schema.validate_values(&Value::Object(out.values.clone()));
                    prop_assert_eq!(again, Ok(out));
                }
                Err(errors) => {
                    prop_assert!(!errors.is_empty() && errors.len() <= MAX_ERRORS);
                    for error in errors {
                        prop_assert!(error.key.chars().count() <= MAX_KEY_LEN);
                    }
                }
            }
        }

        #[test]
        fn valid_text_survives_a_round_trip(raw in any::<String>()) {
            let schema = full();
            if let Ok(out) = schema.validate_values(&json!({ "subject": raw, "detail": raw })) {
                let again = schema.validate_values(&Value::Object(out.values.clone()));
                prop_assert_eq!(again, Ok(out));
            }
        }

        #[test]
        fn schema_definitions_never_panic_and_normalize_idempotently(value in arb_json()) {
            if let Ok(schema) = serde_json::from_value::<FormSchema>(json!({ "fields": value })) {
                if let Ok(normalized) = schema.normalized() {
                    prop_assert_eq!(normalized.clone().normalized(), Ok(normalized.clone()));
                    let stored = serde_json::to_string(&normalized).expect("json");
                    prop_assert_eq!(parse_stored(&stored), Ok(normalized));
                }
            }
        }

        #[test]
        fn generated_schemas_validate_arbitrary_values(
            kinds in prop::collection::vec(0usize..11, 0..12),
            values in arb_json(),
        ) {
            let fields: Vec<Value> = kinds.iter().enumerate().map(|(index, kind)| {
                let key = format!("f{index}");
                match kind {
                    0 => json!({ "key": key, "label": "글", "kind": "text", "required": index % 2 == 0 }),
                    1 => json!({ "key": key, "label": "글", "kind": "textarea" }),
                    2 => json!({ "key": key, "label": "수", "kind": "number", "min": 0 }),
                    3 => json!({ "key": key, "label": "금액", "kind": "money" }),
                    4 => json!({ "key": key, "label": "일자", "kind": "date" }),
                    5 => json!({ "key": key, "label": "기간", "kind": "date_range" }),
                    6 => json!({ "key": key, "label": "시간", "kind": "time_range" }),
                    7 => json!({ "key": key, "label": "구분", "kind": "select", "options": ["식대"] }),
                    8 => json!({ "key": key, "label": "확인", "kind": "checkbox", "required": true }),
                    9 => json!({ "key": key, "label": "구성원", "kind": "member" }),
                    _ => json!({ "key": key, "label": "표", "kind": "table", "columns": [
                        { "key": "price", "label": "금액", "kind": "money" },
                        { "key": "place", "label": "사용처", "kind": "text" }
                    ]}),
                }
            }).collect();
            let schema = schema(Value::Array(fields));
            // 같은 값을 모든 필드에 넣어 본다.
            let object: Map<String, Value> = schema
                .fields
                .iter()
                .map(|field| (field.key.clone(), values.clone()))
                .collect();
            if let Ok(out) = schema.validate_values(&Value::Object(object)) {
                let again = schema.validate_values(&Value::Object(out.values.clone()));
                prop_assert_eq!(again, Ok(out));
            }
        }
    }
}
