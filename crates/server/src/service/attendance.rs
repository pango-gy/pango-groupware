//! 출퇴근: 출근·퇴근, 월별 기록·요약, 변경 이력, 팀 현황, 관리자 생성·수정·내보내기.
//!
//! 판정 규칙(출근·퇴근 전이, 하루 근태, 수정 값 검증)은 모두 `pango_core::attendance`에 있다.
//! 이 모듈은 기록을 읽어 규칙에 넘기고, 결과를 저장하고, 이력·감사 로그를 같은 트랜잭션에 남긴다.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};

use chrono::NaiveDate;
use pango_api::attendance::{
    AdminAttendanceItem, AdminAttendanceQuery, AdminCreateRecordRequest, AdminEditRecordRequest,
    AttendanceEventDto, AttendanceRecordDto, ClockInRequest, DayEntry, ExportQuery, MonthQuery,
    MonthRecords, MonthlySummaryDto, TeamAttendance, TeamAttendanceEntry, TeamAttendanceQuery,
    TodayResponse,
};
use pango_api::{Page, PageQuery};
use pango_core::attendance::{
    decide_clock_in, decide_clock_out, evaluate_day, validate_edit, AttendanceEventKind,
    ClockInDecision, ClockOutDecision, DayEvaluation, DayInput, DayStatus, MonthlySummary,
};
use pango_core::authz;
use pango_core::calendar::{parse_date, parse_month, DateRange, WorkCalendar};
use pango_core::error::{ConflictReason, ValidationReason};
use pango_core::id::{AttendanceEventId, AttendanceRecordId, MemberId};
use pango_core::leave::LeaveUnit;
use pango_core::member::MemberStatus;
use pango_core::text::{self, Comment, Reason, TextRule};
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::attendance::{
    self, AttendanceEvent, AttendanceRecord, RecordCursor, RecordFilter, RecordWithMember,
};
use pango_db::members::{self, Member, MemberFilter};
use pango_db::{org, settings, Conn};

use super::support::{
    audit, department_tree, found, push_csv_row, require_updated, Audit, AuthContext, Company,
    Idempotency,
};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 메모는 짧은 설명(`Comment`) 규칙을 따르되, 오류에는 `note` 필드로 알린다.
const NOTE_RULE: TextRule = TextRule {
    field: "note",
    ..Comment::RULE
};

const CSV_HEADER: [&str; 9] = [
    "날짜",
    "이름",
    "아이디",
    "부서",
    "근무 형태",
    "출근",
    "퇴근",
    "근무 분",
    "상태",
];

/// 휴가 기능과 연결되는 지점: 구성원이 기간 안의 어느 날 어떤 단위의 휴가를 쓰는지.
///
/// 근태 판정(지각·조퇴 기준 조정, 종일 휴가의 `on_leave`)은 **모두** 이 함수를 거친다.
/// 승인된 휴가만 반영한다.
async fn leave_units_by_date(
    conn: &mut Conn,
    member_id: MemberId,
    range: DateRange,
) -> ServiceResult<BTreeMap<NaiveDate, LeaveUnit>> {
    super::leave::units_by_date(conn, member_id, range).await
}

fn record_dto(record: &AttendanceRecord) -> AttendanceRecordDto {
    AttendanceRecordDto {
        id: record.id,
        member_id: record.member_id,
        work_date: record.work_date,
        clock_in_at: record.clock_in_at,
        clock_out_at: record.clock_out_at,
        work_type: record.work_type,
        note: record.note.clone(),
        version: record.version,
    }
}

fn parse_note(raw: Option<&str>) -> ServiceResult<Option<String>> {
    Ok(text::optional(raw, |value| NOTE_RULE.normalize(value))?)
}

fn parse_range(from: &str, to: &str) -> ServiceResult<DateRange> {
    Ok(DateRange::new(
        parse_date("from", from)?,
        parse_date("to", to)?,
    )?)
}

