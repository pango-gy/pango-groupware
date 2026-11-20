//! 휴가: 종류, 신청, 원장(추가 전용).

use chrono::NaiveDate;
use pango_core::calendar::DateRange;
use pango_core::id::{DocumentId, LeaveLedgerEntryId, LeaveRequestId, LeaveTypeId, MemberId};
use pango_core::leave::{
    LeaveAmount, LeaveRequestStatus, LeaveUnit, LedgerEntry, LedgerEntryKind, LedgerKind,
};
use pango_core::time::Timestamp;
use pango_core::{DomainError, ValidationReason};

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

// ───────────── 휴가 종류 ─────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveType {
    pub id: LeaveTypeId,
    pub code: String,
    pub name: String,
    pub deducts_balance: bool,
    pub allow_partial: bool,
    pub active: bool,
    pub sort_order: i64,
    pub version: i64,
}

#[derive(sqlx::FromRow)]
struct LeaveTypeRow {
    id: String,
    code: String,
    name: String,
    deducts_balance: i64,
    allow_partial: i64,
    active: i64,
    sort_order: i64,
    version: i64,
}

impl TryFrom<LeaveTypeRow> for LeaveType {
    type Error = crate::DbError;

    fn try_from(row: LeaveTypeRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            code: row.code,
            name: row.name,
            deducts_balance: row.deducts_balance != 0,
            allow_partial: row.allow_partial != 0,
            active: row.active != 0,
            sort_order: row.sort_order,
            version: row.version,
        })
    }
}

const TYPE_COLUMNS: &str =
    "id, code, name, deducts_balance, allow_partial, active, sort_order, version";

/// 정렬 순서대로. `active_only`면 사용 중인 종류만.
pub async fn list_types(conn: &mut Conn, active_only: bool) -> DbResult<Vec<LeaveType>> {
    sqlx::query_as::<_, LeaveTypeRow>(&format!(
        "SELECT {TYPE_COLUMNS} FROM leave_types
         WHERE (?1 = 0 OR active = 1) ORDER BY sort_order, name, id"
    ))
    .bind(active_only)
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(LeaveType::try_from)
    .collect()
}

