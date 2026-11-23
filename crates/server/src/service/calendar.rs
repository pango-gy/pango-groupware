//! 캘린더: 내 일정·전사 일정·부서 일정, 참석자 초대·응답, 참석자 일정 확인.
//!
//! 권한은 [`pango_core::calendar_access`]가 정한다. 다른 사람의 개인 일정은 초대받은 참석자만
//! 보고, 관리자도 예외가 아니다. 캘린더 화면의 공휴일·휴가는 저장하지 않고 조회할 때 읽는다
//! (휴가는 휴가 달력과 같은 [`leave::schedule_tx`]로, 사유·종류 없이).
//!
//! 캘린더 행은 첫 일정을 만들 때 생긴다(전사 일정은 마이그레이션이 만든다). ID가 주인에게서
//! 정해지므로 행이 없어도 목록과 권한 판정은 같은 ID를 쓴다.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use pango_api::admin::HolidayDto;
use pango_api::calendar::{
    BusyBlock, BusyKind, CalendarDto, CalendarView, CalendarViewQuery, ConflictQuery,
    ConflictReport, CreateEventRequest, EventAttendee, EventDetail, EventInput, EventSummary,
    MemberBusy, UpdateEventRequest,
};
use pango_api::Versioned;
use pango_core::authz::{self, Actor};
use pango_core::calendar::{parse_date, DateRange};
use pango_core::calendar_access::{
    self as access, Calendar, CalendarKind, EventRole, COMPANY_CALENDAR_ID,
};
use pango_core::calendar_event::{
    bounded_range, check_attendees, parse_description, parse_location, AttendeeResponse, EventTime,
    MAX_ATTENDEES, MAX_CONFLICT_DAYS, MAX_VIEW_DAYS,
};
use pango_core::error::ValidationReason;
use pango_core::id::{CalendarEventId, CalendarId, DepartmentId, MemberId};
use pango_core::leave::LeaveRequestStatus;
use pango_core::org::DepartmentTree;
use pango_core::text::Title;
use pango_core::time::{MinuteOfDay, Timestamp, WorkCalendarZone};
use pango_core::DomainError;
use pango_db::calendar::{self, Attendee, CalendarEvent, EventWindow};
use pango_db::org::Department;
use pango_db::{members, org, settings, Conn};

use super::leave;
use super::support::{
    audit, current_actor, found, member_names, notify, require_updated, Audit, AuthContext,
    Company, Idempotency, Notify,
};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 알림 종류. 초대는 일정·참석자마다 한 번만 간다(빠졌다가 다시 초대돼도 다시 가지 않는다).
const INVITED: &str = "calendar.invited";
const UPDATED: &str = "calendar.updated";
const CANCELED: &str = "calendar.canceled";
const RESPONDED: &str = "calendar.responded";

/// `GET /calendar/view`의 `calendars`에 한 번에 줄 수 있는 캘린더 수.
const MAX_SELECTED_CALENDARS: usize = 200;

fn invalid(field: &'static str, reason: ValidationReason) -> ServiceError {
    DomainError::validation(field, reason).into()
}

// ───────────── 권한 맥락 ─────────────

/// 요청자 한 명이 캘린더를 판정하는 데 필요한 것: 권한 정보, 소속 부서의 조상, 살아 있는 부서.
struct Access {
    actor: Actor,
    ancestors: Vec<DepartmentId>,
    /// 보관되지 않은 부서(조직도 순서).
    departments: Vec<Department>,
}

impl Access {
    async fn load(conn: &mut Conn, actor: Actor) -> ServiceResult<Self> {
        let departments = org::list_departments(conn).await?;
        let tree = DepartmentTree::new(departments.iter().map(Department::node))
            .map_err(ServiceError::internal)?;
        let ancestors = actor
            .department_id
            .map(|own| tree.ancestors_inclusive(own))
            .unwrap_or_default();
        Ok(Self {
            actor,
            ancestors,
            departments,
        })
    }

    /// 쓰기 트랜잭션 안에서 요청자의 권한(역할·관할 부서)을 다시 읽어 만든다. 인증 뒤에 조직장에서
    /// 물러났거나 관리자에서 해제됐으면 그 값으로 판정한다.
    async fn current(conn: &mut Conn, ctx: &AuthContext) -> ServiceResult<Self> {
        let actor = current_actor(conn, ctx).await?;
        Self::load(conn, actor).await
    }