/// 이력 한 줄을 남긴다. `before`/`after`는 API의 기록 모양 그대로 저장해 화면이 바로 보여 줄 수 있게 한다.
async fn append_event(
    conn: &mut Conn,
    now: Timestamp,
    kind: AttendanceEventKind,
    before: Option<&AttendanceRecord>,
    after: &AttendanceRecord,
    reason: Option<&Reason>,
    actor_id: MemberId,
) -> ServiceResult<()> {
    let snapshot = |record: &AttendanceRecord| {
        serde_json::to_value(record_dto(record)).map_err(ServiceError::internal)
    };
    let event = AttendanceEvent {
        id: AttendanceEventId::new(),
        record_id: after.id,
        member_id: after.member_id,
        kind,
        before: before.map(snapshot).transpose()?,
        after: snapshot(after)?,
        reason: reason.map(|r| r.as_str().to_owned()),
        actor_id,
        created_at: now,
    };
    Ok(attendance::insert_event(conn, &event).await?)
}

/// 하루 판정에 필요한 정책·달력·현재 시각.
struct Evaluator {
    company: Company,
    calendar: WorkCalendar,
    now: Timestamp,
}

impl Evaluator {
    async fn new(
        conn: &mut Conn,
        company: Company,
        range: DateRange,
        now: Timestamp,
    ) -> ServiceResult<Self> {
        let calendar = company.calendar(conn, range).await?;
        Ok(Self {
            company,
            calendar,
            now,
        })
    }

    fn evaluate(
        &self,
        date: NaiveDate,
        record: Option<&AttendanceRecord>,
        leave: Option<LeaveUnit>,
    ) -> DayEvaluation {
        let state = record.map(AttendanceRecord::state);
        let input = DayInput {
            work_date: date,
            is_working_day: self.calendar.is_working_day(date),
            record: state.as_ref(),
            leave,
            now: self.now,
        };
        evaluate_day(&input, &self.company.policy, &self.company.zone)
    }

    fn local_minute(&self, at: Timestamp) -> String {
        self.company
            .zone
            .local(at)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    }
}

/// 여러 구성원의 휴가를 한 번씩만 읽어 둔다.
async fn leaves_for(
    conn: &mut Conn,
    member_ids: Vec<MemberId>,
    range: DateRange,
) -> ServiceResult<HashMap<MemberId, BTreeMap<NaiveDate, LeaveUnit>>> {
    let mut leaves = HashMap::new();
    for member_id in member_ids {
        if let Entry::Vacant(slot) = leaves.entry(member_id) {
            slot.insert(leave_units_by_date(conn, member_id, range).await?);
        }
    }
    Ok(leaves)
}

fn member_ids_of(rows: &[RecordWithMember]) -> Vec<MemberId> {
    rows.iter().map(|r| r.record.member_id).collect()
}

fn leave_on(
    leaves: &HashMap<MemberId, BTreeMap<NaiveDate, LeaveUnit>>,
    member_id: MemberId,
    date: NaiveDate,
) -> Option<LeaveUnit> {
    leaves.get(&member_id)?.get(&date).copied()
}

async fn holiday_names(
    conn: &mut Conn,
    range: DateRange,
) -> ServiceResult<BTreeMap<NaiveDate, String>> {
    Ok(settings::list_holidays(conn, range)
        .await?
        .into_iter()
        .map(|h| (h.date, h.name))
        .collect())
}

// ── 본인 ────────────────────────────────────────────────────────────────

