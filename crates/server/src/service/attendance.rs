//! 출퇴근: 출근·퇴근, 월별 기록·요약, 변경 이력, 팀 현황, 정정 요청, 관리자 생성·수정·내보내기.
//!
//! 판정 규칙(출근·퇴근 전이, 하루 근태, 수정 값 검증)은 모두 `pango_core::attendance`에 있다.
//! 이 모듈은 기록을 읽어 규칙에 넘기고, 결과를 저장하고, 이력·감사 로그를 같은 트랜잭션에 남긴다.
//!
//! 구성원은 자기 기록을 직접 고칠 수 없다. 고칠 값을 적은 **정정 요청**을 올리면 그것이 결재
//! 문서가 되고, 문서가 종료될 때 결재가 [`on_correction_document_closed_tx`]를 불러 기록을
//! 고치거나(최종 승인) 요청을 끝낸다(반려·회수). 휴가 신청과 같은 구조다.

use std::collections::{BTreeMap, HashMap};

use chrono::{Days, NaiveDate};
use pango_api::attendance::{
    AdminAttendanceItem, AdminAttendanceQuery, AdminCreateRecordRequest, AdminEditRecordRequest,
    AttendanceEventDto, AttendanceRecordDto, ClockInRequest, CorrectionDto, CorrectionQuery,
    CreateCorrectionRequest, DayEntry, ExportQuery, MonthQuery, MonthRecords, MonthlySummaryDto,
    TeamAttendance, TeamAttendanceEntry, TeamAttendanceQuery, TodayResponse,
};
use pango_api::home::MissingClockOut;
use pango_api::Page;
use pango_core::approval::{ApprovalLine, DocumentKind};
use pango_core::attendance::{
    decide_clock_in, decide_clock_out, decide_correction_close, evaluate_day,
    validate_correction_date, validate_edit, AttendanceEventKind, ClockInDecision,
    ClockOutDecision, CorrectionDecision, CorrectionStatus, DayEvaluation, DayInput, DayStatus,
    MonthlySummary, TrackedPeriod,
};
use pango_core::authz::{self, Actor};
use pango_core::calendar::{parse_date, parse_month, DateRange, WorkCalendar};
use pango_core::error::{ConflictReason, ValidationReason};
use pango_core::id::{
    AttendanceCorrectionId, AttendanceEventId, AttendanceRecordId, DocumentId, MemberId,
};
use pango_core::leave::DayLeave;
use pango_core::text::{self, Body, Comment, Reason, TextRule, Title};
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::attendance::{
    self, AttendanceEvent, AttendanceRecord, RecordCursor, RecordFilter, RecordWithMember,
};
use pango_db::attendance_corrections::{self as corrections, AttendanceCorrection};
use pango_db::members::{self, Member, MemberFilter};
use pango_db::{settings, Conn};