    fn me(&self) -> MemberId {
        self.actor.member_id
    }

    /// 보관된 부서의 일정은 조직도에서 사라진 부서의 것이므로 없는 것으로 본다.
    fn live(&self, calendar: &Calendar) -> bool {
        match calendar {
            Calendar::Department(id) => self.departments.iter().any(|d| d.id == *id),
            Calendar::Personal(_) | Calendar::Company => true,
        }
    }

    fn department_name(&self, id: DepartmentId) -> String {
        self.departments
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.name.clone())
            .unwrap_or_default()
    }

    fn read(&self, calendar: &Calendar) -> ServiceResult<()> {
        if !self.live(calendar) {
            return Err(DomainError::NotFound.into());
        }
        Ok(access::can_read(&self.actor, calendar, &self.ancestors)?)
    }

    fn write(&self, calendar: &Calendar) -> ServiceResult<()> {
        if !self.live(calendar) {
            return Err(DomainError::NotFound.into());
        }
        Ok(access::can_write(&self.actor, calendar, &self.ancestors)?)
    }

    fn role(&self, calendar: &Calendar, is_attendee: bool) -> ServiceResult<EventRole> {
        if !self.live(calendar) {
            return Err(DomainError::NotFound.into());
        }
        Ok(access::event_role(
            &self.actor,
            calendar,
            &self.ancestors,
            is_attendee,
        )?)
    }

    /// 요청자가 볼 수 있는 캘린더 전부: 내 일정, 전사 일정, 볼 수 있는 부서 일정(조직도 순서).
    fn visible_calendars(&self) -> Vec<Calendar> {
        let mut calendars = vec![Calendar::Personal(self.me()), Calendar::Company];
        calendars.extend(
            self.departments
                .iter()
                .map(|d| Calendar::Department(d.id))
                .filter(|calendar| self.read(calendar).is_ok()),
        );
        calendars
    }

    /// 캘린더 ID → 캘린더. 없거나 보관된 부서면 `None`(볼 수 있는지는 따로 판정한다).
    async fn resolve(&self, conn: &mut Conn, id: CalendarId) -> ServiceResult<Option<Calendar>> {
        if id == COMPANY_CALENDAR_ID {
            return Ok(Some(Calendar::Company));
        }
        let department = DepartmentId::from_uuid(id.as_uuid());
        if self.departments.iter().any(|d| d.id == department) {
            return Ok(Some(Calendar::Department(department)));
        }
        let member = MemberId::from_uuid(id.as_uuid());
        Ok(members::find_existing(conn, member)
            .await?
            .map(|_| Calendar::Personal(member)))
    }
}

// ───────────── 입력 ─────────────

/// 검증을 마친 일정 내용.
struct Content {
    title: String,
    time: EventTime,
    location: Option<String>,
    description: Option<String>,
}

impl Content {
    fn parse(input: &EventInput) -> ServiceResult<Self> {
        Ok(Self {
            title: Title::parse(&input.title)?.into_string(),
            time: EventTime::new(
                input.all_day,
                input.start_date,
                input.end_date,
                input.start_at,
                input.end_at,
            )?,
            location: parse_location(input.location.as_deref())?,
            description: parse_description(input.description.as_deref())?,
        })
    }
}

