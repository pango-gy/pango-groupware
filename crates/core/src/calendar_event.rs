//! 캘린더 일정의 값 규칙: 일정 시간, 참석자, 조회 기간, 장소·설명.
//!
//! - 종일 일정은 회사 시간대와 무관한 **날짜**(`YYYY-MM-DD`, 양 끝 포함)로, 시간 일정은
//!   [`Timestamp`]로 저장한다. 하나의 일정은 둘 중 하나만 가진다.
//! - 일정 하나는 최대 [`MAX_EVENT_DAYS`]일까지 걸칠 수 있다.
//! - 반복 일정과 미리 알림은 MVP에 없다.

use std::collections::BTreeSet;

use chrono::{Duration, NaiveDate};

use crate::calendar::{check_date, DateRange};
use crate::error::{DomainError, DomainResult, ValidationReason};
use crate::id::MemberId;
use crate::string_enum;
use crate::text::{optional, TextRule};
use crate::time::Timestamp;

/// 일정 하나가 걸칠 수 있는 최대 일수(종일: 날짜 수, 시간: 31×24시간).
pub const MAX_EVENT_DAYS: i64 = 31;
/// 캘린더 화면 한 번에 조회할 수 있는 최대 일수(양 끝 포함).
pub const MAX_VIEW_DAYS: i64 = 62;
/// 참석자 일정 확인으로 한 번에 조회할 수 있는 최대 일수(양 끝 포함).
pub const MAX_CONFLICT_DAYS: i64 = 7;
/// 일정 하나에 초대할 수 있는 최대 인원(주인 제외).
pub const MAX_ATTENDEES: usize = 50;

/// 시간 일정이 쓸 수 있는 시각 범위: 1900-01-01T00:00Z 이상, 10000-01-01T00:00Z 미만.
/// 날짜 입력의 연도 범위([`check_date`])와 같다.
const MIN_EVENT_MILLIS: i64 = -2_208_988_800_000;
const MAX_EVENT_MILLIS: i64 = 253_402_300_800_000;

const LOCATION: TextRule = TextRule {
    field: "location",
    min_chars: 0,
    max_chars: 200,
    multiline: false,
};

const DESCRIPTION: TextRule = TextRule {
    field: "description",
    min_chars: 0,
    max_chars: 5_000,
    multiline: true,
};

string_enum! {
    /// 참석자의 응답.
    pub enum AttendeeResponse("status") {
        Pending => "pending",
        Accepted => "accepted",
        Declined => "declined",
    }
}

/// 일정의 시간. 종일 일정은 날짜(양 끝 포함), 시간 일정은 `[start, end)` 시각.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventTime {
    AllDay { start: NaiveDate, end: NaiveDate },
    Timed { start: Timestamp, end: Timestamp },
}

fn invalid(field: &'static str, reason: ValidationReason) -> DomainError {
    DomainError::validation(field, reason)
}

fn check_timestamp(field: &'static str, at: Timestamp) -> DomainResult<Timestamp> {
    if (MIN_EVENT_MILLIS..MAX_EVENT_MILLIS).contains(&at.as_millis()) {
        Ok(at)
    } else {
        Err(invalid(field, ValidationReason::OutOfRange))
    }
}

/// 값이 있으면 안 되는 필드(종일 일정의 시각 등). 둘 다 보낸 요청은 의도를 알 수 없다.
fn forbid<T>(field: &'static str, value: Option<T>) -> DomainResult<()> {
    match value {
        None => Ok(()),
        Some(_) => Err(invalid(field, ValidationReason::Inconsistent)),
    }
}

fn require<T>(field: &'static str, value: Option<T>) -> DomainResult<T> {
    value.ok_or(invalid(field, ValidationReason::Empty))
}