/// 홈의 출퇴근 버튼 상태.
///
/// 오늘 근무일의 기록이 없어도, 전날 시작한 야간 근무가 아직 퇴근으로 닫을 수 있으면 그 기록을
/// 보여 준다(그래야 06:00에 "퇴근" 버튼이 나온다). 판정은 퇴근 규칙(`decide_clock_out`)을 그대로 쓴다.
pub async fn today(state: &AppState, ctx: &AuthContext) -> ServiceResult<TodayResponse> {
    let now = state.now();
    let mut conn = state.read().await?;
    let company = Company::load(&mut conn).await?;
    let work_date = company.work_date(now);
    let latest = attendance::latest_for_member(&mut conn, ctx.member_id()).await?;
    let latest_state = latest.as_ref().map(AttendanceRecord::state);
    let closable = matches!(
        decide_clock_out(latest_state.as_ref(), now, &company.zone),
        Ok(ClockOutDecision::Close { .. })
    );
    let record = latest.filter(|r| r.work_date == work_date || closable);
    let evaluated_date = record.as_ref().map_or(work_date, |r| r.work_date);

    let range = DateRange::new(evaluated_date.min(work_date), work_date)?;
    let leaves = leave_units_by_date(&mut conn, ctx.member_id(), range).await?;
    let holiday_name = holiday_names(&mut conn, DateRange::single(work_date))
        .await?
        .remove(&work_date);
    let (expected_start, expected_end) = (company.policy.work_start(), company.policy.work_end());
    let evaluator = Evaluator::new(&mut conn, company, range, now).await?;
    Ok(TodayResponse {
        work_date,
        is_working_day: evaluator.calendar.is_working_day(work_date),
        holiday_name,
        evaluation: evaluator.evaluate(
            evaluated_date,
            record.as_ref(),
            leaves.get(&evaluated_date).copied(),
        ),
        record: record.as_ref().map(record_dto),
        expected_start,
        expected_end,
    })
}

/// 출근. 이미 오늘 출근했으면 그 기록을 그대로 돌려준다(이력 없음).
///
/// 같은 근무일 기록은 DB 유일 제약으로도 하나뿐이다. 다른 경로로 먼저 생긴 기록과 부딪히면
/// 500이 아니라 그 기록을 돌려준다.
pub async fn clock_in(
    state: &AppState,
    ctx: &AuthContext,
    request: &ClockInRequest,
) -> ServiceResult<AttendanceRecordDto> {
    ctx.member.status.require_active()?;
    let note = parse_note(request.note.as_deref())?;
    let now = state.now();
    let member_id = ctx.member_id();
    state
        .write(async |conn: &mut Conn| {
            let company = Company::load(conn).await?;
            let latest = attendance::latest_for_member(conn, member_id).await?;
            let latest_state = latest.as_ref().map(AttendanceRecord::state);
            let (work_date, clock_in_at) =
                match decide_clock_in(latest_state.as_ref(), now, &company.zone)? {
                    ClockInDecision::AlreadyClockedIn(_) => return Ok(record_dto(&found(latest)?)),
                    ClockInDecision::Create {
                        work_date,
                        clock_in_at,
                    } => (work_date, clock_in_at),
                };
            let record = AttendanceRecord {
                id: AttendanceRecordId::new(),
                member_id,
                work_date,
                clock_in_at,
                clock_out_at: None,
                work_type: request.work_type,
                note,
                version: 1,
            };
            match attendance::insert(conn, &record, now).await {
                Ok(()) => {}
                Err(error) if attendance::is_duplicate_work_date(&error) => {
                    let existing =
                        attendance::find_by_member_date(conn, member_id, work_date).await?;
                    return Ok(record_dto(&found(existing)?));
                }
                Err(error) => return Err(error.into()),
            }
            append_event(
                conn,
                now,
                AttendanceEventKind::ClockIn,
                None,
                &record,
                None,
                member_id,
            )
            .await?;
            Ok(record_dto(&record))
        })
        .await
}

/// 퇴근. 이미 퇴근했으면 그 기록을 그대로 돌려준다(퇴근 시각을 덮어쓰지 않는다).
pub async fn clock_out(state: &AppState, ctx: &AuthContext) -> ServiceResult<AttendanceRecordDto> {
    ctx.member.status.require_active()?;
    let now = state.now();
    let member_id = ctx.member_id();
    state
        .write(async |conn: &mut Conn| {
            let company = Company::load(conn).await?;
            let latest = attendance::latest_for_member(conn, member_id).await?;
            let latest_state = latest.as_ref().map(AttendanceRecord::state);
            let decision = decide_clock_out(latest_state.as_ref(), now, &company.zone)?;
            let record = found(latest)?;
            let ClockOutDecision::Close { clock_out_at, .. } = decision else {
                return Ok(record_dto(&record));
            };
            require_updated(attendance::close(conn, record.id, clock_out_at, now).await?)?;
            let closed = AttendanceRecord {
                clock_out_at: Some(clock_out_at),
                version: record.version + 1,
                ..record.clone()
            };
            append_event(
                conn,
                now,
                AttendanceEventKind::ClockOut,
                Some(&record),
                &closed,
                None,
                member_id,
            )
            .await?;
            Ok(record_dto(&closed))
        })
        .await
}

