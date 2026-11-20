//! 캘린더: 왼쪽에 등록·작은 달력·캘린더 목록, 오른쪽에 월·주·목록 보기.
//!
//! 보이는 기간만 `GET /calendar/view`로 읽는다(월: 화면에 보이는 꽉 찬 주, 주: 7일(모바일 3일),
//! 목록: 그 달). 배치(막대의 줄, 시간 칸의 열)는 모두 [`logic`]의 순수 함수가 정한다.

use std::collections::BTreeSet;

use chrono::{Datelike, NaiveDate};
use chrono_tz::Tz;
use dioxus::prelude::*;
use pango_api::calendar::{CalendarDto, CalendarView, EventDetail, EventSummary, RespondRequest};
use pango_api::Empty;
use pango_core::calendar::WorkWeek;
use pango_core::calendar_access::EventRole;
use pango_core::calendar_event::AttendeeResponse;
use pango_core::id::{CalendarEventId, CalendarId};

use crate::api;
use crate::calendar::YearMonth;
use crate::company::use_company;
use crate::format;
use crate::pages::calendar_form::{EventForm, FormMode};
use crate::pages::leave::logic::DayEntry;
use crate::route::Route;
use crate::session::use_session;
use crate::text;
use crate::ui::{
    loaded_or_empty, now, report_error, report_success, resource_view, today, Avatar, Badge,
    Button, ButtonKind, ConfirmModal, EmptyState, Icon, IconName, Meta, Modal, MonthCalendar,
    MonthSwitcher,
};

use logic::{Filter, Mode, Prepared, Range};

#[component]
pub fn Calendar() -> Element {
    rsx! { CalendarScreen { open: None } }
}

/// 알림 링크(`/calendar/events/{id}`). 캘린더를 열고 그 일정의 상세를 띄운다.
#[component]
pub fn CalendarEvent(id: String) -> Element {
    // 경로 값은 형식을 확인한 뒤에만 쓴다. 틀리면 캘린더만 연다.
    let open = CalendarEventId::parse(&id).ok();
    rsx! { CalendarScreen { key: "{id}", open } }
}

/// 화면 폭이 모바일(959px 이하)인지. 창 크기가 바뀌면 다시 본다.
fn use_compact() -> Signal<bool> {
    let mut compact = use_signal(viewport::is_compact);
    use_future(move || async move {
        loop {
            viewport::resized().await;
            let next = viewport::is_compact();
            if *compact.peek() != next {
                compact.set(next);
            }
        }
    });
    compact
}

/// 브라우저 창(DOM). 계산은 [`logic`]이 한다.
mod viewport {
    /// 모바일 배치로 바뀌는 폭(style.css의 `max-width: 959px`와 같다).
    const COMPACT_MAX: f64 = 959.0;

    pub fn is_compact() -> bool {
        web_sys::window()
            .and_then(|window| window.inner_width().ok())
            .and_then(|width| width.as_f64())
            .is_some_and(|width| width <= COMPACT_MAX)
    }

    /// 창 크기가 한 번 바뀔 때까지 기다린다. 창이 없는 환경(네이티브)에서는 끝나지 않는다.
    pub async fn resized() {
        let Some(window) = web_sys::window() else {
            return std::future::pending().await;
        };
        let promise = js_sys::Promise::new(&mut |resolve, _reject| {
            let options = web_sys::AddEventListenerOptions::new();
            options.set_once(true);
            // 등록에 실패하면 이 기다림은 끝나지 않는다(폭 전환만 빠진다).
            let _ = window.add_event_listener_with_callback_and_add_event_listener_options(
                "resize", &resolve, &options,
            );
        });
        let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    }

    /// 세로로 스크롤되는 영역을 `top`(px)으로 옮긴다(주 보기의 첫 화면을 업무 시간 근처로).
    pub fn scroll_to(id: &str, top: f64) {
        if let Some(element) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id(id))
        {
            element.set_scroll_top(top as i32);
        }
    }
}

#[component]
fn CalendarScreen(open: Option<CalendarEventId>) -> Element {
    let tz = use_session().zone();
    let company = use_company();
    let nav = navigator();
    let today = today(tz);
    let week = company.work_week();

    let mut mode = use_signal(|| Mode::Month);
    let mut anchor = use_signal(|| today);
    let mut mini = use_signal(|| YearMonth::of(today));
    let mut hidden = use_signal(BTreeSet::<CalendarId>::new);
    let mut show_holidays = use_signal(|| true);
    let mut show_leaves = use_signal(|| true);
    let compact = use_compact();
    let mut detail = use_signal(move || open);
    let mut form = use_signal(|| None::<FormMode>);
    let mut day_sheet = use_signal(|| None::<NaiveDate>);
    let mut picker = use_signal(|| false);

    let calendars =
        use_resource(|| async { api::get::<Vec<CalendarDto>>("/calendar/calendars").await });
    let path = use_memo(move || {
        let range = logic::visible_range(mode(), anchor(), compact());
        let list = loaded_or_empty(&calendars);
        let selected = logic::selected_ids(&list, &hidden.read());
        logic::view_path(range, selected.as_deref())
    });
    let mut view = use_resource(move || {
        let path = path();
        async move { api::get::<CalendarView>(&path).await }
    });

    let list = loaded_or_empty(&calendars);
    let current_mode = mode();
    let range = logic::visible_range(current_mode, anchor(), compact());
    let title = logic::title(current_mode, range, anchor());
    let filter = Filter {
        hidden: hidden(),
        personal: logic::personal_calendar(&list),
        holidays: show_holidays(),
        leaves: show_leaves(),
    };

    let mut go_to = move |date: NaiveDate| {
        anchor.set(date);
        mini.set(YearMonth::of(date));
    };
    let toggle = move |id: CalendarId| {
        let mut set = hidden.write();
        if !set.remove(&id) {
            set.insert(id);
        }
    };
    let close_detail = move |_| {
        detail.set(None);
        if open.is_some() {
            // 알림에서 왔으면 주소도 캘린더로 돌려 둔다(새로 고쳐도 다시 뜨지 않게).
            nav.replace(Route::Calendar {});
        }
    };

    let side_list = rsx! {
        CalendarList {
            calendars: list.clone(),
            hidden: filter.hidden.clone(),
            holidays: filter.holidays,
            leaves: filter.leaves,
            ontoggle: toggle,
            onholidays: move |on| show_holidays.set(on),
            onleaves: move |on| show_leaves.set(on),
        }
    };

    rsx! {
        div { class: "cal-layout",
            aside { class: "cal-side panel only-desktop", "aria-label": "캘린더",
                Button {
                    block: true,
                    onclick: move |_| form.set(Some(FormMode::New { date: anchor() })),
                    Icon { name: IconName::Plus }
                    "일정 등록"
                }
                MiniMonth {
                    month: mini(),
                    selected: anchor(),
                    today,
                    onmonth: move |month| mini.set(month),
                    onpick: go_to,
                }
                {side_list.clone()}
            }
            section { class: "panel cal-main",
                div { class: "cal-toolbar",
                    div { class: "cal-nav",
                        button {
                            class: "icon-btn month-prev",
                            r#type: "button",
                            "aria-label": logic::step_label(current_mode, false),
                            onclick: move |_| go_to(logic::step(mode(), anchor(), false, compact(), today)),
                            Icon { name: IconName::Chevron }
                        }
                        button {
                            class: "icon-btn",
                            r#type: "button",
                            "aria-label": logic::step_label(current_mode, true),
                            onclick: move |_| go_to(logic::step(mode(), anchor(), true, compact(), today)),
                            Icon { name: IconName::Chevron }
                        }
                        Button { kind: ButtonKind::Grey, small: true, onclick: move |_| go_to(today), "오늘" }
                    }
                    h1 { class: "cal-title", "aria-live": "polite", "{title}" }
                    div { class: "cal-toolbar-end",
                        div { class: "segmented", role: "group", "aria-label": "보기",
                            for option in Mode::ALL {
                                button {
                                    key: "{option.label()}",
                                    class: if option == current_mode { "segmented-item segmented-item-active" } else { "segmented-item" },
                                    r#type: "button",
                                    "aria-pressed": option == current_mode,
                                    onclick: move |_| mode.set(option),
                                    "{option.label()}"
                                }
                            }
                        }
                    }
                    div { class: "cal-toolbar-mobile only-mobile",
                        button {
                            class: "icon-btn",
                            r#type: "button",
                            "aria-label": "캘린더 선택",
                            onclick: move |_| picker.set(true),
                            Icon { name: IconName::Menu }
                        }
                        button {
                            class: "icon-btn",
                            r#type: "button",
                            "aria-label": "일정 등록",
                            onclick: move |_| form.set(Some(FormMode::New { date: anchor() })),
                            Icon { name: IconName::Plus }
                        }
                    }
                }
                {
                    let view_filter = filter.clone();
                    let list = list.clone();
                    resource_view(
                        view,
                        move |data: CalendarView| {
                            let prepared = logic::prepare(&data, &view_filter, range, week);
                            match current_mode {
                                Mode::Month => rsx! {
                                    MonthView {
                                        data: prepared,
                                        month: YearMonth::of(anchor()),
                                        calendars: list.clone(),
                                        today,
                                        week,
                                        onopen: move |id| detail.set(Some(id)),
                                        onday: move |date| day_sheet.set(Some(date)),
                                    }
                                },
                                Mode::Week => rsx! {
                                    WeekView {
                                        data: prepared,
                                        range,
                                        calendars: list.clone(),
                                        today,
                                        week,
                                        onopen: move |id| detail.set(Some(id)),
                                        onday: move |date| day_sheet.set(Some(date)),
                                    }
                                },
                                Mode::List => rsx! {
                                    ListView {
                                        data: prepared,
                                        range,
                                        calendars: list.clone(),
                                        onopen: move |id| detail.set(Some(id)),
                                        oncreate: move |_| form.set(Some(FormMode::New { date: anchor() })),
                                    }
                                },
                            }
                        },
                    )
                }
            }
        }
        if let Some(date) = day_sheet() {
            DaySheet {
                key: "{date}",
                date,
                data: match &*view.read() {
                    Some(Ok(data)) => logic::prepare(data, &filter, Range { from: date, to: date }, week),
                    _ => Prepared::default(),
                },
                calendars: list.clone(),
                onclose: move |_| day_sheet.set(None),
                onopen: move |id| {
                    day_sheet.set(None);
                    detail.set(Some(id));
                },
                oncreate: move |date| {
                    day_sheet.set(None);
                    form.set(Some(FormMode::New { date }));
                },
            }
        }
        if picker() {
            Modal { title: "캘린더", onclose: move |_| picker.set(false), {side_list} }
        }
        if let Some(id) = detail() {
            EventDetailModal {
                key: "{id}",
                id,
                calendars: list.clone(),
                onclose: close_detail,
                onedit: move |event: EventDetail| {
                    detail.set(None);
                    form.set(Some(FormMode::Edit(Box::new(event))));
                },
                onchanged: move |_| view.restart(),
            }
        }
        if let Some(mode) = form() {
            EventForm {
                mode,
                calendars: list.clone(),
                onclose: move |_| form.set(None),
                onsaved: move |event: EventDetail| {
                    form.set(None);
                    view.restart();
                    // 등록한 날로 옮겨 방금 만든 일정이 보이게 한다.
                    if let Some(date) = logic::event_days(&event.summary, tz).map(|(start, _)| start) {
                        go_to(date);
                    }
                },
                onreload: move |id| {
                    // 다른 사람이 먼저 고쳤다. 폼을 닫고 새로 읽은 상세를 보여 준다.
                    form.set(None);
                    view.restart();
                    detail.set(Some(id));
                },
            }
        }
    }
}

/// 왼쪽의 작은 달력. 날짜를 누르면 그날로 옮긴다.
#[component]
fn MiniMonth(
    month: YearMonth,
    selected: NaiveDate,
    today: NaiveDate,
    onmonth: EventHandler<YearMonth>,
    onpick: EventHandler<NaiveDate>,
) -> Element {
    rsx! {
        div { class: "cal-mini",
            MonthSwitcher { month, onchange: move |next| onmonth.call(next) }
            MonthCalendar {
                month,
                outside: true,
                label: "{month.label()} 날짜 고르기",
                class: "calendar-mini",
                day: move |date: NaiveDate| {
                    let mut class = "cal-mini-day".to_owned();
                    if date == today {
                        class.push_str(" cal-mini-today");
                    }
                    if date == selected {
                        class.push_str(" cal-mini-selected");
                    }
                    rsx! {
                        button {
                            class,
                            r#type: "button",
                            "aria-label": format::date_md(date),
                            "aria-pressed": date == selected,
                            onclick: move |_| onpick.call(date),
                            "{date.day()}"
                        }
                    }
                },
            }
        }
    }
}

