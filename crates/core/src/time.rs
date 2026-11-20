//! 시각·시계·근무일 계산.
//!
//! - [`Timestamp`]: UTC 밀리초. 저장과 전송에 쓰는 유일한 시각 표현.
//! - [`MinuteOfDay`]: 하루 중 시각(분 단위). 근무 시작·종료 같은 정책 값.
//! - [`WorkCalendarZone`]: 회사 시간대와 근무일 전환 시각. 시각 ↔ 근무일 변환을 담당.

use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};

use chrono::{DateTime, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::error::{DomainError, DomainResult, ValidationReason};

/// UTC 기준 유닉스 밀리초.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct Timestamp(i64);

impl TryFrom<i64> for Timestamp {
    type Error = DomainError;
    fn try_from(millis: i64) -> DomainResult<Self> {
        Self::from_millis(millis)
    }
}

impl From<Timestamp> for i64 {
    fn from(value: Timestamp) -> Self {
        value.0
    }
}

impl Timestamp {
    /// chrono가 표현할 수 있는 범위(약 ±26만 년) 안의 값만 만든다.
    pub fn from_millis(millis: i64) -> DomainResult<Self> {
        DateTime::<Utc>::from_timestamp_millis(millis)
            .map(|_| Self(millis))
            .ok_or(DomainError::validation("timestamp", ValidationReason::OutOfRange))
    }

    pub const fn as_millis(self) -> i64 {
        self.0
    }

    pub fn from_utc(value: DateTime<Utc>) -> Self {
        Self(value.timestamp_millis())
    }

    pub fn to_utc(self) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp_millis(self.0)
            .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
    }

    /// 범위를 벗어나면 `None`.
    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        self.0.checked_add(duration.num_milliseconds()).and_then(|ms| Self::from_millis(ms).ok())
    }

    pub fn checked_sub(self, duration: Duration) -> Option<Self> {
        self.0.checked_sub(duration.num_milliseconds()).and_then(|ms| Self::from_millis(ms).ok())
    }

    /// `self - earlier`. 음수일 수 있다.
    pub fn since(self, earlier: Self) -> Duration {
        Duration::milliseconds(self.0.saturating_sub(earlier.0))
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.to_utc().to_rfc3339_opts(chrono::SecondsFormat::Millis, true).fmt(f)
    }
}

/// 현재 시각 공급자. 서비스 코드는 이 트레이트로만 시각을 얻는다.
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// 운영 환경 시계.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

#[cfg(not(target_arch = "wasm32"))]
impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0);
        Timestamp(millis)
    }
}

/// 테스트용 시계. 직접 맞추거나 앞으로 돌린다.
#[derive(Debug)]
pub struct ManualClock(AtomicI64);

impl ManualClock {
    pub fn new(start: Timestamp) -> Self {
        Self(AtomicI64::new(start.0))
    }

    pub fn set(&self, now: Timestamp) {
        self.0.store(now.0, Ordering::SeqCst);
    }

    pub fn advance(&self, duration: Duration) {
        self.0.fetch_add(duration.num_milliseconds(), Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Timestamp {
        Timestamp(self.0.load(Ordering::SeqCst))
    }
}

/// 하루 중 시각. 자정부터의 분(0..=1439).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MinuteOfDay(u16);

impl MinuteOfDay {
    pub const MIDNIGHT: Self = Self(0);
    pub const MINUTES_PER_DAY: u16 = 24 * 60;

    pub fn new(minutes: u16) -> DomainResult<Self> {
        if minutes < Self::MINUTES_PER_DAY {
            Ok(Self(minutes))
        } else {
            Err(DomainError::validation("time_of_day", ValidationReason::OutOfRange))
        }
    }

    pub fn from_hm(hour: u16, minute: u16) -> DomainResult<Self> {
        if hour < 24 && minute < 60 {
            Ok(Self(hour * 60 + minute))
        } else {
            Err(DomainError::validation("time_of_day", ValidationReason::OutOfRange))
        }
    }

    /// `HH:MM` 형식(두 자리 고정)만 받는다.
    pub fn parse(value: &str) -> DomainResult<Self> {
        const INVALID: DomainError =
            DomainError::validation("time_of_day", ValidationReason::InvalidFormat);
        let bytes = value.as_bytes();
        if bytes.len() != 5 || bytes[2] != b':' {
            return Err(INVALID);
        }
        let digit = |b: u8| b.is_ascii_digit().then(|| u16::from(b - b'0')).ok_or(INVALID);
        let hour = digit(bytes[0])? * 10 + digit(bytes[1])?;
        let minute = digit(bytes[3])? * 10 + digit(bytes[4])?;
        Self::from_hm(hour, minute)
    }

    pub const fn minutes(self) -> u16 {
        self.0
    }
}

impl fmt::Display for MinuteOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.0 / 60, self.0 % 60)
    }
}

