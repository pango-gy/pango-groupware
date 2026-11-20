//! 캘린더 열람·쓰기 권한. 캘린더의 역할 비교는 이 모듈에서만 한다.
//!
//! | 캘린더 | 열람 | 쓰기 |
//! | --- | --- | --- |
//! | 내 일정(`personal`) | 본인만 (관리자도 못 본다) | 본인 |
//! | 전사 일정(`company`) | 모두 | 관리자 |
//! | 부서 일정(`department`) | 그 부서·하위 부서 소속, 관할 조직장, 관리자 | 관할 조직장, 관리자 |
//!
//! 다른 사람의 개인 일정은 **초대받은 참석자**에게만 보인다([`event_role`]). 판정은
//! [`crate::authz`]와 같은 규칙을 따른다: 보이지 않으면 `NotFound`, 보이지만 금지된 동작이면
//! `Forbidden`.

use crate::authz::Actor;
use crate::error::{DomainError, DomainResult};
use crate::id::{CalendarId, DepartmentId, MemberId};
use crate::string_enum;

string_enum! {
    /// 캘린더 종류.
    pub enum CalendarKind("kind") {
        /// 구성원마다 하나인 내 일정.
        Personal => "personal",
        /// 회사에 하나인 전사 일정.
        Company => "company",
        /// 부서마다 하나인 부서 일정.
        Department => "department",
    }
}

string_enum! {
    /// 일정 하나에 대한 요청자의 자격.
    pub enum EventRole("my_role") {
        /// 수정·삭제할 수 있다(개인 일정의 주인, 공유 캘린더의 쓰기 권한자).
        Owner => "owner",
        /// 초대받은 참석자. 보고 응답할 수 있지만 고칠 수 없다.
        Attendee => "attendee",
        /// 읽기만 한다(공유 캘린더의 열람자).
        Viewer => "viewer",
    }
}

/// 전사 일정 캘린더의 고정 ID. 마이그레이션 `0005_calendar.sql`이 이 ID로 행을 만든다.
pub const COMPANY_CALENDAR_ID: CalendarId = CalendarId::from_uuid(uuid::Uuid::from_u128(
    0x0000_0000_0000_7000_8000_0000_0000_0001,
));

/// 캘린더 하나. ID는 주인에게서 정해진다: 내 일정은 구성원 ID, 부서 일정은 부서 ID와 같은
/// UUID이고, 전사 일정은 [`COMPANY_CALENDAR_ID`]다. 그래서 캘린더 행이 아직 없어도(첫 일정을
/// 만들 때 생긴다) 목록과 권한 판정에 쓸 ID가 있다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Calendar {
    Personal(MemberId),
    Company,
    Department(DepartmentId),
}

impl Calendar {
    pub fn id(&self) -> CalendarId {
        match self {
            Self::Personal(owner) => CalendarId::from_uuid(owner.as_uuid()),
            Self::Company => COMPANY_CALENDAR_ID,
            Self::Department(department) => CalendarId::from_uuid(department.as_uuid()),
        }
    }

    pub const fn kind(&self) -> CalendarKind {
        match self {
            Self::Personal(_) => CalendarKind::Personal,
            Self::Company => CalendarKind::Company,
            Self::Department(_) => CalendarKind::Department,
        }
    }

    pub const fn owner_id(&self) -> Option<MemberId> {
        match self {
            Self::Personal(owner) => Some(*owner),
            _ => None,
        }
    }

    pub const fn department_id(&self) -> Option<DepartmentId> {
        match self {
            Self::Department(department) => Some(*department),
            _ => None,
        }
    }
}

/// 요청자 소속 부서의 조상 목록(자신 포함)이 정말 그 부서의 것인지. 다른 부서의 목록이면
/// 쓰지 않는다(호출 실수로 열람 범위가 넓어지지 않게).
fn own_chain<'a>(
    actor: &Actor,
    own_department_ancestors: &'a [DepartmentId],
) -> &'a [DepartmentId] {
    let matches = actor
        .department_id
        .is_some_and(|own| own_department_ancestors.first() == Some(&own));
    if matches {
        own_department_ancestors
    } else {
        &[]
    }
}

