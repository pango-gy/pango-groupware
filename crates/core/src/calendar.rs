//! 근무 달력: 근무 요일과 공휴일로 근무일 여부를 판정한다.

use std::collections::BTreeSet;

use chrono::{Datelike, NaiveDate, Weekday};

use crate::error::{DomainError, DomainResult, ValidationReason};

/// 한 번에 다룰 수 있는 최대 기간(일). 달력 순회 비용의 상한이다.
pub const MAX_RANGE_DAYS: i64 = 366;

/// 근무 요일 집합. 비트 0 = 월요일 … 비트 6 = 일요일.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WorkWeek(u8);

impl WorkWeek {
    pub const MON_TO_FRI: Self = Self(0b001_1111);
    const MASK: u8 = 0b111_1111;

    /// 근무 요일이 하루도 없는 값은 받지 않는다.
    pub fn from_bits(bits: u8) -> DomainResult<Self> {
        if bits == 0 || bits & !Self::MASK != 0 {
            return Err(DomainError::validation(
                "workdays",
                ValidationReason::OutOfRange,
            ));
        }
        Ok(Self(bits))
    }

    pub fn from_weekdays(days: impl IntoIterator<Item = Weekday>) -> DomainResult<Self> {
        let bits = days
            .into_iter()
            .fold(0u8, |acc, day| acc | 1 << day.num_days_from_monday());
        Self::from_bits(bits)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub fn contains(self, day: Weekday) -> bool {
        self.0 & (1 << day.num_days_from_monday()) != 0
    }
}

/// 날짜 구간(양 끝 포함). 만들 때 순서와 길이를 확인한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DateRange {
    start: NaiveDate,
    end: NaiveDate,
}

impl DateRange {
    pub fn new(start: NaiveDate, end: NaiveDate) -> DomainResult<Self> {
        if end < start {
            return Err(DomainError::validation(
                "date_range",
                ValidationReason::Inconsistent,
            ));
        }
        if (end - start).num_days() >= MAX_RANGE_DAYS {
            return Err(DomainError::validation(
                "date_range",
                ValidationReason::OutOfRange,
            ));
        }
        Ok(Self { start, end })
    }

    pub fn single(date: NaiveDate) -> Self {
        Self {
            start: date,
            end: date,
        }
    }

    /// `YYYY-MM` 한 달 전체.
    pub fn month(year: i32, month: u32) -> DomainResult<Self> {
        let invalid = DomainError::validation("month", ValidationReason::OutOfRange);
        let start = NaiveDate::from_ymd_opt(year, month, 1).ok_or(invalid.clone())?;
        let next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)
        };
        let end = next.and_then(|d| d.pred_opt()).ok_or(invalid)?;
        Ok(Self { start, end })
    }

    pub const fn start(&self) -> NaiveDate {
        self.start
    }

    pub const fn end(&self) -> NaiveDate {
        self.end
    }

    pub fn contains(&self, date: NaiveDate) -> bool {
        self.start <= date && date <= self.end
    }

    pub fn overlaps(&self, other: &DateRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    pub fn days(&self) -> impl Iterator<Item = NaiveDate> {
        let end = self.end;
        self.start.iter_days().take_while(move |d| *d <= end)
    }
}

/// 쿼리·본문으로 받는 날짜가 다룰 수 있는 연도 범위. 출퇴근 시각 범위(1900~9999년)와 같다.
const MIN_INPUT_YEAR: i32 = 1900;
const MAX_INPUT_YEAR: i32 = 9999;

/// 고정 폭 숫자 묶음을 읽는다. `str::parse`는 `+`를 받으므로 숫자만인지 먼저 확인한다.
fn fixed_digits(raw: &str) -> Option<u32> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

fn supported_year(year: u32) -> Option<i32> {
    i32::try_from(year)
        .ok()
        .filter(|y| (MIN_INPUT_YEAR..=MAX_INPUT_YEAR).contains(y))
}

