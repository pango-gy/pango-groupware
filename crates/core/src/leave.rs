//! 휴가: 수량 단위, 신청 크기와 겹침, 연차 자동 부여, 원장 기반 잔여, 신청 상태 전이.
//!
//! 모든 함수는 순수하다. 날짜와 달력은 인자로 받고, 원장은 읽기만 한다.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use chrono::{Months, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::calendar::{DateRange, WorkCalendar};
use crate::error::{ConflictReason, DomainError, DomainResult, ValidationReason};
use crate::id::LeaveRequestId;

/// 수량이 표현 범위를 벗어났거나 부호가 맞지 않을 때의 오류.
const AMOUNT_OUT_OF_RANGE: DomainError =
    DomainError::validation("amount", ValidationReason::OutOfRange);

/// 휴가 수량. 단위는 **1/4일**이다 (1일 = 4, 반차 = 2, 반반차 = 1). 음수는 차감을 뜻한다.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct LeaveAmount(i32);

impl LeaveAmount {
    pub const ZERO: Self = Self(0);
    pub const QUARTERS_PER_DAY: i32 = 4;

    pub const fn from_quarters(quarters: i32) -> Self {
        Self(quarters)
    }

    pub fn from_days(days: i32) -> Option<Self> {
        days.checked_mul(Self::QUARTERS_PER_DAY).map(Self)
    }

    pub const fn quarters(self) -> i32 {
        self.0
    }

    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }

    pub fn checked_neg(self) -> Option<Self> {
        self.0.checked_neg().map(Self)
    }
}

/// `1`, `0.5`, `-2.25` 처럼 일 단위로 표시한다.
impl fmt::Display for LeaveAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let abs = self.0.unsigned_abs();
        let (days, rem) = (abs / 4, abs % 4);
        match rem {
            0 => write!(f, "{sign}{days}"),
            1 => write!(f, "{sign}{days}.25"),
            2 => write!(f, "{sign}{days}.5"),
            _ => write!(f, "{sign}{days}.75"),
        }
    }
}

crate::string_enum! {
    /// 하루 중 휴가를 쓰는 단위.
    pub enum LeaveUnit("unit") {
        /// 종일. 여러 날에 걸쳐 쓸 수 있는 유일한 단위.
        FullDay => "full_day",
        /// 오전 반차.
        AmHalf => "am_half",
        /// 오후 반차.
        PmHalf => "pm_half",
        /// 오전 반반차(근무 시작 후 2시간).
        AmQuarter => "am_quarter",
        /// 오후 반반차(근무 종료 전 2시간).
        PmQuarter => "pm_quarter",
    }
}

impl LeaveUnit {
    /// 근무일 하루당 차감되는 수량.
    pub const fn amount_per_day(self) -> LeaveAmount {
        match self {
            Self::FullDay => LeaveAmount(4),
            Self::AmHalf | Self::PmHalf => LeaveAmount(2),
            Self::AmQuarter | Self::PmQuarter => LeaveAmount(1),
        }
    }

    pub const fn is_partial(self) -> bool {
        !matches!(self, Self::FullDay)
    }

    /// 하루를 1/4일 네 칸으로 나눴을 때 이 단위가 차지하는 칸(비트 0 = 첫 칸).
    /// 두 단위가 같은 칸을 쓰면 같은 날에 함께 쓸 수 없다.
    const fn slots(self) -> u8 {
        match self {
            Self::FullDay => 0b1111,
            Self::AmHalf => 0b0011,
            Self::PmHalf => 0b1100,
            Self::AmQuarter => 0b0001,
            Self::PmQuarter => 0b1000,
        }
    }
}

/// 한 근무일에 승인된 휴가를 모두 합친 것. 같은 날 시간대가 다른 반차·반반차 두 건(예: 오전
/// 반차 + 오후 반차)이 함께 승인될 수 있으므로, 근태 판정은 단위 하나가 아니라 이 합을 본다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DayLeave {
    slots: u8,
}

impl DayLeave {
    /// 이 휴가에 `unit`을 더한 것.
    #[must_use]
    pub const fn with(self, unit: LeaveUnit) -> Self {
        Self {
            slots: self.slots | unit.slots(),
        }
    }

    /// 하루를 모두 쉰다(종일 휴가이거나 반차 둘처럼 네 칸이 다 찼다).
    pub const fn is_full_day(self) -> bool {
        self.slots == LeaveUnit::FullDay.slots()
    }

    /// 근무 시작부터 쉬는 칸 수(0~4).
    pub const fn leading_quarters(self) -> u32 {
        self.slots.trailing_ones()
    }

    /// 근무 종료 쪽에서 쉬는 칸 수(0~4).
    pub const fn trailing_quarters(self) -> u32 {
        (self.slots << 4).leading_ones()
    }
}

impl From<LeaveUnit> for DayLeave {
    fn from(unit: LeaveUnit) -> Self {
        Self {
            slots: unit.slots(),
        }
    }
}

/// 검증된 휴가 신청 크기: 실제로 쉬는 근무일과 차감 수량.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveSpan {
    range: DateRange,
    unit: LeaveUnit,
    working_dates: Vec<NaiveDate>,
    first_day: NaiveDate,
    amount: LeaveAmount,
}

impl LeaveSpan {
    /// 근무 요일이 아닌 날과 공휴일은 건너뛰고 센다.
    ///
    /// 반차·반반차는 하루짜리 신청에만 쓸 수 있다. 며칠에 걸친 "매일 오전 반차"는
    /// 날마다 따로 신청해야 겹침 판정과 지각·조퇴 기준 조정이 단순해진다.
    pub fn new(range: DateRange, unit: LeaveUnit, calendar: &WorkCalendar) -> DomainResult<Self> {
        if unit.is_partial() && range.start() != range.end() {
            return Err(DomainError::validation(
                "unit",
                ValidationReason::Inconsistent,
            ));
        }
        let working_dates = calendar.working_days_in(range);
        let Some(&first_day) = working_dates.first() else {
            return Err(ConflictReason::LeaveNoWorkingDays.into());
        };
        let amount = i32::try_from(working_dates.len())
            .ok()
            .and_then(|days| days.checked_mul(unit.amount_per_day().quarters()))
            .map(LeaveAmount)
            .ok_or(AMOUNT_OUT_OF_RANGE)?;
        Ok(Self {
            range,
            unit,
            working_dates,
            first_day,
            amount,
        })
    }

    pub const fn range(&self) -> DateRange {
        self.range
    }

    pub const fn unit(&self) -> LeaveUnit {
        self.unit
    }

    /// 실제로 쉬는 근무일(오름차순, 비어 있지 않음).
    pub fn working_dates(&self) -> &[NaiveDate] {
        &self.working_dates
    }

    /// 휴가가 실제로 시작되는 날 = 첫 근무일. 원장의 사용 기준일과 취소 가능 여부는
    /// 구간의 시작일이 아니라 이 날짜로 판단한다(토요일부터 신청해도 월요일에 시작).
    pub const fn first_day(&self) -> NaiveDate {
        self.first_day
    }

    /// 차감할 총 수량.
    pub const fn amount(&self) -> LeaveAmount {
        self.amount
    }
}

/// 같은 구성원의 두 신청이 겹치는지.
///
/// 날짜를 하루라도 공유하면 겹친다(근무일이 아닌 날을 공유해도 마찬가지). 예외는
/// 둘 다 같은 하루짜리 신청이고 서로 다른 시간대를 쓰는 경우뿐이다
/// (오전 반차 + 오후 반차, 오전 반반차 + 오후 반차·반반차 등).
///
/// [`LeaveSpan`] 검증을 거치지 않은 "여러 날 + 반차"가 들어오면 겹치는 것으로 본다.
pub fn requests_overlap(a: (DateRange, LeaveUnit), b: (DateRange, LeaveUnit)) -> bool {
    let ((range_a, unit_a), (range_b, unit_b)) = (a, b);
    if !range_a.overlaps(&range_b) {
        return false;
    }
    let single_day = |range: DateRange| range.start() == range.end();
    if !(single_day(range_a) && single_day(range_b)) {
        return true;
    }
    unit_a.slots() & unit_b.slots() != 0
}

// ───────────── 연차 자동 부여 ─────────────

/// 자동 부여를 계산하는 최대 근속 연수. 입력 날짜가 터무니없어도(1년 입사, 9999년 조회)
/// 결과 크기가 이 값으로 묶인다. 실제 근속은 이보다 길 수 없다고 본다.
pub const MAX_SERVICE_YEARS: u32 = 80;

/// 입사 1년 미만 구간에서 매월 생기는 연차의 최대 횟수.
const FIRST_YEAR_MONTHLY_GRANTS: u32 = 11;

crate::string_enum! {
    /// 자동 부여의 종류. 표기는 `grant_key`의 앞부분으로도 쓰인다.
    pub enum AccrualKind("kind") {
        /// 1년 미만: 만 1개월마다 1일.
        Monthly => "monthly",
        /// 만 n년: 15일 + 가산.
        Annual => "annual",
    }
}

/// 있어야 하는 자동 부여 한 건. 서비스는 `grant_key`에 UNIQUE 제약을 두고 그대로 넣는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccrualGrant {
    /// `monthly:m`(m = 1..=11, 입사 후 만 m개월) / `annual:n`(n ≥ 1, 만 n년). 부여의 **차수**다.
    ///
    /// 날짜가 아니라 차수로 키를 만드는 이유: 관리자가 입사일을 바로잡으면 부여일이 전부
    /// 달라진다. 키에 날짜가 들어 있으면 그때 모든 키가 새것이 되어, 이미 받은 부여 옆에
    /// 한 벌이 통째로 더 들어간다(잔여 2배). 차수는 입사일이 바뀌어도 같으므로 "만 2년
    /// 연차"는 구성원당 한 번만 들어간다. 언제 계산해도 같은 키가 나오고 DB가 중복을 막는다.
    pub grant_key: String,
    pub kind: AccrualKind,
    pub amount: LeaveAmount,
    /// 부여일이자 사용 가능 시작일.
    pub granted_on: NaiveDate,
    /// 사용할 수 있는 마지막 날.
    pub expires_on: NaiveDate,
}

impl AccrualGrant {
    /// `ordinal`은 월 부여면 입사 후 개월 수, 연 부여면 근속 연수다.
    fn new(
        kind: AccrualKind,
        ordinal: u32,
        amount: LeaveAmount,
        granted_on: NaiveDate,
        expires_on: NaiveDate,
    ) -> Self {
        Self {
            grant_key: format!("{}:{ordinal}", kind.as_str()),
            kind,
            amount,
            granted_on,
            expires_on,
        }
    }

    /// 원장에 넣을 부여 항목.
    pub fn to_ledger_entry(&self) -> LedgerEntry {
        LedgerEntry {
            kind: LedgerEntryKind::Grant {
                valid_from: self.granted_on,
                expires_on: Some(self.expires_on),
            },
            amount: self.amount,
        }
    }
}

/// 만 `years_of_service`년이 된 날 생기는 연차 일수(근로기준법 제60조 제1·4항).
///
/// 15일에서 시작해 3년차부터 2년마다 1일씩 늘고 25일에서 멈춘다. 0년은 0일.
pub const fn annual_leave_days(years_of_service: u32) -> u32 {
    match years_of_service {
        0 => 0,
        // 25일 상한에 닿는 21년 이후는 계산하지 않아 큰 입력에서도 넘치지 않는다.
        n if n >= 21 => 25,
        n => 15 + (n - 1) / 2,
    }
}

/// `as_of`까지(당일 포함) 있어야 하는 모든 법정 연차 부여(입사일 기준).
///
/// - 1년 미만: 입사 후 만 m개월(m = 1..=11)이 되는 날마다 1일. 첫 기념일 전날 소멸한다.
/// - 만 n년(n ≥ 1)이 되는 날: [`annual_leave_days`]일. 다음 기념일 전날 소멸한다.
/// - 말일 입사는 달마다 입사일에서 직접 계산해 그 달의 말일로 맞춘다(1/31 → 2/28, 3/31).
///   2월 29일 입사자의 기념일은 평년에 2월 28일이다.
/// - 퇴직일 뒤의 날짜에는 부여하지 않는다. 퇴직일 당일의 부여는 포함한다.
///
/// 결과는 부여일 오름차순이고 `as_of`가 늦어져도 앞부분이 바뀌지 않는다. 그래서 주기적으로
/// 호출해 없는 키만 넣으면 된다. 키는 차수(`monthly:1..=11`, `annual:1..`)라서 입사일이
/// 달라져도 같은 차수의 키는 같다. 입사일을 고친 뒤 다시 호출하면 **아직 없는 차수만** 새
/// 입사일 기준 날짜로 들어가고, 이미 들어간 부여의 날짜·만료일은 받은 그대로 남는다. 근속 [`MAX_SERVICE_YEARS`]년 이후와, 만료일을 날짜로
/// 표현할 수 없는 부여는 만들지 않는다.
///
/// 출근율 조건(1개월 개근, 연 80% 이상 출근)은 MVP에서 다루지 않는다. 조건을 채우지 못한
/// 경우는 관리자가 음수 조정으로 바로잡는다.
pub fn accrual_grants(
    hired_on: NaiveDate,
    retired_on: Option<NaiveDate>,
    as_of: NaiveDate,
) -> Vec<AccrualGrant> {
    let mut grants = Vec::new();
    let limit = retired_on.map_or(as_of, |retired| retired.min(as_of));
    if limit < hired_on {
        return grants;
    }
    // 매번 입사일에서 직접 더한다. 앞 결과에 이어 더하면 말일 보정이 누적된다.
    let months_after = |months: u32| hired_on.checked_add_months(Months::new(months));
    let anniversary = |years: u32| years.checked_mul(12).and_then(months_after);
    let day_before = |date: Option<NaiveDate>| date.and_then(|d| d.pred_opt());

    let Some(first_year_end) = day_before(anniversary(1)) else {
        return grants;
    };
    for month in 1..=FIRST_YEAR_MONTHLY_GRANTS {
        match months_after(month) {
            Some(granted_on) if granted_on <= limit => grants.push(AccrualGrant::new(
                AccrualKind::Monthly,
                month,
                LeaveAmount(LeaveAmount::QUARTERS_PER_DAY),
                granted_on,
                first_year_end,
            )),
            _ => return grants,
        }
    }
    for years in 1..=MAX_SERVICE_YEARS {
        let (Some(granted_on), Some(expires_on)) =
            (anniversary(years), day_before(anniversary(years + 1)))
        else {
            break;
        };
        if granted_on > limit {
            break;
        }
        // 최대 25일이라 변환과 곱셈이 넘치지 않지만, 실패하면 부여하지 않는 쪽으로 둔다.
        let Some(amount) = i32::try_from(annual_leave_days(years))
            .ok()
            .and_then(LeaveAmount::from_days)
        else {
            break;
        };
        grants.push(AccrualGrant::new(
            AccrualKind::Annual,
            years,
            amount,
            granted_on,
            expires_on,
        ));
    }
    grants
}

