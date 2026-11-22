//! 구성원: 디렉터리, 팀 구성원, 계정 관리.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use pango_api::auth::SessionInfo;
use pango_api::org::{
    ChangeRoleRequest, ChangeStatusRequest, CreateMemberRequest, IssuedCredentials, MemberDetail,
    MemberQuery, MemberSummary, UpdateMemberRequest,
};
use pango_core::authz::{self, Role};
use pango_core::calendar::check_date;
use pango_core::error::{ConflictReason, ValidationReason};
use pango_core::id::{DepartmentId, MemberId, PositionId, SessionId};
use pango_core::member::{validate_hired_on, validate_retired_on, MemberStatus};
use pango_core::password::PlainPassword;
use pango_core::text::{optional, Email, LoginId, PersonName, Phone, SearchTerm};
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::members::{self, Member, MemberFilter};
use pango_db::{org, sessions, Conn, DbError};

use super::support::{
    audit, current_actor, department_names, found, managed_member, require_updated, Audit,
    AuthContext, Changes, Company,
};
use crate::error::{ServiceError, ServiceResult};
use crate::security::generate_temporary_password;
use crate::state::AppState;

// ── 응답 만들기 ──────────────────────────────────────────────────────────

/// 구성원 목록에 붙일 부서·직책 이름.
struct Names {
    departments: BTreeMap<DepartmentId, String>,
    positions: BTreeMap<PositionId, String>,
}

impl Names {
    async fn load(conn: &mut Conn, members: &[Member]) -> ServiceResult<Self> {
        let mut departments = department_names(conn).await?;
        let mut positions: BTreeMap<_, _> = org::list_positions(conn)
            .await?
            .into_iter()
            .map(|p| (p.id, p.name))
            .collect();
        // 보관된 부서·직책에 남아 있는 퇴사자도 이름은 보여 준다.
        for member in members {
            if let Some(id) = member
                .department_id
                .filter(|id| !departments.contains_key(id))
            {
                if let Some(d) = org::find_department(conn, id).await? {
                    departments.insert(id, d.name);
                }
            }
            if let Some(id) = member.position_id.filter(|id| !positions.contains_key(id)) {
                if let Some(p) = org::find_position(conn, id).await? {
                    positions.insert(id, p.name);
                }
            }
        }
        Ok(Self {
            departments,
            positions,
        })
    }

    fn summary(&self, member: &Member) -> MemberSummary {
        MemberSummary {
            id: member.id,
            name: member.name.clone(),
            email: member.email.clone(),
            phone: member.phone.clone(),
            department_id: member.department_id,
            department_name: member
                .department_id
                .and_then(|id| self.departments.get(&id).cloned()),
            position_id: member.position_id,
            position_name: member
                .position_id
                .and_then(|id| self.positions.get(&id).cloned()),
            status: member.status,
        }
    }

    fn detail(&self, member: &Member) -> MemberDetail {
        MemberDetail {
            summary: self.summary(member),
            login_id: member.login_id.clone(),
            role: member.role,
            hired_on: member.hired_on,
            retired_on: member.retired_on,
            must_change_password: member.must_change_password,
            version: member.version,
        }
    }
}

async fn summaries(conn: &mut Conn, list: &[Member]) -> ServiceResult<Vec<MemberSummary>> {
    let names = Names::load(conn, list).await?;
    Ok(list.iter().map(|m| names.summary(m)).collect())
}

async fn detail(conn: &mut Conn, member: &Member) -> ServiceResult<MemberDetail> {
    Ok(Names::load(conn, std::slice::from_ref(member))
        .await?
        .detail(member))
}

/// 트랜잭션 안에서 다시 읽은 구성원의 상세.
async fn reload_detail(conn: &mut Conn, id: MemberId) -> ServiceResult<MemberDetail> {
    let member = found(members::find(conn, id).await?)?;
    detail(conn, &member).await
}

// ── 조회 ────────────────────────────────────────────────────────────────