/// `YYYY-MM`(두 자리 월 고정)을 그 달 전체 구간으로 바꾼다.
///
/// `2026-1`처럼 표기가 여러 개면 같은 달이 링크·캐시에서 다른 값이 되므로 하나만 받는다.
pub fn parse_month(raw: &str) -> DomainResult<DateRange> {
    let invalid = || DomainError::validation("month", ValidationReason::InvalidFormat);
    let (year, month) = raw
        .split_once('-')
        .filter(|(y, m)| y.len() == 4 && m.len() == 2)
        .ok_or_else(invalid)?;
    let year = fixed_digits(year)
        .and_then(supported_year)
        .ok_or_else(invalid)?;
    let month = fixed_digits(month)
        .filter(|m| (1..=12).contains(m))
        .ok_or_else(invalid)?;
    DateRange::month(year, month).map_err(|_| invalid())
}

/// `YYYY-MM-DD`(고정 폭)만 받는다. `field`는 오류에 실을 입력 이름이다.
///
/// `NaiveDate::from_str`은 `2026-3-2`도 받으므로 외부 입력에는 이 함수를 쓴다.
pub fn parse_date(field: &'static str, raw: &str) -> DomainResult<NaiveDate> {
    let invalid = || DomainError::validation(field, ValidationReason::InvalidFormat);
    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(invalid());
    }
    let part = |from: usize, to: usize| raw.get(from..to).and_then(fixed_digits);
    let year = part(0, 4).and_then(supported_year).ok_or_else(invalid)?;
    let (month, day) = part(5, 7).zip(part(8, 10)).ok_or_else(invalid)?;
    NaiveDate::from_ymd_opt(year, month, day).ok_or_else(invalid)
}

/// 근무 요일 + 공휴일.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkCalendar {
    work_week: WorkWeek,
    holidays: BTreeSet<NaiveDate>,
}

impl WorkCalendar {
    pub fn new(work_week: WorkWeek, holidays: impl IntoIterator<Item = NaiveDate>) -> Self {
        Self {
            work_week,
            holidays: holidays.into_iter().collect(),
        }
    }

    pub fn is_holiday(&self, date: NaiveDate) -> bool {
        self.holidays.contains(&date)
    }

    pub fn is_working_day(&self, date: NaiveDate) -> bool {
        self.work_week.contains(date.weekday()) && !self.is_holiday(date)
    }

    pub fn working_days_in(&self, range: DateRange) -> Vec<NaiveDate> {
        range.days().filter(|d| self.is_working_day(*d)).collect()
    }

