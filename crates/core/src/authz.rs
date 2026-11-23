//! 권한 판정. 역할 비교는 이 모듈에서만 한다.
//!
//! 판정 함수는 `Ok(())` 또는 오류를 돌려준다.
//! - 대상이 **보이지 않아야** 하면 [`DomainError::NotFound`] (존재 여부를 숨긴다).
//! - 대상은 보이지만 **동작이 금지**되면 [`DomainError::Forbidden`].

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::error::{ConflictReason, DomainError, DomainResult, ValidationReason};
use crate::id::{DepartmentId, MemberId};
use crate::member::MemberStatus;
use crate::string_enum;

string_enum! {
    /// 역할. 선언 순서가 곧 권한의 크기다(`Member < Admin < Owner`). 순서 비교는 매크로가
    /// 만들지 않으므로 여기서 따로 파생한다.
    #[derive(PartialOrd, Ord)]
    pub enum Role("role") {
        Member => "member",
        Admin => "admin",
        Owner => "owner",
    }
}

impl Role {
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
        } else if subject
            .department_id
            .is_some_and(|d| self.manages_department(d))
        {
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

/// 본인의 것만 다룰 수 있는 동작(본인 신청의 취소, 본인 임시저장의 삭제 등). 관리자도 예외가
/// 아니다. 남의 것이면 `denial`: 볼 수도 없는 자원이면 `NotFound`(ID 추측 방지), 볼 수는
/// 있지만 이 동작만 금지면 `Forbidden`.
pub fn require_own(actor: &Actor, owner_id: MemberId, denial: DomainError) -> DomainResult<()> {
    if actor.member_id == owner_id {
        Ok(())
    } else {
        Err(denial)
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

/// 구성원 개인의 신청(휴가, 근태 정정)을 **최종 승인**할 권한이 있는지: 관할 조직장 또는 관리자.
///
/// 결재선은 신청자가 고를 수 있으므로, 이 확인이 없으면 동료끼리 서로 승인해 줄 수 있다.
/// 본인은 관리자여도 자기 신청을 승인할 수 없다.
pub fn can_resolve_personal_request(approver: &Actor, subject: &Subject) -> bool {
    matches!(
        approver.relation_to(subject),
        Relation::Administered | Relation::Managed
    )
}

/// 구성원의 **개인 기록을 관리자 권한으로 직접 다룰** 수 있는지: 출퇴근 기록 생성·수정,
/// 연차 수동 조정, 휴가의 관리자 취소.
///
/// - 관리자는 **다른 사람**의 기록만 다룬다. 자기 기록(자기 지각을 지우거나 자기 연차를
///   늘리는 일)은 두 번째 사람이 필요하다: 다른 관리자나 최고 관리자가 한다.
/// - 최고 관리자는 본인 기록도 다룰 수 있다. 최종 권한자라 대신 해 줄 윗사람이 없을 수
///   있고(관리자가 한 명도 없는 회사), 모든 동작이 감사 로그에 남는다.
/// - 본인(관리자 포함)과 조직장은 기록이 보이므로 403, 그 외는 404.
pub fn edit_personal_records(actor: &Actor, subject: &Subject) -> DomainResult<()> {
    match actor.relation_to(subject) {
        Relation::Administered => Ok(()),
        Relation::Own if actor.role == Role::Owner => Ok(()),
        Relation::Own | Relation::Managed => Err(DomainError::Forbidden),
        Relation::Colleague | Relation::Unrelated => Err(DomainError::NotFound),
    }
}

/// 진행 중인 결재 문서를 관리자가 **결재자를 대신해** 끝낼 수 있는지(대리 반려).
///
/// 관리자만 할 수 있고, 자기가 기안한 문서에는 쓸 수 없다(최고 관리자도 마찬가지). 자기
/// 문서의 결과를 스스로 정하는 길이 되기 때문이다. 본인 문서는 다른 관리자가 처리한다.
pub fn override_document(actor: &Actor, drafter: MemberId) -> DomainResult<()> {
    require_admin(actor)?;
    if actor.member_id == drafter {
        Err(DomainError::Forbidden)
    } else {
        Ok(())
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

/// 계정 삭제: 최고 관리자만, 본인 제외.
/// 다른 최고 관리자를 삭제할 때는 서비스가 `keep_an_owner`로 최소 1명 유지도 함께 확인한다.
pub fn delete_account(actor: &Actor, subject: &Subject) -> DomainResult<()> {
    require_owner(actor)?;
    if actor.member_id == subject.member_id {
        Err(DomainError::Forbidden)
    } else {
        Ok(())
    }
}

/// 구성원 디렉터리에서 정지·퇴사 계정까지 볼 수 있는지(관리자만).
pub fn view_inactive_members(actor: &Actor) -> bool {
    actor.is_admin()
}

/// 구성원 한 명의 공개 정보를 볼 수 있는지. 재직 중이면 누구나, 아니면 관리자만.
/// 관리자가 아니면 정지·퇴사 계정은 존재 자체를 숨긴다.
pub fn view_member(actor: &Actor, status: MemberStatus) -> DomainResult<()> {
    if status.can_sign_in() || view_inactive_members(actor) {
        Ok(())
    } else {
        Err(DomainError::NotFound)
    }
}

/// 팀 메뉴의 범위. 관리자는 전체(`None`), 조직장은 관할 부서, 그 외는 403.
pub fn team_scope(actor: &Actor) -> DomainResult<Option<&BTreeSet<DepartmentId>>> {
    require_manager(actor)?;
    Ok((!actor.is_admin()).then_some(&actor.managed_departments))
}

/// 역할·상태 변경 뒤에도 로그인할 수 있는 최고 관리자가 한 명은 남는지.
///
/// `active_owners`는 변경 전, 대상을 포함한 수다. 최고 관리자가 없으면 역할을 바꿀 수
/// 있는 사람이 없어 시스템을 되돌릴 수 없으므로, 마지막 한 명은 내리거나 막을 수 없다.
pub fn keep_an_owner(
    before: (Role, MemberStatus),
    after: (Role, MemberStatus),
    active_owners: i64,
) -> DomainResult<()> {
    let counts = |(role, status): (Role, MemberStatus)| role == Role::Owner && status.can_sign_in();
    if counts(before) && !counts(after) && active_owners <= 1 {
        Err(ConflictReason::LastOwner.into())
    } else {
        Ok(())
    }
}

string_enum! {
    /// 공지 등 대상을 지정해 게시하는 자원의 대상 종류.
    pub enum AudienceKind("audience") {
        /// 전 구성원.
        All => "all",
        /// 특정 부서와 그 하위 부서.
        Department => "department",
    }
}

/// 게시 대상. 부서 대상이면 그 부서와 **하위 부서 전체**가 받는다(본부 공지는 팀에도 간다).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Audience {
    All,
    Department(DepartmentId),
}

impl Audience {
    /// 부서 대상은 부서가 반드시 있어야 하고, 전체 대상은 부서를 받지 않는다.
    /// 둘이 어긋난 요청은 작성자의 의도를 알 수 없으므로 거부한다.
    pub fn new(kind: AudienceKind, department_id: Option<DepartmentId>) -> DomainResult<Self> {
        match (kind, department_id) {
            (AudienceKind::All, None) => Ok(Self::All),
            (AudienceKind::Department, Some(id)) => Ok(Self::Department(id)),
            (AudienceKind::All, Some(_)) => Err(DomainError::validation(
                "department_id",
                ValidationReason::Inconsistent,
            )),
            (AudienceKind::Department, None) => Err(DomainError::validation(
                "department_id",
                ValidationReason::Empty,
            )),
        }
    }

    pub const fn kind(&self) -> AudienceKind {
        match self {
            Self::All => AudienceKind::All,
            Self::Department(_) => AudienceKind::Department,
        }
    }

    pub const fn department_id(&self) -> Option<DepartmentId> {
        match self {
            Self::All => None,
            Self::Department(id) => Some(*id),
        }
    }
}

/// 구성원이 볼 수 있는 게시 대상의 범위. 목록 조회의 필터와 단건 판정이 같은 값을 쓴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudienceScope {
    /// 모든 게시물(관리자).
    Everything,
    /// 전체 대상 게시물 + 이 부서들을 대상으로 한 게시물.
    Departments(BTreeSet<DepartmentId>),
}

impl AudienceScope {
    pub fn allows(&self, audience: &Audience) -> bool {
        match (self, audience) {
            (Self::Everything, _) | (_, Audience::All) => true,
            (Self::Departments(visible), Audience::Department(id)) => visible.contains(id),
        }
    }
}

/// 게시 대상 열람 범위: 관리자는 전부, 그 외는 전체 대상과 **자기 부서 또는 그 상위 부서**
/// 대상. `own_department_ancestors`는 행위자 소속 부서의 조상 목록(자신 포함)이다.
pub fn audience_scope(actor: &Actor, own_department_ancestors: &[DepartmentId]) -> AudienceScope {
    if actor.is_admin() {
        return AudienceScope::Everything;
    }
    // 조상 목록이 다른 부서의 것이면 쓰지 않는다(호출 실수로 범위가 넓어지지 않게).
    let own_chain = actor
        .department_id
        .is_some_and(|own| own_department_ancestors.first() == Some(&own));
    let visible = if own_chain {
        own_department_ancestors.iter().copied().collect()
    } else {
        BTreeSet::new()
    };
    AudienceScope::Departments(visible)
}

/// 게시물 하나를 볼 수 있는지. 범위 밖이면 존재를 숨긴다.
pub fn view_audience(scope: &AudienceScope, audience: &Audience) -> DomainResult<()> {
    if scope.allows(audience) {
        Ok(())
    } else {
        Err(DomainError::NotFound)
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

    #[test]
    fn require_own_allows_only_the_owner_even_for_admins() {
        let w = world();
        for role in [Role::Member, Role::Admin, Role::Owner] {
            let me = actor(role, Some(w.dept_a), &[w.dept_a]);
            assert_eq!(
                require_own(&me, me.member_id, DomainError::NotFound),
                Ok(())
            );
            let other = MemberId::new();
            assert_eq!(
                require_own(&me, other, DomainError::NotFound),
                Err(DomainError::NotFound)
            );
            assert_eq!(
                require_own(&me, other, DomainError::Forbidden),
                Err(DomainError::Forbidden)
            );
        }
    }

    fn subject(role: Role, dept: Option<DepartmentId>) -> Subject {
        Subject {
            member_id: MemberId::new(),
            role,
            department_id: dept,
        }
    }

    fn self_subject(actor: &Actor) -> Subject {
        Subject {
            member_id: actor.member_id,
            role: actor.role,
            department_id: actor.department_id,
        }
    }

    #[test]
    fn role_wire_strings_are_fixed() {
        let strings: Vec<&str> = Role::ALL.iter().map(|role| role.as_str()).collect();
        assert_eq!(strings, ["member", "admin", "owner"]);
        for &role in Role::ALL {
            let json = serde_json::to_string(&role).expect("serialize");
            assert_eq!(json, format!("\"{}\"", role.as_str()));
            assert_eq!(
                serde_json::from_str::<Role>(&json).expect("deserialize"),
                role
            );
        }
        assert!(serde_json::from_str::<Role>("\"Owner\"").is_err());
        assert!(serde_json::from_str::<Role>("2").is_err());
    }

    #[test]
    fn role_string_round_trip_and_rejects_unknown() {
        for &role in Role::ALL {
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
        assert_eq!(
            manager.relation_to(&subject(Role::Member, Some(w.dept_a_child))),
            Relation::Managed
        );
        assert_eq!(
            manager.relation_to(&subject(Role::Member, Some(w.dept_b))),
            Relation::Unrelated
        );
        assert_eq!(
            manager.relation_to(&subject(Role::Member, None)),
            Relation::Unrelated
        );

        let member = actor(Role::Member, Some(w.dept_a), &[]);
        assert_eq!(
            member.relation_to(&subject(Role::Member, Some(w.dept_a))),
            Relation::Colleague
        );

        let admin = actor(Role::Admin, None, &[]);
        assert_eq!(
            admin.relation_to(&subject(Role::Owner, Some(w.dept_b))),
            Relation::Administered
        );
    }

    #[test]
    fn members_without_department_are_not_colleagues_of_each_other() {
        let a = actor(Role::Member, None, &[]);
        assert_eq!(
            a.relation_to(&subject(Role::Member, None)),
            Relation::Unrelated
        );
    }

    #[test]
    fn personal_records_visibility_matrix() {
        let w = world();
        let member = actor(Role::Member, Some(w.dept_a), &[]);
        let manager = actor(Role::Member, Some(w.dept_a), &[w.dept_a]);
        let admin = actor(Role::Admin, None, &[]);
        let colleague = subject(Role::Member, Some(w.dept_a));
        let outsider = subject(Role::Member, Some(w.dept_b));

        assert_eq!(
            view_personal_records(&member, &self_subject(&member)),
            Ok(())
        );
        assert_eq!(
            view_personal_records(&member, &colleague),
            Err(DomainError::NotFound)
        );
        assert_eq!(
            view_personal_records(&member, &outsider),
            Err(DomainError::NotFound)
        );
        assert_eq!(view_personal_records(&manager, &colleague), Ok(()));
        assert_eq!(
            view_personal_records(&manager, &outsider),
            Err(DomainError::NotFound)
        );
        assert_eq!(view_personal_records(&admin, &outsider), Ok(()));
    }

    #[test]
    fn leave_schedule_is_visible_to_colleagues_but_not_outsiders() {
        let w = world();
        let member = actor(Role::Member, Some(w.dept_a), &[]);
        assert_eq!(
            view_leave_schedule(&member, &subject(Role::Member, Some(w.dept_a))),
            Ok(())
        );
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

        assert_eq!(
            edit_personal_records(&member, &self_subject(&member)),
            Err(DomainError::Forbidden)
        );
        assert_eq!(
            edit_personal_records(&member, &colleague),
            Err(DomainError::NotFound)
        );
        assert_eq!(
            edit_personal_records(&manager, &colleague),
            Err(DomainError::Forbidden)
        );
        assert_eq!(edit_personal_records(&admin, &colleague), Ok(()));
        // 관리자는 다른 관리자·최고 관리자의 기록도 다루지만, 자기 기록은 다루지 못한다.
        let owner = actor(Role::Owner, None, &[]);
        assert_eq!(
            edit_personal_records(&admin, &subject(Role::Admin, None)),
            Ok(())
        );
        assert_eq!(edit_personal_records(&admin, &self_subject(&owner)), Ok(()));
        assert_eq!(
            edit_personal_records(&admin, &self_subject(&admin)),
            Err(DomainError::Forbidden)
        );
        // 조직장을 겸한 관리자도 마찬가지다.
        let admin_head = actor(Role::Admin, Some(w.dept_a), &[w.dept_a]);
        assert_eq!(
            edit_personal_records(&admin_head, &self_subject(&admin_head)),
            Err(DomainError::Forbidden)
        );
        // 최고 관리자는 본인 기록도, 관리자의 기록도 다룬다.
        assert_eq!(edit_personal_records(&owner, &self_subject(&owner)), Ok(()));
        assert_eq!(edit_personal_records(&owner, &self_subject(&admin)), Ok(()));
        assert_eq!(edit_personal_records(&owner, &colleague), Ok(()));
    }

    #[test]
    fn documents_are_overridden_only_by_admins_and_never_their_own() {
        let w = world();
        let other = MemberId::new();
        for role in [Role::Admin, Role::Owner] {
            let admin = actor(role, None, &[]);
            assert_eq!(override_document(&admin, other), Ok(()));
            assert_eq!(
                override_document(&admin, admin.member_id),
                Err(DomainError::Forbidden),
                "{role:?} 본인 문서"
            );
        }
        // 조직장이어도 관리자가 아니면 할 수 없다.
        let head = actor(Role::Member, Some(w.dept_a), &[w.dept_a]);
        assert_eq!(override_document(&head, other), Err(DomainError::Forbidden));
        let member = actor(Role::Member, Some(w.dept_a), &[]);
        assert_eq!(
            override_document(&member, other),
            Err(DomainError::Forbidden)
        );
        assert_eq!(
            override_document(&member, member.member_id),
            Err(DomainError::Forbidden)
        );
    }

    #[test]
    fn admin_cannot_manage_admin_or_owner_accounts() {
        let admin = actor(Role::Admin, None, &[]);
        assert_eq!(manage_account(&admin, &subject(Role::Member, None)), Ok(()));
        assert_eq!(
            manage_account(&admin, &subject(Role::Admin, None)),
            Err(DomainError::Forbidden)
        );
        assert_eq!(
            manage_account(&admin, &subject(Role::Owner, None)),
            Err(DomainError::Forbidden)
        );
        assert_eq!(
            manage_account(&admin, &self_subject(&admin)),
            Err(DomainError::Forbidden),
            "관리자는 관리 API로 자기 계정도 바꿀 수 없다(본인용 API를 쓴다)"
        );
    }

    #[test]
    fn owner_manages_everyone_but_not_self_for_destructive_actions() {
        let owner = actor(Role::Owner, None, &[]);
        for &role in Role::ALL {
            assert_eq!(manage_account(&owner, &subject(role, None)), Ok(()));
            assert_eq!(manage_other_account(&owner, &subject(role, None)), Ok(()));
        }
        assert_eq!(manage_account(&owner, &self_subject(&owner)), Ok(()));
        assert_eq!(
            manage_other_account(&owner, &self_subject(&owner)),
            Err(DomainError::Forbidden)
        );
    }

    #[test]
    fn members_and_managers_cannot_manage_accounts() {
        let w = world();
        let manager = actor(Role::Member, Some(w.dept_a), &[w.dept_a]);
        let target = subject(Role::Member, Some(w.dept_a));
        assert_eq!(
            manage_account(&manager, &target),
            Err(DomainError::Forbidden)
        );
        assert_eq!(require_admin(&manager), Err(DomainError::Forbidden));
        assert_eq!(require_manager(&manager), Ok(()));
        assert_eq!(
            require_manager(&actor(Role::Member, None, &[])),
            Err(DomainError::Forbidden)
        );
        assert_eq!(require_manager(&actor(Role::Admin, None, &[])), Ok(()));
    }

    #[test]
    fn only_owner_changes_roles_and_never_own() {
        let owner = actor(Role::Owner, None, &[]);
        let admin = actor(Role::Admin, None, &[]);
        let target = subject(Role::Member, None);
        assert_eq!(change_role(&owner, &target), Ok(()));
        assert_eq!(change_role(&admin, &target), Err(DomainError::Forbidden));
        assert_eq!(
            change_role(&owner, &self_subject(&owner)),
            Err(DomainError::Forbidden)
        );
    }

    #[test]
    fn department_team_view_hides_unmanaged_departments() {
        let w = world();
        let manager = actor(Role::Member, Some(w.dept_a), &[w.dept_a, w.dept_a_child]);
        assert_eq!(view_department_team(&manager, w.dept_a_child), Ok(()));
        assert_eq!(
            view_department_team(&manager, w.dept_b),
            Err(DomainError::NotFound)
        );
        assert_eq!(
            view_department_team(&actor(Role::Admin, None, &[]), w.dept_b),
            Ok(())
        );
    }

    #[test]
    fn audience_requires_department_exactly_when_targeted() {
        let d = DepartmentId::new();
        assert_eq!(Audience::new(AudienceKind::All, None), Ok(Audience::All));
        assert_eq!(
            Audience::new(AudienceKind::Department, Some(d)),
            Ok(Audience::Department(d))
        );
        assert_eq!(
            Audience::new(AudienceKind::All, Some(d)),
            Err(DomainError::validation(
                "department_id",
                ValidationReason::Inconsistent
            ))
        );
        assert_eq!(
            Audience::new(AudienceKind::Department, None),
            Err(DomainError::validation(
                "department_id",
                ValidationReason::Empty
            ))
        );
        for audience in [Audience::All, Audience::Department(d)] {
            assert_eq!(
                Audience::new(audience.kind(), audience.department_id()),
                Ok(audience)
            );
        }
    }

    #[test]
    fn audience_visibility_matrix() {
        // 본부(hq) ─ 팀(team) ─ 파트(part), 그리고 형제 부서(other).
        let (hq, team, part, other) = (
            DepartmentId::new(),
            DepartmentId::new(),
            DepartmentId::new(),
            DepartmentId::new(),
        );
        let team_member = actor(Role::Member, Some(team), &[]);
        let scope = audience_scope(&team_member, &[team, hq]);
        assert_eq!(view_audience(&scope, &Audience::All), Ok(()));
        assert_eq!(view_audience(&scope, &Audience::Department(team)), Ok(()));
        assert_eq!(
            view_audience(&scope, &Audience::Department(hq)),
            Ok(()),
            "상위 부서 공지는 하위 부서원에게도 보인다"
        );
        for hidden in [part, other] {
            assert_eq!(
                view_audience(&scope, &Audience::Department(hidden)),
                Err(DomainError::NotFound),
                "하위·형제 부서 공지는 보이지 않는다"
            );
        }

        let homeless = actor(Role::Member, None, &[]);
        let scope = audience_scope(&homeless, &[]);
        assert_eq!(view_audience(&scope, &Audience::All), Ok(()));
        assert_eq!(
            view_audience(&scope, &Audience::Department(hq)),
            Err(DomainError::NotFound)
        );

        // 조직장이라도 관리자가 아니면 관할 하위 부서 공지를 따로 보지 않는다.
        let manager = actor(Role::Member, Some(hq), &[hq, team, part]);
        let scope = audience_scope(&manager, &[hq]);
        assert_eq!(
            view_audience(&scope, &Audience::Department(team)),
            Err(DomainError::NotFound)
        );

        let admin = actor(Role::Admin, None, &[]);
        assert_eq!(audience_scope(&admin, &[]), AudienceScope::Everything);
        assert_eq!(
            view_audience(&AudienceScope::Everything, &Audience::Department(other)),
            Ok(())
        );
    }

    #[test]
    fn audience_scope_ignores_ancestors_of_another_department() {
        let (mine, foreign) = (DepartmentId::new(), DepartmentId::new());
        let member = actor(Role::Member, Some(mine), &[]);
        assert_eq!(
            audience_scope(&member, &[foreign]),
            AudienceScope::Departments(BTreeSet::new())
        );
    }

    #[test]
    fn capabilities_follow_role_and_management() {
        let w = world();
        let caps = |a: &Actor| a.capabilities();
        assert_eq!(
            caps(&actor(Role::Member, None, &[])),
            Capabilities {
                admin: false,
                manage_roles: false,
                team: false
            }
        );
        assert_eq!(
            caps(&actor(Role::Member, Some(w.dept_a), &[w.dept_a])),
            Capabilities {
                admin: false,
                manage_roles: false,
                team: true
            }
        );
        assert_eq!(
            caps(&actor(Role::Admin, None, &[])),
            Capabilities {
                admin: true,
                manage_roles: false,
                team: false
            }
        );
        assert_eq!(
            caps(&actor(Role::Owner, None, &[])),
            Capabilities {
                admin: true,
                manage_roles: true,
                team: false
            }
        );
    }

    #[test]
    fn inactive_members_are_hidden_from_non_admins() {
        let w = world();
        let member = actor(Role::Member, Some(w.dept_a), &[]);
        let manager = actor(Role::Member, Some(w.dept_a), &[w.dept_a]);
        let admin = actor(Role::Admin, None, &[]);
        for status in [MemberStatus::Suspended, MemberStatus::Retired] {
            assert_eq!(view_member(&member, status), Err(DomainError::NotFound));
            assert_eq!(view_member(&manager, status), Err(DomainError::NotFound));
            assert_eq!(view_member(&admin, status), Ok(()));
        }
        assert_eq!(view_member(&member, MemberStatus::Active), Ok(()));
        assert!(!view_inactive_members(&manager));
        assert!(view_inactive_members(&actor(Role::Owner, None, &[])));
    }

    #[test]
    fn team_scope_is_all_for_admins_and_managed_for_heads() {
        let w = world();
        let manager = actor(Role::Member, Some(w.dept_a), &[w.dept_a, w.dept_a_child]);
        assert_eq!(
            team_scope(&manager),
            Ok(Some(&BTreeSet::from([w.dept_a, w.dept_a_child])))
        );
        let admin_head = actor(Role::Admin, Some(w.dept_b), &[w.dept_b]);
        assert_eq!(
            team_scope(&admin_head),
            Ok(None),
            "관리자는 관할과 무관하게 전체"
        );
        assert_eq!(
            team_scope(&actor(Role::Member, Some(w.dept_a), &[])),
            Err(DomainError::Forbidden)
        );
    }

    #[test]
    fn last_active_owner_cannot_be_removed() {
        use MemberStatus::{Active, Retired, Suspended};
        let last = Err(ConflictReason::LastOwner.into());
        assert_eq!(
            keep_an_owner((Role::Owner, Active), (Role::Admin, Active), 1),
            last
        );
        assert_eq!(
            keep_an_owner((Role::Owner, Active), (Role::Owner, Suspended), 1),
            last
        );
        assert_eq!(
            keep_an_owner((Role::Owner, Active), (Role::Member, Retired), 0),
            last
        );
        assert_eq!(
            keep_an_owner((Role::Owner, Active), (Role::Admin, Active), 2),
            Ok(())
        );
        assert_eq!(
            keep_an_owner((Role::Owner, Active), (Role::Owner, Active), 1),
            Ok(())
        );
        // 이미 로그인할 수 없는 최고 관리자는 세지 않으므로 바꿔도 남은 수는 그대로다.
        assert_eq!(
            keep_an_owner((Role::Owner, Suspended), (Role::Member, Suspended), 1),
            Ok(())
        );
        assert_eq!(
            keep_an_owner((Role::Admin, Active), (Role::Member, Active), 1),
            Ok(())
        );
        assert_eq!(
            keep_an_owner((Role::Member, Active), (Role::Owner, Active), 0),
            Ok(())
        );
    }

    #[test]
    fn personal_requests_are_resolved_only_by_managing_heads_or_admins() {
        let w = world();
        let requester = subject(Role::Member, Some(w.dept_a_child));
        let colleague = actor(Role::Member, Some(w.dept_a_child), &[]);
        let other_head = actor(Role::Member, Some(w.dept_b), &[w.dept_b]);
        let head = actor(Role::Member, Some(w.dept_a), &[w.dept_a, w.dept_a_child]);
        let admin = actor(Role::Admin, None, &[]);
        assert!(!can_resolve_personal_request(&colleague, &requester));
        assert!(!can_resolve_personal_request(&other_head, &requester));
        assert!(can_resolve_personal_request(&head, &requester));
        assert!(can_resolve_personal_request(&admin, &requester));
        // 본인 신청은 관리자·조직장이어도 승인할 수 없다.
        assert!(!can_resolve_personal_request(&admin, &self_subject(&admin)));
        assert!(!can_resolve_personal_request(&head, &self_subject(&head)));
    }

    #[test]
    fn only_owners_can_delete_other_accounts() {
        let owner = actor(Role::Owner, None, &[]);
        let admin = actor(Role::Admin, None, &[]);
        let member = actor(Role::Member, None, &[]);
        for role in Role::ALL.iter().copied() {
            let target = subject(role, None);
            assert_eq!(delete_account(&owner, &target), Ok(()), "{role:?}");
            assert_eq!(
                delete_account(&admin, &target),
                Err(DomainError::Forbidden),
                "{role:?}"
            );
            assert_eq!(
                delete_account(&member, &target),
                Err(DomainError::Forbidden),
                "{role:?}"
            );
        }
        assert_eq!(
            delete_account(&owner, &self_subject(&owner)),
            Err(DomainError::Forbidden),
            "본인 계정은 삭제할 수 없다"
        );
    }
}
