//! 일정 등록·수정 폼(데스크톱은 대화 상자, 모바일은 화면 가득한 시트)과 참석자 일정 확인.
//!
//! 입력 ↔ 요청 변환과 검증, 바쁜 시간의 위치 계산은 [`logic`]의 순수 함수가 한다.

use chrono::NaiveDate;
use dioxus::prelude::*;
use pango_api::calendar::{CalendarDto, ConflictReport, EventDetail};
use pango_api::Versioned;
use pango_core::calendar_access::CalendarKind;
use pango_core::calendar_event::MAX_ATTENDEES;
use pango_core::id::{CalendarEventId, MemberId};

use crate::api;
use crate::company::use_company;
use crate::session::use_session;
use crate::ui::{
    now, report_error, report_success, today, Button, ButtonKind, Checkbox, DateField, FormError,
    Icon, IconName, MemberPicker, Modal, SelectField, TextArea, TextField,
};

use logic::{Attendee, Slot};

/// 폼을 여는 방식.
#[derive(Debug, Clone, PartialEq)]
pub enum FormMode {
    /// 이 날짜로 새 일정.
    New {
        date: NaiveDate,
    },
    Edit(Box<EventDetail>),
}

#[component]
pub fn EventForm(
    mode: FormMode,
    calendars: Vec<CalendarDto>,
    onclose: EventHandler<()>,
    onsaved: EventHandler<EventDetail>,
    /// 다른 사람이 먼저 고쳤다(`version_conflict`). 이 일정을 다시 읽어 보여 줘야 한다.
    onreload: EventHandler<CalendarEventId>,
) -> Element {
    let session = use_session();
    let company = use_company();
    let tz = session.zone();
    let me = session.me().map(|me| me.id);
    let (work_start, _) = company.work_hours();

    let initial = mode.clone();
    let default_calendar = logic::default_calendar(&calendars);
    let mut draft = use_signal(move || match &initial {
        FormMode::New { date } => {
            let local_now = now().to_utc().with_timezone(&tz).naive_local();
            let local_now = (*date == today(tz)).then_some(local_now);
            logic::new_draft(*date, local_now, work_start, default_calendar)
        }
        FormMode::Edit(detail) => logic::draft_from(detail, tz),
    });
    // 폼을 열 때 하나 만들어 제출이 끝날 때까지 쓴다(두 번 눌러도 한 번만 생긴다).
    let key = use_hook(api::new_idempotency_key);
    let mut busy = use_signal(|| false);
    let mut problem = use_signal(|| None::<logic::Problem>);

    let editing = match &mode {
        FormMode::Edit(detail) => Some((
            detail.summary.id,
            detail.summary.version,
            detail.summary.calendar_kind,
        )),
        FormMode::New { .. } => None,
    };
    let current = draft();
    let personal = match editing {
        Some((_, _, kind)) => kind == CalendarKind::Personal,
        None => logic::is_personal(&calendars, &current.calendar_id),
    };

    let conflict_path = use_memo(move || logic::conflicts_path(&draft.read(), tz));
    let conflicts = use_resource(move || {
        let path = conflict_path();
        async move {
            match path {
                Some(path) => api::get::<ConflictReport>(&path).await.map(Some),
                None => Ok(None),
            }
        }
    });

    let message_for = move |slot: Slot| -> String {
        match problem() {
            Some(found) if found.slot == slot => found.message,
            _ => String::new(),
        }
    };

    let submit = move |event: FormEvent| {
        let key = key.clone();
        async move {
            event.prevent_default();
            if busy() {
                return;
            }
            let snapshot = draft();
            let result = match editing {
                None => match logic::create_request(&snapshot, tz, personal) {
                    Ok(request) => {
                        busy.set(true);
                        api::post_once::<_, EventDetail>("/calendar/events", &request, &key).await
                    }
                    Err(found) => {
                        problem.set(Some(found));
                        return;
                    }
                },
                Some((id, version, _)) => match logic::update_request(&snapshot, tz, personal) {
                    Ok(data) => {
                        busy.set(true);
                        api::patch::<_, EventDetail>(
                            &format!("/calendar/events/{id}"),
                            &Versioned { version, data },
                        )
                        .await
                    }
                    Err(found) => {
                        problem.set(Some(found));
                        return;
                    }
                },
            };
            busy.set(false);
            match result {
                Ok(saved) => {
                    problem.set(None);
                    report_success(if editing.is_some() {
                        "일정을 수정했어요."
                    } else {
                        "일정을 등록했어요."
                    });
                    onsaved.call(saved);
                }
                Err(error) if error.code == "version_conflict" => {
                    report_error(&error);
                    if let Some((id, _, _)) = editing {
                        onreload.call(id);
                    }
                }
                Err(error) => {
                    let slot = logic::error_slot(&error.code, error.field.as_deref());
                    if slot == Slot::Form && error.code != "validation_failed" {
                        report_error(&error);
                    }
                    problem.set(Some(logic::Problem {
                        slot,
                        message: error.message(),
                    }));
                }
            }
        }
    };

    let options: Vec<(String, String)> = logic::calendar_options(&calendars, editing.is_some());
    let title = if editing.is_some() {
        "일정 수정"
    } else {
        "일정 등록"
    };
    let mut excluded: Vec<MemberId> = current.attendees.iter().map(|a| a.id).collect();
    excluded.extend(me);
    let full = current.attendees.len() >= MAX_ATTENDEES;
    let time_error = message_for(Slot::Time);

    rsx! {
        Modal { title, onclose: move |_| onclose.call(()), class: "modal-full cal-form-modal",
            form { class: "form", onsubmit: submit,
                FormError { message: message_for(Slot::Form) }
                TextField {
                    label: "제목",
                    value: current.title.clone(),
                    required: true,
                    maxlength: 200,
                    error: message_for(Slot::Title),
                    oninput: move |value| draft.write().title = value,
                }
                SelectField {
                    label: "캘린더",
                    value: current.calendar_id.clone(),
                    options,
                    disabled: editing.is_some(),
                    onchange: move |value| draft.write().calendar_id = value,
                }
                if editing.is_some() {
                    p { class: "field-hint cal-form-note", "만든 뒤에는 캘린더를 바꿀 수 없어요." }
                }
                Checkbox {
                    label: "종일",
                    checked: current.all_day,
                    onchange: move |on| {
                        let next = logic::set_all_day(&draft.peek(), on);
                        draft.set(next);
                    },
                }
                div { class: "form-row",
                    if current.all_day {
                        DateField {
                            label: "시작일",
                            value: current.start_date.clone(),
                            required: true,
                            oninput: move |value: String| {
                                let next = logic::move_start(&draft.peek(), &value);
                                draft.set(next);
                            },
                        }
                        DateField {
                            label: "종료일",
                            value: current.end_date.clone(),
                            min: current.start_date.clone(),
                            required: true,
                            error: time_error.clone(),
                            oninput: move |value| draft.write().end_date = value,
                        }
                    } else {
                        TextField {
                            label: "시작",
                            kind: "datetime-local",
                            value: current.start_at.clone(),
                            required: true,
                            oninput: move |value: String| {
                                let next = logic::move_start(&draft.peek(), &value);
                                draft.set(next);
                            },
                        }
                        TextField {
                            label: "종료",
                            kind: "datetime-local",
                            value: current.end_at.clone(),
                            required: true,
                            error: time_error.clone(),
                            oninput: move |value| draft.write().end_at = value,
                        }
                    }
                }
                TextField {
                    label: "장소",
                    value: current.location.clone(),
                    maxlength: 200,
                    error: message_for(Slot::Location),
                    oninput: move |value| draft.write().location = value,
                }
                TextArea {
                    label: "설명",
                    value: current.description.clone(),
                    rows: 4,
                    maxlength: 5000,
                    oninput: move |value| draft.write().description = value,
                }
                if !message_for(Slot::Description).is_empty() {
                    p { class: "field-error", role: "alert", "{message_for(Slot::Description)}" }
                }
                if personal {
                    section { class: "cal-attendees",
                        div { class: "cal-attendees-head",
                            span { class: "field-label", "참석자" }
                            span { class: "field-hint tnum", "{current.attendees.len()} / {MAX_ATTENDEES}" }
                        }
                        if !current.attendees.is_empty() {
                            ul { class: "chips", "aria-label": "초대할 참석자",
                                for attendee in current.attendees.iter().cloned() {
                                    li { key: "{attendee.id}", class: "chip",
                                        span { "{attendee.name}" }
                                        button {
                                            class: "icon-btn chip-remove",
                                            r#type: "button",
                                            "aria-label": "{attendee.name} 빼기",
                                            onclick: move |_| draft.write().attendees.retain(|a| a.id != attendee.id),
                                            Icon { name: IconName::Close }
                                        }
                                    }
                                }
                            }
                        }
                        if full {
                            p { class: "field-hint", "참석자는 {MAX_ATTENDEES}명까지 초대할 수 있어요." }
                        } else {
                            MemberPicker {
                                label: "참석자 추가",
                                exclude: excluded,
                                onpick: move |member: pango_api::org::MemberSummary| {
                                    draft
                                        .write()
                                        .attendees
                                        .push(Attendee {
                                            id: member.id,
                                            name: member.name,
                                        });
                                },
                            }
                        }
                        if !message_for(Slot::Attendees).is_empty() {
                            p { class: "field-error", role: "alert", "{message_for(Slot::Attendees)}" }
                        }
                        if !current.attendees.is_empty() {
                            match &*conflicts.read() {
                                Some(Ok(Some(report))) => rsx! {
                                    ConflictStrip { report: report.clone(), proposal: logic::proposal(&current, report.from, report.to, tz) }
                                },
                                Some(Err(error)) => rsx! {
                                    p { class: "field-hint", role: "status", "참석자 일정을 불러오지 못했어요. {error.message()}" }
                                },
                                _ => rsx! {
                                    p { class: "field-hint", role: "status", "참석자 일정을 확인하는 중이에요." }
                                },
                            }
                        }
                    }
                }
                div { class: "modal-actions",
                    Button { kind: ButtonKind::Grey, disabled: busy(), onclick: move |_| onclose.call(()), "취소" }
                    Button { submit: true, busy: busy(), if editing.is_some() { "저장" } else { "등록" } }
                }
            }
        }
    }
}