// ── 조회 ────────────────────────────────────────────────────────────────

/// 개인 기록을 볼 대상 구성원. 생략하면 본인. 볼 수 없는 구성원은 없는 구성원과 같이 404.
async fn viewable_member(
    conn: &mut Conn,
    ctx: &AuthContext,
    member_id: Option<MemberId>,
) -> ServiceResult<Member> {
    let member = match member_id {
        None => ctx.member.clone(),
        Some(id) if id == ctx.member_id() => ctx.member.clone(),
        Some(id) => found(members::find(conn, id).await?)?,
    };
    authz::view_personal_records(&ctx.actor, &member.subject())?;
    Ok(member)
}

/// 한 달의 날짜별 기록과 판정.
async fn month_days(
    conn: &mut Conn,
    member_id: MemberId,
    range: DateRange,
    now: Timestamp,
) -> ServiceResult<Vec<DayEntry>> {
    let records: BTreeMap<_, _> = attendance::list_for_members(conn, &[member_id], range)
        .await?
        .into_iter()
        .map(|r| (r.work_date, r))
        .collect();
    let leaves = leave_units_by_date(conn, member_id, range).await?;
    let mut holidays = holiday_names(conn, range).await?;
    let company = Company::load(conn).await?;
    let evaluator = Evaluator::new(conn, company, range, now).await?;
    Ok(range
        .days()
        .map(|date| {
            let record = records.get(&date);
            DayEntry {
                date,
                is_working_day: evaluator.calendar.is_working_day(date),
                holiday_name: holidays.remove(&date),
                record: record.map(record_dto),
                evaluation: evaluator.evaluate(date, record, leaves.get(&date).copied()),
            }
        })
        .collect())
}

pub async fn month_records(
    state: &AppState,
    ctx: &AuthContext,
    query: &MonthQuery,
) -> ServiceResult<MonthRecords> {
    let mut conn = state.read().await?;
    let member = viewable_member(&mut conn, ctx, query.member_id).await?;
    let range = parse_month(&query.month)?;
    let days = month_days(&mut conn, member.id, range, state.now()).await?;
    Ok(MonthRecords {
        member_id: member.id,
        month: query.month.clone(),
        days,
    })
}

pub async fn month_summary(
    state: &AppState,
    ctx: &AuthContext,
    query: &MonthQuery,
) -> ServiceResult<MonthlySummaryDto> {
    let mut conn = state.read().await?;
    let member = viewable_member(&mut conn, ctx, query.member_id).await?;
    let range = parse_month(&query.month)?;
    let days = month_days(&mut conn, member.id, range, state.now()).await?;
    let evaluations: Vec<_> = days.iter().map(|d| d.evaluation).collect();
    Ok(MonthlySummaryDto {
        member_id: member.id,
        month: query.month.clone(),
        summary: MonthlySummary::from_days(&evaluations),
    })
}

/// 기록 하나의 변경 이력. 기록을 볼 수 있는 사람만 볼 수 있고, 그 밖에는 없는 기록과 같이 404.
pub async fn record_events(
    state: &AppState,
    ctx: &AuthContext,
    record_id: AttendanceRecordId,
) -> ServiceResult<Vec<AttendanceEventDto>> {
    let mut conn = state.read().await?;
    let record = found(attendance::find(&mut conn, record_id).await?)?;
    viewable_member(&mut conn, ctx, Some(record.member_id)).await?;
    let events = attendance::list_events(&mut conn, record_id).await?;
    let mut actor_ids: Vec<_> = events.iter().map(|e| e.actor_id).collect();
    actor_ids.sort();
    actor_ids.dedup();
    let names: HashMap<_, _> = members::find_many(&mut conn, &actor_ids)
        .await?
        .into_iter()
        .map(|m| (m.id, m.name))
        .collect();
    Ok(events
        .into_iter()
        .map(|e| AttendanceEventDto {
            id: e.id,
            record_id: e.record_id,
            kind: e.kind,
            actor_name: names.get(&e.actor_id).cloned(),
            before: e.before,
            after: e.after,
            reason: e.reason,
            actor_id: e.actor_id,
            created_at: e.created_at,
        })
        .collect())
}

