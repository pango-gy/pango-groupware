//! 부서·직책: 조회와 관리.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

use pango_api::org::{DepartmentDto, DepartmentInput, PositionDto, PositionInput};
use pango_api::Versioned;
use pango_core::authz;
use pango_core::error::{ConflictReason, ValidationReason};
use pango_core::id::{DepartmentId, MemberId, PositionId};
use pango_core::text::LabelName;
use pango_core::DomainError;
use pango_db::org::{self, Department, Position};
use pango_db::{members, Conn, DbError};

use super::support::{
    audit, count_u32, current_actor, department_tree, found, member_names, require_updated, Audit,
    AuthContext, Changes, Idempotency,
};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 부서 정렬 값의 범위. 화면 정렬용이라 넉넉히 두되, 극단값이 섞여 정렬이 깨지지 않게 한다.
const SORT_ORDER_RANGE: RangeInclusive<i64> = -1_000_000..=1_000_000;
/// 직책 등급의 범위. DB의 CHECK 제약과 같다.
const LEVEL_RANGE: RangeInclusive<i64> = 0..=1000;

fn in_range(field: &'static str, value: i64, range: RangeInclusive<i64>) -> ServiceResult<i64> {
    if range.contains(&value) {
        Ok(value)
    } else {
        Err(DomainError::validation(field, ValidationReason::OutOfRange).into())
    }
}

/// 검증을 마친 부서 입력.
struct DepartmentFields {
    name: String,
    parent_id: Option<DepartmentId>,
    head_member_id: Option<MemberId>,
    sort_order: i64,
}

impl DepartmentFields {
    fn parse(input: &DepartmentInput) -> ServiceResult<Self> {
        Ok(Self {
            name: LabelName::parse(&input.name)?.into_string(),
            parent_id: input.parent_id,
            head_member_id: input.head_member_id,
            sort_order: in_range("sort_order", input.sort_order, SORT_ORDER_RANGE)?,
        })
    }
}

/// 보관되지 않은 부서. 보관된 부서는 관리 대상에서 사라진 것으로 보고 `NotFound`.
pub(super) async fn active_department(
    conn: &mut Conn,
    id: DepartmentId,
) -> ServiceResult<Department> {
    found(
        org::find_department(conn, id)
            .await?
            .filter(|d| d.archived_at.is_none()),
    )
}

/// 조직장은 지금 일할 수 있는 구성원이어야 한다. 관할 권한이 곧바로 생기기 때문이다.
async fn require_head(conn: &mut Conn, head: Option<MemberId>) -> ServiceResult<()> {
    if let Some(id) = head {
        found(members::find_existing(conn, id).await?)?
            .status
            .require_active()?;
    }
    Ok(())
}

async fn department_dtos(
    conn: &mut Conn,
    departments: Vec<Department>,
) -> ServiceResult<Vec<DepartmentDto>> {
    let counts: BTreeMap<DepartmentId, i64> = members::count_active_by_department(conn)
        .await?
        .into_iter()
        .collect();
    let heads = member_names(conn, departments.iter().filter_map(|d| d.head_member_id)).await?;
    departments
        .into_iter()
        .map(|d| {
            let count = counts.get(&d.id).copied().unwrap_or(0);
            Ok(DepartmentDto {
                id: d.id,
                parent_id: d.parent_id,
                head_name: d.head_member_id.and_then(|id| heads.get(&id).cloned()),
                head_member_id: d.head_member_id,
                name: d.name,
                sort_order: d.sort_order,
                member_count: count_u32(count)?,
                version: d.version,
            })
        })
        .collect()
}

async fn department_dto(conn: &mut Conn, department: Department) -> ServiceResult<DepartmentDto> {
    department_dtos(conn, vec![department])
        .await?
        .pop()
        .ok_or_else(|| ServiceError::internal("department dto missing"))
}

/// 보관되지 않은 부서 전체(트리는 `parent_id`로 클라이언트가 그린다).
pub async fn list_departments(state: &AppState) -> ServiceResult<Vec<DepartmentDto>> {
    let mut conn = state.read().await?;
    let departments = org::list_departments(&mut conn).await?;
    department_dtos(&mut conn, departments).await
}

