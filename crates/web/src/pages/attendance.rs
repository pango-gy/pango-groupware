//! 근태: 오늘 출퇴근, 월별 기록, 근태 정정 요청.
//!
//! 월별 기록(`MonthAttendance`)은 팀 현황 화면이 구성원 한 명의 달을 보여 줄 때도 그대로 쓴다.
//! 오늘 카드(`TodayBody`)는 홈도 함께 쓴다.

use chrono::NaiveDate;
use dioxus::prelude::*;
use pango_api::attendance::{
    AttendanceEventDto, AttendanceRecordDto, CorrectionDto, MonthRecords, MonthlySummaryDto,
    TodayResponse,
};
use pango_api::{Empty, Page};
use pango_core::attendance::WorkType;
use pango_core::id::{AttendanceRecordId, MemberId};

use crate::api;
use crate::calendar::YearMonth;
use crate::company::use_company;
use crate::format;
use crate::route::Route;
use crate::session::{use_session, use_toasts};
use crate::text;
use crate::ui::approval_line::line_problem;
use crate::ui::workday_bar;
use crate::ui::{
    report_error, report_success, resource_view, use_line_draft, use_more_pages,
    ApprovalLineEditor, ApproverEntry, Badge, Button, ButtonKind, Card, ConfirmModal, EmptyState,
    FormError, IconName, LoadMore, Meta, Modal, MonthSwitcher, PageHeader, SelectField, Skeleton,
    Stat, TextArea, TextField, Timeline, TimelineItem, Tone, WeekProgress, WorkdayBar,
};

use logic::{CorrectionSlot, CorrectionTarget, DayRow, MonthEndpoint, TodayStage};

#[component]
pub fn Attendance() -> Element {
    let mut today = use_resource(|| async { api::get::<TodayResponse>("/attendance/today").await });
    // 출퇴근하면 아래 월별 기록을, 정정을 요청하면 요청 내역을 다시 읽는다.
    let mut revision = use_signal(|| 0_u32);
    let mut corrections = use_signal(|| 0_u32);
    rsx! {
        PageHeader { title: "근태", description: "출퇴근을 기록하고, 틀린 기록은 정정을 요청해요." }
        Card { title: "오늘 근무", class: "today-card",
            {resource_view(today, move |state: TodayResponse| rsx! {
                TodayBody {
                    state,
                    onchange: move |_| {
                        today.restart();
                        *revision.write() += 1;
                    },
                }
            })}
        }
        MonthAttendance {
            member_id: None,
            revision: revision(),
            oncorrect: move |_| *corrections.write() += 1,
        }
        CorrectionHistory { revision: corrections() }
    }
}

/// "오늘" 카드의 내용: 큰 출근·퇴근 버튼. 근태 화면과 홈이 같은 것을 쓴다.
///
/// 상태(`state`)는 부모가 읽어 넘긴다(홈은 `GET /home` 한 번으로 받는다). 출퇴근을 시도한 뒤에는
/// 성공·실패와 관계없이 `onchange`를 불러 부모가 다시 읽게 한다.
#[component]
pub fn TodayBody(state: TodayResponse, onchange: EventHandler<()>) -> Element {
    let session = use_session();
    let tz = session.zone();
    let company = use_company();
    let mut now = use_signal(crate::ui::now);
    let mut work_type = use_signal(|| WorkType::Office);
    let mut note = use_signal(String::new);
    let mut busy = use_signal(|| false);

    // 경과 시간 표시를 위해 30초마다 시각을 갱신한다.
    use_future(move || async move {
        loop {
            crate::platform::sleep_ms(30_000).await;
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
                report_success("출근했어요.");
            }
            Err(error) => report_error(&error),
        }
        now.set(crate::ui::now());
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
            Ok(_) => report_success("퇴근했어요. 수고하셨어요."),
            Err(error) => report_error(&error),
        }
        now.set(crate::ui::now());
        onchange.call(());
    };

    let stage = logic::today_stage(&state);
    let day_note = logic::day_note(state.is_working_day, state.holiday_name.as_deref());
    let hours = logic::expected_hours(state.expected_start, state.expected_end);
    let status = state.evaluation.status;
    // 전날 시작한 야간 근무를 닫는 중이면 그 근무일의 막대를 그린다.
    let bar_date = state
        .record
        .as_ref()
        .map_or(state.work_date, |r| r.work_date);
    let bar = workday_bar::day_bar(&logic::day_input(
        bar_date,
        state.record.as_ref(),
        &state.evaluation,
        (state.expected_start, state.expected_end),
        company.day_cutoff(),
        now(),
        tz,
    ));
    let cells = logic::record_cells(bar_date, state.record.as_ref(), &state.evaluation, tz);
    let live_minutes = logic::live_minutes(&stage, now());
    rsx! {
        div { class: "today-head",
            div { class: "today-when",
                p { class: "today-date", {format::date_md(state.work_date)} }
                Meta { items: vec![format!("근무 시간 {hours}"), day_note.unwrap_or_default()] }
            }
            Badge { tone: text::day_status_tone(status), {text::day_status_label(status)} }
        }
        div { class: "today-main",
            {match stage {
                TodayStage::ClockIn => rsx! {
                    div { class: "today-entry",
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
                    }
                    div { class: "today-action",
                        Button { block: true, large: true, busy: busy(), onclick: clock_in, "출근" }
                    }
                },
                TodayStage::Working { clock_in_at, work_type, overnight } => rsx! {
                    div { class: "figure",
                        span { class: "figure-label", "근무 중" }
                        strong { class: "figure-value tnum",
                            {format::duration_minutes(format::elapsed_minutes(clock_in_at, now()))}
                        }
                        Meta {
                            items: vec![
                                if overnight {
                                    format!("출근 {}", format::datetime(clock_in_at, tz))
                                } else {
                                    format!("출근 {}", format::time_hm(clock_in_at, tz))
                                },
                                if overnight { "전날 시작한 근무".to_owned() } else { String::new() },
                                text::work_type_label(work_type).to_owned(),
                            ],
                        }
                    }
                    div { class: "today-action",
                        Button { block: true, large: true, busy: busy(), onclick: clock_out, "퇴근" }
                    }
                },
                TodayStage::Finished { clock_in_at, clock_out_at, worked_minutes, work_type } => rsx! {
                    div { class: "stats today-done",
                        Stat { label: "출근", value: format::time_hm(clock_in_at, tz) }
                        Stat { label: "퇴근", value: format::time_hm(clock_out_at, tz) }
                        Stat { label: "근무", value: format::duration_minutes(worked_minutes) }
                        Stat { label: "형태", value: text::work_type_label(work_type) }
                    }
                },
                TodayStage::MissedClockOut { clock_in_at } => rsx! {
                    p { class: "text-sub",
                        "{format::datetime(clock_in_at, tz)} 출근 기록이 퇴근 없이 끝났어요. 근태 화면의 월별 기록에서 정정을 요청해 주세요."
                    }
                },
            }}
        }
        WorkdayBar {
            bar,
            summary: logic::bar_summary(&hours, &cells),
            clock_in_label: logic::clock_in_flag(&cells),
        }
        WeekLine {
            work_date: state.work_date,
            live_minutes,
            stamp: logic::record_stamp(state.record.as_ref()),
        }
    }
}

