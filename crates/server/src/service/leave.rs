//! 휴가: 종류, 연차 자동 부여, 잔여·원장, 신청·취소, 달력.
//!
//! 잔여는 원장(`leave_ledger`)으로만 계산한다. 신청할 때 `use` 한 줄(첫 근무일 기준)로 **예약**하고,
//! 반려·회수·취소되면 `restore` 한 줄로 되돌린다. 신청 1건에 `use`와 `restore`는 각각
//! 한 줄뿐이며(DB 유일 인덱스), 확인과 기록이 한 쓰기 트랜잭션이라 동시에 신청해도
//! 같은 잔여를 두 번 쓸 수 없다.
//!
//! 휴가 신청은 결재 문서와 한 몸이다. 신청의 상태는 문서가 종료될 때
//! [`on_document_closed_tx`]로만 바뀌고(승인·반려·회수), 유일한 예외는 이미 승인된 휴가의
//! 취소다([`cancel_tx`] 참고).

use std::collections::BTreeMap;

use chrono::{Datelike, Days, NaiveDate};
use pango_api::leave::{
    AccrualRunResult, AdjustLeaveRequest, AdminLeaveRequestQuery, BalanceDto, CreateLeaveRequest,
    CreateLeaveTypeRequest, LeaveCalendarEntry, LeaveRequestDto, LeaveRequestQuery, LeaveTypeDto,
    LedgerEntryDto, LedgerKind, MemberBalance, UpdateLeaveTypeRequest,
};
use pango_api::Page;
use pango_core::approval::{ApprovalLine, DocumentKind};
use pango_core::authz;
use pango_core::calendar::{check_date, parse_month, DateRange};
use pango_core::error::{ConflictReason, ValidationReason};
use pango_core::id::{DocumentId, LeaveLedgerEntryId, LeaveRequestId, LeaveTypeId, MemberId};
use pango_core::leave::{
    self as rules, Balance, CancelDecision, LeaveAmount, LeaveRequestStatus, LeaveSpan, LeaveUnit,
    LedgerEntry, LedgerEntryKind, ResolutionDecision,
};
use pango_core::text::{Body, Code, LabelName, Reason, Title};
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::leave::{self, LeaveRequest, LeaveType, LedgerRecord, RequestFilter};
use pango_db::members::{self, Member, MemberFilter};
use pango_db::Conn;

use super::approval::{self, LinkedOutcome, SystemDocument};
use super::support::{
    audit, current_member, department_names, found, member_names, members_by_id, next_page, notify,
    personal_records_member, require_current_admin, require_updated, Audit, AuthContext, Company,
    Idempotency, Notify,
};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 이보다 더 지난 날짜로 시작하는 휴가는 신청할 수 없다. 사후 신청은 허용하되,
/// 이미 마감된 달의 근태를 구성원이 뒤늦게 바꾸지 못하게 한다.
const MAX_BACKDATE_DAYS: u64 = 30;
/// "곧 소멸"로 보는 기간(일).
const EXPIRING_SOON_DAYS: u64 = 30;
/// 수동 조정 한 건의 최대 크기(1/4일 단위, 100일).
const MAX_ADJUST_QUARTERS: i32 = 400;
/// 관리자가 회수할 수 없는 신청을 취소할 때 결재 문서에 남기는 반려 사유.
const ADMIN_CANCEL_REASON: &str = "관리자가 휴가 신청을 취소했습니다.";
/// 저장·전송 표기(`YYYY-MM-DD`)로 쓸 수 있는 연도 범위.
const YEARS: std::ops::RangeInclusive<i32> = 2000..=9999;

fn out_of_range(field: &'static str) -> ServiceError {
    DomainError::validation(field, ValidationReason::OutOfRange).into()
}

/// 값으로 받은 날짜가 저장 표기로 쓸 수 있는 범위인지 확인한다. 날짜를 받는 모든 입력이
/// 같은 규칙([`check_date`])을 쓴다.
fn plain_date(field: &'static str, date: NaiveDate) -> ServiceResult<NaiveDate> {
    Ok(check_date(field, date)?)
}

// ───────────── 초기 데이터 ─────────────

/// 기본 휴가 종류를 만든다. 최초 설정과 같은 트랜잭션에서 한 번만 부른다.
pub async fn seed_defaults_tx(conn: &mut Conn, now: Timestamp) -> ServiceResult<()> {
    // (코드, 이름, 잔여 차감, 반차 허용)
    let defaults = [
        ("annual", "연차", true, true),
        ("sick", "병가", false, false),
        ("family_event", "경조사", false, false),
        ("official", "공가", false, false),
    ];
    for ((code, name, deducts_balance, allow_partial), sort_order) in
        defaults.into_iter().zip(0i64..)
    {
        let leave_type = LeaveType {
            id: LeaveTypeId::new(),
            code: code.to_owned(),
            name: name.to_owned(),
            deducts_balance,
            allow_partial,
            active: true,
            sort_order,
            version: 1,
        };
        leave::insert_type(conn, &leave_type, now).await?;
    }
    Ok(())
}

// ───────────── 휴가 종류 ─────────────

fn type_dto(leave_type: LeaveType) -> LeaveTypeDto {
    LeaveTypeDto {
        id: leave_type.id,
        code: leave_type.code,
        name: leave_type.name,
        deducts_balance: leave_type.deducts_balance,
        allow_partial: leave_type.allow_partial,
        active: leave_type.active,
        sort_order: leave_type.sort_order,
        version: leave_type.version,
    }
}

pub async fn list_types(state: &AppState) -> ServiceResult<Vec<LeaveTypeDto>> {
    let types = leave::list_types(&mut *state.read().await?, true).await?;
    Ok(types.into_iter().map(type_dto).collect())
}

pub async fn admin_list_types(
    state: &AppState,
    ctx: &AuthContext,
) -> ServiceResult<Vec<LeaveTypeDto>> {
    authz::require_admin(&ctx.actor)?;
    let types = leave::list_types(&mut *state.read().await?, false).await?;
    Ok(types.into_iter().map(type_dto).collect())
}