/// 캘린더 켜고 끄기(내 일정·부서 일정·전사 일정)와 함께 보이는 정보(공휴일·휴가).
#[component]
fn CalendarList(
    calendars: Vec<CalendarDto>,
    hidden: BTreeSet<CalendarId>,
    holidays: bool,
    leaves: bool,
    ontoggle: EventHandler<CalendarId>,
    onholidays: EventHandler<bool>,
    onleaves: EventHandler<bool>,
) -> Element {
    rsx! {
        div { class: "cal-list",
            h2 { class: "cal-list-title", "캘린더" }
            ul { class: "cal-list-items",
                for calendar in calendars {
                    li { key: "{calendar.id}",
                        label { class: "cal-toggle",
                            input {
                                class: "check-input",
                                r#type: "checkbox",
                                checked: !hidden.contains(&calendar.id),
                                onchange: move |_| ontoggle.call(calendar.id),
                            }
                            span { class: "cal-swatch {logic::kind_class(calendar.kind)}", "aria-hidden": "true" }
                            span { class: "cal-toggle-name", "{calendar.name}" }
                            if !calendar.can_write {
                                span { class: "cal-toggle-note", "보기만" }
                            }
                        }
                    }
                }
            }
            h2 { class: "cal-list-title", "함께 보기" }
            ul { class: "cal-list-items",
                li {
                    label { class: "cal-toggle",
                        input {
                            class: "check-input",
                            r#type: "checkbox",
                            checked: holidays,
                            onchange: move |event| onholidays.call(event.checked()),
                        }
                        span { class: "cal-swatch cal-holiday", "aria-hidden": "true" }
                        span { class: "cal-toggle-name", "공휴일" }
                    }
                }
                li {
                    label { class: "cal-toggle",
                        input {
                            class: "check-input",
                            r#type: "checkbox",
                            checked: leaves,
                            onchange: move |event| onleaves.call(event.checked()),
                        }
                        span { class: "cal-swatch cal-leave", "aria-hidden": "true" }
                        span { class: "cal-toggle-name", "휴가" }
                    }
                }
            }
        }
    }
}

// ───────────── 월 보기 ─────────────

#[component]
fn MonthView(
    data: Prepared,
    month: YearMonth,
    calendars: Vec<CalendarDto>,
    today: NaiveDate,
    week: WorkWeek,
    onopen: EventHandler<CalendarEventId>,
    onday: EventHandler<NaiveDate>,
) -> Element {
    let tz = use_session().zone();
    let cells = logic::month_cells(&data, month, tz);
    rsx! {
        MonthCalendar {
            month,
            outside: true,
            label: "{month.label()} 일정",
            class: "cal-month",
            day: move |date: NaiveDate| {
                let cell = cells.get(&date).cloned().unwrap_or_default();
                rsx! {
                    MonthDay {
                        date,
                        cell,
                        data: data.clone(),
                        calendars: calendars.clone(),
                        today,
                        week,
                        onopen,
                        onday,
                    }
                }
            },
        }
    }
}

#[component]
fn MonthDay(
    date: NaiveDate,
    cell: logic::DayCell,
    data: Prepared,
    calendars: Vec<CalendarDto>,
    today: NaiveDate,
    week: WorkWeek,
    onopen: EventHandler<CalendarEventId>,
    onday: EventHandler<NaiveDate>,
) -> Element {
    let tz = use_session().zone();
    let holidays = data.holidays.get(&date).cloned().unwrap_or_default();
    let leaves = data.leaves.get(&date).cloned().unwrap_or_default();
    let count = logic::day_count(&data, date, tz);
    rsx! {
        div { class: logic::day_class(date, today, week, !holidays.is_empty()),
            div { class: "cal-day-head",
                button {
                    class: "cal-date",
                    r#type: "button",
                    "aria-label": logic::day_aria(date, &holidays, count),
                    "aria-current": if date == today { "date" },
                    onclick: move |_| onday.call(date),
                    "{date.day()}"
                }
                if !holidays.is_empty() {
                    span { class: "cal-holiday-name", "{holidays.join(\", \")}" }
                }
            }
            for (lane , slot) in cell.lanes.iter().enumerate() {
                div { key: "{lane}", class: "cal-slot",
                    if let logic::Lane::Start(bar) = slot {
                        if let Some(event) = data.events.get(bar.event).cloned() {
                            EventBar {
                                event,
                                bar: *bar,
                                calendar: logic::calendar_name(&calendars, &data.events[bar.event]),
                                onopen,
                            }
                        }
                    }
                }
            }
            for item in cell.items.iter().copied() {
                match item {
                    logic::DayItem::Event(index) => rsx! {
                        if let Some(event) = data.events.get(index).cloned() {
                            EventLine {
                                key: "{event.id}",
                                calendar: logic::calendar_name(&calendars, &event),
                                event,
                                onopen,
                            }
                        }
                    },
                    logic::DayItem::Leave(index) => rsx! {
                        if let Some(entry) = leaves.get(index).cloned() {
                            LeaveChip { key: "{entry.member_id}-{entry.unit.as_str()}", entry }
                        }
                    },
                }
            }
            if cell.hidden > 0 {
                button {
                    class: "cal-more",
                    r#type: "button",
                    "aria-label": "{format::date_md(date)} 일정 {cell.hidden}개 더 보기",
                    onclick: move |_| onday.call(date),
                    "+{cell.hidden}"
                }
            }
        }
    }
}

/// 월 보기·주 보기 종일 줄의 막대. 여러 날에 걸치면 시작 칸에서 오른쪽으로 늘어난다.
#[component]
fn EventBar(
    event: EventSummary,
    bar: logic::Bar,
    #[props(into)] calendar: String,
    onopen: EventHandler<CalendarEventId>,
) -> Element {
    let tz = use_session().zone();
    let id = event.id;
    let class = logic::bar_class(&event, bar);
    rsx! {
        button {
            class,
            r#type: "button",
            style: "--span: {bar.span}",
            "aria-label": logic::event_aria(&event, &calendar, tz),
            onclick: move |_| onopen.call(id),
            if !event.all_day && !bar.cont_before {
                span { class: "cal-bar-time", "{logic::short_time(&event, tz)}" }
            }
            span { class: "cal-bar-title", "{event.title}" }
        }
    }
}

/// 월 보기 칸의 시간 일정 한 줄: 점 + `09:30 제목`.
#[component]
fn EventLine(
    event: EventSummary,
    #[props(into)] calendar: String,
    onopen: EventHandler<CalendarEventId>,
) -> Element {
    let tz = use_session().zone();
    let id = event.id;
    rsx! {
        button {
            class: if logic::is_tentative(&event) { "cal-line cal-line-tentative" } else { "cal-line" },
            r#type: "button",
            "aria-label": logic::event_aria(&event, &calendar, tz),
            onclick: move |_| onopen.call(id),
            span { class: "cal-dot {logic::kind_class(event.calendar_kind)}", "aria-hidden": "true" }
            span { class: "cal-line-time", "{logic::short_time(&event, tz)}" }
            span { class: "cal-line-title", "{event.title}" }
        }
    }
}

/// 휴가 칩: `홍길동 휴가(오후½)`. 결재 중이면 옅은 점선 + "대기" 글자.
#[component]
fn LeaveChip(entry: DayEntry) -> Element {
    rsx! {
        span {
            class: if entry.pending { "cal-chip cal-leave-chip cal-leave-pending" } else { "cal-chip cal-leave-chip" },
            title: logic::leave_aria(&entry),
            span { class: "cal-chip-text", "{logic::leave_chip(&entry)}" }
            if entry.pending {
                span { class: "cal-chip-mark", "대기" }
            }
            span { class: "sr-only", ", {logic::leave_aria(&entry)}" }
        }
    }
}

// ───────────── 주 보기 ─────────────

/// 시간 칸 한 시간의 높이(px). style.css의 `--hour-h`와 같다.
const HOUR_PX: f64 = 48.0;
const WEEK_SCROLL_ID: &str = "cal-week-scroll";

