//! 출퇴근 기록과 변경 이력(추가 전용).

use chrono::NaiveDate;
use pango_core::attendance::{AttendanceEventKind, RecordState, WorkType};
use pango_core::calendar::DateRange;
use pango_core::id::{AttendanceEventId, AttendanceRecordId, DepartmentId, MemberId};
use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::codec;
use crate::error::{DbError, DbResult};
use crate::pool::Conn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttendanceRecord {
    pub id: AttendanceRecordId,
    pub member_id: MemberId,
    pub work_date: NaiveDate,
    pub clock_in_at: Timestamp,
    pub clock_out_at: Option<Timestamp>,
    pub work_type: WorkType,
    pub note: Option<String>,
    pub version: i64,
}

impl AttendanceRecord {
    /// 출퇴근 판정(`pango_core::attendance`)에 넘기는 상태.
    pub fn state(&self) -> RecordState {
        RecordState {
            work_date: self.work_date,
            clock_in_at: self.clock_in_at,
            clock_out_at: self.clock_out_at,
        }
    }
}

#[derive(sqlx::FromRow)]
struct RecordRow {
    id: String,
    member_id: String,
    work_date: String,
    clock_in_at: i64,
    clock_out_at: Option<i64>,
    work_type: String,
    note: Option<String>,
    version: i64,
}

impl TryFrom<RecordRow> for AttendanceRecord {
    type Error = DbError;

    fn try_from(row: RecordRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            member_id: codec::parse("member_id", &row.member_id)?,
            work_date: codec::date("work_date", &row.work_date)?,
            clock_in_at: codec::timestamp("clock_in_at", row.clock_in_at)?,
            clock_out_at: codec::timestamp_opt("clock_out_at", row.clock_out_at)?,
            work_type: codec::parse_with("work_type", &row.work_type, WorkType::parse)?,
            note: row.note,
            version: row.version,
        })
    }
}

const RECORD_COLUMNS: &str =
    "r.id, r.member_id, r.work_date, r.clock_in_at, r.clock_out_at, r.work_type, r.note, r.version";

fn rows_to_records(rows: Vec<RecordRow>) -> DbResult<Vec<AttendanceRecord>> {
    rows.into_iter().map(AttendanceRecord::try_from).collect()
}

/// 새 기록. 같은 구성원·근무일 기록이 있으면 유일 제약 위반([`is_duplicate_work_date`]).
pub async fn insert(conn: &mut Conn, record: &AttendanceRecord, now: Timestamp) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO attendance_records
            (id, member_id, work_date, clock_in_at, clock_out_at, work_type, note,
             created_at, updated_at, version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(record.id.to_string())
    .bind(record.member_id.to_string())
    .bind(codec::date_text(record.work_date))
    .bind(record.clock_in_at.as_millis())
    .bind(record.clock_out_at.map(Timestamp::as_millis))
    .bind(record.work_type.as_str())
    .bind(&record.note)
    .bind(now.as_millis())
    .bind(now.as_millis())
    .bind(record.version)
    .execute(conn)
    .await?;
    Ok(())
}

/// `UNIQUE (member_id, work_date)` 위반인지. 동시 출근 요청의 패자가 이 오류를 받는다.
pub fn is_duplicate_work_date(error: &DbError) -> bool {
    error.is_unique_violation("attendance_records.member_id")
}

pub async fn find(conn: &mut Conn, id: AttendanceRecordId) -> DbResult<Option<AttendanceRecord>> {
    sqlx::query_as::<_, RecordRow>(&format!(
        "SELECT {RECORD_COLUMNS} FROM attendance_records r WHERE r.id = ?"
    ))
    .bind(id.to_string())
    .fetch_optional(conn)
    .await?
    .map(AttendanceRecord::try_from)
    .transpose()
}

pub async fn find_by_member_date(
    conn: &mut Conn,
    member_id: MemberId,
    work_date: NaiveDate,
) -> DbResult<Option<AttendanceRecord>> {
    sqlx::query_as::<_, RecordRow>(&format!(
        "SELECT {RECORD_COLUMNS} FROM attendance_records r
         WHERE r.member_id = ? AND r.work_date = ?"
    ))
    .bind(member_id.to_string())
    .bind(codec::date_text(work_date))
    .fetch_optional(conn)
    .await?
    .map(AttendanceRecord::try_from)
    .transpose()
}

/// 출근 시각이 가장 늦은 기록. 출근·퇴근 판정의 기준이다.
pub async fn latest_for_member(
    conn: &mut Conn,
    member_id: MemberId,
) -> DbResult<Option<AttendanceRecord>> {
    sqlx::query_as::<_, RecordRow>(&format!(
        "SELECT {RECORD_COLUMNS} FROM attendance_records r
         WHERE r.member_id = ? ORDER BY r.clock_in_at DESC, r.id DESC LIMIT 1"
    ))
    .bind(member_id.to_string())
    .fetch_optional(conn)
    .await?
    .map(AttendanceRecord::try_from)
    .transpose()
}

