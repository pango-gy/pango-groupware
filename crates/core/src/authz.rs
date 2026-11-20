//! 권한 판정. 역할 비교는 이 모듈에서만 한다.
//!
//! 판정 함수는 `Ok(())` 또는 오류를 돌려준다.
//! - 대상이 **보이지 않아야** 하면 [`DomainError::NotFound`] (존재 여부를 숨긴다).
//! - 대상은 보이지만 **동작이 금지**되면 [`DomainError::Forbidden`].

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::{DomainError, DomainResult, ValidationReason};
use crate::id::{DepartmentId, MemberId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Member,
    Admin,
    Owner,
}

impl Role {
    pub const ALL: [Role; 3] = [Role::Member, Role::Admin, Role::Owner];

    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Member => "member",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }

    pub fn parse(value: &str) -> DomainResult<Self> {
        Self::ALL
            .into_iter()
            .find(|role| role.as_str() == value)
            .ok_or(DomainError::validation("role", ValidationReason::InvalidFormat))
    }

    pub const fn is_admin(self) -> bool {
        matches!(self, Role::Admin | Role::Owner)
    }
}

/// 요청을 보낸 구성원.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub member_id: MemberId,
    pub role: Role,
    pub department_id: Option<DepartmentId>,
    /// 조직장으로서 관할하는 부서(하위 부서 포함).
    pub managed_departments: BTreeSet<DepartmentId>,
}

/// 동작의 대상이 되는 구성원.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Subject {
    pub member_id: MemberId,
    pub role: Role,
    pub department_id: Option<DepartmentId>,
}

/// 행위자와 대상의 관계. 가장 강한 관계 하나로 분류한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// 본인.
    Own,
    /// 관리자(admin/owner)로서 접근.
    Administered,
    /// 조직장으로서 관할 부서의 구성원에 접근.
    Managed,
    /// 같은 부서 동료.
    Colleague,
    /// 그 외.
    Unrelated,
}

/// 화면 노출용 권한 묶음. 서버는 이 값을 믿지 않고 매 요청 다시 판정한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// 관리 메뉴 전체.
    pub admin: bool,
    /// 역할 변경(관리자 임명·해제).
    pub manage_roles: bool,
    /// 팀 메뉴(조직장).
    pub team: bool,
}

impl Actor {
    pub fn is_admin(&self) -> bool {
        self.role.is_admin()
    }

    pub fn is_manager(&self) -> bool {
        !self.managed_departments.is_empty()
    }

    pub fn manages_department(&self, department_id: DepartmentId) -> bool {
        self.managed_departments.contains(&department_id)
    }

    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            admin: self.is_admin(),
            manage_roles: self.role == Role::Owner,
            team: self.is_manager(),
        }
    }

    pub fn relation_to(&self, subject: &Subject) -> Relation {
        if self.member_id == subject.member_id {
            Relation::Own
        } else if self.is_admin() {
            Relation::Administered
        } else if subject.department_id.is_some_and(|d| self.manages_department(d)) {
            Relation::Managed
        } else if subject.department_id.is_some() && subject.department_id == self.department_id {
            Relation::Colleague
        } else {
            Relation::Unrelated
        }
    }
}

/// 관리자 전용 동작.
pub fn require_admin(actor: &Actor) -> DomainResult<()> {
    if actor.is_admin() {
        Ok(())
    } else {
        Err(DomainError::Forbidden)
    }
}

/// 최고 관리자 전용 동작.
pub fn require_owner(actor: &Actor) -> DomainResult<()> {
    if actor.role == Role::Owner {
        Ok(())
    } else {
        Err(DomainError::Forbidden)
    }
}

/// 조직장 또는 관리자 전용 동작(팀 메뉴).
pub fn require_manager(actor: &Actor) -> DomainResult<()> {
    if actor.is_manager() || actor.is_admin() {
        Ok(())
    } else {
        Err(DomainError::Forbidden)
    }
}