async fn audit_type(
    conn: &mut Conn,
    now: Timestamp,
    ctx: &AuthContext,
    action: &'static str,
    leave_type: &LeaveType,
) -> ServiceResult<()> {
    audit(
        conn,
        now,
        Audit {
            action,
            target_type: "leave_type",
            target_id: Some(leave_type.id.to_string()),
            detail: Some(serde_json::json!({
                "code": leave_type.code,
                "name": leave_type.name,
                "deducts_balance": leave_type.deducts_balance,
                "allow_partial": leave_type.allow_partial,
                "active": leave_type.active,
            })),
            ctx: Some(ctx),
        },
    )
    .await
}

pub async fn admin_create_type(
    state: &AppState,
    ctx: &AuthContext,
    request: &CreateLeaveTypeRequest,
    idempotency: Option<&Idempotency>,
) -> ServiceResult<LeaveTypeDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let leave_type = LeaveType {
        id: LeaveTypeId::new(),
        code: Code::parse(&request.code)?.into_string(),
        name: LabelName::parse(&request.name)?.into_string(),
        deducts_balance: request.deducts_balance,
        allow_partial: request.allow_partial,
        active: true,
        sort_order: request.sort_order,
        version: 1,
    };
    state
        .write_idempotent(ctx.member_id(), idempotency, async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            if leave::find_type_by_code(conn, &leave_type.code)
                .await?
                .is_some()
            {
                return Err(DomainError::Conflict(ConflictReason::AlreadyExists).into());
            }
            leave::insert_type(conn, &leave_type, now).await?;
            audit_type(conn, now, ctx, "leave_type.create", &leave_type).await?;
            Ok(type_dto(leave_type.clone()))
        })
        .await
}

/// 휴가 종류 수정. 결재 중인 신청이 있는 종류는 끌 수 없다(`InUse`).
pub async fn admin_update_type(
    state: &AppState,
    ctx: &AuthContext,
    id: LeaveTypeId,
    request: &UpdateLeaveTypeRequest,
) -> ServiceResult<LeaveTypeDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let name = LabelName::parse(&request.name)?.into_string();
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            let current = found(leave::find_type(conn, id).await?)?;
            let deactivating = current.active && !request.active;
            if deactivating && leave::count_pending_for_type(conn, id).await? > 0 {
                return Err(DomainError::Conflict(ConflictReason::InUse).into());
            }
            let leave_type = LeaveType {
                name,
                allow_partial: request.allow_partial,
                active: request.active,
                sort_order: request.sort_order,
                version: request.version,
                ..current
            };
            require_updated(leave::update_type(conn, &leave_type, now).await?)?;
            audit_type(conn, now, ctx, "leave_type.update", &leave_type).await?;
            Ok(type_dto(LeaveType {
                version: leave_type.version + 1,
                ..leave_type
            }))
        })
        .await
}

// ───────────── 자동 부여·잔여 ─────────────

/// `today`까지 있어야 하는 법정 연차 부여 중 아직 없는 것을 원장에 넣는다. 넣은 건수를 돌려준다.
///
/// `grant_key` 유일 제약으로 같은 부여는 한 번만 들어가므로 몇 번을 불러도 결과가 같다.
/// 잔여를 보고 결정을 내리는 모든 유스케이스는 원장을 읽기 전에 같은 트랜잭션에서 이것을 부른다.
pub async fn ensure_accrued_tx(
    conn: &mut Conn,
    member: &Member,
    today: NaiveDate,
    now: Timestamp,
) -> ServiceResult<u32> {
    let mut inserted = 0;
    for grant in rules::accrual_grants(member.hired_on, member.retired_on, today) {
        let record = LedgerRecord {
            id: LeaveLedgerEntryId::new(),
            member_id: member.id,
            entry: grant.to_ledger_entry(),
            grant_key: Some(grant.grant_key),
            memo: None,
            actor_id: None,
            created_at: now,
        };
        if leave::insert_grant_if_absent(conn, &record).await? {
            inserted += 1;
        }
    }
    Ok(inserted)
}

async fn ledger_entries(conn: &mut Conn, member_id: MemberId) -> ServiceResult<Vec<LedgerEntry>> {
    let records = leave::list_ledger(conn, member_id).await?;
    Ok(records.into_iter().map(|record| record.entry).collect())
}

/// 부여를 반영한 뒤 `today` 기준 잔여를 계산한다.
async fn balance_tx(
    conn: &mut Conn,
    member: &Member,
    today: NaiveDate,
    now: Timestamp,
) -> ServiceResult<BalanceDto> {
    ensure_accrued_tx(conn, member, today, now).await?;
    stored_balance_tx(conn, member.id, today).await
}

/// **저장된 원장 그대로** 계산한 `today` 기준 잔여. 아무것도 쓰지 않는다.
///
/// 홈 화면처럼 읽기만 해야 하는 곳에서 쓴다. 오늘 생겼어야 할 자동 부여가 아직 원장에
/// 없으면 결과에도 없다(부여는 하루 한 번 도는 정리 작업과 [`balance`] 조회가 반영한다).
pub async fn stored_balance_tx(
    conn: &mut Conn,
    member_id: MemberId,
    today: NaiveDate,
) -> ServiceResult<BalanceDto> {
    let entries = ledger_entries(conn, member_id).await?;
    let balance = Balance::compute(&entries, today)?;
    // 기간이 끝난 다음 날 기준으로 다시 계산했을 때 늘어난 소멸량이 곧 그 사이에 사라질 수량이다.
    let horizon = today
        .checked_add_days(Days::new(EXPIRING_SOON_DAYS + 1))
        .ok_or_else(|| out_of_range("date"))?;
    let expiring_soon = Balance::compute(&entries, horizon)?
        .expired
        .checked_sub(balance.expired)
        .ok_or_else(|| out_of_range("amount"))?;
    Ok(BalanceDto {
        member_id,
        as_of: today,
        balance,
        expiring_soon,
    })
}

/// 오늘 기준 잔여. 오늘까지 생겼어야 할 부여를 먼저 반영하므로 조회지만 쓰기 트랜잭션이다.
pub async fn balance(
    state: &AppState,
    ctx: &AuthContext,
    member_id: Option<MemberId>,
) -> ServiceResult<BalanceDto> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let member = personal_records_member(conn, ctx, member_id).await?;
            let today = Company::load(conn).await?.today(now);
            balance_tx(conn, &member, today, now).await
        })
        .await
}