/// 조직장(관할 부서)·관리자(전체)의 하루 출근 현황. 재직 중인 구성원만.
pub async fn team(
    state: &AppState,
    ctx: &AuthContext,
    query: &TeamAttendanceQuery,
) -> ServiceResult<TeamAttendance> {
    authz::require_manager(&ctx.actor)?;
    let now = state.now();
    let mut conn = state.read().await?;
    let company = Company::load(&mut conn).await?;
    let date = match query.date.as_deref() {
        Some(raw) => parse_date("date", raw)?,
        None => company.work_date(now),
    };
    let range = DateRange::single(date);
    let filter = MemberFilter {
        statuses: vec![MemberStatus::Active],
        ..MemberFilter::default()
    };
    // 볼 수 있는 범위는 개인 기록 조회 규칙과 같다(관리자 전체, 조직장 관할 부서).
    let visible: Vec<Member> = members::list(&mut conn, &filter)
        .await?
        .into_iter()
        .filter(|m| authz::view_personal_records(&ctx.actor, &m.subject()).is_ok())
        .collect();
    let ids: Vec<_> = visible.iter().map(|m| m.id).collect();
    let records: HashMap<_, _> = attendance::list_for_members(&mut conn, &ids, range)
        .await?
        .into_iter()
        .map(|r| (r.member_id, r))
        .collect();
    let leaves = leaves_for(&mut conn, ids.clone(), range).await?;
    let departments: HashMap<_, _> = org::list_departments(&mut conn)
        .await?
        .into_iter()
        .map(|d| (d.id, d.name))
        .collect();
    let evaluator = Evaluator::new(&mut conn, company, range, now).await?;
    let members = visible
        .into_iter()
        .map(|m| {
            let record = records.get(&m.id);
            TeamAttendanceEntry {
                evaluation: evaluator.evaluate(date, record, leave_on(&leaves, m.id, date)),
                record: record.map(record_dto),
                member_id: m.id,
                department_name: m.department_id.and_then(|d| departments.get(&d).cloned()),
                department_id: m.department_id,
                name: m.name,
            }
        })
        .collect();
    Ok(TeamAttendance {
        date,
        is_working_day: evaluator.calendar.is_working_day(date),
        members,
    })
}

// ── 관리자 ──────────────────────────────────────────────────────────────

fn encode_cursor(cursor: RecordCursor) -> String {
    format!("{}~{}", cursor.work_date.format("%Y-%m-%d"), cursor.id)
}

fn decode_cursor(raw: &str) -> ServiceResult<RecordCursor> {
    let invalid = || DomainError::validation("cursor", ValidationReason::InvalidFormat);
    let (date, id) = raw.split_once('~').ok_or_else(invalid)?;
    Ok(RecordCursor {
        work_date: parse_date("cursor", date)?,
        id: id.parse().map_err(|_| invalid())?,
    })
}

async fn admin_filter(
    conn: &mut Conn,
    range: DateRange,
    query: &AdminAttendanceQuery,
) -> ServiceResult<RecordFilter> {
    let departments = match query.department_id {
        Some(id) => Some(
            department_tree(conn)
                .await?
                .descendants_inclusive(id)
                .into_iter()
                .collect(),
        ),
        None => None,
    };
    Ok(RecordFilter {
        range,
        member_id: query.member_id,
        departments,
    })
}