// ───────────── 원장과 잔여 ─────────────

crate::string_enum! {
    /// 원장 항목 종류의 표기. DB(`leave_ledger.kind`)와 API(`LedgerEntryDto.kind`)가 같은 값을 쓴다.
    pub enum LedgerKind("kind") {
        Grant => "grant",
        Use => "use",
        Restore => "restore",
        Adjust => "adjust",
    }
}

/// 원장 항목의 종류. 원장은 추가만 하며 고치거나 지우지 않는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerEntryKind {
    /// 부여. `valid_from`부터 `expires_on`까지(양 끝 포함) 쓸 수 있다. 수량은 0 이상.
    /// `expires_on`이 없으면 소멸하지 않는다.
    Grant {
        valid_from: NaiveDate,
        expires_on: Option<NaiveDate>,
    },
    /// 신청에 따른 사용(예약 포함). 수량은 쓴 만큼의 양수.
    /// `effective_on`은 휴가의 첫 근무일([`LeaveSpan::first_day`])이다.
    Use {
        effective_on: NaiveDate,
        request: LeaveRequestId,
    },
    /// 해당 신청의 사용을 전부 무효로 한다(반려·취소). 수량은 기록용이며 계산에 쓰지 않는다.
    Restore { request: LeaveRequestId },
    /// 관리자의 수동 조정. 양수는 그날부터 유효하고 소멸하지 않는 부여,
    /// 음수는 그날의 사용과 같다.
    Adjust { effective_on: NaiveDate },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerEntry {
    pub kind: LedgerEntryKind,
    pub amount: LeaveAmount,
}

/// 유효 기간이 같은 부여를 합친 묶음.
struct Bucket {
    valid_from: NaiveDate,
    /// 소멸하지 않는 부여는 `NaiveDate::MAX`. 그날까지 유효한 부여와 구별할 필요가 없다.
    expires_on: NaiveDate,
    amount: i64,
    consumed: i64,
}

impl Bucket {
    fn is_valid_on(&self, date: NaiveDate) -> bool {
        self.valid_from <= date && date <= self.expires_on
    }

    /// `consumed <= amount`가 항상 성립하므로 넘치지 않는다.
    fn unconsumed(&self) -> i64 {
        self.amount - self.consumed
    }
}

/// 모든 사용을 부여에 배정한 결과.
struct Allocation {
    /// 차감 우선순위(만료 빠른 순, 같으면 시작 빠른 순)로 정렬돼 있다.
    buckets: Vec<Bucket>,
    /// 사용일에 유효한 부여가 모자라 어느 부여에서도 차감하지 못한 수량.
    uncovered: i64,
}

fn accumulate<K: Ord>(totals: &mut BTreeMap<K, i64>, key: K, quarters: i64) -> DomainResult<()> {
    let total = totals.entry(key).or_insert(0);
    *total = total.checked_add(quarters).ok_or(AMOUNT_OUT_OF_RANGE)?;
    Ok(())
}

/// 원장의 사용을 부여에 배정한다. 결과는 항목 순서와 무관하다.
///
/// 사용을 기준일이 빠른 것부터 처리하고, 각 사용은 그날 유효한 부여 중 만료가 가장 빠른
/// 것부터 차감한다. 이 순서는 소멸로 버려지는 수량을 가장 적게 만든다: 어떤 사용이
/// 고를 수 있는 부여들은 모두 이미 시작됐으므로, 만료가 늦은 쪽을 남겨 두면 뒤의 사용이
/// 쓸 수 있는 범위가 줄지 않는다.
fn allocate(entries: &[LedgerEntry]) -> DomainResult<Allocation> {
    let restored: BTreeSet<LeaveRequestId> = entries
        .iter()
        .filter_map(|entry| match entry.kind {
            LedgerEntryKind::Restore { request } => Some(request),
            _ => None,
        })
        .collect();

    // 키 순서 (만료일, 시작일) 가 곧 차감 우선순위다.
    let mut grants: BTreeMap<(NaiveDate, NaiveDate), i64> = BTreeMap::new();
    // 같은 날의 사용은 고를 수 있는 부여가 같으므로 합쳐도 결과가 같다.
    let mut uses: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    for entry in entries {
        let quarters = i64::from(entry.amount.quarters());
        match entry.kind {
            LedgerEntryKind::Grant {
                valid_from,
                expires_on,
            } => {
                if quarters < 0 {
                    return Err(AMOUNT_OUT_OF_RANGE);
                }
                let key = (expires_on.unwrap_or(NaiveDate::MAX), valid_from);
                accumulate(&mut grants, key, quarters)?;
            }
            LedgerEntryKind::Use {
                effective_on,
                request,
            } => {
                // 음수 사용은 공짜 잔여가 되므로, 복원된 신청의 것이라도 원장 오류로 본다.
                if quarters < 0 {
                    return Err(AMOUNT_OUT_OF_RANGE);
                }
                if !restored.contains(&request) {
                    accumulate(&mut uses, effective_on, quarters)?;
                }
            }
            LedgerEntryKind::Restore { .. } => {}
            LedgerEntryKind::Adjust { effective_on } if quarters >= 0 => {
                accumulate(&mut grants, (NaiveDate::MAX, effective_on), quarters)?;
            }
            LedgerEntryKind::Adjust { effective_on } => {
                // i32 를 i64 로 넓혔으므로 부호 반전은 넘치지 않는다.
                accumulate(&mut uses, effective_on, -quarters)?;
            }
        }
    }

    let mut buckets: Vec<Bucket> = grants
        .into_iter()
        .map(|((expires_on, valid_from), amount)| Bucket {
            valid_from,
            expires_on,
            amount,
            consumed: 0,
        })
        .collect();
    let mut uncovered = 0i64;
    for (effective_on, mut needed) in uses {
        for bucket in buckets
            .iter_mut()
            .filter(|bucket| bucket.is_valid_on(effective_on))
        {
            if needed == 0 {
                break;
            }
            // taken 은 needed 와 남은 수량 이하이므로 아래 덧셈·뺄셈은 넘치지 않는다.
            let taken = needed.min(bucket.unconsumed());
            bucket.consumed += taken;
            needed -= taken;
        }
        uncovered = uncovered.checked_add(needed).ok_or(AMOUNT_OUT_OF_RANGE)?;
    }
    Ok(Allocation { buckets, uncovered })
}

fn to_amount(quarters: i64) -> DomainResult<LeaveAmount> {
    i32::try_from(quarters)
        .map(LeaveAmount)
        .map_err(|_| AMOUNT_OUT_OF_RANGE)
}

/// 특정 날짜 기준의 휴가 잔여. 항상 `granted − used − expired == remaining`이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Balance {
    /// 기준일까지 시작된 부여(양수 조정 포함)의 합. 이미 소멸한 것도 포함한다.
    pub granted: LeaveAmount,
    /// 위 부여에서 차감된 수량 + 어느 부여에서도 차감하지 못한 사용.
    /// 기준일 뒤의 휴가도 이미 예약된 것이므로 포함한다. 다만 아직 시작되지 않은
    /// 부여에서 차감된 몫은 그 부여가 `granted`에 들어올 때 함께 잡힌다.
    pub used: LeaveAmount,
    /// 기준일 전에 만료된 부여 중 쓰지 못하고 사라진 수량.
    pub expired: LeaveAmount,
    /// 기준일에 유효한 부여의 남은 수량 − 차감하지 못한 사용. 음수일 수 있다.
    pub remaining: LeaveAmount,
}

impl Balance {
    /// 원장 전체로 `on` 기준 잔여를 계산한다. 항목 순서는 결과에 영향을 주지 않는다.
    ///
    /// - 부여는 만료일 당일까지 유효하고 다음 날부터 소멸로 잡힌다.
    /// - 사용은 기준일에 유효한 부여에서만, 만료가 빠른 것부터 차감한다([`allocate`]).
    /// - 복원은 같은 신청의 사용을 전부 무효로 한다. 짝이 없는 복원과 중복 복원은
    ///   아무 효과가 없다. 그래서 복원을 몇 번 기록해도 잔여가 늘지 않는다.
    /// - 유효한 부여가 모자란 사용(주로 음수 조정)은 사라지지 않고 **부채**로 남아
    ///   `remaining`을 음수로 만들 수 있다. 부채는 이후의 부여와 상계되어 보일 뿐,
    ///   그 부여를 소비하지는 않는다.
    /// - 부여·사용의 음수 수량과 `i32`를 넘는 합계는
    ///   `Validation { field: "amount", OutOfRange }`다.
    pub fn compute(entries: &[LedgerEntry], on: NaiveDate) -> DomainResult<Self> {
        let allocation = allocate(entries)?;
        let add = |total: i64, value: i64| total.checked_add(value).ok_or(AMOUNT_OUT_OF_RANGE);
        let (mut granted, mut used, mut expired) = (0i64, allocation.uncovered, 0i64);
        for bucket in allocation
            .buckets
            .iter()
            .filter(|bucket| bucket.valid_from <= on)
        {
            granted = add(granted, bucket.amount)?;
            used = add(used, bucket.consumed)?;
            if bucket.expires_on < on {
                expired = add(expired, bucket.unconsumed())?;
            }
        }
        let remaining = granted
            .checked_sub(used)
            .and_then(|rest| rest.checked_sub(expired))
            .ok_or(AMOUNT_OUT_OF_RANGE)?;
        Ok(Self {
            granted: to_amount(granted)?,
            used: to_amount(used)?,
            expired: to_amount(expired)?,
            remaining: to_amount(remaining)?,
        })
    }
}

/// `leave_start`에 시작하는 휴가로 신청할 수 있는 최대 수량(0 이상).
///
/// 그날 유효한 부여의 남은 수량에서 부채를 뺀 값, 즉 그날 기준 `remaining`이다(음수면 0).
/// 이미 기록된 사용은 날짜가 더 늦은 것까지 모두 먼저 배정하므로, 뒤 날짜의 휴가가
/// 같은 부여를 쓰고 있으면 그만큼 줄어든다.
///
/// 이 수량 이하를 `leave_start` 기준 사용으로 기록하면 어느 날짜의 잔여도 기록한 수량보다
/// 더 줄지 않고, 새 부채가 생기지 않는다.
pub fn available_for(entries: &[LedgerEntry], leave_start: NaiveDate) -> DomainResult<LeaveAmount> {
    Ok(Balance::compute(entries, leave_start)?
        .remaining
        .max(LeaveAmount::ZERO))
}

/// 신청 수량이 잔여 안에 들어오는지 확인한다. 잔여와 같으면 통과한다.
///
/// 0 이하의 신청 수량은 잔여와 무관하게 `Validation { field: "amount", OutOfRange }`다.
pub fn check_sufficient(
    entries: &[LedgerEntry],
    leave_start: NaiveDate,
    requested: LeaveAmount,
) -> DomainResult<()> {
    let available = available_for(entries, leave_start)?;
    if requested <= LeaveAmount::ZERO {
        return Err(AMOUNT_OUT_OF_RANGE);
    }
    if requested > available {
        return Err(ConflictReason::InsufficientLeaveBalance.into());
    }
    Ok(())
}