fn ledger_dto(record: LedgerRecord) -> LedgerEntryDto {
    let (kind, valid_from, expires_on, effective_on, request_id) = match record.entry.kind {
        LedgerEntryKind::Grant {
            valid_from,
            expires_on,
        } => (LedgerKind::Grant, Some(valid_from), expires_on, None, None),
        LedgerEntryKind::Use {
            effective_on,
            request,
        } => (
            LedgerKind::Use,
            None,
            None,
            Some(effective_on),
            Some(request),
        ),
        LedgerEntryKind::Restore { request } => {
            (LedgerKind::Restore, None, None, None, Some(request))
        }
        LedgerEntryKind::Adjust { effective_on } => {
            (LedgerKind::Adjust, None, None, Some(effective_on), None)
        }
    };
    LedgerEntryDto {
        id: record.id,
        kind,
        amount: record.entry.amount,
        valid_from,
        expires_on,
        effective_on,
        request_id,
        memo: record.memo,
        created_at: record.created_at,
    }
}

/// 원장 전체(기록 순서). 잔여와 같은 내용을 보여 주려고 부여를 먼저 반영한다.
pub async fn ledger(
    state: &AppState,
    ctx: &AuthContext,
    member_id: Option<MemberId>,
) -> ServiceResult<Vec<LedgerEntryDto>> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let member = personal_records_member(conn, ctx, member_id).await?;
            let today = Company::load(conn).await?.today(now);
            ensure_accrued_tx(conn, &member, today, now).await?;
            let records = leave::list_ledger(conn, member.id).await?;
            Ok(records.into_iter().map(ledger_dto).collect())
        })
        .await
}

// ───────────── 신청 조회 ─────────────

async fn request_dtos(
    conn: &mut Conn,
    requests: Vec<LeaveRequest>,
) -> ServiceResult<Vec<LeaveRequestDto>> {
    let names = member_names(conn, requests.iter().map(|r| r.member_id)).await?;
    let types: BTreeMap<LeaveTypeId, String> = leave::list_types(conn, false)
        .await?
        .into_iter()
        .map(|leave_type| (leave_type.id, leave_type.name))
        .collect();
    Ok(requests
        .into_iter()
        .map(|request| LeaveRequestDto {
            id: request.id,
            member_id: request.member_id,
            member_name: names.get(&request.member_id).cloned().unwrap_or_default(),
            leave_type_id: request.leave_type_id,
            leave_type_name: types
                .get(&request.leave_type_id)
                .cloned()
                .unwrap_or_default(),
            start_date: request.range.start(),
            end_date: request.range.end(),
            unit: request.unit,
            amount: request.amount,
            reason: request.reason,
            status: request.status,
            document_id: request.document_id,
            created_at: request.created_at,
            decided_at: request.decided_at,
        })
        .collect())
}

async fn request_dto(conn: &mut Conn, id: LeaveRequestId) -> ServiceResult<LeaveRequestDto> {
    let request = found(leave::find_request(conn, id).await?)?;
    let mut dtos = request_dtos(conn, vec![request]).await?;
    found(dtos.pop())
}

/// 한 해의 신청 목록(그 해와 하루라도 겹치는 신청).
pub async fn list_requests(
    state: &AppState,
    ctx: &AuthContext,
    query: &LeaveRequestQuery,
) -> ServiceResult<Vec<LeaveRequestDto>> {
    let now = state.now();
    let mut conn = state.read().await?;
    let member = personal_records_member(&mut conn, ctx, query.member_id).await?;
    let year = match query.year {
        Some(year) if YEARS.contains(&year) => year,
        Some(_) => return Err(out_of_range("year")),
        None => Company::load(&mut conn).await?.today(now).year(),
    };
    let (first, last) = (
        NaiveDate::from_ymd_opt(year, 1, 1),
        NaiveDate::from_ymd_opt(year, 12, 31),
    );
    let (Some(first), Some(last)) = (first, last) else {
        return Err(out_of_range("year"));
    };
    let filter = RequestFilter {
        member_id: Some(member.id),
        overlapping: Some(DateRange::new(first, last)?),
        ..RequestFilter::default()
    };
    let (requests, _) = leave::list_requests(&mut conn, &filter, None).await?;
    request_dtos(&mut conn, requests).await
}

pub async fn admin_list_requests(
    state: &AppState,
    ctx: &AuthContext,
    query: &AdminLeaveRequestQuery,
) -> ServiceResult<Page<LeaveRequestDto>> {
    authz::require_admin(&ctx.actor)?;
    let page = query.page();
    let filter = RequestFilter {
        member_id: query.member_id,
        overlapping: None,
        statuses: query.status.into_iter().collect(),
        before: page.after()?,
    };
    let mut conn = state.read().await?;
    let (requests, has_more) = leave::list_requests(&mut conn, &filter, Some(page.limit())).await?;
    let items = request_dtos(&mut conn, requests).await?;
    next_page(items, has_more, |last: &LeaveRequestDto| last.id)
}

const SCHEDULED: [LeaveRequestStatus; 2] =
    [LeaveRequestStatus::Pending, LeaveRequestStatus::Approved];

/// 그 달에 걸친 대기·승인 신청 중, `visible`이 허용하는 구성원의 것.
async fn scheduled_in_month(
    conn: &mut Conn,
    month: &str,
    visible: impl Fn(&Member) -> bool,
) -> ServiceResult<Vec<LeaveRequest>> {
    let filter = RequestFilter {
        overlapping: Some(parse_month(month)?),
        statuses: SCHEDULED.to_vec(),
        ..RequestFilter::default()
    };
    let (requests, _) = leave::list_requests(conn, &filter, None).await?;
    let people = members_by_id(conn, requests.iter().map(|request| request.member_id)).await?;
    let mut shown: Vec<LeaveRequest> = requests
        .into_iter()
        .filter(|request| people.get(&request.member_id).is_some_and(&visible))
        .collect();
    shown.sort_by_key(|request| (request.range.start(), request.id));
    Ok(shown)
}