use super::approval::{self, LinkedOutcome, SystemDocument};
use super::support::{
    audit, audit_as, current_member, department_names, department_subtree, found, member_names,
    members_by_id, next_page, notify, personal_records_member, push_csv_row, require_current_admin,
    require_updated, Audit, AuthContext, Company, Idempotency, Notify,
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
    "이메일",
    "부서",
    "근무 형태",
    "출근",
    "퇴근",
    "근무 분",
    "상태",
];

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

    /// 구성원의 근태 추적 기간. 계정 생성일은 회사 시간대의 달력 날짜다.
    fn tracked_period(&self, member: &Member) -> TrackedPeriod {
        TrackedPeriod::new(
            member.hired_on,
            self.company.zone.calendar_date(member.created_at),
            member.retired_on,
        )
    }

    /// `member`의 `date` 하루를 판정한다. 추적 기간 밖의 날은 그 구성원에게 근무일이 아니다.
    fn evaluate(
        &self,
        member: &Member,
        date: NaiveDate,
        record: Option<&AttendanceRecord>,
        leave: Option<DayLeave>,
    ) -> DayEvaluation {
        let state = record.map(AttendanceRecord::state);
        let is_working_day = self
            .tracked_period(member)
            .working_day(date, self.calendar.is_working_day(date));
        let input = DayInput {
            work_date: date,
            is_working_day,
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

/// 여러 구성원의 휴가를 구성원 수와 무관한 횟수의 조회로 읽어 둔다.
///
/// 기간 안의 승인된 휴가를 전부 한 번에 읽어 `member_ids`의 것만 남긴다. 구성원마다
/// [`super::leave::units_by_date`]를 부른 것과 결과가 같다(같은 규칙을 한 번에 적용할 뿐이다).
async fn leaves_for(
    conn: &mut Conn,
    member_ids: Vec<MemberId>,
    range: DateRange,
) -> ServiceResult<HashMap<MemberId, BTreeMap<NaiveDate, DayLeave>>> {
    let mut leaves: HashMap<MemberId, BTreeMap<NaiveDate, DayLeave>> = member_ids
        .into_iter()
        .map(|member_id| (member_id, BTreeMap::new()))
        .collect();
    if leaves.is_empty() {
        return Ok(leaves);
    }
    for ((member_id, date), unit) in super::leave::units_in_range(conn, range).await? {
        if let Some(days) = leaves.get_mut(&member_id) {
            days.insert(date, unit);
        }
    }
    Ok(leaves)
}

fn member_ids_of(rows: &[RecordWithMember]) -> Vec<MemberId> {
    rows.iter().map(|r| r.record.member_id).collect()
}

/// 기록의 주인. 기록은 외래 키로 구성원에 묶여 있으므로 없으면 내부 오류다.
fn owner_of(owners: &BTreeMap<MemberId, Member>, member_id: MemberId) -> ServiceResult<&Member> {
    owners
        .get(&member_id)
        .ok_or_else(|| ServiceError::internal("attendance record has no member"))
}

fn leave_on(
    leaves: &HashMap<MemberId, BTreeMap<NaiveDate, DayLeave>>,
    member_id: MemberId,
    date: NaiveDate,
) -> Option<DayLeave> {
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
    today_tx(&mut *state.read().await?, &ctx.member, state.now()).await
}

/// [`today`]의 본체. 홈 화면이 다른 요약과 한 연결에서 함께 읽을 때도 이것을 쓴다.
pub async fn today_tx(
    conn: &mut Conn,
    member: &Member,
    now: Timestamp,
) -> ServiceResult<TodayResponse> {
    let member_id = member.id;
    let company = Company::load(conn).await?;
    let work_date = company.work_date(now);
    let latest = attendance::latest_for_member(conn, member_id).await?;
    let latest_state = latest.as_ref().map(AttendanceRecord::state);
    let closable = matches!(
        decide_clock_out(latest_state.as_ref(), now, &company.zone),
        Ok(ClockOutDecision::Close { .. })
    );
    let record = latest.filter(|r| r.work_date == work_date || closable);
    let evaluated_date = record.as_ref().map_or(work_date, |r| r.work_date);

    let range = DateRange::new(evaluated_date.min(work_date), work_date)?;
    let leaves = super::leave::units_by_date(conn, member_id, range).await?;
    let holiday_name = holiday_names(conn, DateRange::single(work_date))
        .await?
        .remove(&work_date);
    let (expected_start, expected_end) = (company.policy.work_start(), company.policy.work_end());
    let evaluator = Evaluator::new(conn, company, range, now).await?;
    Ok(TodayResponse {
        work_date,
        is_working_day: evaluator.calendar.is_working_day(work_date),
        holiday_name,
        evaluation: evaluator.evaluate(
            member,
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

/// 한 달의 날짜별 기록과 판정.
async fn month_days(
    conn: &mut Conn,
    member: &Member,
    range: DateRange,
    now: Timestamp,
) -> ServiceResult<Vec<DayEntry>> {
    let member_id = member.id;
    let records: BTreeMap<_, _> = attendance::list_for_members(conn, &[member_id], range)
        .await?
        .into_iter()
        .map(|r| (r.work_date, r))
        .collect();
    let leaves = super::leave::units_by_date(conn, member_id, range).await?;
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
                evaluation: evaluator.evaluate(member, date, record, leaves.get(&date).copied()),
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
    let member = personal_records_member(&mut conn, ctx, query.member_id).await?;
    let range = parse_month(&query.month)?;
    let days = month_days(&mut conn, &member, range, state.now()).await?;
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
    let member = personal_records_member(&mut conn, ctx, query.member_id).await?;
    let range = parse_month(&query.month)?;
    let days = month_days(&mut conn, &member, range, state.now()).await?;
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
    personal_records_member(&mut conn, ctx, Some(record.member_id)).await?;
    let events = attendance::list_events(&mut conn, record_id).await?;
    let names = member_names(&mut conn, events.iter().map(|e| e.actor_id)).await?;
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
    let visible = visible_active_members_tx(&mut conn, &ctx.actor).await?;
    let ids: Vec<_> = visible.iter().map(|m| m.id).collect();
    let records: HashMap<_, _> = attendance::list_for_members(&mut conn, &ids, range)
        .await?
        .into_iter()
        .map(|r| (r.member_id, r))
        .collect();
    let leaves = leaves_for(&mut conn, ids.clone(), range).await?;
    let departments = department_names(&mut conn).await?;
    let evaluator = Evaluator::new(&mut conn, company, range, now).await?;
    let members = visible
        .into_iter()
        .map(|m| {
            let record = records.get(&m.id);
            TeamAttendanceEntry {
                evaluation: evaluator.evaluate(&m, date, record, leave_on(&leaves, m.id, date)),
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

/// 행위자가 개인 기록을 볼 수 있는 재직 구성원: 관리자는 전체, 조직장은 관할 부서.
/// 팀 현황과 홈·대시보드의 인원 집계가 같은 범위를 쓴다.
pub async fn visible_active_members_tx(
    conn: &mut Conn,
    actor: &Actor,
) -> ServiceResult<Vec<Member>> {
    Ok(members::list(conn, &MemberFilter::active())
        .await?
        .into_iter()
        .filter(|m| authz::view_personal_records(actor, &m.subject()).is_ok())
        .collect())
}

/// 구성원 집합의 오늘 근무일 현황.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Headcount {
    pub date: NaiveDate,
    pub is_working_day: bool,
    pub total: u32,
    /// 오늘 근무일의 출근 기록이 있는 사람.
    pub present: u32,
    /// 그중 지각(유예 초과).
    pub late: u32,
    /// 종일 휴가이고 출근 기록이 없는 사람.
    pub on_leave: u32,
    /// 근무일인데 출근 기록도 종일 휴가도 없는 사람. 근무일이 아니면 0.
    pub not_yet: u32,
}

/// `members`의 오늘 현황을 센다. 한 사람씩의 판정은 [`Evaluator`]로 다른 화면과 똑같이 하고,
/// 기록과 휴가는 구성원 수와 무관하게 한 번씩만 읽는다.
pub async fn headcount_tx(
    conn: &mut Conn,
    members: &[Member],
    now: Timestamp,
) -> ServiceResult<Headcount> {
    let company = Company::load(conn).await?;
    let date = company.work_date(now);
    let range = DateRange::single(date);
    let ids: Vec<_> = members.iter().map(|m| m.id).collect();
    let records: HashMap<_, _> = attendance::list_for_members(conn, &ids, range)
        .await?
        .into_iter()
        .map(|r| (r.member_id, r))
        .collect();
    let leaves = super::leave::units_on(conn, date).await?;
    let evaluator = Evaluator::new(conn, company, range, now).await?;
    let mut count = Headcount {
        date,
        is_working_day: evaluator.calendar.is_working_day(date),
        total: u32::try_from(members.len()).unwrap_or(u32::MAX),
        present: 0,
        late: 0,
        on_leave: 0,
        not_yet: 0,
    };
    for member in members {
        let record = records.get(&member.id);
        let evaluation = evaluator.evaluate(member, date, record, leaves.get(&member.id).copied());
        if record.is_some() {
            count.present += 1;
            if evaluation.late_minutes > 0 {
                count.late += 1;
            }
        } else {
            match evaluation.status {
                DayStatus::OnLeave => count.on_leave += 1,
                DayStatus::Scheduled | DayStatus::Absent => count.not_yet += 1,
                _ => {}
            }
        }
    }
    Ok(count)
}

/// 최근 `days`일 안에 퇴근을 찍지 않아 `missing_clock_out`이 된 구성원(최근 날짜 먼저,
/// 최대 `max_members`명). 관리 대시보드용.
pub async fn missing_clock_outs_tx(
    conn: &mut Conn,
    now: Timestamp,
    days: u64,
    max_members: usize,
) -> ServiceResult<Vec<MissingClockOut>> {
    let company = Company::load(conn).await?;
    let today = company.work_date(now);
    let start = today
        .checked_sub_days(Days::new(days))
        .ok_or_else(|| DomainError::validation("date", ValidationReason::OutOfRange))?;
    let range = DateRange::new(start, today)?;
    let open = attendance::list_open(conn, range).await?;
    let owners = members_by_id(conn, member_ids_of(&open)).await?;
    let evaluator = Evaluator::new(conn, company, range, now).await?;
    let mut missing: Vec<MissingClockOut> = Vec::new();
    for row in open {
        let record = &row.record;
        // 누락 여부는 출근 후 지난 시간만으로 정해지므로 휴가는 넘기지 않아도 된다.
        let owner = owner_of(&owners, record.member_id)?;
        let status = evaluator
            .evaluate(owner, record.work_date, Some(record), None)
            .status;
        if status != DayStatus::MissingClockOut {
            continue;
        }
        if let Some(existing) = missing.iter_mut().find(|m| m.member_id == record.member_id) {
            existing.dates.push(record.work_date);
        } else if missing.len() < max_members {
            missing.push(MissingClockOut {
                member_id: record.member_id,
                name: row.member_name,
                department_name: row.department_name,
                dates: vec![record.work_date],
            });
        }
    }
    Ok(missing)
}

// ── 정정 요청 ───────────────────────────────────────────────────────────

async fn correction_dtos(
    conn: &mut Conn,
    corrections: Vec<AttendanceCorrection>,
) -> ServiceResult<Vec<CorrectionDto>> {
    let names = member_names(conn, corrections.iter().map(|c| c.member_id)).await?;
    Ok(corrections
        .into_iter()
        .map(|c| CorrectionDto {
            id: c.id,
            member_id: c.member_id,
            member_name: names.get(&c.member_id).cloned().unwrap_or_default(),
            work_date: c.work_date,
            clock_in_at: c.clock_in_at,
            clock_out_at: c.clock_out_at,
            work_type: c.work_type,
            reason: c.reason,
            status: c.status,
            document_id: c.document_id,
            created_at: c.created_at,
            decided_at: c.decided_at,
        })
        .collect())
}

async fn correction_dto(
    conn: &mut Conn,
    id: AttendanceCorrectionId,
) -> ServiceResult<CorrectionDto> {
    let correction = found(corrections::find(conn, id).await?)?;
    let mut dtos = correction_dtos(conn, vec![correction]).await?;
    found(dtos.pop())
}

async fn audit_correction(
    conn: &mut Conn,
    now: Timestamp,
    ctx: &AuthContext,
    action: &'static str,
    correction: &AttendanceCorrection,
) -> ServiceResult<()> {
    audit(
        conn,
        now,
        Audit {
            action,
            target_type: "attendance_correction",
            target_id: Some(correction.id.to_string()),
            detail: Some(serde_json::json!({
                "member_id": correction.member_id,
                "work_date": correction.work_date,
                "document_id": correction.document_id,
            })),
            ctx: Some(ctx),
        },
    )
    .await
}

/// 정정 요청의 결재 문서(시스템 양식). 페이로드에는 요청 값과 요청 당시의 기록을 함께 싣는다.
fn correction_document(
    id: AttendanceCorrectionId,
    work_date: NaiveDate,
    request: &CreateCorrectionRequest,
    reason: &Reason,
    current: Option<&AttendanceRecord>,
    line: ApprovalLine,
) -> ServiceResult<SystemDocument> {
    Ok(SystemDocument {
        kind: DocumentKind::AttendanceCorrection,
        form_code: approval::ATTENDANCE_CORRECTION_FORM_CODE,
        title: Title::parse(&format!("근태 정정 요청 ({work_date})"))?,
        body: Body::parse(reason.as_str())?,
        payload: serde_json::json!({
            "request_id": id,
            "work_date": work_date,
            "clock_in_at": request.clock_in_at,
            "clock_out_at": request.clock_out_at,
            "work_type": request.work_type,
            "reason": reason.as_str(),
            "current": current.map(record_dto),
        }),
        line,
    })
}

/// 정정 요청을 저장한다. 같은 근무일의 대기 요청은 호출 전에 확인하지만, 동시 요청의 마지막
/// 방어선은 유일 인덱스다(위반하면 `AlreadyExists`).
async fn insert_correction_tx(
    conn: &mut Conn,
    correction: &AttendanceCorrection,
) -> ServiceResult<()> {
    corrections::insert(conn, correction)
        .await
        .map_err(|error| {
            if corrections::is_duplicate_pending(&error) {
                DomainError::Conflict(ConflictReason::AlreadyExists).into()
            } else {
                ServiceError::from(error)
            }
        })
}

/// 정정 요청. 한 트랜잭션에서 값 검증 → 중복 확인 → 결재 문서 상신 → 요청 저장까지 한다.
/// 어느 단계든 실패하면 아무것도 남지 않는다.
///
/// - 대상은 오늘 근무일까지, [`CORRECTION_WINDOW_DAYS`](pango_core::attendance::CORRECTION_WINDOW_DAYS)일
///   전까지다. 값은 관리자 수정과 같은 규칙([`validate_edit`])으로 본다.
/// - 같은 근무일에 결재 중인 요청은 하나뿐이다(`AlreadyExists`). 끝난 뒤에는 다시 올릴 수 있다.
/// - 요청 당시의 기록(없으면 `null`)을 문서에 싣고 그 `version`을 기억해 둔다. 결재자가 본
///   "현재 기록"과 승인 시점의 기록이 다르면 반영하지 않기 위해서다.
pub async fn create_correction(
    state: &AppState,
    ctx: &AuthContext,
    request: &CreateCorrectionRequest,
    idempotency: Option<&Idempotency>,
) -> ServiceResult<CorrectionDto> {
    ctx.member.status.require_active()?;
    let work_date = parse_date("work_date", &request.work_date)?;
    let reason = Reason::parse(&request.reason)?;
    let now = state.now();
    state
        .write_idempotent(ctx.member_id(), idempotency, async |conn: &mut Conn| {
            // 인증 뒤에 부서가 바뀌었을 수 있다. 최종 결재자 확인과 문서의 기안 부서는 지금의
            // 소속으로 한다.
            let member = &current_member(conn, ctx).await?;
            let company = Company::load(conn).await?;
            validate_correction_date(work_date, company.work_date(now))?;
            validate_edit(
                request.clock_in_at,
                request.clock_out_at,
                work_date,
                now,
                &company.zone,
            )?;
            if corrections::find_pending(conn, member.id, work_date)
                .await?
                .is_some()
            {
                return Err(DomainError::Conflict(ConflictReason::AlreadyExists).into());
            }
            let current = attendance::find_by_member_date(conn, member.id, work_date).await?;

            let id = AttendanceCorrectionId::new();
            let line = approval::request_line_tx(
                conn,
                member,
                request.steps.as_deref(),
                &request.references,
            )
            .await?;
            let document =
                correction_document(id, work_date, request, &reason, current.as_ref(), line)?;
            let document_id = approval::create_submitted_tx(conn, member, document, now).await?;

            let correction = AttendanceCorrection {
                id,
                member_id: member.id,
                work_date,
                clock_in_at: request.clock_in_at,
                clock_out_at: request.clock_out_at,
                work_type: request.work_type,
                reason: reason.into_string(),
                base_record_version: current.map(|record| record.version),
                status: CorrectionStatus::Pending,
                document_id,
                created_at: now,
                decided_at: None,
                version: 1,
            };
            insert_correction_tx(conn, &correction).await?;
            audit_correction(conn, now, ctx, "attendance.correction_request", &correction).await?;
            correction_dto(conn, id).await
        })
        .await
}

/// 정정 요청 목록(최신 접수순). 개인 기록과 같은 범위로 볼 수 있다: 본인, 관할 조직장, 관리자.
pub async fn list_corrections(
    state: &AppState,
    ctx: &AuthContext,
    query: &CorrectionQuery,
) -> ServiceResult<Page<CorrectionDto>> {
    let page = query.page();
    let before = page.after::<AttendanceCorrectionId>()?;
    let mut conn = state.read().await?;
    let member = personal_records_member(&mut conn, ctx, query.member_id).await?;
    let (items, has_more) =
        corrections::list_for_member(&mut conn, member.id, before, page.limit()).await?;
    let items = correction_dtos(&mut conn, items).await?;
    next_page(items, has_more, |last: &CorrectionDto| last.id)
}

/// 본인의 정정 요청 취소. 남의 요청은 `NotFound`다.
///
/// 취소는 곧 문서 **회수**다. 문서를 회수하면 그 연결 효과([`on_correction_document_closed_tx`])가
/// 요청을 `canceled`로 바꾸므로, 결재 화면에서 회수하든 여기서 취소하든 같은 경로, 같은 결과다.
/// 누군가 이미 결재했으면 `WithdrawTooLate`, 승인·반려로 끝났으면 `DocumentAlreadyClosed`,
/// 이미 취소됐으면 아무것도 하지 않고 성공으로 답한다.
pub async fn cancel_correction(
    state: &AppState,
    ctx: &AuthContext,
    id: AttendanceCorrectionId,
) -> ServiceResult<CorrectionDto> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let correction = found(corrections::find(conn, id).await?)?;
            authz::require_own(&ctx.actor, correction.member_id, DomainError::NotFound)?;
            let decision = decide_correction_close(correction.status, CorrectionStatus::Canceled)?;
            if decision == CorrectionDecision::Close {
                approval::withdraw_linked_tx(conn, ctx.member_id(), correction.document_id, now)
                    .await?;
                audit_correction(conn, now, ctx, "attendance.correction_cancel", &correction)
                    .await?;
            }
            correction_dto(conn, id).await
        })
        .await
}

/// 정정 요청 문서가 종료됐을 때 결재가 같은 트랜잭션에서 부르는 연결 효과.
///
/// - 최종 승인 → 기록을 고치고([`apply_correction_tx`]) `approved`.
/// - 반려 → `rejected`, 회수 → `canceled`. 기록은 건드리지 않는다.
/// - 같은 결과가 다시 오면 아무것도 하지 않는다.
///
/// 문서에 연결된 요청이 없거나 정정 값이 더는 유효하지 않으면 오류다. 그러면 결재 전이 전체가
/// 롤백되어, 문서만 승인되고 기록은 고쳐지지 않은 상태가 생기지 않는다. 승인이 그렇게 실패한
/// 문서는 결재자가 반려로 끝낼 수 있다.
pub async fn on_correction_document_closed_tx(
    conn: &mut Conn,
    document_id: DocumentId,
    outcome: LinkedOutcome,
    actor_id: MemberId,
    now: Timestamp,
) -> ServiceResult<()> {
    let correction = corrections::find_by_document(conn, document_id)
        .await?
        .ok_or_else(|| ServiceError::internal("correction document has no linked request"))?;
    let target = match outcome {
        LinkedOutcome::Completed => CorrectionStatus::Approved,
        LinkedOutcome::Rejected => CorrectionStatus::Rejected,
        LinkedOutcome::Withdrawn => CorrectionStatus::Canceled,
    };
    if decide_correction_close(correction.status, target)? == CorrectionDecision::AlreadyClosed {
        return Ok(());
    }
    if target == CorrectionStatus::Approved {
        apply_correction_tx(conn, &correction, actor_id, now).await?;
    }
    let updated =
        corrections::set_status(conn, correction.id, correction.version, target, now).await?;
    require_updated(updated)
}

/// 승인된 정정 값을 기록에 반영한다. `actor_id`는 최종 승인자다.
///
/// 요청 뒤로 시간이 흘렀으므로 두 가지를 다시 확인한다:
/// - 값이 지금도 규칙에 맞는지([`validate_edit`]). 그사이 회사 시간대나 근무일 전환 시각이
///   바뀌었으면 같은 출근 시각이 다른 근무일에 속할 수 있다.
/// - 기록이 요청 당시 그대로인지. 그사이 관리자가 고쳤거나 본인이 퇴근을 찍었다면, 결재자가
///   보고 승인한 "현재 기록"은 더는 사실이 아니고 그 위에 덮어쓰면 새 값이 사라진다.
///   이때는 `VersionConflict`이며, 결재자는 반려하고 요청자는 새 기록을 보고 다시 올린다.
async fn apply_correction_tx(
    conn: &mut Conn,
    correction: &AttendanceCorrection,
    actor_id: MemberId,
    now: Timestamp,
) -> ServiceResult<()> {
    let company = Company::load(conn).await?;
    validate_edit(
        correction.clock_in_at,
        correction.clock_out_at,
        correction.work_date,
        now,
        &company.zone,
    )?;
    let reason = Reason::parse(&correction.reason)?;
    let current =
        attendance::find_by_member_date(conn, correction.member_id, correction.work_date).await?;
    if current.as_ref().map(|record| record.version) != correction.base_record_version {
        return Err(DomainError::Conflict(ConflictReason::VersionConflict).into());
    }
    let after = write_corrected_record_tx(conn, correction, current.as_ref(), now).await?;
    append_event(
        conn,
        now,
        AttendanceEventKind::Correction,
        current.as_ref(),
        &after,
        Some(&reason),
        actor_id,
    )
    .await?;
    audit_as(
        conn,
        now,
        actor_id,
        Audit {
            action: "attendance.correction",
            target_type: "attendance_record",
            target_id: Some(after.id.to_string()),
            detail: Some(serde_json::json!({
                "member_id": correction.member_id,
                "work_date": correction.work_date,
                "correction_id": correction.id,
                "document_id": correction.document_id,
                "reason": reason.as_str(),
            })),
            ctx: None,
        },
    )
    .await?;
    notify_corrected_tx(conn, correction, now).await
}

/// 정정 값을 기록에 쓴다: 기록이 없던 날이면 새로 만들고, 있으면 그 기록을 고친다.
/// 쓴 뒤의 기록(이력의 "이후" 값)을 돌려준다.
async fn write_corrected_record_tx(
    conn: &mut Conn,
    correction: &AttendanceCorrection,
    current: Option<&AttendanceRecord>,
    now: Timestamp,
) -> ServiceResult<AttendanceRecord> {
    match current {
        None => {
            let record = AttendanceRecord {
                id: AttendanceRecordId::new(),
                member_id: correction.member_id,
                work_date: correction.work_date,
                clock_in_at: correction.clock_in_at,
                clock_out_at: correction.clock_out_at,
                work_type: correction.work_type,
                note: None,
                version: 1,
            };
            attendance::insert(conn, &record, now).await?;
            Ok(record)
        }
        Some(record) => {
            let edited = AttendanceRecord {
                clock_in_at: correction.clock_in_at,
                clock_out_at: correction.clock_out_at,
                work_type: correction.work_type,
                ..record.clone()
            };
            require_updated(attendance::update(conn, &edited, now).await?)?;
            Ok(AttendanceRecord {
                version: record.version + 1,
                ..edited
            })
        }
    }
}

/// 정정이 반영됐음을 요청자에게 알린다.
async fn notify_corrected_tx(
    conn: &mut Conn,
    correction: &AttendanceCorrection,
    now: Timestamp,
) -> ServiceResult<()> {
    let dedupe_key = format!("attendance.corrected:{}", correction.id);
    notify(
        conn,
        now,
        Notify {
            to: correction.member_id,
            kind: "attendance.corrected",
            title: "근태 정정 요청이 승인되어 기록이 수정되었습니다".to_owned(),
            body: correction.work_date.to_string(),
            link: Some(approval::document_link(correction.document_id)),
            dedupe_key: Some(&dedupe_key),
        },
    )
    .await?;
    Ok(())
}

// ── 관리자 ──────────────────────────────────────────────────────────────

async fn admin_filter(
    conn: &mut Conn,
    range: DateRange,
    query: &AdminAttendanceQuery,
) -> ServiceResult<RecordFilter> {
    Ok(RecordFilter {
        range,
        member_id: query.member_id,
        departments: department_subtree(conn, query.department_id).await?,
    })
}

pub async fn admin_list(
    state: &AppState,
    ctx: &AuthContext,
    query: &AdminAttendanceQuery,
) -> ServiceResult<Page<AdminAttendanceItem>> {
    authz::require_admin(&ctx.actor)?;
    let range = parse_range(&query.from, &query.to)?;
    let page = query.page();
    let after = page.after::<RecordCursor>()?;
    let now = state.now();
    let mut conn = state.read().await?;
    let filter = admin_filter(&mut conn, range, query).await?;
    let (rows, has_more) = attendance::list_page(&mut conn, &filter, after, page.limit()).await?;
    let leaves = leaves_for(&mut conn, member_ids_of(&rows), range).await?;
    let owners = members_by_id(&mut conn, member_ids_of(&rows)).await?;
    let company = Company::load(&mut conn).await?;
    let evaluator = Evaluator::new(&mut conn, company, range, now).await?;
    let items = rows
        .into_iter()
        .map(|row| {
            let record = &row.record;
            Ok(AdminAttendanceItem {
                evaluation: evaluator.evaluate(
                    owner_of(&owners, record.member_id)?,
                    record.work_date,
                    Some(record),
                    leave_on(&leaves, record.member_id, record.work_date),
                ),
                record: record_dto(record),
                member_name: row.member_name,
                login_id: row.login_id,
                department_name: row.department_name,
            })
        })
        .collect::<ServiceResult<_>>()?;
    next_page(items, has_more, |last: &AdminAttendanceItem| RecordCursor {
        work_date: last.record.work_date,
        id: last.record.id,
    })
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
            let actor = require_current_admin(conn, ctx).await?;
            let member = found(members::find(conn, request.member_id).await?)?;
            authz::edit_personal_records(&actor, &member.subject())?;
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
            let actor = require_current_admin(conn, ctx).await?;
            let record = found(attendance::find(conn, record_id).await?)?;
            let member = found(members::find(conn, record.member_id).await?)?;
            authz::edit_personal_records(&actor, &member.subject())?;
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

fn csv_line(
    out: &mut String,
    evaluator: &Evaluator,
    owner: &Member,
    row: &RecordWithMember,
    leave: Option<DayLeave>,
) {
    let record = &row.record;
    let evaluation = evaluator.evaluate(owner, record.work_date, Some(record), leave);
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
            evaluation.status.as_str().to_owned(),
        ],
    );
}

/// 내보내기의 읽기 단계가 만든 것: 파일과 그 안의 기록 수.
pub struct ExportBody {
    pub file: CsvFile,
    pub rows: usize,
}

/// 기간의 모든 기록으로 CSV 본문을 만든다. **읽기만** 하므로 읽기 전용 연결에서 부른다.
/// 엑셀이 한글을 깨뜨리지 않도록 UTF-8 BOM을 붙인다.
///
/// 조회 횟수는 기록·구성원 수와 무관하다: 기록, 휴가, 기록 주인, 설정, 달력을 한 번씩 읽는다.
pub async fn export_body_tx(
    conn: &mut Conn,
    range: DateRange,
    now: Timestamp,
) -> ServiceResult<ExportBody> {
    let filter = RecordFilter {
        range,
        member_id: None,
        departments: None,
    };
    let rows = attendance::list_for_export(conn, &filter).await?;
    let leaves = leaves_for(conn, member_ids_of(&rows), range).await?;
    let owners = members_by_id(conn, member_ids_of(&rows)).await?;
    let company = Company::load(conn).await?;
    let evaluator = Evaluator::new(conn, company, range, now).await?;
    let mut body = String::from("\u{feff}");
    push_csv_row(&mut body, CSV_HEADER);
    for row in &rows {
        let leave = leave_on(&leaves, row.record.member_id, row.record.work_date);
        let owner = owner_of(&owners, row.record.member_id)?;
        csv_line(&mut body, &evaluator, owner, row, leave);
    }
    let (from, to) = (
        range.start().format("%Y-%m-%d"),
        range.end().format("%Y-%m-%d"),
    );
    Ok(ExportBody {
        file: CsvFile {
            filename: format!("attendance-{from}-{to}.csv"),
            body,
        },
        rows: rows.len(),
    })
}

/// 기간의 모든 기록을 CSV로 내보낸다.
///
/// 본문은 읽기 연결의 한 시점 데이터로 만든다([`export_body_tx`]). 하나뿐인 쓰기 연결을 쥔 채
/// 만들면 큰 내보내기 동안 모든 쓰기(출근 포함)가 멈추기 때문이다. 쓰기 트랜잭션은 감사 로그
/// 한 줄을 남길 때만 연다. 개인 기록을 대량으로 꺼내는 동작이므로, 감사 로그를 남기지 못하면
/// 만든 본문도 돌려주지 않는다.
pub async fn export_csv(
    state: &AppState,
    ctx: &AuthContext,
    query: &ExportQuery,
) -> ServiceResult<CsvFile> {
    authz::require_admin(&ctx.actor)?;
    let range = parse_range(&query.from, &query.to)?;
    let now = state.now();
    let exported = {
        let mut snapshot = state.read_snapshot().await?;
        export_body_tx(&mut snapshot, range, now).await?
    };
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "attendance.export",
                    target_type: "attendance_record",
                    target_id: None,
                    detail: Some(serde_json::json!({
                        "from": range.start().format("%Y-%m-%d").to_string(),
                        "to": range.end().format("%Y-%m-%d").to_string(),
                        "rows": exported.rows,
                    })),
                    ctx: Some(ctx),
                },
            )
            .await
        })
        .await?;
    Ok(exported.file)
}