/// 목록이 보여 줄 범위. 디렉터리와 관리자 목록이 같은 쿼리를 다르게 해석한다.
struct ListScope {
    /// 정지·퇴사 계정도 포함할지.
    include_inactive: bool,
    /// 검색어를 로그인 아이디와도 비교할지(관리자 목록만).
    search_login_id: bool,
}

fn filter(query: &MemberQuery, scope: ListScope) -> ServiceResult<MemberFilter> {
    let ListScope {
        include_inactive,
        search_login_id,
    } = scope;
    let search = match &query.search {
        Some(raw) => Some(SearchTerm::parse(raw)?.into_string()).filter(|t| !t.is_empty()),
        None => None,
    };
    Ok(MemberFilter {
        statuses: if include_inactive {
            Vec::new()
        } else {
            vec![MemberStatus::Active]
        },
        departments: query.department_id.map(|id| vec![id]),
        search,
        search_login_id,
    })
}

/// 구성원 디렉터리. 누구나 재직자를 볼 수 있고, `include_inactive`는 관리자에게만 적용된다
/// (관리자가 아니면 오류 없이 무시해, 정지·퇴사 계정의 존재를 드러내지 않는다).
pub async fn list_directory(
    state: &AppState,
    ctx: &AuthContext,
    query: &MemberQuery,
) -> ServiceResult<Vec<MemberSummary>> {
    let include_inactive = query.include_inactive && authz::view_inactive_members(&ctx.actor);
    let scope = ListScope {
        include_inactive,
        search_login_id: false,
    };
    let filter = filter(query, scope)?;
    let mut conn = state.read().await?;
    let list = members::list(&mut conn, &filter).await?;
    summaries(&mut conn, &list).await
}

pub async fn get_member(
    state: &AppState,
    ctx: &AuthContext,
    id: MemberId,
) -> ServiceResult<MemberSummary> {
    let mut conn = state.read().await?;
    let member = found(members::find(&mut conn, id).await?)?;
    authz::view_member(&ctx.actor, member.status)?;
    Ok(Names::load(&mut conn, std::slice::from_ref(&member))
        .await?
        .summary(&member))
}

/// 조직장이 관할하는 부서(하위 포함)의 재직자. 관리자는 전체.
pub async fn team_members(
    state: &AppState,
    ctx: &AuthContext,
) -> ServiceResult<Vec<MemberSummary>> {
    let scope = authz::team_scope(&ctx.actor)?;
    let filter = MemberFilter {
        departments: scope.map(|departments| departments.iter().copied().collect()),
        ..MemberFilter::active()
    };
    let mut conn = state.read().await?;
    let list = members::list(&mut conn, &filter).await?;
    summaries(&mut conn, &list).await
}

pub async fn admin_list(
    state: &AppState,
    ctx: &AuthContext,
    query: &MemberQuery,
) -> ServiceResult<Vec<MemberDetail>> {
    authz::require_admin(&ctx.actor)?;
    let scope = ListScope {
        include_inactive: query.include_inactive,
        search_login_id: true,
    };
    let filter = filter(query, scope)?;
    let mut conn = state.read().await?;
    let list = members::list(&mut conn, &filter).await?;
    let names = Names::load(&mut conn, &list).await?;
    Ok(list.iter().map(|m| names.detail(m)).collect())
}

pub async fn admin_get(
    state: &AppState,
    ctx: &AuthContext,
    id: MemberId,
) -> ServiceResult<MemberDetail> {
    authz::require_admin(&ctx.actor)?;
    let mut conn = state.read().await?;
    let member = found(members::find(&mut conn, id).await?)?;
    detail(&mut conn, &member).await
}

// ── 관리 ────────────────────────────────────────────────────────────────

/// 검증을 마친 프로필 입력(생성·수정 공통).
struct Profile {
    name: String,
    email: Option<String>,
    phone: Option<String>,
    department_id: Option<DepartmentId>,
    position_id: Option<PositionId>,
    hired_on: NaiveDate,
}