/// 신청한 근무일 **하루하루가** 그날 유효한 부여로 덮이는지 확인한다.
///
/// 이미 기록된 사용을 모두 배정한 뒤([`allocate`]), 근무일을 날짜순으로 하나씩, 그날 유효한
/// 부여 중 만료가 빠른 것부터 하루치(`unit`)를 차감해 본다. 어느 날이든 모자라면
/// `InsufficientLeaveBalance`다. 아무것도 기록하지 않는 **검사**다.
///
/// [`check_sufficient`]는 휴가의 첫날 하루만 본다. 그것만으로는 첫날에 유효한 부여가 넉넉하면,
/// 그 부여가 만료된 뒤의 날짜까지(다음 부여가 아직 없는데도) 걸친 신청이 통과한다. 이 함수는
/// 그런 신청, 곧 쉴 권리가 없는 날이 섞인 신청을 막는다.
///
/// `working_dates`는 실제로 쉬는 근무일([`LeaveSpan::working_dates`])이며 순서는 상관없다.
/// 비어 있으면 `Validation { field: "amount", OutOfRange }`다.
pub fn check_span_covered(
    entries: &[LedgerEntry],
    working_dates: &[NaiveDate],
    unit: LeaveUnit,
) -> DomainResult<()> {
    if working_dates.is_empty() {
        return Err(AMOUNT_OUT_OF_RANGE);
    }
    let per_day = i64::from(unit.amount_per_day().quarters());
    let mut buckets = allocate(entries)?.buckets;
    let mut dates = working_dates.to_vec();
    dates.sort_unstable();
    for date in dates {
        let mut needed = per_day;
        for bucket in buckets.iter_mut().filter(|bucket| bucket.is_valid_on(date)) {
            if needed == 0 {
                break;
            }
            // taken 은 needed 와 남은 수량 이하이므로 넘치지 않는다.
            let taken = needed.min(bucket.unconsumed());
            bucket.consumed += taken;
            needed -= taken;
        }
        if needed > 0 {
            return Err(ConflictReason::InsufficientLeaveBalance.into());
        }
    }
    Ok(())
}

/// 차감하는 휴가 신청을 접수(예약)할 수 있는지. 두 조건을 **모두** 만족해야 한다.
///
/// 1. [`check_sufficient`]: 총 수량이 **첫 근무일** 기준 잔여 안에 든다.
/// 2. [`check_span_covered`]: 근무일 하루하루가 그날 유효한 부여로 덮인다.
///
/// # 예약을 한 줄로 두는 이유와 그 한계
///
/// 예약은 신청당 원장 한 줄(`Use { effective_on: 첫 근무일, amount: 총 수량 }`)이다. 부여
/// 구간마다 나눠 적지 않는다. 그래도 안전한 까닭:
///
/// - 조건 1 덕분에 그 한 줄은 첫 근무일에 유효한 부여만으로 전부 차감된다. 그래서 새 부채가
///   생기지 않고, 어느 날짜의 잔여도 기록한 수량보다 더 줄지 않는다([`available_for`]의 약속).
///   신청 구간의 어느 날짜를 기준으로 [`Balance::compute`]를 다시 해도, 접수 전에 0 이상이던
///   잔여가 음수가 되지 않는다(속성 테스트 `reservable_span_never_creates_debt`).
/// - 조건 2 덕분에 쉬는 날마다 그날 유효한 부여가 실제로 남아 있다. 만료된 뒤이거나 아직
///   부여받지 않은 날에 쉬는 신청은 접수되지 않는다.
///
/// 남는 한계(의도된 것): 부여 경계(연차 기념일 등)를 넘는 신청은 **경계 뒤의 날짜도 첫날에
/// 유효하던 부여에서** 차감된다. 조건 2의 날짜순 차감은 자격을 따지는 계산일 뿐 기록에는
/// 쓰이지 않는다. 따라서
///
/// - 첫날의 부여만으로 전체를 낼 수 없으면, 경계 뒤의 새 부여가 넉넉해도 거부된다. 신청자는
///   경계에서 둘로 나눠 신청하면 된다(뒤쪽 신청은 그 첫날 기준으로 새 부여에서 차감된다).
/// - 경계를 넘는 신청이 통과하면 뒤쪽 날짜의 몫만큼 새 부여가 덜 줄어든다(곧 소멸할 부여가
///   대신 쓰인다). 그 날짜들에 새 부여가 실제로 남아 있었음은 조건 2가 보장한다.
pub fn check_reservable(entries: &[LedgerEntry], span: &LeaveSpan) -> DomainResult<()> {
    check_sufficient(entries, span.first_day(), span.amount())?;
    check_span_covered(entries, span.working_dates(), span.unit())
}

// ───────────── 신청 상태 ─────────────

crate::string_enum! {
    /// 휴가 신청의 상태. 신청 시점에 잔여를 예약(차감)하고 `Pending`이 된다.
    pub enum LeaveRequestStatus("status") {
        Pending => "pending",
        Approved => "approved",
        Rejected => "rejected",
        Canceled => "canceled",
    }
}

/// 취소 요청의 처리 방법.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelDecision {
    /// `Canceled`로 바꾼다. `restore_balance`가 참이면 같은 트랜잭션에서 복원 항목을 넣는다.
    Cancel { restore_balance: bool },
    /// 이미 취소됨. 아무것도 바꾸지 않는다(복원 항목도 다시 넣지 않는다).
    AlreadyCanceled,
}

impl CancelDecision {
    /// 예약한 잔여를 복원해야 하는지.
    pub const fn restore_balance(self) -> bool {
        match self {
            Self::Cancel { restore_balance } => restore_balance,
            Self::AlreadyCanceled => false,
        }
    }
}

/// 취소할 수 있는지와 그 효과를 정한다.
///
/// - 대기 중: 누구든 언제든 취소한다.
/// - 승인됨: 본인은 휴가 시작 전(`today < leave_start`)에만, 관리자는 언제든.
///   `leave_start`는 [`LeaveSpan::first_day`]다.
/// - 이미 취소됨: 같은 요청이 다시 와도 성공으로 답하되 아무것도 하지 않는다.
/// - 반려됨: 이미 끝난 신청이다.
pub fn decide_cancel(
    status: LeaveRequestStatus,
    leave_start: NaiveDate,
    today: NaiveDate,
    by_admin: bool,
) -> DomainResult<CancelDecision> {
    // 대기·승인 상태는 모두 잔여를 예약하고 있으므로 취소하면 반드시 복원한다.
    let cancel = CancelDecision::Cancel {
        restore_balance: true,
    };
    match status {
        LeaveRequestStatus::Pending => Ok(cancel),
        LeaveRequestStatus::Approved if by_admin || today < leave_start => Ok(cancel),
        LeaveRequestStatus::Approved => Err(ConflictReason::LeaveAlreadyStarted.into()),
        LeaveRequestStatus::Canceled => Ok(CancelDecision::AlreadyCanceled),
        LeaveRequestStatus::Rejected => Err(ConflictReason::DocumentAlreadyClosed.into()),
    }
}

/// 승인·반려 요청의 처리 방법.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionDecision {
    /// `status`로 바꾼다. `restore_balance`가 참이면 같은 트랜잭션에서 복원 항목을 넣는다.
    Resolve {
        status: LeaveRequestStatus,
        restore_balance: bool,
    },
    /// 이미 같은 결과로 처리됨. 아무것도 바꾸지 않는다.
    AlreadyResolved,
}

impl ResolutionDecision {
    /// 예약한 잔여를 복원해야 하는지.
    pub const fn restore_balance(self) -> bool {
        match self {
            Self::Resolve {
                restore_balance, ..
            } => restore_balance,
            Self::AlreadyResolved => false,
        }
    }
}