/// 참석자 일정 확인: 참석자마다 고른 날의 바쁜 시간을 하루 막대처럼 그린다.
/// 제목은 서버가 준 것(내가 볼 수 있는 일정)만 보이고, 나머지는 "바쁨"이다.
#[component]
fn ConflictStrip(report: ConflictReport, proposal: Option<logic::Span>) -> Element {
    let tz = use_session().zone();
    let (work_start, work_end) = use_company().work_hours();
    let rows = logic::busy_rows(&report, tz);
    let bands = logic::work_bands(report.from, report.to, work_start, work_end);
    let ticks = logic::axis_ticks(report.from, report.to);
    rsx! {
        section { class: "busy", "aria-label": "참석자 일정 확인",
            h3 { class: "sub-title", "참석자 일정 확인" }
            div {
                class: if report.from == report.to { "busy-axis" } else { "busy-axis busy-axis-days" },
                "aria-hidden": "true",
                for (index , tick) in ticks.iter().enumerate() {
                    span { key: "{index}", "{tick}" }
                }
            }
            ul { class: "busy-rows",
                for row in rows {
                    li { key: "{row.member_id}", class: "busy-row",
                        span { class: "busy-name", "{row.name}" }
                        div { class: "workday-track busy-track", role: "img", "aria-label": "{row.summary}",
                            for (index , band) in bands.iter().enumerate() {
                                span {
                                    key: "band-{index}",
                                    class: "workday-band",
                                    style: band.style(),
                                }
                            }
                            for (index , block) in row.blocks.iter().enumerate() {
                                span {
                                    key: "{index}",
                                    class: "busy-block busy-{block.kind.as_str()}",
                                    style: block.span.style(),
                                    title: "{block.label}",
                                }
                            }
                            if let Some(span) = proposal {
                                span { class: "busy-proposal", style: span.style() }
                            }
                        }
                    }
                }
            }
            p { class: "field-hint busy-legend",
                span { class: "busy-key busy-event", "aria-hidden": "true" }
                "일정"
                span { class: "busy-key busy-leave", "aria-hidden": "true" }
                "휴가"
                span { class: "busy-key busy-busy", "aria-hidden": "true" }
                "바쁨(제목 비공개)"
                span { class: "busy-key busy-key-proposal", "aria-hidden": "true" }
                "이 일정"
            }
        }
    }
}

/// 폼 값 ↔ 요청 변환, 검증, 바쁜 시간의 위치.
pub(crate) mod logic {
    use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
    use chrono_tz::Tz;
    use pango_api::calendar::{
        BusyBlock, BusyKind, CalendarDto, ConflictReport, CreateEventRequest, EventDetail,
        EventInput, UpdateEventRequest,
    };
    use pango_core::calendar_access::CalendarKind;
    use pango_core::calendar_event::{
        parse_description, parse_location, MAX_ATTENDEES, MAX_CONFLICT_DAYS, MAX_EVENT_DAYS,
    };
    use pango_core::id::{CalendarId, MemberId};
    use pango_core::text::Title;
    use pango_core::time::{MinuteOfDay, Timestamp};

