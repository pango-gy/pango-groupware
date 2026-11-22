//! 출퇴근: 근무 형태, 근무 정책, 출근·퇴근 전이, 하루 근태 판정, 월 요약.
//!
//! IO도 시계 호출도 하지 않는다. 기록·현재 시각·휴가는 모두 인자로 받는다.
//!
//! 시각 계산 규칙:
//! - 근태 분(지각·조퇴·연장·근무)은 **모든 시각을 분 단위로 내림한 뒤** 뺀다.
//!   09:00:59 출근은 09:00 출근, 17:59:30 퇴근은 17:59 퇴근으로 본다.
//!   한 규칙만 쓰므로 출근·퇴근 어느 쪽에도 초 단위 이득·손해가 쌓이지 않는다.
//! - 근무 시간은 벽시계가 아니라 실제 경과 시간이다(서머타임 전환일에도 그렇다).

use chrono::{Days, Duration, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::error::{ConflictReason, DomainError, DomainResult, ValidationReason};
use crate::leave::DayLeave;
use crate::time::{MinuteOfDay, Timestamp, WorkCalendarZone};

/// 한 번의 근무로 인정하는 최대 길이. 이보다 오래 열린 기록은 퇴근으로 닫을 수 없고
/// `missing_clock_out`이 된다. 야간 근무가 근무일 전환 시각을 넘어도 이 안이면 닫힌다.
pub const MAX_SHIFT: Duration = Duration::hours(24);
/// 수정 값이 현재 시각보다 이만큼까지는 미래여도 받는다(기기 간 시계 오차).
pub const EDIT_FUTURE_SKEW: Duration = Duration::minutes(5);
pub const MAX_LATE_GRACE_MINUTES: u16 = 120;
pub const MAX_BREAK_TIERS: usize = 6;

/// 출퇴근이 다루는 시각 범위(1900-01-01 ~ 9999-12-31 UTC).
///
/// chrono는 표현 한계 근처에서 시간대 변환 시 패닉할 수 있다. 저장 전에 이 범위로
/// 거부하고, 판정 함수는 범위 밖 값을 경계로 끌어당겨 패닉 없이 처리한다.
const MIN_SUPPORTED_MILLIS: i64 = -2_208_988_800_000;
const MAX_SUPPORTED_MILLIS: i64 = 253_402_300_799_999;
const MIN_SUPPORTED_YEAR: i32 = 1900;
const MAX_SUPPORTED_YEAR: i32 = 9999;
const MILLIS_PER_MINUTE: i64 = 60_000;

crate::string_enum! {
    /// 근무 형태.
    pub enum WorkType("work_type") {
        /// 사무실 출근.
        Office => "office",
        /// 재택.
        Remote => "remote",
        /// 외근·출장.
        Field => "field",
    }
}

/// 근무 구간이 `min_span_minutes` 이상이면 `break_minutes`를 휴게로 뺀다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BreakTier {
    pub min_span_minutes: u16,
    pub break_minutes: u16,
}

/// 검증된 근무 정책. 필드를 직접 만들 수 없게 막아 잘못된 정책이 판정에 들어오지 않게 한다.
///
/// 시각은 근무일의 **달력 날짜** 기준 벽시계다. 근무일 전환 시각보다 이른 시작은
/// 지원하지 않는다(교대 근무는 MVP 범위 밖).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkPolicy {
    work_start: MinuteOfDay,
    work_end: MinuteOfDay,
    late_grace_minutes: u16,
    half_day_split: MinuteOfDay,
    quarter_minutes: u16,
    break_tiers: Vec<BreakTier>,
    /// 오전 반반차일 때의 기대 시작(= 시작 + 반반차). 생성 시 한 번 계산해 둔다.
    am_quarter_start: MinuteOfDay,
    /// 오후 반반차일 때의 기대 종료(= 종료 − 반반차).
    pm_quarter_end: MinuteOfDay,
}

impl WorkPolicy {
    pub const DEFAULT_QUARTER_MINUTES: u16 = 120;

    pub fn new(
        work_start: MinuteOfDay,
        work_end: MinuteOfDay,
        late_grace_minutes: u16,
        half_day_split: MinuteOfDay,
        quarter_minutes: u16,
        break_tiers: Vec<BreakTier>,
    ) -> DomainResult<Self> {
        let (start, end) = (work_start.minutes(), work_end.minutes());
        if end <= start {
            return Err(DomainError::validation(
                "work_end",
                ValidationReason::Inconsistent,
            ));
        }
        if late_grace_minutes > MAX_LATE_GRACE_MINUTES {
            return Err(DomainError::validation(
                "late_grace_minutes",
                ValidationReason::OutOfRange,
            ));
        }
        if !(start < half_day_split.minutes() && half_day_split.minutes() < end) {
            return Err(DomainError::validation(
                "half_day_split",
                ValidationReason::OutOfRange,
            ));
        }
        // 반반차를 빼고도 근무 구간이 남아야 한다.
        if quarter_minutes == 0 || quarter_minutes >= end - start {
            return Err(DomainError::validation(
                "quarter_minutes",
                ValidationReason::OutOfRange,
            ));
        }
        validate_break_tiers(&break_tiers)?;
        Ok(Self {
            work_start,
            work_end,
            late_grace_minutes,
            half_day_split,
            quarter_minutes,
            break_tiers,
            am_quarter_start: MinuteOfDay::new(start + quarter_minutes)?,
            pm_quarter_end: MinuteOfDay::new(end - quarter_minutes)?,
        })
    }

    /// 09:00–18:00, 유예 0분, 반차 기준 14:00, 반반차 2시간, 휴게 4시간 30분·8시간 60분.
    pub fn default_korean() -> Self {
        // 상수 값이라 검증을 다시 돌리지 않고 바로 만든다. `new`를 통과하는지는 테스트가 확인한다.
        let hm = |hour, minute| MinuteOfDay::from_hm(hour, minute).unwrap_or(MinuteOfDay::MIDNIGHT);
        Self {
            work_start: hm(9, 0),
            work_end: hm(18, 0),
            late_grace_minutes: 0,
            half_day_split: hm(14, 0),
            quarter_minutes: Self::DEFAULT_QUARTER_MINUTES,
            break_tiers: vec![
                BreakTier {
                    min_span_minutes: 240,
                    break_minutes: 30,
                },
                BreakTier {
                    min_span_minutes: 480,
                    break_minutes: 60,
                },
            ],
            am_quarter_start: hm(11, 0),
            pm_quarter_end: hm(16, 0),
        }
    }

    pub const fn work_start(&self) -> MinuteOfDay {
        self.work_start
    }

    pub const fn work_end(&self) -> MinuteOfDay {
        self.work_end
    }

    pub const fn late_grace_minutes(&self) -> u16 {
        self.late_grace_minutes
    }

    pub const fn half_day_split(&self) -> MinuteOfDay {
        self.half_day_split
    }

    pub const fn quarter_minutes(&self) -> u16 {
        self.quarter_minutes
    }

    pub fn break_tiers(&self) -> &[BreakTier] {
        &self.break_tiers
    }

    /// 근무 구간(분)에 적용할 휴게(분). 기준을 넘는 가장 높은 단계 하나만 적용한다.
    pub fn break_minutes_for(&self, span_minutes: u32) -> u32 {
        self.break_tiers
            .iter()
            .rev()
            .find(|tier| span_minutes >= u32::from(tier.min_span_minutes))
            .map_or(0, |tier| u32::from(tier.break_minutes))
    }

    /// 그날 휴가에 맞춘 기대 시작·종료. 하루를 다 쉬면 기대 시각이 없다.
    ///
    /// 앞쪽 칸을 쉬면 시작이, 뒤쪽 칸을 쉬면 종료가 밀린다(반반차 한 칸, 반차 두 칸).
    fn expected_window(&self, leave: Option<DayLeave>) -> Option<(MinuteOfDay, MinuteOfDay)> {
        let Some(leave) = leave else {
            return Some((self.work_start, self.work_end));
        };
        if leave.is_full_day() {
            return None;
        }
        let start = match leave.leading_quarters() {
            0 => self.work_start,
            1 => self.am_quarter_start,
            _ => self.half_day_split,
        };
        let end = match leave.trailing_quarters() {
            0 => self.work_end,
            1 => self.pm_quarter_end,
            _ => self.half_day_split,
        };
        Some((start, end))
    }
}

fn validate_break_tiers(tiers: &[BreakTier]) -> DomainResult<()> {
    if tiers.len() > MAX_BREAK_TIERS {
        return Err(DomainError::validation(
            "break_tiers",
            ValidationReason::TooLong {
                max: MAX_BREAK_TIERS,
            },
        ));
    }
    let inconsistent = DomainError::validation("break_tiers", ValidationReason::Inconsistent);
    // 근무 구간은 MAX_SHIFT를 넘지 않으므로 그보다 긴 기준은 설정 실수다.
    let max_span = u16::try_from(MAX_SHIFT.num_minutes()).unwrap_or(u16::MAX);
    let mut previous: Option<u16> = None;
    for tier in tiers {
        if tier.min_span_minutes > max_span {
            return Err(DomainError::validation(
                "break_tiers",
                ValidationReason::OutOfRange,
            ));
        }
        // 휴게가 기준 이상이면 근무 시간이 0 이하가 된다.
        if tier.break_minutes >= tier.min_span_minutes {
            return Err(inconsistent);
        }
        if previous.is_some_and(|prev| tier.min_span_minutes <= prev) {
            return Err(inconsistent);
        }
        previous = Some(tier.min_span_minutes);
    }
    Ok(())
}

/// 저장된 출퇴근 기록의 판정용 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordState {
    pub work_date: NaiveDate,
    pub clock_in_at: Timestamp,
    pub clock_out_at: Option<Timestamp>,
}

