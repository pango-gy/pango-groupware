//! 관리자 화면.

pub(crate) mod attendance;
mod audit;
mod forms;
mod home;
pub(crate) mod leave;
mod members;
mod notices;
mod org;
mod policy;

pub use attendance::AdminAttendance;
pub use audit::AdminAudit;
pub use forms::AdminApprovalForms;
pub use home::AdminHome;
pub use leave::AdminLeave;
pub use members::AdminMembers;
pub use notices::AdminNotices;
pub use org::AdminOrg;
pub use policy::AdminPolicy;

/// 관리자 화면들이 함께 쓰는 판정(순수 함수).
pub(crate) mod logic {
    use pango_core::authz::Role;
    use pango_core::id::MemberId;

    /// 관리자 권한으로 `target`의 개인 기록(출퇴근 기록 추가·수정, 연차 조정, 휴가의 관리자
    /// 취소)을 직접 다룰 수 있는지. 서버 `authz::edit_personal_records`와 같은 규칙이다: 대상이
    /// 다른 사람이어야 하고, 최고 관리자만 본인 기록도 다룬다. 숨김은 편의일 뿐 서버가 다시 판정한다.
    pub fn can_edit_personal_records(me: MemberId, role: Role, target: MemberId) -> bool {
        role == Role::Owner || me != target
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_owners_may_handle_their_own_records() {
            let me = MemberId::new();
            let other = MemberId::new();
            assert!(can_edit_personal_records(me, Role::Admin, other));
            assert!(!can_edit_personal_records(me, Role::Admin, me));
            assert!(can_edit_personal_records(me, Role::Owner, me));
            assert!(can_edit_personal_records(me, Role::Owner, other));
            // 관리자 화면은 관리자만 열지만, 역할이 바뀐 직후에도 본인 기록은 열리지 않는다.
            assert!(!can_edit_personal_records(me, Role::Member, me));
        }
    }
}
