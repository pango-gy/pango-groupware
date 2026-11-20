//! 구성원 계정 상태.

use chrono::{Months, NaiveDate};

use crate::calendar::check_date;
use crate::error::{ConflictReason, DomainError, DomainResult, ValidationReason};
use crate::string_enum;

string_enum! {
    /// 계정 상태.
    pub enum MemberStatus("status") {
        /// 재직 중. 로그인 가능.
        Active => "active",
        /// 일시 정지(휴직, 보안 사유 등). 로그인 불가, 기록은 유지.
        Suspended => "suspended",
        /// 퇴사. 로그인 불가.
        Retired => "retired",
    }
}

impl MemberStatus {
    pub const fn can_sign_in(self) -> bool {
        matches!(self, Self::Active)
    }

    /// 결재선·수신 대상 등 **새 업무의 당사자**가 될 수 있는지.
    pub fn require_active(self) -> DomainResult<()> {
        if self.can_sign_in() {
            Ok(())
        } else {
            Err(ConflictReason::MemberInactive.into())
        }
    }
}

/// 받을 수 있는 가장 이른 입사일. 이보다 이르면 입력 실수로 본다.
pub const EARLIEST_HIRE_DATE: NaiveDate = match NaiveDate::from_ymd_opt(1950, 1, 1) {
    Some(date) => date,
    None => panic!("valid date"),
};
/// 입사 예정자를 미리 등록할 수 있는 기간(개월). `today`에서 이만큼 뒤까지 받는다.
pub const HIRE_DATE_LEAD_MONTHS: u32 = 12;

/// 새로 정하는 입사일을 확인한다. `today`는 회사 시간대의 오늘이다.
///
/// 날짜 표기 범위([`check_date`]) 안이어야 하고, [`EARLIEST_HIRE_DATE`]부터 오늘의
/// [`HIRE_DATE_LEAD_MONTHS`]개월 뒤까지만 받는다. 입사일은 연차 부여와 근태 추적 기간의
/// 기준이므로 터무니없는 값(202년, 9999년)이 한 번 저장되면 모든 계산이 어긋난다.
pub fn validate_hired_on(hired_on: NaiveDate, today: NaiveDate) -> DomainResult<NaiveDate> {
    let out_of_range = DomainError::validation("hired_on", ValidationReason::OutOfRange);
    check_date("hired_on", hired_on)?;
    let latest = today
        .checked_add_months(Months::new(HIRE_DATE_LEAD_MONTHS))
        .ok_or(out_of_range.clone())?;
    if hired_on < EARLIEST_HIRE_DATE || hired_on > latest {
        return Err(out_of_range);
    }
    Ok(hired_on)
}

/// 퇴사일을 확인한다: 날짜 표기 범위 안이고 입사일보다 이르지 않아야 한다(당일 퇴사 허용).
pub fn validate_retired_on(retired_on: NaiveDate, hired_on: NaiveDate) -> DomainResult<NaiveDate> {
    check_date("retired_on", retired_on)?;
    if retired_on < hired_on {
        return Err(DomainError::validation(
            "retired_on",
            ValidationReason::Inconsistent,
        ));
    }
    Ok(retired_on)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(value: &str) -> NaiveDate {
        value.parse().expect("date")
    }

    #[test]
    fn hire_date_window_boundaries() {
        let today = d("2026-03-02");
        for ok in ["1950-01-01", "2024-02-29", "2026-03-02", "2027-03-02"] {
            assert_eq!(validate_hired_on(d(ok), today), Ok(d(ok)));
        }
        let out_of_range = DomainError::validation("hired_on", ValidationReason::OutOfRange);
        for bad in ["1949-12-31", "0202-03-02", "2027-03-03", "9999-12-31"] {
            assert_eq!(
                validate_hired_on(d(bad), today),
                Err(out_of_range.clone()),
                "accepted {bad}"
            );
        }
        for bad in [NaiveDate::MIN, NaiveDate::MAX] {
            assert_eq!(validate_hired_on(bad, today), Err(out_of_range.clone()));
        }
        // 2월 29일의 1년 뒤는 2월 28일이다.
        assert!(validate_hired_on(d("2029-02-28"), d("2028-02-29")).is_ok());
        assert!(validate_hired_on(d("2029-03-01"), d("2028-02-29")).is_err());
        // 오늘 자체가 표현 범위의 끝이어도 넘치지 않고 거부한다.
        assert_eq!(
            validate_hired_on(d("2026-03-02"), NaiveDate::MAX),
            Err(out_of_range)
        );
    }

    #[test]
    fn retirement_date_is_bounded_and_not_before_hire() {
        let hired = d("2024-03-02");
        assert_eq!(validate_retired_on(hired, hired), Ok(hired));
        assert_eq!(
            validate_retired_on(d("9999-12-31"), hired),
            Ok(d("9999-12-31"))
        );
        assert_eq!(
            validate_retired_on(d("2024-03-01"), hired),
            Err(DomainError::validation(
                "retired_on",
                ValidationReason::Inconsistent
            ))
        );
        let far = NaiveDate::from_ymd_opt(10000, 1, 1).expect("date");
        assert_eq!(
            validate_retired_on(far, hired),
            Err(DomainError::validation(
                "retired_on",
                ValidationReason::OutOfRange
            ))
        );
    }

    #[test]
    fn only_active_members_can_sign_in() {
        assert!(MemberStatus::Active.can_sign_in());
        assert!(!MemberStatus::Suspended.can_sign_in());
        assert!(!MemberStatus::Retired.can_sign_in());
        assert_eq!(
            MemberStatus::Retired.require_active(),
            Err(ConflictReason::MemberInactive.into())
        );
    }

    #[test]
    fn string_enum_round_trips_and_is_strict() {
        for status in MemberStatus::ALL {
            assert_eq!(MemberStatus::parse(status.as_str()), Ok(*status));
            let json = serde_json::to_string(status).expect("serialize");
            assert_eq!(
                serde_json::from_str::<MemberStatus>(&json).expect("deserialize"),
                *status
            );
        }
        for bad in ["", "Active", "ACTIVE", "active ", "deleted", "active\0"] {
            assert!(MemberStatus::parse(bad).is_err(), "accepted {bad:?}");
        }
        assert!(serde_json::from_str::<MemberStatus>("1").is_err());
    }
}