impl Profile {
    fn parse(
        name: &str,
        email: Option<&str>,
        phone: Option<&str>,
        department_id: Option<DepartmentId>,
        position_id: Option<PositionId>,
        hired_on: NaiveDate,
    ) -> ServiceResult<Self> {
        Ok(Self {
            name: PersonName::parse(name)?.into_string(),
            email: optional(email, Email::parse)?.map(|e| e.as_str().to_owned()),
            phone: optional(phone, Phone::parse)?.map(|p| p.as_str().to_owned()),
            department_id,
            position_id,
            // 저장 표기로 쓸 수 없는 날짜는 다른 확인보다 먼저 거른다. 입사일로 받을 수 있는
            // 기간은 회사 시간대의 오늘이 필요하므로 트랜잭션 안에서 확인한다.
            hired_on: check_date("hired_on", hired_on)?,
        })
    }

    /// 새로 정하는 입사일이 받을 수 있는 기간 안인지. 이미 저장된 값을 그대로 두는 수정은
    /// 확인하지 않는다(예전 값 때문에 이름조차 고칠 수 없게 되지 않도록).
    async fn check_hired_on(
        &self,
        conn: &mut Conn,
        current: Option<&Member>,
        now: Timestamp,
    ) -> ServiceResult<()> {
        if current.is_some_and(|member| member.hired_on == self.hired_on) {
            return Ok(());
        }
        let today = Company::load(conn).await?.today(now);
        validate_hired_on(self.hired_on, today)?;
        Ok(())
    }

    /// 새로 지정하는 부서·직책은 있어야 하고 보관되지 않아야 한다. 이미 갖고 있던 값은
    /// 그대로 둘 수 있다(보관된 부서에 남은 퇴사자의 이름을 고치는 경우).
    async fn check_references(
        &self,
        conn: &mut Conn,
        current: Option<&Member>,
    ) -> ServiceResult<()> {
        if let Some(id) = self.department_id {
            if current.and_then(|m| m.department_id) != Some(id) {
                super::org::active_department(conn, id).await?;
            }
        }
        if let Some(id) = self.position_id {
            if current.and_then(|m| m.position_id) != Some(id) {
                super::org::active_position(conn, id).await?;
            }
        }
        Ok(())
    }
}

fn login_id_taken(error: DbError) -> ServiceError {
    if members::is_duplicate_login_id(&error) {
        DomainError::Conflict(ConflictReason::LoginIdTaken).into()
    } else {
        error.into()
    }
}

/// 임시 비밀번호를 만들고 해시한다. 해시는 비싸므로 트랜잭션 밖에서 한다.
async fn temporary_password(state: &AppState) -> ServiceResult<(String, String)> {
    let plain = generate_temporary_password()?;
    let hash = state
        .passwords
        .hash(&PlainPassword::for_verification(&plain)?)
        .await?;
    Ok((plain, hash))
}