impl Serialize for MinuteOfDay {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MinuteOfDay {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// IANA 시간대 이름을 검증해 파싱한다.
pub fn parse_timezone(name: &str) -> DomainResult<Tz> {
    name.parse::<Tz>()
        .map_err(|_| DomainError::validation("timezone", ValidationReason::InvalidFormat))
}

/// 회사 시간대와 근무일 전환 시각.
///
/// `day_cutoff`가 05:00이면 05:00 이전의 시각은 **전날 근무일**에 속한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkCalendarZone {
    tz: Tz,
    day_cutoff: MinuteOfDay,
}

impl WorkCalendarZone {
    /// 전환 시각은 정오 이전이어야 한다. 그보다 늦으면 "오전 출근"이 전날로 잡힌다.
    pub fn new(tz: Tz, day_cutoff: MinuteOfDay) -> DomainResult<Self> {
        if day_cutoff.minutes() > 12 * 60 {
            return Err(DomainError::validation("day_cutoff", ValidationReason::OutOfRange));
        }
        Ok(Self { tz, day_cutoff })
    }

    pub const fn timezone(&self) -> Tz {
        self.tz
    }

    pub const fn day_cutoff(&self) -> MinuteOfDay {
        self.day_cutoff
    }

    pub fn local(&self, at: Timestamp) -> DateTime<Tz> {
        at.to_utc().with_timezone(&self.tz)
    }

    /// 회사 시간대의 달력 날짜(전환 시각 미적용).
    pub fn calendar_date(&self, at: Timestamp) -> NaiveDate {
        self.local(at).date_naive()
    }

    /// 이 시각이 속한 근무일.
    pub fn work_date(&self, at: Timestamp) -> NaiveDate {
        let shifted = self.local(at).naive_local()
            - Duration::minutes(i64::from(self.day_cutoff.minutes()));
        shifted.date()
    }

    /// 달력 날짜의 벽시계 시각을 실제 시각으로 바꾼다.
    ///
    /// 서머타임으로 존재하지 않는 시각은 건너뛴 직후로, 두 번 존재하는 시각은
    /// 먼저 오는 쪽으로 해석한다.
    pub fn instant_at(&self, date: NaiveDate, time: MinuteOfDay) -> Timestamp {
        let naive = date.and_time(chrono::NaiveTime::MIN)
            + Duration::minutes(i64::from(time.minutes()));
        Timestamp::from_utc(self.resolve_local(naive))
    }

    /// 근무일이 시작되는 시각(전환 시각). 근무일이 끝나는 시각은 다음 날의 이 값이다.
    pub fn work_date_start(&self, work_date: NaiveDate) -> Timestamp {
        self.instant_at(work_date, self.day_cutoff)
    }

