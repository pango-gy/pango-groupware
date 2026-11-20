//! 구성원 계정 상태.

use crate::error::{ConflictReason, DomainResult};
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

#[cfg(test)]
mod tests {
    use super::*;

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