pub async fn find_type(conn: &mut Conn, id: LeaveTypeId) -> DbResult<Option<LeaveType>> {
    sqlx::query_as::<_, LeaveTypeRow>(&format!(
        "SELECT {TYPE_COLUMNS} FROM leave_types WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_optional(conn)
    .await?
    .map(LeaveType::try_from)
    .transpose()
}

pub async fn find_type_by_code(conn: &mut Conn, code: &str) -> DbResult<Option<LeaveType>> {
    sqlx::query_as::<_, LeaveTypeRow>(&format!(
        "SELECT {TYPE_COLUMNS} FROM leave_types WHERE code = ?"
    ))
    .bind(code)
    .fetch_optional(conn)
    .await?
    .map(LeaveType::try_from)
    .transpose()
}

pub async fn insert_type(conn: &mut Conn, leave_type: &LeaveType, now: Timestamp) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO leave_types
            (id, code, name, deducts_balance, allow_partial, active, sort_order,
             created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(leave_type.id.to_string())
    .bind(&leave_type.code)
    .bind(&leave_type.name)
    .bind(leave_type.deducts_balance)
    .bind(leave_type.allow_partial)
    .bind(leave_type.active)
    .bind(leave_type.sort_order)
    .bind(now.as_millis())
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

/// `code`와 `deducts_balance`는 바꾸지 않는다. `version`이 현재 값과 같을 때만 갱신한다.
pub async fn update_type(
    conn: &mut Conn,
    leave_type: &LeaveType,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE leave_types SET
            name = ?, allow_partial = ?, active = ?, sort_order = ?,
            updated_at = ?, version = version + 1
         WHERE id = ? AND version = ?",
    )
    .bind(&leave_type.name)
    .bind(leave_type.allow_partial)
    .bind(leave_type.active)
    .bind(leave_type.sort_order)
    .bind(now.as_millis())
    .bind(leave_type.id.to_string())
    .bind(leave_type.version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

// ───────────── 신청 ─────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaveRequest {
    pub id: LeaveRequestId,
    pub member_id: MemberId,
    pub leave_type_id: LeaveTypeId,
    pub range: DateRange,
    pub unit: LeaveUnit,
    pub amount: LeaveAmount,
    pub reason: String,
    pub status: LeaveRequestStatus,
    pub document_id: DocumentId,
    pub created_at: Timestamp,
    pub decided_at: Option<Timestamp>,
    pub version: i64,
}

#[derive(sqlx::FromRow)]
struct RequestRow {
    id: String,
    member_id: String,
    leave_type_id: String,
    start_date: String,
    end_date: String,
    unit: String,
    amount_q: i64,
    reason: String,
    status: String,
    document_id: String,
    created_at: i64,
    decided_at: Option<i64>,
    version: i64,
}

fn amount(column: &'static str, raw: i64) -> DbResult<LeaveAmount> {
    codec::narrow(column, raw).map(LeaveAmount::from_quarters)
}

impl TryFrom<RequestRow> for LeaveRequest {
    type Error = crate::DbError;

    fn try_from(row: RequestRow) -> DbResult<Self> {
        let start = codec::date("start_date", &row.start_date)?;
        let end = codec::date("end_date", &row.end_date)?;
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            member_id: codec::parse("member_id", &row.member_id)?,
            leave_type_id: codec::parse("leave_type_id", &row.leave_type_id)?,
            range: DateRange::new(start, end).map_err(codec::corrupt("end_date"))?,
            unit: codec::parse_with("unit", &row.unit, LeaveUnit::parse)?,
            amount: amount("amount_q", row.amount_q)?,
            reason: row.reason,
            status: codec::parse_with("status", &row.status, LeaveRequestStatus::parse)?,
            document_id: codec::parse("document_id", &row.document_id)?,
            created_at: codec::timestamp("created_at", row.created_at)?,
            decided_at: codec::timestamp_opt("decided_at", row.decided_at)?,
            version: row.version,
        })
    }
}

const REQUEST_COLUMNS: &str = "id, member_id, leave_type_id, start_date, end_date, unit, \
     amount_q, reason, status, document_id, created_at, decided_at, version";

pub async fn insert_request(conn: &mut Conn, request: &LeaveRequest) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO leave_requests
            (id, member_id, leave_type_id, start_date, end_date, unit, amount_q, reason,
             status, document_id, created_at, decided_at, version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(request.id.to_string())
    .bind(request.member_id.to_string())
    .bind(request.leave_type_id.to_string())
    .bind(codec::date_text(request.range.start()))
    .bind(codec::date_text(request.range.end()))
    .bind(request.unit.as_str())
    .bind(request.amount.quarters())
    .bind(&request.reason)
    .bind(request.status.as_str())
    .bind(request.document_id.to_string())
    .bind(request.created_at.as_millis())
    .bind(request.decided_at.map(Timestamp::as_millis))
    .bind(request.version)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn find_request(conn: &mut Conn, id: LeaveRequestId) -> DbResult<Option<LeaveRequest>> {
    sqlx::query_as::<_, RequestRow>(&format!(
        "SELECT {REQUEST_COLUMNS} FROM leave_requests WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_optional(conn)
    .await?
    .map(LeaveRequest::try_from)
    .transpose()
}

/// 결재 문서에 연결된 신청.
pub async fn find_request_by_document(
    conn: &mut Conn,
    document_id: DocumentId,
) -> DbResult<Option<LeaveRequest>> {
    sqlx::query_as::<_, RequestRow>(&format!(
        "SELECT {REQUEST_COLUMNS} FROM leave_requests WHERE document_id = ?"
    ))
    .bind(document_id.to_string())
    .fetch_optional(conn)
    .await?
    .map(LeaveRequest::try_from)
    .transpose()
}

/// 신청의 상태를 바꾼다. `version`이 현재 값과 같을 때만. 바꿨으면 `true`.
pub async fn set_request_status(
    conn: &mut Conn,
    id: LeaveRequestId,
    version: i64,
    status: LeaveRequestStatus,
    decided_at: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE leave_requests SET status = ?, decided_at = ?, version = version + 1
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestFilter {
    pub member_id: Option<MemberId>,
    /// 이 기간과 하루라도 겹치는 신청만.
    pub overlapping: Option<DateRange>,
    /// 비어 있으면 모든 상태.
    pub statuses: Vec<LeaveRequestStatus>,
    /// 이 ID보다 오래된 신청부터(다음 페이지).
    pub before: Option<LeaveRequestId>,
}

/// 최신 접수순 목록. `limit`이 없으면 조건에 맞는 전부(기간·구성원으로 범위를 좁힌 조회용).
pub async fn list_requests(
    conn: &mut Conn,
    filter: &RequestFilter,
    limit: Option<u32>,
) -> DbResult<(Vec<LeaveRequest>, bool)> {
    let mut sql = format!("SELECT {REQUEST_COLUMNS} FROM leave_requests WHERE 1 = 1");
    let mut binds: Vec<String> = Vec::new();
    if let Some(member_id) = filter.member_id {
        sql.push_str(" AND member_id = ?");
        binds.push(member_id.to_string());
    }
    if let Some(range) = filter.overlapping {
        sql.push_str(" AND start_date <= ? AND end_date >= ?");
        binds.extend([
            codec::date_text(range.end()),
            codec::date_text(range.start()),
        ]);
    }
    if !filter.statuses.is_empty() {
        sql.push_str(&format!(
            " AND status IN ({})",
            codec::placeholders(filter.statuses.len())
        ));
        binds.extend(filter.statuses.iter().map(|s| s.as_str().to_owned()));
    }
    if let Some(before) = filter.before {
        sql.push_str(" AND id < ?");
        binds.push(before.to_string());
    }
    sql.push_str(" ORDER BY id DESC");
    if limit.is_some() {
        sql.push_str(" LIMIT ?");
    }
    let mut query = sqlx::query_as::<_, RequestRow>(&sql);
    for value in &binds {
        query = query.bind(value);
    }
    if let Some(limit) = limit {
        query = query.bind(codec::limit_plus_one(limit));
    }
    let items = query
        .fetch_all(conn)
        .await?
        .into_iter()
        .map(LeaveRequest::try_from)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(match limit {
        Some(limit) => codec::split_page(items, limit),
        None => (items, false),
    })
}

/// 결재 중인 신청 수(관리 대시보드).
pub async fn count_pending(conn: &mut Conn) -> DbResult<i64> {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM leave_requests WHERE status = 'pending'")
            .fetch_one(conn)
            .await?;
    Ok(count)
}

/// 이 종류로 접수돼 아직 결재 중인 신청 수.
pub async fn count_pending_for_type(conn: &mut Conn, leave_type_id: LeaveTypeId) -> DbResult<i64> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM leave_requests WHERE leave_type_id = ? AND status = 'pending'",
    )
    .bind(leave_type_id.to_string())
    .fetch_one(conn)
    .await?;
    Ok(count)
}

// ───────────── 원장 ─────────────

/// 원장의 한 줄. 계산에 쓰는 값([`LedgerEntry`])과 기록용 정보를 함께 담는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerRecord {
    pub id: LeaveLedgerEntryId,
    pub member_id: MemberId,
    pub entry: LedgerEntry,
    /// 자동 부여의 멱등성 키. 수동 조정·사용·복원에는 없다.
    pub grant_key: Option<String>,
    pub memo: Option<String>,
    /// 시스템이 넣은 줄(자동 부여)은 `None`.
    pub actor_id: Option<MemberId>,
    pub created_at: Timestamp,
}

#[derive(sqlx::FromRow)]
struct LedgerRow {
    id: String,
    member_id: String,
    kind: String,
    amount_q: i64,
    valid_from: Option<String>,
    expires_on: Option<String>,
    effective_on: Option<String>,
    request_id: Option<String>,
    grant_key: Option<String>,
    memo: Option<String>,
    actor_id: Option<String>,
    created_at: i64,
}

/// 종류에 꼭 있어야 하는 열이 비어 있을 때의 오류.
fn missing<T>(column: &'static str, value: Option<T>) -> DbResult<T> {
    value.ok_or_else(|| {
        codec::corrupt(column)(DomainError::validation(column, ValidationReason::Empty))
    })
}

impl TryFrom<LedgerRow> for LedgerRecord {
    type Error = crate::DbError;

    fn try_from(row: LedgerRow) -> DbResult<Self> {
        let valid_from = codec::date_opt("valid_from", row.valid_from.as_deref())?;
        let expires_on = codec::date_opt("expires_on", row.expires_on.as_deref())?;
        let effective_on = codec::date_opt("effective_on", row.effective_on.as_deref())?;
        let request: Option<LeaveRequestId> =
            codec::parse_opt("request_id", row.request_id.as_deref())?;
        let kind = match LedgerKind::parse(&row.kind).map_err(codec::corrupt("kind"))? {
            LedgerKind::Grant => LedgerEntryKind::Grant {
                valid_from: missing("valid_from", valid_from)?,
                expires_on,
            },
            LedgerKind::Use => LedgerEntryKind::Use {
                effective_on: missing("effective_on", effective_on)?,
                request: missing("request_id", request)?,
            },
            LedgerKind::Restore => LedgerEntryKind::Restore {
                request: missing("request_id", request)?,
            },
            LedgerKind::Adjust => LedgerEntryKind::Adjust {
                effective_on: missing("effective_on", effective_on)?,
            },
        };
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            member_id: codec::parse("member_id", &row.member_id)?,
            entry: LedgerEntry {
                kind,
                amount: amount("amount_q", row.amount_q)?,
            },
            grant_key: row.grant_key,
            memo: row.memo,
            actor_id: codec::parse_opt("actor_id", row.actor_id.as_deref())?,
            created_at: codec::timestamp("created_at", row.created_at)?,
        })
    }
}