/// 휴가 달력: 누가 언제 쉬는지. 같은 부서 동료까지 보이므로 사유와 휴가 종류는 싣지 않는다.
pub async fn calendar(
    state: &AppState,
    ctx: &AuthContext,
    month: &str,
) -> ServiceResult<Vec<LeaveCalendarEntry>> {
    let mut conn = state.read().await?;
    let requests = scheduled_in_month(&mut conn, month, |member| {
        authz::view_leave_schedule(&ctx.actor, &member.subject()).is_ok()
    })
    .await?;
    let names = member_names(&mut conn, requests.iter().map(|r| r.member_id)).await?;
    Ok(requests
        .into_iter()
        .map(|request| LeaveCalendarEntry {
            member_id: request.member_id,
            member_name: names.get(&request.member_id).cloned().unwrap_or_default(),
            start_date: request.range.start(),
            end_date: request.range.end(),
            unit: request.unit,
            status: request.status,
        })
        .collect())
}

/// 팀 휴가(조직장·관리자): 개인 기록을 볼 수 있는 구성원의 신청을 종류·사유까지 보여 준다.
pub async fn team_leave(
    state: &AppState,
    ctx: &AuthContext,
    month: &str,
) -> ServiceResult<Vec<LeaveRequestDto>> {
    authz::require_manager(&ctx.actor)?;
    let mut conn = state.read().await?;
    let requests = scheduled_in_month(&mut conn, month, |member| {
        authz::view_personal_records(&ctx.actor, &member.subject()).is_ok()
    })
    .await?;
    request_dtos(&mut conn, requests).await
}

/// 기간 안의 (구성원, 날짜)별 휴가 단위. 승인된 신청의 근무일만 담는다.
/// 같은 날에 반차·반반차가 둘 있으면 더 큰 단위를 돌려준다. `member_id`가 없으면 전원.
async fn approved_units(
    conn: &mut Conn,
    member_id: Option<MemberId>,
    range: DateRange,
) -> ServiceResult<BTreeMap<(MemberId, NaiveDate), LeaveUnit>> {
    let filter = RequestFilter {
        member_id,
        overlapping: Some(range),
        statuses: vec![LeaveRequestStatus::Approved],
        before: None,
    };
    let (requests, _) = leave::list_requests(conn, &filter, None).await?;
    let calendar = Company::load(conn).await?.calendar(conn, range).await?;
    let mut units: BTreeMap<(MemberId, NaiveDate), LeaveUnit> = BTreeMap::new();
    for request in &requests {
        let days = request
            .range
            .days()
            .filter(|day| range.contains(*day) && calendar.is_working_day(*day));
        for day in days {
            let key = (request.member_id, day);
            let larger = units
                .get(&key)
                .is_none_or(|current| request.unit.amount_per_day() > current.amount_per_day());
            if larger {
                units.insert(key, request.unit);
            }
        }
    }
    Ok(units)
}

/// 기간 안 **전원**의 (구성원, 날짜)별 휴가 단위. 여러 구성원의 근태를 한꺼번에 판정할 때
/// 구성원마다 조회하지 않으려고 둔다(신청 조회 한 번, 달력 조회 한 번).
pub async fn units_in_range(
    conn: &mut Conn,
    range: DateRange,
) -> ServiceResult<BTreeMap<(MemberId, NaiveDate), LeaveUnit>> {
    approved_units(conn, None, range).await
}

/// 한 구성원의 기간 안 날짜별 휴가 단위. 휴가 기능과 근태가 만나는 지점이다.
///
/// 근태 판정(지각·조퇴 기준 조정, 종일 휴가의 `on_leave`)은 **모두** 이 함수(여러 구성원이면
/// [`units_in_range`])를 거친다. 승인된 휴가만 반영한다.
pub async fn units_by_date(
    conn: &mut Conn,
    member_id: MemberId,
    range: DateRange,
) -> ServiceResult<BTreeMap<NaiveDate, LeaveUnit>> {
    let units = approved_units(conn, Some(member_id), range).await?;
    Ok(units
        .into_iter()
        .map(|((_, day), unit)| (day, unit))
        .collect())
}

/// 그날 휴가인 모든 구성원의 휴가 단위. 전원의 하루 현황을 셀 때 구성원마다 조회하지 않으려고 둔다.
pub async fn units_on(
    conn: &mut Conn,
    date: NaiveDate,
) -> ServiceResult<BTreeMap<MemberId, LeaveUnit>> {
    let units = approved_units(conn, None, DateRange::single(date)).await?;
    Ok(units
        .into_iter()
        .map(|((member_id, _), unit)| (member_id, unit))
        .collect())
}

/// 오늘부터 `days`일 안에 걸친 내 휴가(결재 중·승인), 시작일순. 홈 화면용.
pub async fn upcoming_tx(
    conn: &mut Conn,
    member_id: MemberId,
    today: NaiveDate,
    days: u64,
) -> ServiceResult<Vec<LeaveRequestDto>> {
    let until = today
        .checked_add_days(Days::new(days))
        .ok_or_else(|| out_of_range("date"))?;
    let filter = RequestFilter {
        member_id: Some(member_id),
        overlapping: Some(DateRange::new(today, until)?),
        statuses: SCHEDULED.to_vec(),
        before: None,
    };
    let (mut requests, _) = leave::list_requests(conn, &filter, None).await?;
    requests.sort_by_key(|request| (request.range.start(), request.id));
    request_dtos(conn, requests).await
}

// ───────────── 신청 ─────────────

/// 같은 구성원의 대기·승인 신청과 기간이 겹치면(같은 날 서로 다른 시간대의 반차·반반차는
/// 제외, `rules::requests_overlap`) `LeaveOverlap`.
async fn reject_overlap_tx(
    conn: &mut Conn,
    member_id: MemberId,
    range: DateRange,
    unit: LeaveUnit,
) -> ServiceResult<()> {
    let active = RequestFilter {
        member_id: Some(member_id),
        overlapping: Some(range),
        statuses: SCHEDULED.to_vec(),
        before: None,
    };
    let (existing, _) = leave::list_requests(conn, &active, None).await?;
    let overlaps = existing
        .iter()
        .any(|other| rules::requests_overlap((other.range, other.unit), (range, unit)));
    if overlaps {
        return Err(DomainError::Conflict(ConflictReason::LeaveOverlap).into());
    }
    Ok(())
}

