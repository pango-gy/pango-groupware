//! 근태: 오늘 출퇴근과 월별 기록.
//!
//! 월별 기록(`MonthAttendance`)은 팀 현황 화면이 구성원 한 명의 달을 보여 줄 때도 그대로 쓴다.

use dioxus::prelude::*;
use pango_api::attendance::{
    AttendanceEventDto, AttendanceRecordDto, MonthRecords, MonthlySummaryDto, TodayResponse,
};
use pango_api::Empty;
use pango_core::attendance::WorkType;
use pango_core::id::{AttendanceRecordId, MemberId};

use crate::api;
use crate::calendar::YearMonth;
use crate::format;
use crate::session::use_session;
use crate::text;
use crate::ui::{
    report_error, report_success, resource_view, today, Badge, Button, Card, EmptyState, IconName,
    Modal, MonthSwitcher, PageHeader, Stat, TextField, Tone,
};

use logic::{DayRow, MonthEndpoint, TodayStage};

#[component]
pub fn Attendance() -> Element {
    // 출퇴근하면 아래 월별 기록도 다시 읽는다.
    let mut revision = use_signal(|| 0_u32);
    rsx! {
        PageHeader { title: "근태" }
        TodayCard { onchange: move |_| *revision.write() += 1 }
        MonthAttendance { member_id: None, revision: revision() }
    }
}

/// "오늘" 카드: 큰 출근·퇴근 버튼.
#[component]
fn TodayCard(onchange: EventHandler<()>) -> Element {
    let session = use_session();
    let tz = session.zone();
    let mut today = use_resource(|| async { api::get::<TodayResponse>("/attendance/today").await });
    let mut now = use_signal(crate::ui::now);
    let mut work_type = use_signal(|| WorkType::Office);
    let mut note = use_signal(String::new);
    let mut busy = use_signal(|| false);

    // 경과 시간 표시를 위해 30초마다 시각을 갱신한다.
    use_future(move || async move {
        loop {
            gloo_timers::future::TimeoutFuture::new(30_000).await;
            now.set(crate::ui::now());
        }
    });

    let clock_in = move |_| async move {
        if busy() {
            return;
        }
        busy.set(true);
        let request = logic::clock_in_request(work_type(), &note());
        let result = api::post::<_, AttendanceRecordDto>("/attendance/clock-in", &request).await;
        busy.set(false);
        match result {
            Ok(_) => {
                note.set(String::new());
                report_success("출근했습니다.");
            }
            Err(error) => report_error(&error),
        }
        now.set(crate::ui::now());
        today.restart();
        onchange.call(());
    };
    let clock_out = move |_| async move {
        if busy() {
            return;
        }
        busy.set(true);
        let result = api::post::<_, AttendanceRecordDto>("/attendance/clock-out", &Empty {}).await;
        busy.set(false);
        match result {
            Ok(_) => report_success("퇴근했습니다. 수고하셨습니다."),
            Err(error) => report_error(&error),
        }
        now.set(crate::ui::now());
        today.restart();
        onchange.call(());
    };

    let body = resource_view(today, move |state: TodayResponse| {
        let stage = logic::today_stage(&state);
        let day_note = logic::day_note(state.is_working_day, state.holiday_name.as_deref());
        let hours = logic::expected_hours(state.expected_start, state.expected_end);
        let status = state.evaluation.status;
        rsx! {
            div { class: "today-head",
                div {
                    p { class: "today-date", {format::date_md(state.work_date)} }
                    p { class: "today-meta",
                        "근무 시간 {hours}"
                        if let Some(day_note) = day_note {
                            " · {day_note}"
                        }
                    }
                }
                Badge { tone: text::day_status_tone(status), {text::day_status_label(status)} }
            }
            {match stage {
                TodayStage::ClockIn => rsx! {
                    div { class: "chip-group", role: "group", "aria-label": "근무 형태",
                        for option in WorkType::ALL.iter().copied() {
                            button {
                                key: "{option.as_str()}",
                                r#type: "button",
                                class: if option == work_type() { "chip chip-active" } else { "chip" },
                                "aria-pressed": option == work_type(),
                                disabled: busy(),
                                onclick: move |_| work_type.set(option),
                                {text::work_type_label(option)}
                            }
                        }
                    }
                    TextField {
                        label: "메모 (선택)",
                        value: note(),
                        placeholder: "예: 오전 외근 후 복귀",
                        maxlength: logic::NOTE_MAX_CHARS,
                        disabled: busy(),
                        oninput: move |value| note.set(value),
                    }
                    div { class: "clock-action",
                        Button { block: true, busy: busy(), onclick: clock_in, "출근하기" }
                    }
                },
                TodayStage::Working { clock_in_at, work_type, overnight } => rsx! {
                    div { class: "today-working",
                        p { class: "today-elapsed",
                            "근무 중 "
                            strong { {format::duration_minutes(format::elapsed_minutes(clock_in_at, now()))} }
                        }
                        p { class: "today-meta",
                            if overnight {
                                "출근 {format::datetime(clock_in_at, tz)} · 전날 시작한 근무"
                            } else {
                                "출근 {format::time_hm(clock_in_at, tz)}"
                            }
                            " · {text::work_type_label(work_type)}"
                        }
                    }
                    div { class: "clock-action",
                        Button { block: true, busy: busy(), onclick: clock_out, "퇴근하기" }
                    }
                },
                TodayStage::Finished { clock_in_at, clock_out_at, worked_minutes, work_type } => rsx! {
                    dl { class: "today-summary",
                        div {
                            dt { "출근" }
                            dd { {format::time_hm(clock_in_at, tz)} }
                        }
                        div {
                            dt { "퇴근" }
                            dd { {format::time_hm(clock_out_at, tz)} }
                        }
                        div {
                            dt { "근무" }
                            dd { {format::duration_minutes(worked_minutes)} }
                        }
                        div {
                            dt { "형태" }
                            dd { {text::work_type_label(work_type)} }
                        }
                    }
                    p { class: "today-meta", "오늘 근무를 마쳤습니다." }
                },
                TodayStage::MissedClockOut { clock_in_at } => rsx! {
                    p { class: "today-meta",
                        "{format::datetime(clock_in_at, tz)} 출근 기록이 퇴근 없이 끝났습니다. 관리자에게 정정을 요청해 주세요."
                    }
                },
            }}
        }
    });

    rsx! {
        Card { title: "오늘", class: "today-card", {body} }
    }
}