/// 캘린더를 볼 수 있는지. 볼 수 없으면 존재를 숨긴다(`NotFound`).
///
/// `own_department_ancestors`는 요청자 소속 부서의 조상 목록(자신 → 루트)이다. 부서 일정은
/// 그 부서나 하위 부서에 속한 구성원이 본다(본부 일정은 팀원도 본다).
pub fn can_read(
    actor: &Actor,
    calendar: &Calendar,
    own_department_ancestors: &[DepartmentId],
) -> DomainResult<()> {
    let visible = match calendar {
        Calendar::Personal(owner) => actor.member_id == *owner,
        Calendar::Company => true,
        Calendar::Department(department) => {
            actor.is_admin()
                || actor.manages_department(*department)
                || own_chain(actor, own_department_ancestors).contains(department)
        }
    };
    if visible {
        Ok(())
    } else {
        Err(DomainError::NotFound)
    }
}

/// 캘린더에 일정을 만들거나 고칠 수 있는지. 볼 수 없으면 `NotFound`, 보이지만 읽기 전용이면
/// `Forbidden`.
pub fn can_write(
    actor: &Actor,
    calendar: &Calendar,
    own_department_ancestors: &[DepartmentId],
) -> DomainResult<()> {
    can_read(actor, calendar, own_department_ancestors)?;
    let writable = match calendar {
        Calendar::Personal(owner) => actor.member_id == *owner,
        Calendar::Company => actor.is_admin(),
        Calendar::Department(department) => {
            actor.is_admin() || actor.manages_department(*department)
        }
    };
    if writable {
        Ok(())
    } else {
        Err(DomainError::Forbidden)
    }
}

/// 일정 하나에 대한 요청자의 자격. 볼 수 없으면 `NotFound`.
///
/// 쓰기 권한이 있으면 [`EventRole::Owner`], 초대받았으면 [`EventRole::Attendee`], 캘린더를 볼
/// 수 있으면 [`EventRole::Viewer`]. 다른 사람의 개인 일정은 초대받은 경우에만 보인다.
pub fn event_role(
    actor: &Actor,
    calendar: &Calendar,
    own_department_ancestors: &[DepartmentId],
    is_attendee: bool,
) -> DomainResult<EventRole> {
    match can_write(actor, calendar, own_department_ancestors) {
        Ok(()) => Ok(EventRole::Owner),
        Err(_) if is_attendee => Ok(EventRole::Attendee),
        Err(DomainError::Forbidden) => Ok(EventRole::Viewer),
        Err(error) => Err(error),
    }
}

/// 일정을 고치거나 지울 수 있는지. 볼 수 있는 일정에 대해서만 부른다(참석자·열람자는 403).
pub fn edit_event(role: EventRole) -> DomainResult<()> {
    match role {
        EventRole::Owner => Ok(()),
        EventRole::Attendee | EventRole::Viewer => Err(DomainError::Forbidden),
    }
}