/// 휴가 신청의 결재 문서(시스템 양식). 본문은 사유, 페이로드는 신청 내용이다.
fn leave_document(
    id: LeaveRequestId,
    leave_type: &LeaveType,
    span: &LeaveSpan,
    reason: &Reason,
    line: ApprovalLine,
) -> ServiceResult<SystemDocument> {
    let range = span.range();
    Ok(SystemDocument {
        kind: DocumentKind::Leave,
        form_code: approval::LEAVE_FORM_CODE,
        title: Title::parse(&format!("휴가 신청 ({})", span_label(span)))?,
        body: Body::parse(reason.as_str())?,
        payload: serde_json::json!({
            "request_id": id,
            "leave_type_id": leave_type.id,
            "leave_type_name": leave_type.name,
            "start_date": range.start(),
            "end_date": range.end(),
            "unit": span.unit(),
            "amount_q": span.amount(),
            "reason": reason.as_str(),
        }),
        line,
    })
}

/// 신청의 잔여 예약: 원장에 신청 수량만큼의 사용 한 줄(기준일은 첫 근무일).
async fn reserve_tx(
    conn: &mut Conn,
    request: &LeaveRequest,
    first_day: NaiveDate,
    now: Timestamp,
) -> ServiceResult<()> {
    let reservation = LedgerRecord {
        id: LeaveLedgerEntryId::new(),
        member_id: request.member_id,
        entry: LedgerEntry {
            kind: LedgerEntryKind::Use {
                effective_on: first_day,
                request: request.id,
            },
            amount: request.amount,
        },
        grant_key: None,
        memo: None,
        actor_id: Some(request.member_id),
        created_at: now,
    };
    Ok(leave::insert_ledger(conn, &reservation).await?)
}

fn span_label(span: &LeaveSpan) -> String {
    format!(
        "{} ~ {}, {}일",
        span.range().start(),
        span.range().end(),
        span.amount()
    )
}

/// 휴가 신청. 한 트랜잭션에서 부여 반영 → 일수 계산 → 겹침 확인 → 잔여 확인과 예약 →
/// 결재 문서 상신 → 신청 저장까지 한다. 어느 단계든 실패하면 아무것도 남지 않는다.
pub async fn create_request(
    state: &AppState,
    ctx: &AuthContext,
    request: &CreateLeaveRequest,
    idempotency: Option<&Idempotency>,
) -> ServiceResult<LeaveRequestDto> {
    let now = state.now();
    let reason = Reason::parse(&request.reason)?;
    let range = DateRange::new(
        plain_date("start_date", request.start_date)?,
        plain_date("end_date", request.end_date)?,
    )?;
    state
        .write_idempotent(ctx.member_id(), idempotency, async |conn: &mut Conn| {
            // 인증 뒤에 부서가 바뀌었거나 입사일이 정정됐을 수 있다. 연차 부여(입사일)와
            // 최종 결재자 확인(소속)은 지금의 값으로 한다.
            let member = &current_member(conn, ctx).await?;
            let company = Company::load(conn).await?;
            let today = company.today(now);
            let earliest = today.checked_sub_days(Days::new(MAX_BACKDATE_DAYS));
            if earliest.is_none_or(|earliest| range.start() < earliest) {
                return Err(out_of_range("start_date"));
            }
            let leave_type = found(leave::find_type(conn, request.leave_type_id).await?)?;
            if !leave_type.active {
                return Err(DomainError::Conflict(ConflictReason::LeaveTypeInactive).into());
            }
            if request.unit.is_partial() && !leave_type.allow_partial {
                return Err(DomainError::validation("unit", ValidationReason::Inconsistent).into());
            }
            ensure_accrued_tx(conn, member, today, now).await?;

            let calendar = company.calendar(conn, range).await?;
            let span = LeaveSpan::new(range, request.unit, &calendar)?;
            reject_overlap_tx(conn, member.id, range, request.unit).await?;

            // 총량이 첫 근무일 잔여 안에 들고, 쉬는 날 하루하루가 그날 유효한 부여로 덮여야
            // 한다. 예약은 아래에서 첫 근무일 한 줄로 기록한다(`rules::check_reservable` 참고).
            if leave_type.deducts_balance {
                let entries = ledger_entries(conn, member.id).await?;
                rules::check_reservable(&entries, &span)?;
            }

            let id = LeaveRequestId::new();
            let line = approval::request_line_tx(
                conn,
                member,
                request.steps.as_deref(),
                &request.references,
            )
            .await?;
            let document = leave_document(id, &leave_type, &span, &reason, line)?;
            let document_id = approval::create_submitted_tx(conn, member, document, now).await?;

            let stored = LeaveRequest {
                id,
                member_id: member.id,
                leave_type_id: leave_type.id,
                range,
                unit: request.unit,
                amount: span.amount(),
                reason: reason.into_string(),
                status: LeaveRequestStatus::Pending,
                document_id,
                created_at: now,
                decided_at: None,
                version: 1,
            };
            leave::insert_request(conn, &stored).await?;
            // 원장이 신청을 참조하므로 신청을 넣은 뒤에 기록한다(같은 트랜잭션).
            if leave_type.deducts_balance {
                reserve_tx(conn, &stored, span.first_day(), now).await?;
            }
            audit_request(conn, now, ctx, "leave.request", &stored).await?;
            request_dto(conn, id).await
        })
        .await
}

async fn audit_request(
    conn: &mut Conn,
    now: Timestamp,
    ctx: &AuthContext,
    action: &'static str,
    request: &LeaveRequest,
) -> ServiceResult<()> {
    audit(
        conn,
        now,
        Audit {
            action,
            target_type: "leave_request",
            target_id: Some(request.id.to_string()),
            detail: Some(serde_json::json!({
                "member_id": request.member_id,
                "start_date": request.range.start(),
                "end_date": request.range.end(),
                "amount_q": request.amount,
            })),
            ctx: Some(ctx),
        },
    )
    .await
}