/// 관리자가 구성원 계정을 만든다. 역할은 항상 `member`이고(관리자 임명은 역할 변경으로만),
/// 임시 비밀번호를 한 번만 돌려준다.
///
/// 멱등성 키(`write_idempotent`)는 쓰지 않는다. 저장해 두는 첫 응답에 임시 비밀번호 원문이
/// 남기 때문이다. 대신 로그인 아이디의 유일 제약이 자연스러운 중복 방지 장치가 된다:
/// 같은 요청을 다시 보내면 계정이 하나 더 생기지 않고 409 `login_id_taken`이 된다.
pub async fn create_member(
    state: &AppState,
    ctx: &AuthContext,
    request: &CreateMemberRequest,
) -> ServiceResult<IssuedCredentials> {
    authz::require_admin(&ctx.actor)?;
    let login_id = LoginId::parse(&request.login_id)?;
    let profile = Profile::parse(
        &request.name,
        request.email.as_deref(),
        request.phone.as_deref(),
        request.department_id,
        request.position_id,
        request.hired_on,
    )?;
    let (temporary, hash) = temporary_password(state).await?;
    let now = state.now();
    let member = state
        .write(async |conn: &mut Conn| {
            authz::require_admin(&current_actor(conn, ctx).await?)?;
            profile.check_hired_on(conn, None, now).await?;
            profile.check_references(conn, None).await?;
            let member = Member {
                id: MemberId::new(),
                login_id: login_id.as_str().to_owned(),
                name: profile.name.clone(),
                email: profile.email.clone(),
                phone: profile.phone.clone(),
                department_id: profile.department_id,
                position_id: profile.position_id,
                role: Role::Member,
                status: MemberStatus::Active,
                hired_on: profile.hired_on,
                retired_on: None,
                must_change_password: true,
                created_at: now,
                version: 1,
            };
            members::insert(conn, &member, &hash, now)
                .await
                .map_err(login_id_taken)?;
            audit(
                conn,
                now,
                Audit {
                    action: "member.create",
                    target_type: "member",
                    target_id: Some(member.id.to_string()),
                    detail: Some(serde_json::json!({
                        "login_id": member.login_id,
                        "role": member.role,
                        "department_id": member.department_id,
                        "position_id": member.position_id,
                    })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            detail(conn, &member).await
        })
        .await?;
    Ok(IssuedCredentials {
        member,
        temporary_password: temporary,
    })
}

/// 프로필·소속 수정. 역할·상태·비밀번호는 각각의 전용 동작으로만 바꾼다.
pub async fn update_member(
    state: &AppState,
    ctx: &AuthContext,
    id: MemberId,
    request: &UpdateMemberRequest,
) -> ServiceResult<MemberDetail> {
    authz::require_admin(&ctx.actor)?;
    let profile = Profile::parse(
        &request.name,
        request.email.as_deref(),
        request.phone.as_deref(),
        request.department_id,
        request.position_id,
        request.hired_on,
    )?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let actor = current_actor(conn, ctx).await?;
            authz::require_admin(&actor)?;
            let current = found(members::find(conn, id).await?)?;
            authz::manage_account(&actor, &current.subject())?;
            // 낡은 화면에서 보낸 요청이 다른 검증 오류로 보이지 않도록 먼저 비교한다.
            require_updated(current.version == request.version)?;
            profile.check_hired_on(conn, Some(&current), now).await?;
            profile.check_references(conn, Some(&current)).await?;
            if current
                .retired_on
                .is_some_and(|retired| profile.hired_on > retired)
            {
                return Err(
                    DomainError::validation("hired_on", ValidationReason::Inconsistent).into(),
                );
            }
            let next = Member {
                name: profile.name.clone(),
                email: profile.email.clone(),
                phone: profile.phone.clone(),
                department_id: profile.department_id,
                position_id: profile.position_id,
                hired_on: profile.hired_on,
                ..current.clone()
            };
            let mut changes = Changes::default();
            changes
                .field("name", &current.name, &next.name)
                .field("email", &current.email, &next.email)
                .field("phone", &current.phone, &next.phone)
                .field("department_id", &current.department_id, &next.department_id)
                .field("position_id", &current.position_id, &next.position_id)
                .field("hired_on", &current.hired_on, &next.hired_on);
            if changes.is_empty() {
                return detail(conn, &current).await;
            }
            require_updated(members::update(conn, &next, now).await?)?;
            audit(
                conn,
                now,
                Audit {
                    action: "member.update",
                    target_type: "member",
                    target_id: Some(id.to_string()),
                    detail: Some(changes.into_detail()),
                    ctx: Some(ctx),
                },
            )
            .await?;
            reload_detail(conn, id).await
        })
        .await
}

/// 재직·정지·퇴사 전환.
///
/// 로그인할 수 없는 상태가 되면 같은 트랜잭션에서 모든 세션을 폐기하고 조직장 지정을 푼다.
/// 이미 그 상태면(퇴사일을 새로 주지 않았다면) 아무것도 바꾸지 않고 성공한다(자연 멱등).
pub async fn change_status(
    state: &AppState,
    ctx: &AuthContext,
    id: MemberId,
    request: &ChangeStatusRequest,
) -> ServiceResult<MemberDetail> {
    authz::require_admin(&ctx.actor)?;
    let retiring = request.status == MemberStatus::Retired;
    if request.retired_on.is_some() && !retiring {
        return Err(DomainError::validation("retired_on", ValidationReason::Inconsistent).into());
    }
    if let Some(date) = request.retired_on {
        check_date("retired_on", date)?;
    }
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let actor = current_actor(conn, ctx).await?;
            authz::require_admin(&actor)?;
            let current = found(members::find(conn, id).await?)?;
            authz::manage_other_account(&actor, &current.subject())?;
            let repeated = current.status == request.status
                && (request.retired_on.is_none() || request.retired_on == current.retired_on);
            if repeated {
                return detail(conn, &current).await;
            }
            require_updated(current.version == request.version)?;
            let retired_on = match (retiring, request.retired_on) {
                (false, _) => None,
                (true, Some(date)) => Some(date),
                (true, None) => Some(Company::load(conn).await?.today(now)),
            };
            if let Some(date) = retired_on {
                validate_retired_on(date, current.hired_on)?;
            }
            let active_owners = members::count_active_owners(conn).await?;
            authz::keep_an_owner(
                (current.role, current.status),
                (current.role, request.status),
                active_owners,
            )?;
            // 퇴사자만 남은 부서·직책은 보관할 수 있다. 그 사람을 다시 로그인할 수 있는 상태로
            // 돌리면 보관된 소속에 남지 않도록 소속을 비운다(관리자가 새로 지정한다).
            let (mut department_id, mut position_id) = (current.department_id, current.position_id);
            if request.status.can_sign_in() && !current.status.can_sign_in() {
                if let Some(id) = department_id {
                    if org::find_department(conn, id)
                        .await?
                        .is_none_or(|d| d.archived_at.is_some())
                    {
                        department_id = None;
                    }
                }
                if let Some(id) = position_id {
                    if org::find_position(conn, id)
                        .await?
                        .is_none_or(|p| p.archived_at.is_some())
                    {
                        position_id = None;
                    }
                }
            }
            let next = Member {
                status: request.status,
                retired_on,
                department_id,
                position_id,
                ..current.clone()
            };
            require_updated(members::update(conn, &next, now).await?)?;
            let (revoked_sessions, cleared_heads) = if next.status.can_sign_in() {
                (0, 0)
            } else {
                (
                    sessions::revoke_all(conn, id, None, now).await?,
                    org::clear_head(conn, id, now).await?,
                )
            };
            audit(
                conn,
                now,
                Audit {
                    action: "member.status",
                    target_type: "member",
                    target_id: Some(id.to_string()),
                    detail: Some(serde_json::json!({
                        "from": current.status,
                        "to": next.status,
                        "retired_on": next.retired_on,
                        "revoked_sessions": revoked_sessions,
                        "cleared_heads": cleared_heads,
                        "cleared_department": current.department_id != next.department_id,
                        "cleared_position": current.position_id != next.position_id,
                    })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            reload_detail(conn, id).await
        })
        .await
}

/// 역할 변경(최고 관리자만, 본인 제외). 같은 역할이면 아무것도 바꾸지 않고 성공한다.
pub async fn change_role(
    state: &AppState,
    ctx: &AuthContext,
    id: MemberId,
    request: &ChangeRoleRequest,
) -> ServiceResult<MemberDetail> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let actor = current_actor(conn, ctx).await?;
            authz::require_admin(&actor)?;
            let current = found(members::find(conn, id).await?)?;
            authz::change_role(&actor, &current.subject())?;
            let next = Member {
                role: request.role,
                ..current.clone()
            };
            if next == current {
                return detail(conn, &current).await;
            }
            require_updated(current.version == request.version)?;
            let active_owners = members::count_active_owners(conn).await?;
            authz::keep_an_owner(
                (current.role, current.status),
                (next.role, next.status),
                active_owners,
            )?;
            require_updated(members::update(conn, &next, now).await?)?;
            audit(
                conn,
                now,
                Audit {
                    action: "member.role",
                    target_type: "member",
                    target_id: Some(id.to_string()),
                    detail: Some(serde_json::json!({ "from": current.role, "to": next.role })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            reload_detail(conn, id).await
        })
        .await
}

/// 비밀번호 초기화: 새 임시 비밀번호를 발급하고 모든 세션을 폐기한다.
///
/// 본인 계정에는 쓸 수 없다. 탈취한 세션만으로 현재 비밀번호 없이 새 비밀번호를 얻어
/// 계정을 빼앗는 것을 막기 위해서다(본인은 `/me/password`를 쓴다).
pub async fn reset_password(
    state: &AppState,
    ctx: &AuthContext,
    id: MemberId,
) -> ServiceResult<IssuedCredentials> {
    authz::require_admin(&ctx.actor)?;
    let (temporary, hash) = temporary_password(state).await?;
    let now = state.now();
    let member = state
        .write(async |conn: &mut Conn| {
            let actor = current_actor(conn, ctx).await?;
            authz::require_admin(&actor)?;
            let current = found(members::find(conn, id).await?)?;
            authz::manage_other_account(&actor, &current.subject())?;
            members::set_password(conn, id, &hash, true, now).await?;
            let revoked_sessions = sessions::revoke_all(conn, id, None, now).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "member.password_reset",
                    target_type: "member",
                    target_id: Some(id.to_string()),
                    detail: Some(serde_json::json!({ "revoked_sessions": revoked_sessions })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            reload_detail(conn, id).await
        })
        .await?;
    Ok(IssuedCredentials {
        member,
        temporary_password: temporary,
    })
}

// ── 로그인 기기(세션) ───────────────────────────────────────────────────

/// 구성원의 로그인 기기(살아 있는 세션, 최근 사용 순). 계정 관리 권한이 있어야 한다
/// (`authz::manage_account`: 관리자는 일반 구성원만, 최고 관리자는 전부).
pub async fn list_member_sessions(
    state: &AppState,
    ctx: &AuthContext,
    id: MemberId,
) -> ServiceResult<Vec<SessionInfo>> {
    authz::require_admin(&ctx.actor)?;
    let mut conn = state.read().await?;
    managed_member(&mut conn, ctx, id, authz::manage_account).await?;
    let list = sessions::list_active(&mut conn, id, state.now()).await?;
    Ok(list
        .into_iter()
        .map(|s| SessionInfo {
            id: s.id,
            current: s.id == ctx.session_id,
            user_agent: s.user_agent,
            ip: s.ip,
            created_at: s.created_at,
            last_seen_at: s.last_seen_at,
        })
        .collect())
}

/// 구성원의 기기 하나를 로그아웃시킨다. 이미 끊긴 그 구성원의 세션이면 성공(멱등),
/// 다른 구성원의 세션이면 `NotFound`.
pub async fn revoke_member_session(
    state: &AppState,
    ctx: &AuthContext,
    id: MemberId,
    session_id: SessionId,
) -> ServiceResult<()> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            managed_member(conn, ctx, id, authz::manage_account).await?;
            if !sessions::belongs_to(conn, id, session_id).await? {
                return Err(DomainError::NotFound.into());
            }
            if sessions::revoke(conn, id, session_id, now).await? {
                audit(
                    conn,
                    now,
                    Audit {
                        action: "member.session_revoke",
                        target_type: "member",
                        target_id: Some(id.to_string()),
                        detail: Some(serde_json::json!({ "session_id": session_id })),
                        ctx: Some(ctx),
                    },
                )
                .await?;
            }
            Ok(())
        })
        .await
}

/// 구성원의 모든 기기를 로그아웃시킨다(분실 등). 끊은 세션 수를 돌려준다.
///
/// 본인 계정(최고 관리자가 자기 상세를 볼 때)이면 지금 쓰는 세션은 남긴다.
pub async fn revoke_member_sessions(
    state: &AppState,
    ctx: &AuthContext,
    id: MemberId,
) -> ServiceResult<u64> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            managed_member(conn, ctx, id, authz::manage_account).await?;
            let keep = (id == ctx.member_id()).then_some(ctx.session_id);
            let revoked = sessions::revoke_all(conn, id, keep, now).await?;
            if revoked > 0 {
                audit(
                    conn,
                    now,
                    Audit {
                        action: "member.sessions_revoke_all",
                        target_type: "member",
                        target_id: Some(id.to_string()),
                        detail: Some(serde_json::json!({ "revoked_sessions": revoked })),
                        ctx: Some(ctx),
                    },
                )
                .await?;
            }
            Ok(revoked)
        })
        .await
}