    fn resolve_local(&self, naive: NaiveDateTime) -> DateTime<Utc> {
        match self.tz.from_local_datetime(&naive) {
            LocalResult::Single(dt) => dt.with_timezone(&Utc),
            LocalResult::Ambiguous(first, _) => first.with_timezone(&Utc),
            LocalResult::None => {
                // 서머타임 공백. 공백은 최대 몇 시간이므로 분 단위로 전진하며 찾는다.
                (1..=180)
                    .find_map(|step| {
                        self.tz
                            .from_local_datetime(&(naive + Duration::minutes(step)))
                            .earliest()
                    })
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|| Utc.from_utc_datetime(&naive))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn seoul(cutoff: &str) -> WorkCalendarZone {
        WorkCalendarZone::new(chrono_tz::Asia::Seoul, MinuteOfDay::parse(cutoff).expect("cutoff"))
            .expect("zone")
    }

    fn ts(rfc3339: &str) -> Timestamp {
        Timestamp::from_utc(
            DateTime::parse_from_rfc3339(rfc3339).expect("rfc3339").with_timezone(&Utc),
        )
    }

    fn date(value: &str) -> NaiveDate {
        value.parse().expect("date")
    }

    #[test]
    fn minute_of_day_parses_fixed_width_only() {
        assert_eq!(MinuteOfDay::parse("09:00").map(MinuteOfDay::minutes), Ok(540));
        assert_eq!(MinuteOfDay::parse("00:00").map(MinuteOfDay::minutes), Ok(0));
        assert_eq!(MinuteOfDay::parse("23:59").map(MinuteOfDay::minutes), Ok(1439));
        for bad in ["24:00", "23:60", "9:00", "09:0", "0900", "09:00:00", "", "ab:cd", "+9:00", "०९:००", " 9:00"] {
            assert!(MinuteOfDay::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn minute_of_day_display_round_trips() {
        for minutes in [0u16, 1, 59, 60, 540, 1439] {
            let value = MinuteOfDay::new(minutes).expect("valid");
            assert_eq!(MinuteOfDay::parse(&value.to_string()), Ok(value));
        }
        assert!(MinuteOfDay::new(1440).is_err());
    }

    #[test]
    fn timestamp_rejects_out_of_range_millis() {
        assert!(Timestamp::from_millis(i64::MAX).is_err());
        assert!(Timestamp::from_millis(i64::MIN).is_err());
        assert!(Timestamp::from_millis(0).is_ok());
    }

    #[test]
    fn timestamp_deserialization_validates_range() {
        assert_eq!(serde_json::from_str::<Timestamp>("1000").map(Timestamp::as_millis).ok(), Some(1000));
        assert!(serde_json::from_str::<Timestamp>("9223372036854775807").is_err());
        assert!(serde_json::from_str::<Timestamp>("\"1000\"").is_err());
        assert!(serde_json::from_str::<Timestamp>("1.5").is_err());
    }

    #[test]
    fn timestamp_arithmetic_does_not_overflow() {
        let t = Timestamp::from_millis(0).expect("epoch");
        assert_eq!(t.checked_add(Duration::MAX), None);
        assert_eq!(t.checked_sub(Duration::MAX), None);
        assert_eq!(t.checked_add(Duration::minutes(1)).map(Timestamp::as_millis), Some(60_000));
    }

    #[test]
    fn work_date_before_cutoff_belongs_to_previous_day() {
        let zone = seoul("05:00");
        // 2026-03-02 04:59:59.999 KST
        assert_eq!(zone.work_date(ts("2026-03-01T19:59:59.999Z")), date("2026-03-01"));
        // 2026-03-02 05:00:00.000 KST
        assert_eq!(zone.work_date(ts("2026-03-01T20:00:00Z")), date("2026-03-02"));
        // 달력 날짜는 전환 시각과 무관
        assert_eq!(zone.calendar_date(ts("2026-03-01T19:59:59.999Z")), date("2026-03-02"));
    }

    #[test]
    fn zero_cutoff_matches_calendar_date() {
        let zone = seoul("00:00");
        assert_eq!(zone.work_date(ts("2026-12-31T14:59:59Z")), date("2026-12-31"));
        assert_eq!(zone.work_date(ts("2026-12-31T15:00:00Z")), date("2027-01-01"));
    }

    #[test]
    fn cutoff_after_noon_is_rejected() {
        let noon = MinuteOfDay::parse("12:00").expect("noon");
        let after = MinuteOfDay::parse("12:01").expect("after noon");
        assert!(WorkCalendarZone::new(chrono_tz::Asia::Seoul, noon).is_ok());
        assert!(WorkCalendarZone::new(chrono_tz::Asia::Seoul, after).is_err());
    }

    #[test]
    fn work_date_start_is_inverse_boundary_of_work_date() {
        let zone = seoul("05:00");
        let start = zone.work_date_start(date("2028-02-29"));
        assert_eq!(zone.work_date(start), date("2028-02-29"));
        let just_before = start.checked_sub(Duration::milliseconds(1)).expect("in range");
        assert_eq!(zone.work_date(just_before), date("2028-02-28"));
    }

    #[test]
    fn dst_gap_resolves_to_first_valid_instant() {
        let zone = WorkCalendarZone::new(
            chrono_tz::America::New_York,
            MinuteOfDay::MIDNIGHT,
        )
        .expect("zone");
        // 2026-03-08 02:30 은 존재하지 않는다 (02:00 → 03:00).
        let resolved = zone.instant_at(date("2026-03-08"), MinuteOfDay::parse("02:30").expect("t"));
        assert_eq!(resolved, ts("2026-03-08T07:00:00Z"));
    }

    #[test]
    fn dst_overlap_resolves_to_earlier_instant() {
        let zone = WorkCalendarZone::new(
            chrono_tz::America::New_York,
            MinuteOfDay::MIDNIGHT,
        )
        .expect("zone");
        // 2026-11-01 01:30 은 두 번 존재한다. 먼저 오는 EDT(-04:00)를 택한다.
        let resolved = zone.instant_at(date("2026-11-01"), MinuteOfDay::parse("01:30").expect("t"));
        assert_eq!(resolved, ts("2026-11-01T05:30:00Z"));
    }

    #[test]
    fn parse_timezone_rejects_unknown_names() {
        assert!(parse_timezone("Asia/Seoul").is_ok());
        for bad in ["", "Seoul", "Asia/Atlantis", "+09:00", "../etc/passwd"] {
            assert!(parse_timezone(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn manual_clock_advances() {
        let clock = ManualClock::new(ts("2026-01-01T00:00:00Z"));
        clock.advance(Duration::hours(9));
        assert_eq!(clock.now(), ts("2026-01-01T09:00:00Z"));
        clock.set(ts("2025-01-01T00:00:00Z"));
        assert_eq!(clock.now(), ts("2025-01-01T00:00:00Z"));
    }

    proptest! {
        /// 근무일의 시작 시각부터 다음 근무일 시작 직전까지는 모두 같은 근무일이다.
        #[test]
        fn every_instant_maps_into_its_work_date_window(
            millis in 0i64..4_102_444_800_000, // 1970..2100
            cutoff in 0u16..=720,
        ) {
            let zone = WorkCalendarZone::new(
                chrono_tz::Asia::Seoul,
                MinuteOfDay::new(cutoff).expect("cutoff"),
            ).expect("zone");
            let at = Timestamp::from_millis(millis).expect("in range");
            let work_date = zone.work_date(at);
            let start = zone.work_date_start(work_date);
            let next = zone.work_date_start(work_date.succ_opt().expect("next day"));
            prop_assert!(start <= at && at < next);
        }

        #[test]
        fn timestamp_display_is_parseable(millis in -62_135_596_800_000i64..253_402_300_799_000) {
            let at = Timestamp::from_millis(millis).expect("in range");
            let parsed = DateTime::parse_from_rfc3339(&at.to_string()).expect("rfc3339");
            prop_assert_eq!(Timestamp::from_utc(parsed.with_timezone(&Utc)), at);
        }
    }
}