/// 쉼표로 구분한 ID 목록. 비었거나, 형식이 틀렸거나, `max`개를 넘거나, 겹치면 `field`의 입력
/// 오류다. 순서는 받은 그대로.
fn parse_id_list<T>(field: &'static str, raw: &str, max: usize) -> ServiceResult<Vec<T>>
where
    T: FromStr<Err = DomainError> + Ord + Copy,
{
    // ID 하나는 36자다. 쉼표를 셀 것도 없이 길이로 먼저 자른다.
    if raw.len() > max.saturating_mul(37) {
        return Err(invalid(field, ValidationReason::OutOfRange));
    }
    if raw.trim().is_empty() {
        return Err(invalid(field, ValidationReason::Empty));
    }
    let ids = raw
        .split(',')
        .map(|part| part.trim().parse::<T>().map_err(|e| e.with_field(field)))
        .collect::<Result<Vec<T>, DomainError>>()?;
    if ids.len() > max {
        return Err(invalid(field, ValidationReason::OutOfRange));
    }
    let mut seen = BTreeSet::new();
    if !ids.iter().all(|id| seen.insert(*id)) {
        return Err(invalid(field, ValidationReason::Duplicate));
    }
    Ok(ids)
}

fn query_range(from: &str, to: &str, max_days: i64) -> ServiceResult<DateRange> {
    let from = parse_date("from", from)?;
    let to = parse_date("to", to)?;
    Ok(bounded_range(from, to, max_days)?)
}

/// 회사 시간대로 본 기간: 첫날 0시부터 마지막 날 다음 날 0시까지.
fn window(zone: &WorkCalendarZone, range: DateRange) -> ServiceResult<EventWindow> {
    let after_last = range
        .end()
        .succ_opt()
        .ok_or_else(|| invalid("to", ValidationReason::OutOfRange))?;
    Ok(EventWindow {
        first_day: range.start(),
        last_day: range.end(),
        starts: zone.instant_at(range.start(), MinuteOfDay::MIDNIGHT),
        ends: zone.instant_at(after_last, MinuteOfDay::MIDNIGHT),
    })
}

/// 새로 초대하는 구성원은 디렉터리에서 볼 수 있어야 하고(아니면 `NotFound`), 재직 중이어야
/// 한다(`MemberInactive`, 정지·퇴사 계정이 보이는 관리자에게만 나온다).
async fn check_invitees(conn: &mut Conn, actor: &Actor, ids: &[MemberId]) -> ServiceResult<()> {
    let found = members::find_many_existing(conn, ids).await?;
    for id in ids {
        let member = found
            .iter()
            .find(|member| member.id == *id)
            .ok_or(DomainError::NotFound)?;
        authz::view_member(actor, member.status)?;
        member.status.require_active()?;
    }
    Ok(())
}

// ───────────── 응답 변환 ─────────────

fn summary(
    event: &CalendarEvent,
    role: EventRole,
    my_response: Option<AttendeeResponse>,
    attendee_count: usize,
) -> EventSummary {
    let dates = event.time.dates();
    let instants = event.time.instants();
    EventSummary {
        id: event.id,
        calendar_id: event.calendar.id(),
        calendar_kind: event.calendar.kind(),
        title: event.title.clone(),
        all_day: event.time.is_all_day(),
        start_date: dates.map(|(start, _)| start),
        end_date: dates.map(|(_, end)| end),
        start_at: instants.map(|(start, _)| start),
        end_at: instants.map(|(_, end)| end),
        location: event.location.clone(),
        my_role: role,
        my_response,
        attendee_count: u32::try_from(attendee_count).unwrap_or(u32::MAX),
        version: event.version,
    }
}

/// 일정 하나를 요청자 기준으로 읽는다. 볼 수 없으면 `NotFound`. 참석자 목록과 자격을 함께 준다.
async fn visible_event(
    conn: &mut Conn,
    access: &Access,
    id: CalendarEventId,
) -> ServiceResult<(CalendarEvent, Vec<Attendee>, EventRole)> {
    let event = found(calendar::find_event(conn, id).await?)?;
    let attendees = calendar::list_attendees(conn, id).await?;
    let is_attendee = attendees.iter().any(|a| a.member_id == access.me());
    let role = access.role(&event.calendar, is_attendee)?;
    Ok((event, attendees, role))
}

async fn detail(
    conn: &mut Conn,
    access: &Access,
    event: &CalendarEvent,
    attendees: Vec<Attendee>,
    role: EventRole,
) -> ServiceResult<EventDetail> {
    let owner_name = member_names(conn, [event.owner_id])
        .await?
        .remove(&event.owner_id)
        .unwrap_or_default();
    let my_response = attendees
        .iter()
        .find(|a| a.member_id == access.me())
        .map(|a| a.response);
    Ok(EventDetail {
        summary: summary(event, role, my_response, attendees.len()),
        description: event.description.clone(),
        owner_id: event.owner_id,
        owner_name,
        attendees: attendees
            .into_iter()
            .map(|a| EventAttendee {
                member_id: a.member_id,
                name: a.name,
                response: a.response,
                responded_at: a.responded_at,
            })
            .collect(),
        created_at: event.created_at,
        updated_at: event.updated_at,
    })
}

async fn load_detail(
    conn: &mut Conn,
    access: &Access,
    id: CalendarEventId,
) -> ServiceResult<EventDetail> {
    let (event, attendees, role) = visible_event(conn, access, id).await?;
    detail(conn, access, &event, attendees, role).await
}

// ───────────── 알림·감사 ─────────────

fn event_link(id: CalendarEventId) -> Option<String> {
    Some(format!("/calendar/events/{id}"))
}

async fn tell(
    conn: &mut Conn,
    now: Timestamp,
    to: MemberId,
    kind: &'static str,
    event: &CalendarEvent,
    body: &str,
    dedupe_key: &str,
) -> ServiceResult<()> {
    let link = if kind == CANCELED {
        None
    } else {
        event_link(event.id)
    };
    notify(
        conn,
        now,
        Notify {
            to,
            kind,
            title: event.title.clone(),
            body: body.to_owned(),
            link,
            dedupe_key: Some(dedupe_key),
        },
    )
    .await?;
    Ok(())
}

async fn invite(
    conn: &mut Conn,
    now: Timestamp,
    event: &CalendarEvent,
    member_id: MemberId,
) -> ServiceResult<()> {
    calendar::add_attendee(conn, event.id, member_id, now).await?;
    let key = format!("{INVITED}:{}", event.id);
    tell(
        conn,
        now,
        member_id,
        INVITED,
        event,
        "일정에 초대되었습니다.",
        &key,
    )
    .await
}

/// 공유 캘린더(전사·부서)의 일정 변경만 감사 로그에 남긴다. 개인 일정은 본인만의 기록이다.
async fn audit_shared(
    conn: &mut Conn,
    now: Timestamp,
    ctx: &AuthContext,
    action: &'static str,
    event: &CalendarEvent,
) -> ServiceResult<()> {
    if event.calendar.kind() == CalendarKind::Personal {
        return Ok(());
    }
    audit(
        conn,
        now,
        Audit {
            action,
            target_type: "calendar_event",
            target_id: Some(event.id.to_string()),
            detail: Some(serde_json::json!({
                "calendar_kind": event.calendar.kind().as_str(),
                "department_id": event.calendar.department_id(),
                "title": event.title,
            })),
            ctx: Some(ctx),
        },
    )
    .await
}

// ───────────── 조회 ─────────────

/// 볼 수 있는 캘린더 목록.
pub async fn list_calendars(
    state: &AppState,
    ctx: &AuthContext,
) -> ServiceResult<Vec<CalendarDto>> {
    let mut conn = state.read().await?;
    let access = Access::load(&mut conn, ctx.actor.clone()).await?;
    Ok(access
        .visible_calendars()
        .into_iter()
        .map(|calendar| CalendarDto {
            id: calendar.id(),
            kind: calendar.kind(),
            name: match calendar {
                Calendar::Personal(_) => "내 일정".to_owned(),
                Calendar::Company => "전사 일정".to_owned(),
                Calendar::Department(id) => access.department_name(id),
            },
            department_id: calendar.department_id(),
            can_write: access.write(&calendar).is_ok(),
        })
        .collect())
}

/// 캘린더 화면: 기간 안의 일정 + 공휴일 + 볼 수 있는 구성원의 휴가.
///
/// 초대받은 다른 사람의 일정은 내 일정과 함께 나온다(`calendars`에 내 일정이 있을 때).
pub async fn view(
    state: &AppState,
    ctx: &AuthContext,
    query: &CalendarViewQuery,
) -> ServiceResult<CalendarView> {
    let range = query_range(&query.from, &query.to, MAX_VIEW_DAYS)?;
    let selected = match query.calendars.as_deref() {
        None => None,
        Some(raw) => Some(parse_id_list::<CalendarId>(
            "calendars",
            raw,
            MAX_SELECTED_CALENDARS,
        )?),
    };
    let mut snapshot = state.read_snapshot().await?;
    let conn: &mut Conn = &mut snapshot;
    let access = Access::load(conn, ctx.actor.clone()).await?;
    let calendars = match selected {
        None => access.visible_calendars(),
        Some(ids) => {
            let mut calendars = Vec::with_capacity(ids.len());
            for id in ids {
                let calendar = found(access.resolve(conn, id).await?)?;
                access.read(&calendar)?;
                calendars.push(calendar);
            }
            calendars
        }
    };
    let with_invitations = calendars.contains(&Calendar::Personal(access.me()));
    let ids: Vec<CalendarId> = calendars.iter().map(Calendar::id).collect();

    let zone = Company::load(conn).await?.zone;
    let window = window(&zone, range)?;
    let events =
        calendar::list_events(conn, &ids, with_invitations.then_some(access.me()), &window).await?;
    let event_ids: Vec<CalendarEventId> = events.iter().map(|e| e.id).collect();
    let mut attendance: BTreeMap<CalendarEventId, Vec<(MemberId, AttendeeResponse)>> =
        BTreeMap::new();
    for (event_id, member_id, response) in calendar::attendance_of(conn, &event_ids).await? {
        attendance
            .entry(event_id)
            .or_default()
            .push((member_id, response));
    }
    let mut summaries = Vec::with_capacity(events.len());
    for event in &events {
        let attendees = attendance.get(&event.id).map(Vec::as_slice).unwrap_or(&[]);
        let mine = attendees
            .iter()
            .find(|(member, _)| *member == access.me())
            .map(|(_, response)| *response);
        // 목록 조건이 넓게 잡혀도(초대 조건 등) 볼 수 없는 일정은 싣지 않는다.
        let Ok(role) = access.role(&event.calendar, mine.is_some()) else {
            continue;
        };
        summaries.push((
            start_instant(&zone, &event.time),
            summary(event, role, mine, attendees.len()),
        ));
    }
    summaries.sort_by(|(a, x), (b, y)| (a, !x.all_day, x.id).cmp(&(b, !y.all_day, y.id)));

    let holidays = settings::list_holidays(conn, range)
        .await?
        .into_iter()
        .map(|h| HolidayDto {
            date: h.date,
            name: h.name,
        })
        .collect();
    let actor = &access.actor;
    let leaves = leave::schedule_tx(conn, range, |member| {
        authz::view_leave_schedule(actor, &member.subject()).is_ok()
    })
    .await?;
    Ok(CalendarView {
        events: summaries.into_iter().map(|(_, s)| s).collect(),
        holidays,
        leaves,
    })
}

/// 정렬용 시작 시각. 종일 일정은 회사 시간대의 그날 0시.
fn start_instant(zone: &WorkCalendarZone, time: &EventTime) -> Timestamp {
    match time {
        EventTime::AllDay { start, .. } => zone.instant_at(*start, MinuteOfDay::MIDNIGHT),
        EventTime::Timed { start, .. } => *start,
    }
}

/// 일정 상세. 볼 수 없으면 404.
pub async fn get_event(
    state: &AppState,
    ctx: &AuthContext,
    id: CalendarEventId,
) -> ServiceResult<EventDetail> {
    let mut conn = state.read().await?;
    let access = Access::load(&mut conn, ctx.actor.clone()).await?;
    load_detail(&mut conn, &access, id).await
}

// ───────────── 쓰기 ─────────────

/// 일정을 만든다. 캘린더를 생략하면 내 일정. 참석자는 내 일정에만 초대할 수 있고, 초대받은
/// 사람에게 `calendar.invited`를 보낸다.
pub async fn create_event(
    state: &AppState,
    ctx: &AuthContext,
    input: &CreateEventRequest,
    idempotency: Option<&Idempotency>,
) -> ServiceResult<EventDetail> {
    let now = state.now();
    let content = Content::parse(&input.event)?;
    check_attendees(ctx.member_id(), &input.attendee_ids)?;
    state
        .write_idempotent(ctx.member_id(), idempotency, async |conn: &mut Conn| {
            let access = Access::current(conn, ctx).await?;
            let target = match input.calendar_id {
                None => Calendar::Personal(access.me()),
                Some(id) => found(access.resolve(conn, id).await?)?,
            };
            access.write(&target)?;
            if !input.attendee_ids.is_empty() && target.kind() != CalendarKind::Personal {
                return Err(invalid("attendee_ids", ValidationReason::Inconsistent));
            }
            check_invitees(conn, &access.actor, &input.attendee_ids).await?;

            let event = CalendarEvent {
                id: CalendarEventId::new(),
                calendar: target,
                owner_id: access.me(),
                title: content.title.clone(),
                time: content.time,
                location: content.location.clone(),
                description: content.description.clone(),
                created_at: now,
                updated_at: now,
                version: 1,
            };
            calendar::ensure_calendar(conn, &target, now).await?;
            calendar::insert_event(conn, &event).await?;
            for member_id in &input.attendee_ids {
                invite(conn, now, &event, *member_id).await?;
            }
            audit_shared(conn, now, ctx, "calendar.event_create", &event).await?;
            load_detail(conn, &access, event.id).await
        })
        .await
}

/// 일정 수정(전체 내용을 다시 보낸다). 주인·쓰기 권한자만, 참석자는 403.
///
/// `attendee_ids`를 보내면 참석자를 그 목록으로 바꾼다: 빠진 사람에게 `calendar.canceled`, 새로
/// 들어온 사람에게 `calendar.invited`. 시간이나 장소가 바뀌면 남은 참석자에게 `calendar.updated`.
pub async fn update_event(
    state: &AppState,
    ctx: &AuthContext,
    id: CalendarEventId,
    request: &Versioned<UpdateEventRequest>,
) -> ServiceResult<EventDetail> {
    let now = state.now();
    let content = Content::parse(&request.data.event)?;
    state
        .write(async |conn: &mut Conn| {
            let access = Access::current(conn, ctx).await?;
            let (existing, attendees, role) = visible_event(conn, &access, id).await?;
            access::edit_event(role)?;
            let current: Vec<MemberId> = attendees.iter().map(|a| a.member_id).collect();
            let wanted = match &request.data.attendee_ids {
                None => current.clone(),
                Some(ids) => {
                    if !ids.is_empty() && existing.calendar.kind() != CalendarKind::Personal {
                        return Err(invalid("attendee_ids", ValidationReason::Inconsistent));
                    }
                    check_attendees(existing.owner_id, ids)?;
                    ids.clone()
                }
            };
            let added: Vec<MemberId> = wanted
                .iter()
                .copied()
                .filter(|id| !current.contains(id))
                .collect();
            let removed: Vec<MemberId> = current
                .iter()
                .copied()
                .filter(|id| !wanted.contains(id))
                .collect();
            // 이미 초대된 사람은 그사이 정지·퇴사했어도 목록에 남을 수 있다. 새로 부르는 사람만 확인한다.
            check_invitees(conn, &access.actor, &added).await?;

            let updated = CalendarEvent {
                title: content.title.clone(),
                time: content.time,
                location: content.location.clone(),
                description: content.description.clone(),
                version: request.version,
                ..existing.clone()
            };
            require_updated(calendar::update_event(conn, &updated, now).await?)?;
            let new_version = request.version + 1;
            let moved = existing.time != updated.time || existing.location != updated.location;

            let canceled_key = format!("{CANCELED}:{id}:{new_version}");
            for member_id in &removed {
                calendar::remove_attendee(conn, id, *member_id).await?;
                tell(
                    conn,
                    now,
                    *member_id,
                    CANCELED,
                    &updated,
                    "일정 참석자에서 제외되었습니다.",
                    &canceled_key,
                )
                .await?;
            }
            for member_id in &added {
                invite(conn, now, &updated, *member_id).await?;
            }
            if moved {
                let key = format!("{UPDATED}:{id}:{new_version}");
                for member_id in current.iter().filter(|m| wanted.contains(m)) {
                    tell(
                        conn,
                        now,
                        *member_id,
                        UPDATED,
                        &updated,
                        "일정의 시간이나 장소가 바뀌었습니다.",
                        &key,
                    )
                    .await?;
                }
            }
            audit_shared(conn, now, ctx, "calendar.event_update", &updated).await?;
            load_detail(conn, &access, id).await
        })
        .await
}

/// 일정 삭제(되돌릴 수 없다). 주인·쓰기 권한자만. 참석자에게 `calendar.canceled`를 보낸다.
pub async fn delete_event(
    state: &AppState,
    ctx: &AuthContext,
    id: CalendarEventId,
    version: i64,
) -> ServiceResult<()> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let access = Access::current(conn, ctx).await?;
            let (existing, attendees, role) = visible_event(conn, &access, id).await?;
            access::edit_event(role)?;
            require_updated(calendar::delete_event(conn, id, version).await?)?;
            let key = format!("{CANCELED}:{id}:deleted");
            for attendee in &attendees {
                tell(
                    conn,
                    now,
                    attendee.member_id,
                    CANCELED,
                    &existing,
                    "일정이 취소되었습니다.",
                    &key,
                )
                .await?;
            }
            audit_shared(conn, now, ctx, "calendar.event_delete", &existing).await
        })
        .await
}