pub async fn create_department(
    state: &AppState,
    ctx: &AuthContext,
    input: &DepartmentInput,
    idempotency: Option<&Idempotency>,
) -> ServiceResult<DepartmentDto> {
    authz::require_admin(&ctx.actor)?;
    let fields = DepartmentFields::parse(input)?;
    let now = state.now();
    state
        .write_idempotent(ctx.member_id(), idempotency, async |conn: &mut Conn| {
            authz::require_admin(&current_actor(conn, ctx).await?)?;
            department_tree(conn)
                .await?
                .check_insert(fields.parent_id)?;
            require_head(conn, fields.head_member_id).await?;
            let department = Department {
                id: DepartmentId::new(),
                parent_id: fields.parent_id,
                name: fields.name.clone(),
                head_member_id: fields.head_member_id,
                sort_order: fields.sort_order,
                archived_at: None,
                version: 1,
            };
            org::insert_department(conn, &department, now).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "department.create",
                    target_type: "department",
                    target_id: Some(department.id.to_string()),
                    detail: Some(serde_json::json!({
                        "name": department.name,
                        "parent_id": department.parent_id,
                        "head_member_id": department.head_member_id,
                    })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            department_dto(conn, department).await
        })
        .await
}

/// 부서 수정. 입력은 전체 값이다(생략한 `parent_id`·`head_member_id`는 해제).
pub async fn update_department(
    state: &AppState,
    ctx: &AuthContext,
    id: DepartmentId,
    request: &Versioned<DepartmentInput>,
) -> ServiceResult<DepartmentDto> {
    authz::require_admin(&ctx.actor)?;
    let fields = DepartmentFields::parse(&request.data)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            authz::require_admin(&current_actor(conn, ctx).await?)?;
            let current = active_department(conn, id).await?;
            // 낡은 화면에서 보낸 요청이 다른 검증 오류로 보이지 않도록 먼저 비교한다.
            require_updated(current.version == request.version)?;
            let next = Department {
                parent_id: fields.parent_id,
                name: fields.name.clone(),
                head_member_id: fields.head_member_id,
                sort_order: fields.sort_order,
                ..current.clone()
            };
            let mut changes = Changes::default();
            changes
                .field("name", &current.name, &next.name)
                .field("parent_id", &current.parent_id, &next.parent_id)
                .field(
                    "head_member_id",
                    &current.head_member_id,
                    &next.head_member_id,
                )
                .field("sort_order", &current.sort_order, &next.sort_order);
            if changes.is_empty() {
                return department_dto(conn, current).await;
            }
            if next.parent_id != current.parent_id {
                department_tree(conn)
                    .await?
                    .check_move(id, next.parent_id)?;
            }
            if next.head_member_id != current.head_member_id {
                require_head(conn, next.head_member_id).await?;
            }
            require_updated(org::update_department(conn, &next, now).await?)?;
            audit(
                conn,
                now,
                Audit {
                    action: "department.update",
                    target_type: "department",
                    target_id: Some(id.to_string()),
                    detail: Some(changes.into_detail()),
                    ctx: Some(ctx),
                },
            )
            .await?;
            let updated = active_department(conn, id).await?;
            department_dto(conn, updated).await
        })
        .await
}

/// 부서 보관(삭제 대신). 재직·정지 구성원이나 보관되지 않은 하위 부서가 있으면 거부한다.
/// 퇴사자는 기록 보존을 위해 보관된 부서를 계속 가리킨다.
pub async fn archive_department(
    state: &AppState,
    ctx: &AuthContext,
    id: DepartmentId,
    version: i64,
) -> ServiceResult<()> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            authz::require_admin(&current_actor(conn, ctx).await?)?;
            let current = active_department(conn, id).await?;
            require_updated(current.version == version)?;
            let has_members = members::count_in_department(conn, id).await? > 0;
            let has_children = department_tree(conn).await?.descendants_inclusive(id).len() > 1;
            if has_members || has_children {
                return Err(DomainError::Conflict(ConflictReason::DepartmentNotEmpty).into());
            }
            let archived = Department {
                archived_at: Some(now),
                ..current.clone()
            };
            require_updated(org::update_department(conn, &archived, now).await?)?;
            audit(
                conn,
                now,
                Audit {
                    action: "department.archive",
                    target_type: "department",
                    target_id: Some(id.to_string()),
                    detail: Some(serde_json::json!({ "name": current.name })),
                    ctx: Some(ctx),
                },
            )
            .await
        })
        .await
}

// ── 직책 ─────────────────────────────────────────────────────────────────

struct PositionFields {
    name: String,
    level: i64,
}