pub async fn admin_list(
    state: &AppState,
    ctx: &AuthContext,
    query: &AdminAttendanceQuery,
) -> ServiceResult<Page<AdminAttendanceItem>> {
    authz::require_admin(&ctx.actor)?;
    let range = parse_range(&query.from, &query.to)?;
    let after = query.cursor.as_deref().map(decode_cursor).transpose()?;
    let limit = PageQuery {
        limit: query.limit,
        cursor: None,
    }
    .limit();
    let now = state.now();
    let mut conn = state.read().await?;
    let filter = admin_filter(&mut conn, range, query).await?;
    let (rows, has_more) = attendance::list_page(&mut conn, &filter, after, limit).await?;
    let leaves = leaves_for(&mut conn, member_ids_of(&rows), range).await?;
    let company = Company::load(&mut conn).await?;
    let evaluator = Evaluator::new(&mut conn, company, range, now).await?;
    let next_cursor = rows.last().filter(|_| has_more).map(|last| {
        encode_cursor(RecordCursor {
            work_date: last.record.work_date,
            id: last.record.id,
        })
    });
    let items = rows
        .into_iter()
        .map(|row| {
            let record = &row.record;
            AdminAttendanceItem {
                evaluation: evaluator.evaluate(
                    record.work_date,
                    Some(record),
                    leave_on(&leaves, record.member_id, record.work_date),
                ),
                record: record_dto(record),
                member_name: row.member_name,
                login_id: row.login_id,
                department_name: row.department_name,
            }
        })
        .collect();
    Ok(Page { items, next_cursor })
}