impl RecordState {
    pub const fn is_open(&self) -> bool {
        self.clock_out_at.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockInDecision {
    /// 새 기록을 만든다.
    Create {
        work_date: NaiveDate,
        clock_in_at: Timestamp,
    },
    /// 오늘 근무일의 열린 기록이 이미 있다. 아무것도 바꾸지 않고 그대로 돌려준다.
    AlreadyClockedIn(RecordState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockOutDecision {
    /// 이 기록을 닫는다. `work_date`는 출근 기록의 근무일이다(야간 근무면 전날).
    Close {
        work_date: NaiveDate,
        clock_in_at: Timestamp,
        clock_out_at: Timestamp,
    },
    /// 이미 닫힌 기록. 아무것도 바꾸지 않는다.
    AlreadyClockedOut(RecordState),
}

fn ensure_supported(field: &'static str, at: Timestamp) -> DomainResult<()> {
    if (MIN_SUPPORTED_MILLIS..=MAX_SUPPORTED_MILLIS).contains(&at.as_millis()) {
        Ok(())
    } else {
        Err(DomainError::validation(field, ValidationReason::OutOfRange))
    }
}

/// 출근 판정. `latest`는 구성원의 가장 최근 기록(출근 시각 기준)이다.
///
/// - 오늘 근무일 기록이 열려 있으면 그대로 돌려준다(멱등).
/// - 오늘 근무일 기록이 이미 닫혔으면 충돌. 재출근은 정정 요청으로 처리한다.
/// - 이전 근무일 기록은 열려 있어도 막지 않는다(그 기록은 `missing_clock_out` 대상).
/// - 가장 최근 기록이 오늘보다 **뒤의** 근무일이면 서버 시계가 거꾸로 간 것이므로 거부한다.
pub fn decide_clock_in(
    latest: Option<&RecordState>,
    now: Timestamp,
    zone: &WorkCalendarZone,
) -> DomainResult<ClockInDecision> {
    ensure_supported("now", now)?;
    let today = zone.work_date(now);
    match latest {
        Some(record) if record.work_date > today => Err(DomainError::validation(
            "now",
            ValidationReason::Inconsistent,
        )),
        Some(record) if record.work_date == today => {
            if record.is_open() {
                Ok(ClockInDecision::AlreadyClockedIn(*record))
            } else {
                Err(ConflictReason::AlreadyClockedOut.into())
            }
        }
        _ => Ok(ClockInDecision::Create {
            work_date: today,
            clock_in_at: now,
        }),
    }
}

/// 퇴근 판정. `latest`는 구성원의 가장 최근 기록(출근 시각 기준)이다.
///
/// 가장 최근 기록만 본다. 그보다 오래된 열린 기록은 이미 뒤에 다른 기록이 생겼으므로
/// 퇴근으로 닫지 않는다(정정 요청 대상).
/// - 열린 기록이고 출근 후 `MAX_SHIFT` 이내면 닫는다. 근무일이 바뀌었어도 닫는다(야간 근무).
/// - 퇴근 시각이 출근 시각과 같거나 이르면 충돌(시계 오차·순서 역전). 길이 0 근무는 만들지 않는다.
/// - 닫힌 기록이 오늘 근무일이거나 출근 후 `MAX_SHIFT` 이내면 그대로 돌려준다(멱등).
/// - 그 밖에는 `not_clocked_in`.
pub fn decide_clock_out(
    latest: Option<&RecordState>,
    now: Timestamp,
    zone: &WorkCalendarZone,
) -> DomainResult<ClockOutDecision> {
    ensure_supported("now", now)?;
    let Some(record) = latest else {
        return Err(ConflictReason::NotClockedIn.into());
    };
    let within_shift = now.since(record.clock_in_at) <= MAX_SHIFT;
    if record.is_open() {
        if now <= record.clock_in_at {
            return Err(ConflictReason::ClockOutBeforeClockIn.into());
        }
        if within_shift {
            return Ok(ClockOutDecision::Close {
                work_date: record.work_date,
                clock_in_at: record.clock_in_at,
                clock_out_at: now,
            });
        }
        return Err(ConflictReason::NotClockedIn.into());
    }
    if within_shift || record.work_date == zone.work_date(now) {
        return Ok(ClockOutDecision::AlreadyClockedOut(*record));
    }
    Err(ConflictReason::NotClockedIn.into())
}

/// 관리자 수정·정정 승인 값 검증.
///
/// 퇴근 > 출근, 길이 ≤ `MAX_SHIFT`, 어느 값도 `now + EDIT_FUTURE_SKEW`보다 미래가 아니어야 하고,
/// 출근 시각이 `work_date`에 속해야 한다.
pub fn validate_edit(
    clock_in_at: Timestamp,
    clock_out_at: Option<Timestamp>,
    work_date: NaiveDate,
    now: Timestamp,
    zone: &WorkCalendarZone,
) -> DomainResult<()> {
    ensure_supported("now", now)?;
    ensure_supported("clock_in_at", clock_in_at)?;
    if let Some(out) = clock_out_at {
        ensure_supported("clock_out_at", out)?;
    }
    let in_future = |at: Timestamp| at.since(now) > EDIT_FUTURE_SKEW;
    if in_future(clock_in_at) {
        return Err(DomainError::validation(
            "clock_in_at",
            ValidationReason::OutOfRange,
        ));
    }
    if zone.work_date(clock_in_at) != work_date {
        return Err(DomainError::validation(
            "work_date",
            ValidationReason::Inconsistent,
        ));
    }
    if let Some(out) = clock_out_at {
        if out <= clock_in_at {
            return Err(DomainError::validation(
                "clock_out_at",
                ValidationReason::Inconsistent,
            ));
        }
        if out.since(clock_in_at) > MAX_SHIFT || in_future(out) {
            return Err(DomainError::validation(
                "clock_out_at",
                ValidationReason::OutOfRange,
            ));
        }
    }
    Ok(())
}

crate::string_enum! {
    /// 하루 근태 상태.
    pub enum DayStatus("status") {
        /// 기록 없는 근무일로 아직 끝나지 않음(미래, 또는 오늘 출근 전·지각 중).
        Scheduled => "scheduled",
        /// 출근했고 아직 퇴근하지 않음.
        Working => "working",
        Normal => "normal",
        Late => "late",
        EarlyLeave => "early_leave",
        LateAndEarlyLeave => "late_and_early_leave",
        /// 출근 후 `MAX_SHIFT`가 지나도록 퇴근하지 않음. 더는 퇴근으로 닫을 수 없다.
        MissingClockOut => "missing_clock_out",
        /// 끝난 근무일에 기록도 종일 휴가도 없음.
        Absent => "absent",
        /// 종일 휴가이고 기록 없음.
        OnLeave => "on_leave",
        /// 근무일이 아니고 기록 없음.
        DayOff => "day_off",
    }
}

/// 하루 판정 입력. `record`는 `work_date`의 기록이어야 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayInput<'a> {
    pub work_date: NaiveDate,
    pub is_working_day: bool,
    pub record: Option<&'a RecordState>,
    pub leave: Option<DayLeave>,
    pub now: Timestamp,
}

/// 구성원의 근태를 따지는 기간: `max(입사일, 계정 생성일)`부터 퇴사일(있으면, 포함)까지.
///
/// 이 기간 밖의 날은 그 구성원에게 **근무일이 아니다**. 계정이 생기기 전이나 입사 전·퇴사 후에는
/// 출근을 찍을 수 없었으므로, 기록이 없다고 결근으로 세면 안 된다. 그런 날의 기록(관리자가
/// 만들었거나 정정이 승인된 것)은 비근무일 근무로 판정한다([`Self::working_day`]).
///
/// 계정 생성일은 생성 시각의 회사 시간대 **달력 날짜**다(근무일 전환 시각을 적용하지 않는다).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackedPeriod {
    from: NaiveDate,
    until: Option<NaiveDate>,
}

impl TrackedPeriod {
    pub fn new(
        hired_on: NaiveDate,
        account_created_on: NaiveDate,
        retired_on: Option<NaiveDate>,
    ) -> Self {
        Self {
            from: hired_on.max(account_created_on),
            until: retired_on,
        }
    }

    /// 첫 추적일. 퇴사일이 이보다 앞서면 기간은 비어 있다.
    pub fn from(&self) -> NaiveDate {
        self.from
    }

    pub fn until(&self) -> Option<NaiveDate> {
        self.until
    }

    pub fn contains(&self, date: NaiveDate) -> bool {
        date >= self.from && self.until.is_none_or(|until| date <= until)
    }

    /// 회사 달력의 근무일 여부를 이 구성원 기준으로 바꾼다. 기간 밖이면 근무일이 아니다.
    /// [`DayInput::is_working_day`]에는 이 값을 넣는다.
    pub fn working_day(&self, date: NaiveDate, company_working_day: bool) -> bool {
        company_working_day && self.contains(date)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DayEvaluation {
    pub status: DayStatus,
    /// 휴게를 뺀 근무 분. 퇴근 전이면 0.
    pub worked_minutes: u32,
    pub break_minutes: u32,
    /// 기대 시작부터 센 지각 분. 유예 안이면 0.
    pub late_minutes: u32,
    pub early_leave_minutes: u32,
    /// 기대 종료 이후 근무 분. 휴일·종일 휴가 중 근무는 연장과 구분해 `worked_minutes`로만 센다.
    pub overtime_minutes: u32,
}

impl DayEvaluation {
    const fn empty(status: DayStatus) -> Self {
        Self {
            status,
            worked_minutes: 0,
            break_minutes: 0,
            late_minutes: 0,
            early_leave_minutes: 0,
            overtime_minutes: 0,
        }
    }
}

/// 분 단위로 내린 유닉스 분.
fn minute_index(at: Timestamp) -> i64 {
    at.as_millis().div_euclid(MILLIS_PER_MINUTE)
}

/// `later - earlier`(분). 음수면 0.
fn minutes_between(earlier: Timestamp, later: Timestamp) -> u32 {
    let diff = minute_index(later)
        .saturating_sub(minute_index(earlier))
        .max(0);
    u32::try_from(diff).unwrap_or(u32::MAX)
}

fn clamp_supported(at: Timestamp) -> Timestamp {
    Timestamp::from_millis(
        at.as_millis()
            .clamp(MIN_SUPPORTED_MILLIS, MAX_SUPPORTED_MILLIS),
    )
    .unwrap_or(at)
}

fn clamp_supported_date(date: NaiveDate) -> NaiveDate {
    match (
        NaiveDate::from_ymd_opt(MIN_SUPPORTED_YEAR, 1, 1),
        NaiveDate::from_ymd_opt(MAX_SUPPORTED_YEAR, 12, 30),
    ) {
        (Some(lo), Some(hi)) => date.clamp(lo, hi),
        _ => date,
    }
}

/// 근무일이 끝났는지(다음 근무일이 시작됐는지).
fn work_date_ended(zone: &WorkCalendarZone, work_date: NaiveDate, now: Timestamp) -> bool {
    work_date
        .succ_opt()
        .is_some_and(|next| now >= zone.work_date_start(next))
}

/// 하루 근태 판정.
///
/// - 기록 없음: 비근무일 → `DayOff`, 종일 휴가 → `OnLeave`, 근무일이 끝남 → `Absent`, 그 외 `Scheduled`.
/// - 열린 기록: 출근 후 `MAX_SHIFT` 초과 → `MissingClockOut`, 아니면 `Working`.
///   야간 근무는 다음 근무일이 시작돼도 `MAX_SHIFT` 안이면 아직 `Working`이다(퇴근 규칙과 같은 기준).
/// - 닫힌 기록: 근무 분을 계산한다. 비근무일·종일 휴가 날의 근무는 지각·조퇴 없이 `Normal`.
///
/// 범위 밖 시각·날짜는 저장 전에 거부되므로 여기서는 패닉하지 않도록 경계로 끌어당기기만 한다.
pub fn evaluate_day(
    input: &DayInput,
    policy: &WorkPolicy,
    zone: &WorkCalendarZone,
) -> DayEvaluation {
    let work_date = clamp_supported_date(input.work_date);
    let now = clamp_supported(input.now);
    let Some(record) = input.record else {
        let status = if !input.is_working_day {
            DayStatus::DayOff
        } else if input.leave.is_some_and(DayLeave::is_full_day) {
            DayStatus::OnLeave
        } else if work_date_ended(zone, work_date, now) {
            DayStatus::Absent
        } else {
            DayStatus::Scheduled
        };
        return DayEvaluation::empty(status);
    };

    let clock_in = clamp_supported(record.clock_in_at);
    let expected = if input.is_working_day {
        policy.expected_window(input.leave)
    } else {
        None
    };
    let expected = expected.map(|(start, end)| {
        (
            zone.instant_at(work_date, start),
            zone.instant_at(work_date, end),
        )
    });
    let late_minutes = expected.map_or(0, |(start, _)| {
        let late = minutes_between(start, clock_in);
        if late > u32::from(policy.late_grace_minutes) {
            late
        } else {
            0
        }
    });

    let Some(clock_out) = record.clock_out_at.map(clamp_supported) else {
        let status = if now.since(clock_in) > MAX_SHIFT {
            DayStatus::MissingClockOut
        } else {
            DayStatus::Working
        };
        return DayEvaluation {
            late_minutes,
            ..DayEvaluation::empty(status)
        };
    };

    let span = minutes_between(clock_in, clock_out);
    let break_minutes = policy.break_minutes_for(span);
    let worked_minutes = span.saturating_sub(break_minutes);
    let (early_leave_minutes, overtime_minutes) = expected.map_or((0, 0), |(_, end)| {
        (
            minutes_between(clock_out, end),
            minutes_between(end, clock_out),
        )
    });
    let status = match (late_minutes > 0, early_leave_minutes > 0) {
        (false, false) => DayStatus::Normal,
        (true, false) => DayStatus::Late,
        (false, true) => DayStatus::EarlyLeave,
        (true, true) => DayStatus::LateAndEarlyLeave,
    };
    DayEvaluation {
        status,
        worked_minutes,
        break_minutes,
        late_minutes,
        early_leave_minutes,
        overtime_minutes,
    }
}

/// 월 근태 요약. 상태별 일수는 서로 겹치지 않는다(지각+조퇴는 `late_and_early_leave_days`에만).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MonthlySummary {
    pub scheduled_days: u32,
    pub working_days: u32,
    pub normal_days: u32,
    pub late_days: u32,
    pub early_leave_days: u32,
    pub late_and_early_leave_days: u32,
    pub missing_clock_out_days: u32,
    pub absent_days: u32,
    pub on_leave_days: u32,
    pub day_off_days: u32,
    pub total_worked_minutes: u64,
    pub total_overtime_minutes: u64,
    pub total_late_minutes: u64,
}

impl MonthlySummary {
    pub fn from_days(days: &[DayEvaluation]) -> Self {
        days.iter().fold(Self::default(), |mut acc, day| {
            let counter = match day.status {
                DayStatus::Scheduled => &mut acc.scheduled_days,
                DayStatus::Working => &mut acc.working_days,
                DayStatus::Normal => &mut acc.normal_days,
                DayStatus::Late => &mut acc.late_days,
                DayStatus::EarlyLeave => &mut acc.early_leave_days,
                DayStatus::LateAndEarlyLeave => &mut acc.late_and_early_leave_days,
                DayStatus::MissingClockOut => &mut acc.missing_clock_out_days,
                DayStatus::Absent => &mut acc.absent_days,
                DayStatus::OnLeave => &mut acc.on_leave_days,
                DayStatus::DayOff => &mut acc.day_off_days,
            };
            *counter = counter.saturating_add(1);
            acc.total_worked_minutes = acc
                .total_worked_minutes
                .saturating_add(u64::from(day.worked_minutes));
            acc.total_overtime_minutes = acc
                .total_overtime_minutes
                .saturating_add(u64::from(day.overtime_minutes));
            acc.total_late_minutes = acc
                .total_late_minutes
                .saturating_add(u64::from(day.late_minutes));
            acc
        })
    }
}

crate::string_enum! {
    /// 출퇴근 기록 변경 이력(`attendance_events`)의 종류.
    pub enum AttendanceEventKind("kind") {
        /// 본인 출근.
        ClockIn => "clock_in",
        /// 본인 퇴근.
        ClockOut => "clock_out",
        /// 관리자가 기록을 새로 만듦.
        AdminCreate => "admin_create",
        /// 관리자가 기록을 고침.
        AdminEdit => "admin_edit",
        /// 정정 요청 승인으로 고쳐짐.
        Correction => "correction",
    }
}

// ───────────── 정정 요청 ─────────────

/// 정정을 요청할 수 있는 가장 오래된 근무일까지의 일수. 이미 마감된 달의 근태를 구성원이
/// 뒤늦게 바꾸지 못하게 한다(더 오래된 기록은 관리자가 직접 고친다).
pub const CORRECTION_WINDOW_DAYS: u64 = 31;

crate::string_enum! {
    /// 근태 정정 요청의 상태. 요청은 결재 문서와 한 몸이라 문서가 종료될 때만 바뀐다.
    pub enum CorrectionStatus("status") {
        /// 결재 중.
        Pending => "pending",
        /// 최종 승인되어 기록에 반영됨.
        Approved => "approved",
        Rejected => "rejected",
        /// 요청자가 회수함.
        Canceled => "canceled",
    }
}

/// 정정을 요청할 수 있는 근무일인지. `current_work_date`(지금이 속한 근무일)보다 미래이거나
/// [`CORRECTION_WINDOW_DAYS`]보다 오래됐으면 `Validation { work_date, OutOfRange }`.
pub fn validate_correction_date(
    work_date: NaiveDate,
    current_work_date: NaiveDate,
) -> DomainResult<()> {
    let earliest = current_work_date.checked_sub_days(Days::new(CORRECTION_WINDOW_DAYS));
    let too_old = earliest.is_none_or(|earliest| work_date < earliest);
    if too_old || work_date > current_work_date {
        return Err(DomainError::validation(
            "work_date",
            ValidationReason::OutOfRange,
        ));
    }
    Ok(())
}

/// 문서 종료 결과를 정정 요청에 반영할지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorrectionDecision {
    /// 대기 중인 요청을 `target` 상태로 끝낸다.
    Close,
    /// 이미 그 상태다. 아무것도 하지 않는다.
    AlreadyClosed,
}

/// 정정 요청을 `target`으로 끝낼 수 있는지 판정한다.
///
/// 같은 결과가 다시 오면 `AlreadyClosed`(자연 멱등), 다른 결과로 이미 끝났으면
/// `DocumentAlreadyClosed`다. 끝난 요청은 다시 열리지 않는다.
pub fn decide_correction_close(
    current: CorrectionStatus,
    target: CorrectionStatus,
) -> DomainResult<CorrectionDecision> {
    if current == target {
        Ok(CorrectionDecision::AlreadyClosed)
    } else if current == CorrectionStatus::Pending {
        Ok(CorrectionDecision::Close)
    } else {
        Err(ConflictReason::DocumentAlreadyClosed.into())
    }
}

/// 퇴근 누락 알림을 보내는 시각: 업무 종료 시각에서 이만큼 지난 뒤.
pub const CLOCK_OUT_REMINDER_DELAY: Duration = Duration::hours(1);

/// 열린 기록에 퇴근 누락 알림을 보낼 시각.
///
/// 근무일의 업무 종료 시각(근무일 전환 시각보다 이르면 다음 날 그 시각, 즉 자정을 넘긴 종료)에
/// [`CLOCK_OUT_REMINDER_DELAY`]를 더한다. 업무 종료 뒤에 출근했으면 출근 시각을 기준으로 한다
/// (늦게 출근한 사람에게 출근하자마자 알림이 가지 않도록).
pub fn clock_out_reminder_at(
    zone: &WorkCalendarZone,
    work_date: NaiveDate,
    work_end: MinuteOfDay,
    clock_in_at: Timestamp,
) -> Timestamp {
    let end_date = if work_end < zone.day_cutoff() {
        work_date
            .checked_add_days(Days::new(1))
            .unwrap_or(work_date)
    } else {
        work_date
    };
    let end = zone.instant_at(end_date, work_end).max(clock_in_at);
    end.checked_add(CLOCK_OUT_REMINDER_DELAY).unwrap_or(end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leave::LeaveUnit;
    use chrono::{DateTime, Utc};
    use proptest::prelude::*;

    #[test]
    fn clock_out_reminder_is_an_hour_after_the_scheduled_end() {
        let zone = seoul();
        // 18:00 종료 → 19:00(KST) 알림.
        assert_eq!(
            clock_out_reminder_at(
                &zone,
                date("2026-03-02"),
                hm("18:00"),
                ts("2026-03-02T09:00:00+09:00")
            ),
            ts("2026-03-02T19:00:00+09:00")
        );
        // 늦게 출근했으면 출근 시각 기준.
        assert_eq!(
            clock_out_reminder_at(
                &zone,
                date("2026-03-02"),
                hm("18:00"),
                ts("2026-03-02T20:30:00+09:00")
            ),
            ts("2026-03-02T21:30:00+09:00")
        );
        // 종료가 전환 시각(05:00)보다 이르면 다음 날 새벽 종료다.
        assert_eq!(
            clock_out_reminder_at(
                &zone,
                date("2026-03-02"),
                hm("02:00"),
                ts("2026-03-02T18:00:00+09:00")
            ),
            ts("2026-03-03T03:00:00+09:00")
        );
    }

    fn ts(rfc3339: &str) -> Timestamp {
        Timestamp::from_utc(
            DateTime::parse_from_rfc3339(rfc3339)
                .expect("rfc3339")
                .with_timezone(&Utc),
        )
    }

    fn date(value: &str) -> NaiveDate {
        value.parse().expect("date")
    }

    fn hm(value: &str) -> MinuteOfDay {
        MinuteOfDay::parse(value).expect("time")
    }

    fn ms(at: Timestamp, delta: i64) -> Timestamp {
        Timestamp::from_millis(at.as_millis() + delta).expect("in range")
    }

    fn seoul() -> WorkCalendarZone {
        WorkCalendarZone::new(chrono_tz::Asia::Seoul, hm("05:00")).expect("zone")
    }

    fn open(work_date: &str, clock_in: &str) -> RecordState {
        RecordState {
            work_date: date(work_date),
            clock_in_at: ts(clock_in),
            clock_out_at: None,
        }
    }

    fn closed(work_date: &str, clock_in: &str, clock_out: &str) -> RecordState {
        RecordState {
            work_date: date(work_date),
            clock_in_at: ts(clock_in),
            clock_out_at: Some(ts(clock_out)),
        }
    }

    fn tiers(pairs: &[(u16, u16)]) -> Vec<BreakTier> {
        pairs
            .iter()
            .map(|&(min_span_minutes, break_minutes)| BreakTier {
                min_span_minutes,
                break_minutes,
            })
            .collect()
    }

    fn policy_with_grace(grace: u16) -> WorkPolicy {
        WorkPolicy::new(
            hm("09:00"),
            hm("18:00"),
            grace,
            hm("14:00"),
            120,
            tiers(&[(240, 30), (480, 60)]),
        )
        .expect("policy")
    }

    /// 2026-03-02(월) Seoul 근무일 평가. 시각은 KST 오프셋을 붙인 RFC3339.
    fn eval_on(
        record: Option<&RecordState>,
        leave: Option<LeaveUnit>,
        is_working_day: bool,
        now: &str,
        policy: &WorkPolicy,
    ) -> DayEvaluation {
        let input = DayInput {
            work_date: date("2026-03-02"),
            is_working_day,
            record,
            leave: leave.map(DayLeave::from),
            now: ts(now),
        };
        evaluate_day(&input, policy, &seoul())
    }

    fn eval_closed(clock_in: &str, clock_out: &str, leave: Option<LeaveUnit>) -> DayEvaluation {
        let record = closed("2026-03-02", clock_in, clock_out);
        eval_on(
            Some(&record),
            leave,
            true,
            "2026-03-04T00:00:00+09:00",
            &policy_with_grace(0),
        )
    }

    const NOW_LATER: &str = "2026-03-04T00:00:00+09:00";

    // ---- WorkType ----

    #[test]
    fn enum_wire_strings_are_fixed() {
        let work_types: Vec<&str> = WorkType::ALL.iter().map(|w| w.as_str()).collect();
        assert_eq!(work_types, ["office", "remote", "field"]);
        let statuses: Vec<&str> = DayStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            statuses,
            [
                "scheduled",
                "working",
                "normal",
                "late",
                "early_leave",
                "late_and_early_leave",
                "missing_clock_out",
                "absent",
                "on_leave",
                "day_off",
            ]
        );
        for &status in DayStatus::ALL {
            let json = serde_json::to_string(&status).expect("json");
            assert_eq!(json, format!("\"{}\"", status.as_str()));
            assert_eq!(
                serde_json::from_str::<DayStatus>(&json).expect("parse"),
                status
            );
        }
        assert!(serde_json::from_str::<DayStatus>("\"DayOff\"").is_err());
        let events: Vec<&str> = AttendanceEventKind::ALL
            .iter()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(
            events,
            [
                "clock_in",
                "clock_out",
                "admin_create",
                "admin_edit",
                "correction"
            ]
        );
        let corrections: Vec<&str> = CorrectionStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(corrections, ["pending", "approved", "rejected", "canceled"]);
    }

    // ---- 정정 요청 ----

    #[test]
    fn correction_date_window_boundaries() {
        let today = date("2026-03-02");
        assert_eq!(validate_correction_date(today, today), Ok(()));
        // 31일 전(2026-01-30)까지 받고 32일 전은 거부한다.
        assert_eq!(validate_correction_date(date("2026-01-30"), today), Ok(()));
        let out_of_range = Err(DomainError::validation(
            "work_date",
            ValidationReason::OutOfRange,
        ));
        assert_eq!(
            validate_correction_date(date("2026-01-29"), today),
            out_of_range
        );
        assert_eq!(
            validate_correction_date(date("2026-03-03"), today),
            out_of_range
        );
        assert_eq!(
            validate_correction_date(NaiveDate::MAX, NaiveDate::MIN),
            out_of_range
        );
        assert_eq!(
            validate_correction_date(NaiveDate::MIN, NaiveDate::MIN),
            out_of_range,
            "기준일을 계산할 수 없으면 거부한다"
        );
    }

    #[test]
    fn correction_close_is_idempotent_and_never_reopens() {
        use CorrectionStatus::*;
        for &target in &[Approved, Rejected, Canceled] {
            assert_eq!(
                decide_correction_close(Pending, target),
                Ok(CorrectionDecision::Close)
            );
            assert_eq!(
                decide_correction_close(target, target),
                Ok(CorrectionDecision::AlreadyClosed)
            );
        }
        for &current in &[Approved, Rejected, Canceled] {
            for &target in CorrectionStatus::ALL {
                if current != target {
                    assert_eq!(
                        decide_correction_close(current, target),
                        Err(ConflictReason::DocumentAlreadyClosed.into()),
                        "{current} → {target}"
                    );
                }
            }
        }
    }

    #[test]
    fn work_type_round_trips_and_rejects_unknown() {
        for &work_type in WorkType::ALL {
            assert_eq!(WorkType::parse(work_type.as_str()), Ok(work_type));
            let json = serde_json::to_string(&work_type).expect("json");
            assert_eq!(json, format!("\"{}\"", work_type.as_str()));
        }
        assert_eq!(WorkType::Remote.as_str(), "remote");
        for bad in ["", "Office", "home", " office", "office\0"] {
            assert!(WorkType::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    // ---- WorkPolicy ----

    #[test]
    fn default_korean_policy_values() {
        let policy = WorkPolicy::default_korean();
        assert_eq!(policy.work_start(), hm("09:00"));
        assert_eq!(policy.work_end(), hm("18:00"));
        assert_eq!(policy.late_grace_minutes(), 0);
        assert_eq!(policy.half_day_split(), hm("14:00"));
        assert_eq!(policy.quarter_minutes(), 120);
        assert_eq!(
            policy.break_tiers(),
            tiers(&[(240, 30), (480, 60)]).as_slice()
        );
        // 직접 만든 기본값이 검증을 거친 값과 같다(파생 필드 포함).
        assert_eq!(
            WorkPolicy::new(
                hm("09:00"),
                hm("18:00"),
                0,
                hm("14:00"),
                120,
                tiers(&[(240, 30), (480, 60)])
            ),
            Ok(policy)
        );
    }

    #[test]
    fn policy_rejects_end_not_after_start() {
        let make = |start, end| WorkPolicy::new(hm(start), hm(end), 0, hm("12:00"), 60, Vec::new());
        assert!(make("09:00", "18:00").is_ok());
        for (start, end) in [("09:00", "09:00"), ("18:00", "09:00")] {
            assert_eq!(
                make(start, end),
                Err(DomainError::validation(
                    "work_end",
                    ValidationReason::Inconsistent
                )),
                "{start}-{end}"
            );
        }
    }

    #[test]
    fn policy_rejects_split_outside_range() {
        let make = |split| WorkPolicy::new(hm("09:00"), hm("18:00"), 0, hm(split), 120, Vec::new());
        assert!(make("09:01").is_ok());
        assert!(make("17:59").is_ok());
        for split in ["09:00", "18:00", "08:00", "23:00", "00:00"] {
            assert_eq!(
                make(split),
                Err(DomainError::validation(
                    "half_day_split",
                    ValidationReason::OutOfRange
                )),
                "{split}"
            );
        }
    }

    #[test]
    fn policy_grace_upper_bound_is_120() {
        let make = |grace| {
            WorkPolicy::new(
                hm("09:00"),
                hm("18:00"),
                grace,
                hm("14:00"),
                120,
                Vec::new(),
            )
        };
        assert!(make(120).is_ok());
        assert_eq!(
            make(121),
            Err(DomainError::validation(
                "late_grace_minutes",
                ValidationReason::OutOfRange
            ))
        );
        assert!(make(u16::MAX).is_err());
    }

    #[test]
    fn policy_rejects_quarter_outside_work_span() {
        let make = |quarter| {
            WorkPolicy::new(
                hm("09:00"),
                hm("18:00"),
                0,
                hm("14:00"),
                quarter,
                Vec::new(),
            )
        };
        assert!(make(1).is_ok());
        assert!(make(539).is_ok());
        for quarter in [0, 540, u16::MAX] {
            assert_eq!(
                make(quarter),
                Err(DomainError::validation(
                    "quarter_minutes",
                    ValidationReason::OutOfRange
                )),
                "{quarter}"
            );
        }
    }

    #[test]
    fn policy_rejects_malformed_break_tiers() {
        let make = |pairs: &[(u16, u16)]| {
            WorkPolicy::new(hm("09:00"), hm("18:00"), 0, hm("14:00"), 120, tiers(pairs))
        };
        assert!(make(&[]).is_ok());
        assert!(make(&[(1, 0)]).is_ok());
        assert!(make(&[(240, 30), (480, 60)]).is_ok());
        let inconsistent = Err(DomainError::validation(
            "break_tiers",
            ValidationReason::Inconsistent,
        ));
        // 정렬되지 않음, 같은 기준 중복, 휴게가 기준 이상
        assert_eq!(make(&[(480, 60), (240, 30)]), inconsistent);
        assert_eq!(make(&[(240, 30), (240, 60)]), inconsistent);
        assert_eq!(make(&[(240, 240)]), inconsistent);
        assert_eq!(make(&[(0, 0)]), inconsistent);
        // 하루를 넘는 기준은 도달할 수 없다
        assert!(make(&[(1440, 60)]).is_ok());
        assert_eq!(
            make(&[(1441, 60)]),
            Err(DomainError::validation(
                "break_tiers",
                ValidationReason::OutOfRange
            ))
        );
    }

    #[test]
    fn policy_allows_at_most_six_break_tiers() {
        let make = |count: u16| {
            let pairs: Vec<(u16, u16)> = (1..=count).map(|i| (i * 60, i)).collect();
            WorkPolicy::new(hm("09:00"), hm("18:00"), 0, hm("14:00"), 120, tiers(&pairs))
        };
        assert!(make(6).is_ok());
        assert_eq!(
            make(7),
            Err(DomainError::validation(
                "break_tiers",
                ValidationReason::TooLong { max: 6 }
            ))
        );
    }

    #[test]
    fn break_tiers_apply_at_exact_thresholds() {
        let policy = WorkPolicy::default_korean();
        assert_eq!(policy.break_minutes_for(0), 0);
        assert_eq!(policy.break_minutes_for(239), 0);
        assert_eq!(policy.break_minutes_for(240), 30);
        assert_eq!(policy.break_minutes_for(479), 30);
        assert_eq!(policy.break_minutes_for(480), 60);
        assert_eq!(policy.break_minutes_for(u32::MAX), 60);
    }

    // ---- 출근 ----

    #[test]
    fn clock_in_creates_record_for_today() {
        let now = ts("2026-03-02T08:55:00+09:00");
        assert_eq!(
            decide_clock_in(None, now, &seoul()),
            Ok(ClockInDecision::Create {
                work_date: date("2026-03-02"),
                clock_in_at: now
            })
        );
    }

    #[test]
    fn clock_in_at_day_cutoff_boundary() {
        let before = ts("2026-03-02T04:59:59.999+09:00");
        let at = ts("2026-03-02T05:00:00+09:00");
        assert_eq!(
            decide_clock_in(None, before, &seoul()),
            Ok(ClockInDecision::Create {
                work_date: date("2026-03-01"),
                clock_in_at: before
            })
        );
        assert_eq!(
            decide_clock_in(None, at, &seoul()),
            Ok(ClockInDecision::Create {
                work_date: date("2026-03-02"),
                clock_in_at: at
            })
        );
        let at_0459 = ts("2026-03-02T04:59:00+09:00");
        assert!(matches!(
            decide_clock_in(None, at_0459, &seoul()),
            Ok(ClockInDecision::Create { work_date, .. }) if work_date == date("2026-03-01")
        ));
    }

    #[test]
    fn double_clock_in_returns_existing_record() {
        let existing = open("2026-03-02", "2026-03-02T09:00:00+09:00");
        let later = ts("2026-03-02T09:00:01+09:00");
        assert_eq!(
            decide_clock_in(Some(&existing), later, &seoul()),
            Ok(ClockInDecision::AlreadyClockedIn(existing))
        );
        // 기록보다 이른 시각의 재요청(기기 시계 오차)도 같은 근무일이면 그대로 돌려준다.
        let earlier = ts("2026-03-02T08:59:00+09:00");
        assert_eq!(
            decide_clock_in(Some(&existing), earlier, &seoul()),
            Ok(ClockInDecision::AlreadyClockedIn(existing))
        );
    }

    #[test]
    fn clock_in_after_clock_out_same_day_conflicts() {
        let done = closed(
            "2026-03-02",
            "2026-03-02T09:00:00+09:00",
            "2026-03-02T18:00:00+09:00",
        );
        assert_eq!(
            decide_clock_in(Some(&done), ts("2026-03-02T19:00:00+09:00"), &seoul()),
            Err(ConflictReason::AlreadyClockedOut.into())
        );
    }

    #[test]
    fn open_record_from_earlier_work_date_does_not_block() {
        let stale = open("2026-03-01", "2026-03-01T09:00:00+09:00");
        let now = ts("2026-03-02T09:00:00+09:00");
        assert_eq!(
            decide_clock_in(Some(&stale), now, &seoul()),
            Ok(ClockInDecision::Create {
                work_date: date("2026-03-02"),
                clock_in_at: now
            })
        );
    }

    #[test]
    fn early_morning_clock_in_belongs_to_yesterday_then_daytime_creates_today() {
        let zone = seoul();
        let at_0400 = ts("2026-03-03T04:00:00+09:00");
        let first = decide_clock_in(None, at_0400, &zone);
        assert_eq!(
            first,
            Ok(ClockInDecision::Create {
                work_date: date("2026-03-02"),
                clock_in_at: at_0400
            })
        );
        let yesterday = RecordState {
            work_date: date("2026-03-02"),
            clock_in_at: at_0400,
            clock_out_at: None,
        };
        let at_0900 = ts("2026-03-03T09:00:00+09:00");
        assert_eq!(
            decide_clock_in(Some(&yesterday), at_0900, &zone),
            Ok(ClockInDecision::Create {
                work_date: date("2026-03-03"),
                clock_in_at: at_0900
            })
        );
        // 어제 기록이 닫혀 있어도 마찬가지.
        let closed_yesterday = RecordState {
            clock_out_at: Some(ts("2026-03-03T04:30:00+09:00")),
            ..yesterday
        };
        assert!(matches!(
            decide_clock_in(Some(&closed_yesterday), at_0900, &zone),
            Ok(ClockInDecision::Create { .. })
        ));
    }

    #[test]
    fn clock_in_rejects_record_from_future_work_date() {
        let future = open("2026-03-03", "2026-03-03T09:00:00+09:00");
        assert_eq!(
            decide_clock_in(Some(&future), ts("2026-03-02T09:00:00+09:00"), &seoul()),
            Err(DomainError::validation(
                "now",
                ValidationReason::Inconsistent
            ))
        );
    }

    // ---- 퇴근 ----

    #[test]
    fn clock_out_closes_today() {
        let record = open("2026-03-02", "2026-03-02T09:00:00+09:00");
        let now = ts("2026-03-02T18:00:00+09:00");
        assert_eq!(
            decide_clock_out(Some(&record), now, &seoul()),
            Ok(ClockOutDecision::Close {
                work_date: date("2026-03-02"),
                clock_in_at: record.clock_in_at,
                clock_out_at: now,
            })
        );
    }

    #[test]
    fn night_shift_clock_out_closes_previous_work_date() {
        let record = open("2026-03-02", "2026-03-02T22:00:00+09:00");
        let now = ts("2026-03-03T06:00:00+09:00");
        assert_eq!(seoul().work_date(now), date("2026-03-03"));
        assert_eq!(
            decide_clock_out(Some(&record), now, &seoul()),
            Ok(ClockOutDecision::Close {
                work_date: date("2026-03-02"),
                clock_in_at: record.clock_in_at,
                clock_out_at: now,
            })
        );
    }

    #[test]
    fn clock_out_at_max_shift_boundary() {
        let record = open("2026-03-02", "2026-03-02T09:00:00+09:00");
        let exactly = ts("2026-03-03T09:00:00+09:00");
        assert!(matches!(
            decide_clock_out(Some(&record), exactly, &seoul()),
            Ok(ClockOutDecision::Close { clock_out_at, .. }) if clock_out_at == exactly
        ));
        assert!(matches!(
            decide_clock_out(Some(&record), ms(exactly, -1), &seoul()),
            Ok(ClockOutDecision::Close { .. })
        ));
        assert_eq!(
            decide_clock_out(Some(&record), ms(exactly, 1), &seoul()),
            Err(ConflictReason::NotClockedIn.into())
        );
    }

    #[test]
    fn clock_out_without_record_conflicts() {
        assert_eq!(
            decide_clock_out(None, ts("2026-03-02T18:00:00+09:00"), &seoul()),
            Err(ConflictReason::NotClockedIn.into())
        );
    }

    #[test]
    fn double_clock_out_is_idempotent() {
        let done = closed(
            "2026-03-02",
            "2026-03-02T09:00:00+09:00",
            "2026-03-02T18:00:00+09:00",
        );
        assert_eq!(
            decide_clock_out(Some(&done), ts("2026-03-02T18:00:05+09:00"), &seoul()),
            Ok(ClockOutDecision::AlreadyClockedOut(done))
        );
        // 야간 근무를 닫은 뒤 다음 근무일에 다시 눌러도 같은 기록을 돌려준다.
        let night = closed(
            "2026-03-02",
            "2026-03-02T22:00:00+09:00",
            "2026-03-03T06:00:00+09:00",
        );
        assert_eq!(
            decide_clock_out(Some(&night), ts("2026-03-03T06:00:30+09:00"), &seoul()),
            Ok(ClockOutDecision::AlreadyClockedOut(night))
        );
    }

    #[test]
    fn clock_out_with_only_old_closed_record_conflicts() {
        let old = closed(
            "2026-03-01",
            "2026-03-01T09:00:00+09:00",
            "2026-03-01T18:00:00+09:00",
        );
        assert_eq!(
            decide_clock_out(Some(&old), ts("2026-03-02T18:00:00+09:00"), &seoul()),
            Err(ConflictReason::NotClockedIn.into())
        );
    }

    #[test]
    fn clock_out_not_after_clock_in_conflicts() {
        let record = open("2026-03-02", "2026-03-02T09:00:00+09:00");
        for now in [ms(record.clock_in_at, -1), record.clock_in_at] {
            assert_eq!(
                decide_clock_out(Some(&record), now, &seoul()),
                Err(ConflictReason::ClockOutBeforeClockIn.into())
            );
        }
        assert!(decide_clock_out(Some(&record), ms(record.clock_in_at, 1), &seoul()).is_ok());
    }

    // ---- 수정 검증 ----

    #[test]
    fn validate_edit_accepts_normal_and_open_records() {
        let zone = seoul();
        let now = ts("2026-03-03T10:00:00+09:00");
        let clock_in = ts("2026-03-02T09:00:00+09:00");
        assert_eq!(
            validate_edit(
                clock_in,
                Some(ts("2026-03-02T18:00:00+09:00")),
                date("2026-03-02"),
                now,
                &zone
            ),
            Ok(())
        );
        assert_eq!(
            validate_edit(clock_in, None, date("2026-03-02"), now, &zone),
            Ok(())
        );
    }

    #[test]
    fn validate_edit_requires_out_after_in() {
        let zone = seoul();
        let now = ts("2026-03-03T10:00:00+09:00");
        let clock_in = ts("2026-03-02T09:00:00+09:00");
        let err = Err(DomainError::validation(
            "clock_out_at",
            ValidationReason::Inconsistent,
        ));
        assert_eq!(
            validate_edit(clock_in, Some(clock_in), date("2026-03-02"), now, &zone),
            err
        );
        assert_eq!(
            validate_edit(
                clock_in,
                Some(ms(clock_in, -1)),
                date("2026-03-02"),
                now,
                &zone
            ),
            err
        );
        assert_eq!(
            validate_edit(
                clock_in,
                Some(ms(clock_in, 1)),
                date("2026-03-02"),
                now,
                &zone
            ),
            Ok(())
        );
    }

    #[test]
    fn validate_edit_span_boundary_is_max_shift() {
        let zone = seoul();
        let now = ts("2026-03-05T10:00:00+09:00");
        let clock_in = ts("2026-03-02T09:00:00+09:00");
        let exactly = ts("2026-03-03T09:00:00+09:00");
        assert_eq!(
            validate_edit(clock_in, Some(exactly), date("2026-03-02"), now, &zone),
            Ok(())
        );
        assert_eq!(
            validate_edit(
                clock_in,
                Some(ms(exactly, 1)),
                date("2026-03-02"),
                now,
                &zone
            ),
            Err(DomainError::validation(
                "clock_out_at",
                ValidationReason::OutOfRange
            ))
        );
    }

    #[test]
    fn validate_edit_future_skew_boundary() {
        let zone = seoul();
        let now = ts("2026-03-02T12:00:00+09:00");
        let limit = ts("2026-03-02T12:05:00+09:00");
        assert_eq!(
            validate_edit(limit, None, date("2026-03-02"), now, &zone),
            Ok(())
        );
        assert_eq!(
            validate_edit(ms(limit, 1), None, date("2026-03-02"), now, &zone),
            Err(DomainError::validation(
                "clock_in_at",
                ValidationReason::OutOfRange
            ))
        );
        let clock_in = ts("2026-03-02T09:00:00+09:00");
        assert_eq!(
            validate_edit(clock_in, Some(limit), date("2026-03-02"), now, &zone),
            Ok(())
        );
        assert_eq!(
            validate_edit(clock_in, Some(ms(limit, 1)), date("2026-03-02"), now, &zone),
            Err(DomainError::validation(
                "clock_out_at",
                ValidationReason::OutOfRange
            ))
        );
    }

    #[test]
    fn validate_edit_requires_clock_in_on_work_date() {
        let zone = seoul();
        let now = ts("2026-03-03T10:00:00+09:00");
        let err = Err(DomainError::validation(
            "work_date",
            ValidationReason::Inconsistent,
        ));
        // 04:59 KST 출근은 전날 근무일이다.
        assert_eq!(
            validate_edit(
                ts("2026-03-02T04:59:00+09:00"),
                None,
                date("2026-03-02"),
                now,
                &zone
            ),
            err
        );
        assert_eq!(
            validate_edit(
                ts("2026-03-02T04:59:00+09:00"),
                None,
                date("2026-03-01"),
                now,
                &zone
            ),
            Ok(())
        );
        assert_eq!(
            validate_edit(
                ts("2026-03-02T05:00:00+09:00"),
                None,
                date("2026-03-02"),
                now,
                &zone
            ),
            Ok(())
        );
    }

    // ---- 하루 판정 ----

    #[test]
    fn on_time_full_day_is_normal() {
        let eval = eval_closed(
            "2026-03-02T09:00:00+09:00",
            "2026-03-02T18:00:00+09:00",
            None,
        );
        assert_eq!(
            eval,
            DayEvaluation {
                status: DayStatus::Normal,
                worked_minutes: 480,
                break_minutes: 60,
                late_minutes: 0,
                early_leave_minutes: 0,
                overtime_minutes: 0,
            }
        );
    }

    #[test]
    fn seconds_are_truncated_to_minutes_with_zero_grace() {
        let late = |clock_in: &str| eval_closed(clock_in, "2026-03-02T18:00:00+09:00", None);
        let normal = late("2026-03-02T08:59:59+09:00");
        assert_eq!((normal.status, normal.late_minutes), (DayStatus::Normal, 0));
        let normal = late("2026-03-02T09:00:00+09:00");
        assert_eq!((normal.status, normal.late_minutes), (DayStatus::Normal, 0));
        let normal = late("2026-03-02T09:00:59+09:00");
        assert_eq!((normal.status, normal.late_minutes), (DayStatus::Normal, 0));
        let late_one = late("2026-03-02T09:01:00+09:00");
        assert_eq!(
            (late_one.status, late_one.late_minutes),
            (DayStatus::Late, 1)
        );
    }

    #[test]
    fn grace_boundary_counts_minutes_from_start() {
        let policy = policy_with_grace(10);
        let check = |clock_in: Timestamp| {
            let record = RecordState {
                work_date: date("2026-03-02"),
                clock_in_at: clock_in,
                clock_out_at: Some(ts("2026-03-02T18:00:00+09:00")),
            };
            let eval = eval_on(Some(&record), None, true, NOW_LATER, &policy);
            (eval.status, eval.late_minutes)
        };
        let grace_end = ts("2026-03-02T09:10:00+09:00");
        assert_eq!(check(ms(grace_end, -1)), (DayStatus::Normal, 0));
        assert_eq!(check(grace_end), (DayStatus::Normal, 0));
        assert_eq!(check(ms(grace_end, 1)), (DayStatus::Normal, 0));
        let next_minute = ts("2026-03-02T09:11:00+09:00");
        assert_eq!(check(ms(next_minute, -1)), (DayStatus::Normal, 0));
        // 지각 분은 유예가 아니라 시작 시각부터 센다.
        assert_eq!(check(next_minute), (DayStatus::Late, 11));
    }

    #[test]
    fn early_leave_and_overtime() {
        let early = eval_closed(
            "2026-03-02T09:00:00+09:00",
            "2026-03-02T17:30:00+09:00",
            None,
        );
        assert_eq!(
            (
                early.status,
                early.early_leave_minutes,
                early.overtime_minutes
            ),
            (DayStatus::EarlyLeave, 30, 0)
        );
        // 초는 버리므로 17:59:30 퇴근은 17:59 퇴근이다.
        let early = eval_closed(
            "2026-03-02T09:00:00+09:00",
            "2026-03-02T17:59:30+09:00",
            None,
        );
        assert_eq!(
            (early.status, early.early_leave_minutes),
            (DayStatus::EarlyLeave, 1)
        );
        let over = eval_closed(
            "2026-03-02T09:00:00+09:00",
            "2026-03-02T20:15:00+09:00",
            None,
        );
        assert_eq!(
            (over.status, over.early_leave_minutes, over.overtime_minutes),
            (DayStatus::Normal, 0, 135)
        );
        let both = eval_closed(
            "2026-03-02T10:00:00+09:00",
            "2026-03-02T17:00:00+09:00",
            None,
        );
        assert_eq!(
            (both.status, both.late_minutes, both.early_leave_minutes),
            (DayStatus::LateAndEarlyLeave, 60, 60)
        );
    }

    #[test]
    fn partial_leave_shifts_expected_times() {
        let am_half = eval_closed(
            "2026-03-02T14:00:00+09:00",
            "2026-03-02T18:00:00+09:00",
            Some(LeaveUnit::AmHalf),
        );
        assert_eq!(am_half.status, DayStatus::Normal);
        let am_half_late = eval_closed(
            "2026-03-02T14:01:00+09:00",
            "2026-03-02T18:00:00+09:00",
            Some(LeaveUnit::AmHalf),
        );
        assert_eq!(
            (am_half_late.status, am_half_late.late_minutes),
            (DayStatus::Late, 1)
        );
        let pm_half = eval_closed(
            "2026-03-02T09:00:00+09:00",
            "2026-03-02T14:00:00+09:00",
            Some(LeaveUnit::PmHalf),
        );
        assert_eq!(
            (pm_half.status, pm_half.overtime_minutes),
            (DayStatus::Normal, 0)
        );
        let pm_half_over = eval_closed(
            "2026-03-02T09:00:00+09:00",
            "2026-03-02T15:00:00+09:00",
            Some(LeaveUnit::PmHalf),
        );
        assert_eq!(pm_half_over.overtime_minutes, 60);
        let am_quarter = eval_closed(
            "2026-03-02T11:00:00+09:00",
            "2026-03-02T18:00:00+09:00",
            Some(LeaveUnit::AmQuarter),
        );
        assert_eq!(am_quarter.status, DayStatus::Normal);
        let am_quarter_late = eval_closed(
            "2026-03-02T11:01:00+09:00",
            "2026-03-02T18:00:00+09:00",
            Some(LeaveUnit::AmQuarter),
        );
        assert_eq!(am_quarter_late.late_minutes, 1);
        let pm_quarter = eval_closed(
            "2026-03-02T09:00:00+09:00",
            "2026-03-02T16:00:00+09:00",
            Some(LeaveUnit::PmQuarter),
        );
        assert_eq!(pm_quarter.status, DayStatus::Normal);
        let pm_quarter_early = eval_closed(
            "2026-03-02T09:00:00+09:00",
            "2026-03-02T15:59:00+09:00",
            Some(LeaveUnit::PmQuarter),
        );
        assert_eq!(
            (
                pm_quarter_early.status,
                pm_quarter_early.early_leave_minutes
            ),
            (DayStatus::EarlyLeave, 1)
        );
    }

    #[test]
    fn two_partial_leaves_on_one_day_are_combined() {
        let both = |a: LeaveUnit, b: LeaveUnit| Some(DayLeave::from(a).with(b));
        let policy = policy_with_grace(0);
        let eval = |clock_in: &str, clock_out: &str, leave: Option<DayLeave>| {
            let record = closed("2026-03-02", clock_in, clock_out);
            let input = DayInput {
                work_date: date("2026-03-02"),
                is_working_day: true,
                record: Some(&record),
                leave,
                now: ts("2026-03-04T00:00:00+09:00"),
            };
            evaluate_day(&input, &policy, &seoul())
        };
        // 오전 반차 + 오후 반차 = 종일: 기록이 없어도 결근이 아니다.
        let no_record = DayInput {
            work_date: date("2026-03-02"),
            is_working_day: true,
            record: None,
            leave: both(LeaveUnit::AmHalf, LeaveUnit::PmHalf),
            now: ts("2026-03-04T00:00:00+09:00"),
        };
        assert_eq!(
            evaluate_day(&no_record, &policy, &seoul()).status,
            DayStatus::OnLeave
        );
        // 오전 반반차 + 오후 반차: 11:00~14:00이 기대 근무.
        let quarter_half = eval(
            "2026-03-02T11:00:00+09:00",
            "2026-03-02T14:00:00+09:00",
            both(LeaveUnit::AmQuarter, LeaveUnit::PmHalf),
        );
        assert_eq!(
            (
                quarter_half.status,
                quarter_half.late_minutes,
                quarter_half.early_leave_minutes
            ),
            (DayStatus::Normal, 0, 0)
        );
        // 오전 반반차 + 오후 반반차: 11:00~16:00.
        let quarters = eval(
            "2026-03-02T11:00:00+09:00",
            "2026-03-02T16:00:00+09:00",
            both(LeaveUnit::AmQuarter, LeaveUnit::PmQuarter),
        );
        assert_eq!(
            (
                quarters.status,
                quarters.late_minutes,
                quarters.early_leave_minutes
            ),
            (DayStatus::Normal, 0, 0)
        );
    }

    #[test]
    fn break_tier_boundaries_in_evaluation() {
        let worked = |out: &str| {
            let eval = eval_closed("2026-03-02T09:00:00+09:00", out, None);
            (eval.worked_minutes, eval.break_minutes)
        };
        assert_eq!(worked("2026-03-02T12:59:00+09:00"), (239, 0));
        assert_eq!(worked("2026-03-02T13:00:00+09:00"), (210, 30));
        assert_eq!(worked("2026-03-02T16:59:00+09:00"), (449, 30));
        assert_eq!(worked("2026-03-02T17:00:00+09:00"), (420, 60));
    }

    #[test]
    fn zero_length_shift_counts_nothing() {
        let eval = eval_closed(
            "2026-03-02T09:00:00+09:00",
            "2026-03-02T09:00:00+09:00",
            None,
        );
        assert_eq!((eval.worked_minutes, eval.break_minutes), (0, 0));
        assert_eq!(eval.status, DayStatus::EarlyLeave);
        // 저장 값이 뒤집혀 있어도(퇴근 < 출근) 패닉하거나 음수가 되지 않는다.
        let inverted = eval_closed(
            "2026-03-02T18:00:00+09:00",
            "2026-03-02T09:00:00+09:00",
            None,
        );
        assert_eq!((inverted.worked_minutes, inverted.break_minutes), (0, 0));
    }

    #[test]
    fn open_record_is_working_until_max_shift_then_missing() {
        let record = open("2026-03-02", "2026-03-02T09:05:00+09:00");
        let policy = WorkPolicy::default_korean();
        let working = eval_on(
            Some(&record),
            None,
            true,
            "2026-03-02T12:00:00+09:00",
            &policy,
        );
        assert_eq!(
            (working.status, working.late_minutes, working.worked_minutes),
            (DayStatus::Working, 5, 0)
        );
        let limit = ts("2026-03-03T09:05:00+09:00");
        let at_limit = DayInput {
            work_date: date("2026-03-02"),
            is_working_day: true,
            record: Some(&record),
            leave: None,
            now: limit,
        };
        assert_eq!(
            evaluate_day(&at_limit, &policy, &seoul()).status,
            DayStatus::Working
        );
        let past = DayInput {
            now: ms(limit, 1),
            ..at_limit
        };
        assert_eq!(
            evaluate_day(&past, &policy, &seoul()).status,
            DayStatus::MissingClockOut
        );
    }

    #[test]
    fn night_shift_is_still_working_after_next_work_date_starts() {
        let record = open("2026-03-02", "2026-03-02T22:00:00+09:00");
        let eval = eval_on(
            Some(&record),
            None,
            true,
            "2026-03-03T06:00:00+09:00",
            &WorkPolicy::default_korean(),
        );
        assert_eq!(eval.status, DayStatus::Working);
    }

    #[test]
    fn night_shift_evaluation_counts_across_cutoff() {
        let eval = eval_closed(
            "2026-03-02T22:00:00+09:00",
            "2026-03-03T06:00:00+09:00",
            None,
        );
        assert_eq!((eval.worked_minutes, eval.break_minutes), (420, 60));
        assert_eq!(eval.late_minutes, 13 * 60);
        assert_eq!(eval.overtime_minutes, 12 * 60);
    }

    #[test]
    fn no_record_status_depends_on_day_and_time() {
        let policy = WorkPolicy::default_korean();
        let next_start = ts("2026-03-03T05:00:00+09:00");
        let later = ts(NOW_LATER);
        let input = DayInput {
            work_date: date("2026-03-02"),
            is_working_day: true,
            record: None,
            leave: None,
            now: ms(next_start, -1),
        };
        assert_eq!(
            evaluate_day(&input, &policy, &seoul()).status,
            DayStatus::Scheduled
        );
        assert_eq!(
            evaluate_day(
                &DayInput {
                    now: next_start,
                    ..input
                },
                &policy,
                &seoul()
            )
            .status,
            DayStatus::Absent
        );
        let before_start = DayInput {
            now: ts("2026-03-02T08:00:00+09:00"),
            ..input
        };
        assert_eq!(
            evaluate_day(&before_start, &policy, &seoul()).status,
            DayStatus::Scheduled
        );
        let future = DayInput {
            now: ts("2026-02-20T08:00:00+09:00"),
            ..input
        };
        assert_eq!(
            evaluate_day(&future, &policy, &seoul()).status,
            DayStatus::Scheduled
        );
        let off = DayInput {
            is_working_day: false,
            now: later,
            ..input
        };
        assert_eq!(
            evaluate_day(&off, &policy, &seoul()).status,
            DayStatus::DayOff
        );
        let leave = DayInput {
            leave: Some(LeaveUnit::FullDay.into()),
            now: later,
            ..input
        };
        assert_eq!(
            evaluate_day(&leave, &policy, &seoul()).status,
            DayStatus::OnLeave
        );
        // 반차만 있고 기록이 없으면 결근이다.
        let half = DayInput {
            leave: Some(LeaveUnit::AmHalf.into()),
            now: later,
            ..input
        };
        assert_eq!(
            evaluate_day(&half, &policy, &seoul()).status,
            DayStatus::Absent
        );
    }

    #[test]
    fn work_on_day_off_or_full_leave_is_normal_without_late_or_early() {
        let record = closed(
            "2026-03-02",
            "2026-03-02T13:00:00+09:00",
            "2026-03-02T15:00:00+09:00",
        );
        let policy = WorkPolicy::default_korean();
        for (is_working_day, leave) in [(false, None), (true, Some(LeaveUnit::FullDay))] {
            let eval = eval_on(Some(&record), leave, is_working_day, NOW_LATER, &policy);
            assert_eq!(
                eval,
                DayEvaluation {
                    status: DayStatus::Normal,
                    worked_minutes: 120,
                    break_minutes: 0,
                    late_minutes: 0,
                    early_leave_minutes: 0,
                    overtime_minutes: 0,
                }
            );
        }
    }

    #[test]
    fn dst_spring_forward_counts_real_elapsed_minutes() {
        let zone = WorkCalendarZone::new(chrono_tz::America::New_York, MinuteOfDay::MIDNIGHT)
            .expect("zone");
        // 2026-03-08 02:00 EST → 03:00 EDT. 벽시계로는 9시간, 실제로는 8시간.
        let record = closed(
            "2026-03-08",
            "2026-03-08T00:30:00-05:00",
            "2026-03-08T09:30:00-04:00",
        );
        let input = DayInput {
            work_date: date("2026-03-08"),
            is_working_day: true,
            record: Some(&record),
            leave: None,
            now: ts("2026-03-10T00:00:00Z"),
        };
        let eval = evaluate_day(&input, &WorkPolicy::default_korean(), &zone);
        assert_eq!((eval.worked_minutes, eval.break_minutes), (420, 60));
        // 기대 종료 18:00 EDT 기준 조퇴 510분.
        assert_eq!(eval.early_leave_minutes, 510);
    }

    #[test]
    fn extreme_timestamps_do_not_panic() {
        let min = Timestamp::from_millis(DateTime::<Utc>::MIN_UTC.timestamp_millis()).expect("min");
        let max = Timestamp::from_millis(DateTime::<Utc>::MAX_UTC.timestamp_millis()).expect("max");
        let zones = [
            seoul(),
            WorkCalendarZone::new(chrono_tz::America::New_York, hm("05:00")).expect("zone"),
            WorkCalendarZone::new(chrono_tz::Pacific::Kiritimati, hm("12:00")).expect("zone"),
        ];
        for zone in &zones {
            for at in [min, max] {
                let out_of_range = DomainError::validation("now", ValidationReason::OutOfRange);
                assert_eq!(decide_clock_in(None, at, zone), Err(out_of_range.clone()));
                assert_eq!(decide_clock_out(None, at, zone), Err(out_of_range));
                let hostile = RecordState {
                    work_date: NaiveDate::MIN,
                    clock_in_at: at,
                    clock_out_at: Some(at),
                };
                let _ = decide_clock_out(Some(&hostile), ts("2026-03-02T09:00:00Z"), zone);
                assert!(validate_edit(
                    at,
                    None,
                    date("2026-03-02"),
                    ts("2026-03-02T09:00:00Z"),
                    zone
                )
                .is_err());
                assert!(validate_edit(
                    ts("2026-03-02T09:00:00Z"),
                    Some(at),
                    date("2026-03-02"),
                    at,
                    zone
                )
                .is_err());
                for work_date in [NaiveDate::MIN, NaiveDate::MAX, date("2026-03-02")] {
                    for record in [
                        None,
                        Some(RecordState {
                            work_date,
                            clock_in_at: min,
                            clock_out_at: Some(max),
                        }),
                        Some(RecordState {
                            work_date,
                            clock_in_at: max,
                            clock_out_at: Some(min),
                        }),
                        Some(RecordState {
                            work_date,
                            clock_in_at: at,
                            clock_out_at: None,
                        }),
                    ] {
                        for leave in [None, Some(LeaveUnit::AmHalf), Some(LeaveUnit::PmQuarter)] {
                            let input = DayInput {
                                work_date,
                                is_working_day: true,
                                record: record.as_ref(),
                                leave: leave.map(DayLeave::from),
                                now: at,
                            };
                            let _ = evaluate_day(&input, &WorkPolicy::default_korean(), zone);
                        }
                    }
                }
            }
        }
    }

    // ---- 월 요약 ----

    fn day(status: DayStatus, worked: u32, overtime: u32, late: u32) -> DayEvaluation {
        DayEvaluation {
            status,
            worked_minutes: worked,
            break_minutes: 0,
            late_minutes: late,
            early_leave_minutes: 0,
            overtime_minutes: overtime,
        }
    }

    #[test]
    fn monthly_summary_counts_statuses_and_totals() {
        let days = [
            day(DayStatus::Normal, 480, 0, 0),
            day(DayStatus::Normal, 480, 30, 0),
            day(DayStatus::Late, 470, 0, 10),
            day(DayStatus::EarlyLeave, 400, 0, 0),
            day(DayStatus::LateAndEarlyLeave, 300, 0, 60),
            day(DayStatus::MissingClockOut, 0, 0, 5),
            day(DayStatus::Absent, 0, 0, 0),
            day(DayStatus::OnLeave, 0, 0, 0),
            day(DayStatus::DayOff, 0, 0, 0),
            day(DayStatus::DayOff, 0, 0, 0),
            day(DayStatus::Working, 0, 0, 0),
            day(DayStatus::Scheduled, 0, 0, 0),
        ];
        assert_eq!(
            MonthlySummary::from_days(&days),
            MonthlySummary {
                scheduled_days: 1,
                working_days: 1,
                normal_days: 2,
                late_days: 1,
                early_leave_days: 1,
                late_and_early_leave_days: 1,
                missing_clock_out_days: 1,
                absent_days: 1,
                on_leave_days: 1,
                day_off_days: 2,
                total_worked_minutes: 2130,
                total_overtime_minutes: 30,
                total_late_minutes: 75,
            }
        );
        assert_eq!(MonthlySummary::from_days(&[]), MonthlySummary::default());
    }

    #[test]
    fn monthly_summary_does_not_overflow() {
        let days = vec![day(DayStatus::Normal, u32::MAX, u32::MAX, u32::MAX); 40];
        let summary = MonthlySummary::from_days(&days);
        assert_eq!(summary.normal_days, 40);
        assert_eq!(summary.total_worked_minutes, u64::from(u32::MAX) * 40);
        assert_eq!(summary.total_overtime_minutes, u64::from(u32::MAX) * 40);
    }

    proptest! {
        #[test]
        fn evaluate_day_never_panics_and_obeys_invariants(
            in_ms in -2_208_988_800_000i64..7_258_118_400_000, // 1900..2200
            span_ms in -172_800_000i64..172_800_000,
            now_offset_ms in -172_800_000i64..259_200_000,
            has_out in any::<bool>(),
            is_working_day in any::<bool>(),
            leave_index in 0usize..6,
            grace in 0u16..=120,
        ) {
            let zone = seoul();
            let policy = policy_with_grace(grace);
            let clock_in_at = Timestamp::from_millis(in_ms).expect("in");
            let clock_out_at = Timestamp::from_millis(in_ms + span_ms).expect("out");
            let now = Timestamp::from_millis(in_ms + now_offset_ms).expect("now");
            let record = RecordState {
                work_date: zone.work_date(clock_in_at),
                clock_in_at,
                clock_out_at: has_out.then_some(clock_out_at),
            };
            let leave = LeaveUnit::ALL.get(leave_index).copied();
            let input = DayInput { work_date: record.work_date, is_working_day, record: Some(&record), leave: leave.map(DayLeave::from), now };
            let eval = evaluate_day(&input, &policy, &zone);
            if has_out {
                let span = (clock_out_at.as_millis().div_euclid(60_000)
                    - clock_in_at.as_millis().div_euclid(60_000)).max(0);
                prop_assert!(i64::from(eval.worked_minutes) + i64::from(eval.break_minutes) <= span);
            }
            if !is_working_day || leave == Some(LeaveUnit::FullDay) {
                prop_assert_eq!(eval.late_minutes, 0);
                prop_assert_eq!(eval.early_leave_minutes, 0);
                prop_assert!(matches!(
                    eval.status,
                    DayStatus::Normal | DayStatus::Working | DayStatus::MissingClockOut
                ));
            }
            let without_record = DayInput { record: None, ..input };
            let _ = evaluate_day(&without_record, &policy, &zone);
        }
    }

    // ── 추적 기간 ──

    #[test]
    fn tracked_period_starts_at_later_of_hire_and_account_creation() {
        let period = TrackedPeriod::new(date("2020-01-01"), date("2026-03-10"), None);
        assert_eq!(period.from(), date("2026-03-10"));
        assert!(!period.contains(date("2026-03-09")));
        assert!(period.contains(date("2026-03-10")));
        assert!(period.contains(date("2099-12-31")));

        let hired_later = TrackedPeriod::new(date("2026-04-01"), date("2026-03-10"), None);
        assert_eq!(hired_later.from(), date("2026-04-01"));
        assert!(!hired_later.contains(date("2026-03-31")));
        assert!(hired_later.contains(date("2026-04-01")));
    }

    #[test]
    fn tracked_period_includes_retired_on_and_excludes_the_day_after() {
        let period = TrackedPeriod::new(
            date("2024-03-02"),
            date("2024-03-02"),
            Some(date("2026-03-15")),
        );
        assert!(period.contains(date("2026-03-15")));
        assert!(!period.contains(date("2026-03-16")));
        assert!(!period.contains(date("2024-03-01")));
        assert!(period.contains(date("2024-03-02")));
    }

    #[test]
    fn tracked_period_is_empty_when_retired_before_tracking_starts() {
        let period = TrackedPeriod::new(
            date("2026-03-01"),
            date("2026-03-10"),
            Some(date("2026-03-05")),
        );
        for day in [
            "2026-03-01",
            "2026-03-05",
            "2026-03-06",
            "2026-03-10",
            "2026-03-11",
        ] {
            assert!(!period.contains(date(day)), "{day}");
        }
    }

    #[test]
    fn tracked_period_with_future_hire_date_tracks_nothing_until_then() {
        let period = TrackedPeriod::new(date("2026-12-01"), date("2026-03-10"), None);
        assert!(!period.contains(date("2026-03-10")));
        assert!(!period.contains(date("2026-11-30")));
        assert!(period.contains(date("2026-12-01")));
    }

    #[test]
    fn untracked_day_without_record_is_day_off_and_with_record_is_off_day_work() {
        let policy = WorkPolicy::default_korean();
        let period = TrackedPeriod::new(date("2026-03-03"), date("2026-03-03"), None);
        let day = date("2026-03-02");
        let input = DayInput {
            work_date: day,
            is_working_day: period.working_day(day, true),
            record: None,
            leave: None,
            now: ts("2026-03-10T12:00:00+09:00"),
        };
        assert_eq!(
            evaluate_day(&input, &policy, &seoul()),
            DayEvaluation::empty(DayStatus::DayOff)
        );
        let summary = MonthlySummary::from_days(&[evaluate_day(&input, &policy, &seoul())]);
        assert_eq!((summary.absent_days, summary.day_off_days), (0, 1));

        let record = closed(
            "2026-03-02",
            "2026-03-02T10:00:00+09:00",
            "2026-03-02T15:00:00+09:00",
        );
        let with_record = DayInput {
            record: Some(&record),
            ..input
        };
        let evaluation = evaluate_day(&with_record, &policy, &seoul());
        assert_eq!(evaluation.status, DayStatus::Normal);
        assert_eq!(
            (evaluation.late_minutes, evaluation.early_leave_minutes),
            (0, 0)
        );
        assert!(evaluation.worked_minutes > 0);

        let tracked = date("2026-03-03");
        assert!(period.working_day(tracked, true));
        assert!(!period.working_day(tracked, false));
    }
}