/// 이번 주 누적 근무시간. 이번 달 기록을 읽고, 주가 달을 넘을 때만 이웃한 달을 하나 더 읽는다.
///
/// `stamp`가 바뀌면(출퇴근 직후) 다시 읽는다. 읽는 중이거나 읽지 못하면 자리만 비워 둔다
/// (오늘 카드의 나머지는 그대로 쓸 수 있어야 한다).
#[component]
fn WeekLine(
    work_date: ReadSignal<NaiveDate>,
    live_minutes: u32,
    stamp: ReadSignal<String>,
) -> Element {
    let days = use_resource(move || async move {
        let _ = stamp();
        let (first, second) = workday_bar::week_months(workday_bar::week_of(work_date()));
        let path = |month| logic::month_path(MonthEndpoint::Records, month, None);
        let mut days = api::get::<MonthRecords>(&path(first)).await?.days;
        if let Some(second) = second {
            days.extend(api::get::<MonthRecords>(&path(second)).await?.days);
        }
        Ok::<_, api::ApiError>(days)
    });
    let minutes = match &*days.read() {
        Some(Ok(days)) => Some(workday_bar::week_minutes(
            logic::worked_days(days),
            workday_bar::week_of(work_date()),
            live_minutes,
        )),
        _ => None,
    };
    rsx! {
        div { class: "today-week",
            if let Some(minutes) = minutes {
                WeekProgress { minutes }
            }
        }
    }
}