/// 관리자가 기록을 새로 만든다(누락된 날 등). 같은 구성원·근무일 기록이 있으면 409.
pub async fn admin_create(
    state: &AppState,
    ctx: &AuthContext,
    request: &AdminCreateRecordRequest,
    idempotency: Option<&Idempotency>,
) -> ServiceResult<AttendanceRecordDto> {
    authz::require_admin(&ctx.actor)?;
    let work_date = parse_date("work_date", &request.work_date)?;
    let note = parse_note(request.note.as_deref())?;
    let reason = Reason::parse(&request.reason)?;
    let now = state.now();
    state
        .write_idempotent(ctx.member_id(), idempotency, async |conn: &mut Conn| {
            let member = found(members::find(conn, request.member_id).await?)?;
            authz::edit_personal_records(&ctx.actor, &member.subject())?;
            let company = Company::load(conn).await?;
            validate_edit(
                request.clock_in_at,
                request.clock_out_at,
                work_date,
                now,
                &company.zone,
            )?;
            let record = AttendanceRecord {
                id: AttendanceRecordId::new(),
                member_id: member.id,
                work_date,
                clock_in_at: request.clock_in_at,
                clock_out_at: request.clock_out_at,
                work_type: request.work_type,
                note,
                version: 1,
            };
            attendance::insert(conn, &record, now)
                .await
                .map_err(|error| {
                    if attendance::is_duplicate_work_date(&error) {
                        DomainError::Conflict(ConflictReason::AlreadyExists).into()
                    } else {
                        ServiceError::from(error)
                    }
                })?;
            append_event(
                conn,
                now,
                AttendanceEventKind::AdminCreate,
                None,
                &record,
                Some(&reason),
                ctx.member_id(),
            )
            .await?;
            audit(
                conn,
                now,
                Audit {
                    action: "attendance.create",
                    target_type: "attendance_record",
                    target_id: Some(record.id.to_string()),
                    detail: Some(serde_json::json!({
                        "member_id": member.id,
                        "work_date": record.work_date,
                        "reason": reason.as_str(),
                    })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            Ok(record_dto(&record))
        })
        .await
}

/// 관리자 수정. 사유 필수, 읽었던 `version`이 다르면 409. 이전·이후 값을 이력에 남긴다.
pub async fn admin_edit(
    state: &AppState,
    ctx: &AuthContext,
    record_id: AttendanceRecordId,
    request: &AdminEditRecordRequest,
) -> ServiceResult<AttendanceRecordDto> {
    authz::require_admin(&ctx.actor)?;
    let reason = Reason::parse(&request.reason)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let record = found(attendance::find(conn, record_id).await?)?;
            let member = found(members::find(conn, record.member_id).await?)?;
            authz::edit_personal_records(&ctx.actor, &member.subject())?;
            let company = Company::load(conn).await?;
            validate_edit(
                request.clock_in_at,
                request.clock_out_at,
                record.work_date,
                now,
                &company.zone,
            )?;
            let edited = AttendanceRecord {
                clock_in_at: request.clock_in_at,
                clock_out_at: request.clock_out_at,
                work_type: request.work_type,
                version: request.version,
                ..record.clone()
            };
            require_updated(attendance::update(conn, &edited, now).await?)?;
            let after = AttendanceRecord {
                version: request.version + 1,
                ..edited
            };
            append_event(
                conn,
                now,
                AttendanceEventKind::AdminEdit,
                Some(&record),
                &after,
                Some(&reason),
                ctx.member_id(),
            )
            .await?;
            audit(
                conn,
                now,
                Audit {
                    action: "attendance.edit",
                    target_type: "attendance_record",
                    target_id: Some(record.id.to_string()),
                    detail: Some(serde_json::json!({
                        "member_id": member.id,
                        "work_date": record.work_date,
                        "reason": reason.as_str(),
                    })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            Ok(record_dto(&after))
        })
        .await
}

/// 내보낸 CSV 파일.
pub struct CsvFile {
    pub filename: String,
    pub body: String,
}

fn status_code(status: DayStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn csv_line(
    out: &mut String,
    evaluator: &Evaluator,
    row: &RecordWithMember,
    leave: Option<LeaveUnit>,
) {
    let record = &row.record;
    let evaluation = evaluator.evaluate(record.work_date, Some(record), leave);
    push_csv_row(
        out,
        [
            record.work_date.format("%Y-%m-%d").to_string(),
            row.member_name.clone(),
            row.login_id.clone(),
            row.department_name.clone().unwrap_or_default(),
            record.work_type.as_str().to_owned(),
            evaluator.local_minute(record.clock_in_at),
            record
                .clock_out_at
                .map(|at| evaluator.local_minute(at))
                .unwrap_or_default(),
            evaluation.worked_minutes.to_string(),
            status_code(evaluation.status),
        ],
    );
}

/// 기간의 모든 기록을 CSV로 내보낸다. 엑셀이 한글을 깨뜨리지 않도록 UTF-8 BOM을 붙인다.
///
/// 개인 기록을 대량으로 꺼내는 동작이므로 감사 로그를 남긴다(그래서 읽기지만 쓰기 트랜잭션이다).
pub async fn export_csv(
    state: &AppState,
    ctx: &AuthContext,
    query: &ExportQuery,
) -> ServiceResult<CsvFile> {
    authz::require_admin(&ctx.actor)?;
    let range = parse_range(&query.from, &query.to)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let filter = RecordFilter {
                range,
                member_id: None,
                departments: None,
            };
            let rows = attendance::list_for_export(conn, &filter).await?;
            let leaves = leaves_for(conn, member_ids_of(&rows), range).await?;
            let company = Company::load(conn).await?;
            let evaluator = Evaluator::new(conn, company, range, now).await?;
            let mut body = String::from("\u{feff}");
            push_csv_row(&mut body, CSV_HEADER);
            for row in &rows {
                let leave = leave_on(&leaves, row.record.member_id, row.record.work_date);
                csv_line(&mut body, &evaluator, row, leave);
            }
            let (from, to) = (
                range.start().format("%Y-%m-%d"),
                range.end().format("%Y-%m-%d"),
            );
            audit(
                conn,
                now,
                Audit {
                    action: "attendance.export",
                    target_type: "attendance_record",
                    target_id: None,
                    detail: Some(serde_json::json!({
                        "from": from.to_string(),
                        "to": to.to_string(),
                        "rows": rows.len(),
                    })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            Ok(CsvFile {
                filename: format!("attendance-{from}-{to}.csv"),
                body,
            })
        })
        .await
}
