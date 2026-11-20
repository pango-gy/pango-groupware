//! 구성원 계정.

use chrono::NaiveDate;
use pango_core::authz::{Role, Subject};
use pango_core::id::{DepartmentId, MemberId, PositionId};
use pango_core::member::MemberStatus;
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: MemberId,
    pub login_id: String,
    pub name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub department_id: Option<DepartmentId>,
    pub position_id: Option<PositionId>,
    pub role: Role,
    pub status: MemberStatus,
    pub hired_on: NaiveDate,
    pub retired_on: Option<NaiveDate>,
    pub must_change_password: bool,
    pub version: i64,
}

impl Member {
    pub fn subject(&self) -> Subject {
        Subject {
            member_id: self.id,
            role: self.role,
            department_id: self.department_id,
        }
    }
}

/// 로그인 판정에 필요한 비밀 정보. 일반 조회 결과([`Member`])에는 들어가지 않는다.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub member: Member,
    pub password_hash: String,
}

#[derive(sqlx::FromRow)]
struct MemberRow {
    id: String,
    login_id: String,
    name: String,
    email: Option<String>,
    phone: Option<String>,
    department_id: Option<String>,
    position_id: Option<String>,
    role: String,
    status: String,
    hired_on: String,
    retired_on: Option<String>,
    must_change_password: i64,
    version: i64,
}

impl TryFrom<MemberRow> for Member {
    type Error = crate::DbError;

    fn try_from(row: MemberRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            login_id: row.login_id,
            name: row.name,
            email: row.email,
            phone: row.phone,
            department_id: codec::parse_opt("department_id", row.department_id.as_deref())?,
            position_id: codec::parse_opt("position_id", row.position_id.as_deref())?,
            role: codec::parse_with("role", &row.role, Role::parse)?,
            status: codec::parse_with("status", &row.status, MemberStatus::parse)?,
            hired_on: codec::date("hired_on", &row.hired_on)?,
            retired_on: codec::date_opt("retired_on", row.retired_on.as_deref())?,
            must_change_password: row.must_change_password != 0,
            version: row.version,
        })
    }
}

const MEMBER_COLUMNS: &str = "id, login_id, name, email, phone, department_id, position_id, \
     role, status, hired_on, retired_on, must_change_password, version";