    pub fn count_working_days(&self, range: DateRange) -> u32 {
        // MAX_RANGE_DAYS 이하이므로 u32로 충분하다.
        range.days().filter(|d| self.is_working_day(*d)).count() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(value: &str) -> NaiveDate {
        value.parse().expect("date")
    }

    fn calendar(holidays: &[&str]) -> WorkCalendar {
        WorkCalendar::new(WorkWeek::MON_TO_FRI, holidays.iter().map(|h| d(h)))
    }

    #[test]
    fn work_week_bits() {
        assert!(WorkWeek::MON_TO_FRI.contains(Weekday::Mon));
        assert!(WorkWeek::MON_TO_FRI.contains(Weekday::Fri));
        assert!(!WorkWeek::MON_TO_FRI.contains(Weekday::Sat));
        assert!(
            WorkWeek::from_bits(0).is_err(),
            "근무 요일이 없으면 안 된다"
        );
        assert!(WorkWeek::from_bits(0b1000_0000).is_err());
        assert!(WorkWeek::from_bits(0b111_1111).is_ok());
        assert_eq!(
            WorkWeek::from_weekdays([
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri
            ]),
            Ok(WorkWeek::MON_TO_FRI)
        );
        assert!(WorkWeek::from_weekdays([]).is_err());
    }

    #[test]
    fn date_range_validation() {
        assert!(DateRange::new(d("2026-03-02"), d("2026-03-01")).is_err());
        assert!(DateRange::new(d("2026-03-01"), d("2026-03-01")).is_ok());
        assert!(
            DateRange::new(d("2026-01-01"), d("2027-01-01")).is_ok(),
            "366일"
        );
        assert!(
            DateRange::new(d("2026-01-01"), d("2027-01-02")).is_err(),
            "367일"
        );
    }

    #[test]
    fn month_range_handles_year_end_and_leap_february() {
        let dec = DateRange::month(2026, 12).expect("december");
        assert_eq!((dec.start(), dec.end()), (d("2026-12-01"), d("2026-12-31")));
        assert_eq!(
            DateRange::month(2028, 2).expect("leap").end(),
            d("2028-02-29")
        );
        assert_eq!(
            DateRange::month(2026, 2).expect("common").end(),
            d("2026-02-28")
        );
        assert!(DateRange::month(2026, 0).is_err());
        assert!(DateRange::month(2026, 13).is_err());
        assert!(DateRange::month(i32::MAX, 12).is_err());
    }

    #[test]
    fn overlap_is_inclusive_on_both_ends() {
        let a = DateRange::new(d("2026-03-02"), d("2026-03-04")).expect("a");
        let touching = DateRange::new(d("2026-03-04"), d("2026-03-06")).expect("touching");
        let after = DateRange::new(d("2026-03-05"), d("2026-03-06")).expect("after");
        assert!(a.overlaps(&touching) && touching.overlaps(&a));
        assert!(!a.overlaps(&after) && !after.overlaps(&a));
        assert!(a.contains(d("2026-03-02")) && a.contains(d("2026-03-04")));
        assert!(!a.contains(d("2026-03-05")));
    }

    #[test]
    fn counts_working_days_excluding_weekends_and_holidays() {
        // 2026-03-02(월) ~ 2026-03-08(일), 03-04(수) 공휴일
        let cal = calendar(&["2026-03-04", "2026-03-07"]);
        let week = DateRange::new(d("2026-03-02"), d("2026-03-08")).expect("week");
        assert_eq!(cal.count_working_days(week), 4);
        assert_eq!(
            cal.working_days_in(week),
            vec![
                d("2026-03-02"),
                d("2026-03-03"),
                d("2026-03-05"),
                d("2026-03-06")
            ]
        );
        assert_eq!(
            cal.count_working_days(DateRange::single(d("2026-03-07"))),
            0
        );
        assert!(cal.is_holiday(d("2026-03-07")) && !cal.is_working_day(d("2026-03-07")));
    }

    #[test]
    fn parse_month_accepts_only_strict_yyyy_mm() {
        let march = parse_month("2026-03").expect("march");
        assert_eq!(
            (march.start(), march.end()),
            (d("2026-03-01"), d("2026-03-31"))
        );
        assert_eq!(parse_month("2028-02").expect("leap").end(), d("2028-02-29"));
        assert_eq!(
            parse_month("2026-12").expect("december").end(),
            d("2026-12-31")
        );
        for bad in [
            "2026-13",
            "2026-00",
            "0000-00",
            "0000-01",
            "1899-12",
            "2026-1",
            "2026-3",
            "26-03",
            "2026/03",
            "2026-03-01",
            "202603",
            "",
            " 2026-03",
            "2026-03 ",
            "+2026-03",
            "99999-01",
            "2026-0a",
            "２０２６-03",
        ] {
            assert_eq!(
                parse_month(bad),
                Err(DomainError::validation(
                    "month",
                    ValidationReason::InvalidFormat
                )),
                "accepted {bad:?}"
            );
        }
        assert!(parse_month("9999-12").is_ok());
    }

    #[test]
    fn parse_date_accepts_only_strict_yyyy_mm_dd() {
        assert_eq!(parse_date("from", "2028-02-29"), Ok(d("2028-02-29")));
        for bad in [
            "2026-3-2",
            "2026-02-30",
            "2026-13-01",
            "20260302",
            "2026-03-02T00:00:00",
            "",
            " 2026-03-02",
            "+2026-03-02",
            "0000-01-01",
            "1899-12-31",
            "2026-03-0x",
        ] {
            assert_eq!(
                parse_date("from", bad),
                Err(DomainError::validation(
                    "from",
                    ValidationReason::InvalidFormat
                )),
                "accepted {bad:?}"
            );
        }
    }

    #[test]
    fn custom_work_week_including_saturday() {
        let week = WorkWeek::from_weekdays([Weekday::Tue, Weekday::Sat]).expect("valid");
        let cal = WorkCalendar::new(week, []);
        assert!(cal.is_working_day(d("2026-03-07")));
        assert!(!cal.is_working_day(d("2026-03-02")));
    }
}
