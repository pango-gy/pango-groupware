//! 캘린더 일정과 참석자.

use chrono::{Duration, NaiveDate};
use pango_core::calendar_access::{Calendar, CalendarKind};
use pango_core::calendar_event::{AttendeeResponse, EventTime, MAX_EVENT_DAYS};
use pango_core::id::{CalendarEventId, CalendarId, DepartmentId, MemberId};
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::{DbError, DbResult};
use crate::pool::Conn;

/// 저장되는 일정 한 건.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub id: CalendarEventId,
    pub calendar: Calendar,
    pub owner_id: MemberId,
    pub title: String,
    pub time: EventTime,
    pub location: Option<String>,
    pub description: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub version: i64,
}

/// 조회 기간. 종일 일정은 날짜(`first_day`~`last_day`, 양 끝 포함)로, 시간 일정은 시각
/// (`[starts, ends)`, 회사 시간대의 첫날 0시 ~ 마지막 날 다음 날 0시)으로 겹침을 본다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventWindow {
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    pub starts: Timestamp,
    pub ends: Timestamp,
}

/// 참석자 한 명(이름 포함).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attendee {
    pub member_id: MemberId,
    pub name: String,
    pub response: AttendeeResponse,
    pub responded_at: Option<Timestamp>,
}

#[derive(sqlx::FromRow)]
struct EventRow {
    id: String,
    calendar_id: String,
    kind: String,
    calendar_owner_id: Option<String>,
    department_id: Option<String>,
    owner_id: String,
    title: String,
    all_day: bool,
    start_date: Option<String>,
    end_date: Option<String>,
    start_at: Option<i64>,
    end_at: Option<i64>,
    location: Option<String>,
    description: Option<String>,
    created_at: i64,
    updated_at: i64,
    version: i64,
}

fn calendar(
    id: &str,
    kind: &str,
    owner_id: Option<&str>,
    department_id: Option<&str>,
) -> DbResult<Calendar> {
    let kind = codec::parse_with("kind", kind, CalendarKind::parse)?;
    let calendar = match kind {
        CalendarKind::Personal => {
            Calendar::Personal(codec::parse("owner_id", owner_id.unwrap_or_default())?)
        }
        CalendarKind::Company => Calendar::Company,
        CalendarKind::Department => Calendar::Department(codec::parse::<DepartmentId>(
            "department_id",
            department_id.unwrap_or_default(),
        )?),
    };
    let id: CalendarId = codec::parse("calendar_id", id)?;
    if calendar.id() != id {
        return Err(DbError::Corrupt {
            column: "calendar_id",
            source: pango_core::DomainError::validation(
                "calendar_id",
                pango_core::ValidationReason::Inconsistent,
            ),
        });
    }
    Ok(calendar)
}

impl TryFrom<EventRow> for CalendarEvent {
    type Error = DbError;

    fn try_from(row: EventRow) -> DbResult<Self> {
        let start_date = codec::date_opt("start_date", row.start_date.as_deref())?;
        let end_date = codec::date_opt("end_date", row.end_date.as_deref())?;
        let start_at = codec::timestamp_opt("start_at", row.start_at)?;
        let end_at = codec::timestamp_opt("end_at", row.end_at)?;
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            calendar: calendar(
                &row.calendar_id,
                &row.kind,
                row.calendar_owner_id.as_deref(),
                row.department_id.as_deref(),
            )?,
            owner_id: codec::parse("owner_id", &row.owner_id)?,
            title: row.title,
            time: EventTime::new(row.all_day, start_date, end_date, start_at, end_at)
                .map_err(codec::corrupt("time"))?,
            location: row.location,
            description: row.description,
            created_at: codec::timestamp("created_at", row.created_at)?,
            updated_at: codec::timestamp("updated_at", row.updated_at)?,
            version: row.version,
        })
    }
}