pub async fn find(conn: &mut Conn, id: MemberId) -> DbResult<Option<Member>> {
    sqlx::query_as::<_, MemberRow>(&format!(
        "SELECT {MEMBER_COLUMNS} FROM members WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_optional(conn)
    .await?
    .map(Member::try_from)
    .transpose()
}

pub async fn find_many(conn: &mut Conn, ids: &[MemberId]) -> DbResult<Vec<Member>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let sql = format!("SELECT {MEMBER_COLUMNS} FROM members WHERE id IN ({placeholders})");
    let mut query = sqlx::query_as::<_, MemberRow>(&sql);
    for id in ids {
        query = query.bind(id.to_string());
    }
    query
        .fetch_all(conn)
        .await?
        .into_iter()
        .map(Member::try_from)
        .collect()
}

#[derive(sqlx::FromRow)]
struct CredentialsRow {
    #[sqlx(flatten)]
    member: MemberRow,
    password_hash: String,
}

pub async fn find_credentials(conn: &mut Conn, login_id: &str) -> DbResult<Option<Credentials>> {
    credentials_where(conn, "login_id = ?", login_id).await
}

pub async fn find_credentials_by_id(
    conn: &mut Conn,
    id: MemberId,
) -> DbResult<Option<Credentials>> {
    credentials_where(conn, "id = ?", &id.to_string()).await
}

async fn credentials_where(
    conn: &mut Conn,
    condition: &'static str,
    value: &str,
) -> DbResult<Option<Credentials>> {
    let row = sqlx::query_as::<_, CredentialsRow>(&format!(
        "SELECT {MEMBER_COLUMNS}, password_hash FROM members WHERE {condition}"
    ))
    .bind(value)
    .fetch_optional(conn)
    .await?;
    row.map(|row| {
        Ok(Credentials {
            member: row.member.try_into()?,
            password_hash: row.password_hash,
        })
    })
    .transpose()
}

#[derive(Debug, Clone, Default)]
pub struct MemberFilter {
    /// 비어 있으면 모든 상태.
    pub statuses: Vec<MemberStatus>,
    /// 지정하면 이 부서들에 속한 구성원만.
    pub departments: Option<Vec<DepartmentId>>,
    /// 이름·아이디 부분 일치.
    pub search: Option<String>,
}

/// `LIKE` 패턴의 특수문자를 이스케이프한다. 검색어의 `%`, `_`가 와일드카드로 쓰이지 않게 한다.
fn like_contains(term: &str) -> String {
    let mut pattern = String::with_capacity(term.len() + 2);
    pattern.push('%');
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    pattern
}

/// 이름순 목록. 구성원 수는 사내 규모(수백 명)이므로 페이지를 나누지 않는다.
pub async fn list(conn: &mut Conn, filter: &MemberFilter) -> DbResult<Vec<Member>> {
    let mut sql = format!("SELECT {MEMBER_COLUMNS} FROM members WHERE 1 = 1");
    let mut binds: Vec<String> = Vec::new();
    if !filter.statuses.is_empty() {
        sql.push_str(&format!(
            " AND status IN ({})",
            vec!["?"; filter.statuses.len()].join(", ")
        ));
        binds.extend(filter.statuses.iter().map(|s| s.as_str().to_owned()));
    }
    if let Some(departments) = &filter.departments {
        if departments.is_empty() {
            return Ok(Vec::new());
        }
        sql.push_str(&format!(
            " AND department_id IN ({})",
            vec!["?"; departments.len()].join(", ")
        ));
        binds.extend(departments.iter().map(ToString::to_string));
    }
    if let Some(term) = filter.search.as_deref().filter(|t| !t.is_empty()) {
        sql.push_str(" AND (name LIKE ? ESCAPE '\\' OR login_id LIKE ? ESCAPE '\\')");
        let pattern = like_contains(term);
        binds.extend([pattern.clone(), pattern]);
    }
    sql.push_str(" ORDER BY name, id");
    let mut query = sqlx::query_as::<_, MemberRow>(&sql);
    for value in &binds {
        query = query.bind(value);
    }
    query
        .fetch_all(conn)
        .await?
        .into_iter()
        .map(Member::try_from)
        .collect()
}

pub async fn insert(
    conn: &mut Conn,
    member: &Member,
    password_hash: &str,
    now: Timestamp,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO members
            (id, login_id, name, email, phone, department_id, position_id, role, status,
             hired_on, retired_on, password_hash, must_change_password, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(member.id.to_string())
    .bind(&member.login_id)
    .bind(&member.name)
    .bind(&member.email)
    .bind(&member.phone)
    .bind(member.department_id.map(|id| id.to_string()))
    .bind(member.position_id.map(|id| id.to_string()))
    .bind(member.role.as_str())
    .bind(member.status.as_str())
    .bind(codec::date_text(member.hired_on))
    .bind(member.retired_on.map(codec::date_text))
    .bind(password_hash)
    .bind(member.must_change_password)
    .bind(now.as_millis())
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

/// 프로필·소속·역할·상태를 갱신한다. `member.version`이 현재 값과 같을 때만. 갱신했으면 `true`.
pub async fn update(conn: &mut Conn, member: &Member, now: Timestamp) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE members SET
            name = ?, email = ?, phone = ?, department_id = ?, position_id = ?, role = ?,
            status = ?, hired_on = ?, retired_on = ?, updated_at = ?, version = version + 1
         WHERE id = ? AND version = ?",
    )
    .bind(&member.name)
    .bind(&member.email)
    .bind(&member.phone)
    .bind(member.department_id.map(|id| id.to_string()))
    .bind(member.position_id.map(|id| id.to_string()))
    .bind(member.role.as_str())
    .bind(member.status.as_str())
    .bind(codec::date_text(member.hired_on))
    .bind(member.retired_on.map(codec::date_text))
    .bind(now.as_millis())
    .bind(member.id.to_string())
    .bind(member.version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 비밀번호를 바꾼다.
pub async fn set_password(
    conn: &mut Conn,
    id: MemberId,
    password_hash: &str,
    must_change: bool,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE members SET
            password_hash = ?, must_change_password = ?, password_changed_at = ?,
            updated_at = ?, version = version + 1
         WHERE id = ?",
    )
    .bind(password_hash)
    .bind(must_change)
    .bind(now.as_millis())
    .bind(now.as_millis())
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 로그인할 수 있는 최고 관리자 수. 마지막 한 명을 없애지 않기 위해 쓴다.
pub async fn count_active_owners(conn: &mut Conn) -> DbResult<i64> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM members WHERE role = 'owner' AND status = 'active'")
            .fetch_one(conn)
            .await?;
    Ok(count)
}

pub async fn count_in_department(conn: &mut Conn, department_id: DepartmentId) -> DbResult<i64> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM members WHERE department_id = ? AND status <> 'retired'",
    )
    .bind(department_id.to_string())
    .fetch_one(conn)
    .await?;
    Ok(count)
}

pub async fn count_with_position(conn: &mut Conn, position_id: PositionId) -> DbResult<i64> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM members WHERE position_id = ? AND status <> 'retired'",
    )
    .bind(position_id.to_string())
    .fetch_one(conn)
    .await?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_contains("홍"), "%홍%");
        assert_eq!(like_contains("100%"), "%100\\%%");
        assert_eq!(like_contains("a_b\\c"), "%a\\_b\\\\c%");
    }
}