impl PositionFields {
    fn parse(input: &PositionInput) -> ServiceResult<Self> {
        Ok(Self {
            name: LabelName::parse(&input.name)?.into_string(),
            level: in_range("level", input.level, LEVEL_RANGE)?,
        })
    }
}

fn position_dto(position: Position) -> PositionDto {
    PositionDto {
        id: position.id,
        name: position.name,
        level: position.level,
        version: position.version,
    }
}

/// 보관되지 않은 직책끼리 이름이 겹치면 DB 유일 제약이 막는다. 그 위반을 충돌로 바꾼다.
fn position_name_taken(error: DbError) -> ServiceError {
    if org::is_duplicate_position_name(&error) {
        DomainError::Conflict(ConflictReason::AlreadyExists).into()
    } else {
        error.into()
    }
}

pub(super) async fn active_position(conn: &mut Conn, id: PositionId) -> ServiceResult<Position> {
    found(
        org::find_position(conn, id)
            .await?
            .filter(|p| p.archived_at.is_none()),
    )
}

pub async fn list_positions(state: &AppState) -> ServiceResult<Vec<PositionDto>> {
    let positions = org::list_positions(&mut *state.read().await?).await?;
    Ok(positions.into_iter().map(position_dto).collect())
}

pub async fn create_position(
    state: &AppState,
    ctx: &AuthContext,
    input: &PositionInput,
    idempotency: Option<&Idempotency>,
) -> ServiceResult<PositionDto> {
    authz::require_admin(&ctx.actor)?;
    let fields = PositionFields::parse(input)?;
    let now = state.now();
    state
        .write_idempotent(ctx.member_id(), idempotency, async |conn: &mut Conn| {
            authz::require_admin(&current_actor(conn, ctx).await?)?;
            let position = Position {
                id: PositionId::new(),
                name: fields.name.clone(),
                level: fields.level,
                archived_at: None,
                version: 1,
            };
            org::insert_position(conn, &position, now)
                .await
                .map_err(position_name_taken)?;
            audit(
                conn,
                now,
                Audit {
                    action: "position.create",
                    target_type: "position",
                    target_id: Some(position.id.to_string()),
                    detail: Some(serde_json::json!({
                        "name": position.name,
                        "level": position.level,
                    })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            Ok(position_dto(position))
        })
        .await
}

pub async fn update_position(
    state: &AppState,
    ctx: &AuthContext,
    id: PositionId,
    request: &Versioned<PositionInput>,
) -> ServiceResult<PositionDto> {
    authz::require_admin(&ctx.actor)?;
    let fields = PositionFields::parse(&request.data)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            authz::require_admin(&current_actor(conn, ctx).await?)?;
            let current = active_position(conn, id).await?;
            require_updated(current.version == request.version)?;
            let next = Position {
                name: fields.name.clone(),
                level: fields.level,
                ..current.clone()
            };
            let mut changes = Changes::default();
            changes.field("name", &current.name, &next.name).field(
                "level",
                &current.level,
                &next.level,
            );
            if changes.is_empty() {
                return Ok(position_dto(current));
            }
            require_updated(
                org::update_position(conn, &next, now)
                    .await
                    .map_err(position_name_taken)?,
            )?;
            audit(
                conn,
                now,
                Audit {
                    action: "position.update",
                    target_type: "position",
                    target_id: Some(id.to_string()),
                    detail: Some(changes.into_detail()),
                    ctx: Some(ctx),
                },
            )
            .await?;
            Ok(position_dto(active_position(conn, id).await?))
        })
        .await
}

/// 직책 보관. 재직·정지 구성원이 가진 직책은 보관할 수 없다.
pub async fn archive_position(
    state: &AppState,
    ctx: &AuthContext,
    id: PositionId,
    version: i64,
) -> ServiceResult<()> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            authz::require_admin(&current_actor(conn, ctx).await?)?;
            let current = active_position(conn, id).await?;
            require_updated(current.version == version)?;
            if members::count_with_position(conn, id).await? > 0 {
                return Err(DomainError::Conflict(ConflictReason::InUse).into());
            }
            let archived = Position {
                archived_at: Some(now),
                ..current.clone()
            };
            require_updated(org::update_position(conn, &archived, now).await?)?;
            audit(
                conn,
                now,
                Audit {
                    action: "position.archive",
                    target_type: "position",
                    target_id: Some(id.to_string()),
                    detail: Some(serde_json::json!({ "name": current.name })),
                    ctx: Some(ctx),
                },
            )
            .await
        })
        .await
}