// ───────────── 종료·취소 ─────────────

/// 신청을 끝난 상태로 바꾸고, 필요하면 예약한 잔여를 되돌린다.
///
/// 되돌릴지는 종류의 현재 설정이 아니라 **실제로 예약한 기록(`use`)이 있는지**로 정한다.
/// 복원은 신청당 한 줄만 들어갈 수 있으므로(유일 인덱스) 두 번 되돌리려 하면 전체가 실패한다.
async fn close_request_tx(
    conn: &mut Conn,
    request: &LeaveRequest,
    status: LeaveRequestStatus,
    restore_balance: bool,
    actor_id: MemberId,
    now: Timestamp,
) -> ServiceResult<()> {
    let updated = leave::set_request_status(conn, request.id, request.version, status, now).await?;
    require_updated(updated)?;
    if !restore_balance {
        return Ok(());
    }
    let Some(reserved) = leave::find_use(conn, request.id).await? else {
        return Ok(());
    };
    let restore = LedgerRecord {
        id: LeaveLedgerEntryId::new(),
        member_id: request.member_id,
        entry: LedgerEntry {
            kind: LedgerEntryKind::Restore {
                request: request.id,
            },
            amount: reserved.entry.amount,
        },
        grant_key: None,
        memo: None,
        actor_id: Some(actor_id),
        created_at: now,
    };
    leave::insert_ledger(conn, &restore).await?;
    Ok(())
}

/// 휴가가 실제로 시작되는 날(첫 근무일). 신청 뒤에 공휴일이 바뀌어 근무일이 하나도
/// 남지 않았다면 신청 구간의 첫날로 본다.
async fn leave_start_tx(
    conn: &mut Conn,
    company: &Company,
    request: &LeaveRequest,
) -> ServiceResult<NaiveDate> {
    let calendar = company.calendar(conn, request.range).await?;
    Ok(LeaveSpan::new(request.range, request.unit, &calendar)
        .map_or(request.range.start(), |span| span.first_day()))
}

/// 휴가 신청 문서가 종료됐을 때 결재가 같은 트랜잭션에서 부르는 연결 효과.
///
/// - 최종 승인 → `approved`. 신청할 때 예약한 차감이 그대로 확정된다.
/// - 반려 → `rejected` + 복원, 회수 → `canceled` + 복원.
/// - 같은 결과가 다시 오면 아무것도 하지 않는다.
///
/// 문서에 연결된 신청이 없으면 오류다. 그러면 결재 전이 전체가 롤백되어, 문서만 승인되고
/// 휴가는 반영되지 않는 상태가 생기지 않는다.
pub async fn on_document_closed_tx(
    conn: &mut Conn,
    document_id: DocumentId,
    outcome: LinkedOutcome,
    actor_id: MemberId,
    now: Timestamp,
) -> ServiceResult<()> {
    let request = leave::find_request_by_document(conn, document_id)
        .await?
        .ok_or_else(|| ServiceError::internal("leave document has no linked request"))?;
    let (status, restore_balance) = match outcome {
        LinkedOutcome::Completed | LinkedOutcome::Rejected => {
            let approve = outcome == LinkedOutcome::Completed;
            match rules::decide_resolution(request.status, approve)? {
                ResolutionDecision::Resolve {
                    status,
                    restore_balance,
                } => (status, restore_balance),
                ResolutionDecision::AlreadyResolved => return Ok(()),
            }
        }
        LinkedOutcome::Withdrawn => {
            // 회수는 아무도 결재하기 전에만 되므로 신청은 대기 중이고, 날짜와 무관하게 취소된다.
            let company = Company::load(conn).await?;
            let start = leave_start_tx(conn, &company, &request).await?;
            match rules::decide_cancel(request.status, start, company.today(now), false)? {
                CancelDecision::Cancel { restore_balance } => {
                    (LeaveRequestStatus::Canceled, restore_balance)
                }
                CancelDecision::AlreadyCanceled => return Ok(()),
            }
        }
    };
    close_request_tx(conn, &request, status, restore_balance, actor_id, now).await
}

/// 휴가를 취소하는 자격. 본인 자격의 취소와 관리자 권한의 취소는 규칙이 다르다([`cancel_tx`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Canceller {
    /// 신청자 본인.
    Owner,
    /// 관리자 권한(다른 사람의 신청).
    Admin,
}

/// 휴가 취소의 공통 경로.
///
/// 신청과 결재 문서가 어긋나지 않게 하는 규칙:
/// - **대기 중**: 취소는 곧 문서 **회수**다. 문서를 회수하면 그 연결 효과
///   ([`on_document_closed_tx`])가 신청을 `canceled`로 바꾸고 잔여를 복원한다. 따라서 결재
///   화면에서 회수하든 휴가 화면에서 취소하든 같은 경로, 같은 결과다. 누군가 이미 결재한
///   문서는 회수할 수 없으므로 **본인의** 취소는 `WithdrawTooLate`로 거부한다(남은 결재자의
///   반려로 끝낼 수 있고, 그때 잔여가 복원된다). 문서만 진행 중으로 남는 일을 막기 위해서다.
///   **관리자의** 취소는 그 경우 문서를 대리 반려로 끝낸다
///   ([`approval::close_linked_by_admin_tx`]). 남은 결재자가 정지·퇴사해 아무도 끝낼 수 없는
///   신청의 잔여가 영영 예약돼 있지 않게 하기 위해서다. 이때 신청은 문서와 같이 `rejected`가
///   되고 잔여가 복원된다.
/// - **승인됨**: 결재는 이미 끝난 기록이므로 문서는 `approved`로 둔 채 신청만 `canceled`로
///   바꾸고 잔여를 복원한다. 휴가가 실제로 유효한지는 신청의 상태가 정한다. 결재했던
///   사람들에게 취소를 알린다. 본인은 휴가 시작 전에만, 관리자는 언제든 할 수 있다.
/// - **이미 취소됨**: 아무것도 하지 않고 성공으로 답한다.
async fn cancel_tx(
    conn: &mut Conn,
    ctx: &AuthContext,
    request: &LeaveRequest,
    canceller: Canceller,
    now: Timestamp,
) -> ServiceResult<()> {
    let by_admin = canceller == Canceller::Admin;
    let company = Company::load(conn).await?;
    let start = leave_start_tx(conn, &company, request).await?;
    let decision = rules::decide_cancel(request.status, start, company.today(now), by_admin)?;
    let CancelDecision::Cancel { restore_balance } = decision else {
        return Ok(());
    };
    let actor_id = ctx.member_id();
    if request.status == LeaveRequestStatus::Pending && by_admin {
        let reason = Reason::parse(ADMIN_CANCEL_REASON)?;
        approval::close_linked_by_admin_tx(conn, ctx, request.document_id, reason, now).await?;
    } else if request.status == LeaveRequestStatus::Pending {
        approval::withdraw_linked_tx(conn, actor_id, request.document_id, now).await?;
    } else {
        close_request_tx(
            conn,
            request,
            LeaveRequestStatus::Canceled,
            restore_balance,
            actor_id,
            now,
        )
        .await?;
        notify_canceled_tx(conn, request, actor_id, now).await?;
    }
    let action = if by_admin {
        "leave.admin_cancel"
    } else {
        "leave.cancel"
    };
    audit_request(conn, now, ctx, action, request).await
}