/// 종류별로 채워지는 열.
struct LedgerColumns {
    kind: &'static str,
    valid_from: Option<NaiveDate>,
    expires_on: Option<NaiveDate>,
    effective_on: Option<NaiveDate>,
    request: Option<LeaveRequestId>,
}

fn ledger_columns(kind: LedgerEntryKind) -> LedgerColumns {
    let empty = LedgerColumns {
        kind: "",
        valid_from: None,
        expires_on: None,
        effective_on: None,
        request: None,
    };
    match kind {
        LedgerEntryKind::Grant {
            valid_from,
            expires_on,
        } => LedgerColumns {
            kind: LedgerKind::Grant.as_str(),
            valid_from: Some(valid_from),
            expires_on,
            ..empty
        },
        LedgerEntryKind::Use {
            effective_on,
            request,
        } => LedgerColumns {
            kind: LedgerKind::Use.as_str(),
            effective_on: Some(effective_on),
            request: Some(request),
            ..empty
        },
        LedgerEntryKind::Restore { request } => LedgerColumns {
            kind: LedgerKind::Restore.as_str(),
            request: Some(request),
            ..empty
        },
        LedgerEntryKind::Adjust { effective_on } => LedgerColumns {
            kind: LedgerKind::Adjust.as_str(),
            effective_on: Some(effective_on),
            ..empty
        },
    }
}

