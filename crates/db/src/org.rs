//! 부서와 직책.

use pango_core::id::{DepartmentId, MemberId, PositionId};
use pango_core::org::DepartmentNode;
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::{DbError, DbResult};
use crate::pool::Conn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Department {
    pub id: DepartmentId,
    pub parent_id: Option<DepartmentId>,
    pub name: String,
    pub head_member_id: Option<MemberId>,
    pub sort_order: i64,
    pub archived_at: Option<Timestamp>,
    pub version: i64,
}

impl Department {
    pub fn node(&self) -> DepartmentNode {
        DepartmentNode {
            id: self.id,
            parent_id: self.parent_id,
        }
    }
}

#[derive(sqlx::FromRow)]
struct DepartmentRow {
    id: String,
    parent_id: Option<String>,
    name: String,
    head_member_id: Option<String>,
    sort_order: i64,
    archived_at: Option<i64>,
    version: i64,
}

impl TryFrom<DepartmentRow> for Department {
    type Error = crate::DbError;

    fn try_from(row: DepartmentRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            parent_id: codec::parse_opt("parent_id", row.parent_id.as_deref())?,
            name: row.name,
            head_member_id: codec::parse_opt("head_member_id", row.head_member_id.as_deref())?,
            sort_order: row.sort_order,
            archived_at: codec::timestamp_opt("archived_at", row.archived_at)?,
            version: row.version,
        })
    }
}

const DEPARTMENT_COLUMNS: &str =
    "id, parent_id, name, head_member_id, sort_order, archived_at, version";

/// 보관되지 않은 부서 전체. 트리 계산은 호출하는 쪽에서 한다.
pub async fn list_departments(conn: &mut Conn) -> DbResult<Vec<Department>> {
    sqlx::query_as::<_, DepartmentRow>(&format!(
        "SELECT {DEPARTMENT_COLUMNS} FROM departments
         WHERE archived_at IS NULL ORDER BY sort_order, name, id"
    ))
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Department::try_from)
    .collect()
}

pub async fn find_department(conn: &mut Conn, id: DepartmentId) -> DbResult<Option<Department>> {
    sqlx::query_as::<_, DepartmentRow>(&format!(
        "SELECT {DEPARTMENT_COLUMNS} FROM departments WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_optional(conn)
    .await?
    .map(Department::try_from)
    .transpose()
}

pub async fn insert_department(
    conn: &mut Conn,
    department: &Department,
    now: Timestamp,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO departments
            (id, parent_id, name, head_member_id, sort_order, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(department.id.to_string())
    .bind(department.parent_id.map(|id| id.to_string()))
    .bind(&department.name)
    .bind(department.head_member_id.map(|id| id.to_string()))
    .bind(department.sort_order)
    .bind(now.as_millis())
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

/// `department.version`이 현재 값과 같을 때만 갱신한다. 갱신했으면 `true`.
pub async fn update_department(
    conn: &mut Conn,
    department: &Department,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE departments SET
            parent_id = ?, name = ?, head_member_id = ?, sort_order = ?, archived_at = ?,
            updated_at = ?, version = version + 1
         WHERE id = ? AND version = ?",
    )
    .bind(department.parent_id.map(|id| id.to_string()))
    .bind(&department.name)
    .bind(department.head_member_id.map(|id| id.to_string()))
    .bind(department.sort_order)
    .bind(department.archived_at.map(Timestamp::as_millis))
    .bind(now.as_millis())
    .bind(department.id.to_string())
    .bind(department.version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 구성원이 조직장인 부서들.
pub async fn departments_headed_by(
    conn: &mut Conn,
    member_id: MemberId,
) -> DbResult<Vec<DepartmentId>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT id FROM departments WHERE head_member_id = ? AND archived_at IS NULL",
    )
    .bind(member_id.to_string())
    .fetch_all(conn)
    .await?;
    rows.iter().map(|(id,)| codec::parse("id", id)).collect()
}

/// 구성원을 모든 부서의 조직장에서 해제한다(퇴사·비활성화 시).
pub async fn clear_head(conn: &mut Conn, member_id: MemberId, now: Timestamp) -> DbResult<u64> {
    let result = sqlx::query(
        "UPDATE departments SET head_member_id = NULL, updated_at = ?, version = version + 1
         WHERE head_member_id = ?",
    )
    .bind(now.as_millis())
    .bind(member_id.to_string())
    .execute(conn)
    .await?;
    Ok(result.rows_affected())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub id: PositionId,
    pub name: String,
    pub level: i64,
    pub archived_at: Option<Timestamp>,
    pub version: i64,
}

#[derive(sqlx::FromRow)]
struct PositionRow {
    id: String,
    name: String,
    level: i64,
    archived_at: Option<i64>,
    version: i64,
}

impl TryFrom<PositionRow> for Position {
    type Error = crate::DbError;

    fn try_from(row: PositionRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            name: row.name,
            level: row.level,
            archived_at: codec::timestamp_opt("archived_at", row.archived_at)?,
            version: row.version,
        })
    }
}

pub async fn list_positions(conn: &mut Conn) -> DbResult<Vec<Position>> {
    sqlx::query_as::<_, PositionRow>(
        "SELECT id, name, level, archived_at, version FROM positions
         WHERE archived_at IS NULL ORDER BY level DESC, name, id",
    )
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Position::try_from)
    .collect()
}

pub async fn find_position(conn: &mut Conn, id: PositionId) -> DbResult<Option<Position>> {
    sqlx::query_as::<_, PositionRow>(
        "SELECT id, name, level, archived_at, version FROM positions WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(conn)
    .await?
    .map(Position::try_from)
    .transpose()
}

/// 보관되지 않은 직책끼리의 이름 유일 제약(`positions.name`) 위반인지.
pub fn is_duplicate_position_name(error: &DbError) -> bool {
    error.is_unique_violation("positions.name")
}

pub async fn insert_position(conn: &mut Conn, position: &Position, now: Timestamp) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO positions (id, name, level, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(position.id.to_string())
    .bind(&position.name)
    .bind(position.level)
    .bind(now.as_millis())
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn update_position(
    conn: &mut Conn,
    position: &Position,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE positions SET name = ?, level = ?, archived_at = ?, updated_at = ?,
            version = version + 1
         WHERE id = ? AND version = ?",
    )
    .bind(&position.name)
    .bind(position.level)
    .bind(position.archived_at.map(Timestamp::as_millis))
    .bind(now.as_millis())
    .bind(position.id.to_string())
    .bind(position.version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}