/// 참석 응답. 참석자만 할 수 있고(주인·열람자 403, 볼 수 없으면 404), 같은 응답을 다시 보내면
/// 아무것도 바꾸지 않는다(자연 멱등). 응답이 바뀌면 주인에게 알린다.
pub async fn respond(
    state: &AppState,
    ctx: &AuthContext,
    id: CalendarEventId,
    status: AttendeeResponse,
) -> ServiceResult<EventDetail> {
    if status == AttendeeResponse::Pending {
        return Err(invalid("status", ValidationReason::InvalidFormat));
    }
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let access = Access::current(conn, ctx).await?;
            let (event, attendees, role) = visible_event(conn, &access, id).await?;
            access::respond_to_event(role)?;
            let previous = attendees
                .iter()
                .find(|a| a.member_id == access.me())
                .map(|a| a.response);
            if previous != Some(status) {
                calendar::set_response(conn, id, access.me(), status, now).await?;
                let body = match status {
                    AttendeeResponse::Accepted => "참석자가 일정을 수락했습니다.",
                    _ => "참석자가 일정을 거절했습니다.",
                };
                let key = format!("{RESPONDED}:{id}:{}:{status}", access.me());
                tell(conn, now, event.owner_id, RESPONDED, &event, body, &key).await?;
            }
            load_detail(conn, &access, id).await
        })
        .await
}

