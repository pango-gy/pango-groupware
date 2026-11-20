//! 감사 로그(추가 전용).

use pango_core::id::{AuditLogId, MemberId};
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    pub id: AuditLogId,
    pub at: Timestamp,
    /// 시스템 작업이면 `None`.
    pub actor_id: Option<MemberId>,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub detail: Option<serde_json::Value>,
    pub ip: Option<String>,
}

#[derive(sqlx::FromRow)]
struct AuditRow {
    id: String,
    at: i64,
    actor_id: Option<String>,
    action: String,
    target_type: String,
    target_id: Option<String>,
    detail: Option<String>,
    ip: Option<String>,
}

impl TryFrom<AuditRow> for AuditEntry {
    type Error = crate::DbError;

    fn try_from(row: AuditRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            at: codec::timestamp("at", row.at)?,
            actor_id: codec::parse_opt("actor_id", row.actor_id.as_deref())?,
            action: row.action,
            target_type: row.target_type,
            target_id: row.target_id,
            detail: row
                .detail
                .as_deref()
                .map(|raw| codec::json("detail", raw))
                .transpose()?,
            ip: row.ip,
        })
    }
}

pub async fn insert(conn: &mut Conn, entry: &AuditEntry) -> DbResult<()> {
    let detail = entry
        .detail
        .as_ref()
        .map(|value| codec::json_text("detail", value))
        .transpose()?;
    sqlx::query(
        "INSERT INTO audit_log (id, at, actor_id, action, target_type, target_id, detail, ip)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(entry.id.to_string())
    .bind(entry.at.as_millis())
    .bind(entry.actor_id.map(|id| id.to_string()))
    .bind(&entry.action)
    .bind(&entry.target_type)
    .bind(&entry.target_id)
    .bind(detail)
    .bind(&entry.ip)
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub actor_id: Option<MemberId>,
    pub action: Option<String>,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub from: Option<Timestamp>,
    pub until: Option<Timestamp>,
    /// 이 ID보다 오래된 항목부터(다음 페이지).
    pub before: Option<AuditLogId>,
}

/// 최신순 목록. `(항목, 다음 페이지 존재 여부)`.
pub async fn list(
    conn: &mut Conn,
    filter: &AuditFilter,
    limit: u32,
) -> DbResult<(Vec<AuditEntry>, bool)> {
    let rows = sqlx::query_as::<_, AuditRow>(
        "SELECT id, at, actor_id, action, target_type, target_id, detail, ip FROM audit_log
         WHERE (?1 IS NULL OR actor_id = ?1)
           AND (?2 IS NULL OR action = ?2)
           AND (?3 IS NULL OR target_type = ?3)
           AND (?4 IS NULL OR target_id = ?4)
           AND (?5 IS NULL OR at >= ?5)
           AND (?6 IS NULL OR at < ?6)
           AND (?7 IS NULL OR id < ?7)
         ORDER BY id DESC LIMIT ?8",
    )
    .bind(filter.actor_id.map(|id| id.to_string()))
    .bind(&filter.action)
    .bind(&filter.target_type)
    .bind(&filter.target_id)
    .bind(filter.from.map(Timestamp::as_millis))
    .bind(filter.until.map(Timestamp::as_millis))
    .bind(filter.before.map(|id| id.to_string()))
    .bind(codec::limit_plus_one(limit))
    .fetch_all(conn)
    .await?;
    let entries = rows
        .into_iter()
        .map(AuditEntry::try_from)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(codec::split_page(entries, limit))
}