impl EventTime {
    /// 요청 필드로 일정 시간을 만든다.
    ///
    /// - 종일: `start_date` 필수, `end_date`는 생략하면 같은 날. 시각 필드는 보내지 않는다.
    /// - 시간: `start_at`·`end_at` 필수, `end_at > start_at`. 날짜 필드는 보내지 않는다.
    /// - 어느 쪽이든 [`MAX_EVENT_DAYS`]일을 넘으면 `OutOfRange`.
    pub fn new(
        all_day: bool,
        start_date: Option<NaiveDate>,
        end_date: Option<NaiveDate>,
        start_at: Option<Timestamp>,
        end_at: Option<Timestamp>,
    ) -> DomainResult<Self> {
        if all_day {
            forbid("start_at", start_at)?;
            forbid("end_at", end_at)?;
            let start = check_date("start_date", require("start_date", start_date)?)?;
            let end = check_date("end_date", end_date.unwrap_or(start))?;
            if end < start {
                return Err(invalid("end_date", ValidationReason::Inconsistent));
            }
            if (end - start).num_days() + 1 > MAX_EVENT_DAYS {
                return Err(invalid("end_date", ValidationReason::OutOfRange));
            }
            Ok(Self::AllDay { start, end })
        } else {
            forbid("start_date", start_date)?;
            forbid("end_date", end_date)?;
            let start = check_timestamp("start_at", require("start_at", start_at)?)?;
            let end = check_timestamp("end_at", require("end_at", end_at)?)?;
            if end <= start {
                return Err(invalid("end_at", ValidationReason::Inconsistent));
            }
            if end.since(start) > Duration::days(MAX_EVENT_DAYS) {
                return Err(invalid("end_at", ValidationReason::OutOfRange));
            }
            Ok(Self::Timed { start, end })
        }
    }

    pub const fn is_all_day(&self) -> bool {
        matches!(self, Self::AllDay { .. })
    }

    pub const fn dates(&self) -> Option<(NaiveDate, NaiveDate)> {
        match self {
            Self::AllDay { start, end } => Some((*start, *end)),
            Self::Timed { .. } => None,
        }
    }

    pub const fn instants(&self) -> Option<(Timestamp, Timestamp)> {
        match self {
            Self::Timed { start, end } => Some((*start, *end)),
            Self::AllDay { .. } => None,
        }
    }
}

/// 조회 기간(`from`~`to`, 양 끝 포함). `to < from`이면 `Inconsistent`, `max_days`일을 넘으면
/// `OutOfRange`(둘 다 `to` 필드).
pub fn bounded_range(from: NaiveDate, to: NaiveDate, max_days: i64) -> DomainResult<DateRange> {
    if to < from {
        return Err(invalid("to", ValidationReason::Inconsistent));
    }
    if (to - from).num_days() + 1 > max_days {
        return Err(invalid("to", ValidationReason::OutOfRange));
    }
    DateRange::new(from, to).map_err(|error| error.with_field("to"))
}

/// 참석자 목록을 확인한다: [`MAX_ATTENDEES`]명 이하, 중복 없음, 주인 제외. 존재·재직 여부는
/// 저장소를 봐야 하므로 서비스가 확인한다.
pub fn check_attendees(owner: MemberId, attendees: &[MemberId]) -> DomainResult<()> {
    const FIELD: &str = "attendee_ids";
    if attendees.len() > MAX_ATTENDEES {
        return Err(invalid(FIELD, ValidationReason::OutOfRange));
    }
    let mut seen = BTreeSet::new();
    for id in attendees {
        if *id == owner {
            return Err(invalid(FIELD, ValidationReason::Inconsistent));
        }
        if !seen.insert(*id) {
            return Err(invalid(FIELD, ValidationReason::Duplicate));
        }
    }
    Ok(())
}

/// 장소(선택, 한 줄, 200자 이하). 비어 있으면 `None`.
pub fn parse_location(raw: Option<&str>) -> DomainResult<Option<String>> {
    optional(raw, |value| LOCATION.normalize(value))
}