/// 특정 부서의 팀 정보를 볼 수 있는지. 관할이 아니면 존재를 숨긴다.
pub fn view_department_team(actor: &Actor, department_id: DepartmentId) -> DomainResult<()> {
    if actor.is_admin() || actor.manages_department(department_id) {
        Ok(())
    } else {
        Err(DomainError::NotFound)
    }
}

/// 근태·휴가 등 **개인 기록**을 볼 수 있는지: 본인, 관할 조직장, 관리자.
pub fn view_personal_records(actor: &Actor, subject: &Subject) -> DomainResult<()> {
    match actor.relation_to(subject) {
        Relation::Own | Relation::Administered | Relation::Managed => Ok(()),
        Relation::Colleague | Relation::Unrelated => Err(DomainError::NotFound),
    }
}

/// 휴가 **일정**(누가 언제 쉬는지)을 볼 수 있는지: 같은 부서 동료까지 허용.
pub fn view_leave_schedule(actor: &Actor, subject: &Subject) -> DomainResult<()> {
    match actor.relation_to(subject) {
        Relation::Unrelated => Err(DomainError::NotFound),
        _ => Ok(()),
    }
}

/// 다른 사람의 기록을 **직접 수정**할 수 있는지: 관리자만.
/// 본인과 조직장은 기록이 보이므로 403, 그 외는 404.
pub fn edit_personal_records(actor: &Actor, subject: &Subject) -> DomainResult<()> {
    match actor.relation_to(subject) {
        Relation::Administered => Ok(()),
        Relation::Own if actor.is_admin() => Ok(()),
        Relation::Own | Relation::Managed => Err(DomainError::Forbidden),
        Relation::Colleague | Relation::Unrelated => Err(DomainError::NotFound),
    }
}

/// 계정 자체를 관리(수정·비활성화·비밀번호 초기화)할 수 있는지.
///
/// 관리자는 일반 구성원 계정만, 최고 관리자는 모든 계정을 관리한다.
/// 관리자가 다른 관리자나 최고 관리자의 비밀번호를 초기화해 권한을 탈취하는 것을 막는다.
pub fn manage_account(actor: &Actor, subject: &Subject) -> DomainResult<()> {
    require_admin(actor)?;
    if actor.role == Role::Owner || subject.role == Role::Member {
        Ok(())
    } else {
        Err(DomainError::Forbidden)
    }
}

/// 본인 계정에는 할 수 없는 관리 동작(비활성화, 역할 변경).
/// 스스로를 잠가 버리거나 권한을 올리는 실수를 막는다.
pub fn manage_other_account(actor: &Actor, subject: &Subject) -> DomainResult<()> {
    manage_account(actor, subject)?;
    if actor.member_id == subject.member_id {
        Err(DomainError::Forbidden)
    } else {
        Ok(())
    }
}