/// 구성원들의 기간 내 기록(근무일순). 구성원 목록이 비면 빈 결과.
pub async fn list_for_members(
    conn: &mut Conn,
    member_ids: &[MemberId],
    range: DateRange,
) -> DbResult<Vec<AttendanceRecord>> {
    if member_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; member_ids.len()].join(", ");
    let sql = format!(
        "SELECT {RECORD_COLUMNS} FROM attendance_records r
         WHERE r.work_date BETWEEN ? AND ? AND r.member_id IN ({placeholders})
         ORDER BY r.work_date, r.member_id"
    );
    let mut query = sqlx::query_as::<_, RecordRow>(&sql)
        .bind(codec::date_text(range.start()))
        .bind(codec::date_text(range.end()));
    for id in member_ids {
        query = query.bind(id.to_string());
    }
    rows_to_records(query.fetch_all(conn).await?)
}

/// 열린 기록에 퇴근 시각을 넣는다. 이미 닫혀 있으면 바꾸지 않고 `false`.
pub async fn close(
    conn: &mut Conn,
    id: AttendanceRecordId,
    clock_out_at: Timestamp,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE attendance_records SET clock_out_at = ?, updated_at = ?, version = version + 1
         WHERE id = ? AND clock_out_at IS NULL",
    )
    .bind(clock_out_at.as_millis())
    .bind(now.as_millis())
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 시각·근무 형태·메모를 고친다. `record.version`이 현재 값과 같을 때만. 고쳤으면 `true`.
pub async fn update(conn: &mut Conn, record: &AttendanceRecord, now: Timestamp) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE attendance_records SET
            clock_in_at = ?, clock_out_at = ?, work_type = ?, note = ?, updated_at = ?,
            version = version + 1
         WHERE id = ? AND version = ?",
    )
    .bind(record.clock_in_at.as_millis())
    .bind(record.clock_out_at.map(Timestamp::as_millis))
    .bind(record.work_type.as_str())
    .bind(&record.note)
    .bind(now.as_millis())
    .bind(record.id.to_string())
    .bind(record.version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 기록 변경 이력 한 줄. 테이블은 트리거로 수정·삭제가 막혀 있다.
#[derive(Debug, Clone, PartialEq)]
pub struct AttendanceEvent {
    pub id: AttendanceEventId,
    pub record_id: AttendanceRecordId,
    pub member_id: MemberId,
    pub kind: AttendanceEventKind,
    pub before: Option<serde_json::Value>,
    pub after: serde_json::Value,
    pub reason: Option<String>,
    pub actor_id: MemberId,
    pub created_at: Timestamp,
}

#[derive(sqlx::FromRow)]
struct EventRow {
    id: String,
    record_id: String,
    member_id: String,
    kind: String,
    before: Option<String>,
    after: String,
    reason: Option<String>,
    actor_id: String,
    created_at: i64,
}

impl TryFrom<EventRow> for AttendanceEvent {
    type Error = DbError;

    fn try_from(row: EventRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            record_id: codec::parse("record_id", &row.record_id)?,
            member_id: codec::parse("member_id", &row.member_id)?,
            kind: codec::parse_with("kind", &row.kind, AttendanceEventKind::parse)?,
            before: row
                .before
                .as_deref()
                .map(|raw| codec::json("before", raw))
                .transpose()?,
            after: codec::json("after", &row.after)?,
            reason: row.reason,
            actor_id: codec::parse("actor_id", &row.actor_id)?,
            created_at: codec::timestamp("created_at", row.created_at)?,
        })
    }
}