#[component]
fn WeekView(
    data: Prepared,
    range: Range,
    calendars: Vec<CalendarDto>,
    today: NaiveDate,
    week: WorkWeek,
    onopen: EventHandler<CalendarEventId>,
    onday: EventHandler<NaiveDate>,
) -> Element {
    let tz = use_session().zone();
    let company = use_company();
    let (work_start, work_end) = company.work_hours();
    let first_hour = logic::first_visible_hour(work_start, work_end);
    let mut clock = use_signal(now);
    use_future(move || async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(60_000).await;
            clock.set(now());
        }
    });
    // 처음 그릴 때 업무 시작 무렵으로 스크롤한다(00~24시는 위아래로 스크롤해 본다).
    // 시 글자가 칸 경계에 걸쳐 있어 조금 위에서 멈춘다.
    use_effect(move || {
        viewport::scroll_to(
            WEEK_SCROLL_ID,
            (f64::from(first_hour) * HOUR_PX - 8.0).max(0.0),
        )
    });

    let days = range.days();
    let all_day = logic::all_day_rows(&data, &days, tz);
    let day_count = days.len();
    let hours: Vec<u32> = (0..24).collect();
    rsx! {
        div { class: "cal-week", style: "--days: {day_count}",
            div { class: "cal-week-head",
                span { class: "cal-week-gutter" }
                for date in days.iter().copied() {
                    button {
                        key: "{date}",
                        class: logic::week_head_class(date, today, week, data.holidays.contains_key(&date)),
                        r#type: "button",
                        "aria-label": logic::day_aria(
                            date,
                            &data.holidays.get(&date).cloned().unwrap_or_default(),
                            logic::day_count(&data, date, tz),
                        ),
                        "aria-current": if date == today { "date" },
                        onclick: move |_| onday.call(date),
                        span { class: "cal-week-weekday", "{format::weekday_short(date.weekday())}" }
                        span { class: "cal-week-date", "{date.day()}" }
                        if let Some(names) = data.holidays.get(&date) {
                            span { class: "cal-holiday-name", "{names.join(\", \")}" }
                        }
                    }
                }
            }
            div { class: "cal-week-allday", style: "--rows: {all_day.rows.max(1)}",
                span { class: "cal-week-gutter cal-week-allday-label", "종일" }
                for bar in all_day.bars.iter().copied() {
                    if let Some(event) = data.events.get(bar.event).cloned() {
                        div {
                            key: "{event.id}",
                            class: "cal-week-bar",
                            style: "grid-column: {bar.start + 2} / span {bar.span}; grid-row: {bar.lane + 1}",
                            EventBar {
                                calendar: logic::calendar_name(&calendars, &event),
                                event,
                                bar: logic::Bar { span: 1, ..bar },
                                onopen,
                            }
                        }
                    }
                }
                for chip in all_day.leaves.iter().cloned() {
                    div {
                        key: "{chip.col}-{chip.row}",
                        class: "cal-week-bar",
                        style: "grid-column: {chip.col + 2}; grid-row: {chip.row + 1}",
                        match chip.entry {
                            Some(entry) => rsx! { LeaveChip { entry } },
                            None => rsx! {
                                button {
                                    class: "cal-more",
                                    r#type: "button",
                                    "aria-label": "휴가 {chip.more}명 더 보기",
                                    onclick: {
                                        let date = days[chip.col];
                                        move |_| onday.call(date)
                                    },
                                    "휴가 +{chip.more}"
                                }
                            },
                        }
                    }
                }
            }
            div { id: WEEK_SCROLL_ID, class: "cal-week-scroll",
                div { class: "cal-week-grid",
                    div { class: "cal-hours", "aria-hidden": "true",
                        for hour in hours {
                            span { key: "{hour}", class: "cal-hour", style: "--hour: {hour}", "{hour:02}:00" }
                        }
                    }
                    for date in days.iter().copied() {
                        div {
                            key: "{date}",
                            class: if date == today { "cal-week-col cal-week-col-today" } else { "cal-week-col" },
                            role: "group",
                            "aria-label": format::date_md(date),
                            for block in logic::day_blocks(&data.events, date, tz) {
                                if let Some(event) = data.events.get(block.event).cloned() {
                                    TimedBlock {
                                        key: "{event.id}",
                                        calendar: logic::calendar_name(&calendars, &event),
                                        event,
                                        block,
                                        onopen,
                                    }
                                }
                            }
                            if let Some(minute) = logic::now_minute(clock(), date, tz) {
                                div {
                                    class: "cal-now",
                                    style: "top: {logic::percent(minute)}%",
                                    "aria-hidden": "true",
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn TimedBlock(
    event: EventSummary,
    block: logic::Block,
    #[props(into)] calendar: String,
    onopen: EventHandler<CalendarEventId>,
) -> Element {
    let tz = use_session().zone();
    let id = event.id;
    let mut class = format!("cal-block {}", logic::kind_class(event.calendar_kind));
    if logic::is_tentative(&event) {
        class.push_str(" cal-tentative");
    }
    if block.is_short() {
        class.push_str(" cal-block-short");
    }
    rsx! {
        button {
            class,
            r#type: "button",
            style: block.style(),
            "aria-label": logic::event_aria(&event, &calendar, tz),
            onclick: move |_| onopen.call(id),
            span { class: "cal-block-title", "{event.title}" }
            span { class: "cal-block-time", "{logic::block_time(&block)}" }
        }
    }
}

// ───────────── 목록 보기·하루 목록 ─────────────

#[component]
fn ListView(
    data: Prepared,
    range: Range,
    calendars: Vec<CalendarDto>,
    onopen: EventHandler<CalendarEventId>,
    oncreate: EventHandler<()>,
) -> Element {
    let tz = use_session().zone();
    let days = logic::agenda(&data, range, tz);
    if days.is_empty() {
        return rsx! {
            EmptyState { title: "이 기간에 일정이 없어요.", icon: IconName::Schedule,
                Button { kind: ButtonKind::Weak, onclick: move |_| oncreate.call(()), "일정 등록" }
            }
        };
    }
    rsx! {
        div { class: "cal-agenda",
            for day in days {
                AgendaDayBlock {
                    key: "{day.date}",
                    day: day.clone(),
                    events: data.events.clone(),
                    calendars: calendars.clone(),
                    onopen,
                }
            }
        }
    }
}

#[component]
fn AgendaDayBlock(
    day: logic::AgendaDay,
    events: Vec<EventSummary>,
    calendars: Vec<CalendarDto>,
    onopen: EventHandler<CalendarEventId>,
    /// 날짜 제목을 붙인다(하루 목록은 대화 상자 제목이 날짜라 공휴일만 보인다).
    #[props(default = true)]
    dated: bool,
) -> Element {
    let tz = use_session().zone();
    rsx! {
        section { class: "cal-agenda-day",
            if dated {
                h2 { class: "cal-agenda-date",
                    "{format::date_md(day.date)}"
                    for name in day.holidays.iter() {
                        span { key: "{name}", class: "cal-holiday-name", "{name}" }
                    }
                }
            } else if !day.holidays.is_empty() {
                p { class: "cal-agenda-date cal-holiday-name", "{day.holidays.join(\", \")}" }
            }
            ul { class: "list",
                for index in day.events.iter().copied() {
                    if let Some(event) = events.get(index).cloned() {
                        AgendaRow {
                            key: "{event.id}",
                            time: logic::agenda_time(&event, day.date, tz),
                            calendar: logic::calendar_name(&calendars, &event),
                            event,
                            onopen,
                        }
                    }
                }
                for entry in day.leaves.iter().cloned() {
                    li { key: "{entry.member_id}-{entry.unit.as_str()}", class: "list-row cal-agenda-row",
                        span { class: "cal-agenda-time", {text::leave_unit_short(entry.unit)} }
                        span { class: "cal-dot cal-leave", "aria-hidden": "true" }
                        div { class: "list-row-main",
                            strong { "{entry.member_name} 휴가" }
                            small { {text::leave_unit_label(entry.unit)} }
                        }
                        div { class: "list-row-side",
                            Badge { tone: text::leave_status_tone(entry.status()), {text::leave_status_label(entry.status())} }
                        }
                    }
                }
            }
        }
    }
}

/// 일정 한 줄(목록 보기·하루 목록·홈의 오늘 일정이 함께 쓴다).
///
/// `onopen`을 주면 누를 때 그 일정을 띄우고, 없으면 그 일정의 주소(`/calendar/events/{id}`)로 간다.
#[component]
pub fn AgendaRow(
    event: EventSummary,
    /// 시간 칸 문구(`종일`, `09:30 – 10:00`).
    #[props(into)]
    time: String,
    #[props(into)] calendar: String,
    #[props(default)] onopen: Option<EventHandler<CalendarEventId>>,
) -> Element {
    let tz = use_session().zone();
    let id = event.id;
    let aria = logic::event_aria(&event, &calendar, tz);
    let invite = logic::invite_state(&event);
    let body = rsx! {
        span { class: "cal-agenda-time", "{time}" }
        span { class: "cal-dot {logic::kind_class(event.calendar_kind)}", "aria-hidden": "true" }
        div { class: "list-row-main",
            strong { "{event.title}" }
            Meta { items: vec![calendar.clone(), event.location.clone().unwrap_or_default()] }
        }
        if let Some(response) = invite {
            div { class: "list-row-side",
                Badge { tone: text::attendee_response_tone(response), {text::attendee_response_label(response)} }
            }
        }
    };
    let class = if logic::is_tentative(&event) {
        "list-row list-row-btn cal-agenda-row cal-tentative-row"
    } else {
        "list-row list-row-btn cal-agenda-row"
    };
    rsx! {
        li {
            match onopen {
                Some(onopen) => rsx! {
                    button { class, r#type: "button", "aria-label": aria, onclick: move |_| onopen.call(id), {body} }
                },
                None => rsx! {
                    Link { class, to: Route::CalendarEvent { id: id.to_string() }, "aria-label": aria, {body} }
                },
            }
        }
    }
}

/// 하루의 모든 것(월 보기의 날짜·"+N", 주 보기의 날짜 머리를 누르면 뜬다).
#[component]
fn DaySheet(
    date: NaiveDate,
    data: Prepared,
    calendars: Vec<CalendarDto>,
    onclose: EventHandler<()>,
    onopen: EventHandler<CalendarEventId>,
    oncreate: EventHandler<NaiveDate>,
) -> Element {
    let tz = use_session().zone();
    let day = logic::agenda(
        &data,
        Range {
            from: date,
            to: date,
        },
        tz,
    )
    .into_iter()
    .next();
    rsx! {
        Modal { title: format::date_md(date), onclose: move |_| onclose.call(()),
            match day {
                Some(day) => rsx! {
                    AgendaDayBlock { day, events: data.events.clone(), calendars, onopen, dated: false }
                },
                None => rsx! {
                    p { class: "field-hint", "이날은 일정이 없어요." }
                },
            }
            div { class: "modal-actions",
                Button { kind: ButtonKind::Weak, onclick: move |_| oncreate.call(date), "이날 일정 등록" }
            }
        }
    }
}

// ───────────── 일정 상세 ─────────────

#[component]
fn EventDetailModal(
    id: CalendarEventId,
    calendars: Vec<CalendarDto>,
    onclose: EventHandler<()>,
    onedit: EventHandler<EventDetail>,
    onchanged: EventHandler<()>,
) -> Element {
    let mut detail = use_resource(use_reactive!(|id| async move {
        api::get::<EventDetail>(&format!("/calendar/events/{id}")).await
    }));
    let mut busy = use_signal(|| false);
    let mut confirming = use_signal(|| false);

    let title = match &*detail.read() {
        Some(Ok(event)) => event.summary.title.clone(),
        _ => "일정".to_owned(),
    };

    let respond = move |status: AttendeeResponse| async move {
        if busy() {
            return;
        }
        busy.set(true);
        match api::post::<_, EventDetail>(
            &format!("/calendar/events/{id}/respond"),
            &RespondRequest { status },
        )
        .await
        {
            Ok(_) => {
                report_success(logic::respond_message(status));
                onchanged.call(());
            }
            Err(error) => report_error(&error),
        }
        detail.restart();
        busy.set(false);
    };
    let remove = move |version: i64| async move {
        if busy() {
            return;
        }
        busy.set(true);
        let path = format!(
            "/calendar/events/{id}{}",
            api::query_string(&[("version", version.to_string())])
        );
        match api::delete::<Empty>(&path).await {
            Ok(_) => {
                report_success("일정을 삭제했어요.");
                confirming.set(false);
                onchanged.call(());
                onclose.call(());
                return;
            }
            Err(error) => {
                report_error(&error);
                confirming.set(false);
                // 그사이 바뀌었거나 지워졌을 수 있다. 다시 읽어 지금 상태를 보여 준다.
                detail.restart();
                onchanged.call(());
            }
        }
        busy.set(false);
    };

    rsx! {
        Modal { title, onclose: move |_| onclose.call(()), class: "cal-detail-modal",
            {
                resource_view(
                    detail,
                    move |event: EventDetail| {
                        let calendar = logic::calendar_name(&calendars, &event.summary);
                        let version = event.summary.version;
                        let role = event.summary.my_role;
                        let mine = event.summary.my_response;
                        let delete_message = logic::delete_message(&event);
                        let edit = event.clone();
                        rsx! {
                            DetailBody { event: event.clone(), calendar }
                            match role {
                                EventRole::Owner => rsx! {
                                    div { class: "modal-actions",
                                        Button {
                                            kind: ButtonKind::Danger,
                                            disabled: busy(),
                                            onclick: move |_| confirming.set(true),
                                            "삭제"
                                        }
                                        Button {
                                            kind: ButtonKind::Weak,
                                            disabled: busy(),
                                            onclick: move |_| onedit.call(edit.clone()),
                                            "수정"
                                        }
                                    }
                                },
                                EventRole::Attendee => rsx! {
                                    div { class: "cal-respond", role: "group", "aria-label": "참석 응답",
                                        span { class: "cal-respond-label", "참석하시나요?" }
                                        for status in [AttendeeResponse::Accepted, AttendeeResponse::Declined] {
                                            button {
                                                key: "{status.as_str()}",
                                                class: if mine == Some(status) { "btn btn-weak cal-respond-on" } else { "btn btn-grey" },
                                                r#type: "button",
                                                disabled: busy(),
                                                "aria-pressed": mine == Some(status),
                                                onclick: move |_| respond(status),
                                                if mine == Some(status) {
                                                    Icon { name: IconName::Check }
                                                }
                                                {text::attendee_response_label(status)}
                                            }
                                        }
                                    }
                                },
                                EventRole::Viewer => rsx! {},
                            }
                            if confirming() {
                                ConfirmModal {
                                    title: "일정을 삭제할까요?",
                                    message: delete_message.clone(),
                                    confirm_label: "삭제",
                                    danger: true,
                                    busy: busy(),
                                    onconfirm: move |_| remove(version),
                                    oncancel: move |_| confirming.set(false),
                                }
                            }
                        }
                    },
                )
            }
        }
    }
}

#[component]
fn DetailBody(event: EventDetail, #[props(into)] calendar: String) -> Element {
    let tz = use_session().zone();
    let summary = &event.summary;
    rsx! {
        div { class: "cal-detail",
            p { class: "cal-detail-time", "{logic::time_label(summary, tz)}" }
            dl { class: "cal-detail-meta",
                div {
                    dt { "캘린더" }
                    dd {
                        span { class: "cal-dot {logic::kind_class(summary.calendar_kind)}", "aria-hidden": "true" }
                        "{calendar}"
                    }
                }
                if let Some(location) = summary.location.as_ref() {
                    div {
                        dt { "장소" }
                        dd { "{location}" }
                    }
                }
                div {
                    dt { "만든 사람" }
                    dd { "{event.owner_name}" }
                }
            }
            if let Some(description) = event.description.as_ref() {
                p { class: "cal-detail-desc", "{description}" }
            }
            if !event.attendees.is_empty() {
                h3 { class: "sub-title", "참석자 {event.attendees.len()}명" }
                ul { class: "list",
                    for attendee in event.attendees.iter() {
                        li { key: "{attendee.member_id}", class: "list-row",
                            Avatar { name: attendee.name.clone(), seed: attendee.member_id.to_string() }
                            div { class: "list-row-main",
                                strong { "{attendee.name}" }
                            }
                            div { class: "list-row-side",
                                Badge { tone: text::attendee_response_tone(attendee.response),
                                    {text::attendee_response_label(attendee.response)}
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 홈의 "오늘 일정" 패널. 오늘(회사 시간대) 하루만 읽는다.
#[component]
pub fn TodayEvents(tz: Tz) -> Element {
    let today = today(tz);
    let events = use_resource(move || async move {
        let range = Range {
            from: today,
            to: today,
        };
        api::get::<CalendarView>(&logic::view_path(range, None)).await
    });
    rsx! {
        crate::ui::Card {
            title: "오늘 일정",
            class: "grid-span",
            action: rsx! {
                crate::ui::PanelLink { to: Route::Calendar {}, label: "캘린더" }
            },
            {
                resource_view(
                    events,
                    move |view: CalendarView| {
                        let (shown, hidden) = logic::today_rows(&view.events, today, tz);
                        rsx! {
                            if shown.is_empty() {
                                p { class: "field-hint", "오늘 잡힌 일정이 없어요." }
                            } else {
                                ul { class: "list",
                                    for event in shown {
                                        AgendaRow {
                                            key: "{event.id}",
                                            time: logic::agenda_time(&event, today, tz),
                                            calendar: logic::calendar_name(&[], &event),
                                            event: event.clone(),
                                        }
                                    }
                                }
                                if hidden > 0 {
                                    p { class: "field-hint", "외 {hidden}건은 캘린더에서 볼 수 있어요." }
                                }
                            }
                        }
                    },
                )
            }
        }
    }
}

/// 화면이 그리는 값을 만드는 순수 함수.
pub(crate) mod logic {
    use std::collections::{BTreeMap, BTreeSet};

    use chrono::{Duration, NaiveDate, Timelike};
    use chrono_tz::Tz;
    use pango_api::calendar::{CalendarDto, CalendarView, EventDetail, EventSummary};
    use pango_core::calendar::WorkWeek;
    use pango_core::calendar_access::{CalendarKind, EventRole};
    use pango_core::calendar_event::{AttendeeResponse, MAX_VIEW_DAYS};
    use pango_core::id::CalendarId;
    use pango_core::leave::LeaveUnit;
    use pango_core::time::{MinuteOfDay, Timestamp};

    use crate::api;
    use crate::calendar::{week_start, YearMonth};
    use crate::company::is_day_off;
    use crate::format;
    use crate::pages::leave::logic::{default_selected_day, entries_in_range, DayEntry};
    use crate::text;

    pub const DAY_MINUTES: i64 = 24 * 60;
    /// 월 보기 한 칸에 보이는 줄 수(막대·시간 일정·휴가를 합쳐). 넘치면 "+N"을 붙인다.
    pub const MAX_ROWS: usize = 3;
    /// 주 보기 종일 줄에서 하루에 보이는 휴가 칩 수.
    pub const WEEK_LEAVES: usize = 2;
    /// 모바일의 주 보기는 3일만 보인다.
    pub const COMPACT_DAYS: i64 = 3;
    /// 주 보기에서 일정 칸의 최소 길이(분). 짧은 일정도 글자가 보이게 그리므로 겹침도 이 길이로 본다.
    pub const MIN_BLOCK_MINUTES: i64 = 30;
    /// 홈의 오늘 일정에 보여 줄 줄 수.
    pub const TODAY_SHOWN: usize = 5;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Mode {
        Month,
        Week,
        List,
    }

    impl Mode {
        pub const ALL: [Self; 3] = [Self::Month, Self::Week, Self::List];

        pub const fn label(self) -> &'static str {
            match self {
                Self::Month => "월",
                Self::Week => "주",
                Self::List => "목록",
            }
        }
    }

    /// 화면에 보이는 기간(양 끝 포함).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Range {
        pub from: NaiveDate,
        pub to: NaiveDate,
    }

    impl Range {
        pub fn days(self) -> Vec<NaiveDate> {
            self.from
                .iter_days()
                .take_while(|day| *day <= self.to)
                .collect()
        }

        pub fn len(self) -> i64 {
            (self.to - self.from).num_days() + 1
        }
    }

    fn add_days(date: NaiveDate, days: i64) -> NaiveDate {
        date.checked_add_signed(Duration::days(days))
            .unwrap_or(date)
    }

    /// 보기마다 읽고 그리는 기간. 월은 앞뒤 달로 채운 꽉 찬 주(최대 42일), 주는 일~토(모바일은
    /// `anchor`부터 3일), 목록은 그 달. 모두 서버 한도(62일) 안이다.
    pub fn visible_range(mode: Mode, anchor: NaiveDate, compact: bool) -> Range {
        match mode {
            Mode::Month => {
                let month = YearMonth::of(anchor);
                Range {
                    from: week_start(month.first_day()),
                    to: add_days(week_start(month.last_day()), 6),
                }
            }
            Mode::Week if compact => Range {
                from: anchor,
                to: add_days(anchor, COMPACT_DAYS - 1),
            },
            Mode::Week => {
                let start = week_start(anchor);
                Range {
                    from: start,
                    to: add_days(start, 6),
                }
            }
            Mode::List => {
                let month = YearMonth::of(anchor);
                Range {
                    from: month.first_day(),
                    to: month.last_day(),
                }
            }
        }
    }

    /// ‹ › 로 옮길 날. 월·목록은 한 달(그 달에 오늘이 있으면 오늘, 아니면 1일), 주는 7일(모바일 3일).
    pub fn step(
        mode: Mode,
        anchor: NaiveDate,
        forward: bool,
        compact: bool,
        today: NaiveDate,
    ) -> NaiveDate {
        match mode {
            Mode::Month | Mode::List => {
                let month = YearMonth::of(anchor);
                let target = match forward {
                    true if month.can_go_next() => month.next(),
                    false if month.can_go_prev() => month.prev(),
                    _ => return anchor,
                };
                default_selected_day(target, today)
            }
            Mode::Week => {
                let days = if compact { COMPACT_DAYS } else { 7 };
                add_days(anchor, if forward { days } else { -days })
            }
        }
    }

    pub fn step_label(mode: Mode, forward: bool) -> &'static str {
        match (mode, forward) {
            (Mode::Week, false) => "이전 기간",
            (Mode::Week, true) => "다음 기간",
            (_, false) => "이전 달",
            (_, true) => "다음 달",
        }
    }

    /// 머리 제목: `2026년 11월`. 주가 두 달에 걸치면 `2026년 11월 – 12월`.
    pub fn title(mode: Mode, range: Range, anchor: NaiveDate) -> String {
        match mode {
            Mode::Month | Mode::List => YearMonth::of(anchor).label(),
            Mode::Week => {
                let (first, last) = (YearMonth::of(range.from), YearMonth::of(range.to));
                if first == last {
                    first.label()
                } else if first.year == last.year {
                    format!("{}년 {}월 – {}월", first.year, first.month, last.month)
                } else {
                    format!("{} – {}", first.label(), last.label())
                }
            }
        }
    }

    /// 내 일정 캘린더의 ID.
    pub fn personal_calendar(calendars: &[CalendarDto]) -> Option<CalendarId> {
        calendars
            .iter()
            .find(|c| c.kind == CalendarKind::Personal)
            .map(|c| c.id)
    }

    /// 조회할 캘린더. 숨긴 것이 없거나(목록을 아직 못 읽은 경우 포함) 모두 숨겼으면 `None`
    /// (쿼리를 생략해 전부 읽고, 모두 숨겼으면 화면이 [`prepare`]에서 걸러 낸다).
    pub fn selected_ids(
        calendars: &[CalendarDto],
        hidden: &BTreeSet<CalendarId>,
    ) -> Option<Vec<CalendarId>> {
        if hidden.is_empty() {
            return None;
        }
        let ids: Vec<CalendarId> = calendars
            .iter()
            .map(|c| c.id)
            .filter(|id| !hidden.contains(id))
            .collect();
        (!ids.is_empty() && ids.len() < calendars.len()).then_some(ids)
    }

    /// `GET /calendar/view` 경로. 기간이 서버 한도(62일)를 넘으면 앞에서부터 62일로 자른다.
    pub fn view_path(range: Range, selected: Option<&[CalendarId]>) -> String {
        let to = if range.len() > MAX_VIEW_DAYS {
            add_days(range.from, MAX_VIEW_DAYS - 1)
        } else {
            range.to
        };
        let mut pairs = vec![("from", range.from.to_string()), ("to", to.to_string())];
        if let Some(ids) = selected {
            let joined = ids
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            pairs.push(("calendars", joined));
        }
        format!("/calendar/view{}", api::query_string(&pairs))
    }

    /// 화면에서 켜고 끈 것.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct Filter {
        pub hidden: BTreeSet<CalendarId>,
        pub personal: Option<CalendarId>,
        pub holidays: bool,
        pub leaves: bool,
    }

    /// 켜 둔 캘린더의 일정인지. 초대받은 다른 사람의 일정은 내 일정에 딸려 온다(서버도 내 일정을
    /// 읽을 때만 준다).
    pub fn event_shown(event: &EventSummary, filter: &Filter) -> bool {
        if filter.hidden.contains(&event.calendar_id) {
            return false;
        }
        match filter.personal {
            Some(mine)
                if event.my_role == EventRole::Attendee
                    && event.calendar_kind == CalendarKind::Personal =>
            {
                !filter.hidden.contains(&mine)
            }
            _ => true,
        }
    }

    /// 그릴 것만 남긴 조회 결과. 일정은 서버 순서(시작순, 같은 시각이면 종일 먼저)다.
    #[derive(Debug, Clone, PartialEq, Default)]
    pub struct Prepared {
        pub events: Vec<EventSummary>,
        pub holidays: BTreeMap<NaiveDate, Vec<String>>,
        /// 근무 요일의 휴가만(휴가 달력과 같은 규칙).
        pub leaves: BTreeMap<NaiveDate, Vec<DayEntry>>,
    }

    pub fn prepare(view: &CalendarView, filter: &Filter, range: Range, week: WorkWeek) -> Prepared {
        let events = view
            .events
            .iter()
            .filter(|event| event_shown(event, filter))
            .cloned()
            .collect();
        let mut holidays: BTreeMap<NaiveDate, Vec<String>> = BTreeMap::new();
        if filter.holidays {
            for holiday in &view.holidays {
                if range.from <= holiday.date && holiday.date <= range.to {
                    holidays
                        .entry(holiday.date)
                        .or_default()
                        .push(holiday.name.clone());
                }
            }
        }
        let leaves = if filter.leaves {
            entries_in_range(&view.leaves, range.from, range.to, week)
        } else {
            BTreeMap::new()
        };
        Prepared {
            events,
            holidays,
            leaves,
        }
    }

    // ── 일정의 날짜·시각 ──

    /// 회사 시간대의 날짜와 그날 0시부터의 분.
    fn local(at: Timestamp, tz: Tz) -> (NaiveDate, i64) {
        let local = at.to_utc().with_timezone(&tz);
        (
            local.date_naive(),
            i64::from(local.hour()) * 60 + i64::from(local.minute()),
        )
    }

    /// 끝 시각은 포함하지 않으므로 마지막 날은 끝 직전 시각의 날짜다(자정에 끝나면 전날).
    fn just_before(at: Timestamp) -> Timestamp {
        Timestamp::from_millis(at.as_millis().saturating_sub(1)).unwrap_or(at)
    }

    /// 일정이 걸친 첫날과 마지막 날(회사 시간대, 양 끝 포함).
    pub fn event_days(event: &EventSummary, tz: Tz) -> Option<(NaiveDate, NaiveDate)> {
        if event.all_day {
            let start = event.start_date?;
            return Some((start, event.end_date.unwrap_or(start).max(start)));
        }
        let (start, end) = (event.start_at?, event.end_at?);
        let first = format::local_date(start, tz);
        let last = if end > start {
            format::local_date(just_before(end), tz)
        } else {
            first
        };
        Some((first, last.max(first)))
    }

    /// 월 보기에서 막대로 그리는가: 종일 일정과 여러 날에 걸친 시간 일정.
    pub fn is_bar(event: &EventSummary, tz: Tz) -> bool {
        event.all_day || event_days(event, tz).is_some_and(|(first, last)| first != last)
    }

    /// 주 보기의 종일 줄에 놓는가: 종일 일정과 24시간 이상인 시간 일정. 더 짧은 시간 일정은
    /// 자정을 넘어도 날마다 잘라 시간 칸에 그린다.
    pub fn in_all_day_row(event: &EventSummary) -> bool {
        event.all_day
            || matches!(
                (event.start_at, event.end_at),
                (Some(start), Some(end)) if end.since(start) >= Duration::days(1)
            )
    }

    /// 일정이 `date`에 걸치는지.
    pub fn covers(event: &EventSummary, date: NaiveDate, tz: Tz) -> bool {
        if event.all_day {
            event_days(event, tz).is_some_and(|(first, last)| first <= date && date <= last)
        } else {
            day_segment(event, date, tz).is_some()
        }
    }

    // ── 막대의 줄 ──

    /// 칸 구간 `(시작, 끝)`(양 끝 포함)을 겹치지 않게 줄에 놓는다. 일찍 시작하는 것, 같으면 긴
    /// 것부터 위쪽 빈 줄에 넣는다. 결과는 입력 순서대로의 줄 번호다.
    pub fn assign_lanes(spans: &[(usize, usize)]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..spans.len()).collect();
        order.sort_by_key(|&i| (spans[i].0, std::cmp::Reverse(spans[i].1), i));
        let mut lane_ends: Vec<usize> = Vec::new();
        let mut lanes = vec![0; spans.len()];
        for index in order {
            let (start, end) = spans[index];
            let lane = match lane_ends.iter().position(|&last| last < start) {
                Some(lane) => {
                    lane_ends[lane] = end;
                    lane
                }
                None => {
                    lane_ends.push(end);
                    lane_ends.len() - 1
                }
            };
            lanes[index] = lane;
        }
        lanes
    }

    /// 한 줄(주)에 놓인 막대 하나. 칸 번호는 그 주의 0~6(주 보기는 0~날 수-1).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Bar {
        /// [`Prepared::events`]의 순서.
        pub event: usize,
        pub start: usize,
        pub span: usize,
        pub lane: usize,
        /// 이 주보다 앞에서 시작했다(왼쪽 끝을 둥글리지 않는다).
        pub cont_before: bool,
        /// 이 주보다 뒤에서 끝난다.
        pub cont_after: bool,
    }

    /// `days`(이어진 날짜)에 걸친 `candidates` 일정을 막대로 자르고 줄을 정한다.
    pub fn row_bars(
        events: &[EventSummary],
        candidates: &[usize],
        days: &[NaiveDate],
        tz: Tz,
    ) -> Vec<Bar> {
        let (Some(&first), Some(&last)) = (days.first(), days.last()) else {
            return Vec::new();
        };
        let mut bars = Vec::new();
        for &index in candidates {
            let Some((start, end)) = events.get(index).and_then(|e| event_days(e, tz)) else {
                continue;
            };
            if end < first || start > last {
                continue;
            }
            let from = (start.max(first) - first).num_days();
            let to = (end.min(last) - first).num_days();
            let (Ok(from), Ok(to)) = (usize::try_from(from), usize::try_from(to)) else {
                continue;
            };
            bars.push(Bar {
                event: index,
                start: from,
                span: to - from + 1,
                lane: 0,
                cont_before: start < first,
                cont_after: end > last,
            });
        }
        let spans: Vec<(usize, usize)> = bars
            .iter()
            .map(|bar| (bar.start, bar.start + bar.span - 1))
            .collect();
        for (bar, lane) in bars.iter_mut().zip(assign_lanes(&spans)) {
            bar.lane = lane;
        }
        bars
    }

    /// 월 보기 칸의 막대 자리.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Lane {
        /// 이 줄에 막대가 없다(아래 줄의 막대와 높이를 맞추는 빈자리).
        Empty,
        /// 왼쪽 칸에서 시작한 막대가 지나간다.
        Covered,
        /// 막대가 여기서 시작한다(오른쪽으로 `span`칸).
        Start(Bar),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DayItem {
        /// [`Prepared::events`]의 순서.
        Event(usize),
        /// 그날 [`Prepared::leaves`] 목록의 순서.
        Leave(usize),
    }

    /// 월 보기 한 칸: 막대 자리, 그 아래 시간 일정·휴가, 넘친 수.
    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct DayCell {
        pub lanes: Vec<Lane>,
        pub items: Vec<DayItem>,
        pub hidden: usize,
    }

    /// 한 주의 칸들. 막대 줄은 주 전체에서 정하므로 같은 막대는 날마다 같은 높이에 있다.
    /// 줄 번호가 [`MAX_ROWS`] 이상인 막대는 그리지 않고 지나는 날마다 "+N"에 센다.
    pub fn week_cells(data: &Prepared, week: &[NaiveDate], tz: Tz) -> Vec<DayCell> {
        let bar_events: Vec<usize> = (0..data.events.len())
            .filter(|&i| is_bar(&data.events[i], tz))
            .collect();
        let bars = row_bars(&data.events, &bar_events, week, tz);
        week.iter()
            .enumerate()
            .map(|(col, date)| {
                let covering: Vec<&Bar> = bars
                    .iter()
                    .filter(|bar| bar.start <= col && col < bar.start + bar.span)
                    .collect();
                let used = covering.iter().map(|bar| bar.lane + 1).max().unwrap_or(0);
                let visible = used.min(MAX_ROWS);
                let lanes = (0..visible)
                    .map(|lane| match covering.iter().find(|bar| bar.lane == lane) {
                        Some(bar) if bar.start == col => Lane::Start(**bar),
                        Some(_) => Lane::Covered,
                        None => Lane::Empty,
                    })
                    .collect();
                let mut hidden = covering.iter().filter(|bar| bar.lane >= MAX_ROWS).count();
                let mut items: Vec<DayItem> = (0..data.events.len())
                    .filter(|&i| {
                        let event = &data.events[i];
                        !is_bar(event, tz)
                            && event_days(event, tz).is_some_and(|(first, _)| first == *date)
                    })
                    .map(DayItem::Event)
                    .collect();
                let leave_count = data.leaves.get(date).map_or(0, Vec::len);
                items.extend((0..leave_count).map(DayItem::Leave));
                let room = MAX_ROWS - visible;
                hidden += items.len().saturating_sub(room);
                items.truncate(room);
                DayCell {
                    lanes,
                    items,
                    hidden,
                }
            })
            .collect()
    }

    /// 월 보기 전체(앞뒤 달로 채운 주까지)의 칸.
    pub fn month_cells(data: &Prepared, month: YearMonth, tz: Tz) -> BTreeMap<NaiveDate, DayCell> {
        let mut cells = BTreeMap::new();
        for week in month.full_weeks() {
            for (date, cell) in week.iter().zip(week_cells(data, &week, tz)) {
                cells.insert(*date, cell);
            }
        }
        cells
    }

    /// 그날 걸친 일정·휴가 수(날짜 버튼의 접근성 이름).
    pub fn day_count(data: &Prepared, date: NaiveDate, tz: Tz) -> usize {
        let events = data.events.iter().filter(|e| covers(e, date, tz)).count();
        events + data.leaves.get(&date).map_or(0, Vec::len)
    }

    // ── 주 보기 ──

    /// 주 보기 종일 줄의 휴가 칩(또는 `휴가 +N`) 자리.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct LeaveSlot {
        pub col: usize,
        pub row: usize,
        /// `None`이면 "휴가 +`more`" 버튼.
        pub entry: Option<DayEntry>,
        pub more: usize,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    pub struct AllDayRows {
        pub bars: Vec<Bar>,
        pub leaves: Vec<LeaveSlot>,
        /// 줄 수(막대 줄 + 휴가 줄).
        pub rows: usize,
    }

    /// 주 보기 맨 위 종일 줄: 막대(종일·24시간 이상)를 줄에 놓고, 그 아래에 날마다 휴가를
    /// [`WEEK_LEAVES`]개까지 놓는다.
    pub fn all_day_rows(data: &Prepared, days: &[NaiveDate], tz: Tz) -> AllDayRows {
        let candidates: Vec<usize> = (0..data.events.len())
            .filter(|&i| in_all_day_row(&data.events[i]))
            .collect();
        let bars = row_bars(&data.events, &candidates, days, tz);
        let lanes = bars.iter().map(|bar| bar.lane + 1).max().unwrap_or(0);
        let mut leaves = Vec::new();
        let mut leave_rows = 0;
        for (col, date) in days.iter().enumerate() {
            let list = data.leaves.get(date).cloned().unwrap_or_default();
            let overflow = list.len() > WEEK_LEAVES;
            let shown = if overflow {
                WEEK_LEAVES - 1
            } else {
                list.len()
            };
            for (row, entry) in list.iter().take(shown).enumerate() {
                leaves.push(LeaveSlot {
                    col,
                    row: lanes + row,
                    entry: Some(entry.clone()),
                    more: 0,
                });
            }
            if overflow {
                leaves.push(LeaveSlot {
                    col,
                    row: lanes + shown,
                    entry: None,
                    more: list.len() - shown,
                });
            }
            leave_rows = leave_rows.max(list.len().min(WEEK_LEAVES));
        }
        AllDayRows {
            bars,
            leaves,
            rows: lanes + leave_rows,
        }
    }

    /// 시간 일정이 `date`(회사 시간대의 벽시계)에 걸친 부분: `(시작 분, 끝 분, 앞날에서 이어짐,
    /// 다음 날로 이어짐)`. 분은 0~1440이고 끝은 포함하지 않는다. 걸치지 않으면 `None`.
    pub fn day_segment(
        event: &EventSummary,
        date: NaiveDate,
        tz: Tz,
    ) -> Option<(i64, i64, bool, bool)> {
        let (start, end) = (event.start_at?, event.end_at?);
        if end <= start {
            return None;
        }
        let minute_on = |at: Timestamp| {
            let (day, minute) = local(at, tz);
            if day < date {
                0
            } else if day > date {
                DAY_MINUTES
            } else {
                minute
            }
        };
        let (from, to) = (minute_on(start), minute_on(end));
        (to > from).then(|| {
            (
                from,
                to,
                format::local_date(start, tz) < date,
                format::local_date(just_before(end), tz) > date,
            )
        })
    }

    /// 겹치는 구간을 나란히 놓을 열: 입력 순서대로 `(열 번호, 그 묶음의 열 수)`.
    /// 서로 이어 겹치는 구간끼리 한 묶음이고, 묶음 안에서 일찍 시작하는 것부터 왼쪽 빈 열에 넣는다.
    pub fn layout_columns(segments: &[(i64, i64)]) -> Vec<(usize, usize)> {
        fn close(group: &mut Vec<usize>, columns: &mut Vec<i64>, result: &mut [(usize, usize)]) {
            for &index in group.iter() {
                result[index].1 = columns.len().max(1);
            }
            group.clear();
            columns.clear();
        }
        let mut order: Vec<usize> = (0..segments.len()).collect();
        order.sort_by_key(|&i| (segments[i].0, std::cmp::Reverse(segments[i].1), i));
        let mut result = vec![(0, 1); segments.len()];
        let mut group: Vec<usize> = Vec::new();
        let mut columns: Vec<i64> = Vec::new();
        let mut group_end = i64::MIN;
        for index in order {
            let (start, end) = segments[index];
            if !group.is_empty() && start >= group_end {
                close(&mut group, &mut columns, &mut result);
                group_end = i64::MIN;
            }
            let column = match columns.iter().position(|&last| last <= start) {
                Some(column) => {
                    columns[column] = end;
                    column
                }
                None => {
                    columns.push(end);
                    columns.len() - 1
                }
            };
            result[index].0 = column;
            group.push(index);
            group_end = group_end.max(end);
        }
        close(&mut group, &mut columns, &mut result);
        result
    }

    /// 주 보기 시간 칸의 일정 하나.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Block {
        pub event: usize,
        pub start: i64,
        pub end: i64,
        pub col: usize,
        pub cols: usize,
        pub cont_before: bool,
        pub cont_after: bool,
    }

    impl Block {
        /// 제목과 시각을 두 줄로 놓기에 짧은 일정(45분 미만). 한 줄로 줄여 그린다.
        pub fn is_short(&self) -> bool {
            self.end - self.start < 45
        }

        pub fn style(&self) -> String {
            let cols = self.cols.max(1) as f64;
            format!(
                "top: {:.3}%; height: {:.3}%; left: {:.3}%; width: {:.3}%",
                percent(self.start),
                percent(self.end - self.start),
                self.col as f64 / cols * 100.0,
                100.0 / cols,
            )
        }
    }

    /// 하루 분(0~1440)을 시간 칸 높이의 퍼센트로.
    pub fn percent(minutes: i64) -> f64 {
        minutes.clamp(0, DAY_MINUTES) as f64 / DAY_MINUTES as f64 * 100.0
    }

    /// `date` 칸에 그릴 시간 일정(종일 줄에 가는 것은 뺀다).
    pub fn day_blocks(events: &[EventSummary], date: NaiveDate, tz: Tz) -> Vec<Block> {
        let segments: Vec<(usize, (i64, i64, bool, bool))> = events
            .iter()
            .enumerate()
            .filter(|(_, event)| !in_all_day_row(event))
            .filter_map(|(index, event)| day_segment(event, date, tz).map(|s| (index, s)))
            .collect();
        let shapes: Vec<(i64, i64)> = segments
            .iter()
            .map(|(_, (start, end, _, _))| (*start, (*end).max(start + MIN_BLOCK_MINUTES)))
            .collect();
        segments
            .into_iter()
            .zip(layout_columns(&shapes))
            .map(
                |((event, (start, end, cont_before, cont_after)), (col, cols))| Block {
                    event,
                    start,
                    end,
                    col,
                    cols,
                    cont_before,
                    cont_after,
                },
            )
            .collect()
    }

    /// 오늘 칸의 지금 선 위치(분). 다른 날이면 `None`.
    pub fn now_minute(now: Timestamp, date: NaiveDate, tz: Tz) -> Option<i64> {
        let (day, minute) = local(now, tz);
        (day == date).then_some(minute)
    }

    /// 주 보기를 처음 열 때 맨 위에 둘 시. 낮 근무면 7시(업무가 더 일찍 시작하면 그 시),
    /// 밤을 넘기는 근무면 업무 시작 한 시간 전.
    pub fn first_visible_hour(work_start: MinuteOfDay, work_end: MinuteOfDay) -> u32 {
        let start = u32::from(work_start.minutes() / 60);
        if work_end > work_start {
            start.min(7)
        } else {
            start.saturating_sub(1)
        }
    }

    fn hm(minutes: i64) -> String {
        format!("{:02}:{:02}", minutes / 60, minutes % 60)
    }

    /// 시간 칸 일정의 시각 `09:30 – 10:00`(그날 부분만).
    pub fn block_time(block: &Block) -> String {
        format!("{} – {}", hm(block.start), hm(block.end))
    }

    // ── 목록 ──

    /// 목록 보기의 하루.
    #[derive(Debug, Clone, PartialEq)]
    pub struct AgendaDay {
        pub date: NaiveDate,
        pub holidays: Vec<String>,
        /// [`Prepared::events`]의 순서. 종일 먼저, 그다음 그날 시작 시각순.
        pub events: Vec<usize>,
        pub leaves: Vec<DayEntry>,
    }

    /// 기간 안에서 무언가 있는 날만, 날짜순으로. 여러 날에 걸친 일정은 날마다 나온다.
    pub fn agenda(data: &Prepared, range: Range, tz: Tz) -> Vec<AgendaDay> {
        range
            .days()
            .into_iter()
            .filter_map(|date| {
                let mut events: Vec<(i64, usize)> = data
                    .events
                    .iter()
                    .enumerate()
                    .filter(|(_, event)| covers(event, date, tz))
                    .map(|(index, event)| {
                        let start = if event.all_day {
                            -1
                        } else {
                            day_segment(event, date, tz).map_or(0, |(start, ..)| start)
                        };
                        (start, index)
                    })
                    .collect();
                events.sort();
                let day = AgendaDay {
                    date,
                    holidays: data.holidays.get(&date).cloned().unwrap_or_default(),
                    events: events.into_iter().map(|(_, index)| index).collect(),
                    leaves: data.leaves.get(&date).cloned().unwrap_or_default(),
                };
                (!day.holidays.is_empty() || !day.events.is_empty() || !day.leaves.is_empty())
                    .then_some(day)
            })
            .collect()
    }

    /// 홈의 오늘 일정: 앞의 [`TODAY_SHOWN`]개와 남은 수.
    pub fn today_rows(
        events: &[EventSummary],
        today: NaiveDate,
        tz: Tz,
    ) -> (Vec<EventSummary>, usize) {
        let data = Prepared {
            events: events.to_vec(),
            ..Prepared::default()
        };
        let all: Vec<EventSummary> = agenda(
            &data,
            Range {
                from: today,
                to: today,
            },
            tz,
        )
        .into_iter()
        .flat_map(|day| day.events)
        .map(|index| data.events[index].clone())
        .collect();
        let hidden = all.len().saturating_sub(TODAY_SHOWN);
        (all.into_iter().take(TODAY_SHOWN).collect(), hidden)
    }

    // ── 문구 ──

    /// 목록 줄의 시간 칸: `종일`, `09:30 – 10:00`, 전날부터 이어지면 `– 01:00`, 다음 날로
    /// 이어지면 `23:00 –`, 하루를 다 덮으면 `종일`.
    pub fn agenda_time(event: &EventSummary, date: NaiveDate, tz: Tz) -> String {
        if event.all_day {
            return "종일".to_owned();
        }
        match day_segment(event, date, tz) {
            Some((_, _, true, true)) => "종일".to_owned(),
            Some((start, _, false, true)) => format!("{} –", hm(start)),
            Some((_, end, true, false)) => format!("– {}", hm(end)),
            Some((start, end, false, false)) => format!("{} – {}", hm(start), hm(end)),
            None => String::new(),
        }
    }

    /// 상세의 시간: `11월 20일 (금) 14:00 – 15:00`, 날이 바뀌면 끝에도 날짜, 종일이면
    /// `11월 20일 (금) 종일` · `11월 20일 (금) – 11월 22일 (일) 종일`.
    pub fn time_label(event: &EventSummary, tz: Tz) -> String {
        if event.all_day {
            return match event_days(event, tz) {
                Some((first, last)) if first == last => format!("{} 종일", format::date_md(first)),
                Some((first, last)) => format!(
                    "{} – {} 종일",
                    format::date_md(first),
                    format::date_md(last)
                ),
                None => "종일".to_owned(),
            };
        }
        let (Some(start), Some(end)) = (event.start_at, event.end_at) else {
            return String::new();
        };
        let (first, last) = (format::local_date(start, tz), format::local_date(end, tz));
        if first == last {
            format!(
                "{} {} – {}",
                format::date_md(first),
                format::time_hm(start, tz),
                format::time_hm(end, tz)
            )
        } else {
            format!(
                "{} {} – {} {}",
                format::date_md(first),
                format::time_hm(start, tz),
                format::date_md(last),
                format::time_hm(end, tz)
            )
        }
    }

    /// 월 보기 줄의 시작 시각 `09:30`. 종일이면 빈 문자열.
    pub fn short_time(event: &EventSummary, tz: Tz) -> String {
        match (event.all_day, event.start_at) {
            (false, Some(start)) => format::time_hm(start, tz),
            _ => String::new(),
        }
    }

    /// 초대받은 일정의 내 응답. 참석자가 아니면 `None`.
    pub fn invite_state(event: &EventSummary) -> Option<AttendeeResponse> {
        (event.my_role == EventRole::Attendee)
            .then(|| event.my_response.unwrap_or(AttendeeResponse::Pending))
    }

    /// 참석을 수락하기 전의 초대(테두리만 그린다).
    pub fn is_tentative(event: &EventSummary) -> bool {
        invite_state(event).is_some_and(|response| response != AttendeeResponse::Accepted)
    }

    pub const fn kind_class(kind: CalendarKind) -> &'static str {
        match kind {
            CalendarKind::Personal => "cal-personal",
            CalendarKind::Department => "cal-department",
            CalendarKind::Company => "cal-company",
        }
    }

    pub fn bar_class(event: &EventSummary, bar: Bar) -> String {
        let mut class = format!("cal-bar {}", kind_class(event.calendar_kind));
        if is_tentative(event) {
            class.push_str(" cal-tentative");
        }
        if bar.cont_before {
            class.push_str(" cal-bar-cont-before");
        }
        if bar.cont_after {
            class.push_str(" cal-bar-cont-after");
        }
        class
    }

    /// 일정의 캘린더 이름. 목록에 없으면(초대받은 다른 사람의 일정) `초대받은 일정`.
    pub fn calendar_name(calendars: &[CalendarDto], event: &EventSummary) -> String {
        match calendars.iter().find(|c| c.id == event.calendar_id) {
            Some(calendar) => calendar.name.clone(),
            None if event.my_role == EventRole::Attendee => "초대받은 일정".to_owned(),
            None => text::calendar_kind_label(event.calendar_kind).to_owned(),
        }
    }

    /// 일정 버튼의 접근성 이름. 색으로 나타내는 것(캘린더, 초대 응답)을 말로도 준다.
    pub fn event_aria(event: &EventSummary, calendar: &str, tz: Tz) -> String {
        let mut label = format!("{}, {}, {}", event.title, time_label(event, tz), calendar);
        if let Some(response) = invite_state(event) {
            label.push_str(&format!(
                ", 초대 {}",
                text::attendee_response_label(response)
            ));
        }
        label
    }

    /// 날짜 버튼의 접근성 이름: `11월 20일 (금), 공휴일 추석, 일정 3개`.
    pub fn day_aria(date: NaiveDate, holidays: &[String], count: usize) -> String {
        let mut label = format::date_md(date);
        if !holidays.is_empty() {
            label.push_str(&format!(", 공휴일 {}", holidays.join(", ")));
        }
        if count > 0 {
            label.push_str(&format!(", 일정 {count}개"));
        }
        label
    }

    /// 월 보기 칸의 클래스: 오늘, 쉬는 날(근무 요일이 아니거나 공휴일).
    pub fn day_class(date: NaiveDate, today: NaiveDate, week: WorkWeek, holiday: bool) -> String {
        let mut class = "cal-day".to_owned();
        if is_day_off(week, date) || holiday {
            class.push_str(" cal-day-off");
        }
        if holiday {
            class.push_str(" cal-day-holiday");
        }
        if date == today {
            class.push_str(" cal-day-today");
        }
        class
    }

    /// 주 보기 날짜 머리의 클래스.
    pub fn week_head_class(
        date: NaiveDate,
        today: NaiveDate,
        week: WorkWeek,
        holiday: bool,
    ) -> String {
        day_class(date, today, week, holiday).replace("cal-day", "cal-week-day")
    }

    /// 휴가 칩 `홍길동 휴가`, 반차 이하면 `홍길동 휴가(오전½)`.
    pub fn leave_chip(entry: &DayEntry) -> String {
        match entry.unit {
            LeaveUnit::FullDay => format!("{} 휴가", entry.member_name),
            unit => format!(
                "{} 휴가({})",
                entry.member_name,
                text::leave_unit_short(unit)
            ),
        }
    }

    /// 휴가 칩의 풀어 쓴 설명 `홍길동 오전 반차, 결재 중`.
    pub fn leave_aria(entry: &DayEntry) -> String {
        format!(
            "{} {} 휴가, {}",
            entry.member_name,
            text::leave_unit_label(entry.unit),
            text::leave_status_label(entry.status())
        )
    }

    pub fn respond_message(status: AttendeeResponse) -> &'static str {
        match status {
            AttendeeResponse::Declined => "불참으로 답했어요.",
            _ => "참석으로 답했어요.",
        }
    }

    pub fn delete_message(event: &EventDetail) -> String {
        if event.attendees.is_empty() {
            format!(
                "'{}' 일정을 삭제해요. 되돌릴 수 없어요.",
                event.summary.title
            )
        } else {
            format!(
                "'{}' 일정을 삭제해요. 참석자 {}명에게 취소 알림이 가고, 되돌릴 수 없어요.",
                event.summary.title,
                event.attendees.len()
            )
        }
    }

    #[cfg(test)]
    pub mod fixtures {
        use super::*;
        use pango_core::id::{CalendarEventId, MemberId};

        pub fn d(value: &str) -> NaiveDate {
            value.parse().expect("date")
        }

        pub fn seoul() -> Tz {
            chrono_tz::Asia::Seoul
        }

        /// KST 벽시계 `2026-11-20T09:30` → 시각.
        pub fn kst(value: &str) -> Timestamp {
            format::parse_local_input(value, seoul()).expect("kst")
        }

        fn base(title: &str) -> EventSummary {
            EventSummary {
                id: CalendarEventId::new(),
                calendar_id: CalendarId::new(),
                calendar_kind: CalendarKind::Personal,
                title: title.into(),
                all_day: false,
                start_date: None,
                end_date: None,
                start_at: None,
                end_at: None,
                location: None,
                my_role: EventRole::Owner,
                my_response: None,
                attendee_count: 0,
                version: 1,
            }
        }

        pub fn all_day(title: &str, start: &str, end: &str) -> EventSummary {
            EventSummary {
                all_day: true,
                start_date: Some(d(start)),
                end_date: Some(d(end)),
                ..base(title)
            }
        }

        pub fn timed(title: &str, start: &str, end: &str) -> EventSummary {
            EventSummary {
                start_at: Some(kst(start)),
                end_at: Some(kst(end)),
                ..base(title)
            }
        }

        pub fn leave(name: &str, unit: LeaveUnit, pending: bool) -> DayEntry {
            DayEntry {
                member_id: MemberId::new(),
                member_name: name.into(),
                unit,
                pending,
            }
        }

        pub fn data(events: Vec<EventSummary>) -> Prepared {
            Prepared {
                events,
                ..Prepared::default()
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::fixtures::*;
        use super::*;
        use pango_api::admin::HolidayDto;
        use pango_api::leave::LeaveCalendarEntry;
        use pango_core::leave::LeaveRequestStatus;

        fn range(from: &str, to: &str) -> Range {
            Range {
                from: d(from),
                to: d(to),
            }
        }

        fn calendar(kind: CalendarKind, name: &str) -> CalendarDto {
            CalendarDto {
                id: CalendarId::new(),
                kind,
                name: name.into(),
                department_id: None,
                can_write: kind == CalendarKind::Personal,
            }
        }

        #[test]
        fn visible_ranges_cover_whole_weeks_and_stay_within_the_api_limit() {
            let today = d("2026-11-20");
            // 2026년 11월: 11/1(일)~12/5(토).
            assert_eq!(
                visible_range(Mode::Month, today, false),
                range("2026-11-01", "2026-12-05")
            );
            // 2026년 8월은 6주: 7/26~9/5, 42일.
            let august = visible_range(Mode::Month, d("2026-08-15"), false);
            assert_eq!(august, range("2026-07-26", "2026-09-05"));
            assert_eq!(august.len(), 42);
            assert!(august.len() <= MAX_VIEW_DAYS);
            assert_eq!(
                visible_range(Mode::Week, today, false),
                range("2026-11-15", "2026-11-21")
            );
            assert_eq!(
                visible_range(Mode::Week, today, true),
                range("2026-11-20", "2026-11-22")
            );
            // 연말을 넘는 주.
            assert_eq!(
                visible_range(Mode::Week, d("2026-12-31"), false),
                range("2026-12-27", "2027-01-02")
            );
            assert_eq!(
                visible_range(Mode::List, today, false),
                range("2026-11-01", "2026-11-30")
            );
        }

        #[test]
        fn stepping_moves_by_the_view_unit() {
            let today = d("2026-11-20");
            assert_eq!(
                step(Mode::Month, today, true, false, today),
                d("2026-12-01")
            );
            assert_eq!(
                step(Mode::Month, d("2026-12-01"), false, false, today),
                today,
                "오늘이 있는 달로 돌아오면 오늘"
            );
            assert_eq!(
                step(Mode::List, d("2026-01-15"), false, false, today),
                d("2025-12-01")
            );
            assert_eq!(step(Mode::Week, today, true, false, today), d("2026-11-27"));
            assert_eq!(step(Mode::Week, today, false, true, today), d("2026-11-17"));
            // 달력 한도(2000~2100년) 밖으로는 가지 않는다.
            assert_eq!(
                step(Mode::Month, d("2000-01-10"), false, false, today),
                d("2000-01-10")
            );
        }

        #[test]
        fn titles_name_both_months_when_a_week_spans_them() {
            let anchor = d("2026-11-20");
            assert_eq!(
                title(Mode::Month, range("2026-11-01", "2026-12-05"), anchor),
                "2026년 11월"
            );
            assert_eq!(
                title(Mode::Week, range("2026-11-15", "2026-11-21"), anchor),
                "2026년 11월"
            );
            assert_eq!(
                title(Mode::Week, range("2026-11-29", "2026-12-05"), anchor),
                "2026년 11월 – 12월"
            );
            assert_eq!(
                title(Mode::Week, range("2026-12-27", "2027-01-02"), anchor),
                "2026년 12월 – 2027년 1월"
            );
        }

        #[test]
        fn view_path_selects_calendars_only_when_some_are_hidden() {
            let mine = calendar(CalendarKind::Personal, "내 일정");
            let company = calendar(CalendarKind::Company, "전사 일정");
            let all = vec![mine.clone(), company.clone()];
            assert_eq!(selected_ids(&all, &BTreeSet::new()), None);
            assert_eq!(
                selected_ids(&all, &BTreeSet::from([company.id])),
                Some(vec![mine.id])
            );
            assert_eq!(
                selected_ids(&all, &BTreeSet::from([mine.id, company.id])),
                None,
                "모두 숨기면 전부 읽고 화면에서 거른다"
            );
            assert_eq!(selected_ids(&[], &BTreeSet::from([mine.id])), None);

            let path = view_path(range("2026-11-01", "2026-12-05"), None);
            assert_eq!(path, "/calendar/view?from=2026-11-01&to=2026-12-05");
            let path = view_path(
                range("2026-11-01", "2026-11-30"),
                Some(&[mine.id, company.id]),
            );
            assert_eq!(
                path,
                format!(
                    "/calendar/view?from=2026-11-01&to=2026-11-30&calendars={}%2C{}",
                    mine.id, company.id
                )
            );
            // 한도를 넘는 기간은 62일로 자른다.
            assert_eq!(
                view_path(range("2026-01-01", "2026-12-31"), None),
                "/calendar/view?from=2026-01-01&to=2026-03-03"
            );
        }

        #[test]
        fn hidden_calendars_and_their_invitations_are_filtered() {
            let mine = calendar(CalendarKind::Personal, "내 일정");
            let company = calendar(CalendarKind::Company, "전사 일정");
            let mut own = timed("내 회의", "2026-11-20T09:00", "2026-11-20T10:00");
            own.calendar_id = mine.id;
            let mut shared = all_day("창립 기념일", "2026-11-20", "2026-11-20");
            shared.calendar_id = company.id;
            shared.calendar_kind = CalendarKind::Company;
            let mut invited = timed("초대", "2026-11-20T11:00", "2026-11-20T12:00");
            invited.my_role = EventRole::Attendee;
            let mut filter = Filter {
                personal: Some(mine.id),
                ..Filter::default()
            };
            assert!([&own, &shared, &invited]
                .iter()
                .all(|e| event_shown(e, &filter)));
            filter.hidden.insert(mine.id);
            assert!(!event_shown(&own, &filter));
            assert!(!event_shown(&invited, &filter), "초대는 내 일정에 딸린다");
            assert!(event_shown(&shared, &filter));
            filter.hidden = BTreeSet::from([company.id]);
            assert!(!event_shown(&shared, &filter));
            assert!(event_shown(&invited, &filter));
        }

        #[test]
        fn prepare_keeps_overlays_in_range_and_only_when_on() {
            let view = CalendarView {
                events: vec![timed("a", "2026-11-20T09:00", "2026-11-20T10:00")],
                holidays: vec![
                    HolidayDto {
                        date: d("2026-11-20"),
                        name: "기념일".into(),
                    },
                    HolidayDto {
                        date: d("2026-12-25"),
                        name: "성탄절".into(),
                    },
                ],
                leaves: vec![LeaveCalendarEntry {
                    member_id: pango_core::id::MemberId::new(),
                    member_name: "홍길동".into(),
                    // 금~월: 주말은 빠진다.
                    start_date: d("2026-11-20"),
                    end_date: d("2026-11-23"),
                    unit: LeaveUnit::FullDay,
                    status: LeaveRequestStatus::Pending,
                }],
            };
            let on = Filter {
                holidays: true,
                leaves: true,
                ..Filter::default()
            };
            let week = WorkWeek::MON_TO_FRI;
            let prepared = prepare(&view, &on, range("2026-11-15", "2026-11-28"), week);
            assert_eq!(prepared.events.len(), 1);
            assert_eq!(
                prepared.holidays.keys().copied().collect::<Vec<_>>(),
                [d("2026-11-20")]
            );
            assert_eq!(
                prepared.leaves.keys().copied().collect::<Vec<_>>(),
                [d("2026-11-20"), d("2026-11-23")]
            );
            assert!(prepared.leaves[&d("2026-11-20")][0].pending);
            let off = prepare(
                &view,
                &Filter::default(),
                range("2026-11-15", "2026-11-28"),
                week,
            );
            assert!(off.holidays.is_empty() && off.leaves.is_empty());
        }

        #[test]
        fn event_days_treat_the_end_instant_as_exclusive() {
            let tz = seoul();
            let one = timed("a", "2026-11-20T09:00", "2026-11-20T10:00");
            assert_eq!(
                event_days(&one, tz),
                Some((d("2026-11-20"), d("2026-11-20")))
            );
            // 자정에 끝나면 그날까지만.
            let to_midnight = timed("b", "2026-11-20T22:00", "2026-11-21T00:00");
            assert_eq!(
                event_days(&to_midnight, tz),
                Some((d("2026-11-20"), d("2026-11-20")))
            );
            assert!(!is_bar(&to_midnight, tz));
            // 자정을 넘으면 이틀.
            let overnight = timed("c", "2026-11-20T23:00", "2026-11-21T01:00");
            assert_eq!(
                event_days(&overnight, tz),
                Some((d("2026-11-20"), d("2026-11-21")))
            );
            assert!(is_bar(&overnight, tz));
            assert!(
                !in_all_day_row(&overnight),
                "24시간 미만은 시간 칸에 나눠 그린다"
            );
            assert!(in_all_day_row(&timed(
                "d",
                "2026-11-20T09:00",
                "2026-11-21T09:00"
            )));
            let single = all_day("e", "2026-11-20", "2026-11-20");
            assert_eq!(
                event_days(&single, tz),
                Some((d("2026-11-20"), d("2026-11-20")))
            );
            let missing_end = EventSummary {
                end_date: None,
                ..single.clone()
            };
            assert_eq!(
                event_days(&missing_end, tz),
                Some((d("2026-11-20"), d("2026-11-20")))
            );
            // UTC로는 전날이어도 회사 시간대 날짜를 쓴다(KST 00:30 = UTC 전날 15:30).
            let early = timed("f", "2026-11-20T00:30", "2026-11-20T01:00");
            assert_eq!(
                event_days(&early, tz),
                Some((d("2026-11-20"), d("2026-11-20")))
            );
        }

        #[test]
        fn lanes_never_collide() {
            assert!(assign_lanes(&[]).is_empty());
            assert_eq!(assign_lanes(&[(0, 6)]), [0]);
            // 겹치지 않으면 같은 줄을 다시 쓴다.
            assert_eq!(assign_lanes(&[(0, 1), (2, 3), (4, 6)]), [0, 0, 0]);
            // 셋이 겹치면 세 줄, 넷째는 비는 줄로.
            assert_eq!(
                assign_lanes(&[(0, 2), (1, 3), (2, 4), (3, 5)]),
                [0, 1, 2, 0]
            );
            // 같은 날 시작이면 긴 것이 위.
            assert_eq!(assign_lanes(&[(1, 1), (1, 5)]), [1, 0]);
            let spans = [(0, 6), (0, 0), (1, 2), (2, 2), (3, 6), (5, 5), (6, 6)];
            let lanes = assign_lanes(&spans);
            for i in 0..spans.len() {
                for j in (i + 1)..spans.len() {
                    let overlap = spans[i].0 <= spans[j].1 && spans[j].0 <= spans[i].1;
                    assert!(!(overlap && lanes[i] == lanes[j]), "{i} {j}");
                }
            }
        }

        fn week_of(sunday: &str) -> Vec<NaiveDate> {
            Range {
                from: d(sunday),
                to: d(sunday) + Duration::days(6),
            }
            .days()
        }

        #[test]
        fn bars_are_clipped_to_the_week_and_marked_as_continuing() {
            let tz = seoul();
            // 10/28(수)~11/3(화): 11/1 주에서는 일~화 3칸, 앞에서 이어짐.
            let events = vec![
                all_day("출장", "2026-10-28", "2026-11-03"),
                all_day("워크숍", "2026-11-06", "2026-11-09"),
            ];
            let week = week_of("2026-11-01");
            let bars = row_bars(&events, &[0, 1], &week, tz);
            assert_eq!(
                bars,
                [
                    Bar {
                        event: 0,
                        start: 0,
                        span: 3,
                        lane: 0,
                        cont_before: true,
                        cont_after: false
                    },
                    Bar {
                        event: 1,
                        start: 5,
                        span: 2,
                        lane: 0,
                        cont_before: false,
                        cont_after: true
                    },
                ]
            );
            // 31일짜리 일정은 다섯 주에 걸쳐 잘린다.
            let long = vec![all_day("긴 일정", "2026-11-01", "2026-12-01")];
            let spans: Vec<usize> = YearMonth::of(d("2026-11-01"))
                .full_weeks()
                .iter()
                .flat_map(|w| row_bars(&long, &[0], w, tz))
                .map(|b| b.span)
                .collect();
            assert_eq!(spans, [7, 7, 7, 7, 3]);
            assert!(row_bars(&long, &[0], &[], tz).is_empty());
            assert!(row_bars(&long, &[0], &week_of("2026-12-06"), tz).is_empty());
        }

        #[test]
        fn month_cells_cap_rows_and_count_the_rest() {
            let tz = seoul();
            let week = week_of("2026-11-15");
            let friday = d("2026-11-20");
            let mut prepared = data(vec![
                all_day("A", "2026-11-16", "2026-11-20"),
                all_day("B", "2026-11-18", "2026-11-21"),
                timed("회의", "2026-11-20T09:30", "2026-11-20T10:00"),
                timed("점심", "2026-11-20T12:00", "2026-11-20T13:00"),
            ]);
            prepared
                .leaves
                .insert(friday, vec![leave("홍길동", LeaveUnit::AmHalf, true)]);
            let cells = week_cells(&prepared, &week, tz);
            // 월(1번 칸): A가 시작.
            assert!(matches!(
                cells[1].lanes[..],
                [Lane::Start(Bar {
                    event: 0,
                    span: 5,
                    ..
                })]
            ));
            // 수(3번 칸): A는 지나가고 B가 둘째 줄에서 시작.
            assert!(matches!(
                cells[3].lanes[..],
                [
                    Lane::Covered,
                    Lane::Start(Bar {
                        event: 1,
                        lane: 1,
                        ..
                    })
                ]
            ));
            // 금: 막대 두 줄 + 시간 일정 한 줄, 나머지(점심·휴가) 둘은 +2.
            assert_eq!(cells[5].lanes, [Lane::Covered, Lane::Covered]);
            assert_eq!(cells[5].items, [DayItem::Event(2)]);
            assert_eq!(cells[5].hidden, 2);
            // 토: A는 끝났지만 B 줄 높이를 맞추는 빈자리가 있다.
            assert_eq!(cells[6].lanes, [Lane::Empty, Lane::Covered]);
            // 빈 날.
            assert_eq!(cells[0], DayCell::default());
        }

        #[test]
        fn bars_beyond_the_visible_rows_are_counted_on_every_day_they_cross() {
            let tz = seoul();
            let week = week_of("2026-11-15");
            let prepared = data(vec![
                all_day("1", "2026-11-16", "2026-11-18"),
                all_day("2", "2026-11-16", "2026-11-18"),
                all_day("3", "2026-11-16", "2026-11-18"),
                all_day("4", "2026-11-17", "2026-11-17"),
            ]);
            let cells = week_cells(&prepared, &week, tz);
            assert_eq!(cells[1].lanes.len(), MAX_ROWS);
            assert_eq!(cells[1].hidden, 0);
            assert_eq!(cells[2].hidden, 1, "넷째 줄 막대는 +1");
            assert!(cells[2].items.is_empty());
            assert_eq!(cells[3].hidden, 0);
        }

        #[test]
        fn overnight_timed_events_become_bars_in_the_month() {
            let tz = seoul();
            let prepared = data(vec![timed("야간", "2026-11-20T23:00", "2026-11-21T01:00")]);
            let cells = week_cells(&prepared, &week_of("2026-11-15"), tz);
            assert!(matches!(
                cells[5].lanes[..],
                [Lane::Start(Bar { span: 2, .. })]
            ));
            assert_eq!(cells[6].lanes, [Lane::Covered]);
            // 토요일에서 끝나고 다음 주로 넘어가는 것은 다음 주 첫 칸에서 이어진다.
            let crossing = data(vec![timed("주말", "2026-11-21T22:00", "2026-11-22T02:00")]);
            let next = week_cells(&crossing, &week_of("2026-11-22"), tz);
            assert!(matches!(
                next[0].lanes[..],
                [Lane::Start(Bar {
                    cont_before: true,
                    span: 1,
                    ..
                })]
            ));
            let month = month_cells(&crossing, YearMonth::of(d("2026-11-01")), tz);
            assert_eq!(month.len(), 35);
        }

        #[test]
        fn day_segments_split_events_at_company_midnight() {
            let tz = seoul();
            let overnight = timed("야간", "2026-11-20T23:00", "2026-11-21T01:30");
            assert_eq!(
                day_segment(&overnight, d("2026-11-20"), tz),
                Some((1380, 1440, false, true))
            );
            assert_eq!(
                day_segment(&overnight, d("2026-11-21"), tz),
                Some((0, 90, true, false))
            );
            assert_eq!(day_segment(&overnight, d("2026-11-22"), tz), None);
            let to_midnight = timed("늦게", "2026-11-20T22:00", "2026-11-21T00:00");
            assert_eq!(
                day_segment(&to_midnight, d("2026-11-20"), tz),
                Some((1320, 1440, false, false))
            );
            assert_eq!(day_segment(&to_midnight, d("2026-11-21"), tz), None);
            let three_days = timed("긴", "2026-11-20T09:00", "2026-11-22T09:00");
            assert_eq!(
                day_segment(&three_days, d("2026-11-21"), tz),
                Some((0, 1440, true, true))
            );
            assert_eq!(
                day_segment(
                    &all_day("종일", "2026-11-20", "2026-11-20"),
                    d("2026-11-20"),
                    tz
                ),
                None
            );
        }

        #[test]
        fn overlapping_blocks_share_the_width() {
            assert!(layout_columns(&[]).is_empty());
            assert_eq!(layout_columns(&[(540, 600)]), [(0, 1)]);
            // 이어 붙은(겹치지 않는) 일정은 각자 전체 폭.
            assert_eq!(layout_columns(&[(540, 600), (600, 660)]), [(0, 1), (0, 1)]);
            // 셋이 모두 겹친다.
            assert_eq!(
                layout_columns(&[(540, 660), (570, 630), (600, 720)]),
                [(0, 3), (1, 3), (2, 3)]
            );
            // 넷 이상: 같은 시각에 시작하면 긴 것이 왼쪽, 먼저 끝난 열을 다시 쓰고, 묶음 전체가 세 열.
            assert_eq!(
                layout_columns(&[(540, 600), (540, 720), (560, 620), (600, 660), (630, 700)]),
                [(1, 3), (0, 3), (2, 3), (1, 3), (2, 3)]
            );
            // 따로 떨어진 묶음은 열 수를 따로 센다.
            assert_eq!(
                layout_columns(&[(540, 600), (550, 610), (800, 860)]),
                [(0, 2), (1, 2), (0, 1)]
            );
        }

        #[test]
        fn day_blocks_mix_all_day_and_timed_and_keep_short_events_visible() {
            let tz = seoul();
            let events = vec![
                all_day("종일", "2026-11-20", "2026-11-20"),
                timed("짧은", "2026-11-20T09:00", "2026-11-20T09:10"),
                timed("바로 뒤", "2026-11-20T09:15", "2026-11-20T10:00"),
                timed("하루 넘게", "2026-11-19T09:00", "2026-11-21T09:00"),
                timed("야간", "2026-11-19T23:00", "2026-11-20T01:00"),
            ];
            let blocks = day_blocks(&events, d("2026-11-20"), tz);
            let shown: Vec<(usize, usize, usize)> =
                blocks.iter().map(|b| (b.event, b.col, b.cols)).collect();
            // 10분짜리도 30분 높이로 그리므로 9:15 일정과 나란히 놓는다. 종일·24시간 이상은 빠진다.
            assert_eq!(shown, [(1, 0, 2), (2, 1, 2), (4, 0, 1)]);
            let night = blocks[2];
            assert_eq!((night.start, night.end, night.cont_before), (0, 60, true));
            assert!(day_blocks(&events, d("2026-11-25"), tz).is_empty());
            assert_eq!(
                blocks[0].style(),
                "top: 37.500%; height: 0.694%; left: 0.000%; width: 50.000%"
            );
            assert_eq!(block_time(&blocks[0]), "09:00 – 09:10");
            assert!(blocks[0].is_short() && !blocks[2].is_short());
        }

        #[test]
        fn all_day_row_stacks_bars_then_leaves() {
            let tz = seoul();
            let days = week_of("2026-11-15");
            let mut prepared = data(vec![
                all_day("A", "2026-11-16", "2026-11-17"),
                timed("하루 넘게", "2026-11-17T09:00", "2026-11-18T09:00"),
                timed("짧은", "2026-11-17T09:00", "2026-11-17T10:00"),
            ]);
            prepared.leaves.insert(
                d("2026-11-18"),
                vec![
                    leave("가", LeaveUnit::FullDay, false),
                    leave("나", LeaveUnit::FullDay, false),
                    leave("다", LeaveUnit::FullDay, true),
                ],
            );
            prepared
                .leaves
                .insert(d("2026-11-19"), vec![leave("라", LeaveUnit::PmHalf, false)]);
            let rows = all_day_rows(&prepared, &days, tz);
            assert_eq!(rows.bars.len(), 2);
            assert_eq!(rows.bars.iter().map(|b| b.lane).collect::<Vec<_>>(), [0, 1]);
            let slots: Vec<(usize, usize, Option<&str>, usize)> = rows
                .leaves
                .iter()
                .map(|s| {
                    (
                        s.col,
                        s.row,
                        s.entry.as_ref().map(|e| e.member_name.as_str()),
                        s.more,
                    )
                })
                .collect();
            assert_eq!(
                slots,
                [
                    (3, 2, Some("가"), 0),
                    (3, 3, None, 2),
                    (4, 2, Some("라"), 0)
                ]
            );
            assert_eq!(rows.rows, 4);
            assert_eq!(all_day_rows(&data(vec![]), &days, tz).rows, 0);
        }

        #[test]
        fn now_line_and_first_hour() {
            let tz = seoul();
            let at = kst("2026-11-20T14:45");
            assert_eq!(now_minute(at, d("2026-11-20"), tz), Some(885));
            assert_eq!(now_minute(at, d("2026-11-21"), tz), None);
            let hm = |v: &str| MinuteOfDay::parse(v).expect("time");
            assert_eq!(first_visible_hour(hm("09:00"), hm("18:00")), 7);
            assert_eq!(first_visible_hour(hm("06:00"), hm("15:00")), 6);
            assert_eq!(first_visible_hour(hm("22:00"), hm("07:00")), 21);
            assert_eq!(first_visible_hour(hm("00:30"), hm("00:00")), 0);
            assert_eq!(percent(720), 50.0);
            assert_eq!(percent(-5), 0.0);
            assert_eq!(percent(2000), 100.0);
        }

        #[test]
        fn agenda_groups_days_and_repeats_multi_day_events() {
            let tz = seoul();
            let mut prepared = data(vec![
                timed("야간", "2026-11-20T23:00", "2026-11-21T01:00"),
                all_day("출장", "2026-11-20", "2026-11-21"),
                timed("아침", "2026-11-20T08:00", "2026-11-20T09:00"),
            ]);
            prepared
                .holidays
                .insert(d("2026-11-23"), vec!["대체공휴일".into()]);
            prepared.leaves.insert(
                d("2026-11-24"),
                vec![leave("홍길동", LeaveUnit::FullDay, false)],
            );
            let days = agenda(&prepared, range("2026-11-19", "2026-11-25"), tz);
            let summary: Vec<(NaiveDate, Vec<usize>)> = days
                .iter()
                .map(|day| (day.date, day.events.clone()))
                .collect();
            assert_eq!(
                summary,
                [
                    (d("2026-11-20"), vec![1, 2, 0]),
                    (d("2026-11-21"), vec![1, 0]),
                    (d("2026-11-23"), vec![]),
                    (d("2026-11-24"), vec![]),
                ]
            );
            assert_eq!(days[2].holidays, ["대체공휴일"]);
            assert_eq!(days[3].leaves.len(), 1);
            assert!(agenda(&data(vec![]), range("2026-11-01", "2026-11-30"), tz).is_empty());
        }

        #[test]
        fn agenda_time_describes_the_part_of_the_day() {
            let tz = seoul();
            let overnight = timed("야간", "2026-11-20T23:00", "2026-11-21T01:00");
            assert_eq!(agenda_time(&overnight, d("2026-11-20"), tz), "23:00 –");
            assert_eq!(agenda_time(&overnight, d("2026-11-21"), tz), "– 01:00");
            let long = timed("긴", "2026-11-19T09:00", "2026-11-21T09:00");
            assert_eq!(agenda_time(&long, d("2026-11-20"), tz), "종일");
            let normal = timed("회의", "2026-11-20T14:00", "2026-11-20T15:00");
            assert_eq!(agenda_time(&normal, d("2026-11-20"), tz), "14:00 – 15:00");
            assert_eq!(agenda_time(&normal, d("2026-11-21"), tz), "");
            let late = timed("늦게", "2026-11-20T22:00", "2026-11-21T00:00");
            assert_eq!(agenda_time(&late, d("2026-11-20"), tz), "22:00 – 24:00");
            assert_eq!(
                agenda_time(
                    &all_day("휴무", "2026-11-20", "2026-11-20"),
                    d("2026-11-20"),
                    tz
                ),
                "종일"
            );
        }

        #[test]
        fn today_rows_cap_and_count() {
            let tz = seoul();
            let events: Vec<EventSummary> = (0..7)
                .map(|hour| {
                    timed(
                        &format!("{hour}"),
                        &format!("2026-11-20T{:02}:00", hour + 9),
                        &format!("2026-11-20T{:02}:30", hour + 9),
                    )
                })
                .collect();
            let (shown, hidden) = today_rows(&events, d("2026-11-20"), tz);
            assert_eq!(shown.len(), TODAY_SHOWN);
            assert_eq!(hidden, 2);
            assert_eq!(today_rows(&[], d("2026-11-20"), tz), (vec![], 0));
        }

        #[test]
        fn labels_describe_time_calendar_and_invitation() {
            let tz = seoul();
            let meeting = timed("회의", "2026-11-20T14:00", "2026-11-20T15:00");
            assert_eq!(time_label(&meeting, tz), "11월 20일 (금) 14:00 – 15:00");
            let overnight = timed("야간", "2026-11-20T23:00", "2026-11-21T01:00");
            assert_eq!(
                time_label(&overnight, tz),
                "11월 20일 (금) 23:00 – 11월 21일 (토) 01:00"
            );
            assert_eq!(
                time_label(&all_day("휴무", "2026-11-20", "2026-11-20"), tz),
                "11월 20일 (금) 종일"
            );
            assert_eq!(
                time_label(&all_day("출장", "2026-11-20", "2026-11-22"), tz),
                "11월 20일 (금) – 11월 22일 (일) 종일"
            );
            assert_eq!(short_time(&meeting, tz), "14:00");
            assert_eq!(
                short_time(&all_day("휴무", "2026-11-20", "2026-11-20"), tz),
                ""
            );

            let mine = calendar(CalendarKind::Personal, "내 일정");
            let mut own = meeting.clone();
            own.calendar_id = mine.id;
            assert_eq!(calendar_name(std::slice::from_ref(&mine), &own), "내 일정");
            assert_eq!(invite_state(&own), None);
            assert!(!is_tentative(&own));
            assert_eq!(
                event_aria(&own, "내 일정", tz),
                "회의, 11월 20일 (금) 14:00 – 15:00, 내 일정"
            );

            let mut invited = meeting.clone();
            invited.my_role = EventRole::Attendee;
            assert_eq!(calendar_name(&[mine], &invited), "초대받은 일정");
            assert_eq!(invite_state(&invited), Some(AttendeeResponse::Pending));
            assert!(is_tentative(&invited));
            assert!(event_aria(&invited, "초대받은 일정", tz).ends_with(", 초대 응답 대기"));
            invited.my_response = Some(AttendeeResponse::Accepted);
            assert!(!is_tentative(&invited));
            invited.my_response = Some(AttendeeResponse::Declined);
            assert!(is_tentative(&invited));
            let bar = Bar {
                event: 0,
                start: 0,
                span: 2,
                lane: 0,
                cont_before: true,
                cont_after: false,
            };
            assert_eq!(
                bar_class(&invited, bar),
                "cal-bar cal-personal cal-tentative cal-bar-cont-before"
            );
            let mut department = meeting;
            department.calendar_kind = CalendarKind::Department;
            assert_eq!(calendar_name(&[], &department), "부서 일정");
        }

        #[test]
        fn day_labels_and_classes() {
            let week = WorkWeek::MON_TO_FRI;
            let today = d("2026-11-20");
            assert_eq!(day_aria(today, &[], 0), "11월 20일 (금)");
            assert_eq!(
                day_aria(today, &["기념일".to_owned()], 3),
                "11월 20일 (금), 공휴일 기념일, 일정 3개"
            );
            assert_eq!(
                day_class(today, today, week, false),
                "cal-day cal-day-today"
            );
            assert_eq!(
                day_class(d("2026-11-21"), today, week, false),
                "cal-day cal-day-off"
            );
            assert_eq!(
                day_class(d("2026-11-19"), today, week, true),
                "cal-day cal-day-off cal-day-holiday"
            );
            assert_eq!(
                week_head_class(today, today, week, false),
                "cal-week-day cal-week-day-today"
            );
            let count = data(vec![
                all_day("A", "2026-11-19", "2026-11-21"),
                timed("B", "2026-11-20T09:00", "2026-11-20T10:00"),
            ]);
            assert_eq!(day_count(&count, today, chrono_tz::Asia::Seoul), 2);
            assert_eq!(
                day_count(&count, d("2026-11-22"), chrono_tz::Asia::Seoul),
                0
            );
        }

        #[test]
        fn leave_chips_name_the_unit_and_state() {
            let full = leave("홍길동", LeaveUnit::FullDay, false);
            assert_eq!(leave_chip(&full), "홍길동 휴가");
            assert_eq!(leave_aria(&full), "홍길동 종일 휴가, 승인");
            let half = leave("김", LeaveUnit::AmHalf, true);
            assert_eq!(leave_chip(&half), "김 휴가(오전½)");
            assert!(
                leave_aria(&half).ends_with(text::leave_status_label(LeaveRequestStatus::Pending))
            );
        }
    }
}