/// 승인된 휴가가 취소됐음을 결재했던 사람들과(다른 사람이 취소했다면) 본인에게 알린다.
async fn notify_canceled_tx(
    conn: &mut Conn,
    request: &LeaveRequest,
    actor_id: MemberId,
    now: Timestamp,
) -> ServiceResult<()> {
    let approvers = approval::approvers_tx(conn, request.document_id).await?;
    let recipients = approvers
        .into_iter()
        .chain([request.member_id])
        .filter(|member| *member != actor_id);
    let dedupe_key = format!("leave.canceled:{}", request.id);
    for to in recipients {
        notify(
            conn,
            now,
            Notify {
                to,
                kind: "leave.canceled",
                title: "승인된 휴가가 취소되었습니다".to_owned(),
                body: format!("{} ~ {}", request.range.start(), request.range.end()),
                link: Some(approval::document_link(request.document_id)),
                dedupe_key: Some(&dedupe_key),
            },
        )
        .await?;
    }
    Ok(())
}

/// 본인의 휴가 취소. 남의 신청은 `NotFound`다.
pub async fn cancel_request(
    state: &AppState,
    ctx: &AuthContext,
    id: LeaveRequestId,
) -> ServiceResult<LeaveRequestDto> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let request = found(leave::find_request(conn, id).await?)?;
            authz::require_own(&ctx.actor, request.member_id, DomainError::NotFound)?;
            cancel_tx(conn, ctx, &request, Canceller::Owner, now).await?;
            request_dto(conn, id).await
        })
        .await
}

/// 관리자의 휴가 취소. 이미 시작했거나 지난 휴가도 취소할 수 있다.
///
/// 관리자는 자기 휴가를 이 권한으로 취소할 수 없다([`authz::edit_personal_records`]). 본인
/// 자격의 취소([`cancel_request`])는 일반 구성원과 같은 규칙으로 할 수 있다.
pub async fn admin_cancel_request(
    state: &AppState,
    ctx: &AuthContext,
    id: LeaveRequestId,
) -> ServiceResult<LeaveRequestDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let actor = require_current_admin(conn, ctx).await?;
            let request = found(leave::find_request(conn, id).await?)?;
            let member = found(members::find(conn, request.member_id).await?)?;
            authz::edit_personal_records(&actor, &member.subject())?;
            cancel_tx(conn, ctx, &request, Canceller::Admin, now).await?;
            request_dto(conn, id).await
        })
        .await
}

// ───────────── 관리 ─────────────

/// 원장에 아직 없는 자동 부여가 있는지(읽기만 한다).
fn accrual_is_behind(member: &Member, ledger: &[LedgerRecord], today: NaiveDate) -> bool {
    rules::accrual_grants(member.hired_on, member.retired_on, today)
        .iter()
        .any(|grant| {
            ledger
                .iter()
                .all(|record| record.grant_key.as_deref() != Some(grant.grant_key.as_str()))
        })
}

/// 재직 구성원 전원의 오늘 기준 잔여.
///
/// 구성원 수만큼 계산하는 긴 읽기이므로 **읽기 연결**에서 한다. 하나뿐인 쓰기 연결을 쥔 채
/// 계산하면 그동안 모든 쓰기(출근, 결재)가 멈춘다. 구성원·부서·원장은 구성원 수와 무관한
/// 횟수의 조회로 한 시점의 것을 읽는다.
///
/// 쓰기 트랜잭션은 오늘까지 생겼어야 할 자동 부여가 원장에 없는 구성원이 있을 때만, 그
/// 구성원들의 부여를 넣기 위해 연다. 하루 한 번 도는 정리 작업이 부여를 넣어 두므로 보통은
/// 열리지 않는다. 결과는 부여를 먼저 반영한 본인 조회([`balance`])와 같다.
pub async fn admin_balances(
    state: &AppState,
    ctx: &AuthContext,
) -> ServiceResult<Vec<MemberBalance>> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let (today, departments, mut people, mut ledgers) = {
        let mut snapshot = state.read_snapshot().await?;
        let conn: &mut Conn = &mut snapshot;
        let today = Company::load(conn).await?.today(now);
        let departments = department_names(conn).await?;
        let people = members::list(conn, &MemberFilter::active()).await?;
        let ids: Vec<MemberId> = people.iter().map(|member| member.id).collect();
        let mut ledgers: BTreeMap<MemberId, Vec<LedgerRecord>> = BTreeMap::new();
        for record in leave::list_ledger_for_members(conn, &ids).await? {
            ledgers.entry(record.member_id).or_default().push(record);
        }
        (today, departments, people, ledgers)
    };

    let behind: Vec<MemberId> = people
        .iter()
        .filter(|member| {
            let ledger = ledgers.get(&member.id).map_or(&[][..], Vec::as_slice);
            accrual_is_behind(member, ledger, today)
        })
        .map(|member| member.id)
        .collect();
    if !behind.is_empty() {
        let refreshed = state
            .write(async |conn: &mut Conn| {
                require_current_admin(conn, ctx).await?;
                let mut refreshed = Vec::with_capacity(behind.len());
                for id in &behind {
                    // 읽은 뒤에 입사일이 정정됐을 수 있으므로 지금의 값으로 부여한다.
                    let member = found(members::find(conn, *id).await?)?;
                    ensure_accrued_tx(conn, &member, today, now).await?;
                    let ledger = leave::list_ledger(conn, *id).await?;
                    refreshed.push((member, ledger));
                }
                Ok(refreshed)
            })
            .await?;
        for (member, ledger) in refreshed {
            ledgers.insert(member.id, ledger);
            if let Some(slot) = people.iter_mut().find(|slot| slot.id == member.id) {
                *slot = member;
            }
        }
    }

    people
        .into_iter()
        .map(|member| {
            let entries: Vec<LedgerEntry> = ledgers
                .get(&member.id)
                .map(|ledger| ledger.iter().map(|record| record.entry).collect())
                .unwrap_or_default();
            Ok(MemberBalance {
                member_id: member.id,
                department_name: member
                    .department_id
                    .and_then(|id| departments.get(&id).cloned()),
                member_name: member.name,
                hired_on: member.hired_on,
                balance: Balance::compute(&entries, today)?,
            })
        })
        .collect()
}