/// 승인(`approve = true`) 또는 반려의 효과를 정한다.
///
/// 승인은 신청할 때 예약한 차감을 그대로 확정하고, 반려는 복원한다. 같은 결과의 재요청은
/// 아무것도 하지 않는다. 반대 결과로 뒤집거나 취소된 신청을 처리하려는 요청은 거부한다.
pub fn decide_resolution(
    status: LeaveRequestStatus,
    approve: bool,
) -> DomainResult<ResolutionDecision> {
    use LeaveRequestStatus::{Approved, Canceled, Pending, Rejected};
    match (status, approve) {
        (Pending, true) => Ok(ResolutionDecision::Resolve {
            status: Approved,
            restore_balance: false,
        }),
        (Pending, false) => Ok(ResolutionDecision::Resolve {
            status: Rejected,
            restore_balance: true,
        }),
        (Approved, true) | (Rejected, false) => Ok(ResolutionDecision::AlreadyResolved),
        (Approved, false) | (Rejected, true) | (Canceled, _) => {
            Err(ConflictReason::DocumentAlreadyClosed.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::WorkWeek;
    use chrono::Duration;
    use proptest::prelude::*;

    const AMOUNT_ERR: DomainError = DomainError::validation("amount", ValidationReason::OutOfRange);

    fn d(value: &str) -> NaiveDate {
        value.parse().expect("date")
    }

    fn q(quarters: i32) -> LeaveAmount {
        LeaveAmount::from_quarters(quarters)
    }

    fn req(n: u128) -> LeaveRequestId {
        LeaveRequestId::from_uuid(uuid::Uuid::from_u128(n))
    }

    fn calendar(holidays: &[&str]) -> WorkCalendar {
        WorkCalendar::new(WorkWeek::MON_TO_FRI, holidays.iter().map(|h| d(h)))
    }

    fn range(start: &str, end: &str) -> DateRange {
        DateRange::new(d(start), d(end)).expect("range")
    }

    fn grant_on(
        valid_from: NaiveDate,
        expires_on: Option<NaiveDate>,
        quarters: i32,
    ) -> LedgerEntry {
        LedgerEntry {
            kind: LedgerEntryKind::Grant {
                valid_from,
                expires_on,
            },
            amount: q(quarters),
        }
    }

    fn grant(valid_from: &str, expires_on: Option<&str>, quarters: i32) -> LedgerEntry {
        grant_on(d(valid_from), expires_on.map(d), quarters)
    }

    fn use_on(effective_on: NaiveDate, quarters: i32, request: u128) -> LedgerEntry {
        LedgerEntry {
            kind: LedgerEntryKind::Use {
                effective_on,
                request: req(request),
            },
            amount: q(quarters),
        }
    }

    fn used(effective_on: &str, quarters: i32, request: u128) -> LedgerEntry {
        use_on(d(effective_on), quarters, request)
    }

    fn restore(request: u128, quarters: i32) -> LedgerEntry {
        LedgerEntry {
            kind: LedgerEntryKind::Restore {
                request: req(request),
            },
            amount: q(quarters),
        }
    }

    fn adjust_on(effective_on: NaiveDate, quarters: i32) -> LedgerEntry {
        LedgerEntry {
            kind: LedgerEntryKind::Adjust { effective_on },
            amount: q(quarters),
        }
    }

    fn adjust(effective_on: &str, quarters: i32) -> LedgerEntry {
        adjust_on(d(effective_on), quarters)
    }

    /// (granted, used, expired, remaining) 을 1/4일 단위로.
    fn balance(entries: &[LedgerEntry], on: &str) -> (i32, i32, i32, i32) {
        let b = Balance::compute(entries, d(on)).expect("balance");
        (
            b.granted.quarters(),
            b.used.quarters(),
            b.expired.quarters(),
            b.remaining.quarters(),
        )
    }

    fn available(entries: &[LedgerEntry], leave_start: &str) -> i32 {
        available_for(entries, d(leave_start))
            .expect("available")
            .quarters()
    }

    // ───────────── 수량·단위 ─────────────

    #[test]
    fn amount_displays_in_days() {
        let cases = [
            (0, "0"),
            (4, "1"),
            (2, "0.5"),
            (1, "0.25"),
            (7, "1.75"),
            (60, "15"),
            (-9, "-2.25"),
        ];
        for (quarters, expected) in cases {
            assert_eq!(LeaveAmount::from_quarters(quarters).to_string(), expected);
        }
        assert_eq!(
            LeaveAmount::from_quarters(i32::MIN).to_string(),
            "-536870912"
        );
    }

    #[test]
    fn amount_arithmetic_is_checked() {
        assert_eq!(
            LeaveAmount::from_days(15),
            Some(LeaveAmount::from_quarters(60))
        );
        assert_eq!(LeaveAmount::from_days(i32::MAX), None);
        let max = LeaveAmount::from_quarters(i32::MAX);
        assert_eq!(max.checked_add(LeaveAmount::from_quarters(1)), None);
        assert_eq!(LeaveAmount::from_quarters(i32::MIN).checked_neg(), None);
    }

    #[test]
    fn unit_round_trip_and_amounts() {
        for &unit in LeaveUnit::ALL {
            assert_eq!(LeaveUnit::parse(unit.as_str()), Ok(unit));
        }
        assert!(LeaveUnit::parse("half").is_err());
        assert_eq!(LeaveUnit::FullDay.amount_per_day().quarters(), 4);
        assert_eq!(LeaveUnit::PmHalf.amount_per_day().quarters(), 2);
        assert_eq!(LeaveUnit::AmQuarter.amount_per_day().quarters(), 1);
        assert!(!LeaveUnit::FullDay.is_partial() && LeaveUnit::AmHalf.is_partial());
    }

    // ───────────── 신청 크기 ─────────────

    #[test]
    fn span_counts_working_days_of_a_full_week() {
        // 2026-03-02(월) ~ 03-06(금)
        let span = LeaveSpan::new(
            range("2026-03-02", "2026-03-06"),
            LeaveUnit::FullDay,
            &calendar(&[]),
        )
        .expect("span");
        assert_eq!(span.amount(), q(20));
        assert_eq!(span.working_dates().len(), 5);
        assert_eq!(span.first_day(), d("2026-03-02"));
        assert_eq!(span.unit(), LeaveUnit::FullDay);
        assert_eq!(span.range(), range("2026-03-02", "2026-03-06"));
    }

    #[test]
    fn span_skips_weekend_and_holiday_inside_range() {
        // 금 ~ 다음 주 수, 화요일이 공휴일 → 금·월·수
        let cal = calendar(&["2026-03-10"]);
        let span = LeaveSpan::new(range("2026-03-06", "2026-03-11"), LeaveUnit::FullDay, &cal)
            .expect("span");
        assert_eq!(
            span.working_dates(),
            [d("2026-03-06"), d("2026-03-09"), d("2026-03-11")]
        );
        assert_eq!(span.amount(), q(12));
    }

    #[test]
    fn span_fully_on_weekend_or_holiday_has_no_working_days() {
        let no_days: DomainError = ConflictReason::LeaveNoWorkingDays.into();
        let cal = calendar(&["2026-03-04"]);
        let weekend = LeaveSpan::new(range("2026-03-07", "2026-03-08"), LeaveUnit::FullDay, &cal);
        assert_eq!(weekend, Err(no_days.clone()));
        let holiday = LeaveSpan::new(DateRange::single(d("2026-03-04")), LeaveUnit::FullDay, &cal);
        assert_eq!(holiday, Err(no_days.clone()));
        let half_on_holiday =
            LeaveSpan::new(DateRange::single(d("2026-03-04")), LeaveUnit::AmHalf, &cal);
        assert_eq!(half_on_holiday, Err(no_days));
    }

    #[test]
    fn span_partial_units_require_a_single_day() {
        let cal = calendar(&[]);
        for unit in LeaveUnit::ALL.iter().copied().filter(|u| u.is_partial()) {
            assert_eq!(
                LeaveSpan::new(range("2026-03-02", "2026-03-03"), unit, &cal),
                Err(DomainError::validation(
                    "unit",
                    ValidationReason::Inconsistent
                )),
                "{unit:?}"
            );
            let single =
                LeaveSpan::new(DateRange::single(d("2026-03-02")), unit, &cal).expect("single");
            assert_eq!(single.amount(), unit.amount_per_day());
            assert_eq!(single.working_dates(), [d("2026-03-02")]);
        }
    }

    #[test]
    fn span_shape_error_wins_over_no_working_days() {
        // 주말 이틀에 반차: 단위 오류를 먼저 알려야 사용자가 고칠 수 있다.
        let result = LeaveSpan::new(
            range("2026-03-07", "2026-03-08"),
            LeaveUnit::PmHalf,
            &calendar(&[]),
        );
        assert_eq!(
            result,
            Err(DomainError::validation(
                "unit",
                ValidationReason::Inconsistent
            ))
        );
    }

    #[test]
    fn span_across_year_end() {
        // 2026-12-30(수) ~ 2027-01-04(월), 신정(금) 공휴일 → 수·목·월
        let cal = calendar(&["2027-01-01"]);
        let span = LeaveSpan::new(range("2026-12-30", "2027-01-04"), LeaveUnit::FullDay, &cal)
            .expect("span");
        assert_eq!(
            span.working_dates(),
            [d("2026-12-30"), d("2026-12-31"), d("2027-01-04")]
        );
        assert_eq!(span.amount(), q(12));
    }

    #[test]
    fn span_first_day_is_first_working_day_not_range_start() {
        // 토 ~ 월: 실제 휴가는 월요일에 시작한다.
        let span = LeaveSpan::new(
            range("2026-03-07", "2026-03-09"),
            LeaveUnit::FullDay,
            &calendar(&[]),
        )
        .expect("span");
        assert_eq!(span.first_day(), d("2026-03-09"));
        assert_eq!(span.amount(), q(4));
    }

    #[test]
    fn span_of_maximum_range_does_not_overflow() {
        let all_days = WorkCalendar::new(WorkWeek::from_bits(0b111_1111).expect("week"), []);
        let span = LeaveSpan::new(
            range("2026-01-01", "2027-01-01"),
            LeaveUnit::FullDay,
            &all_days,
        )
        .expect("span");
        assert_eq!(span.amount(), q(366 * 4));
    }

    // ───────────── 겹침 ─────────────

    #[test]
    fn overlap_of_partial_units_on_the_same_day() {
        use LeaveUnit::*;
        let compatible = [
            (AmHalf, PmHalf),
            (AmQuarter, PmQuarter),
            (AmQuarter, PmHalf),
            (PmQuarter, AmHalf),
        ];
        let day = DateRange::single(d("2026-03-02"));
        for &a in LeaveUnit::ALL {
            for &b in LeaveUnit::ALL {
                let expected_free = compatible.contains(&(a, b)) || compatible.contains(&(b, a));
                assert_eq!(
                    requests_overlap((day, a), (day, b)),
                    !expected_free,
                    "{a:?} + {b:?}"
                );
            }
        }
    }

    #[test]
    fn overlap_named_collisions() {
        use LeaveUnit::*;
        let day = DateRange::single(d("2026-03-02"));
        assert!(requests_overlap((day, AmHalf), (day, AmQuarter)));
        assert!(requests_overlap((day, PmHalf), (day, PmQuarter)));
        assert!(
            requests_overlap((day, AmHalf), (day, AmHalf)),
            "같은 단위 두 번"
        );
        assert!(requests_overlap((day, PmQuarter), (day, PmQuarter)));
        for &unit in LeaveUnit::ALL {
            assert!(
                requests_overlap((day, FullDay), (day, unit)),
                "종일 + {unit:?}"
            );
        }
    }

    #[test]
    fn overlap_requires_a_shared_date() {
        use LeaveUnit::*;
        let week = range("2026-03-02", "2026-03-06");
        let next = range("2026-03-07", "2026-03-10");
        let touching = range("2026-03-06", "2026-03-09");
        assert!(!requests_overlap((week, FullDay), (next, FullDay)));
        assert!(
            requests_overlap((week, FullDay), (touching, FullDay)),
            "끝 날짜 공유"
        );
        let friday = DateRange::single(d("2026-03-06"));
        assert!(requests_overlap((week, FullDay), (friday, PmQuarter)));
        assert!(!requests_overlap(
            (friday, AmHalf),
            (DateRange::single(d("2026-03-09")), AmHalf)
        ));
    }

    #[test]
    fn overlap_treats_malformed_multi_day_partial_as_conflict() {
        // LeaveSpan 검증을 거치지 않은 값이 들어와도 겹침을 놓치지 않는다.
        use LeaveUnit::*;
        let two_days = range("2026-03-02", "2026-03-03");
        let day = DateRange::single(d("2026-03-02"));
        assert!(requests_overlap((two_days, AmHalf), (day, PmHalf)));
        assert!(requests_overlap((day, PmHalf), (two_days, AmHalf)));
    }

    // ───────────── 연차 부여 ─────────────

    fn monthly_dates(grants: &[AccrualGrant]) -> Vec<NaiveDate> {
        grants
            .iter()
            .filter(|g| g.kind == AccrualKind::Monthly)
            .map(|g| g.granted_on)
            .collect()
    }

    fn annual(grants: &[AccrualGrant]) -> Vec<&AccrualGrant> {
        grants
            .iter()
            .filter(|g| g.kind == AccrualKind::Annual)
            .collect()
    }

    #[test]
    fn annual_days_step_every_two_years_up_to_25() {
        let cases = [
            (0, 0),
            (1, 15),
            (2, 15),
            (3, 16),
            (4, 16),
            (5, 17),
            (19, 24),
            (20, 24),
            (21, 25),
            (22, 25),
            (40, 25),
            (u32::MAX, 25),
        ];
        for (years, days) in cases {
            assert_eq!(annual_leave_days(years), days, "{years}년차");
        }
    }

    #[test]
    fn accrual_first_month_boundary() {
        let hired = d("2024-03-15");
        assert!(
            accrual_grants(hired, None, d("2024-03-15")).is_empty(),
            "입사 당일"
        );
        assert!(
            accrual_grants(hired, None, d("2024-04-14")).is_empty(),
            "만 1개월 전날"
        );
        let grants = accrual_grants(hired, None, d("2024-04-15"));
        assert_eq!(
            grants,
            vec![AccrualGrant {
                grant_key: "monthly:1".to_owned(),
                kind: AccrualKind::Monthly,
                amount: q(4),
                granted_on: d("2024-04-15"),
                expires_on: d("2025-03-14"),
            }]
        );
    }

    #[test]
    fn accrual_first_anniversary_boundary() {
        let hired = d("2024-03-15");
        let before = accrual_grants(hired, None, d("2025-03-14"));
        assert_eq!(before.len(), 11);
        assert!(annual(&before).is_empty());
        assert!(before
            .iter()
            .all(|g| g.amount == q(4) && g.expires_on == d("2025-03-14")));
        assert_eq!(before.last().map(|g| g.granted_on), Some(d("2025-02-15")));

        let on = accrual_grants(hired, None, d("2025-03-15"));
        assert_eq!(on.len(), 12);
        let first = annual(&on)[0];
        assert_eq!(first.grant_key, "annual:1");
        assert_eq!(first.amount, q(60));
        assert_eq!(first.granted_on, d("2025-03-15"));
        assert_eq!(first.expires_on, d("2026-03-14"));
    }

    #[test]
    fn accrual_amount_steps_over_long_service() {
        let grants = accrual_grants(d("2000-01-03"), None, d("2040-01-03"));
        let annual = annual(&grants);
        assert_eq!(annual.len(), 40);
        let days = |n: usize| annual[n - 1].amount.quarters() / 4;
        assert_eq!(
            (days(1), days(2), days(3), days(4), days(5)),
            (15, 15, 16, 16, 17)
        );
        assert_eq!((days(20), days(21), days(22), days(40)), (24, 25, 25, 25));
        assert_eq!(annual[20].granted_on, d("2021-01-03"));
        assert_eq!(annual[20].expires_on, d("2022-01-02"));
        // 만 40년 전날에는 40번째가 없다.
        assert_eq!(
            annual.len() - 1,
            self::annual(&accrual_grants(d("2000-01-03"), None, d("2040-01-02"))).len()
        );
    }

    #[test]
    fn accrual_feb_29_hire_uses_feb_28_in_common_years() {
        let grants = accrual_grants(d("2024-02-29"), None, d("2028-02-29"));
        let monthly = monthly_dates(&grants);
        assert_eq!(monthly.first(), Some(&d("2024-03-29")));
        assert_eq!(monthly.last(), Some(&d("2025-01-29")));
        assert!(grants
            .iter()
            .filter(|g| g.kind == AccrualKind::Monthly)
            .all(|g| g.expires_on == d("2025-02-27")));
        let windows: Vec<_> = annual(&grants)
            .iter()
            .map(|g| (g.granted_on, g.expires_on))
            .collect();
        assert_eq!(
            windows,
            vec![
                (d("2025-02-28"), d("2026-02-27")),
                (d("2026-02-28"), d("2027-02-27")),
                (d("2027-02-28"), d("2028-02-28")),
                (d("2028-02-29"), d("2029-02-27")),
            ]
        );
        // 평년 2월 28일이 기념일이고, 27일에는 아직 아니다.
        assert_eq!(
            annual(&accrual_grants(d("2024-02-29"), None, d("2025-02-27"))).len(),
            0
        );
        assert_eq!(
            annual(&accrual_grants(d("2024-02-29"), None, d("2025-02-28"))).len(),
            1
        );
    }

    #[test]
    fn accrual_monthly_dates_clamp_from_hire_date_not_by_chaining() {
        let jan31 = accrual_grants(d("2025-01-31"), None, d("2025-12-31"));
        let expected: Vec<NaiveDate> = [
            "2025-02-28",
            "2025-03-31",
            "2025-04-30",
            "2025-05-31",
            "2025-06-30",
            "2025-07-31",
            "2025-08-31",
            "2025-09-30",
            "2025-10-31",
            "2025-11-30",
            "2025-12-31",
        ]
        .into_iter()
        .map(d)
        .collect();
        assert_eq!(monthly_dates(&jan31), expected);
        assert!(jan31.iter().all(|g| g.expires_on == d("2026-01-30")));

        let leap = accrual_grants(d("2024-01-31"), None, d("2024-03-31"));
        assert_eq!(monthly_dates(&leap), vec![d("2024-02-29"), d("2024-03-31")]);

        let aug31 = accrual_grants(d("2025-08-31"), None, d("2026-08-30"));
        let expected: Vec<NaiveDate> = [
            "2025-09-30",
            "2025-10-31",
            "2025-11-30",
            "2025-12-31",
            "2026-01-31",
            "2026-02-28",
            "2026-03-31",
            "2026-04-30",
            "2026-05-31",
            "2026-06-30",
            "2026-07-31",
        ]
        .into_iter()
        .map(d)
        .collect();
        assert_eq!(monthly_dates(&aug31), expected);
        assert!(aug31.iter().all(|g| g.expires_on == d("2026-08-30")));
    }

    #[test]
    fn accrual_stops_at_retirement() {
        let hired = d("2026-01-10");
        let far = d("2030-01-01");
        let before_fourth = accrual_grants(hired, Some(d("2026-05-09")), far);
        assert_eq!(
            monthly_dates(&before_fourth),
            vec![d("2026-02-10"), d("2026-03-10"), d("2026-04-10")]
        );
        assert!(annual(&before_fourth).is_empty());
        // 퇴직일 당일의 부여는 포함한다.
        assert_eq!(accrual_grants(hired, Some(d("2026-05-10")), far).len(), 4);
        // 퇴직일이 미래여도 as_of 를 넘지 않는다.
        assert_eq!(accrual_grants(hired, Some(far), d("2026-03-10")).len(), 2);
    }

    #[test]
    fn accrual_empty_for_inverted_inputs() {
        let hired = d("2026-01-10");
        assert!(accrual_grants(hired, None, d("2026-01-09")).is_empty());
        assert!(accrual_grants(hired, None, NaiveDate::MIN).is_empty());
        assert!(accrual_grants(hired, Some(d("2025-12-31")), d("2030-01-01")).is_empty());
        assert!(accrual_grants(hired, Some(NaiveDate::MIN), NaiveDate::MAX).is_empty());
    }

    #[test]
    fn accrual_is_bounded_for_absurd_inputs() {
        let year_one = NaiveDate::from_ymd_opt(1, 1, 1).expect("date");
        let grants = accrual_grants(year_one, None, d("9999-12-31"));
        assert_eq!(grants.len(), 11 + MAX_SERVICE_YEARS as usize);
        assert_eq!(grants.last().map(|g| g.amount), Some(q(100)));

        assert_eq!(
            accrual_grants(NaiveDate::MIN, None, NaiveDate::MAX).len(),
            11 + 80
        );
        assert_eq!(
            accrual_grants(NaiveDate::MIN, Some(NaiveDate::MAX), NaiveDate::MAX).len(),
            11 + 80
        );
        // 만료일을 표현할 수 없는 부여는 만들지 않는다.
        assert!(accrual_grants(NaiveDate::MAX, None, NaiveDate::MAX).is_empty());
        let near_end = NaiveDate::MAX - Duration::days(400);
        let grants = accrual_grants(near_end, None, NaiveDate::MAX);
        assert_eq!(grants.len(), 11);
        assert!(annual(&grants).is_empty());
    }

    /// 키는 차수다: 입사일이 며칠·몇 년 달라도 같은 차수의 부여는 같은 키를 가진다.
    #[test]
    fn accrual_keys_are_ordinals_independent_of_the_hire_date() {
        let keys = |hired: &str, as_of: &str| -> Vec<String> {
            accrual_grants(d(hired), None, d(as_of))
                .into_iter()
                .map(|g| g.grant_key)
                .collect()
        };
        let expected: Vec<String> = (1..=11)
            .map(|m| format!("monthly:{m}"))
            .chain((1..=2).map(|n| format!("annual:{n}")))
            .collect();
        assert_eq!(keys("2024-03-02", "2026-03-02"), expected);
        // 하루 앞·뒤로 고친 입사일, 말일·윤일 입사도 같은 차수는 같은 키다.
        assert_eq!(keys("2024-03-01", "2026-03-01"), expected);
        assert_eq!(keys("2024-03-03", "2026-03-03"), expected);
        assert_eq!(keys("2024-01-31", "2026-01-31"), expected);
        assert_eq!(keys("2024-02-29", "2026-02-28"), expected);
        assert_eq!(keys("1990-07-15", "1992-07-15"), expected);
        // 같은 조회일이면 하루 늦은 입사일은 마지막 차수가 아직 없을 뿐, 앞부분은 같다.
        assert_eq!(keys("2024-03-03", "2026-03-02"), expected[..12]);
    }

    #[test]
    fn accrual_keys_are_unique_and_ordered() {
        let grants = accrual_grants(d("2019-08-31"), None, d("2026-11-20"));
        let keys: BTreeSet<&str> = grants.iter().map(|g| g.grant_key.as_str()).collect();
        assert_eq!(keys.len(), grants.len());
        assert!(grants.windows(2).all(|w| w[0].granted_on < w[1].granted_on));
        assert!(grants.iter().all(|g| g.granted_on <= g.expires_on));
    }

    #[test]
    fn accrual_grants_feed_the_ledger() {
        let hired = d("2024-03-15");
        let entries: Vec<LedgerEntry> = accrual_grants(hired, None, d("2025-03-15"))
            .iter()
            .map(AccrualGrant::to_ledger_entry)
            .collect();
        // 첫 기념일 전날: 월 단위 11일이 아직 유효.
        assert_eq!(balance(&entries, "2025-03-14"), (44, 0, 0, 44));
        // 첫 기념일: 월 단위는 소멸하고 15일이 생긴다.
        assert_eq!(balance(&entries, "2025-03-15"), (104, 0, 44, 60));
    }

    // ───────────── 원장·잔여 ─────────────

    #[test]
    fn balance_of_empty_ledger_is_zero() {
        assert_eq!(balance(&[], "2026-01-01"), (0, 0, 0, 0));
        assert_eq!(available(&[], "2026-01-01"), 0);
    }

    #[test]
    fn balance_happy_path() {
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2026-03-02", 8, 1),
        ];
        assert_eq!(balance(&entries, "2026-06-01"), (60, 8, 0, 52));
    }

    #[test]
    fn grant_is_valid_on_expiry_day_and_expired_the_day_after() {
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2026-03-02", 8, 1),
        ];
        assert_eq!(balance(&entries, "2026-12-31"), (60, 8, 0, 52));
        assert_eq!(balance(&entries, "2027-01-01"), (60, 8, 52, 0));
        assert_eq!(available(&entries, "2026-12-31"), 52);
        assert_eq!(available(&entries, "2027-01-01"), 0);
    }

    #[test]
    fn grant_does_not_count_before_it_starts() {
        let entries = [grant("2026-01-01", Some("2026-12-31"), 60)];
        assert_eq!(balance(&entries, "2025-12-31"), (0, 0, 0, 0));
        assert_eq!(balance(&entries, "2026-01-01"), (60, 0, 0, 60));
        assert_eq!(available(&entries, "2025-12-31"), 0);
        assert_eq!(available(&entries, "2026-01-01"), 60);
    }

    #[test]
    fn use_on_expiry_day_consumes_the_grant_but_the_day_after_does_not() {
        let last_day = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2026-12-31", 4, 1),
        ];
        assert_eq!(balance(&last_day, "2027-01-01"), (60, 4, 56, 0));
        let too_late = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2027-01-01", 4, 1),
        ];
        assert_eq!(balance(&too_late, "2027-01-01"), (60, 4, 60, -4));
    }

    #[test]
    fn consumes_earliest_expiring_grant_first_and_never_expiring_last() {
        let entries = [
            grant("2026-01-01", None, 8),
            grant("2026-01-01", Some("2026-12-31"), 8),
            grant("2026-01-01", Some("2026-06-30"), 8),
            used("2026-03-02", 12, 1),
        ];
        // 6월 말 만료분 8 을 먼저 다 쓰고, 12월 말 만료분에서 4.
        assert_eq!(balance(&entries, "2026-07-01"), (24, 12, 0, 12));
        assert_eq!(balance(&entries, "2027-01-01"), (24, 12, 4, 8));
        assert_eq!(balance(&entries, "2099-01-01"), (24, 12, 4, 8));
    }

    #[test]
    fn tie_on_expiry_consumes_earlier_valid_from_first() {
        let entries = [
            grant("2026-03-01", Some("2026-12-31"), 4),
            grant("2026-01-01", Some("2026-12-31"), 4),
            used("2026-04-01", 4, 1),
        ];
        // 1월 부여분에서 차감됐으므로 2월 시점에도 이미 사용으로 잡힌다.
        assert_eq!(balance(&entries, "2026-02-01"), (4, 4, 0, 0));
        assert_eq!(balance(&entries, "2026-04-01"), (8, 4, 0, 4));
    }

    #[test]
    fn future_use_reserves_balance_today() {
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2026-09-01", 8, 1),
        ];
        assert_eq!(balance(&entries, "2026-03-01"), (60, 8, 0, 52));
        assert_eq!(available(&entries, "2026-03-01"), 52);
    }

    #[test]
    fn use_dated_before_grant_starts_is_not_covered_by_it() {
        let entries = [grant("2026-07-01", None, 8), used("2026-06-01", 4, 1)];
        assert_eq!(balance(&entries, "2026-06-15"), (0, 4, 0, -4));
        assert_eq!(balance(&entries, "2026-07-01"), (8, 4, 0, 4));
        assert_eq!(available(&entries, "2026-07-01"), 4);
    }

    #[test]
    fn use_consuming_a_not_yet_started_grant_is_not_counted_before_that_grant() {
        // 내년 부여분에서 차감되는 내년 휴가는 올해 잔여를 줄이지 않는다.
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            grant("2027-01-01", Some("2027-12-31"), 60),
            used("2027-02-01", 8, 1),
        ];
        assert_eq!(balance(&entries, "2026-06-01"), (60, 0, 0, 60));
        assert_eq!(balance(&entries, "2027-02-01"), (120, 8, 60, 52));
    }

    #[test]
    fn restore_voids_its_use() {
        let base = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2026-03-02", 8, 1),
        ];
        let mut entries = base.to_vec();
        entries.push(restore(1, 8));
        assert_eq!(balance(&entries, "2026-06-01"), (60, 0, 0, 60));
        // 다른 신청의 사용은 그대로.
        entries.push(used("2026-04-01", 4, 2));
        assert_eq!(balance(&entries, "2026-06-01"), (60, 4, 0, 56));
    }

    #[test]
    fn double_restore_does_not_create_balance() {
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2026-03-02", 8, 1),
            restore(1, 8),
            restore(1, 8),
        ];
        assert_eq!(balance(&entries, "2026-06-01"), (60, 0, 0, 60));
    }

    #[test]
    fn restore_without_use_is_ignored() {
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            restore(9, 8),
            restore(9, i32::MAX),
            restore(9, i32::MIN),
        ];
        assert_eq!(balance(&entries, "2026-06-01"), (60, 0, 0, 60));
        assert_eq!(available(&entries, "2026-06-01"), 60);
    }

    #[test]
    fn restore_amount_is_irrelevant_the_use_is_voided_entirely() {
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2026-03-02", 8, 1),
            restore(1, 2),
        ];
        assert_eq!(balance(&entries, "2026-06-01"), (60, 0, 0, 60));
    }

    #[test]
    fn restore_before_its_use_in_slice_order() {
        let entries = [
            restore(1, 8),
            used("2026-03-02", 8, 1),
            grant("2026-01-01", Some("2026-12-31"), 60),
        ];
        assert_eq!(balance(&entries, "2026-06-01"), (60, 0, 0, 60));
    }

    #[test]
    fn repeated_use_rows_of_one_request_all_count_and_are_voided_together() {
        let mut entries = vec![
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2026-03-02", 8, 1),
            used("2026-03-02", 8, 1),
        ];
        assert_eq!(balance(&entries, "2026-06-01"), (60, 16, 0, 44));
        entries.push(restore(1, 8));
        assert_eq!(balance(&entries, "2026-06-01"), (60, 0, 0, 60));
    }

    #[test]
    fn positive_adjustment_is_a_non_expiring_grant_from_its_date() {
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 8),
            adjust("2026-02-01", 4),
        ];
        assert_eq!(balance(&entries, "2026-01-31"), (8, 0, 0, 8));
        assert_eq!(balance(&entries, "2026-02-01"), (12, 0, 0, 12));
        assert_eq!(balance(&entries, "2030-01-01"), (12, 0, 8, 4));
        // 만료가 있는 부여부터 쓰므로 조정분은 마지막까지 남는다.
        let with_use = [entries[0], entries[1], used("2026-03-02", 8, 1)];
        assert_eq!(balance(&with_use, "2030-01-01"), (12, 8, 0, 4));
    }

    #[test]
    fn negative_adjustment_is_a_use_on_its_date() {
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 8),
            adjust("2026-02-01", -4),
        ];
        assert_eq!(balance(&entries, "2026-03-01"), (8, 4, 0, 4));
        assert_eq!(balance(&entries, "2027-01-01"), (8, 4, 4, 0));
    }

    #[test]
    fn uncovered_use_is_a_debt_that_only_a_later_grant_offsets() {
        let mut entries = vec![
            adjust("2026-02-01", -12),
            grant("2026-01-01", Some("2026-12-31"), 8),
        ];
        assert_eq!(balance(&entries, "2026-03-01"), (8, 12, 0, -4));
        assert_eq!(
            available(&entries, "2026-03-01"),
            0,
            "음수 잔여는 0 으로 보인다"
        );
        // 부여가 소멸해도 부채는 남는다.
        assert_eq!(balance(&entries, "2027-01-01"), (8, 12, 0, -4));
        entries.push(adjust("2027-02-01", 4));
        assert_eq!(balance(&entries, "2027-02-01"), (12, 12, 0, 0));
        entries.push(grant("2027-03-01", Some("2027-12-31"), 60));
        assert_eq!(balance(&entries, "2027-03-01"), (72, 12, 0, 60));
        assert_eq!(available(&entries, "2027-03-01"), 60);
    }

    #[test]
    fn debt_reduces_what_a_later_grant_makes_available() {
        let entries = [
            adjust("2026-02-01", -4),
            grant("2026-03-01", Some("2026-12-31"), 8),
        ];
        assert_eq!(balance(&entries, "2026-03-01"), (8, 4, 0, 4));
        assert_eq!(available(&entries, "2026-03-01"), 4);
    }

    #[test]
    fn uses_out_of_date_order_give_the_same_balance() {
        let grants = [
            grant("2026-01-01", Some("2026-06-30"), 8),
            grant("2026-01-01", Some("2026-12-31"), 8),
        ];
        let early = used("2026-03-02", 8, 1);
        let late = used("2026-09-01", 8, 2);
        let in_order = [grants[0], grants[1], early, late];
        let reversed = [late, early, grants[1], grants[0]];
        // 날짜순으로 배정하므로 3월 사용이 6월 만료분을, 9월 사용이 12월 만료분을 쓴다.
        for on in ["2026-03-02", "2026-07-01", "2026-09-01", "2027-01-01"] {
            assert_eq!(balance(&in_order, on), balance(&reversed, on), "{on}");
        }
        assert_eq!(balance(&reversed, "2027-01-01"), (16, 16, 0, 0));
    }

    #[test]
    fn uses_are_allocated_in_effective_date_order_not_slice_order() {
        // 5월 사용이 먼저 기록돼 있어도 2월 사용이 6월 만료분을 먼저 가져간다.
        // 기록 순서대로 배정하면 5월 사용이 6월 만료분을 차지해 2월 사용이 부채가 된다.
        let entries = [
            used("2026-05-04", 4, 1),
            used("2026-02-02", 4, 2),
            grant("2026-04-01", Some("2026-12-31"), 4),
            grant("2026-01-01", Some("2026-06-30"), 4),
        ];
        assert_eq!(balance(&entries, "2026-02-02"), (4, 4, 0, 0));
        assert_eq!(balance(&entries, "2026-05-04"), (8, 8, 0, 0));
        assert_eq!(balance(&entries, "2027-01-01"), (8, 8, 0, 0));
    }

    #[test]
    fn zero_amount_entries_change_nothing() {
        let base = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2026-03-02", 8, 1),
        ];
        let mut entries = base.to_vec();
        entries.extend([
            grant("2026-01-01", None, 0),
            used("2026-03-02", 0, 2),
            adjust("2026-03-02", 0),
            restore(3, 0),
        ]);
        for on in ["2026-06-01", "2027-06-01"] {
            assert_eq!(balance(&entries, on), balance(&base, on));
        }
    }

    #[test]
    fn negative_grant_or_use_amount_is_rejected() {
        let ok = grant("2026-01-01", None, 60);
        for bad in [
            grant("2026-01-01", None, -1),
            grant("2026-01-01", None, i32::MIN),
            used("2026-03-02", -1, 1),
            used("2026-03-02", i32::MIN, 1),
        ] {
            let entries = [ok, bad];
            assert_eq!(Balance::compute(&entries, d("2026-06-01")), Err(AMOUNT_ERR));
            assert_eq!(available_for(&entries, d("2026-06-01")), Err(AMOUNT_ERR));
            assert_eq!(
                check_sufficient(&entries, d("2026-06-01"), q(1)),
                Err(AMOUNT_ERR)
            );
        }
        // 복원된 신청의 음수 사용도 원장 오류다. 음수 사용은 공짜 잔여가 되기 때문이다.
        let entries = [ok, used("2026-03-02", -4, 1), restore(1, 4)];
        assert_eq!(Balance::compute(&entries, d("2026-06-01")), Err(AMOUNT_ERR));
    }

    #[test]
    fn grant_with_inverted_window_is_never_usable() {
        let entries = [
            grant("2026-05-01", Some("2026-04-01"), 8),
            used("2026-04-15", 4, 1),
        ];
        assert_eq!(balance(&entries, "2026-04-15"), (0, 4, 0, -4));
        assert_eq!(balance(&entries, "2026-05-01"), (8, 4, 8, -4));
        assert_eq!(available(&entries, "2026-05-01"), 0);
    }

    #[test]
    fn overflowing_amounts_are_errors_not_wraps() {
        let big = grant("2026-01-01", None, i32::MAX);
        let on = d("2026-06-01");
        assert_eq!(
            Balance::compute(&[big], on).map(|b| b.remaining),
            Ok(q(i32::MAX))
        );
        assert_eq!(Balance::compute(&[big, big], on), Err(AMOUNT_ERR));
        assert_eq!(available_for(&[big, big], on), Err(AMOUNT_ERR));
        assert_eq!(check_sufficient(&[big, big], on, q(1)), Err(AMOUNT_ERR));
        // 합은 넘치지만 사용으로 상쇄되는 경우에도 granted 를 표현할 수 없으면 오류.
        assert_eq!(
            Balance::compute(&[big, big, used("2026-02-01", i32::MAX, 1)], on),
            Err(AMOUNT_ERR)
        );
        // i32::MIN 조정은 부호를 뒤집으면 i32 를 넘는다.
        assert_eq!(
            Balance::compute(&[adjust("2026-02-01", i32::MIN)], on),
            Err(AMOUNT_ERR)
        );
        assert_eq!(
            Balance::compute(&[big, adjust("2026-02-01", i32::MIN)], on),
            Err(AMOUNT_ERR)
        );
        // 경계: 정확히 i32::MAX 를 쓰는 것은 된다.
        assert_eq!(
            Balance::compute(&[big, used("2026-02-01", i32::MAX, 1)], on).map(|b| b.remaining),
            Ok(q(0))
        );
        let debt = [
            adjust("2026-02-01", -i32::MAX),
            adjust("2026-02-02", -i32::MAX),
        ];
        assert_eq!(Balance::compute(&debt, on), Err(AMOUNT_ERR));
    }

    #[test]
    fn overflow_error_does_not_depend_on_entry_order() {
        let big = grant("2026-01-01", None, i32::MAX);
        let small = grant("2026-01-01", None, 1);
        let on = d("2026-06-01");
        assert_eq!(Balance::compute(&[big, small], on), Err(AMOUNT_ERR));
        assert_eq!(Balance::compute(&[small, big], on), Err(AMOUNT_ERR));
    }

    #[test]
    fn extreme_dates_do_not_panic() {
        let entries = [
            grant_on(NaiveDate::MIN, Some(NaiveDate::MAX), 8),
            grant_on(NaiveDate::MAX, Some(NaiveDate::MIN), 8),
            grant_on(NaiveDate::MAX, None, 8),
            use_on(NaiveDate::MIN, 2, 1),
            use_on(NaiveDate::MAX, 2, 2),
            adjust_on(NaiveDate::MAX, -1),
            adjust_on(NaiveDate::MIN, 1),
        ];
        let at_min = Balance::compute(&entries, NaiveDate::MIN).expect("min");
        // MIN 시점: 첫 부여 8 + 조정 1. MIN 사용 2 와 MAX 사용 2+1 이 모두 이 둘에서 차감된다.
        assert_eq!(
            (
                at_min.granted,
                at_min.used,
                at_min.expired,
                at_min.remaining
            ),
            (q(9), q(5), q(0), q(4))
        );
        let at_max = Balance::compute(&entries, NaiveDate::MAX).expect("max");
        assert_eq!(
            (
                at_max.granted,
                at_max.used,
                at_max.expired,
                at_max.remaining
            ),
            (q(25), q(5), q(8), q(12))
        );
        assert_eq!(available_for(&entries, NaiveDate::MIN), Ok(q(4)));
        assert_eq!(available_for(&entries, NaiveDate::MAX), Ok(q(12)));
        assert_eq!(check_sufficient(&entries, NaiveDate::MAX, q(12)), Ok(()));
    }

    #[test]
    fn never_expiring_and_max_date_expiry_are_equivalent() {
        let none = [grant_on(d("2026-01-01"), None, 8), used("2026-03-02", 4, 1)];
        let max = [
            grant_on(d("2026-01-01"), Some(NaiveDate::MAX), 8),
            used("2026-03-02", 4, 1),
        ];
        for on in [d("2026-06-01"), NaiveDate::MAX] {
            assert_eq!(Balance::compute(&none, on), Balance::compute(&max, on));
        }
    }

    // ───────────── 신청 가능 수량 ─────────────

    #[test]
    fn available_counts_only_grants_valid_on_leave_start() {
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            grant("2027-01-01", Some("2027-12-31"), 64),
        ];
        assert_eq!(available(&entries, "2026-12-31"), 60);
        assert_eq!(available(&entries, "2027-01-01"), 64);
        assert_eq!(available(&entries, "2028-01-01"), 0);
    }

    #[test]
    fn available_accounts_for_later_dated_uses_of_the_same_grant() {
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2026-11-02", 56, 1),
        ];
        assert_eq!(available(&entries, "2026-06-01"), 4);
        let all = [entries[0], entries[1], used("2026-12-01", 4, 2)];
        assert_eq!(available(&all, "2026-06-01"), 0);
    }

    #[test]
    fn recording_the_available_amount_never_overdraws() {
        // 6월 만료 4, 연말 만료 4. 9월에 4 사용 예정(연말분에서 차감).
        let mut entries = vec![
            grant("2026-01-01", Some("2026-06-30"), 4),
            grant("2026-01-01", Some("2026-12-31"), 4),
            used("2026-09-01", 4, 1),
        ];
        assert_eq!(available(&entries, "2026-03-02"), 4);
        entries.push(used("2026-03-02", 4, 2));
        for on in ["2026-03-02", "2026-06-30", "2026-09-01", "2027-01-01"] {
            assert!(balance(&entries, on).3 >= 0, "{on}");
        }
        assert_eq!(balance(&entries, "2027-01-01"), (8, 8, 0, 0));
        assert_eq!(available(&entries, "2026-03-02"), 0);
    }

    #[test]
    fn check_sufficient_boundary() {
        let entries = [
            grant("2026-01-01", Some("2026-12-31"), 60),
            used("2026-03-02", 8, 1),
        ];
        let start = d("2026-06-01");
        let insufficient: DomainError = ConflictReason::InsufficientLeaveBalance.into();
        assert_eq!(
            check_sufficient(&entries, start, q(52)),
            Ok(()),
            "정확히 잔여만큼"
        );
        assert_eq!(
            check_sufficient(&entries, start, q(53)),
            Err(insufficient.clone()),
            "1/4일 초과"
        );
        assert_eq!(check_sufficient(&entries, start, q(1)), Ok(()));
        assert_eq!(
            check_sufficient(&entries, start, q(i32::MAX)),
            Err(insufficient.clone())
        );
        assert_eq!(
            check_sufficient(&entries, d("2027-01-01"), q(1)),
            Err(insufficient.clone())
        );
        assert_eq!(check_sufficient(&[], start, q(1)), Err(insufficient));
    }

    // ---- 부여 경계를 넘는 신청 ----

    const INSUFFICIENT: DomainError =
        DomainError::Conflict(ConflictReason::InsufficientLeaveBalance);

    fn span(start: &str, end: &str) -> LeaveSpan {
        LeaveSpan::new(range(start, end), LeaveUnit::FullDay, &calendar(&[])).expect("span")
    }

    fn dates(values: &[&str]) -> Vec<NaiveDate> {
        values.iter().map(|value| d(value)).collect()
    }

    /// 하나뿐인 부여가 끝난 뒤까지 걸친 신청: 첫날 잔여는 넉넉해도 뒤쪽 날짜에는 쉴 권리가 없다.
    #[test]
    fn span_ending_after_the_only_grant_expires_is_rejected() {
        // 2026-03-09(월)까지 유효한 11일. 다음 부여는 아직 원장에 없다.
        let entries = [grant("2025-04-10", Some("2026-03-09"), 44)];
        let crossing = span("2026-03-09", "2026-03-10");
        // 첫날 기준 총량 검사만으로는 통과했다(이것이 고치려는 결함이다).
        assert_eq!(
            check_sufficient(&entries, crossing.first_day(), crossing.amount()),
            Ok(())
        );
        assert_eq!(
            check_span_covered(&entries, crossing.working_dates(), LeaveUnit::FullDay),
            Err(INSUFFICIENT)
        );
        assert_eq!(check_reservable(&entries, &crossing), Err(INSUFFICIENT));
        // 긴 휴가의 마지막 하루만 넘어가도 마찬가지다.
        assert_eq!(
            check_reservable(&entries, &span("2026-03-02", "2026-03-10")),
            Err(INSUFFICIENT)
        );
    }

    #[test]
    fn exact_boundary_days_of_a_grant() {
        let entries = [grant("2026-03-04", Some("2026-03-09"), 44)];
        // 만료일 당일까지, 시작일 당일부터는 된다.
        assert_eq!(
            check_reservable(&entries, &span("2026-03-09", "2026-03-09")),
            Ok(())
        );
        assert_eq!(
            check_reservable(&entries, &span("2026-03-04", "2026-03-09")),
            Ok(())
        );
        // 만료 다음 날 하루, 시작 전날 하루는 안 된다.
        assert_eq!(
            check_reservable(&entries, &span("2026-03-10", "2026-03-10")),
            Err(INSUFFICIENT)
        );
        assert_eq!(
            check_reservable(&entries, &span("2026-03-03", "2026-03-03")),
            Err(INSUFFICIENT)
        );
        // 시작 전날부터 걸친 신청도 안 된다(첫날에 유효한 부여가 없다).
        assert_eq!(
            check_reservable(&entries, &span("2026-03-03", "2026-03-04")),
            Err(INSUFFICIENT)
        );
        // 금요일(03-06)~다음 주 월요일(03-09): 주말은 세지 않으므로 만료일에 딱 맞는다.
        assert_eq!(
            check_reservable(&entries, &span("2026-03-06", "2026-03-09")),
            Ok(())
        );
        assert_eq!(
            check_reservable(&entries, &span("2026-03-06", "2026-03-10")),
            Err(INSUFFICIENT)
        );
    }

    /// 기념일을 넘는 신청: 양쪽 부여가 모두 넉넉하면 통과한다.
    #[test]
    fn span_crossing_an_anniversary_with_balance_on_both_sides_is_accepted() {
        let entries = [
            grant("2025-03-02", Some("2026-03-01"), 60),
            grant("2026-03-02", Some("2027-03-01"), 60),
        ];
        // 02-27(금), 03-02(월), 03-03(화).
        let crossing = span("2026-02-27", "2026-03-03");
        assert_eq!(
            crossing.working_dates(),
            dates(&["2026-02-27", "2026-03-02", "2026-03-03"])
        );
        assert_eq!(check_reservable(&entries, &crossing), Ok(()));
        // 예약은 첫 근무일 한 줄이고, 그 뒤 구간의 어느 날짜에도 잔여가 음수가 아니다.
        let mut recorded = entries.to_vec();
        recorded.push(use_on(
            crossing.first_day(),
            crossing.amount().quarters(),
            1,
        ));
        for on in ["2026-02-27", "2026-03-01", "2026-03-02", "2026-03-03"] {
            let remaining = Balance::compute(&recorded, d(on))
                .expect("balance")
                .remaining;
            assert!(!remaining.is_negative(), "{on}: {remaining}");
        }
        // 한계(문서화된 것): 전부 첫날의 부여에서 차감되므로 새 부여는 줄지 않는다.
        assert_eq!(balance(&recorded, "2026-03-03"), (120, 12, 48, 60));
    }

    /// 기념일 뒤쪽이 모자라면, 첫날 부여가 아무리 넉넉해도 거부한다.
    #[test]
    fn span_crossing_an_anniversary_needs_balance_after_it_too() {
        let old = grant("2025-03-02", Some("2026-03-01"), 60);
        // 새 부여 15일 중 14일이 이미 뒤 날짜의 휴가로 예약돼 있다: 1일 남음.
        let entries = [
            old,
            grant("2026-03-02", Some("2027-03-01"), 60),
            used("2026-04-01", 56, 1),
        ];
        // 뒤쪽 하루(03-02)는 남은 1일로 덮인다.
        assert_eq!(
            check_reservable(&entries, &span("2026-02-27", "2026-03-02")),
            Ok(())
        );
        // 뒤쪽 이틀(03-02, 03-03)은 덮이지 않는다. 첫날 기준 총량은 넉넉한데도.
        let two_after = span("2026-02-27", "2026-03-03");
        assert_eq!(
            check_sufficient(&entries, two_after.first_day(), two_after.amount()),
            Ok(())
        );
        assert_eq!(check_reservable(&entries, &two_after), Err(INSUFFICIENT));
        // 그 예약이 복원되면 다시 통과한다.
        let mut restored = entries.to_vec();
        restored.push(restore(1, 56));
        assert_eq!(check_reservable(&restored, &two_after), Ok(()));
        // 새 부여가 아직 원장에 없을 때(기념일 전에 미리 신청)도 거부한다.
        assert_eq!(check_reservable(&[old], &two_after), Err(INSUFFICIENT));
    }

    /// 문서화된 한계: 첫날의 부여만으로 전체를 낼 수 없으면, 날짜별로는 덮여도 거부한다.
    /// 신청자가 경계에서 둘로 나누면 각각 통과한다.
    #[test]
    fn span_must_be_payable_by_the_grants_valid_on_its_first_day() {
        // 옛 부여 1일 남음, 새 부여 15일.
        let entries = [
            grant("2025-03-02", Some("2026-03-01"), 60),
            used("2025-06-02", 56, 1),
            grant("2026-03-02", Some("2027-03-01"), 60),
        ];
        let crossing = span("2026-02-27", "2026-03-03");
        assert_eq!(
            check_span_covered(&entries, crossing.working_dates(), LeaveUnit::FullDay),
            Ok(())
        );
        assert_eq!(check_reservable(&entries, &crossing), Err(INSUFFICIENT));
        // 나눠서 신청: 앞쪽(02-27)과 뒤쪽(03-02~03-03).
        let before = span("2026-02-27", "2026-02-27");
        assert_eq!(check_reservable(&entries, &before), Ok(()));
        let mut recorded = entries.to_vec();
        recorded.push(use_on(before.first_day(), before.amount().quarters(), 2));
        let after = span("2026-03-02", "2026-03-03");
        assert_eq!(check_reservable(&recorded, &after), Ok(()));
        recorded.push(use_on(after.first_day(), after.amount().quarters(), 3));
        assert_eq!(balance(&recorded, "2026-03-03"), (120, 68, 0, 52));
    }

    #[test]
    fn span_coverage_counts_partial_units_and_ignores_date_order() {
        // 반차 한 번(2/4일)만 남은 날.
        let entries = [grant("2026-03-02", Some("2026-03-31"), 2)];
        let day = dates(&["2026-03-10"]);
        assert_eq!(
            check_span_covered(&entries, &day, LeaveUnit::AmHalf),
            Ok(())
        );
        assert_eq!(
            check_span_covered(&entries, &day, LeaveUnit::PmQuarter),
            Ok(())
        );
        assert_eq!(
            check_span_covered(&entries, &day, LeaveUnit::FullDay),
            Err(INSUFFICIENT)
        );
        // 날짜 순서가 뒤섞여 와도 결과가 같다. 날마다 만료가 빠른 부여부터 쓴다.
        let two = [
            grant("2026-03-02", Some("2026-03-10"), 4),
            grant("2026-03-02", Some("2026-03-31"), 4),
        ];
        for order in [
            dates(&["2026-03-10", "2026-03-11"]),
            dates(&["2026-03-11", "2026-03-10"]),
        ] {
            assert_eq!(check_span_covered(&two, &order, LeaveUnit::FullDay), Ok(()));
        }
        assert_eq!(
            check_span_covered(
                &two,
                &dates(&["2026-03-11", "2026-03-12"]),
                LeaveUnit::FullDay
            ),
            Err(INSUFFICIENT)
        );
        // 소멸하지 않는 양수 조정은 그날부터 어느 날이든 덮는다.
        let adjusted = [adjust("2026-03-02", 8)];
        assert_eq!(
            check_span_covered(
                &adjusted,
                &dates(&["2026-03-02", "2030-01-02"]),
                LeaveUnit::FullDay
            ),
            Ok(())
        );
        assert_eq!(
            check_span_covered(&[], &[], LeaveUnit::FullDay),
            Err(AMOUNT_ERR)
        );
        assert_eq!(
            check_span_covered(&[], &day, LeaveUnit::AmQuarter),
            Err(INSUFFICIENT)
        );
    }

    #[test]
    fn check_sufficient_rejects_non_positive_requests() {
        let entries = [grant("2026-01-01", Some("2026-12-31"), 60)];
        for bad in [0, -1, i32::MIN] {
            assert_eq!(
                check_sufficient(&entries, d("2026-06-01"), q(bad)),
                Err(AMOUNT_ERR),
                "{bad}"
            );
        }
    }

    // ───────────── 신청 상태 ─────────────

    #[test]
    fn status_round_trip_and_serde() {
        for &status in LeaveRequestStatus::ALL {
            assert_eq!(LeaveRequestStatus::parse(status.as_str()), Ok(status));
            let json = serde_json::to_string(&status).expect("serialize");
            assert_eq!(json, format!("\"{}\"", status.as_str()));
            assert_eq!(
                serde_json::from_str::<LeaveRequestStatus>(&json).expect("deserialize"),
                status
            );
        }
        assert_eq!(LeaveRequestStatus::Canceled.as_str(), "canceled");
        let strings: Vec<&str> = LeaveRequestStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(strings, ["pending", "approved", "rejected", "canceled"]);
        assert!(serde_json::from_str::<LeaveRequestStatus>("\"Pending\"").is_err());
    }

    #[test]
    fn leave_unit_wire_strings_are_fixed() {
        let strings: Vec<&str> = LeaveUnit::ALL.iter().map(|unit| unit.as_str()).collect();
        assert_eq!(
            strings,
            ["full_day", "am_half", "pm_half", "am_quarter", "pm_quarter"]
        );
        for &unit in LeaveUnit::ALL {
            let json = serde_json::to_string(&unit).expect("serialize");
            assert_eq!(json, format!("\"{}\"", unit.as_str()));
            assert_eq!(
                serde_json::from_str::<LeaveUnit>(&json).expect("deserialize"),
                unit
            );
        }
        assert!(serde_json::from_str::<LeaveUnit>("\"FullDay\"").is_err());
    }

    #[test]
    fn ledger_kind_wire_strings_are_fixed() {
        let strings: Vec<&str> = LedgerKind::ALL.iter().map(|kind| kind.as_str()).collect();
        assert_eq!(strings, ["grant", "use", "restore", "adjust"]);
        for &kind in LedgerKind::ALL {
            let json = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(
                serde_json::from_str::<LedgerKind>(&json).expect("deserialize"),
                kind
            );
        }
        assert_eq!(
            LedgerKind::parse("Grant"),
            Err(DomainError::validation(
                "kind",
                ValidationReason::InvalidFormat
            ))
        );
    }

    #[test]
    fn accrual_kind_wire_strings_are_fixed() {
        let strings: Vec<&str> = AccrualKind::ALL.iter().map(|kind| kind.as_str()).collect();
        assert_eq!(strings, ["monthly", "annual"]);
        for &kind in AccrualKind::ALL {
            let json = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(AccrualKind::parse(kind.as_str()), Ok(kind));
        }
        assert_eq!(
            LeaveRequestStatus::parse("Pending"),
            Err(DomainError::validation(
                "status",
                ValidationReason::InvalidFormat
            ))
        );
        assert!(LeaveRequestStatus::parse("").is_err());
        assert!(serde_json::from_str::<LeaveRequestStatus>("\"cancelled\"").is_err());
    }

    #[test]
    fn cancel_pending_always_restores() {
        use LeaveRequestStatus::Pending;
        let start = d("2026-03-02");
        let expected = Ok(CancelDecision::Cancel {
            restore_balance: true,
        });
        for today in ["2026-03-01", "2026-03-02", "2026-03-10"] {
            assert_eq!(decide_cancel(Pending, start, d(today), false), expected);
            assert_eq!(decide_cancel(Pending, start, d(today), true), expected);
        }
    }

    #[test]
    fn cancel_approved_by_member_only_before_start() {
        use LeaveRequestStatus::Approved;
        let start = d("2026-03-02");
        let started: DomainError = ConflictReason::LeaveAlreadyStarted.into();
        assert_eq!(
            decide_cancel(Approved, start, d("2026-03-01"), false),
            Ok(CancelDecision::Cancel {
                restore_balance: true
            }),
            "시작 전날"
        );
        assert_eq!(
            decide_cancel(Approved, start, start, false),
            Err(started.clone()),
            "시작 당일"
        );
        assert_eq!(
            decide_cancel(Approved, start, d("2026-03-03"), false),
            Err(started)
        );
    }

    #[test]
    fn cancel_approved_by_admin_always() {
        use LeaveRequestStatus::Approved;
        let start = d("2026-03-02");
        for today in ["2026-03-01", "2026-03-02", "2027-01-01"] {
            assert_eq!(
                decide_cancel(Approved, start, d(today), true),
                Ok(CancelDecision::Cancel {
                    restore_balance: true
                })
            );
        }
    }

    #[test]
    fn cancel_is_idempotent_and_rejected_is_closed() {
        use LeaveRequestStatus::{Canceled, Rejected};
        let start = d("2026-03-02");
        for (today, by_admin) in [
            ("2026-03-01", false),
            ("2026-03-05", false),
            ("2026-03-05", true),
        ] {
            let again = decide_cancel(Canceled, start, d(today), by_admin).expect("no-op");
            assert_eq!(again, CancelDecision::AlreadyCanceled);
            assert!(
                !again.restore_balance(),
                "두 번째 취소가 잔여를 다시 복원하면 안 된다"
            );
            assert_eq!(
                decide_cancel(Rejected, start, d(today), by_admin),
                Err(ConflictReason::DocumentAlreadyClosed.into())
            );
        }
        assert!(CancelDecision::Cancel {
            restore_balance: true
        }
        .restore_balance());
    }

    #[test]
    fn cancel_handles_extreme_dates() {
        use LeaveRequestStatus::Approved;
        assert!(decide_cancel(Approved, NaiveDate::MAX, NaiveDate::MIN, false).is_ok());
        assert!(decide_cancel(Approved, NaiveDate::MIN, NaiveDate::MAX, false).is_err());
    }

    #[test]
    fn resolution_of_pending() {
        use LeaveRequestStatus::*;
        let approved = decide_resolution(Pending, true).expect("approve");
        assert_eq!(
            approved,
            ResolutionDecision::Resolve {
                status: Approved,
                restore_balance: false
            }
        );
        assert!(!approved.restore_balance(), "승인은 예약을 유지한다");
        let rejected = decide_resolution(Pending, false).expect("reject");
        assert_eq!(
            rejected,
            ResolutionDecision::Resolve {
                status: Rejected,
                restore_balance: true
            }
        );
        assert!(rejected.restore_balance(), "반려는 예약을 복원한다");
    }

    #[test]
    fn resolution_repeated_is_noop_and_opposite_is_conflict() {
        use LeaveRequestStatus::*;
        let closed: DomainError = ConflictReason::DocumentAlreadyClosed.into();
        let same_approve = decide_resolution(Approved, true).expect("no-op");
        assert_eq!(same_approve, ResolutionDecision::AlreadyResolved);
        assert!(!same_approve.restore_balance());
        let same_reject = decide_resolution(Rejected, false).expect("no-op");
        assert_eq!(same_reject, ResolutionDecision::AlreadyResolved);
        assert!(
            !same_reject.restore_balance(),
            "두 번째 반려가 다시 복원하면 안 된다"
        );
        assert_eq!(decide_resolution(Approved, false), Err(closed.clone()));
        assert_eq!(decide_resolution(Rejected, true), Err(closed.clone()));
        assert_eq!(decide_resolution(Canceled, true), Err(closed.clone()));
        assert_eq!(decide_resolution(Canceled, false), Err(closed));
    }

    // ───────────── 속성 기반 ─────────────

    fn day(offset: i64) -> NaiveDate {
        d("2025-01-01") + Duration::days(offset)
    }

    fn entry_strategy() -> impl Strategy<Value = LedgerEntry> {
        prop_oneof![
            3 => (0i64..700, proptest::option::of(0i64..500), 0i32..40)
                .prop_map(|(from, len, amount)| grant_on(day(from), len.map(|l| day(from + l)), amount)),
            4 => (0i64..900, 0i32..24, 0u128..6).prop_map(|(on, amount, r)| use_on(day(on), amount, r)),
            1 => (0u128..6, 0i32..24).prop_map(|(r, amount)| restore(r, amount)),
            2 => (0i64..900, -16i32..16).prop_map(|(on, amount)| adjust_on(day(on), amount)),
        ]
    }

    fn ledger_strategy() -> impl Strategy<Value = Vec<LedgerEntry>> {
        proptest::collection::vec(entry_strategy(), 0..24)
    }

    /// 그날 유효한(시작됐고 만료되지 않은) 부여·양수 조정의 합.
    fn valid_grant_total(entries: &[LedgerEntry], on: NaiveDate) -> i32 {
        entries
            .iter()
            .map(|e| match e.kind {
                LedgerEntryKind::Grant {
                    valid_from,
                    expires_on,
                } if valid_from <= on && expires_on.is_none_or(|x| on <= x) => e.amount.quarters(),
                LedgerEntryKind::Adjust { effective_on } if effective_on <= on => {
                    e.amount.quarters().max(0)
                }
                _ => 0,
            })
            .sum()
    }

    proptest! {
        /// 입사일을 k일 옮겨도 같은 차수의 부여는 키와 수량이 같다. 그래서 입사일을 고친 뒤
        /// 다시 부여해도 이미 받은 차수는 유일 제약에 걸려 두 번 들어가지 않는다.
        #[test]
        fn accrual_keys_do_not_depend_on_the_hire_date(
            hire in 0i64..15_000,
            shift in -800i64..800,
            elapsed in 0i64..12_000,
        ) {
            let hired = d("1990-01-01") + Duration::days(hire);
            let moved = hired + Duration::days(shift);
            let original = accrual_grants(hired, None, hired + Duration::days(elapsed));
            let corrected = accrual_grants(moved, None, moved + Duration::days(elapsed));
            let common = original.len().min(corrected.len());
            // 말일 보정 때문에 같은 경과일에 차수 하나가 더 있거나 덜 있을 수 있다.
            prop_assert!(original.len().abs_diff(corrected.len()) <= 1);
            for (a, b) in original.iter().zip(&corrected).take(common) {
                prop_assert_eq!(&a.grant_key, &b.grant_key);
                prop_assert_eq!(a.kind, b.kind);
                prop_assert_eq!(a.amount, b.amount);
            }
            // 키만으로 같은 부여인지 알 수 있다: 같은 입사일의 결과 안에서 유일하다.
            let keys: BTreeSet<&str> = corrected.iter().map(|g| g.grant_key.as_str()).collect();
            prop_assert_eq!(keys.len(), corrected.len());
        }

        /// 나중에 다시 계산해도 이전 부여는 키·수량·날짜가 그대로다(앞부분이 일치).
        #[test]
        fn accrual_is_stable_as_time_passes(
            hire in 0i64..15_000,
            elapsed in -30i64..12_000,
            later in 0i64..6_000,
            retire_after in proptest::option::of(-10i64..12_000),
        ) {
            let hired = d("1990-01-01") + Duration::days(hire);
            let retired = retire_after.map(|n| hired + Duration::days(n));
            let as_of = hired + Duration::days(elapsed);
            let earlier = accrual_grants(hired, retired, as_of);
            let after = accrual_grants(hired, retired, as_of + Duration::days(later));
            prop_assert!(after.len() >= earlier.len());
            prop_assert_eq!(&after[..earlier.len()], &earlier[..]);
            prop_assert_eq!(&accrual_grants(hired, retired, as_of), &earlier);

            let limit = retired.map_or(as_of, |r| r.min(as_of));
            let keys: BTreeSet<&str> = earlier.iter().map(|g| g.grant_key.as_str()).collect();
            prop_assert_eq!(keys.len(), earlier.len());
            for g in &earlier {
                prop_assert!(hired < g.granted_on && g.granted_on <= limit);
                prop_assert!(g.granted_on <= g.expires_on);
                prop_assert!((4..=100).contains(&g.amount.quarters()));
            }
            // 키는 차수다: 월 부여는 1부터, 연 부여도 1부터 빈틈없이 이어진다.
            let (monthly, yearly): (Vec<_>, Vec<_>) =
                earlier.iter().partition(|g| g.kind == AccrualKind::Monthly);
            for (ordinal, g) in (1u32..).zip(&monthly) {
                prop_assert_eq!(&g.grant_key, &format!("monthly:{ordinal}"));
            }
            for (ordinal, g) in (1u32..).zip(&yearly) {
                prop_assert_eq!(&g.grant_key, &format!("annual:{ordinal}"));
            }
            // 어느 날이든 유효한 자동 부여는 "월 단위 묶음" 또는 "연 단위 1건" 중 하나뿐이다.
            let live: Vec<_> = earlier
                .iter()
                .filter(|g| g.granted_on <= limit && limit <= g.expires_on)
                .collect();
            prop_assert!(live.len() <= 11);
            prop_assert!(live.iter().filter(|g| g.kind == AccrualKind::Annual).count() <= 1);
        }

        #[test]
        fn balance_identity_holds(entries in ledger_strategy(), on in 0i64..1_500) {
            let on = day(on);
            let b = Balance::compute(&entries, on).expect("balance");
            prop_assert_eq!(
                b.granted.quarters() - b.used.quarters() - b.expired.quarters(),
                b.remaining.quarters()
            );
            prop_assert!(b.granted.quarters() >= 0 && b.used.quarters() >= 0);
            prop_assert!((0..=b.granted.quarters()).contains(&b.expired.quarters()));
            // 남은 수량은 그날 유효한 부여를 넘지 못한다.
            prop_assert!(b.remaining.quarters() <= valid_grant_total(&entries, on));
        }

        #[test]
        fn far_future_balance_accounts_for_every_entry(entries in ledger_strategy()) {
            let restored: BTreeSet<LeaveRequestId> = entries
                .iter()
                .filter_map(|e| match e.kind {
                    LedgerEntryKind::Restore { request } => Some(request),
                    _ => None,
                })
                .collect();
            let (mut granted, mut consumed) = (0, 0);
            for e in &entries {
                match e.kind {
                    LedgerEntryKind::Grant { .. } => granted += e.amount.quarters(),
                    LedgerEntryKind::Use { request, .. } if !restored.contains(&request) => {
                        consumed += e.amount.quarters();
                    }
                    LedgerEntryKind::Adjust { .. } if e.amount.is_negative() => {
                        consumed -= e.amount.quarters();
                    }
                    LedgerEntryKind::Adjust { .. } => granted += e.amount.quarters(),
                    _ => {}
                }
            }
            let b = Balance::compute(&entries, day(5_000)).expect("balance");
            prop_assert_eq!((b.granted.quarters(), b.used.quarters()), (granted, consumed));
        }

        /// 접수할 수 있다고 판정한 신청을 첫 근무일 한 줄로 기록해도, 구간의 어느 날짜에서도
        /// 잔여가 새로 음수가 되지 않고(이미 음수였다면 더 내려가지 않고) 기록한 수량보다 더
        /// 줄지 않는다. 그리고 접수된 신청의 모든 날에는 그날 유효한 부여가 있었다.
        #[test]
        fn reservable_span_never_creates_debt(
            entries in ledger_strategy(),
            start in 0i64..1_200,
            // 며칠 간격으로 띄엄띄엄 쉬는 날(주말·공휴일을 건너뛴 근무일을 흉내 낸다).
            gaps in proptest::collection::vec(1i64..5, 0..12),
            unit in prop_oneof![
                Just(LeaveUnit::FullDay), Just(LeaveUnit::AmHalf), Just(LeaveUnit::PmQuarter)
            ],
        ) {
            let mut working = vec![day(start)];
            let mut offset = start;
            for gap in gaps {
                offset += gap;
                working.push(day(offset));
            }
            let total = unit.amount_per_day().quarters() * working.len() as i32;
            let first = working[0];
            let reservable = check_sufficient(&entries, first, q(total))
                .and_then(|()| check_span_covered(&entries, &working, unit));
            // 날짜 순서는 판정에 영향을 주지 않는다.
            let mut reversed = working.clone();
            reversed.reverse();
            prop_assert_eq!(
                check_span_covered(&entries, &working, unit),
                check_span_covered(&entries, &reversed, unit)
            );
            if reservable.is_err() {
                return Ok(());
            }
            let mut recorded = entries.clone();
            recorded.push(use_on(first, total, 1_000));
            let last = *working.last().expect("non-empty");
            // 구간 안의 모든 날짜: 0 이상이던 잔여는 음수가 되지 않고, 이미 음수였다면
            // (음수 조정으로 생긴 부채) 더 내려가지 않는다.
            let span_days = last.signed_duration_since(first).num_days();
            for probe in (0..=span_days).map(|n| first + Duration::days(n)) {
                let was = Balance::compute(&entries, probe).expect("balance").remaining.quarters();
                let now = Balance::compute(&recorded, probe).expect("balance").remaining.quarters();
                prop_assert!(now >= was.min(0), "{}: {} -> {}", probe, was, now);
            }
            // 구간 밖을 포함한 어느 날짜든: 기록한 수량보다 더 줄지 않는다.
            let around = [first - Duration::days(400), first - Duration::days(1)];
            let later = [last + Duration::days(1), last + Duration::days(400)];
            for probe in around.into_iter().chain(later).chain(working.iter().copied()) {
                let was = Balance::compute(&entries, probe).expect("balance").remaining.quarters();
                let now = Balance::compute(&recorded, probe).expect("balance").remaining.quarters();
                prop_assert!(now >= was - total, "{}: {} -> {}", probe, was, now);
                prop_assert!(now <= was);
            }
            for date in &working {
                prop_assert!(
                    valid_grant_total(&entries, *date) >= unit.amount_per_day().quarters(),
                    "{}에 유효한 부여가 없다", date
                );
            }
            // 복원하면 원래대로 돌아온다.
            recorded.push(restore(1_000, total));
            prop_assert_eq!(
                Balance::compute(&recorded, last).expect("balance"),
                Balance::compute(&entries, last).expect("balance")
            );
        }

        #[test]
        fn entry_order_does_not_matter(
            (entries, shuffled) in ledger_strategy()
                .prop_flat_map(|v| (Just(v.clone()), Just(v).prop_shuffle())),
            on in 0i64..1_500,
        ) {
            let on = day(on);
            prop_assert_eq!(Balance::compute(&entries, on), Balance::compute(&shuffled, on));
            prop_assert_eq!(available_for(&entries, on), available_for(&shuffled, on));
        }

        #[test]
        fn available_is_bounded_and_recordable(entries in ledger_strategy(), start in 0i64..1_200) {
            let start = day(start);
            let before = Balance::compute(&entries, start).expect("balance");
            let available = available_for(&entries, start).expect("available");
            prop_assert!(available.quarters() >= 0);
            prop_assert!(available.quarters() <= valid_grant_total(&entries, start));
            prop_assert_eq!(available.quarters(), before.remaining.quarters().max(0));

            let fresh = 1_000u128;
            if available.quarters() > 0 {
                prop_assert_eq!(check_sufficient(&entries, start, available), Ok(()));
            }
            prop_assert_eq!(
                check_sufficient(&entries, start, q(available.quarters() + 1)),
                Err(ConflictReason::InsufficientLeaveBalance.into())
            );

            // 그만큼을 실제로 기록해도 계산된 잔여는 약속한 수량 아래로 내려가지 않는다.
            let mut recorded = entries.clone();
            recorded.push(use_on(start, available.quarters(), fresh));
            let after = Balance::compute(&recorded, start).expect("balance");
            prop_assert!(after.remaining.quarters() >= before.remaining.quarters() - available.quarters());
            if before.remaining.quarters() >= 0 {
                prop_assert!(after.remaining.quarters() >= 0);
            }
            // 다른 날짜의 잔여도 기록한 수량보다 더 줄지 않는다(새 부채가 생기지 않는다).
            for probe in [0i64, 200, 400, 600, 800, 1_000, 1_400] {
                let probe = day(probe);
                let was = Balance::compute(&entries, probe).expect("balance").remaining.quarters();
                let now = Balance::compute(&recorded, probe).expect("balance").remaining.quarters();
                prop_assert!(now >= was - available.quarters());
                prop_assert!(now <= was);
            }

            // 복원하면 원래대로 돌아오고, 두 번 복원해도 같다.
            recorded.push(restore(fresh, available.quarters()));
            prop_assert_eq!(Balance::compute(&recorded, start).expect("balance"), before);
            recorded.push(restore(fresh, available.quarters()));
            prop_assert_eq!(Balance::compute(&recorded, start).expect("balance"), before);
        }
    }
}