const EVENT_SELECT: &str = "SELECT e.id, e.calendar_id, c.kind, c.owner_id AS calendar_owner_id,
        c.department_id, e.owner_id, e.title, e.all_day, e.start_date, e.end_date, e.start_at,
        e.end_at, e.location, e.description, e.created_at, e.updated_at, e.version
     FROM calendar_events e
     JOIN calendars c ON c.id = e.calendar_id";

/// 기간과 겹치는 일정. 바인드 순서: 마지막 날, 첫날, 첫날 하한, 끝 시각, 시작 시각, 시작 하한.
/// 하한은 일정 최대 길이로 구한 값이라 결과를 바꾸지 않고 색인 범위만 좁힌다.
const WINDOW_WHERE: &str = "((e.all_day = 1 AND e.start_date <= ? AND e.end_date >= ?
                             AND e.start_date >= ?)
       OR (e.all_day = 0 AND e.start_at < ? AND e.end_at > ? AND e.start_at >= ?))";

fn bind_window<'q, O>(
    query: sqlx::query::QueryAs<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments<'q>>,
    window: &EventWindow,
) -> sqlx::query::QueryAs<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments<'q>> {
    let span = Duration::days(MAX_EVENT_DAYS);
    let earliest_day = window
        .first_day
        .checked_sub_signed(span)
        .unwrap_or(NaiveDate::MIN);
    let earliest_start = window
        .starts
        .checked_sub(span)
        .map_or(i64::MIN, Timestamp::as_millis);
    query
        .bind(codec::date_text(window.last_day))
        .bind(codec::date_text(window.first_day))
        .bind(codec::date_text(earliest_day))
        .bind(window.ends.as_millis())
        .bind(window.starts.as_millis())
        .bind(earliest_start)
}

fn ids_json<T: ToString>(column: &'static str, ids: &[T]) -> DbResult<String> {
    let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
    codec::json_text(column, &ids)
}

/// 캘린더 행을 만든다. 이미 있으면 아무 일도 하지 않는다(첫 일정을 만들 때 부른다).
pub async fn ensure_calendar(conn: &mut Conn, calendar: &Calendar, now: Timestamp) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO calendars (id, kind, owner_id, department_id, created_at)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(calendar.id().to_string())
    .bind(calendar.kind().as_str())
    .bind(calendar.owner_id().map(|id| id.to_string()))
    .bind(calendar.department_id().map(|id| id.to_string()))
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

/// 캘린더 행이 있는지(테스트·점검용).
pub async fn calendar_exists(conn: &mut Conn, id: CalendarId) -> DbResult<bool> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM calendars WHERE id = ?")
        .bind(id.to_string())
        .fetch_optional(conn)
        .await?;
    Ok(found.is_some())
}

fn time_columns(
    time: &EventTime,
) -> (
    bool,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
) {
    let dates = time.dates();
    let instants = time.instants();
    (
        time.is_all_day(),
        dates.map(|(start, _)| codec::date_text(start)),
        dates.map(|(_, end)| codec::date_text(end)),
        instants.map(|(start, _)| start.as_millis()),
        instants.map(|(_, end)| end.as_millis()),
    )
}