/// 역할 변경: 최고 관리자만, 본인 제외.
pub fn change_role(actor: &Actor, subject: &Subject) -> DomainResult<()> {
    require_owner(actor)?;
    if actor.member_id == subject.member_id {
        Err(DomainError::Forbidden)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct World {
        dept_a: DepartmentId,
        dept_a_child: DepartmentId,
        dept_b: DepartmentId,
    }

    fn world() -> World {
        World {
            dept_a: DepartmentId::new(),
            dept_a_child: DepartmentId::new(),
            dept_b: DepartmentId::new(),
        }
    }

    fn actor(role: Role, dept: Option<DepartmentId>, managed: &[DepartmentId]) -> Actor {
        Actor {
            member_id: MemberId::new(),
            role,
            department_id: dept,
            managed_departments: managed.iter().copied().collect(),
        }
    }

    fn subject(role: Role, dept: Option<DepartmentId>) -> Subject {
        Subject { member_id: MemberId::new(), role, department_id: dept }
    }

    fn self_subject(actor: &Actor) -> Subject {
        Subject { member_id: actor.member_id, role: actor.role, department_id: actor.department_id }
    }

    #[test]
    fn role_string_round_trip_and_rejects_unknown() {
        for role in Role::ALL {
            assert_eq!(Role::parse(role.as_str()), Ok(role));
        }
        for bad in ["", "Owner", "ADMIN", "root", "admin ", "member\0"] {
            assert!(Role::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn role_ordering_reflects_privilege() {
        assert!(Role::Member < Role::Admin && Role::Admin < Role::Owner);
    }

    #[test]
    fn relation_classification() {
        let w = world();
        let manager = actor(Role::Member, Some(w.dept_a), &[w.dept_a, w.dept_a_child]);
        assert_eq!(manager.relation_to(&self_subject(&manager)), Relation::Own);
        assert_eq!(manager.relation_to(&subject(Role::Member, Some(w.dept_a_child))), Relation::Managed);
        assert_eq!(manager.relation_to(&subject(Role::Member, Some(w.dept_b))), Relation::Unrelated);
        assert_eq!(manager.relation_to(&subject(Role::Member, None)), Relation::Unrelated);

        let member = actor(Role::Member, Some(w.dept_a), &[]);
        assert_eq!(member.relation_to(&subject(Role::Member, Some(w.dept_a))), Relation::Colleague);

        let admin = actor(Role::Admin, None, &[]);
        assert_eq!(admin.relation_to(&subject(Role::Owner, Some(w.dept_b))), Relation::Administered);
    }

    #[test]
    fn members_without_department_are_not_colleagues_of_each_other() {
        let a = actor(Role::Member, None, &[]);
        assert_eq!(a.relation_to(&subject(Role::Member, None)), Relation::Unrelated);
    }

    #[test]
    fn personal_records_visibility_matrix() {
        let w = world();
        let member = actor(Role::Member, Some(w.dept_a), &[]);
        let manager = actor(Role::Member, Some(w.dept_a), &[w.dept_a]);
        let admin = actor(Role::Admin, None, &[]);
        let colleague = subject(Role::Member, Some(w.dept_a));
        let outsider = subject(Role::Member, Some(w.dept_b));

        assert_eq!(view_personal_records(&member, &self_subject(&member)), Ok(()));
        assert_eq!(view_personal_records(&member, &colleague), Err(DomainError::NotFound));
        assert_eq!(view_personal_records(&member, &outsider), Err(DomainError::NotFound));
        assert_eq!(view_personal_records(&manager, &colleague), Ok(()));
        assert_eq!(view_personal_records(&manager, &outsider), Err(DomainError::NotFound));
        assert_eq!(view_personal_records(&admin, &outsider), Ok(()));
    }

    #[test]
    fn leave_schedule_is_visible_to_colleagues_but_not_outsiders() {
        let w = world();
        let member = actor(Role::Member, Some(w.dept_a), &[]);
        assert_eq!(view_leave_schedule(&member, &subject(Role::Member, Some(w.dept_a))), Ok(()));
        assert_eq!(
            view_leave_schedule(&member, &subject(Role::Member, Some(w.dept_b))),
            Err(DomainError::NotFound)
        );
    }

    #[test]
    fn direct_edit_is_admin_only_with_correct_denial_kind() {
        let w = world();
        let member = actor(Role::Member, Some(w.dept_a), &[]);
        let manager = actor(Role::Member, Some(w.dept_a), &[w.dept_a]);
        let admin = actor(Role::Admin, Some(w.dept_a), &[]);
        let colleague = subject(Role::Member, Some(w.dept_a));

        assert_eq!(edit_personal_records(&member, &self_subject(&member)), Err(DomainError::Forbidden));
        assert_eq!(edit_personal_records(&member, &colleague), Err(DomainError::NotFound));
        assert_eq!(edit_personal_records(&manager, &colleague), Err(DomainError::Forbidden));
        assert_eq!(edit_personal_records(&admin, &colleague), Ok(()));
        assert_eq!(edit_personal_records(&admin, &self_subject(&admin)), Ok(()));
    }

    #[test]
    fn admin_cannot_manage_admin_or_owner_accounts() {
        let admin = actor(Role::Admin, None, &[]);
        assert_eq!(manage_account(&admin, &subject(Role::Member, None)), Ok(()));
        assert_eq!(manage_account(&admin, &subject(Role::Admin, None)), Err(DomainError::Forbidden));
        assert_eq!(manage_account(&admin, &subject(Role::Owner, None)), Err(DomainError::Forbidden));
        assert_eq!(
            manage_account(&admin, &self_subject(&admin)),
            Err(DomainError::Forbidden),
            "관리자는 관리 API로 자기 계정도 바꿀 수 없다(본인용 API를 쓴다)"
        );
    }

    #[test]
    fn owner_manages_everyone_but_not_self_for_destructive_actions() {
        let owner = actor(Role::Owner, None, &[]);
        for role in Role::ALL {
            assert_eq!(manage_account(&owner, &subject(role, None)), Ok(()));
            assert_eq!(manage_other_account(&owner, &subject(role, None)), Ok(()));
        }
        assert_eq!(manage_account(&owner, &self_subject(&owner)), Ok(()));
        assert_eq!(manage_other_account(&owner, &self_subject(&owner)), Err(DomainError::Forbidden));
    }

    #[test]
    fn members_and_managers_cannot_manage_accounts() {
        let w = world();
        let manager = actor(Role::Member, Some(w.dept_a), &[w.dept_a]);
        let target = subject(Role::Member, Some(w.dept_a));
        assert_eq!(manage_account(&manager, &target), Err(DomainError::Forbidden));
        assert_eq!(require_admin(&manager), Err(DomainError::Forbidden));
        assert_eq!(require_manager(&manager), Ok(()));
        assert_eq!(require_manager(&actor(Role::Member, None, &[])), Err(DomainError::Forbidden));
        assert_eq!(require_manager(&actor(Role::Admin, None, &[])), Ok(()));
    }

    #[test]
    fn only_owner_changes_roles_and_never_own() {
        let owner = actor(Role::Owner, None, &[]);
        let admin = actor(Role::Admin, None, &[]);
        let target = subject(Role::Member, None);
        assert_eq!(change_role(&owner, &target), Ok(()));
        assert_eq!(change_role(&admin, &target), Err(DomainError::Forbidden));
        assert_eq!(change_role(&owner, &self_subject(&owner)), Err(DomainError::Forbidden));
    }

    #[test]
    fn department_team_view_hides_unmanaged_departments() {
        let w = world();
        let manager = actor(Role::Member, Some(w.dept_a), &[w.dept_a, w.dept_a_child]);
        assert_eq!(view_department_team(&manager, w.dept_a_child), Ok(()));
        assert_eq!(view_department_team(&manager, w.dept_b), Err(DomainError::NotFound));
        assert_eq!(view_department_team(&actor(Role::Admin, None, &[]), w.dept_b), Ok(()));
    }

    #[test]
    fn capabilities_follow_role_and_management() {
        let w = world();
        let caps = |a: &Actor| a.capabilities();
        assert_eq!(
            caps(&actor(Role::Member, None, &[])),
            Capabilities { admin: false, manage_roles: false, team: false }
        );
        assert_eq!(
            caps(&actor(Role::Member, Some(w.dept_a), &[w.dept_a])),
            Capabilities { admin: false, manage_roles: false, team: true }
        );
        assert_eq!(
            caps(&actor(Role::Admin, None, &[])),
            Capabilities { admin: true, manage_roles: false, team: false }
        );
        assert_eq!(
            caps(&actor(Role::Owner, None, &[])),
            Capabilities { admin: true, manage_roles: true, team: false }
        );
    }
}