pub async fn insert_event(conn: &mut Conn, event: &AttendanceEvent) -> DbResult<()> {
    let before = event
        .before
        .as_ref()
        .map(|value| codec::json_text("before", value))
        .transpose()?;
    sqlx::query(
        "INSERT INTO attendance_events
            (id, record_id, member_id, kind, before, after, reason, actor_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(event.id.to_string())
    .bind(event.record_id.to_string())
    .bind(event.member_id.to_string())
    .bind(event.kind.as_str())
    .bind(before)
    .bind(codec::json_text("after", &event.after)?)
    .bind(&event.reason)
    .bind(event.actor_id.to_string())
    .bind(event.created_at.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

/// 기록 하나의 이력(오래된 순).
pub async fn list_events(
    conn: &mut Conn,
    record_id: AttendanceRecordId,
) -> DbResult<Vec<AttendanceEvent>> {
    sqlx::query_as::<_, EventRow>(
        "SELECT id, record_id, member_id, kind, before, after, reason, actor_id, created_at
         FROM attendance_events WHERE record_id = ? ORDER BY created_at, id",
    )
    .bind(record_id.to_string())
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(AttendanceEvent::try_from)
    .collect()
}

/// 관리 화면·내보내기용: 기록과 구성원 이름·아이디·부서 이름.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordWithMember {
    pub record: AttendanceRecord,
    pub member_name: String,
    pub login_id: String,
    pub department_name: Option<String>,
}

#[derive(sqlx::FromRow)]
struct RecordWithMemberRow {
    #[sqlx(flatten)]
    record: RecordRow,
    member_name: String,
    login_id: String,
    department_name: Option<String>,
}

impl TryFrom<RecordWithMemberRow> for RecordWithMember {
    type Error = DbError;

    fn try_from(row: RecordWithMemberRow) -> DbResult<Self> {
        Ok(Self {
            record: row.record.try_into()?,
            member_name: row.member_name,
            login_id: row.login_id,
            department_name: row.department_name,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordFilter {
    pub range: DateRange,
    pub member_id: Option<MemberId>,
    /// 지정하면 현재 이 부서들에 속한 구성원의 기록만.
    pub departments: Option<Vec<DepartmentId>>,
}

/// 관리 목록의 다음 페이지 위치. 정렬 키(근무일, ID) 그대로다. 다음 페이지 커서에 담는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordCursor {
    pub work_date: NaiveDate,
    pub id: AttendanceRecordId,
}

/// 필터의 WHERE 절과 바인딩 값. 목록과 내보내기가 같은 조건을 쓴다.
fn filter_clause(filter: &RecordFilter) -> (String, Vec<String>) {
    let mut sql = String::from("r.work_date BETWEEN ? AND ?");
    let mut binds = vec![
        codec::date_text(filter.range.start()),
        codec::date_text(filter.range.end()),
    ];
    if let Some(member_id) = filter.member_id {
        sql.push_str(" AND r.member_id = ?");
        binds.push(member_id.to_string());
    }
    if let Some(departments) = &filter.departments {
        if departments.is_empty() {
            sql.push_str(" AND 0");
        } else {
            sql.push_str(&format!(
                " AND m.department_id IN ({})",
                vec!["?"; departments.len()].join(", ")
            ));
            binds.extend(departments.iter().map(ToString::to_string));
        }
    }
    (sql, binds)
}

const DETAILED_FROM: &str = "FROM attendance_records r
     JOIN members m ON m.id = r.member_id
     LEFT JOIN departments d ON d.id = m.department_id";

async fn fetch_detailed(
    conn: &mut Conn,
    sql: &str,
    binds: &[String],
    limit: Option<i64>,
) -> DbResult<Vec<RecordWithMember>> {
    let mut query = sqlx::query_as::<_, RecordWithMemberRow>(sql);
    for value in binds {
        query = query.bind(value);
    }
    if let Some(limit) = limit {
        query = query.bind(limit);
    }
    query
        .fetch_all(conn)
        .await?
        .into_iter()
        .map(RecordWithMember::try_from)
        .collect()
}

/// 최신 근무일순 목록. `(항목, 다음 페이지 존재 여부)`.
pub async fn list_page(
    conn: &mut Conn,
    filter: &RecordFilter,
    after: Option<RecordCursor>,
    limit: u32,
) -> DbResult<(Vec<RecordWithMember>, bool)> {
    let (mut condition, mut binds) = filter_clause(filter);
    if let Some(cursor) = after {
        condition.push_str(" AND (r.work_date < ? OR (r.work_date = ? AND r.id < ?))");
        let day = codec::date_text(cursor.work_date);
        binds.extend([day.clone(), day, cursor.id.to_string()]);
    }
    let sql = format!(
        "SELECT {RECORD_COLUMNS}, m.name AS member_name, m.login_id, d.name AS department_name
         {DETAILED_FROM} WHERE {condition}
         ORDER BY r.work_date DESC, r.id DESC LIMIT ?"
    );
    let rows = fetch_detailed(conn, &sql, &binds, Some(codec::limit_plus_one(limit))).await?;
    Ok(codec::split_page(rows, limit))
}

/// 내보내기용 전체 목록(근무일, 이름순). 기간은 [`DateRange`]가 최대 366일로 제한한다.
pub async fn list_for_export(
    conn: &mut Conn,
    filter: &RecordFilter,
) -> DbResult<Vec<RecordWithMember>> {
    let (condition, binds) = filter_clause(filter);
    let sql = format!(
        "SELECT {RECORD_COLUMNS}, m.name AS member_name, m.login_id, d.name AS department_name
         {DETAILED_FROM} WHERE {condition}
         ORDER BY r.work_date, m.name, r.id"
    );
    fetch_detailed(conn, &sql, &binds, None).await
}

/// 기간 안의 퇴근하지 않은 기록(최근 근무일 먼저, 같은 날은 이름순). 퇴근 누락 점검용.
///
/// 열린 기록만 보는 부분 인덱스가 있고 기간이 짧아 결과가 작다. 어떤 것이 "누락"인지는
/// 규칙(`pango_core::attendance::evaluate_day`)이 정하므로 여기서는 가리지 않는다.
pub async fn list_open(conn: &mut Conn, range: DateRange) -> DbResult<Vec<RecordWithMember>> {
    let sql = format!(
        "SELECT {RECORD_COLUMNS}, m.name AS member_name, m.login_id, d.name AS department_name
         {DETAILED_FROM} WHERE r.work_date BETWEEN ? AND ? AND r.clock_out_at IS NULL
         ORDER BY r.work_date DESC, m.name, r.id"
    );
    let binds = [
        codec::date_text(range.start()),
        codec::date_text(range.end()),
    ];
    fetch_detailed(conn, &sql, &binds, None).await
}