/// 한 달의 요약 타일과 날짜별 기록. `member_id`가 없으면 본인.
///
/// `revision`이 바뀌면 다시 읽는다(출퇴근 직후 등). `oncorrect`를 주면 본인의 달에 한해
/// 날짜마다 "정정 요청" 버튼이 붙고, 요청을 올린 뒤 `oncorrect`가 불린다.
#[component]
pub fn MonthAttendance(
    member_id: ReadSignal<Option<MemberId>>,
    revision: ReadSignal<u32>,
    oncorrect: Option<EventHandler<()>>,
) -> Element {
    let session = use_session();
    let tz = session.zone();
    // 근무일 전환 시각 전(예: 새벽 4시)이면 아직 어제 근무일이다.
    let work_today = use_company().work_today(tz);
    let mut month = use_signal(move || YearMonth::of(work_today));
    let mut opened = use_signal(|| None::<(AttendanceRecordId, String)>);
    let mut correcting = use_signal(|| None::<CorrectionTarget>);

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

    // 정정 요청은 본인 기록에만 올릴 수 있다(조직장·관리자가 남의 달을 볼 때는 숨긴다).
    let own = member_id().is_none() && oncorrect.is_some();
    let current_day = work_today;

    let open = move |row: &DayRow| {
        let target = row.record_id.map(|id| (id, row.date_label.clone()));
        move |_| {
            if let Some(target) = target.clone() {
                opened.set(Some(target));
            }
        }
    };
    let correct = move |row: &DayRow| {
        let target = CorrectionTarget {
            date: row.date,
            date_label: row.date_label.clone(),
            record: row.record.clone(),
        };
        move |_| correcting.set(Some(target.clone()))
    };

    rsx! {
        Card {
            title: "월별 근태",
            action: rsx! {
                MonthSwitcher { month: month(), onchange: move |next| month.set(next) }
            },
            {resource_view(summary, |dto: MonthlySummaryDto| rsx! {
                div { class: "stats month-stats",
                    for tile in logic::summary_tiles(&dto.summary) {
                        Stat { key: "{tile.label}", label: tile.label, value: tile.value, tone: tile.tone }
                    }
                }
            })}
            {resource_view(records, move |list: MonthRecords| {
                let rows: Vec<DayRow> = list.days.iter().map(|day| logic::day_row(day, tz)).collect();
                if rows.is_empty() {
                    return rsx! { EmptyState { title: "이 달의 기록이 없어요.", icon: IconName::Clock } };
                }
                rsx! {
                    div { class: "table-wrap only-desktop",
                        table { class: "table table-nowrap",
                            thead {
                                tr {
                                    th { "날짜" }
                                    th { "근무 형태" }
                                    th { "출근" }
                                    th { "퇴근" }
                                    th { class: "num", "근무시간" }
                                    th { "상태" }
                                    if own {
                                        th { span { class: "sr-only", "정정" } }
                                    }
                                }
                            }
                            tbody {
                                for row in rows.clone() {
                                    tr { key: "{row.date}", class: if row.muted { "row-muted" },
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
                                                span { class: "cell-sub", "{note}" }
                                            }
                                        }
                                        td { "{row.cells.work_type}" }
                                        td { "{row.cells.clock_in}" }
                                        td { "{row.cells.clock_out}" }
                                        td { class: "num", "{row.cells.worked}" }
                                        td {
                                            Badge { tone: row.cells.tone, "{row.cells.status}" }
                                        }
                                        if own {
                                            // 모든 줄에 버튼을 늘어놓지 않고, 줄에 마우스를 올리거나 초점이 들어올 때만 보인다.
                                            td { class: "num row-hover-action",
                                                if logic::can_request_correction(row.date, current_day) {
                                                    Button {
                                                        kind: ButtonKind::Grey,
                                                        small: true,
                                                        onclick: correct(&row),
                                                        "정정 요청"
                                                        span { class: "sr-only", " ({row.date_label})" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    ul { class: "list only-mobile",
                        for row in rows {
                            li { key: "{row.date}", class: if row.muted { "list-row row-muted" } else { "list-row" },
                                if row.record_id.is_some() {
                                    button { class: "row-link", r#type: "button", onclick: open(&row),
                                        DayLine { row: row.clone() }
                                        span { class: "sr-only", " 변경 이력 보기" }
                                    }
                                } else {
                                    div { class: "row-link", DayLine { row: row.clone() } }
                                }
                                if own && logic::can_request_correction(row.date, current_day) {
                                    Button {
                                        kind: ButtonKind::Grey,
                                        small: true,
                                        onclick: correct(&row),
                                        "정정"
                                        span { class: "sr-only", " 요청 ({row.date_label})" }
                                    }
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
        if let Some(target) = correcting() {
            CorrectionModal {
                target,
                onclose: move |_| correcting.set(None),
                onsubmitted: move |_| {
                    correcting.set(None);
                    if let Some(handler) = oncorrect {
                        handler.call(());
                    }
                },
            }
        }
    }
}

/// 모바일 날짜 목록의 한 줄.
#[component]
fn DayLine(row: DayRow) -> Element {
    rsx! {
        span { class: "list-row-main",
            strong { "{row.date_label}" }
            Meta { items: logic::day_meta(&row) }
        }
        Badge { tone: row.cells.tone, "{row.cells.status}" }
    }
}

/// 근태 정정 요청 창. 고칠 출퇴근 시각·근무 형태·사유와 결재선을 받아 결재 문서로 올린다.
#[component]
fn CorrectionModal(
    target: CorrectionTarget,
    onclose: EventHandler<()>,
    onsubmitted: EventHandler<()>,
) -> Element {
    let session = use_session();
    let tz = session.zone();
    let zone_name = session.me().map(|me| me.timezone).unwrap_or_default();
    let me_id = session.me().map(|me| me.id);
    // 기록이 없는 날은 회사 근무 시간으로 채워 둔다.
    let defaults = use_company().default_times(target.date);
    let initial = logic::CorrectionForm::for_record(target.record.as_ref(), tz, defaults);
    let mut form = use_signal(move || initial);
    // 창을 열 때 한 번 만든다. 두 번 눌러도 요청은 하나만 생긴다.
    let key = use_signal(api::new_idempotency_key);
    let mut line = use_line_draft(me_id.into_iter().collect());
    let mut busy = use_signal(|| false);
    let mut problem = use_signal(|| None::<(CorrectionSlot, String)>);

    let work_date = target.date;
    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let request = match logic::correction_request(work_date, &form(), &line.steps(), tz) {
            Ok(request) => request,
            Err(issue) => {
                problem.set(Some((issue.slot(), issue.message().to_owned())));
                return;
            }
        };
        busy.set(true);
        problem.set(None);
        match api::post_once::<_, CorrectionDto>("/attendance/corrections", &request, &key()).await
        {
            Ok(_) => {
                report_success("정정을 요청했어요. 결재가 끝나면 기록에 반영돼요.");
                // 창이 닫히므로 버튼을 다시 풀지 않는다(닫히기 전 두 번째 제출 방지).
                onsubmitted.call(());
                return;
            }
            Err(failure) => {
                match logic::correction_failure_slot(&failure.code, failure.field.as_deref()) {
                    Some(slot) => {
                        problem.set(Some((
                            slot,
                            text::correction_error_message(
                                &failure.code,
                                failure.field.as_deref(),
                                failure.reason.as_deref(),
                            ),
                        )));
                    }
                    None => report_error(&failure),
                }
            }
        }
        busy.set(false);
    };

    let message_for = move |slot: CorrectionSlot| -> String {
        match problem() {
            Some((at, message)) if at == slot => message,
            _ => String::new(),
        }
    };
    let current = form();
    let line_now = line.steps();
    let line_message = {
        let from_problem = message_for(CorrectionSlot::Line);
        if from_problem.is_empty() {
            line_problem(&line_now).unwrap_or_default().to_owned()
        } else {
            from_problem
        }
    };

    rsx! {
        Modal { title: "{target.date_label} 정정 요청", onclose: move |_| onclose.call(()),
            form { class: "form", onsubmit: submit,
                FormError { message: message_for(CorrectionSlot::Form) }
                div { class: "field",
                    span { class: "field-label", "지금 기록" }
                    span { class: "field-static", {logic::record_text(target.record.as_ref(), tz)} }
                }
                div { class: "form-row",
                    TextField {
                        label: "출근 시각",
                        kind: "datetime-local",
                        value: current.clock_in.clone(),
                        required: true,
                        hint: "{zone_name} 기준이에요. 이 근무일의 출근이어야 해요.",
                        error: message_for(CorrectionSlot::Times),
                        oninput: move |value| {
                            form.write().clock_in = value;
                            problem.set(None);
                        },
                    }
                    TextField {
                        label: "퇴근 시각",
                        kind: "datetime-local",
                        value: current.clock_out.clone(),
                        hint: "비워 두면 퇴근 전으로 남아요.",
                        oninput: move |value| {
                            form.write().clock_out = value;
                            problem.set(None);
                        },
                    }
                }
                SelectField {
                    label: "근무 형태",
                    value: current.work_type.as_str(),
                    options: logic::work_type_options(),
                    onchange: move |value: String| {
                        if let Ok(work_type) = WorkType::parse(&value) {
                            form.write().work_type = work_type;
                        }
                    },
                }
                div { class: "field",
                    TextArea {
                        label: "사유",
                        value: current.reason.clone(),
                        rows: 3,
                        required: true,
                        maxlength: logic::TEXT_MAX_CHARS as i64,
                        placeholder: "예: 지문 인식기 고장으로 출근이 기록되지 않았어요.",
                        oninput: move |value| {
                            form.write().reason = value;
                            problem.set(None);
                        },
                    }
                    if !message_for(CorrectionSlot::Reason).is_empty() {
                        span { class: "field-error", role: "alert", {message_for(CorrectionSlot::Reason)} }
                    }
                }
                div { class: "field",
                    p { class: "field-hint", "마지막 결재자는 내 조직장 또는 관리자여야 해요. 추천 결재선을 쓰면 이 조건을 맞춰요." }
                    if !line.ready() {
                        Skeleton {}
                    } else {
                        ApprovalLineEditor {
                            steps: line_now,
                            exclude: me_id.into_iter().collect::<Vec<_>>(),
                            disabled: busy(),
                            error: line_message,
                            onchange: move |next: Vec<ApproverEntry>| {
                                line.set(next);
                                problem.set(None);
                            },
                        }
                    }
                }
                div { class: "modal-actions",
                    Button { kind: ButtonKind::Grey, onclick: move |_| onclose.call(()), "취소" }
                    Button { submit: true, busy: busy(), "정정 요청" }
                }
            }
        }
    }
}

/// 내 정정 요청 내역(최신 접수순). 결재 중인 요청은 취소(= 결재 문서 회수)할 수 있다.
#[component]
fn CorrectionHistory(revision: ReadSignal<u32>) -> Element {
    let session = use_session();
    let mut toasts = use_toasts();
    let tz = session.zone();
    let more = use_more_pages::<CorrectionDto>();
    let mut first = use_resource(move || async move {
        let _ = revision();
        more.reset();
        api::get::<Page<CorrectionDto>>(&logic::corrections_path(None)).await
    });
    let mut confirm = use_signal(|| None::<CorrectionDto>);
    let mut busy = use_signal(|| false);

    let cancel = move |id: String| async move {
        if busy() {
            return;
        }
        busy.set(true);
        let path = format!("/attendance/corrections/{id}/cancel");
        let result = api::post::<_, CorrectionDto>(&path, &Empty {}).await;
        busy.set(false);
        confirm.set(None);
        match result {
            Ok(_) => report_success("정정 요청을 취소했어요. 결재 문서도 회수했어요."),
            Err(error) => {
                toasts.error(text::correction_error_message(
                    &error.code,
                    error.field.as_deref(),
                    error.reason.as_deref(),
                ));
            }
        }
        // 성공이든 실패든(이미 결재가 진행됨, 상태가 바뀜 등) 지금 상태로 다시 읽는다.
        first.restart();
    };

    rsx! {
        Card { title: "정정 요청 내역",
            {resource_view(first, move |page: Page<CorrectionDto>| {
                let next = more.cursor(&page);
                let has_more = next.is_some();
                let items: Vec<CorrectionDto> = more.all(page.items, |item| item.id);
                if items.is_empty() {
                    return rsx! {
                        EmptyState {
                            title: "올린 정정 요청이 없어요.",
                            hint: "기록이 틀린 날의 \"정정 요청\"을 눌러 고칠 값을 올리면 결재 후 반영돼요.",
                            icon: IconName::Clock,
                        }
                    };
                }
                rsx! {
                    ul { class: "list",
                        for item in items {
                            li { key: "{item.id}", class: "list-row list-row-top",
                                div { class: "list-row-main",
                                    div { class: "row-title",
                                        strong { {format::date_md(item.work_date)} }
                                        Badge { tone: text::correction_status_tone(item.status), {text::correction_status_label(item.status)} }
                                    }
                                    small { {logic::correction_summary(&item, tz)} }
                                    p { class: "row-reason", "{item.reason}" }
                                }
                                div { class: "list-row-side",
                                    Link {
                                        to: Route::ApprovalDetail { id: item.document_id.to_string() },
                                        "결재 문서"
                                    }
                                    if logic::can_cancel_correction(item.status) {
                                        Button {
                                            kind: ButtonKind::Grey,
                                            small: true,
                                            onclick: {
                                                let item = item.clone();
                                                move |_| confirm.set(Some(item.clone()))
                                            },
                                            "취소"
                                        }
                                    }
                                }
                            }
                        }
                    }
                    LoadMore {
                        has_more,
                        busy: more.busy(),
                        onclick: move |_| {
                            if let Some(cursor) = next.clone() {
                                spawn(more.load(cursor, |c| logic::corrections_path(Some(c))));
                            }
                        },
                    }
                }
            })}
        }
        if let Some(target) = confirm() {
            ConfirmModal {
                title: "정정 요청 취소",
                message: logic::cancel_correction_message(&target),
                confirm_label: "요청 취소",
                danger: true,
                busy: busy(),
                oncancel: move |_| confirm.set(None),
                onconfirm: {
                    let id = target.id.to_string();
                    move |_| cancel(id.clone())
                },
            }
        }
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
                    return rsx! { EmptyState { title: "변경 이력이 없어요." } };
                }
                rsx! {
                    Timeline {
                        for event in list.iter().map(|event| logic::event_row(event, tz)) {
                            TimelineItem { key: "{event.id}", tone: Tone::Info,
                                div { class: "timeline-head",
                                    strong { "{event.kind}" }
                                    Meta { items: vec![event.when.clone(), event.actor.clone()] }
                                }
                                if let Some(before) = &event.before {
                                    p { class: "change-line",
                                        span { class: "change-before", "{before}" }
                                        span { class: "sr-only", "에서 " }
                                        span { "{event.after}" }
                                    }
                                } else {
                                    p { class: "change-line", "{event.after}" }
                                }
                                if let Some(reason) = &event.reason {
                                    p { class: "text-sub", "사유: {reason}" }
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
    use chrono::{Days, NaiveDate};
    use chrono_tz::Tz;
    use pango_api::attendance::{
        AttendanceEventDto, AttendanceRecordDto, ClockInRequest, CorrectionDto,
        CreateCorrectionRequest, DayEntry, TodayResponse,
    };
    use pango_core::attendance::{
        CorrectionStatus, DayEvaluation, DayStatus, MonthlySummary, WorkType,
        CORRECTION_WINDOW_DAYS, MAX_SHIFT,
    };
    use pango_core::id::{AttendanceEventId, AttendanceRecordId, MemberId};
    use pango_core::time::{MinuteOfDay, Timestamp};

    use crate::calendar::YearMonth;
    use crate::format::{self, LocalInputError};
    use crate::text;
    use crate::ui::approval_line::{line_problem, to_inputs};
    use crate::ui::workday_bar::DayInput;
    use crate::ui::{ApproverEntry, Tone};

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

    /// 하루 막대에 넘길 사실. `work_date`는 막대가 나타내는 근무일(기록이 있으면 그 기록의 근무일).
    pub fn day_input(
        work_date: NaiveDate,
        record: Option<&AttendanceRecordDto>,
        evaluation: &DayEvaluation,
        expected: (MinuteOfDay, MinuteOfDay),
        cutoff: MinuteOfDay,
        now: Timestamp,
        tz: Tz,
    ) -> DayInput {
        DayInput {
            work_date,
            cutoff,
            expected_start: expected.0,
            expected_end: expected.1,
            clock_in: record.map(|r| r.clock_in_at),
            clock_out: record.and_then(|r| r.clock_out_at),
            late: evaluation.late_minutes > 0,
            missing_clock_out: evaluation.status == DayStatus::MissingClockOut,
            now,
            tz,
        }
    }

    /// 출퇴근 시각을 한 줄로: "09:02 – 18:10", 퇴근 전이면 "09:02 출근", 기록이 없으면 `None`.
    pub fn clock_span(cells: &RecordCells) -> Option<String> {
        if cells.clock_in == EMPTY {
            None
        } else if cells.clock_out == EMPTY {
            Some(format!("{} 출근", cells.clock_in))
        } else {
            Some(format!("{} – {}", cells.clock_in, cells.clock_out))
        }
    }

    /// 하루 막대를 글로 옮긴 한 줄(화면 낭독기용).
    pub fn bar_summary(hours: &str, cells: &RecordCells) -> String {
        if cells.clock_in == EMPTY {
            return format!("근무 시간 {hours}, 출근 기록 없음, {}", cells.status);
        }
        let clock_out = if cells.clock_out == EMPTY {
            "퇴근 전".to_owned()
        } else {
            format!("퇴근 {}", cells.clock_out)
        };
        format!(
            "근무 시간 {hours}, 출근 {}, {clock_out}, {}",
            cells.clock_in, cells.status
        )
    }

    /// 하루 막대의 출근 표지 글자. 기록이 없으면 빈 값.
    pub fn clock_in_flag(cells: &RecordCells) -> String {
        if cells.clock_in == EMPTY {
            String::new()
        } else {
            format!("출근 {}", cells.clock_in)
        }
    }

    /// 아직 퇴근하지 않은 근무의 경과 분. 서버 판정(`worked_minutes`)은 퇴근 전에 0이라
    /// 이번 주 누적에 따로 더한다.
    pub fn live_minutes(stage: &TodayStage, now: Timestamp) -> u32 {
        match stage {
            TodayStage::Working { clock_in_at, .. } => format::elapsed_minutes(*clock_in_at, now),
            _ => 0,
        }
    }

    /// 날짜별 근무 분(이번 주 누적 계산용).
    pub fn worked_days(days: &[DayEntry]) -> Vec<(NaiveDate, u32)> {
        days.iter()
            .map(|day| (day.date, day.evaluation.worked_minutes))
            .collect()
    }

    /// 기록이 바뀌면 달라지는 값(이번 주 누적을 다시 읽는 기준).
    pub fn record_stamp(record: Option<&AttendanceRecordDto>) -> String {
        record.map_or_else(String::new, |r| format!("{}:{}", r.id, r.version))
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
        /// 정정 요청 창에 지금 값을 채우는 데 쓴다.
        pub record: Option<AttendanceRecordDto>,
        pub cells: RecordCells,
    }

    pub fn day_row(entry: &DayEntry, tz: Tz) -> DayRow {
        DayRow {
            date: entry.date,
            date_label: format::date_md(entry.date),
            note: day_note(entry.is_working_day, entry.holiday_name.as_deref()),
            muted: !entry.is_working_day,
            record_id: entry.record.as_ref().map(|r| r.id),
            record: entry.record.clone(),
            cells: record_cells(entry.date, entry.record.as_ref(), &entry.evaluation, tz),
        }
    }

    /// 모바일 날짜 줄의 보조 정보: (공휴일 이름), (기록이 있으면) 출퇴근 시각, (퇴근했으면) 근무시간.
    pub fn day_meta(row: &DayRow) -> Vec<String> {
        let mut items: Vec<String> = row.note.iter().cloned().collect();
        items.extend(clock_span(&row.cells));
        if row.cells.worked != EMPTY {
            items.push(row.cells.worked.clone());
        }
        items
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

    /// `출근 09:02 · 퇴근 18:10 · 사무실`. 퇴근이 없으면 `퇴근 없음`.
    fn times_text(
        work_date: NaiveDate,
        clock_in_at: Timestamp,
        clock_out_at: Option<Timestamp>,
        work_type: WorkType,
        tz: Tz,
    ) -> String {
        let clock_out = clock_out_at
            .map(|at| clock_text(at, work_date, tz))
            .unwrap_or_else(|| "없음".to_owned());
        format!(
            "출근 {} · 퇴근 {} · {}",
            clock_text(clock_in_at, work_date, tz),
            clock_out,
            text::work_type_label(work_type)
        )
    }

    /// 기록 한 건의 요약. 기록이 없으면 그렇게 말한다.
    pub fn record_text(record: Option<&AttendanceRecordDto>, tz: Tz) -> String {
        match record {
            Some(r) => times_text(r.work_date, r.clock_in_at, r.clock_out_at, r.work_type, tz),
            None => "기록 없음".to_owned(),
        }
    }

    /// 이력에 저장된 기록 모양(JSON)을 `출근 09:02 · 퇴근 18:10 · 사무실`로.
    pub fn record_snapshot(value: &serde_json::Value, tz: Tz) -> String {
        match serde_json::from_value::<AttendanceRecordDto>(value.clone()) {
            Ok(record) => record_text(Some(&record), tz),
            Err(_) => "내용을 표시할 수 없어요.".to_owned(),
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

    // ───────────── 기록 입력(정정 요청·관리자 수정이 함께 쓴다) ─────────────

    /// 사유·메모 최대 글자 수(서버 `Reason`·`Comment` 규칙과 같다).
    pub const TEXT_MAX_CHARS: usize = 1_000;

    pub fn work_type_options() -> Vec<(String, String)> {
        WorkType::ALL
            .iter()
            .copied()
            .map(|t| (t.as_str().to_owned(), text::work_type_label(t).to_owned()))
            .collect()
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FormIssue {
        MemberMissing,
        WorkDateInvalid,
        ClockInMissing,
        ClockInInvalid,
        ClockInNonexistent,
        ClockOutInvalid,
        ClockOutNonexistent,
        ClockOutNotAfterClockIn,
        ShiftTooLong,
        ReasonMissing,
        ReasonTooLong,
        NoteTooLong,
    }

    impl FormIssue {
        pub const fn message(self) -> &'static str {
            match self {
                Self::MemberMissing => "구성원을 골라 주세요.",
                Self::WorkDateInvalid => "근무일을 확인해 주세요.",
                Self::ClockInMissing => "출근 시각을 입력해 주세요.",
                Self::ClockInInvalid => "출근 시각의 형식이 올바르지 않아요.",
                Self::ClockInNonexistent => {
                    "출근 시각이 서머타임 전환으로 존재하지 않는 시각이에요."
                }
                Self::ClockOutInvalid => "퇴근 시각의 형식이 올바르지 않아요.",
                Self::ClockOutNonexistent => {
                    "퇴근 시각이 서머타임 전환으로 존재하지 않는 시각이에요."
                }
                Self::ClockOutNotAfterClockIn => "퇴근 시각은 출근 시각보다 늦어야 해요.",
                Self::ShiftTooLong => "한 번의 근무는 24시간을 넘을 수 없어요.",
                Self::ReasonMissing => "사유를 입력해 주세요. 이력에 남아요.",
                Self::ReasonTooLong => "사유는 1,000자까지 쓸 수 있어요.",
                Self::NoteTooLong => "메모는 1,000자까지 쓸 수 있어요.",
            }
        }
    }

    /// `datetime-local` 출근·퇴근 입력을 시각으로. 퇴근은 비워 둘 수 있다.
    pub fn parse_times(
        clock_in: &str,
        clock_out: &str,
        tz: Tz,
    ) -> Result<(Timestamp, Option<Timestamp>), FormIssue> {
        let clock_in = format::parse_local_input(clock_in, tz).map_err(|e| match e {
            LocalInputError::Empty => FormIssue::ClockInMissing,
            LocalInputError::Invalid => FormIssue::ClockInInvalid,
            LocalInputError::Nonexistent => FormIssue::ClockInNonexistent,
        })?;
        let clock_out = match format::parse_local_input(clock_out, tz) {
            Ok(at) => Some(at),
            Err(LocalInputError::Empty) => None,
            Err(LocalInputError::Invalid) => return Err(FormIssue::ClockOutInvalid),
            Err(LocalInputError::Nonexistent) => return Err(FormIssue::ClockOutNonexistent),
        };
        if let Some(out) = clock_out {
            if out <= clock_in {
                return Err(FormIssue::ClockOutNotAfterClockIn);
            }
            if out.since(clock_in) > MAX_SHIFT {
                return Err(FormIssue::ShiftTooLong);
            }
        }
        Ok((clock_in, clock_out))
    }

    /// 필수 사유: 앞뒤 공백을 지우고 길이를 본다.
    pub fn parse_reason(raw: &str) -> Result<String, FormIssue> {
        let reason = raw.trim();
        if reason.is_empty() {
            return Err(FormIssue::ReasonMissing);
        }
        if reason.chars().count() > TEXT_MAX_CHARS {
            return Err(FormIssue::ReasonTooLong);
        }
        Ok(reason.to_owned())
    }

    // ───────────── 근태 정정 요청 ─────────────

    /// 정정 요청 창을 여는 날.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CorrectionTarget {
        pub date: NaiveDate,
        pub date_label: String,
        /// 그날의 지금 기록(없을 수 있다).
        pub record: Option<AttendanceRecordDto>,
    }

    /// 그날에 정정을 요청할 수 있어 보이는지: 오늘까지, [`CORRECTION_WINDOW_DAYS`]일 전까지.
    /// `today`는 회사의 근무일 전환 시각을 반영한 오늘 근무일이다(브라우저 시계 기준이라
    /// 시계가 틀리면 서버와 어긋날 수 있다). 최종 판정은 서버가 한다.
    pub fn can_request_correction(date: NaiveDate, today: NaiveDate) -> bool {
        let earliest = today.checked_sub_days(Days::new(CORRECTION_WINDOW_DAYS));
        date <= today && earliest.is_some_and(|earliest| date >= earliest)
    }

    /// 정정 요청 창의 입력값.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CorrectionForm {
        /// `datetime-local` 값.
        pub clock_in: String,
        pub clock_out: String,
        pub work_type: WorkType,
        pub reason: String,
    }

    impl CorrectionForm {
        /// 지금 기록이 있으면 그 값으로 채운다(보통 한 칸만 고친다). 기록이 없으면
        /// `defaults`(회사 근무 시작·종료 시각, `datetime-local` 값)로 채운다.
        pub fn for_record(
            record: Option<&AttendanceRecordDto>,
            tz: Tz,
            defaults: (String, String),
        ) -> Self {
            let (default_in, default_out) = defaults;
            Self {
                clock_in: record
                    .map(|r| format::local_input(r.clock_in_at, tz))
                    .unwrap_or(default_in),
                clock_out: match record {
                    Some(r) => r
                        .clock_out_at
                        .map(|at| format::local_input(at, tz))
                        .unwrap_or_default(),
                    None => default_out,
                },
                work_type: record.map_or(WorkType::Office, |r| r.work_type),
                reason: String::new(),
            }
        }
    }

    /// 오류를 붙일 자리.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CorrectionSlot {
        Form,
        Times,
        Reason,
        Line,
    }

    /// 보내기 전에 알 수 있는 문제.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CorrectionProblem {
        Form(FormIssue),
        Line(&'static str),
    }

    impl CorrectionProblem {
        pub fn message(self) -> &'static str {
            match self {
                Self::Form(issue) => issue.message(),
                Self::Line(message) => message,
            }
        }

        pub fn slot(self) -> CorrectionSlot {
            match self {
                Self::Form(FormIssue::ReasonMissing | FormIssue::ReasonTooLong) => {
                    CorrectionSlot::Reason
                }
                Self::Form(
                    FormIssue::ClockInMissing
                    | FormIssue::ClockInInvalid
                    | FormIssue::ClockInNonexistent
                    | FormIssue::ClockOutInvalid
                    | FormIssue::ClockOutNonexistent
                    | FormIssue::ClockOutNotAfterClockIn
                    | FormIssue::ShiftTooLong,
                ) => CorrectionSlot::Times,
                Self::Form(_) => CorrectionSlot::Form,
                Self::Line(_) => CorrectionSlot::Line,
            }
        }
    }

    /// 폼 값을 요청 본문으로. 결재선은 화면에서 고른 그대로 보낸다(서버가 최종 결재자의 권한을 본다).
    pub fn correction_request(
        work_date: NaiveDate,
        form: &CorrectionForm,
        line: &[ApproverEntry],
        tz: Tz,
    ) -> Result<CreateCorrectionRequest, CorrectionProblem> {
        let (clock_in_at, clock_out_at) =
            parse_times(&form.clock_in, &form.clock_out, tz).map_err(CorrectionProblem::Form)?;
        let reason = parse_reason(&form.reason).map_err(CorrectionProblem::Form)?;
        if let Some(message) = line_problem(line) {
            return Err(CorrectionProblem::Line(message));
        }
        Ok(CreateCorrectionRequest {
            work_date: work_date.to_string(),
            clock_in_at,
            clock_out_at,
            work_type: form.work_type,
            reason,
            steps: Some(to_inputs(line)),
            references: Vec::new(),
        })
    }

    /// 서버 오류를 창 안 어디에 보여 줄지. `None`이면 창 밖(토스트)으로 알린다.
    pub fn correction_failure_slot(code: &str, field: Option<&str>) -> Option<CorrectionSlot> {
        match (code, field) {
            ("validation_failed", Some("clock_in_at" | "clock_out_at" | "work_date")) => {
                Some(CorrectionSlot::Times)
            }
            ("validation_failed", Some("reason")) => Some(CorrectionSlot::Reason),
            ("validation_failed", Some("steps" | "references")) | ("member_inactive", _) => {
                Some(CorrectionSlot::Line)
            }
            ("validation_failed" | "already_exists" | "not_working_member", _) => {
                Some(CorrectionSlot::Form)
            }
            _ => None,
        }
    }

    /// `/attendance/corrections?limit=20[&cursor=…]`
    pub fn corrections_path(cursor: Option<&str>) -> String {
        crate::api::with_cursor("/attendance/corrections?limit=20".to_owned(), cursor)
    }

    /// 요청한 값: `출근 09:00 · 퇴근 18:00 · 사무실`.
    pub fn correction_summary(correction: &CorrectionDto, tz: Tz) -> String {
        times_text(
            correction.work_date,
            correction.clock_in_at,
            correction.clock_out_at,
            correction.work_type,
            tz,
        )
    }

    /// 결재 중일 때만 취소(= 결재 문서 회수)할 수 있다. 누군가 이미 결재했으면 서버가 거절한다.
    pub fn can_cancel_correction(status: CorrectionStatus) -> bool {
        status == CorrectionStatus::Pending
    }

    pub fn cancel_correction_message(correction: &CorrectionDto) -> String {
        format!(
            "{} 정정 요청을 취소해요. 결재 문서도 함께 회수돼요.",
            format::date_md(correction.work_date)
        )
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
        fn day_bar_input_follows_the_record_and_judgement() {
            let hours = (
                MinuteOfDay::parse("09:00").expect("time"),
                MinuteOfDay::parse("18:00").expect("time"),
            );
            let now = ts("2026-03-03T03:00:00Z");
            let none = day_input(
                date("2026-03-03"),
                None,
                &evaluation(DayStatus::Scheduled, 0),
                hours,
                MinuteOfDay::MIDNIGHT,
                now,
                seoul(),
            );
            assert_eq!((none.clock_in, none.clock_out), (None, None));
            assert!(!none.late && !none.missing_clock_out);

            let open = record("2026-03-03", "2026-03-03T00:40:00Z", None);
            let mut late = evaluation(DayStatus::Working, 0);
            late.late_minutes = 40;
            let input = day_input(
                date("2026-03-03"),
                Some(&open),
                &late,
                hours,
                MinuteOfDay::MIDNIGHT,
                now,
                seoul(),
            );
            assert_eq!(input.clock_in, Some(ts("2026-03-03T00:40:00Z")));
            assert_eq!(input.clock_out, None);
            assert!(input.late);
            assert_eq!((input.expected_start, input.expected_end), hours);

            let missing = day_input(
                date("2026-03-03"),
                Some(&open),
                &evaluation(DayStatus::MissingClockOut, 0),
                hours,
                MinuteOfDay::MIDNIGHT,
                now,
                seoul(),
            );
            assert!(missing.missing_clock_out);
        }

        #[test]
        fn bar_summary_reads_like_a_sentence() {
            let none = record_cells(
                date("2026-03-03"),
                None,
                &evaluation(DayStatus::Scheduled, 0),
                seoul(),
            );
            assert_eq!(
                bar_summary("09:00 ~ 18:00", &none),
                format!("근무 시간 09:00 ~ 18:00, 출근 기록 없음, {}", none.status)
            );
            assert_eq!(clock_in_flag(&none), "");

            let open = record("2026-03-03", "2026-03-03T00:02:00Z", None);
            let working = record_cells(
                date("2026-03-03"),
                Some(&open),
                &evaluation(DayStatus::Working, 0),
                seoul(),
            );
            assert_eq!(
                bar_summary("09:00 ~ 18:00", &working),
                format!(
                    "근무 시간 09:00 ~ 18:00, 출근 09:02, 퇴근 전, {}",
                    working.status
                )
            );
            assert_eq!(clock_in_flag(&working), "출근 09:02");

            let closed = record(
                "2026-03-03",
                "2026-03-03T00:02:00Z",
                Some("2026-03-03T09:10:00Z"),
            );
            let done = record_cells(
                date("2026-03-03"),
                Some(&closed),
                &evaluation(DayStatus::Normal, 480),
                seoul(),
            );
            assert!(bar_summary("09:00 ~ 18:00", &done).contains("출근 09:02, 퇴근 18:10"));
        }

        #[test]
        fn week_inputs_come_from_today_and_month_records() {
            let now = ts("2026-03-03T03:02:00Z");
            let working = TodayStage::Working {
                clock_in_at: ts("2026-03-03T00:02:00Z"),
                work_type: WorkType::Office,
                overnight: false,
            };
            assert_eq!(live_minutes(&working, now), 180);
            assert_eq!(live_minutes(&TodayStage::ClockIn, now), 0);
            let finished = TodayStage::Finished {
                clock_in_at: ts("2026-03-03T00:02:00Z"),
                clock_out_at: now,
                worked_minutes: 120,
                work_type: WorkType::Office,
            };
            assert_eq!(
                live_minutes(&finished, now),
                0,
                "퇴근한 날은 서버 판정에 들어 있다"
            );

            let entry = |day: &str, minutes: u32| DayEntry {
                date: date(day),
                is_working_day: true,
                holiday_name: None,
                record: None,
                evaluation: evaluation(DayStatus::Normal, minutes),
            };
            assert_eq!(
                worked_days(&[entry("2026-03-02", 480), entry("2026-03-03", 0)]),
                vec![(date("2026-03-02"), 480), (date("2026-03-03"), 0)]
            );

            assert_eq!(record_stamp(None), "");
            let open = record("2026-03-03", "2026-03-03T00:02:00Z", None);
            let mut closed = open.clone();
            closed.version = 2;
            assert_ne!(record_stamp(Some(&open)), record_stamp(Some(&closed)));
        }

        #[test]
        fn mobile_day_meta_lists_only_what_exists() {
            let entry = DayEntry {
                date: date("2026-03-01"),
                is_working_day: false,
                holiday_name: Some("삼일절".into()),
                record: None,
                evaluation: evaluation(DayStatus::DayOff, 0),
            };
            assert_eq!(day_meta(&day_row(&entry, seoul())), vec!["삼일절"]);
            let worked = DayEntry {
                date: date("2026-03-03"),
                is_working_day: true,
                holiday_name: None,
                record: Some(record(
                    "2026-03-03",
                    "2026-03-03T00:02:00Z",
                    Some("2026-03-03T09:10:00Z"),
                )),
                evaluation: evaluation(DayStatus::Normal, 488),
            };
            assert_eq!(
                day_meta(&day_row(&worked, seoul())),
                vec!["09:02 – 18:10".to_owned(), format::duration_minutes(488)]
            );
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

        fn approver(kind: pango_core::approval::StepKind) -> ApproverEntry {
            ApproverEntry {
                approver_id: MemberId::new(),
                approver_name: "팀장".into(),
                kind,
            }
        }

        fn filled() -> CorrectionForm {
            CorrectionForm {
                clock_in: "2026-03-02T09:00".into(),
                clock_out: "2026-03-02T18:00".into(),
                work_type: WorkType::Office,
                reason: " 지문 인식 실패 ".into(),
            }
        }

        #[test]
        fn correction_window_is_today_back_to_31_days() {
            let today = date("2026-03-31");
            assert!(can_request_correction(today, today));
            assert!(!can_request_correction(date("2026-04-01"), today), "미래");
            assert!(can_request_correction(date("2026-02-28"), today), "31일 전");
            assert!(
                !can_request_correction(date("2026-02-27"), today),
                "32일 전"
            );
            assert!(!can_request_correction(NaiveDate::MIN, NaiveDate::MIN));
        }

        #[test]
        fn correction_form_starts_from_the_current_record() {
            let tz = seoul();
            let none = || (String::new(), String::new());
            let blank = CorrectionForm::for_record(None, tz, none());
            assert_eq!(blank.clock_in, "");
            assert_eq!(blank.clock_out, "");
            assert_eq!(blank.work_type, WorkType::Office);
            let hours = || ("2026-03-02T09:00".to_owned(), "2026-03-02T18:00".to_owned());
            let default = CorrectionForm::for_record(None, tz, hours());
            assert_eq!(
                default.clock_in, "2026-03-02T09:00",
                "기록이 없으면 근무 시간"
            );
            assert_eq!(default.clock_out, "2026-03-02T18:00");
            let open = record("2026-03-02", "2026-03-02T00:05:00Z", None);
            let form = CorrectionForm::for_record(Some(&open), tz, hours());
            assert_eq!(form.clock_in, "2026-03-02T09:05");
            assert_eq!(form.clock_out, "");
            assert_eq!(form.work_type, WorkType::Remote);
            assert_eq!(form.reason, "");
        }

        #[test]
        fn correction_request_converts_wall_clock_and_line() {
            let tz = seoul();
            let line = vec![approver(pango_core::approval::StepKind::Approve)];
            let request = correction_request(date("2026-03-02"), &filled(), &line, tz).expect("ok");
            assert_eq!(request.work_date, "2026-03-02");
            assert_eq!(request.clock_in_at, ts("2026-03-02T00:00:00Z"));
            assert_eq!(request.clock_out_at, Some(ts("2026-03-02T09:00:00Z")));
            assert_eq!(request.reason, "지문 인식 실패");
            let steps = request.steps.expect("steps");
            assert_eq!(steps.len(), 1);
            assert_eq!(steps[0].approver_id, line[0].approver_id);
            assert!(request.references.is_empty());

            let mut no_out = filled();
            no_out.clock_out = "  ".into();
            let request = correction_request(date("2026-03-02"), &no_out, &line, tz).expect("ok");
            assert_eq!(request.clock_out_at, None);
        }

        #[test]
        fn correction_request_reports_problems_where_they_belong() {
            let tz = seoul();
            let line = vec![approver(pango_core::approval::StepKind::Approve)];
            let day = date("2026-03-02");
            let check = |change: fn(&mut CorrectionForm), slot: CorrectionSlot| {
                let mut form = filled();
                change(&mut form);
                let problem = correction_request(day, &form, &line, tz).expect_err("problem");
                assert_eq!(problem.slot(), slot, "{problem:?}");
                assert!(!problem.message().is_empty());
            };
            check(|f| f.clock_in = String::new(), CorrectionSlot::Times);
            check(|f| f.clock_in = "09:00".into(), CorrectionSlot::Times);
            check(
                |f| f.clock_out = "2026-03-02T08:00".into(),
                CorrectionSlot::Times,
            );
            check(
                |f| f.clock_out = "2026-03-03T09:01".into(),
                CorrectionSlot::Times,
            );
            check(|f| f.reason = "  ".into(), CorrectionSlot::Reason);
            check(|f| f.reason = "가".repeat(1_001), CorrectionSlot::Reason);

            let empty = correction_request(day, &filled(), &[], tz).expect_err("line");
            assert_eq!(empty.slot(), CorrectionSlot::Line);
            let agree_last = vec![approver(pango_core::approval::StepKind::Agree)];
            assert_eq!(
                correction_request(day, &filled(), &agree_last, tz)
                    .expect_err("line")
                    .slot(),
                CorrectionSlot::Line
            );
            assert_eq!(
                CorrectionProblem::Form(FormIssue::MemberMissing).slot(),
                CorrectionSlot::Form
            );
        }

        #[test]
        fn server_failures_are_placed_in_the_form() {
            use CorrectionSlot::*;
            let cases = [
                ("validation_failed", Some("clock_in_at"), Some(Times)),
                ("validation_failed", Some("work_date"), Some(Times)),
                ("validation_failed", Some("reason"), Some(Reason)),
                ("validation_failed", Some("steps"), Some(Line)),
                ("validation_failed", Some("references"), Some(Line)),
                ("member_inactive", None, Some(Line)),
                ("validation_failed", Some("work_type"), Some(Form)),
                ("already_exists", None, Some(Form)),
                ("not_working_member", None, Some(Form)),
                ("forbidden", None, None),
                ("network_error", None, None),
                ("idempotency_key_reused", None, None),
            ];
            for (code, field, slot) in cases {
                assert_eq!(
                    correction_failure_slot(code, field),
                    slot,
                    "{code} {field:?}"
                );
            }
        }

        fn correction(status: CorrectionStatus) -> CorrectionDto {
            CorrectionDto {
                id: pango_core::id::AttendanceCorrectionId::new(),
                member_id: MemberId::new(),
                member_name: "나".into(),
                work_date: date("2026-03-02"),
                clock_in_at: ts("2026-03-02T00:00:00Z"),
                clock_out_at: Some(ts("2026-03-02T17:10:00Z")),
                work_type: WorkType::Field,
                reason: "외근".into(),
                status,
                document_id: pango_core::id::DocumentId::new(),
                created_at: ts("2026-03-03T00:00:00Z"),
                decided_at: None,
            }
        }

        #[test]
        fn correction_rows_describe_requested_values() {
            let tz = seoul();
            assert_eq!(
                correction_summary(&correction(CorrectionStatus::Pending), tz),
                "출근 09:00 · 퇴근 02:10 (+1일) · 외근"
            );
            assert_eq!(record_text(None, tz), "기록 없음");
            assert!(
                cancel_correction_message(&correction(CorrectionStatus::Pending))
                    .starts_with("3월 2일 (월)")
            );
            for status in CorrectionStatus::ALL.iter().copied() {
                assert_eq!(
                    can_cancel_correction(status),
                    status == CorrectionStatus::Pending,
                    "{status:?}"
                );
            }
            assert_eq!(corrections_path(None), "/attendance/corrections?limit=20");
            assert_eq!(
                corrections_path(Some("a&b")),
                "/attendance/corrections?limit=20&cursor=a%26b"
            );
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
            assert_eq!(row.after, "내용을 표시할 수 없어요.");
            assert_eq!(row.actor, "홍길동");
        }
    }
}
