//! 휴가: 수량 단위, 신청 단위, 연차 부여, 원장 기반 잔여 계산.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{DomainError, DomainResult, ValidationReason};

/// 휴가 수량. 단위는 **1/4일**이다 (1일 = 4, 반차 = 2, 반반차 = 1). 음수는 차감을 뜻한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
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

/// 하루 중 휴가를 쓰는 단위.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaveUnit {
    /// 종일. 여러 날에 걸쳐 쓸 수 있는 유일한 단위.
    FullDay,
    /// 오전 반차.
    AmHalf,
    /// 오후 반차.
    PmHalf,
    /// 오전 반반차(근무 시작 후 2시간).
    AmQuarter,
    /// 오후 반반차(근무 종료 전 2시간).
    PmQuarter,
}

impl LeaveUnit {
    pub const ALL: [LeaveUnit; 5] =
        [Self::FullDay, Self::AmHalf, Self::PmHalf, Self::AmQuarter, Self::PmQuarter];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FullDay => "full_day",
            Self::AmHalf => "am_half",
            Self::PmHalf => "pm_half",
            Self::AmQuarter => "am_quarter",
            Self::PmQuarter => "pm_quarter",
        }
    }

    pub fn parse(value: &str) -> DomainResult<Self> {
        Self::ALL
            .into_iter()
            .find(|unit| unit.as_str() == value)
            .ok_or(DomainError::validation("unit", ValidationReason::InvalidFormat))
    }

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_displays_in_days() {
        let cases = [(0, "0"), (4, "1"), (2, "0.5"), (1, "0.25"), (7, "1.75"), (60, "15"), (-9, "-2.25")];
        for (quarters, expected) in cases {
            assert_eq!(LeaveAmount::from_quarters(quarters).to_string(), expected);
        }
        assert_eq!(LeaveAmount::from_quarters(i32::MIN).to_string(), "-536870912");
    }

    #[test]
    fn amount_arithmetic_is_checked() {
        assert_eq!(LeaveAmount::from_days(15), Some(LeaveAmount::from_quarters(60)));
        assert_eq!(LeaveAmount::from_days(i32::MAX), None);
        let max = LeaveAmount::from_quarters(i32::MAX);
        assert_eq!(max.checked_add(LeaveAmount::from_quarters(1)), None);
        assert_eq!(LeaveAmount::from_quarters(i32::MIN).checked_neg(), None);
    }

    #[test]
    fn unit_round_trip_and_amounts() {
        for unit in LeaveUnit::ALL {
            assert_eq!(LeaveUnit::parse(unit.as_str()), Ok(unit));
        }
        assert!(LeaveUnit::parse("half").is_err());
        assert_eq!(LeaveUnit::FullDay.amount_per_day().quarters(), 4);
        assert_eq!(LeaveUnit::PmHalf.amount_per_day().quarters(), 2);
        assert_eq!(LeaveUnit::AmQuarter.amount_per_day().quarters(), 1);
        assert!(!LeaveUnit::FullDay.is_partial() && LeaveUnit::AmHalf.is_partial());
    }
}