async fn insert_ledger_with(
    conn: &mut Conn,
    record: &LedgerRecord,
    on_conflict: &'static str,
) -> DbResult<bool> {
    let columns = ledger_columns(record.entry.kind);
    let result = sqlx::query(&format!(
        "INSERT INTO leave_ledger
            (id, member_id, kind, amount_q, valid_from, expires_on, effective_on, request_id,
             grant_key, memo, actor_id, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?){on_conflict}"
    ))
    .bind(record.id.to_string())
    .bind(record.member_id.to_string())
    .bind(columns.kind)
    .bind(record.entry.amount.quarters())
    .bind(columns.valid_from.map(codec::date_text))
    .bind(columns.expires_on.map(codec::date_text))
    .bind(columns.effective_on.map(codec::date_text))
    .bind(columns.request.map(|id| id.to_string()))
    .bind(&record.grant_key)
    .bind(&record.memo)
    .bind(record.actor_id.map(|id| id.to_string()))
    .bind(record.created_at.as_millis())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 원장에 한 줄을 추가한다. 같은 신청의 사용·복원이 이미 있으면 유일 제약 위반이다.
pub async fn insert_ledger(conn: &mut Conn, record: &LedgerRecord) -> DbResult<()> {
    insert_ledger_with(conn, record, "").await?;
    Ok(())
}

/// 자동 부여를 넣는다. 같은 구성원에게 같은 `grant_key`가 이미 있으면 아무 일도 하지 않는다.
/// 새로 넣었으면 `true`.
pub async fn insert_grant_if_absent(conn: &mut Conn, record: &LedgerRecord) -> DbResult<bool> {
    insert_ledger_with(conn, record, " ON CONFLICT DO NOTHING").await
}

const LEDGER_COLUMNS: &str = "id, member_id, kind, amount_q, valid_from, expires_on, \
     effective_on, request_id, grant_key, memo, actor_id, created_at";

/// 구성원의 원장 전체(기록 순서).
pub async fn list_ledger(conn: &mut Conn, member_id: MemberId) -> DbResult<Vec<LedgerRecord>> {
    sqlx::query_as::<_, LedgerRow>(&format!(
        "SELECT {LEDGER_COLUMNS} FROM leave_ledger WHERE member_id = ? ORDER BY rowid"
    ))
    .bind(member_id.to_string())
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(LedgerRecord::try_from)
    .collect()
}

/// 한 번에 묶어 조회하는 구성원 수. SQLite의 바인드 변수 한도보다 충분히 작게 둔다.
const MEMBER_CHUNK: usize = 500;

/// 여러 구성원의 원장을 구성원 수와 무관한 횟수의 조회로 읽는다(구성원별로는 기록 순서).
/// 전원의 잔여를 한꺼번에 계산할 때 구성원마다 조회하지 않으려고 둔다.
pub async fn list_ledger_for_members(
    conn: &mut Conn,
    member_ids: &[MemberId],
) -> DbResult<Vec<LedgerRecord>> {
    let mut records = Vec::new();
    for chunk in member_ids.chunks(MEMBER_CHUNK) {
        let placeholders = codec::placeholders(chunk.len());
        let sql = format!(
            "SELECT {LEDGER_COLUMNS} FROM leave_ledger
             WHERE member_id IN ({placeholders}) ORDER BY rowid"
        );
        let mut query = sqlx::query_as::<_, LedgerRow>(&sql);
        for id in chunk {
            query = query.bind(id.to_string());
        }
        for row in query.fetch_all(&mut *conn).await? {
            records.push(LedgerRecord::try_from(row)?);
        }
    }
    Ok(records)
}

/// 신청에 딸린 사용(예약) 기록. 차감하지 않는 종류의 신청에는 없다.
pub async fn find_use(
    conn: &mut Conn,
    request_id: LeaveRequestId,
) -> DbResult<Option<LedgerRecord>> {
    sqlx::query_as::<_, LedgerRow>(&format!(
        "SELECT {LEDGER_COLUMNS} FROM leave_ledger WHERE request_id = ? AND kind = 'use'"
    ))
    .bind(request_id.to_string())
    .fetch_optional(conn)
    .await?
    .map(LedgerRecord::try_from)
    .transpose()
}