/// 일정을 저장한다. 캘린더 행은 [`ensure_calendar`]로 먼저 만들어 둔다.
pub async fn insert_event(conn: &mut Conn, event: &CalendarEvent) -> DbResult<()> {
    let (all_day, start_date, end_date, start_at, end_at) = time_columns(&event.time);
    sqlx::query(
        "INSERT INTO calendar_events
            (id, calendar_id, owner_id, title, all_day, start_date, end_date, start_at, end_at,
             location, description, created_at, updated_at, version)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(event.id.to_string())
    .bind(event.calendar.id().to_string())
    .bind(event.owner_id.to_string())
    .bind(&event.title)
    .bind(all_day)
    .bind(start_date)
    .bind(end_date)
    .bind(start_at)
    .bind(end_at)
    .bind(&event.location)
    .bind(&event.description)
    .bind(event.created_at.as_millis())
    .bind(event.updated_at.as_millis())
    .bind(event.version)
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn find_event(conn: &mut Conn, id: CalendarEventId) -> DbResult<Option<CalendarEvent>> {
    sqlx::query_as::<_, EventRow>(&format!("{EVENT_SELECT} WHERE e.id = ?"))
        .bind(id.to_string())
        .fetch_optional(conn)
        .await?
        .map(CalendarEvent::try_from)
        .transpose()
}

/// 내용과 시간을 갱신한다. `event.version`이 현재 값과 같을 때만. 갱신했으면 `true`.
/// 캘린더와 주인은 바꾸지 않는다.
pub async fn update_event(
    conn: &mut Conn,
    event: &CalendarEvent,
    now: Timestamp,
) -> DbResult<bool> {
    let (all_day, start_date, end_date, start_at, end_at) = time_columns(&event.time);
    let result = sqlx::query(
        "UPDATE calendar_events SET
            title = ?, all_day = ?, start_date = ?, end_date = ?, start_at = ?, end_at = ?,
            location = ?, description = ?, updated_at = ?, version = version + 1
         WHERE id = ? AND version = ?",
    )
    .bind(&event.title)
    .bind(all_day)
    .bind(start_date)
    .bind(end_date)
    .bind(start_at)
    .bind(end_at)
    .bind(&event.location)
    .bind(&event.description)
    .bind(now.as_millis())
    .bind(event.id.to_string())
    .bind(event.version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 일정을 지운다(참석자도 함께). `version`이 현재 값과 같을 때만. 지웠으면 `true`.
pub async fn delete_event(conn: &mut Conn, id: CalendarEventId, version: i64) -> DbResult<bool> {
    let result = sqlx::query("DELETE FROM calendar_events WHERE id = ? AND version = ?")
        .bind(id.to_string())
        .bind(version)
        .execute(conn)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// 기간과 겹치는 일정 중, `calendars`에 속하거나 `attendee`가 초대받은 것(ID순).
pub async fn list_events(
    conn: &mut Conn,
    calendars: &[CalendarId],
    attendee: Option<MemberId>,
    window: &EventWindow,
) -> DbResult<Vec<CalendarEvent>> {
    let sql = format!(
        "{EVENT_SELECT}
         WHERE (e.calendar_id IN (SELECT value FROM json_each(?))
                OR e.id IN (SELECT event_id FROM calendar_attendees WHERE member_id = ?))
           AND {WINDOW_WHERE}
         ORDER BY e.id"
    );
    let query = sqlx::query_as::<_, EventRow>(&sql)
        .bind(ids_json("calendar_id", calendars)?)
        .bind(attendee.map(|id| id.to_string()));
    let rows = bind_window(query, window).fetch_all(conn).await?;
    rows.into_iter().map(CalendarEvent::try_from).collect()
}

#[derive(sqlx::FromRow)]
struct BusyRow {
    busy_member: String,
    #[sqlx(flatten)]
    event: EventRow,
}

/// 기간과 겹치는, 구성원들이 **참여하는** 일정: 각자의 내 일정 + 초대받아 거절하지 않은 일정.
/// `(구성원, 일정)` 쌍으로 돌려준다(같은 일정이 여러 구성원에게 나올 수 있다).
pub async fn busy_events(
    conn: &mut Conn,
    members: &[MemberId],
    window: &EventWindow,
) -> DbResult<Vec<(MemberId, CalendarEvent)>> {
    if members.is_empty() {
        return Ok(Vec::new());
    }
    let ids = ids_json("member_id", members)?;
    let owned = format!(
        "SELECT sub.calendar_owner_id AS busy_member, sub.* FROM ({EVENT_SELECT}
             WHERE c.kind = 'personal' AND c.owner_id IN (SELECT value FROM json_each(?))
               AND {WINDOW_WHERE}) sub"
    );
    let query = sqlx::query_as::<_, BusyRow>(&owned).bind(ids.clone());
    let mut rows = bind_window(query, window).fetch_all(&mut *conn).await?;
    let invited = format!(
        "SELECT a.member_id AS busy_member, sub.* FROM ({EVENT_SELECT}
             WHERE {WINDOW_WHERE}) sub
         JOIN calendar_attendees a ON a.event_id = sub.id
         WHERE a.member_id IN (SELECT value FROM json_each(?)) AND a.response <> 'declined'"
    );
    let query = bind_window(sqlx::query_as::<_, BusyRow>(&invited), window).bind(ids);
    rows.extend(query.fetch_all(conn).await?);
    rows.into_iter()
        .map(|row| {
            Ok((
                codec::parse("busy_member", &row.busy_member)?,
                CalendarEvent::try_from(row.event)?,
            ))
        })
        .collect()
}

/// 참석자를 추가한다(응답 대기). 이미 있으면 그대로 두고 `false`.
pub async fn add_attendee(
    conn: &mut Conn,
    event_id: CalendarEventId,
    member_id: MemberId,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "INSERT INTO calendar_attendees (event_id, member_id, invited_at) VALUES (?, ?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(event_id.to_string())
    .bind(member_id.to_string())
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 참석자를 뺀다. 있었으면 `true`.
pub async fn remove_attendee(
    conn: &mut Conn,
    event_id: CalendarEventId,
    member_id: MemberId,
) -> DbResult<bool> {
    let result = sqlx::query("DELETE FROM calendar_attendees WHERE event_id = ? AND member_id = ?")
        .bind(event_id.to_string())
        .bind(member_id.to_string())
        .execute(conn)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// 참석자의 응답을 바꾼다. 참석자가 아니면 `false`.
pub async fn set_response(
    conn: &mut Conn,
    event_id: CalendarEventId,
    member_id: MemberId,
    response: AttendeeResponse,
    now: Timestamp,
) -> DbResult<bool> {
    let responded_at = (response != AttendeeResponse::Pending).then_some(now.as_millis());
    let result = sqlx::query(
        "UPDATE calendar_attendees SET response = ?, responded_at = ?
         WHERE event_id = ? AND member_id = ?",
    )
    .bind(response.as_str())
    .bind(responded_at)
    .bind(event_id.to_string())
    .bind(member_id.to_string())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

#[derive(sqlx::FromRow)]
struct AttendeeRow {
    member_id: String,
    name: String,
    response: String,
    responded_at: Option<i64>,
}

/// 일정의 참석자(초대 시각순, 같으면 ID순).
pub async fn list_attendees(conn: &mut Conn, event_id: CalendarEventId) -> DbResult<Vec<Attendee>> {
    let rows = sqlx::query_as::<_, AttendeeRow>(
        "SELECT a.member_id, m.name, a.response, a.responded_at
         FROM calendar_attendees a JOIN members m ON m.id = a.member_id
         WHERE a.event_id = ? ORDER BY a.invited_at, a.member_id",
    )
    .bind(event_id.to_string())
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(Attendee {
                member_id: codec::parse("member_id", &row.member_id)?,
                name: row.name,
                response: codec::parse_with("response", &row.response, AttendeeResponse::parse)?,
                responded_at: codec::timestamp_opt("responded_at", row.responded_at)?,
            })
        })
        .collect()
}

/// 여러 일정의 `(일정, 참석자, 응답)`. 목록의 참석자 수와 내 응답을 한 번에 구할 때 쓴다.
pub async fn attendance_of(
    conn: &mut Conn,
    event_ids: &[CalendarEventId],
) -> DbResult<Vec<(CalendarEventId, MemberId, AttendeeResponse)>> {
    if event_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT event_id, member_id, response FROM calendar_attendees
         WHERE event_id IN (SELECT value FROM json_each(?))",
    )
    .bind(ids_json("event_id", event_ids)?)
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|(event_id, member_id, response)| {
            Ok((
                codec::parse("event_id", &event_id)?,
                codec::parse("member_id", &member_id)?,
                codec::parse_with("response", &response, AttendeeResponse::parse)?,
            ))
        })
        .collect()
}