/// 참석 응답을 할 수 있는지: 초대받은 참석자만(주인·열람자는 403).
pub fn respond_to_event(role: EventRole) -> DomainResult<()> {
    match role {
        EventRole::Attendee => Ok(()),
        EventRole::Owner | EventRole::Viewer => Err(DomainError::Forbidden),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::Role;

    /// 본부 ─ 팀, 형제 부서.
    struct World {
        hq: DepartmentId,
        team: DepartmentId,
        other: DepartmentId,
    }

    fn world() -> World {
        World {
            hq: DepartmentId::new(),
            team: DepartmentId::new(),
            other: DepartmentId::new(),
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

    /// 판정 결과를 짧은 표기로: 쓰기 가능 `"rw"`, 읽기만 `"r"`, 안 보임 `"-"`.
    fn access(actor: &Actor, calendar: &Calendar, ancestors: &[DepartmentId]) -> &'static str {
        match (
            can_read(actor, calendar, ancestors),
            can_write(actor, calendar, ancestors),
        ) {
            (Ok(()), Ok(())) => "rw",
            (Ok(()), Err(DomainError::Forbidden)) => "r",
            (Err(DomainError::NotFound), Err(DomainError::NotFound)) => "-",
            other => panic!("inconsistent decisions: {other:?}"),
        }
    }

    #[test]
    fn kind_and_role_wire_strings_are_fixed() {
        let kinds: Vec<_> = CalendarKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(kinds, ["personal", "company", "department"]);
        let roles: Vec<_> = EventRole::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(roles, ["owner", "attendee", "viewer"]);
        assert!(CalendarKind::parse("Company").is_err());
    }

    #[test]
    fn calendar_ids_follow_their_owner() {
        let member = MemberId::new();
        let department = DepartmentId::new();
        assert_eq!(Calendar::Personal(member).id().as_uuid(), member.as_uuid());
        assert_eq!(
            Calendar::Department(department).id().as_uuid(),
            department.as_uuid()
        );
        assert_eq!(Calendar::Company.id(), COMPANY_CALENDAR_ID);
        assert_eq!(
            COMPANY_CALENDAR_ID.to_string(),
            "00000000-0000-7000-8000-000000000001"
        );
        assert_eq!(
            CalendarId::parse("00000000-0000-7000-8000-000000000001"),
            Ok(COMPANY_CALENDAR_ID)
        );
    }

    #[test]
    fn personal_calendar_is_private_to_its_owner_for_every_role() {
        let w = world();
        let owner = actor(Role::Member, Some(w.team), &[]);
        let calendar = Calendar::Personal(owner.member_id);
        assert_eq!(access(&owner, &calendar, &[w.team, w.hq]), "rw");
        let others = [
            ("colleague", actor(Role::Member, Some(w.team), &[])),
            ("head", actor(Role::Member, Some(w.hq), &[w.hq, w.team])),
            ("other department", actor(Role::Member, Some(w.other), &[])),
            ("admin", actor(Role::Admin, None, &[])),
            ("owner role", actor(Role::Owner, None, &[])),
        ];
        for (label, other) in others {
            assert_eq!(access(&other, &calendar, &[]), "-", "{label}");
        }
    }

    #[test]
    fn owners_personal_calendar_is_writable_whatever_their_role() {
        for role in Role::ALL {
            let me = actor(*role, None, &[]);
            assert_eq!(access(&me, &Calendar::Personal(me.member_id), &[]), "rw");
        }
    }

    #[test]
    fn company_calendar_is_read_by_all_and_written_by_admins() {
        let w = world();
        let cases = [
            ("member", actor(Role::Member, Some(w.team), &[]), "r"),
            ("no department", actor(Role::Member, None, &[]), "r"),
            (
                "head",
                actor(Role::Member, Some(w.hq), &[w.hq, w.team]),
                "r",
            ),
            ("admin", actor(Role::Admin, None, &[]), "rw"),
            ("owner role", actor(Role::Owner, Some(w.other), &[]), "rw"),
        ];
        for (label, who, expected) in cases {
            assert_eq!(access(&who, &Calendar::Company, &[]), expected, "{label}");
        }
    }

    #[test]
    fn department_calendar_matrix() {
        let w = world();
        let team = Calendar::Department(w.team);
        let hq = Calendar::Department(w.hq);
        let team_member = actor(Role::Member, Some(w.team), &[]);
        let team_chain = [w.team, w.hq];
        // 소속 부서와 상위 부서의 일정은 읽는다.
        assert_eq!(access(&team_member, &team, &team_chain), "r");
        assert_eq!(access(&team_member, &hq, &team_chain), "r");
        // 상위 부서 소속은 하위 부서 일정을 (관할이 아니면) 보지 못한다.
        let hq_member = actor(Role::Member, Some(w.hq), &[]);
        assert_eq!(access(&hq_member, &team, &[w.hq]), "-");
        assert_eq!(access(&hq_member, &hq, &[w.hq]), "r");
        // 형제 부서는 보지 못한다.
        let outsider = actor(Role::Member, Some(w.other), &[]);
        assert_eq!(access(&outsider, &team, &[w.other]), "-");
        assert_eq!(access(&actor(Role::Member, None, &[]), &team, &[]), "-");
        // 조직장은 관할 부서(하위 포함)에 쓴다. 관할 밖은 소속 규칙대로.
        let hq_head = actor(Role::Member, Some(w.hq), &[w.hq, w.team]);
        assert_eq!(access(&hq_head, &team, &[w.hq]), "rw");
        assert_eq!(access(&hq_head, &hq, &[w.hq]), "rw");
        assert_eq!(
            access(&hq_head, &Calendar::Department(w.other), &[w.hq]),
            "-"
        );
        let team_head = actor(Role::Member, Some(w.team), &[w.team]);
        assert_eq!(access(&team_head, &team, &team_chain), "rw");
        assert_eq!(access(&team_head, &hq, &team_chain), "r");
        // 관리자는 모든 부서 일정에 쓴다.
        for role in [Role::Admin, Role::Owner] {
            let admin = actor(role, None, &[]);
            assert_eq!(access(&admin, &team, &[]), "rw");
            assert_eq!(access(&admin, &Calendar::Department(w.other), &[]), "rw");
        }
    }

    #[test]
    fn ancestors_of_another_department_are_ignored() {
        let w = world();
        let outsider = actor(Role::Member, Some(w.other), &[]);
        // 호출 실수로 다른 부서의 조상 목록을 넘겨도 범위가 넓어지지 않는다.
        assert_eq!(
            access(&outsider, &Calendar::Department(w.team), &[w.team, w.hq]),
            "-"
        );
    }

    #[test]
    fn event_role_prefers_write_then_invitation_then_read() {
        let w = world();
        let owner = actor(Role::Member, Some(w.team), &[]);
        let personal = Calendar::Personal(owner.member_id);
        let guest = actor(Role::Member, Some(w.other), &[]);
        let admin = actor(Role::Admin, None, &[]);
        assert_eq!(
            event_role(&owner, &personal, &[], false),
            Ok(EventRole::Owner)
        );
        assert_eq!(
            event_role(&guest, &personal, &[], true),
            Ok(EventRole::Attendee)
        );
        assert_eq!(
            event_role(&guest, &personal, &[], false),
            Err(DomainError::NotFound)
        );
        // 관리자도 초대받지 않은 개인 일정은 보지 못한다.
        assert_eq!(
            event_role(&admin, &personal, &[], false),
            Err(DomainError::NotFound)
        );
        assert_eq!(
            event_role(&guest, &Calendar::Company, &[], false),
            Ok(EventRole::Viewer)
        );
        assert_eq!(
            event_role(&admin, &Calendar::Company, &[], false),
            Ok(EventRole::Owner)
        );
        assert_eq!(
            event_role(&guest, &Calendar::Department(w.team), &[w.other], false),
            Err(DomainError::NotFound)
        );
    }

    #[test]
    fn only_owners_edit_and_only_attendees_respond() {
        assert_eq!(edit_event(EventRole::Owner), Ok(()));
        assert_eq!(edit_event(EventRole::Attendee), Err(DomainError::Forbidden));
        assert_eq!(edit_event(EventRole::Viewer), Err(DomainError::Forbidden));
        assert_eq!(respond_to_event(EventRole::Attendee), Ok(()));
        assert_eq!(
            respond_to_event(EventRole::Owner),
            Err(DomainError::Forbidden)
        );
        assert_eq!(
            respond_to_event(EventRole::Viewer),
            Err(DomainError::Forbidden)
        );
    }
}
