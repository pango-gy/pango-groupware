//! 캘린더(내 일정·전사 일정·부서 일정)와 참석자.
//!
//! 종일 일정은 `start_date`·`end_date`(양 끝 포함), 시간 일정은 `start_at`·`end_at`(UTC 밀리초,
//! 끝 시각 미포함)만 채운다. 나머지 둘은 `null`이다.

use chrono::NaiveDate;
use pango_core::calendar_access::{CalendarKind, EventRole};
use pango_core::calendar_event::AttendeeResponse;
use pango_core::id::{CalendarEventId, CalendarId, DepartmentId, MemberId};
use pango_core::leave::LeaveUnit;
use pango_core::string_enum;
use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::admin::HolidayDto;
use crate::leave::LeaveCalendarEntry;

/// 요청자가 볼 수 있는 캘린더 하나.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarDto {
    pub id: CalendarId,
    pub kind: CalendarKind,
    /// 화면에 보일 이름(내 일정, 전사 일정, 부서 이름).
    pub name: String,
    /// 부서 일정일 때만.
    pub department_id: Option<DepartmentId>,
    /// 이 캘린더에 일정을 만들 수 있는지.
    pub can_write: bool,
}

/// `GET /calendar/view` 쿼리. 날짜는 `YYYY-MM-DD`, 기간은 양 끝 포함 62일 이하.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarViewQuery {
    pub from: String,
    pub to: String,
    /// 쉼표로 구분한 캘린더 ID. 생략하면 볼 수 있는 캘린더 전부.
    #[serde(default)]
    pub calendars: Option<String>,
}

/// 캘린더 화면: 일정과, 일정 표에 함께 그리는 읽기 전용 정보(공휴일·휴가).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarView {
    /// 기간에 걸친 일정. 여러 날에 걸친 일정도 한 번만 나온다(배치는 화면이 한다).
    pub events: Vec<EventSummary>,
    pub holidays: Vec<HolidayDto>,
    /// 요청자가 볼 수 있는 구성원의 결재 중·승인 휴가. 사유와 휴가 종류는 싣지 않는다.
    pub leaves: Vec<LeaveCalendarEntry>,
}

/// 일정 한 줄.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSummary {
    pub id: CalendarEventId,
    pub calendar_id: CalendarId,
    pub calendar_kind: CalendarKind,
    pub title: String,
    pub all_day: bool,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub start_at: Option<Timestamp>,
    pub end_at: Option<Timestamp>,
    pub location: Option<String>,
    /// `owner`면 수정·삭제할 수 있다.
    pub my_role: EventRole,
    /// 참석자일 때 내 응답.
    pub my_response: Option<AttendeeResponse>,
    pub attendee_count: u32,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventAttendee {
    pub member_id: MemberId,
    pub name: String,
    pub response: AttendeeResponse,
    pub responded_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventDetail {
    #[serde(flatten)]
    pub summary: EventSummary,
    pub description: Option<String>,
    /// 일정을 만든 사람.
    pub owner_id: MemberId,
    pub owner_name: String,
    /// 초대한 순서.
    pub attendees: Vec<EventAttendee>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// `POST /calendar/events` 본문.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateEventRequest {
    /// 생략하면 내 일정.
    #[serde(default)]
    pub calendar_id: Option<CalendarId>,
    #[serde(flatten)]
    pub event: EventInput,
    /// 초대할 구성원(내 일정에만, 50명 이하, 본인·중복 불가).
    #[serde(default)]
    pub attendee_ids: Vec<MemberId>,
}

/// 일정 내용. 수정은 [`UpdateEventRequest`]를 [`crate::Versioned`]로 감싸 전체를 다시 보낸다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventInput {
    pub title: String,
    pub all_day: bool,
    /// 종일 일정의 첫날.
    #[serde(default)]
    pub start_date: Option<NaiveDate>,
    /// 종일 일정의 마지막 날(포함). 생략하면 `start_date`.
    #[serde(default)]
    pub end_date: Option<NaiveDate>,
    #[serde(default)]
    pub start_at: Option<Timestamp>,
    #[serde(default)]
    pub end_at: Option<Timestamp>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// `PATCH /calendar/events/{id}` 본문(`version`과 함께). 캘린더는 옮길 수 없다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateEventRequest {
    #[serde(flatten)]
    pub event: EventInput,
    /// 보내면 참석자 목록을 이 목록으로 바꾼다. 생략하면 그대로 둔다.
    #[serde(default)]
    pub attendee_ids: Option<Vec<MemberId>>,
}

/// `POST /calendar/events/{id}/respond` 본문. `pending`은 보낼 수 없다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RespondRequest {
    pub status: AttendeeResponse,
}

/// `GET /calendar/conflicts` 쿼리. 기간은 양 끝 포함 7일 이하.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictQuery {
    /// 쉼표로 구분한 구성원 ID(50명 이하).
    pub member_ids: String,
    pub from: String,
    pub to: String,
}

string_enum! {
    /// 바쁜 시간의 종류.
    pub enum BusyKind("kind") {
        /// 요청자도 볼 수 있는 일정(요청자가 주인이거나 참석자). 제목이 있다.
        Event => "event",
        /// 요청자가 볼 수 있는 구성원의 승인된 휴가.
        Leave => "leave",
        /// 요청자가 볼 수 없는 일정이나 휴가. 시간만 있다.
        Busy => "busy",
    }
}

/// 바쁜 시간 하나. 종일이면 날짜, 아니면 시각만 채운다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusyBlock {
    pub kind: BusyKind,
    pub all_day: bool,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub start_at: Option<Timestamp>,
    pub end_at: Option<Timestamp>,
    /// `event`일 때만.
    pub event_id: Option<CalendarEventId>,
    /// `event`일 때만.
    pub title: Option<String>,
    /// `leave`일 때만.
    pub leave_unit: Option<LeaveUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberBusy {
    pub member_id: MemberId,
    pub member_name: String,
    /// 시작순.
    pub busy: Vec<BusyBlock>,
}

/// 참석자 일정 확인 결과. 요청한 구성원 순서 그대로.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConflictReport {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub members: Vec<MemberBusy>,
}
