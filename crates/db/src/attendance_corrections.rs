//! 근태 정정 요청. 요청 1건은 결재 문서 1건에 묶인다.

use chrono::NaiveDate;
use pango_core::attendance::{CorrectionStatus, WorkType};
use pango_core::id::{AttendanceCorrectionId, DocumentId, MemberId};
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::{DbError, DbResult};
use crate::pool::Conn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttendanceCorrection {
    pub id: AttendanceCorrectionId,
    pub member_id: MemberId,
    pub work_date: NaiveDate,
    /// 요청한 값.
    pub clock_in_at: Timestamp,
    pub clock_out_at: Option<Timestamp>,
    pub work_type: WorkType,
    pub reason: String,
    /// 요청할 때 본 기록의 `version`. 그날 기록이 없었으면 `None`.
    pub base_record_version: Option<i64>,
    pub status: CorrectionStatus,
    pub document_id: DocumentId,
    pub created_at: Timestamp,
    pub decided_at: Option<Timestamp>,
    pub version: i64,
}

#[derive(sqlx::FromRow)]
struct CorrectionRow {
    id: String,
    member_id: String,
    work_date: String,
    clock_in_at: i64,
    clock_out_at: Option<i64>,
    work_type: String,
    reason: String,
    base_record_version: Option<i64>,
    status: String,
    document_id: String,
    created_at: i64,
    decided_at: Option<i64>,
    version: i64,
}

impl TryFrom<CorrectionRow> for AttendanceCorrection {
    type Error = DbError;

    fn try_from(row: CorrectionRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            member_id: codec::parse("member_id", &row.member_id)?,
            work_date: codec::date("work_date", &row.work_date)?,
            clock_in_at: codec::timestamp("clock_in_at", row.clock_in_at)?,
            clock_out_at: codec::timestamp_opt("clock_out_at", row.clock_out_at)?,
            work_type: codec::parse_with("work_type", &row.work_type, WorkType::parse)?,
            reason: row.reason,
            base_record_version: row.base_record_version,
            status: codec::parse_with("status", &row.status, CorrectionStatus::parse)?,
            document_id: codec::parse("document_id", &row.document_id)?,
            created_at: codec::timestamp("created_at", row.created_at)?,
            decided_at: codec::timestamp_opt("decided_at", row.decided_at)?,
            version: row.version,
        })
    }
}

const COLUMNS: &str = "id, member_id, work_date, clock_in_at, clock_out_at, work_type, reason, \
     base_record_version, status, document_id, created_at, decided_at, version";

/// 새 요청. 같은 구성원·근무일에 결재 중인 요청이 있으면 유일 제약 위반
/// ([`is_duplicate_pending`]).
pub async fn insert(conn: &mut Conn, correction: &AttendanceCorrection) -> DbResult<()> {
    sqlx::query(&format!(
        "INSERT INTO attendance_corrections ({COLUMNS})
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    ))
    .bind(correction.id.to_string())
    .bind(correction.member_id.to_string())
    .bind(codec::date_text(correction.work_date))
    .bind(correction.clock_in_at.as_millis())
    .bind(correction.clock_out_at.map(Timestamp::as_millis))
    .bind(correction.work_type.as_str())
    .bind(&correction.reason)
    .bind(correction.base_record_version)
    .bind(correction.status.as_str())
    .bind(correction.document_id.to_string())
    .bind(correction.created_at.as_millis())
    .bind(correction.decided_at.map(Timestamp::as_millis))
    .bind(correction.version)
    .execute(conn)
    .await?;
    Ok(())
}

/// 결재 중인 요청은 구성원·근무일당 하나라는 유일 인덱스 위반인지.
pub fn is_duplicate_pending(error: &DbError) -> bool {
    error.is_unique_violation("attendance_corrections.member_id")
}

async fn find_where(
    conn: &mut Conn,
    condition: &str,
    binds: &[String],
) -> DbResult<Option<AttendanceCorrection>> {
    let sql = format!("SELECT {COLUMNS} FROM attendance_corrections WHERE {condition}");
    let mut query = sqlx::query_as::<_, CorrectionRow>(&sql);
    for value in binds {
        query = query.bind(value);
    }
    query
        .fetch_optional(conn)
        .await?
        .map(AttendanceCorrection::try_from)
        .transpose()
}

pub async fn find(
    conn: &mut Conn,
    id: AttendanceCorrectionId,
) -> DbResult<Option<AttendanceCorrection>> {
    find_where(conn, "id = ?", &[id.to_string()]).await
}

pub async fn find_by_document(
    conn: &mut Conn,
    document_id: DocumentId,
) -> DbResult<Option<AttendanceCorrection>> {
    find_where(conn, "document_id = ?", &[document_id.to_string()]).await
}

/// 그 구성원·근무일에 결재 중인 요청(최대 하나).
pub async fn find_pending(
    conn: &mut Conn,
    member_id: MemberId,
    work_date: NaiveDate,
) -> DbResult<Option<AttendanceCorrection>> {
    find_where(
        conn,
        "member_id = ? AND work_date = ? AND status = 'pending'",
        &[member_id.to_string(), codec::date_text(work_date)],
    )
    .await
}

/// 상태를 바꾼다. `version`이 현재 값과 같을 때만. 바꿨으면 `true`.
pub async fn set_status(
    conn: &mut Conn,
    id: AttendanceCorrectionId,
    version: i64,
    status: CorrectionStatus,
    decided_at: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE attendance_corrections SET status = ?, decided_at = ?, version = version + 1
         WHERE id = ? AND version = ?",
    )
    .bind(status.as_str())
    .bind(decided_at.as_millis())
    .bind(id.to_string())
    .bind(version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 한 구성원의 요청, 최신 접수순. `(항목, 다음 페이지 존재 여부)`.
pub async fn list_for_member(
    conn: &mut Conn,
    member_id: MemberId,
    before: Option<AttendanceCorrectionId>,
    limit: u32,
) -> DbResult<(Vec<AttendanceCorrection>, bool)> {
    let rows = sqlx::query_as::<_, CorrectionRow>(&format!(
        "SELECT {COLUMNS} FROM attendance_corrections
         WHERE member_id = ?1 AND (?2 IS NULL OR id < ?2)
         ORDER BY id DESC LIMIT ?3"
    ))
    .bind(member_id.to_string())
    .bind(before.map(|id| id.to_string()))
    .bind(codec::limit_plus_one(limit))
    .fetch_all(conn)
    .await?;
    let items = rows
        .into_iter()
        .map(AttendanceCorrection::try_from)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(codec::split_page(items, limit))
}

/// 결재 중인 요청 수(관리 대시보드).
pub async fn count_pending(conn: &mut Conn) -> DbResult<i64> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM attendance_corrections WHERE status = 'pending'")
            .fetch_one(conn)
            .await?;
    Ok(count)
}