/// 한 달의 요약 타일과 날짜별 기록. `member_id`가 없으면 본인.
///
/// `revision`이 바뀌면 다시 읽는다(출퇴근 직후 등).
#[component]
pub fn MonthAttendance(
    member_id: ReadSignal<Option<MemberId>>,
    revision: ReadSignal<u32>,
) -> Element {
    let session = use_session();
    let tz = session.zone();
    let mut month = use_signal(move || YearMonth::of(today(tz)));
    let mut opened = use_signal(|| None::<(AttendanceRecordId, String)>);

    let summary = use_resource(move || async move {
        let _ = revision();
        let path = logic::month_path(MonthEndpoint::Summary, month(), member_id());
        api::get::<MonthlySummaryDto>(&path).await
    });
    let records = use_resource(move || async move {
        let _ = revision();
        let path = logic::month_path(MonthEndpoint::Records, month(), member_id());
        api::get::<MonthRecords>(&path).await
    });

    let open = move |row: &DayRow| {
        let target = row.record_id.map(|id| (id, row.date_label.clone()));
        move |_| {
            if let Some(target) = target.clone() {
                opened.set(Some(target));
            }
        }
    };

    rsx! {
        Card {
            title: "월별 근태",
            action: rsx! {
                MonthSwitcher { month: month(), onchange: move |next| month.set(next) }
            },
            {resource_view(summary, |dto: MonthlySummaryDto| rsx! {
                div { class: "att-stats",
                    for tile in logic::summary_tiles(&dto.summary) {
                        Stat { key: "{tile.label}", label: tile.label, value: tile.value, tone: tile.tone }
                    }
                }
            })}
            {resource_view(records, move |list: MonthRecords| {
                let rows: Vec<DayRow> = list.days.iter().map(|day| logic::day_row(day, tz)).collect();
                if rows.is_empty() {
                    return rsx! { EmptyState { title: "이 달의 기록이 없습니다.", icon: IconName::Clock } };
                }
                rsx! {
                    div { class: "table-wrap att-desktop",
                        table { class: "table att-table",
                            thead {
                                tr {
                                    th { "날짜" }
                                    th { "근무 형태" }
                                    th { "출근" }
                                    th { "퇴근" }
                                    th { class: "num", "근무시간" }
                                    th { "상태" }
                                }
                            }
                            tbody {
                                for row in rows.clone() {
                                    tr { key: "{row.date}", class: if row.muted { "att-muted" },
                                        td {
                                            if row.record_id.is_some() {
                                                button { class: "link-btn", r#type: "button", onclick: open(&row),
                                                    "{row.date_label}"
                                                    span { class: "sr-only", " 변경 이력 보기" }
                                                }
                                            } else {
                                                "{row.date_label}"
                                            }
                                            if let Some(note) = &row.note {
                                                span { class: "att-note", "{note}" }
                                            }
                                        }
                                        td { "{row.cells.work_type}" }
                                        td { "{row.cells.clock_in}" }
                                        td { "{row.cells.clock_out}" }
                                        td { class: "num", "{row.cells.worked}" }
                                        td {
                                            Badge { tone: row.cells.tone, "{row.cells.status}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    ul { class: "list att-mobile",
                        for row in rows {
                            li { key: "{row.date}", class: if row.muted { "att-day att-muted" } else { "att-day" },
                                if row.record_id.is_some() {
                                    button { class: "att-day-main", r#type: "button", onclick: open(&row),
                                        DayLine { row: row.clone() }
                                        span { class: "sr-only", " 변경 이력 보기" }
                                    }
                                } else {
                                    div { class: "att-day-main", DayLine { row: row.clone() } }
                                }
                            }
                        }
                    }
                }
            })}
        }
        if let Some((record_id, title)) = opened() {
            RecordHistory { record_id, title, onclose: move |_| opened.set(None) }
        }
    }
}

/// 모바일 날짜 목록의 한 줄.
#[component]
fn DayLine(row: DayRow) -> Element {
    rsx! {
        span { class: "att-day-text",
            strong { "{row.date_label}" }
            small {
                if let Some(note) = &row.note {
                    "{note} · "
                }
                "{row.cells.clock_in} – {row.cells.clock_out}"
                if row.cells.worked != logic::EMPTY {
                    " · {row.cells.worked}"
                }
            }
        }
        Badge { tone: row.cells.tone, "{row.cells.status}" }
    }
}

/// 출퇴근 기록 하나의 변경 이력. 근태·근태 관리 화면이 함께 쓴다.
#[component]
pub fn RecordHistory(
    record_id: AttendanceRecordId,
    #[props(into)] title: String,
    onclose: EventHandler<()>,
) -> Element {
    let session = use_session();
    let tz = session.zone();
    let events = use_resource(move || async move {
        api::get::<Vec<AttendanceEventDto>>(&format!("/attendance/records/{record_id}/events"))
            .await
    });
    rsx! {
        Modal { title: "{title} 변경 이력", onclose: move |_| onclose.call(()),
            {resource_view(events, move |list: Vec<AttendanceEventDto>| {
                if list.is_empty() {
                    return rsx! { EmptyState { title: "변경 이력이 없습니다." } };
                }
                rsx! {
                    ol { class: "att-history",
                        for event in list.iter().map(|event| logic::event_row(event, tz)) {
                            li { key: "{event.id}", class: "att-history-item",
                                div { class: "att-history-head",
                                    Badge { tone: Tone::Info, "{event.kind}" }
                                    small { "{event.when} · {event.actor}" }
                                }
                                if let Some(before) = &event.before {
                                    p { class: "att-history-change",
                                        span { class: "att-history-before", "{before}" }
                                        span { "aria-hidden": "true", " → " }
                                        span { class: "sr-only", "에서 " }
                                        span { "{event.after}" }
                                    }
                                } else {
                                    p { class: "att-history-change", "{event.after}" }
                                }
                                if let Some(reason) = &event.reason {
                                    p { class: "att-history-reason", "사유: {reason}" }
                                }
                            }
                        }
                    }
                }
            })}
        }
    }
}

/// 화면이 그리는 값을 만드는 순수 함수.
pub(crate) mod logic {
    use chrono::NaiveDate;
    use chrono_tz::Tz;
    use pango_api::attendance::{
        AttendanceEventDto, AttendanceRecordDto, ClockInRequest, DayEntry, TodayResponse,
    };
    use pango_core::attendance::{DayEvaluation, DayStatus, MonthlySummary, WorkType};
    use pango_core::id::{AttendanceEventId, AttendanceRecordId, MemberId};
    use pango_core::time::{MinuteOfDay, Timestamp};

    use crate::calendar::YearMonth;
    use crate::format;
    use crate::text;
    use crate::ui::Tone;

    /// 값이 없는 칸.
    pub const EMPTY: &str = "-";
    /// 출근 메모 최대 글자 수(서버 `Comment` 규칙과 같다).
    pub const NOTE_MAX_CHARS: i64 = 1_000;

    /// 오늘 카드의 단계.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TodayStage {
        /// 출근 전: 출근 버튼.
        ClockIn,
        /// 근무 중: 퇴근 버튼. `overnight`면 전날 근무일에 시작한 기록이다.
        Working {
            clock_in_at: Timestamp,
            work_type: WorkType,
            overnight: bool,
        },
        /// 퇴근까지 마침.
        Finished {
            clock_in_at: Timestamp,
            clock_out_at: Timestamp,
            worked_minutes: u32,
            work_type: WorkType,
        },
        /// 퇴근하지 않은 채 근무 한도가 지나 더는 퇴근으로 닫을 수 없음.
        MissedClockOut { clock_in_at: Timestamp },
    }

    pub fn today_stage(today: &TodayResponse) -> TodayStage {
        let Some(record) = &today.record else {
            return TodayStage::ClockIn;
        };
        match record.clock_out_at {
            Some(clock_out_at) => TodayStage::Finished {
                clock_in_at: record.clock_in_at,
                clock_out_at,
                worked_minutes: today.evaluation.worked_minutes,
                work_type: record.work_type,
            },
            None if today.evaluation.status == DayStatus::MissingClockOut => {
                TodayStage::MissedClockOut {
                    clock_in_at: record.clock_in_at,
                }
            }
            None => TodayStage::Working {
                clock_in_at: record.clock_in_at,
                work_type: record.work_type,
                overnight: record.work_date != today.work_date,
            },
        }
    }

    /// 날짜 옆에 붙는 설명: 공휴일 이름, 또는 근무일이 아니면 `휴무일`.
    pub fn day_note(is_working_day: bool, holiday_name: Option<&str>) -> Option<String> {
        match holiday_name.map(str::trim).filter(|name| !name.is_empty()) {
            Some(name) => Some(name.to_owned()),
            None if !is_working_day => Some("휴무일".to_owned()),
            None => None,
        }
    }

    /// `09:00 ~ 18:00`
    pub fn expected_hours(start: MinuteOfDay, end: MinuteOfDay) -> String {
        format!("{start} ~ {end}")
    }

    /// 메모는 앞뒤 공백을 지우고, 비었으면 보내지 않는다.
    pub fn clock_in_request(work_type: WorkType, note: &str) -> ClockInRequest {
        let note = note.trim();
        ClockInRequest {
            work_type,
            note: (!note.is_empty()).then(|| note.to_owned()),
        }
    }

    /// 근무일 기준 시각. 다음 날로 넘어간 시각은 `02:10 (+1일)`.
    pub fn clock_text(at: Timestamp, work_date: NaiveDate, tz: Tz) -> String {
        let time = format::time_hm(at, tz);
        match (format::local_date(at, tz) - work_date).num_days() {
            0 => time,
            days if days > 0 => format!("{time} (+{days}일)"),
            days => format!("{time} ({days}일)"),
        }
    }

    /// 기록 한 줄의 표시 값. 표와 목록이 함께 쓴다.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RecordCells {
        pub work_type: String,
        pub clock_in: String,
        pub clock_out: String,
        pub worked: String,
        pub status: &'static str,
        pub tone: Tone,
    }

    pub fn record_cells(
        work_date: NaiveDate,
        record: Option<&AttendanceRecordDto>,
        evaluation: &DayEvaluation,
        tz: Tz,
    ) -> RecordCells {
        let text_or_empty = |value: Option<String>| value.unwrap_or_else(|| EMPTY.to_owned());
        RecordCells {
            work_type: text_or_empty(record.map(|r| text::work_type_label(r.work_type).to_owned())),
            clock_in: text_or_empty(record.map(|r| clock_text(r.clock_in_at, work_date, tz))),
            clock_out: text_or_empty(
                record
                    .and_then(|r| r.clock_out_at)
                    .map(|at| clock_text(at, work_date, tz)),
            ),
            worked: text_or_empty(
                record
                    .filter(|r| r.clock_out_at.is_some())
                    .map(|_| format::duration_minutes(evaluation.worked_minutes)),
            ),
            status: text::day_status_label(evaluation.status),
            tone: text::day_status_tone(evaluation.status),
        }
    }

    /// 월별 목록의 한 날.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DayRow {
        pub date: NaiveDate,
        pub date_label: String,
        pub note: Option<String>,
        /// 주말·공휴일 등 근무일이 아닌 날은 흐리게.
        pub muted: bool,
        /// 기록이 있어야 변경 이력을 열 수 있다.
        pub record_id: Option<AttendanceRecordId>,
        pub cells: RecordCells,
    }

    pub fn day_row(entry: &DayEntry, tz: Tz) -> DayRow {
        DayRow {
            date: entry.date,
            date_label: format::date_md(entry.date),
            note: day_note(entry.is_working_day, entry.holiday_name.as_deref()),
            muted: !entry.is_working_day,
            record_id: entry.record.as_ref().map(|r| r.id),
            cells: record_cells(entry.date, entry.record.as_ref(), &entry.evaluation, tz),
        }
    }

    /// 숫자 타일 하나.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Tile {
        pub label: &'static str,
        pub value: String,
        pub tone: Tone,
    }

    impl Tile {
        /// 개수 타일(`3일`, `5명`). 0이면 강조하지 않는다.
        pub fn count(label: &'static str, count: u32, unit: &str, tone: Tone) -> Self {
            Self {
                label,
                value: format!("{count}{unit}"),
                tone: if count == 0 { Tone::Neutral } else { tone },
            }
        }
    }

    fn count_tile(label: &'static str, count: u32, tone: Tone) -> Tile {
        Tile::count(label, count, "일", tone)
    }

    /// 월 요약 타일: 근무일수, 총 근무시간, 지각, 조퇴, 결근, 누락.
    ///
    /// 지각·조퇴가 함께 있는 날은 지각과 조퇴에 모두 센다.
    pub fn summary_tiles(summary: &MonthlySummary) -> Vec<Tile> {
        let attended = [
            summary.normal_days,
            summary.late_days,
            summary.early_leave_days,
            summary.late_and_early_leave_days,
            summary.working_days,
            summary.missing_clock_out_days,
        ]
        .into_iter()
        .fold(0_u32, u32::saturating_add);
        let total = u32::try_from(summary.total_worked_minutes).unwrap_or(u32::MAX);
        vec![
            Tile {
                label: "근무일수",
                value: format!("{attended}일"),
                tone: Tone::Neutral,
            },
            Tile {
                label: "총 근무시간",
                value: format::duration_minutes(total),
                tone: Tone::Neutral,
            },
            count_tile(
                "지각",
                summary
                    .late_days
                    .saturating_add(summary.late_and_early_leave_days),
                Tone::Warning,
            ),
            count_tile(
                "조퇴",
                summary
                    .early_leave_days
                    .saturating_add(summary.late_and_early_leave_days),
                Tone::Warning,
            ),
            count_tile("결근", summary.absent_days, Tone::Danger),
            count_tile("누락", summary.missing_clock_out_days, Tone::Danger),
        ]
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MonthEndpoint {
        Records,
        Summary,
    }

    /// `/attendance/records?month=2026-03[&member_id=…]`
    pub fn month_path(
        endpoint: MonthEndpoint,
        month: YearMonth,
        member: Option<MemberId>,
    ) -> String {
        let name = match endpoint {
            MonthEndpoint::Records => "records",
            MonthEndpoint::Summary => "summary",
        };
        let mut path = format!("/attendance/{name}?month={}", month.query());
        if let Some(member) = member {
            // MemberId는 UUID라 인코딩할 문자가 없다.
            path.push_str(&format!("&member_id={member}"));
        }
        path
    }

    /// 이력 한 줄.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct EventRow {
        pub id: AttendanceEventId,
        pub kind: &'static str,
        pub when: String,
        pub actor: String,
        pub reason: Option<String>,
        pub before: Option<String>,
        pub after: String,
    }

    /// 이력에 저장된 기록 모양(JSON)을 `출근 09:02 · 퇴근 18:10 · 사무실`로.
    pub fn record_snapshot(value: &serde_json::Value, tz: Tz) -> String {
        match serde_json::from_value::<AttendanceRecordDto>(value.clone()) {
            Ok(record) => {
                let clock_out = record
                    .clock_out_at
                    .map(|at| clock_text(at, record.work_date, tz))
                    .unwrap_or_else(|| "없음".to_owned());
                format!(
                    "출근 {} · 퇴근 {} · {}",
                    clock_text(record.clock_in_at, record.work_date, tz),
                    clock_out,
                    text::work_type_label(record.work_type)
                )
            }
            Err(_) => "내용을 표시할 수 없습니다.".to_owned(),
        }
    }

    pub fn event_row(event: &AttendanceEventDto, tz: Tz) -> EventRow {
        EventRow {
            id: event.id,
            kind: text::attendance_event_label(event.kind),
            when: format::datetime(event.created_at, tz),
            actor: event
                .actor_name
                .clone()
                .unwrap_or_else(|| "알 수 없음".to_owned()),
            reason: event
                .reason
                .as_deref()
                .map(str::trim)
                .filter(|reason| !reason.is_empty())
                .map(str::to_owned),
            before: event
                .before
                .as_ref()
                .map(|value| record_snapshot(value, tz)),
            after: record_snapshot(&event.after, tz),
        }
    }

    #[cfg(test)]
    pub(crate) mod fixtures {
        use super::*;

        pub fn ts(rfc3339: &str) -> Timestamp {
            Timestamp::from_utc(
                chrono::DateTime::parse_from_rfc3339(rfc3339)
                    .expect("rfc3339")
                    .with_timezone(&chrono::Utc),
            )
        }

        pub fn date(value: &str) -> NaiveDate {
            value.parse().expect("date")
        }

        pub fn seoul() -> Tz {
            format::zone("Asia/Seoul")
        }

        pub fn evaluation(status: DayStatus, worked_minutes: u32) -> DayEvaluation {
            DayEvaluation {
                status,
                worked_minutes,
                break_minutes: 0,
                late_minutes: 0,
                early_leave_minutes: 0,
                overtime_minutes: 0,
            }
        }

        pub fn record(
            work_date: &str,
            clock_in: &str,
            clock_out: Option<&str>,
        ) -> AttendanceRecordDto {
            AttendanceRecordDto {
                id: AttendanceRecordId::new(),
                member_id: MemberId::new(),
                work_date: date(work_date),
                clock_in_at: ts(clock_in),
                clock_out_at: clock_out.map(ts),
                work_type: WorkType::Remote,
                note: None,
                version: 1,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::fixtures::*;
        use super::*;
        use pango_core::attendance::AttendanceEventKind;

        fn today(record: Option<AttendanceRecordDto>, status: DayStatus) -> TodayResponse {
            TodayResponse {
                work_date: date("2026-03-03"),
                is_working_day: true,
                holiday_name: None,
                record,
                evaluation: evaluation(status, 470),
                expected_start: MinuteOfDay::parse("09:00").expect("time"),
                expected_end: MinuteOfDay::parse("18:00").expect("time"),
            }
        }

        #[test]
        fn today_stage_follows_the_record() {
            assert_eq!(
                today_stage(&today(None, DayStatus::Scheduled)),
                TodayStage::ClockIn
            );

            let open = record("2026-03-03", "2026-03-03T00:02:00Z", None);
            assert_eq!(
                today_stage(&today(Some(open), DayStatus::Working)),
                TodayStage::Working {
                    clock_in_at: ts("2026-03-03T00:02:00Z"),
                    work_type: WorkType::Remote,
                    overnight: false,
                }
            );

            let closed = record(
                "2026-03-03",
                "2026-03-03T00:02:00Z",
                Some("2026-03-03T09:10:00Z"),
            );
            assert_eq!(
                today_stage(&today(Some(closed), DayStatus::Normal)),
                TodayStage::Finished {
                    clock_in_at: ts("2026-03-03T00:02:00Z"),
                    clock_out_at: ts("2026-03-03T09:10:00Z"),
                    worked_minutes: 470,
                    work_type: WorkType::Remote,
                }
            );
        }

        #[test]
        fn yesterday_open_record_is_a_night_shift_to_close() {
            let night = record("2026-03-02", "2026-03-02T13:00:00Z", None);
            assert!(matches!(
                today_stage(&today(Some(night.clone()), DayStatus::Working)),
                TodayStage::Working {
                    overnight: true,
                    ..
                }
            ));
            assert_eq!(
                today_stage(&today(Some(night), DayStatus::MissingClockOut)),
                TodayStage::MissedClockOut {
                    clock_in_at: ts("2026-03-02T13:00:00Z")
                }
            );
        }

        #[test]
        fn day_notes_and_hours() {
            assert_eq!(day_note(false, Some("삼일절")), Some("삼일절".into()));
            assert_eq!(
                day_note(true, Some("창립기념일")),
                Some("창립기념일".into())
            );
            assert_eq!(day_note(false, None), Some("휴무일".into()));
            assert_eq!(day_note(false, Some("  ")), Some("휴무일".into()));
            assert_eq!(day_note(true, None), None);
            let hm = |v| MinuteOfDay::parse(v).expect("time");
            assert_eq!(expected_hours(hm("09:00"), hm("18:00")), "09:00 ~ 18:00");
        }

        #[test]
        fn clock_in_request_trims_note() {
            assert_eq!(clock_in_request(WorkType::Field, "  ").note, None);
            assert_eq!(
                clock_in_request(WorkType::Field, " 고객사 방문 ")
                    .note
                    .as_deref(),
                Some("고객사 방문")
            );
            assert_eq!(
                clock_in_request(WorkType::Field, "").work_type,
                WorkType::Field
            );
        }

        #[test]
        fn clock_text_marks_next_day() {
            let tz = seoul();
            let day = date("2026-03-02");
            assert_eq!(clock_text(ts("2026-03-02T00:05:00Z"), day, tz), "09:05");
            assert_eq!(
                clock_text(ts("2026-03-02T17:10:00Z"), day, tz),
                "02:10 (+1일)"
            );
            assert_eq!(
                clock_text(ts("2026-02-28T15:00:00Z"), day, tz),
                "00:00 (-1일)"
            );
        }

        #[test]
        fn record_cells_without_record_show_dashes() {
            let cells = record_cells(
                date("2026-03-01"),
                None,
                &evaluation(DayStatus::DayOff, 0),
                seoul(),
            );
            assert_eq!(cells.clock_in, EMPTY);
            assert_eq!(cells.clock_out, EMPTY);
            assert_eq!(cells.worked, EMPTY);
            assert_eq!(cells.work_type, EMPTY);
            assert_eq!(cells.status, "휴무");
            assert_eq!(cells.tone, Tone::Neutral);
        }

        #[test]
        fn record_cells_show_worked_only_after_clock_out() {
            let tz = seoul();
            let open = record("2026-03-02", "2026-03-02T00:05:00Z", None);
            let cells = record_cells(
                date("2026-03-02"),
                Some(&open),
                &evaluation(DayStatus::Working, 0),
                tz,
            );
            assert_eq!(
                (cells.clock_in.as_str(), cells.clock_out.as_str()),
                ("09:05", EMPTY)
            );
            assert_eq!(cells.worked, EMPTY);
            assert_eq!(cells.work_type, "재택");

            let closed = record(
                "2026-03-02",
                "2026-03-02T00:05:00Z",
                Some("2026-03-02T09:35:00Z"),
            );
            let cells = record_cells(
                date("2026-03-02"),
                Some(&closed),
                &evaluation(DayStatus::Late, 510),
                tz,
            );
            assert_eq!(cells.clock_out, "18:35");
            assert_eq!(cells.worked, "8시간 30분");
            assert_eq!((cells.status, cells.tone), ("지각", Tone::Warning));
        }

        #[test]
        fn day_rows_mute_days_off_and_keep_record_ids() {
            let tz = seoul();
            let holiday = DayEntry {
                date: date("2026-03-01"),
                is_working_day: false,
                holiday_name: Some("삼일절".into()),
                record: None,
                evaluation: evaluation(DayStatus::DayOff, 0),
            };
            let row = day_row(&holiday, tz);
            assert!(row.muted);
            assert_eq!(row.note.as_deref(), Some("삼일절"));
            assert_eq!(row.record_id, None);
            assert_eq!(row.date_label, "3월 1일 (일)");

            let worked = record("2026-03-02", "2026-03-02T00:00:00Z", None);
            let id = worked.id;
            let workday = DayEntry {
                date: date("2026-03-02"),
                is_working_day: true,
                holiday_name: None,
                record: Some(worked),
                evaluation: evaluation(DayStatus::Working, 0),
            };
            let row = day_row(&workday, tz);
            assert!(!row.muted);
            assert_eq!(row.record_id, Some(id));
            assert_eq!(row.note, None);
        }

        #[test]
        fn summary_tiles_count_combined_days_in_both() {
            let summary = MonthlySummary {
                normal_days: 10,
                late_days: 2,
                early_leave_days: 1,
                late_and_early_leave_days: 1,
                working_days: 1,
                missing_clock_out_days: 1,
                absent_days: 0,
                on_leave_days: 2,
                total_worked_minutes: 9_630,
                ..MonthlySummary::default()
            };
            let tiles = summary_tiles(&summary);
            let values: Vec<_> = tiles.iter().map(|t| (t.label, t.value.as_str())).collect();
            assert_eq!(
                values,
                vec![
                    ("근무일수", "16일"),
                    ("총 근무시간", "160시간 30분"),
                    ("지각", "3일"),
                    ("조퇴", "2일"),
                    ("결근", "0일"),
                    ("누락", "1일"),
                ]
            );
            assert_eq!(tiles[4].tone, Tone::Neutral, "0일은 강조하지 않는다");
            assert_eq!(tiles[5].tone, Tone::Danger);
            assert_eq!(tiles[2].tone, Tone::Warning);
        }

        #[test]
        fn summary_tiles_saturate_instead_of_overflowing() {
            let summary = MonthlySummary {
                late_days: u32::MAX,
                late_and_early_leave_days: 5,
                total_worked_minutes: u64::MAX,
                ..MonthlySummary::default()
            };
            let tiles = summary_tiles(&summary);
            assert_eq!(tiles[0].value, format!("{}일", u32::MAX));
            assert_eq!(tiles[2].value, format!("{}일", u32::MAX));
            assert!(tiles[1].value.ends_with('분') || tiles[1].value.ends_with("시간"));
        }

        #[test]
        fn month_paths() {
            let month = YearMonth {
                year: 2026,
                month: 3,
            };
            assert_eq!(
                month_path(MonthEndpoint::Records, month, None),
                "/attendance/records?month=2026-03"
            );
            let member = MemberId::new();
            assert_eq!(
                month_path(MonthEndpoint::Summary, month, Some(member)),
                format!("/attendance/summary?month=2026-03&member_id={member}")
            );
        }

        #[test]
        fn event_rows_summarise_before_and_after() {
            let tz = seoul();
            let before = record("2026-03-02", "2026-03-02T00:05:00Z", None);
            let after = record(
                "2026-03-02",
                "2026-03-02T00:00:00Z",
                Some("2026-03-02T17:10:00Z"),
            );
            let event = AttendanceEventDto {
                id: AttendanceEventId::new(),
                record_id: before.id,
                kind: AttendanceEventKind::AdminEdit,
                before: Some(serde_json::to_value(&before).expect("json")),
                after: serde_json::to_value(&after).expect("json"),
                reason: Some("  지문 인식 오류  ".into()),
                actor_id: MemberId::new(),
                actor_name: None,
                created_at: ts("2026-03-03T01:00:00Z"),
            };
            let row = event_row(&event, tz);
            assert_eq!(row.kind, "관리자 수정");
            assert_eq!(row.when, "2026. 3. 3. 10:00");
            assert_eq!(row.actor, "알 수 없음");
            assert_eq!(row.reason.as_deref(), Some("지문 인식 오류"));
            assert_eq!(row.before.as_deref(), Some("출근 09:05 · 퇴근 없음 · 재택"));
            assert_eq!(row.after, "출근 09:00 · 퇴근 02:10 (+1일) · 재택");
        }

        #[test]
        fn event_rows_tolerate_unexpected_json() {
            let event = AttendanceEventDto {
                id: AttendanceEventId::new(),
                record_id: AttendanceRecordId::new(),
                kind: AttendanceEventKind::ClockIn,
                before: None,
                after: serde_json::json!({ "unexpected": true }),
                reason: Some("   ".into()),
                actor_id: MemberId::new(),
                actor_name: Some("홍길동".into()),
                created_at: ts("2026-03-02T00:00:00Z"),
            };
            let row = event_row(&event, seoul());
            assert_eq!(row.before, None);
            assert_eq!(row.reason, None);
            assert_eq!(row.after, "내용을 표시할 수 없습니다.");
            assert_eq!(row.actor, "홍길동");
        }
    }
}