    use crate::api;
    use crate::format::{self, LocalInputError};
    use crate::text;

    const WALL: &str = "%Y-%m-%dT%H:%M";

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Attendee {
        pub id: MemberId,
        pub name: String,
    }

    /// 폼에 보이는 값 그대로(날짜는 `YYYY-MM-DD`, 시각은 회사 시간대의 `datetime-local` 값).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Draft {
        pub title: String,
        /// 비었으면 내 일정(서버 기본값).
        pub calendar_id: String,
        pub all_day: bool,
        pub start_date: String,
        pub end_date: String,
        pub start_at: String,
        pub end_at: String,
        pub location: String,
        pub description: String,
        pub attendees: Vec<Attendee>,
    }

    /// 오류를 붙일 자리.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Slot {
        Title,
        Time,
        Location,
        Description,
        Attendees,
        Form,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Problem {
        pub slot: Slot,
        pub message: String,
    }

    fn problem(slot: Slot, message: impl Into<String>) -> Problem {
        Problem {
            slot,
            message: message.into(),
        }
    }

    fn wall(at: NaiveDateTime) -> String {
        at.format(WALL).to_string()
    }

    fn parse_wall(raw: &str) -> Option<NaiveDateTime> {
        NaiveDateTime::parse_from_str(raw.trim(), WALL).ok()
    }

    fn parse_day(raw: &str) -> Option<NaiveDate> {
        raw.trim().parse().ok()
    }

    fn at(date: NaiveDate, minutes: u16) -> NaiveDateTime {
        let time = NaiveTime::from_hms_opt(u32::from(minutes / 60), u32::from(minutes % 60), 0)
            .unwrap_or(NaiveTime::MIN);
        date.and_time(time)
    }

    /// 등록 폼의 기본 캘린더: 내 일정.
    pub fn default_calendar(calendars: &[CalendarDto]) -> Option<CalendarId> {
        calendars
            .iter()
            .find(|c| c.kind == CalendarKind::Personal)
            .map(|c| c.id)
    }

    /// 캘린더 선택지: 쓸 수 있는 캘린더만(수정 중에는 바꿀 수 없으므로 전부 보여 현재 값을 표시한다).
    pub fn calendar_options(calendars: &[CalendarDto], editing: bool) -> Vec<(String, String)> {
        calendars
            .iter()
            .filter(|c| editing || c.can_write)
            .map(|c| (c.id.to_string(), c.name.clone()))
            .collect()
    }

    /// 고른 캘린더가 내 일정인지(참석자는 내 일정에만 둘 수 있다). 비었으면 내 일정이다.
    pub fn is_personal(calendars: &[CalendarDto], selected: &str) -> bool {
        selected.trim().is_empty()
            || calendars
                .iter()
                .find(|c| c.id.to_string() == selected)
                .is_some_and(|c| c.kind == CalendarKind::Personal)
    }

    /// 새 일정의 처음 값: 오늘이면 다음 정시부터 한 시간, 다른 날이면 업무 시작부터 한 시간.
    /// `now`는 회사 시간대의 지금(그날이 오늘일 때만 준다).
    pub fn new_draft(
        date: NaiveDate,
        now: Option<NaiveDateTime>,
        work_start: MinuteOfDay,
        calendar: Option<CalendarId>,
    ) -> Draft {
        let start = match now {
            Some(now) => {
                let next = u16::try_from(now.hour() + 1).unwrap_or(23).min(23);
                at(date, next * 60)
            }
            None => at(date, work_start.minutes()),
        };
        let end = start + Duration::hours(1);
        Draft {
            title: String::new(),
            calendar_id: calendar.map(|id| id.to_string()).unwrap_or_default(),
            all_day: false,
            start_date: date.to_string(),
            end_date: date.to_string(),
            start_at: wall(start),
            end_at: wall(end),
            location: String::new(),
            description: String::new(),
            attendees: Vec::new(),
        }
    }

    /// 수정 폼의 처음 값. 종일 일정이면 시각 칸은 첫날 09~10시로 채워 둔다(종일을 끌 때 쓴다).
    pub fn draft_from(detail: &EventDetail, tz: Tz) -> Draft {
        let summary = &detail.summary;
        let (start_date, end_date, start_at, end_at) = match (
            summary.all_day,
            summary.start_date,
            summary.start_at,
            summary.end_at,
        ) {
            (true, Some(start), _, _) => {
                let end = summary.end_date.unwrap_or(start);
                (
                    start,
                    end,
                    wall(at(start, 9 * 60)),
                    wall(at(start, 10 * 60)),
                )
            }
            (_, _, Some(start), Some(end)) => {
                let first = format::local_date(start, tz);
                (
                    first,
                    format::local_date(end, tz).max(first),
                    format::local_input(start, tz),
                    format::local_input(end, tz),
                )
            }
            _ => (NaiveDate::MIN, NaiveDate::MIN, String::new(), String::new()),
        };
        Draft {
            title: summary.title.clone(),
            calendar_id: summary.calendar_id.to_string(),
            all_day: summary.all_day,
            start_date: start_date.to_string(),
            end_date: end_date.to_string(),
            start_at,
            end_at,
            location: summary.location.clone().unwrap_or_default(),
            description: detail.description.clone().unwrap_or_default(),
            attendees: detail
                .attendees
                .iter()
                .map(|a| Attendee {
                    id: a.member_id,
                    name: a.name.clone(),
                })
                .collect(),
        }
    }

    /// 종일을 켜고 끈다. 켜면 시각 칸의 날짜를, 끄면 날짜 칸에 원래 시각을 붙인다.
    pub fn set_all_day(draft: &Draft, on: bool) -> Draft {
        let mut next = draft.clone();
        next.all_day = on;
        if on {
            if let Some(start) = parse_wall(&draft.start_at) {
                next.start_date = start.date().to_string();
                // 끝은 포함하지 않으므로 자정에 끝나면 전날까지다.
                let last = parse_wall(&draft.end_at)
                    .filter(|end| *end > start)
                    .map_or(start.date(), |end| (end - Duration::minutes(1)).date());
                next.end_date = last.max(start.date()).to_string();
            }
        } else if let Some(first) = parse_day(&draft.start_date) {
            let last = parse_day(&draft.end_date).unwrap_or(first).max(first);
            let time_of = |raw: &str, fallback: u16| {
                parse_wall(raw).map_or(at(first, fallback).time(), |t| t.time())
            };
            let start = first.and_time(time_of(&draft.start_at, 9 * 60));
            let mut end = last.and_time(time_of(&draft.end_at, 10 * 60));
            if end <= start {
                end = start + Duration::hours(1);
            }
            next.start_at = wall(start);
            next.end_at = wall(end);
        }
        next
    }

    /// 시작을 옮기면 길이를 지킨 채 끝도 옮긴다. 값을 읽을 수 없으면 시작만 바꾼다.
    pub fn move_start(draft: &Draft, value: &str) -> Draft {
        let mut next = draft.clone();
        if draft.all_day {
            next.start_date = value.to_owned();
            if let (Some(old), Some(end), Some(new)) = (
                parse_day(&draft.start_date),
                parse_day(&draft.end_date),
                parse_day(value),
            ) {
                let days = (end - old).num_days().max(0);
                if let Some(moved) = new.checked_add_signed(Duration::days(days)) {
                    next.end_date = moved.to_string();
                }
            }
        } else {
            next.start_at = value.to_owned();
            if let (Some(old), Some(end), Some(new)) = (
                parse_wall(&draft.start_at),
                parse_wall(&draft.end_at),
                parse_wall(value),
            ) {
                let length = (end - old).max(Duration::zero());
                if let Some(moved) = new.checked_add_signed(length) {
                    next.end_at = wall(moved);
                }
            }
        }
        next
    }

    fn time_problem(which: &str, error: LocalInputError) -> Problem {
        let message = match error {
            LocalInputError::Empty => format!("{which} 시각을 입력해 주세요."),
            LocalInputError::Invalid => format!("{which} 시각의 형식이 올바르지 않아요."),
            LocalInputError::Nonexistent => {
                format!("{which} 시각은 서머타임 전환으로 회사 시간대에 없는 시각이에요.")
            }
        };
        problem(Slot::Time, message)
    }

    /// 폼 값을 서버가 받는 일정 내용으로. 서버와 같은 규칙(제목 200자, 장소 200자, 설명 5,000자,
    /// 끝 > 시작, 31일 이하)을 먼저 확인한다. 최종 판정은 서버가 한다.
    pub fn event_input(draft: &Draft, tz: Tz) -> Result<EventInput, Problem> {
        let title = Title::parse(&draft.title)
            .map_err(|e| problem(Slot::Title, text::domain_error_message(&e)))?
            .into_string();
        let location = parse_location(format::non_empty(&draft.location))
            .map_err(|e| problem(Slot::Location, text::domain_error_message(&e)))?;
        let description = parse_description(format::non_empty(&draft.description))
            .map_err(|e| problem(Slot::Description, text::domain_error_message(&e)))?;
        let too_long = || {
            problem(
                Slot::Time,
                format!("일정은 {MAX_EVENT_DAYS}일까지 잡을 수 있어요."),
            )
        };
        if draft.all_day {
            let start = parse_day(&draft.start_date)
                .ok_or_else(|| problem(Slot::Time, "시작일을 입력해 주세요."))?;
            let end = match format::non_empty(&draft.end_date) {
                None => start,
                Some(raw) => parse_day(raw)
                    .ok_or_else(|| problem(Slot::Time, "종료일의 형식이 올바르지 않아요."))?,
            };
            if end < start {
                return Err(problem(Slot::Time, "종료일은 시작일보다 이를 수 없어요."));
            }
            if (end - start).num_days() + 1 > MAX_EVENT_DAYS {
                return Err(too_long());
            }
            return Ok(EventInput {
                title,
                all_day: true,
                start_date: Some(start),
                end_date: Some(end),
                start_at: None,
                end_at: None,
                location,
                description,
            });
        }
        let start =
            format::parse_local_input(&draft.start_at, tz).map_err(|e| time_problem("시작", e))?;
        let end =
            format::parse_local_input(&draft.end_at, tz).map_err(|e| time_problem("종료", e))?;
        if end <= start {
            return Err(problem(
                Slot::Time,
                "종료 시각은 시작 시각보다 늦어야 해요.",
            ));
        }
        if end.since(start) > Duration::days(MAX_EVENT_DAYS) {
            return Err(too_long());
        }
        Ok(EventInput {
            title,
            all_day: false,
            start_date: None,
            end_date: None,
            start_at: Some(start),
            end_at: Some(end),
            location,
            description,
        })
    }

    fn attendee_ids(draft: &Draft) -> Result<Vec<MemberId>, Problem> {
        if draft.attendees.len() > MAX_ATTENDEES {
            return Err(problem(
                Slot::Attendees,
                format!("참석자는 {MAX_ATTENDEES}명까지 초대할 수 있어요."),
            ));
        }
        Ok(draft.attendees.iter().map(|a| a.id).collect())
    }

    /// `POST /calendar/events` 본문. 내 일정이 아니면 참석자를 보내지 않는다(서버가 받지 않는다).
    pub fn create_request(
        draft: &Draft,
        tz: Tz,
        personal: bool,
    ) -> Result<CreateEventRequest, Problem> {
        let event = event_input(draft, tz)?;
        let attendee_ids = if personal {
            attendee_ids(draft)?
        } else {
            Vec::new()
        };
        Ok(CreateEventRequest {
            calendar_id: CalendarId::parse(draft.calendar_id.trim()).ok(),
            event,
            attendee_ids,
        })
    }

    /// `PATCH /calendar/events/{id}` 본문. 내 일정이면 참석자 목록을 통째로 보낸다(빠진 사람은 제외된다).
    pub fn update_request(
        draft: &Draft,
        tz: Tz,
        personal: bool,
    ) -> Result<UpdateEventRequest, Problem> {
        let event = event_input(draft, tz)?;
        let attendee_ids = if personal {
            Some(attendee_ids(draft)?)
        } else {
            None
        };
        Ok(UpdateEventRequest {
            event,
            attendee_ids,
        })
    }

    /// 서버 오류를 붙일 자리.
    pub fn error_slot(code: &str, field: Option<&str>) -> Slot {
        match (code, field) {
            ("validation_failed", Some("title")) => Slot::Title,
            ("validation_failed", Some("start_date" | "end_date" | "start_at" | "end_at")) => {
                Slot::Time
            }
            ("validation_failed", Some("location")) => Slot::Location,
            ("validation_failed", Some("description")) => Slot::Description,
            ("validation_failed", Some("attendee_ids")) | ("member_inactive", _) => Slot::Attendees,
            _ => Slot::Form,
        }
    }

    /// 참석자 일정을 볼 날: 일정이 걸친 날(서버 한도 7일까지). 값을 읽을 수 없으면 `None`.
    pub fn conflict_days(draft: &Draft, tz: Tz) -> Option<(NaiveDate, NaiveDate)> {
        let (first, last) = if draft.all_day {
            let first = parse_day(&draft.start_date)?;
            (first, parse_day(&draft.end_date).unwrap_or(first))
        } else {
            let start = format::parse_local_input(&draft.start_at, tz).ok()?;
            let end = format::parse_local_input(&draft.end_at, tz).ok()?;
            let last = if end > start {
                format::local_date(
                    Timestamp::from_millis(end.as_millis().saturating_sub(1)).unwrap_or(end),
                    tz,
                )
            } else {
                format::local_date(start, tz)
            };
            (format::local_date(start, tz), last)
        };
        let last = last.max(first);
        let cap = first.checked_add_signed(Duration::days(MAX_CONFLICT_DAYS - 1))?;
        Some((first, last.min(cap)))
    }

    /// `GET /calendar/conflicts` 경로. 참석자가 없거나 날을 모르면 `None`(읽지 않는다).
    pub fn conflicts_path(draft: &Draft, tz: Tz) -> Option<String> {
        if draft.attendees.is_empty() {
            return None;
        }
        let (from, to) = conflict_days(draft, tz)?;
        let ids = draft
            .attendees
            .iter()
            .take(MAX_ATTENDEES)
            .map(|a| a.id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        Some(format!(
            "/calendar/conflicts{}",
            api::query_string(&[
                ("member_ids", ids),
                ("from", from.to_string()),
                ("to", to.to_string()),
            ])
        ))
    }

    // ── 바쁜 시간 막대 ──

    /// 막대 위의 구간(왼쪽 위치와 폭, 0~100%).
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub struct Span {
        pub left: f64,
        pub width: f64,
    }

    impl Span {
        pub fn style(self) -> String {
            format!("left: {:.2}%; width: {:.2}%", self.left, self.width)
        }
    }

    /// 축(`from` 0시 ~ `to` 다음 날 0시, 회사 시간대) 위의 구간. 축 밖이거나 길이가 없으면 `None`.
    fn axis_span(
        from: NaiveDate,
        to: NaiveDate,
        start: NaiveDateTime,
        end: NaiveDateTime,
    ) -> Option<Span> {
        let days = (to - from).num_days() + 1;
        if days <= 0 {
            return None;
        }
        let total = (days * 24 * 60) as f64;
        let origin = from.and_time(NaiveTime::MIN);
        let position = |at: NaiveDateTime| {
            ((at - origin).num_minutes() as f64 / total * 100.0).clamp(0.0, 100.0)
        };
        let (left, right) = (position(start), position(end));
        (right > left).then_some(Span {
            left,
            width: right - left,
        })
    }

    fn local(at: Timestamp, tz: Tz) -> NaiveDateTime {
        at.to_utc().with_timezone(&tz).naive_local()
    }

    fn next_day(date: NaiveDate) -> NaiveDateTime {
        date.succ_opt().unwrap_or(date).and_time(NaiveTime::MIN)
    }

    fn block_span(block: &BusyBlock, from: NaiveDate, to: NaiveDate, tz: Tz) -> Option<Span> {
        if block.all_day {
            let first = block.start_date?;
            let last = block.end_date.unwrap_or(first);
            axis_span(from, to, first.and_time(NaiveTime::MIN), next_day(last))
        } else {
            axis_span(
                from,
                to,
                local(block.start_at?, tz),
                local(block.end_at?, tz),
            )
        }
    }

    fn block_label(block: &BusyBlock) -> String {
        match block.kind {
            BusyKind::Event => block.title.clone().unwrap_or_else(|| "일정".to_owned()),
            BusyKind::Leave => match block.leave_unit {
                Some(unit) => format!("휴가({})", text::leave_unit_label(unit)),
                None => "휴가".to_owned(),
            },
            BusyKind::Busy => "바쁨".to_owned(),
        }
    }

    fn block_time(block: &BusyBlock, tz: Tz) -> String {
        match (block.all_day, block.start_at, block.end_at) {
            (false, Some(start), Some(end)) => {
                format!(
                    "{}–{}",
                    format::time_hm(start, tz),
                    format::time_hm(end, tz)
                )
            }
            _ => "종일".to_owned(),
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct BusySpan {
        pub span: Span,
        pub kind: BusyKind,
        /// 제목(볼 수 있을 때만) 또는 `휴가(오전 반차)`·`바쁨`, 시각.
        pub label: String,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct BusyRow {
        pub member_id: MemberId,
        pub name: String,
        pub blocks: Vec<BusySpan>,
        /// 막대를 말로 풀어 쓴 것(접근성 이름).
        pub summary: String,
    }

    pub fn busy_rows(report: &ConflictReport, tz: Tz) -> Vec<BusyRow> {
        report
            .members
            .iter()
            .map(|member| {
                let blocks: Vec<BusySpan> = member
                    .busy
                    .iter()
                    .filter_map(|block| {
                        let span = block_span(block, report.from, report.to, tz)?;
                        Some(BusySpan {
                            span,
                            kind: block.kind,
                            label: format!("{} {}", block_time(block, tz), block_label(block)),
                        })
                    })
                    .collect();
                let summary = if blocks.is_empty() {
                    format!("{}: 겹치는 일정 없음", member.member_name)
                } else {
                    let list = blocks
                        .iter()
                        .map(|b| b.label.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}: {list}", member.member_name)
                };
                BusyRow {
                    member_id: member.member_id,
                    name: member.member_name.clone(),
                    blocks,
                    summary,
                }
            })
            .collect()
    }

    /// 지금 폼의 일정이 축 위에 놓이는 자리(테두리로 겹쳐 그린다).
    pub fn proposal(draft: &Draft, from: NaiveDate, to: NaiveDate, tz: Tz) -> Option<Span> {
        if draft.all_day {
            let first = parse_day(&draft.start_date)?;
            let last = parse_day(&draft.end_date).unwrap_or(first).max(first);
            axis_span(from, to, first.and_time(NaiveTime::MIN), next_day(last))
        } else {
            let start = format::parse_local_input(&draft.start_at, tz).ok()?;
            let end = format::parse_local_input(&draft.end_at, tz).ok()?;
            axis_span(from, to, local(start, tz), local(end, tz))
        }
    }

    /// 날마다의 회사 근무 시간대(옅은 띠). 밤을 넘기는 근무는 다음 날로 이어 그린다.
    pub fn work_bands(
        from: NaiveDate,
        to: NaiveDate,
        work_start: MinuteOfDay,
        work_end: MinuteOfDay,
    ) -> Vec<Span> {
        from.iter_days()
            .take_while(|day| *day <= to)
            .filter_map(|day| {
                let start = at(day, work_start.minutes());
                let mut end = at(day, work_end.minutes());
                if end <= start {
                    end += Duration::days(1);
                }
                axis_span(from, to, start, end)
            })
            .collect()
    }

    /// 축 아래 글자. 하루면 6시간 간격(`00`~`24`), 여러 날이면 날짜(`11/20`).
    pub fn axis_ticks(from: NaiveDate, to: NaiveDate) -> Vec<String> {
        use chrono::Datelike;
        if from >= to {
            return ["00", "06", "12", "18", "24"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
        }
        from.iter_days()
            .take_while(|day| *day <= to)
            .map(|day| format!("{}/{}", day.month(), day.day()))
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pango_api::calendar::{EventAttendee, EventSummary, MemberBusy};
        use pango_core::calendar_access::EventRole;
        use pango_core::calendar_event::AttendeeResponse;
        use pango_core::id::CalendarEventId;
        use pango_core::leave::LeaveUnit;

        fn d(value: &str) -> NaiveDate {
            value.parse().expect("date")
        }

        fn seoul() -> Tz {
            chrono_tz::Asia::Seoul
        }

        fn kst(value: &str) -> Timestamp {
            format::parse_local_input(value, seoul()).expect("kst")
        }

        fn hm(value: &str) -> MinuteOfDay {
            MinuteOfDay::parse(value).expect("time")
        }

        fn wall_at(value: &str) -> NaiveDateTime {
            parse_wall(value).expect("wall")
        }

        fn timed_draft(start: &str, end: &str) -> Draft {
            Draft {
                title: "회의".into(),
                start_at: start.into(),
                end_at: end.into(),
                ..new_draft(d("2026-11-20"), None, hm("09:00"), None)
            }
        }

        fn person(name: &str) -> Attendee {
            Attendee {
                id: MemberId::new(),
                name: name.into(),
            }
        }

        fn calendar(kind: CalendarKind, can_write: bool) -> CalendarDto {
            CalendarDto {
                id: CalendarId::new(),
                kind,
                name: text::calendar_kind_label(kind).into(),
                department_id: None,
                can_write,
            }
        }

        #[test]
        fn new_drafts_start_at_the_next_hour_today_or_at_work_start() {
            let draft = new_draft(d("2026-11-25"), None, hm("08:30"), None);
            assert_eq!(
                (draft.start_at.as_str(), draft.end_at.as_str()),
                ("2026-11-25T08:30", "2026-11-25T09:30")
            );
            assert_eq!(draft.calendar_id, "");
            let today = new_draft(
                d("2026-11-20"),
                Some(wall_at("2026-11-20T14:42")),
                hm("09:00"),
                None,
            );
            assert_eq!(
                (today.start_at.as_str(), today.end_at.as_str()),
                ("2026-11-20T15:00", "2026-11-20T16:00")
            );
            // 밤늦게 열면 23시부터(자정을 넘는 한 시간).
            let late = new_draft(
                d("2026-11-20"),
                Some(wall_at("2026-11-20T23:20")),
                hm("09:00"),
                None,
            );
            assert_eq!(
                (late.start_at.as_str(), late.end_at.as_str()),
                ("2026-11-20T23:00", "2026-11-21T00:00")
            );
        }

        #[test]
        fn moving_the_start_keeps_the_duration() {
            let draft = timed_draft("2026-11-20T09:00", "2026-11-20T10:30");
            let moved = move_start(&draft, "2026-11-20T23:00");
            assert_eq!(
                moved.end_at, "2026-11-21T00:30",
                "자정을 넘어도 길이는 그대로"
            );
            let broken = move_start(&draft, "2026-11-2");
            assert_eq!(
                (broken.start_at.as_str(), broken.end_at.as_str()),
                ("2026-11-2", "2026-11-20T10:30")
            );
            let all_day = Draft {
                all_day: true,
                start_date: "2026-11-30".into(),
                end_date: "2026-12-02".into(),
                ..draft
            };
            let moved = move_start(&all_day, "2026-12-30");
            assert_eq!(
                (moved.start_date.as_str(), moved.end_date.as_str()),
                ("2026-12-30", "2027-01-01")
            );
        }

        #[test]
        fn all_day_toggle_converts_between_dates_and_times() {
            let draft = timed_draft("2026-11-20T22:00", "2026-11-21T00:00");
            let on = set_all_day(&draft, true);
            assert!(on.all_day);
            assert_eq!(
                (on.start_date.as_str(), on.end_date.as_str()),
                ("2026-11-20", "2026-11-20"),
                "자정에 끝나면 그날까지"
            );
            let overnight = set_all_day(&timed_draft("2026-11-20T23:00", "2026-11-22T01:00"), true);
            assert_eq!(overnight.end_date, "2026-11-22");
            // 끄면 원래 시각을 날짜에 붙인다.
            let back = set_all_day(&on, false);
            assert_eq!(
                (back.start_at.as_str(), back.end_at.as_str()),
                ("2026-11-20T22:00", "2026-11-20T23:00"),
                "끝이 시작보다 이르면 한 시간"
            );
            let fresh = Draft {
                all_day: true,
                start_date: "2026-11-24".into(),
                end_date: "2026-11-25".into(),
                start_at: String::new(),
                end_at: String::new(),
                ..draft
            };
            let off = set_all_day(&fresh, false);
            assert_eq!(
                (off.start_at.as_str(), off.end_at.as_str()),
                ("2026-11-24T09:00", "2026-11-25T10:00")
            );
        }

        #[test]
        fn timed_drafts_become_company_time_instants() {
            let tz = seoul();
            let input =
                event_input(&timed_draft("2026-11-20T14:00", "2026-11-20T15:00"), tz).expect("ok");
            assert!(!input.all_day);
            assert_eq!(input.start_at, Some(kst("2026-11-20T14:00")));
            assert_eq!(input.end_at, Some(kst("2026-11-20T15:00")));
            assert_eq!((input.start_date, input.end_date), (None, None));
            assert_eq!(
                (input.location, input.description),
                (None, None),
                "빈 값은 보내지 않는다"
            );
            // 31일 정확히는 되고, 넘으면 안 된다.
            assert!(event_input(&timed_draft("2026-11-01T00:00", "2026-12-02T00:00"), tz).is_ok());
            let long = event_input(&timed_draft("2026-11-01T00:00", "2026-12-02T00:01"), tz);
            assert_eq!(long.map_err(|p| p.slot), Err(Slot::Time));
            let reversed = event_input(&timed_draft("2026-11-20T15:00", "2026-11-20T15:00"), tz);
            assert_eq!(reversed.map_err(|p| p.slot), Err(Slot::Time));
            let empty = event_input(&timed_draft("", "2026-11-20T15:00"), tz).expect_err("empty");
            assert_eq!(empty.message, "시작 시각을 입력해 주세요.");
        }

        #[test]
        fn all_day_drafts_send_dates_only() {
            let tz = seoul();
            let draft = Draft {
                all_day: true,
                start_date: "2026-11-20".into(),
                end_date: "".into(),
                location: "  본사 3층 ".into(),
                description: "첫 줄\n둘째 줄".into(),
                ..timed_draft("2026-11-20T09:00", "2026-11-20T10:00")
            };
            let input = event_input(&draft, tz).expect("ok");
            assert_eq!(
                (input.start_date, input.end_date),
                (Some(d("2026-11-20")), Some(d("2026-11-20")))
            );
            assert_eq!((input.start_at, input.end_at), (None, None));
            assert_eq!(input.location.as_deref(), Some("본사 3층"));
            assert_eq!(input.description.as_deref(), Some("첫 줄\n둘째 줄"));
            let month = Draft {
                end_date: "2026-12-20".into(),
                ..draft.clone()
            };
            assert_eq!(
                event_input(&month, tz).expect("31일").end_date,
                Some(d("2026-12-20"))
            );
            let too_long = Draft {
                end_date: "2026-12-21".into(),
                ..draft.clone()
            };
            assert_eq!(
                event_input(&too_long, tz).map_err(|p| p.slot),
                Err(Slot::Time)
            );
            let backwards = Draft {
                end_date: "2026-11-19".into(),
                ..draft
            };
            assert_eq!(
                event_input(&backwards, tz).map_err(|p| p.slot),
                Err(Slot::Time)
            );
        }

        #[test]
        fn text_fields_follow_the_server_rules() {
            let tz = seoul();
            let base = timed_draft("2026-11-20T09:00", "2026-11-20T10:00");
            let blank = Draft {
                title: "   ".into(),
                ..base.clone()
            };
            assert_eq!(
                event_input(&blank, tz).map_err(|p| p.slot),
                Err(Slot::Title)
            );
            let long_title = Draft {
                title: "가".repeat(201),
                ..base.clone()
            };
            assert_eq!(
                event_input(&long_title, tz).map_err(|p| p.slot),
                Err(Slot::Title)
            );
            let long_place = Draft {
                location: "a".repeat(201),
                ..base.clone()
            };
            assert_eq!(
                event_input(&long_place, tz).map_err(|p| p.slot),
                Err(Slot::Location)
            );
            let two_lines = Draft {
                location: "a\nb".into(),
                ..base.clone()
            };
            assert_eq!(
                event_input(&two_lines, tz).map_err(|p| p.slot),
                Err(Slot::Location)
            );
            let long_text = Draft {
                description: "a".repeat(5001),
                ..base
            };
            assert_eq!(
                event_input(&long_text, tz).map_err(|p| p.slot),
                Err(Slot::Description)
            );
        }

        #[test]
        fn attendees_go_only_to_personal_events() {
            let tz = seoul();
            let mut draft = timed_draft("2026-11-20T09:00", "2026-11-20T10:00");
            draft.attendees = vec![person("가"), person("나")];
            let company = calendar(CalendarKind::Company, true);
            draft.calendar_id = company.id.to_string();
            let shared = create_request(&draft, tz, false).expect("ok");
            assert_eq!(shared.calendar_id, Some(company.id));
            assert!(shared.attendee_ids.is_empty());
            assert_eq!(
                update_request(&draft, tz, false).expect("ok").attendee_ids,
                None
            );
            let mine = create_request(&draft, tz, true).expect("ok");
            assert_eq!(mine.attendee_ids.len(), 2);
            assert_eq!(
                update_request(&draft, tz, true)
                    .expect("ok")
                    .attendee_ids
                    .map(|ids| ids.len()),
                Some(2)
            );
            draft.calendar_id = String::new();
            assert_eq!(
                create_request(&draft, tz, true).expect("ok").calendar_id,
                None,
                "비면 내 일정"
            );
            draft.attendees = (0..=MAX_ATTENDEES)
                .map(|i| person(&i.to_string()))
                .collect();
            assert_eq!(
                create_request(&draft, tz, true).map_err(|p| p.slot),
                Err(Slot::Attendees)
            );
            assert!(
                create_request(&draft, tz, false).is_ok(),
                "공유 캘린더는 참석자를 보내지 않는다"
            );
        }

        #[test]
        fn calendar_choices_and_personal_check() {
            let mine = calendar(CalendarKind::Personal, true);
            let company = calendar(CalendarKind::Company, false);
            let team = calendar(CalendarKind::Department, true);
            let all = vec![mine.clone(), company.clone(), team.clone()];
            assert_eq!(default_calendar(&all), Some(mine.id));
            let ids: Vec<String> = calendar_options(&all, false)
                .into_iter()
                .map(|(id, _)| id)
                .collect();
            assert_eq!(ids, [mine.id.to_string(), team.id.to_string()]);
            assert_eq!(calendar_options(&all, true).len(), 3);
            assert!(is_personal(&all, ""));
            assert!(is_personal(&all, &mine.id.to_string()));
            assert!(!is_personal(&all, &team.id.to_string()));
            assert!(!is_personal(&all, "not-an-id"));
        }

        #[test]
        fn server_errors_land_on_their_fields() {
            assert_eq!(error_slot("validation_failed", Some("title")), Slot::Title);
            for field in ["start_date", "end_date", "start_at", "end_at"] {
                assert_eq!(
                    error_slot("validation_failed", Some(field)),
                    Slot::Time,
                    "{field}"
                );
            }
            assert_eq!(
                error_slot("validation_failed", Some("location")),
                Slot::Location
            );
            assert_eq!(
                error_slot("validation_failed", Some("description")),
                Slot::Description
            );
            assert_eq!(
                error_slot("validation_failed", Some("attendee_ids")),
                Slot::Attendees
            );
            assert_eq!(error_slot("member_inactive", None), Slot::Attendees);
            assert_eq!(error_slot("forbidden", None), Slot::Form);
            assert_eq!(error_slot("not_found", None), Slot::Form);
        }

        fn detail(summary: EventSummary) -> EventDetail {
            EventDetail {
                summary,
                description: Some("메모".into()),
                owner_id: MemberId::new(),
                owner_name: "나".into(),
                attendees: vec![EventAttendee {
                    member_id: MemberId::new(),
                    name: "가".into(),
                    response: AttendeeResponse::Pending,
                    responded_at: None,
                }],
                created_at: kst("2026-11-01T09:00"),
                updated_at: kst("2026-11-01T09:00"),
            }
        }

        fn summary(all_day: bool) -> EventSummary {
            EventSummary {
                id: CalendarEventId::new(),
                calendar_id: CalendarId::new(),
                calendar_kind: CalendarKind::Personal,
                title: "회의".into(),
                all_day,
                start_date: all_day.then(|| d("2026-11-20")),
                end_date: all_day.then(|| d("2026-11-21")),
                start_at: (!all_day).then(|| kst("2026-11-20T23:00")),
                end_at: (!all_day).then(|| kst("2026-11-21T01:00")),
                location: Some("회의실".into()),
                my_role: EventRole::Owner,
                my_response: None,
                attendee_count: 1,
                version: 3,
            }
        }

        #[test]
        fn edit_drafts_round_trip_to_the_same_request() {
            let tz = seoul();
            let timed = detail(summary(false));
            let draft = draft_from(&timed, tz);
            assert_eq!(
                (draft.start_at.as_str(), draft.end_at.as_str()),
                ("2026-11-20T23:00", "2026-11-21T01:00")
            );
            assert_eq!(draft.attendees.len(), 1);
            let request = update_request(&draft, tz, true).expect("ok");
            assert_eq!(request.event.start_at, timed.summary.start_at);
            assert_eq!(request.event.end_at, timed.summary.end_at);
            assert_eq!(request.event.location.as_deref(), Some("회의실"));
            assert_eq!(request.event.description.as_deref(), Some("메모"));
            let whole = draft_from(&detail(summary(true)), tz);
            assert!(whole.all_day);
            assert_eq!(
                (whole.start_date.as_str(), whole.end_date.as_str()),
                ("2026-11-20", "2026-11-21")
            );
            let request = update_request(&whole, tz, true).expect("ok");
            assert_eq!(request.event.end_date, Some(d("2026-11-21")));
        }

        #[test]
        fn conflicts_are_read_for_the_event_days_only_with_attendees() {
            let tz = seoul();
            let mut draft = timed_draft("2026-11-20T23:00", "2026-11-21T01:00");
            assert_eq!(
                conflicts_path(&draft, tz),
                None,
                "참석자가 없으면 읽지 않는다"
            );
            let a = person("가");
            draft.attendees = vec![a.clone()];
            assert_eq!(
                conflict_days(&draft, tz),
                Some((d("2026-11-20"), d("2026-11-21")))
            );
            assert_eq!(
                conflicts_path(&draft, tz),
                Some(format!(
                    "/calendar/conflicts?member_ids={}&from=2026-11-20&to=2026-11-21",
                    a.id
                ))
            );
            let midnight = Draft {
                end_at: "2026-11-21T00:00".into(),
                ..draft.clone()
            };
            assert_eq!(
                conflict_days(&midnight, tz),
                Some((d("2026-11-20"), d("2026-11-20")))
            );
            let long = Draft {
                all_day: true,
                start_date: "2026-11-01".into(),
                end_date: "2026-11-30".into(),
                ..draft.clone()
            };
            assert_eq!(
                conflict_days(&long, tz),
                Some((d("2026-11-01"), d("2026-11-07"))),
                "7일까지만"
            );
            let broken = Draft {
                start_at: "x".into(),
                ..draft
            };
            assert_eq!(conflicts_path(&broken, tz), None);
        }

        fn block(kind: BusyKind, start: &str, end: &str, title: Option<&str>) -> BusyBlock {
            BusyBlock {
                kind,
                all_day: false,
                start_date: None,
                end_date: None,
                start_at: Some(kst(start)),
                end_at: Some(kst(end)),
                event_id: None,
                title: title.map(str::to_owned),
                leave_unit: None,
            }
        }

        #[test]
        fn busy_blocks_are_placed_on_the_day_axis_with_titles_only_when_given() {
            let tz = seoul();
            let report = ConflictReport {
                from: d("2026-11-20"),
                to: d("2026-11-20"),
                members: vec![
                    MemberBusy {
                        member_id: MemberId::new(),
                        member_name: "가".into(),
                        busy: vec![
                            block(
                                BusyKind::Event,
                                "2026-11-20T06:00",
                                "2026-11-20T12:00",
                                Some("주간 회의"),
                            ),
                            block(BusyKind::Busy, "2026-11-20T18:00", "2026-11-21T02:00", None),
                        ],
                    },
                    MemberBusy {
                        member_id: MemberId::new(),
                        member_name: "나".into(),
                        busy: vec![BusyBlock {
                            kind: BusyKind::Leave,
                            all_day: true,
                            start_date: Some(d("2026-11-19")),
                            end_date: Some(d("2026-11-20")),
                            start_at: None,
                            end_at: None,
                            event_id: None,
                            title: None,
                            leave_unit: Some(LeaveUnit::FullDay),
                        }],
                    },
                    MemberBusy {
                        member_id: MemberId::new(),
                        member_name: "다".into(),
                        busy: vec![],
                    },
                ],
            };
            let rows = busy_rows(&report, tz);
            assert_eq!(
                rows[0].blocks[0].span,
                Span {
                    left: 25.0,
                    width: 25.0
                }
            );
            assert_eq!(rows[0].blocks[0].label, "06:00–12:00 주간 회의");
            // 축 밖(다음 날)은 잘린다.
            assert_eq!(
                rows[0].blocks[1].span,
                Span {
                    left: 75.0,
                    width: 25.0
                }
            );
            assert_eq!(rows[0].blocks[1].label, "18:00–02:00 바쁨");
            assert_eq!(
                rows[1].blocks[0].span,
                Span {
                    left: 0.0,
                    width: 100.0
                }
            );
            assert_eq!(rows[1].blocks[0].label, "종일 휴가(종일)");
            assert_eq!(rows[2].summary, "다: 겹치는 일정 없음");
            assert!(rows[0].summary.starts_with("가: 06:00–12:00 주간 회의"));
        }

        #[test]
        fn proposal_bands_and_ticks_share_the_axis() {
            let tz = seoul();
            let draft = timed_draft("2026-11-20T12:00", "2026-11-20T18:00");
            assert_eq!(
                proposal(&draft, d("2026-11-20"), d("2026-11-20"), tz),
                Some(Span {
                    left: 50.0,
                    width: 25.0
                })
            );
            assert_eq!(
                proposal(&draft, d("2026-11-20"), d("2026-11-21"), tz),
                Some(Span {
                    left: 25.0,
                    width: 12.5
                })
            );
            assert_eq!(proposal(&draft, d("2026-11-21"), d("2026-11-21"), tz), None);
            let bands = work_bands(d("2026-11-20"), d("2026-11-21"), hm("09:00"), hm("18:00"));
            assert_eq!(bands.len(), 2);
            assert_eq!(
                bands[0],
                Span {
                    left: 18.75,
                    width: 18.75
                }
            );
            let night = work_bands(d("2026-11-20"), d("2026-11-20"), hm("22:00"), hm("06:00"));
            assert_eq!(night.len(), 1, "다음 날로 넘친 부분은 잘린다");
            assert_eq!(
                axis_ticks(d("2026-11-20"), d("2026-11-20")),
                ["00", "06", "12", "18", "24"]
            );
            assert_eq!(
                axis_ticks(d("2026-11-30"), d("2026-12-01")),
                ["11/30", "12/1"]
            );
        }
    }
}