/// 설명(선택, 여러 줄, 5,000자 이하). 비어 있으면 `None`.
pub fn parse_description(raw: Option<&str>) -> DomainResult<Option<String>> {
    optional(raw, |value| DESCRIPTION.normalize(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(value: &str) -> NaiveDate {
        value.parse().expect("date")
    }

    fn t(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis).expect("timestamp")
    }

    fn reason<T: std::fmt::Debug>(result: DomainResult<T>) -> (&'static str, ValidationReason) {
        match result {
            Err(DomainError::Validation { field, reason }) => (field, reason),
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;

    #[test]
    fn all_day_event_defaults_to_a_single_day() {
        assert_eq!(
            EventTime::new(true, Some(d("2026-03-02")), None, None, None),
            Ok(EventTime::AllDay {
                start: d("2026-03-02"),
                end: d("2026-03-02")
            })
        );
    }

    #[test]
    fn all_day_span_is_limited_to_31_dates() {
        let start = d("2026-03-01");
        let ok = EventTime::new(true, Some(start), Some(d("2026-03-31")), None, None);
        assert!(ok.is_ok());
        assert_eq!(
            reason(EventTime::new(
                true,
                Some(start),
                Some(d("2026-04-01")),
                None,
                None
            )),
            ("end_date", ValidationReason::OutOfRange)
        );
        assert_eq!(
            reason(EventTime::new(
                true,
                Some(start),
                Some(d("2026-02-28")),
                None,
                None
            )),
            ("end_date", ValidationReason::Inconsistent)
        );
    }

    #[test]
    fn timed_span_is_limited_to_31_days_and_must_be_positive() {
        let start = t(0);
        assert!(EventTime::new(false, None, None, Some(start), Some(t(31 * DAY))).is_ok());
        assert_eq!(
            reason(EventTime::new(
                false,
                None,
                None,
                Some(start),
                Some(t(31 * DAY + 1))
            )),
            ("end_at", ValidationReason::OutOfRange)
        );
        assert_eq!(
            reason(EventTime::new(false, None, None, Some(start), Some(start))),
            ("end_at", ValidationReason::Inconsistent)
        );
        assert!(EventTime::new(false, None, None, Some(start), Some(t(1))).is_ok());
    }

    #[test]
    fn mixing_date_and_time_fields_is_rejected() {
        let date = Some(d("2026-03-02"));
        let at = Some(t(HOUR));
        assert_eq!(
            reason(EventTime::new(true, date, None, at, None)),
            ("start_at", ValidationReason::Inconsistent)
        );
        assert_eq!(
            reason(EventTime::new(true, date, None, None, at)),
            ("end_at", ValidationReason::Inconsistent)
        );
        assert_eq!(
            reason(EventTime::new(false, date, None, Some(t(0)), at)),
            ("start_date", ValidationReason::Inconsistent)
        );
        assert_eq!(
            reason(EventTime::new(false, None, date, Some(t(0)), at)),
            ("end_date", ValidationReason::Inconsistent)
        );
    }

    #[test]
    fn required_fields_are_named_in_errors() {
        assert_eq!(
            reason(EventTime::new(true, None, None, None, None)),
            ("start_date", ValidationReason::Empty)
        );
        assert_eq!(
            reason(EventTime::new(false, None, None, None, Some(t(1)))),
            ("start_at", ValidationReason::Empty)
        );
        assert_eq!(
            reason(EventTime::new(false, None, None, Some(t(1)), None)),
            ("end_at", ValidationReason::Empty)
        );
    }

    #[test]
    fn values_outside_the_storable_years_are_rejected() {
        let old = NaiveDate::from_ymd_opt(1899, 12, 31).expect("date");
        assert_eq!(
            reason(EventTime::new(true, Some(old), None, None, None)),
            ("start_date", ValidationReason::OutOfRange)
        );
        assert_eq!(
            reason(EventTime::new(
                false,
                None,
                None,
                Some(t(MIN_EVENT_MILLIS - 1)),
                Some(t(0))
            )),
            ("start_at", ValidationReason::OutOfRange)
        );
        assert_eq!(
            reason(EventTime::new(
                false,
                None,
                None,
                Some(t(MAX_EVENT_MILLIS - HOUR)),
                Some(t(MAX_EVENT_MILLIS))
            )),
            ("end_at", ValidationReason::OutOfRange)
        );
        assert!(EventTime::new(
            false,
            None,
            None,
            Some(t(MIN_EVENT_MILLIS)),
            Some(t(MIN_EVENT_MILLIS + 1))
        )
        .is_ok());
    }

    #[test]
    fn storable_year_bounds_match_the_calendar_dates() {
        use chrono::{TimeZone, Utc};
        let min = Utc
            .with_ymd_and_hms(1900, 1, 1, 0, 0, 0)
            .single()
            .expect("min");
        let max = Utc
            .with_ymd_and_hms(10000, 1, 1, 0, 0, 0)
            .single()
            .expect("max");
        assert_eq!(min.timestamp_millis(), MIN_EVENT_MILLIS);
        assert_eq!(max.timestamp_millis(), MAX_EVENT_MILLIS);
    }

    #[test]
    fn view_range_boundaries() {
        let from = d("2026-03-01");
        assert!(bounded_range(from, d("2026-05-01"), MAX_VIEW_DAYS).is_ok()); // 62일
        assert_eq!(
            reason(bounded_range(from, d("2026-05-02"), MAX_VIEW_DAYS)),
            ("to", ValidationReason::OutOfRange)
        );
        assert_eq!(
            reason(bounded_range(from, d("2026-02-28"), MAX_VIEW_DAYS)),
            ("to", ValidationReason::Inconsistent)
        );
        assert!(bounded_range(from, from, MAX_CONFLICT_DAYS).is_ok());
        assert!(bounded_range(from, d("2026-03-07"), MAX_CONFLICT_DAYS).is_ok());
        assert!(bounded_range(from, d("2026-03-08"), MAX_CONFLICT_DAYS).is_err());
    }

    #[test]
    fn attendee_list_rules() {
        let owner = MemberId::new();
        let fifty: Vec<_> = (0..MAX_ATTENDEES).map(|_| MemberId::new()).collect();
        assert_eq!(check_attendees(owner, &fifty), Ok(()));
        let mut fifty_one = fifty.clone();
        fifty_one.push(MemberId::new());
        assert_eq!(
            reason(check_attendees(owner, &fifty_one)),
            ("attendee_ids", ValidationReason::OutOfRange)
        );
        let twice = vec![fifty[0], fifty[0]];
        assert_eq!(
            reason(check_attendees(owner, &twice)),
            ("attendee_ids", ValidationReason::Duplicate)
        );
        assert_eq!(
            reason(check_attendees(owner, &[owner])),
            ("attendee_ids", ValidationReason::Inconsistent)
        );
        assert_eq!(check_attendees(owner, &[]), Ok(()));
    }

    #[test]
    fn location_and_description_limits() {
        assert_eq!(parse_location(None), Ok(None));
        assert_eq!(parse_location(Some("  ")), Ok(None));
        assert_eq!(
            parse_location(Some(" 3층 회의실 ")),
            Ok(Some("3층 회의실".into()))
        );
        assert!(parse_location(Some(&"가".repeat(200))).is_ok());
        assert_eq!(
            reason(parse_location(Some(&"가".repeat(201)))),
            ("location", ValidationReason::TooLong { max: 200 })
        );
        assert_eq!(
            reason(parse_location(Some("회의실\n2층"))),
            ("location", ValidationReason::InvalidCharacter)
        );
        assert_eq!(
            parse_description(Some("첫 줄\n둘째 줄")),
            Ok(Some("첫 줄\n둘째 줄".into()))
        );
        assert!(parse_description(Some(&"a".repeat(5_000))).is_ok());
        assert_eq!(
            reason(parse_description(Some(&"a".repeat(5_001)))),
            ("description", ValidationReason::TooLong { max: 5_000 })
        );
        assert_eq!(
            reason(parse_description(Some("안\u{202E}녕"))),
            ("description", ValidationReason::InvalidCharacter)
        );
    }

    #[test]
    fn response_wire_strings_are_fixed() {
        let all: Vec<_> = AttendeeResponse::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(all, ["pending", "accepted", "declined"]);
    }
}
