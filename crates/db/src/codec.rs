//! DB 값 ↔ 도메인 값 변환. 저장된 값이 규칙에 맞지 않으면 [`DbError::Corrupt`].

use std::str::FromStr;

use chrono::NaiveDate;
use pango_core::error::{DomainError, ValidationReason};
use pango_core::time::Timestamp;

use crate::error::{DbError, DbResult};

pub fn corrupt(column: &'static str) -> impl FnOnce(DomainError) -> DbError {
    move |source| DbError::Corrupt { column, source }
}

/// `FromStr<Err = DomainError>`인 타입(ID 등)을 읽는다.
pub fn parse<T: FromStr<Err = DomainError>>(column: &'static str, raw: &str) -> DbResult<T> {
    raw.parse().map_err(corrupt(column))
}

pub fn parse_opt<T: FromStr<Err = DomainError>>(
    column: &'static str,
    raw: Option<&str>,
) -> DbResult<Option<T>> {
    raw.map(|value| parse(column, value)).transpose()
}

/// 도메인 타입의 `parse` 함수로 읽는다 (열거형, 검증된 문자열).
pub fn parse_with<T>(
    column: &'static str,
    raw: &str,
    parser: impl FnOnce(&str) -> Result<T, DomainError>,
) -> DbResult<T> {
    parser(raw).map_err(corrupt(column))
}

pub fn timestamp(column: &'static str, millis: i64) -> DbResult<Timestamp> {
    Timestamp::from_millis(millis).map_err(corrupt(column))
}

pub fn timestamp_opt(column: &'static str, millis: Option<i64>) -> DbResult<Option<Timestamp>> {
    millis.map(|value| timestamp(column, value)).transpose()
}

pub fn date(column: &'static str, raw: &str) -> DbResult<NaiveDate> {
    // `NaiveDate::from_str`은 `2026-3-2`도 받으므로 길이를 고정해 표기를 하나로 유지한다.
    (raw.len() == 10)
        .then(|| raw.parse::<NaiveDate>().ok())
        .flatten()
        .ok_or(DomainError::validation(
            column,
            ValidationReason::InvalidFormat,
        ))
        .map_err(corrupt(column))
}

pub fn date_opt(column: &'static str, raw: Option<&str>) -> DbResult<Option<NaiveDate>> {
    raw.map(|value| date(column, value)).transpose()
}

pub fn date_text(value: NaiveDate) -> String {
    value.format("%Y-%m-%d").to_string()
}

pub fn json<T: serde::de::DeserializeOwned>(column: &'static str, raw: &str) -> DbResult<T> {
    serde_json::from_str(raw).map_err(|source| DbError::CorruptJson { column, source })
}

pub fn json_text<T: serde::Serialize>(column: &'static str, value: &T) -> DbResult<String> {
    serde_json::to_string(value).map_err(|source| DbError::CorruptJson { column, source })
}

/// 목록 조회의 크기 제한을 SQL `LIMIT` 값으로 바꾼다. 다음 페이지 존재 여부를 알기 위해
/// 하나 더 읽는다.
pub fn limit_plus_one(limit: u32) -> i64 {
    i64::from(limit) + 1
}

/// `limit + 1`개를 읽은 결과를 페이지와 "다음 있음" 여부로 나눈다.
pub fn split_page<T>(mut rows: Vec<T>, limit: u32) -> (Vec<T>, bool) {
    let limit = limit as usize;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    (rows, has_more)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_requires_fixed_width() {
        assert!(date("d", "2026-03-02").is_ok());
        for bad in [
            "2026-3-2",
            "2026-03-02 ",
            "20260302",
            "",
            "2026-02-30",
            "2026-03-02T00:00:00",
        ] {
            assert!(date("d", bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn date_text_round_trips() {
        let value = NaiveDate::from_ymd_opt(2028, 2, 29).expect("date");
        assert_eq!(date("d", &date_text(value)).expect("parse"), value);
    }

    #[test]
    fn split_page_detects_more() {
        assert_eq!(split_page(vec![1, 2, 3], 2), (vec![1, 2], true));
        assert_eq!(split_page(vec![1, 2], 2), (vec![1, 2], false));
        assert_eq!(split_page(Vec::<i32>::new(), 2), (vec![], false));
    }
}