/// 수동 조정. 양수는 그날부터 유효한 부여, 음수는 그날의 사용으로 계산된다.
/// 음수 조정은 잔여를 음수(부채)로 만들 수 있다. 출근율 미달 등을 바로잡는 용도이기 때문이다.
///
/// 관리자는 자기 연차를 조정할 수 없다(최고 관리자는 가능, [`authz::edit_personal_records`]).
pub async fn admin_adjust(
    state: &AppState,
    ctx: &AuthContext,
    request: &AdjustLeaveRequest,
    idempotency: Option<&Idempotency>,
) -> ServiceResult<BalanceDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let amount = request.amount_q;
    if amount == 0
        || amount
            .checked_abs()
            .is_none_or(|abs| abs > MAX_ADJUST_QUARTERS)
    {
        return Err(out_of_range("amount_q"));
    }
    let effective_on = plain_date("effective_on", request.effective_on)?;
    let memo = Reason::parse(&request.memo).map_err(|error| error.with_field("memo"))?;
    state
        .write_idempotent(ctx.member_id(), idempotency, async |conn: &mut Conn| {
            let actor = require_current_admin(conn, ctx).await?;
            let member = found(members::find(conn, request.member_id).await?)?;
            authz::edit_personal_records(&actor, &member.subject())?;
            let today = Company::load(conn).await?.today(now);
            ensure_accrued_tx(conn, &member, today, now).await?;
            let record = LedgerRecord {
                id: LeaveLedgerEntryId::new(),
                member_id: member.id,
                entry: LedgerEntry {
                    kind: LedgerEntryKind::Adjust { effective_on },
                    amount: LeaveAmount::from_quarters(amount),
                },
                grant_key: None,
                memo: Some(memo.as_str().to_owned()),
                actor_id: Some(ctx.member_id()),
                created_at: now,
            };
            leave::insert_ledger(conn, &record).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "leave.adjust",
                    target_type: "member",
                    target_id: Some(member.id.to_string()),
                    detail: Some(serde_json::json!({
                        "amount_q": amount,
                        "effective_on": effective_on,
                        "memo": memo.as_str(),
                    })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            balance_tx(conn, &member, today, now).await
        })
        .await
}

/// 재직 구성원 전원에게 `today`까지의 자동 부여를 반영한다. 관리자의 수동 실행과 하루 한 번
/// 도는 정리 작업이 같은 길을 쓴다.
async fn run_accrual_tx(
    conn: &mut Conn,
    today: NaiveDate,
    now: Timestamp,
) -> ServiceResult<AccrualRunResult> {
    let people = members::list(conn, &MemberFilter::active()).await?;
    let mut grants_inserted = 0u32;
    for member in &people {
        let inserted = ensure_accrued_tx(conn, member, today, now).await?;
        grants_inserted = grants_inserted.saturating_add(inserted);
    }
    Ok(AccrualRunResult {
        members: u32::try_from(people.len()).unwrap_or(u32::MAX),
        grants_inserted,
    })
}

async fn audit_accrual_tx(
    conn: &mut Conn,
    now: Timestamp,
    ctx: Option<&AuthContext>,
    result: AccrualRunResult,
) -> ServiceResult<()> {
    audit(
        conn,
        now,
        Audit {
            action: "leave.accrual_run",
            target_type: "leave_ledger",
            target_id: None,
            detail: Some(serde_json::json!({
                "members": result.members,
                "grants_inserted": result.grants_inserted,
            })),
            ctx,
        },
    )
    .await
}

/// 재직 구성원 전원의 자동 부여를 반영한다. 몇 번을 실행해도, 동시에 실행해도 같은 부여는
/// 한 번만 들어간다.
pub async fn admin_run_accrual(
    state: &AppState,
    ctx: &AuthContext,
) -> ServiceResult<AccrualRunResult> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            let today = Company::load(conn).await?.today(now);
            let result = run_accrual_tx(conn, today, now).await?;
            audit_accrual_tx(conn, now, Some(ctx), result).await?;
            Ok(result)
        })
        .await
}

/// 정리 작업이 실행하는 자동 부여(행위자 없는 시스템 작업). `today`는 회사 시간대의 오늘이다.
///
/// 새로 들어간 부여가 있을 때만 감사 로그를 남긴다. 매일 도는 작업이 아무것도 바꾸지 않은
/// 날까지 한 줄씩 쌓지 않기 위해서다.
pub async fn run_scheduled_accrual(
    state: &AppState,
    today: NaiveDate,
) -> ServiceResult<AccrualRunResult> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let result = run_accrual_tx(conn, today, now).await?;
            if result.grants_inserted > 0 {
                audit_accrual_tx(conn, now, None, result).await?;
            }
            Ok(result)
        })
        .await
}