// ───────────── 참석자 일정 확인 ─────────────

/// 참석자 일정 확인: 구성원마다 기간 안의 바쁜 시간(본인 일정, 거절하지 않은 초대, 승인된 휴가).
///
/// 요청자가 볼 수 없는 일정은 시간만 준다(`busy`, 제목·ID 없음). 휴가는 휴가 일정을 볼 수 있는
/// 구성원이면 `leave`(단위만), 아니면 `busy`(종일)다. 사유·종류는 어느 경우에도 싣지 않는다.
pub async fn conflicts(
    state: &AppState,
    ctx: &AuthContext,
    query: &ConflictQuery,
) -> ServiceResult<ConflictReport> {
    let requested = parse_id_list::<MemberId>("member_ids", &query.member_ids, MAX_ATTENDEES)?;
    let range = query_range(&query.from, &query.to, MAX_CONFLICT_DAYS)?;
    let mut snapshot = state.read_snapshot().await?;
    let conn: &mut Conn = &mut snapshot;
    let access = Access::load(conn, ctx.actor.clone()).await?;
    let people = members::find_many_existing(conn, &requested).await?;
    for id in &requested {
        let member = people
            .iter()
            .find(|m| m.id == *id)
            .ok_or(DomainError::NotFound)?;
        authz::view_member(&access.actor, member.status)?;
        member.status.require_active()?;
    }
    let zone = Company::load(conn).await?.zone;
    let window = window(&zone, range)?;

    let busy = calendar::busy_events(conn, &requested, &window).await?;
    let event_ids: Vec<CalendarEventId> = busy.iter().map(|(_, e)| e.id).collect();
    let my_invitations: BTreeSet<CalendarEventId> = calendar::attendance_of(conn, &event_ids)
        .await?
        .into_iter()
        .filter(|(_, member, _)| *member == access.me())
        .map(|(event, _, _)| event)
        .collect();
    let mut blocks: BTreeMap<MemberId, Vec<(Timestamp, BusyBlock)>> = BTreeMap::new();
    for (member_id, event) in &busy {
        let shared = access
            .role(&event.calendar, my_invitations.contains(&event.id))
            .is_ok();
        let dates = event.time.dates();
        let instants = event.time.instants();
        let block = BusyBlock {
            kind: if shared {
                BusyKind::Event
            } else {
                BusyKind::Busy
            },
            all_day: event.time.is_all_day(),
            start_date: dates.map(|(start, _)| start),
            end_date: dates.map(|(_, end)| end),
            start_at: instants.map(|(start, _)| start),
            end_at: instants.map(|(_, end)| end),
            event_id: shared.then_some(event.id),
            title: shared.then(|| event.title.clone()),
            leave_unit: None,
        };
        blocks
            .entry(*member_id)
            .or_default()
            .push((start_instant(&zone, &event.time), block));
    }

    let wanted: BTreeSet<MemberId> = requested.iter().copied().collect();
    let leaves = leave::schedule_tx(conn, range, |member| wanted.contains(&member.id)).await?;
    for entry in leaves {
        if entry.status != LeaveRequestStatus::Approved {
            continue;
        }
        let Some(member) = people.iter().find(|m| m.id == entry.member_id) else {
            continue;
        };
        let visible = authz::view_leave_schedule(&access.actor, &member.subject()).is_ok();
        let block = BusyBlock {
            kind: if visible {
                BusyKind::Leave
            } else {
                BusyKind::Busy
            },
            all_day: true,
            start_date: Some(entry.start_date),
            end_date: Some(entry.end_date),
            start_at: None,
            end_at: None,
            event_id: None,
            title: None,
            leave_unit: visible.then_some(entry.unit),
        };
        blocks.entry(entry.member_id).or_default().push((
            zone.instant_at(entry.start_date, MinuteOfDay::MIDNIGHT),
            block,
        ));
    }

    let members = requested
        .iter()
        .map(|id| {
            let mut busy = blocks.remove(id).unwrap_or_default();
            busy.sort_by_key(|(start, block)| (*start, !block.all_day));
            MemberBusy {
                member_id: *id,
                member_name: people
                    .iter()
                    .find(|m| m.id == *id)
                    .map(|m| m.name.clone())
                    .unwrap_or_default(),
                busy: busy.into_iter().map(|(_, block)| block).collect(),
            }
        })
        .collect();
    Ok(ConflictReport {
        from: range.start(),
        to: range.end(),
        members,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn id_lists_are_strict() {
        let a = MemberId::new();
        let b = MemberId::new();
        let parsed: Vec<MemberId> =
            parse_id_list("member_ids", &format!("{a}, {b}"), 50).expect("parse");
        assert_eq!(parsed, [a, b]);
        let err = |raw: &str, max: usize| match parse_id_list::<MemberId>("member_ids", raw, max) {
            Err(ServiceError::Domain(DomainError::Validation { field, reason })) => (field, reason),
            other => panic!("expected validation error, got {other:?}"),
        };
        assert_eq!(err("", 50).1, ValidationReason::Empty);
        assert_eq!(err(&format!("{a},"), 50).1, ValidationReason::InvalidFormat);
        assert_eq!(err("not-a-uuid", 50).0, "member_ids");
        assert_eq!(err(&format!("{a},{a}"), 50).1, ValidationReason::Duplicate);
        assert_eq!(err(&format!("{a},{b}"), 1).1, ValidationReason::OutOfRange);
        assert_eq!(err(&"x".repeat(10_000), 50).1, ValidationReason::OutOfRange);
    }

    #[test]
    fn window_uses_company_midnights() {
        let zone = WorkCalendarZone::new(
            pango_core::time::parse_timezone("Asia/Seoul").expect("tz"),
            MinuteOfDay::from_hm(5, 0).expect("cutoff"),
        )
        .expect("zone");
        let range = DateRange::new(
            NaiveDate::from_ymd_opt(2026, 3, 1).expect("date"),
            NaiveDate::from_ymd_opt(2026, 3, 31).expect("date"),
        )
        .expect("range");
        let window = window(&zone, range).expect("window");
        // 근무일 전환 시각(05:00)과 무관하게 달력의 자정을 쓴다.
        assert_eq!(window.starts.to_string(), "2026-02-28T15:00:00.000Z");
        assert_eq!(window.ends.to_string(), "2026-03-31T15:00:00.000Z");
    }
}
