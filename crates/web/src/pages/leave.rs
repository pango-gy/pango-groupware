//! 휴가: 잔여 카드, 내 신청, 휴가 달력, 사용 내역.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use chrono_tz::Tz;
use dioxus::prelude::*;
use pango_api::leave::{BalanceDto, LeaveCalendarEntry, LeaveRequestDto, LedgerEntryDto};

use crate::api;
use crate::calendar::YearMonth;
use crate::company::{is_day_off, use_company};
use crate::format;
use crate::route::Route;
use crate::session::{use_session, use_toasts};
use crate::text;
use crate::ui::{
    report_success, resource_view, today, Badge, Button, ButtonKind, Card, ConfirmModal,
    EmptyState, Icon, IconName, Meta, MonthCalendar, MonthSwitcher, PageHeader, Stat, Tabs, Tone,
    YearSwitcher,
};

const TAB_REQUESTS: &str = "requests";
const TAB_CALENDAR: &str = "calendar";
const TAB_LEDGER: &str = "ledger";

#[component]
pub fn Leave() -> Element {
    let tz = use_session().zone();
    let today = today(tz);
    let mut tab = use_signal(|| TAB_REQUESTS.to_owned());
    let mut balance = use_resource(|| async { api::get::<BalanceDto>("/leave/balance").await });

    let tabs = vec![
        (TAB_REQUESTS.to_owned(), "내 신청".to_owned(), None),
        (TAB_CALENDAR.to_owned(), "휴가 달력".to_owned(), None),
        (TAB_LEDGER.to_owned(), "사용 내역".to_owned(), None),
    ];

    rsx! {
        PageHeader {
            title: "휴가",
            description: "남은 연차와 신청 내역을 확인해요.",
            actions: rsx! {
                Link { class: "btn btn-primary", to: Route::LeaveNew {},
                    Icon { name: IconName::Plus }
                    "휴가 신청"
                }
            },
        }
        Card {
            {resource_view(balance, |dto: BalanceDto| rsx! { BalanceSummary { dto } })}
        }
        Tabs { tabs, active: tab(), onselect: move |key| tab.set(key) }
        match tab().as_str() {
            TAB_CALENDAR => rsx! { CalendarTab { today } },
            TAB_LEDGER => rsx! { LedgerTab { tz } },
            _ => rsx! { RequestsTab { today, onchanged: move |_| balance.restart() } },
        }
    }
}

/// 남은 연차(크게)와 곧 소멸할 수량. 휴가 화면과 홈이 함께 쓴다.
#[component]
pub(crate) fn BalanceHeadline(dto: BalanceDto) -> Element {
    let expiring = dto.expiring_soon.quarters();
    rsx! {
        div { class: "figure",
            span { class: "figure-label", "남은 연차" }
            strong { class: "figure-value tnum", {format::leave_days(dto.balance.remaining.quarters())} }
            if expiring > 0 {
                p { class: "leave-expiring",
                    Badge { tone: Tone::Warning, "곧 소멸" }
                    " 30일 안에 {format::leave_days(expiring)}이 사라져요."
                }
            }
        }
    }
}

#[component]
fn BalanceSummary(dto: BalanceDto) -> Element {
    let balance = dto.balance;
    rsx! {
        BalanceHeadline { dto }
        div { class: "stats",
            Stat { label: "부여", value: format::leave_days(balance.granted.quarters()) }
            Stat { label: "사용", value: format::leave_days(balance.used.quarters()) }
            Stat { label: "소멸", value: format::leave_days(balance.expired.quarters()) }
        }
        p { class: "field-hint", "{format::date_ymd(dto.as_of)} 기준" }
    }
}

// ───────────── 내 신청 ─────────────

#[component]
fn RequestsTab(today: NaiveDate, onchanged: EventHandler<()>) -> Element {
    let mut toasts = use_toasts();
    let (min_year, max_year) = logic::year_bounds(today);
    let mut year = use_signal(|| today.year());
    let mut confirm = use_signal(|| None::<LeaveRequestDto>);
    let mut busy = use_signal(|| false);
    let mut requests = use_resource(move || async move {
        api::get::<Vec<LeaveRequestDto>>(&format!("/leave/requests?year={}", year())).await
    });

    let cancel = move |id: String| async move {
        if busy() {
            return;
        }
        busy.set(true);
        let path = format!("/leave/requests/{id}/cancel");
        match api::post::<_, LeaveRequestDto>(&path, &pango_api::Empty {}).await {
            Ok(_) => {
                report_success("휴가 신청을 취소했어요.");
                confirm.set(None);
                requests.restart();
                onchanged.call(());
            }
            Err(error) => {
                toasts.error(text::leave_cancel_error_message(
                    &error.code,
                    error.field.as_deref(),
                    error.reason.as_deref(),
                ));
                confirm.set(None);
                requests.restart();
            }
        }
        busy.set(false);
    };

    rsx! {
        Card {
            div { class: "toolbar",
                YearSwitcher {
                    year: year(),
                    min: min_year,
                    max: max_year,
                    onchange: move |value| year.set(logic::clamp_year(value, (min_year, max_year))),
                }
            }
            {resource_view(requests, move |list: Vec<LeaveRequestDto>| rsx! {
                if list.is_empty() {
                    EmptyState {
                        title: "이 해에 신청한 휴가가 없어요.",
                        icon: IconName::Calendar,
                    }
                } else {
                    Fragment {
                        ul { class: "list",
                            for item in list {
                                RequestRow {
                                    key: "{item.id}",
                                    cancelable: logic::can_cancel(&item, today),
                                    item: item.clone(),
                                    oncancel: move |item| confirm.set(Some(item)),
                                }
                            }
                        }
                    }
                }
            })}
        }
        if let Some(target) = confirm() {
            ConfirmModal {
                title: "휴가 신청 취소",
                message: logic::cancel_message(&target),
                confirm_label: "신청 취소",
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

/// 휴가 신청 한 줄. 내 신청·홈(다가오는 휴가)·팀 휴가가 함께 쓴다.
///
/// - `show_member`: 신청자 이름을 앞에 붙인다(팀 휴가).
/// - `document_link`: 결재 문서 링크. 문서의 당사자만 열 수 있으므로 본인 신청에만 켠다.
#[component]
pub(crate) fn RequestRow(
    item: LeaveRequestDto,
    #[props(default)] cancelable: bool,
    #[props(default)] oncancel: EventHandler<LeaveRequestDto>,
    #[props(default)] show_member: bool,
    #[props(default = true)] document_link: bool,
) -> Element {
    let period = logic::period_label(item.start_date, item.end_date);
    rsx! {
        li { class: "list-row list-row-top",
            div { class: "list-row-main",
                div { class: "row-title",
                    if show_member {
                        strong { "{item.member_name}" }
                        span { "{item.leave_type_name}" }
                    } else {
                        strong { "{item.leave_type_name}" }
                    }
                    Badge { tone: text::leave_status_tone(item.status), {text::leave_status_label(item.status)} }
                }
                Meta {
                    items: vec![
                        period,
                        text::leave_unit_label(item.unit).to_owned(),
                        format::leave_days(item.amount.quarters()),
                    ],
                }
                p { class: "row-reason", "{item.reason}" }
            }
            div { class: "list-row-side",
                if document_link {
                    Link {
                        to: Route::ApprovalDetail { id: item.document_id.to_string() },
                        "결재 문서"
                    }
                }
                if cancelable {
                    Button {
                        kind: ButtonKind::Grey,
                        small: true,
                        onclick: {
                            let item = item.clone();
                            move |_| oncancel.call(item.clone())
                        },
                        "취소"
                    }
                }
            }
        }
    }
}

// ───────────── 휴가 달력 ─────────────

#[component]
fn CalendarTab(today: NaiveDate) -> Element {
    // 회사 근무 요일. 읽기 전에는 월~금으로 칠한다.
    let week = use_company().work_week();
    let mut month = use_signal(|| YearMonth::of(today));
    let mut selected = use_signal(|| today);
    let entries = use_resource(move || async move {
        let query = month().query();
        api::get::<Vec<LeaveCalendarEntry>>(&format!(
            "/leave/calendar?month={}",
            api::encode(&query)
        ))
        .await
    });

    let current = month();
    rsx! {
        Card { class: "leave-calendar",
            div { class: "toolbar",
                MonthSwitcher {
                    month: current,
                    onchange: move |next: YearMonth| {
                        month.set(next);
                        selected.set(logic::default_selected_day(next, today));
                    },
                }
                p { class: "leave-legend",
                    span { class: "calendar-chip leave-chip", "승인" }
                    span { class: "calendar-chip leave-chip leave-chip-pending", "결재 중" }
                }
            }
            {resource_view(entries, move |list: Vec<LeaveCalendarEntry>| {
                let grouped = logic::entries_by_date(&list, current, week);
                let agenda = grouped.get(&selected()).cloned().unwrap_or_default();
                rsx! {
                    MonthCalendar {
                        month: current,
                        day: move |date: NaiveDate| {
                            let people = grouped.get(&date).cloned().unwrap_or_default();
                            rsx! {
                                DayCell {
                                    date,
                                    people,
                                    today,
                                    off: is_day_off(week, date),
                                    selected: selected() == date,
                                    onselect: move |date| selected.set(date),
                                }
                            }
                        },
                    }
                    DayAgenda { date: selected(), people: agenda }
                }
            })}
        }
    }
}

#[component]
fn DayCell(
    date: NaiveDate,
    people: Vec<logic::DayEntry>,
    today: NaiveDate,
    /// 근무 요일이 아닌 날(흐리게).
    off: bool,
    selected: bool,
    onselect: EventHandler<NaiveDate>,
) -> Element {
    const VISIBLE: usize = 3;
    let mut class = "leave-day".to_owned();
    if off {
        class.push_str(" leave-day-weekend");
    }
    if selected {
        class.push_str(" leave-day-selected");
    }
    let date_class = if date == today {
        "calendar-date calendar-date-today"
    } else if off {
        "calendar-date calendar-date-off"
    } else {
        "calendar-date"
    };
    let hidden = people.len().saturating_sub(VISIBLE);
    let count = people.len();
    rsx! {
        button {
            class,
            r#type: "button",
            "aria-pressed": selected,
            "aria-label": logic::day_aria_label(date, count),
            onclick: move |_| onselect.call(date),
            span { class: date_class, "{date.day()}" }
            for person in people.iter().take(VISIBLE).cloned() {
                span {
                    key: "{person.member_id}-{person.unit.as_str()}",
                    class: if person.pending { "calendar-chip leave-chip leave-chip-pending" } else { "calendar-chip leave-chip" },
                    "{person.member_name} {text::leave_unit_short(person.unit)}"
                    if person.pending {
                        span { class: "sr-only", " 결재 중" }
                    }
                }
            }
            if hidden > 0 {
                span { class: "leave-more", "+{hidden}" }
            }
            if count > 0 {
                span { class: "leave-count", "aria-hidden": "true", "{count}" }
            }
        }
    }
}

#[component]
fn DayAgenda(date: NaiveDate, people: Vec<logic::DayEntry>) -> Element {
    rsx! {
        section { class: "leave-agenda", "aria-live": "polite",
            h3 { class: "sub-title", "{format::date_md(date)}" }
            if people.is_empty() {
                p { class: "field-hint", "이날 휴가인 사람이 없어요." }
            } else {
                ul { class: "list",
                    for person in people {
                        li { key: "{person.member_id}-{person.unit.as_str()}", class: "list-row",
                            div { class: "list-row-main",
                                strong { "{person.member_name}" }
                                small { {text::leave_unit_label(person.unit)} }
                            }
                            div { class: "list-row-side",
                                Badge { tone: text::leave_status_tone(person.status()), {text::leave_status_label(person.status())} }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ───────────── 사용 내역 ─────────────

#[component]
fn LedgerTab(tz: Tz) -> Element {
    let ledger = use_resource(|| async { api::get::<Vec<LedgerEntryDto>>("/leave/ledger").await });
    rsx! {
        {resource_view(ledger, move |entries: Vec<LedgerEntryDto>| {
            let lines = logic::ledger_lines(&entries, tz);
            rsx! {
                if lines.is_empty() {
                    EmptyState { title: "아직 내역이 없어요.", icon: IconName::List }
                } else {
                    Card {
                        ul { class: "list",
                            for line in lines {
                                li { key: "{line.key}", class: "list-row",
                                    Badge { tone: line.tone, "{line.label}" }
                                    div { class: "list-row-main",
                                        strong { class: "tnum", "{line.amount}" }
                                        small { "{line.detail}" }
                                        if let Some(memo) = &line.memo {
                                            p { class: "row-reason", "{memo}" }
                                        }
                                    }
                                    div { class: "list-row-side", small { "{line.recorded}" } }
                                }
                            }
                        }
                    }
                }
            }
        })}
    }
}

/// 화면 계산(순수 함수).
pub(crate) mod logic {
    use super::*;
    use pango_api::leave::LedgerKind;
    use pango_core::calendar::WorkWeek;
    use pango_core::leave::LeaveRequestStatus;
    use pango_core::leave::LeaveUnit;
    use pango_core::time::Timestamp;

    /// 신청 목록에서 고를 수 있는 가장 이른 해. 서버가 받는 범위(2000~)와 같다.
    pub const MIN_YEAR: i32 = 2000;

    /// 연도 이동 범위: 2000년 ~ 내년(미리 낸 신청을 볼 수 있게).
    pub fn year_bounds(today: NaiveDate) -> (i32, i32) {
        (MIN_YEAR, today.year().saturating_add(1).max(MIN_YEAR))
    }

    pub fn clamp_year(year: i32, (min, max): (i32, i32)) -> i32 {
        year.clamp(min, max)
    }

    /// 본인이 취소할 수 있어 보이는지. 결재 중이면 언제나, 승인이면 시작 전까지.
    /// (서버는 첫 근무일 기준으로 다시 판정한다.)
    pub fn can_cancel(request: &LeaveRequestDto, today: NaiveDate) -> bool {
        match request.status {
            LeaveRequestStatus::Pending => true,
            LeaveRequestStatus::Approved => today < request.start_date,
            LeaveRequestStatus::Rejected | LeaveRequestStatus::Canceled => false,
        }
    }

    pub fn period_label(start: NaiveDate, end: NaiveDate) -> String {
        if start == end {
            format::date_md(start)
        } else {
            format!("{} ~ {}", format::date_md(start), format::date_md(end))
        }
    }

    pub fn cancel_message(request: &LeaveRequestDto) -> String {
        let period = period_label(request.start_date, request.end_date);
        match request.status {
            LeaveRequestStatus::Pending => format!(
                "{period} {} 신청을 취소해요. 결재 문서도 함께 회수돼요.",
                request.leave_type_name
            ),
            _ => format!(
                "{period} {} 휴가를 취소해요. 사용한 연차는 복원돼요.",
                request.leave_type_name
            ),
        }
    }

    /// 달을 옮겼을 때 고를 날: 그 달에 오늘이 있으면 오늘, 아니면 1일.
    pub fn default_selected_day(month: YearMonth, today: NaiveDate) -> NaiveDate {
        if YearMonth::of(today) == month {
            today
        } else {
            month.first_day()
        }
    }

    pub fn day_aria_label(date: NaiveDate, count: usize) -> String {
        if count == 0 {
            format::date_md(date)
        } else {
            format!("{}, 휴가 {count}명", format::date_md(date))
        }
    }

    /// 달력 칸 하나에 보이는 사람. 사유·휴가 종류는 API가 주지 않으므로 없다.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct DayEntry {
        pub member_id: pango_core::id::MemberId,
        pub member_name: String,
        pub unit: LeaveUnit,
        pub pending: bool,
    }

    impl DayEntry {
        /// 달력에는 결재 중·승인된 신청만 나온다.
        pub fn status(&self) -> LeaveRequestStatus {
            if self.pending {
                LeaveRequestStatus::Pending
            } else {
                LeaveRequestStatus::Approved
            }
        }
    }

    /// 달력 항목을 그 달의 날짜별로 나눈다. 근무 요일(`week`)이 아닌 날은 휴가일이 아니므로
    /// 넣지 않는다.
    /// 같은 날에는 승인 → 대기, 이름 순으로 놓는다.
    pub fn entries_by_date(
        entries: &[LeaveCalendarEntry],
        month: YearMonth,
        week: WorkWeek,
    ) -> BTreeMap<NaiveDate, Vec<DayEntry>> {
        let (first, last) = (month.first_day(), month.last_day());
        let mut map: BTreeMap<NaiveDate, Vec<DayEntry>> = BTreeMap::new();
        for entry in entries {
            let start = entry.start_date.max(first);
            let end = entry.end_date.min(last);
            if end < start {
                continue;
            }
            // 한 달로 잘랐으므로 최대 31일만 돈다.
            for date in start.iter_days().take_while(|d| *d <= end) {
                if is_day_off(week, date) {
                    continue;
                }
                map.entry(date).or_default().push(DayEntry {
                    member_id: entry.member_id,
                    member_name: entry.member_name.clone(),
                    unit: entry.unit,
                    pending: entry.status == LeaveRequestStatus::Pending,
                });
            }
        }
        for people in map.values_mut() {
            people.sort_by(|a, b| {
                (a.pending, &a.member_name, a.member_id).cmp(&(
                    b.pending,
                    &b.member_name,
                    b.member_id,
                ))
            });
        }
        map
    }

    /// 원장 항목이 잔여에 미치는 방향을 붙인 수량(1/4일). 사용은 빼고, 복원은 더한다.
    pub fn signed_quarters(kind: LedgerKind, quarters: i32) -> i32 {
        match kind {
            LedgerKind::Use => quarters.saturating_abs().saturating_neg(),
            LedgerKind::Grant | LedgerKind::Restore => quarters.saturating_abs(),
            LedgerKind::Adjust => quarters,
        }
    }

    /// `+1.5일`, `-0.25일`, `0일`.
    pub fn signed_days(quarters: i32) -> String {
        if quarters > 0 {
            format!("+{}", format::leave_days(quarters))
        } else {
            format::leave_days(quarters)
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct LedgerLine {
        pub key: String,
        pub label: &'static str,
        pub tone: Tone,
        pub amount: String,
        pub detail: String,
        pub memo: Option<String>,
        pub recorded: String,
    }

    /// 사용 내역. 최근 기록이 위로 온다.
    pub fn ledger_lines(entries: &[LedgerEntryDto], tz: Tz) -> Vec<LedgerLine> {
        let mut sorted: Vec<&LedgerEntryDto> = entries.iter().collect();
        sorted.sort_by_key(|e| std::cmp::Reverse(e.created_at));
        sorted
            .into_iter()
            .map(|entry| {
                let signed = signed_quarters(entry.kind, entry.amount.quarters());
                LedgerLine {
                    key: entry.id.to_string(),
                    label: text::ledger_kind_label(entry.kind),
                    tone: text::ledger_kind_tone(entry.kind),
                    amount: signed_days(signed),
                    detail: text::ledger_detail(entry),
                    memo: entry.memo.clone().filter(|m| !m.trim().is_empty()),
                    recorded: recorded_on(entry.created_at, tz),
                }
            })
            .collect()
    }

    fn recorded_on(at: Timestamp, tz: Tz) -> String {
        format::date_ymd(at.to_utc().with_timezone(&tz).date_naive())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pango_core::id::{
            DocumentId, LeaveLedgerEntryId, LeaveRequestId, LeaveTypeId, MemberId,
        };
        use pango_core::leave::LeaveAmount;

        fn d(value: &str) -> NaiveDate {
            value.parse().expect("date")
        }

        fn ts(rfc3339: &str) -> Timestamp {
            Timestamp::from_utc(
                chrono::DateTime::parse_from_rfc3339(rfc3339)
                    .expect("rfc3339")
                    .with_timezone(&chrono::Utc),
            )
        }

        fn request(status: LeaveRequestStatus, start: &str, end: &str) -> LeaveRequestDto {
            LeaveRequestDto {
                id: LeaveRequestId::new(),
                member_id: MemberId::new(),
                member_name: "홍길동".into(),
                leave_type_id: LeaveTypeId::new(),
                leave_type_name: "연차".into(),
                start_date: d(start),
                end_date: d(end),
                unit: LeaveUnit::FullDay,
                amount: LeaveAmount::from_quarters(4),
                reason: "개인 사정".into(),
                status,
                document_id: DocumentId::new(),
                created_at: ts("2026-03-01T00:00:00Z"),
                decided_at: None,
            }
        }

        fn cal(
            name: &str,
            start: &str,
            end: &str,
            status: LeaveRequestStatus,
        ) -> LeaveCalendarEntry {
            LeaveCalendarEntry {
                member_id: MemberId::new(),
                member_name: name.into(),
                start_date: d(start),
                end_date: d(end),
                unit: LeaveUnit::FullDay,
                status,
            }
        }

        #[test]
        fn year_bounds_allow_next_year_and_clamp() {
            let bounds = year_bounds(d("2026-11-20"));
            assert_eq!(bounds, (2000, 2027));
            assert_eq!(clamp_year(1999, bounds), 2000);
            assert_eq!(clamp_year(2030, bounds), 2027);
            assert_eq!(clamp_year(2026, bounds), 2026);
            assert_eq!(year_bounds(d("1990-01-01")), (2000, 2000));
        }

        #[test]
        fn cancel_is_offered_only_before_start_or_while_pending() {
            let today = d("2026-03-10");
            let pending_past = request(LeaveRequestStatus::Pending, "2026-03-02", "2026-03-02");
            assert!(can_cancel(&pending_past, today));
            let approved_future = request(LeaveRequestStatus::Approved, "2026-03-11", "2026-03-12");
            assert!(can_cancel(&approved_future, today));
            let approved_today = request(LeaveRequestStatus::Approved, "2026-03-10", "2026-03-10");
            assert!(!can_cancel(&approved_today, today), "시작한 날은 불가");
            for status in [LeaveRequestStatus::Rejected, LeaveRequestStatus::Canceled] {
                assert!(!can_cancel(
                    &request(status, "2026-04-01", "2026-04-01"),
                    today
                ));
            }
            assert!(cancel_message(&pending_past).contains("회수"));
            assert!(cancel_message(&approved_future).contains("복원"));
        }

        #[test]
        fn periods_and_default_days() {
            assert_eq!(
                period_label(d("2026-03-02"), d("2026-03-02")),
                "3월 2일 (월)"
            );
            assert_eq!(
                period_label(d("2026-03-02"), d("2026-03-04")),
                "3월 2일 (월) ~ 3월 4일 (수)"
            );
            let march = YearMonth::of(d("2026-03-01"));
            assert_eq!(
                default_selected_day(march, d("2026-03-15")),
                d("2026-03-15")
            );
            assert_eq!(
                default_selected_day(march, d("2026-04-15")),
                d("2026-03-01")
            );
            assert_eq!(day_aria_label(d("2026-03-02"), 0), "3월 2일 (월)");
            assert_eq!(day_aria_label(d("2026-03-02"), 2), "3월 2일 (월), 휴가 2명");
        }

        #[test]
        fn calendar_groups_by_working_day_within_month() {
            let march = YearMonth::of(d("2026-03-01"));
            let entries = vec![
                // 2월 말부터 3월 3일(화)까지: 3월 1일(일)은 빠지고 2·3일만.
                cal(
                    "나",
                    "2026-02-26",
                    "2026-03-03",
                    LeaveRequestStatus::Approved,
                ),
                cal(
                    "가",
                    "2026-03-02",
                    "2026-03-02",
                    LeaveRequestStatus::Pending,
                ),
                cal(
                    "다",
                    "2026-03-02",
                    "2026-03-02",
                    LeaveRequestStatus::Approved,
                ),
                // 다른 달·뒤집힌 구간은 무시.
                cal(
                    "라",
                    "2026-04-01",
                    "2026-04-02",
                    LeaveRequestStatus::Approved,
                ),
                cal(
                    "마",
                    "2026-03-05",
                    "2026-03-04",
                    LeaveRequestStatus::Approved,
                ),
                // 주말만 걸친 신청은 칸이 없다.
                cal(
                    "바",
                    "2026-03-07",
                    "2026-03-08",
                    LeaveRequestStatus::Approved,
                ),
            ];
            let map = entries_by_date(&entries, march, WorkWeek::MON_TO_FRI);
            assert_eq!(
                map.keys().copied().collect::<Vec<_>>(),
                [d("2026-03-02"), d("2026-03-03")]
            );
            let monday: Vec<_> = map[&d("2026-03-02")]
                .iter()
                .map(|p| (p.member_name.as_str(), p.pending))
                .collect();
            assert_eq!(monday, [("나", false), ("다", false), ("가", true)]);
            assert!(entries_by_date(&[], march, WorkWeek::MON_TO_FRI).is_empty());
            // 토요일도 근무하는 회사면 주말에 걸친 신청의 토요일 칸이 생긴다.
            let six_days = WorkWeek::from_bits(0b011_1111).expect("week");
            let saturday = entries_by_date(&entries, march, six_days);
            assert!(saturday.contains_key(&d("2026-03-07")));
            assert!(!saturday.contains_key(&d("2026-03-08")));
        }

        #[test]
        fn calendar_survives_huge_ranges() {
            let march = YearMonth::of(d("2026-03-01"));
            let entry = cal(
                "길",
                "2000-01-01",
                "2099-12-31",
                LeaveRequestStatus::Approved,
            );
            let map = entries_by_date(&[entry], march, WorkWeek::MON_TO_FRI);
            assert_eq!(map.len(), 22, "2026년 3월의 평일 수");
        }

        #[test]
        fn ledger_amounts_are_signed_by_effect() {
            assert_eq!(signed_quarters(LedgerKind::Use, 4), -4);
            assert_eq!(signed_quarters(LedgerKind::Use, -4), -4);
            assert_eq!(signed_quarters(LedgerKind::Restore, 4), 4);
            assert_eq!(signed_quarters(LedgerKind::Grant, 60), 60);
            assert_eq!(signed_quarters(LedgerKind::Adjust, -3), -3);
            assert_eq!(signed_quarters(LedgerKind::Use, i32::MIN), -i32::MAX);
            assert_eq!(signed_days(6), "+1.5일");
            assert_eq!(signed_days(-1), "-0.25일");
            assert_eq!(signed_days(0), "0일");
        }

        #[test]
        fn ledger_lines_are_newest_first_with_dates() {
            let entry = |kind, q, at: &str| LedgerEntryDto {
                id: LeaveLedgerEntryId::new(),
                kind,
                amount: LeaveAmount::from_quarters(q),
                valid_from: Some(d("2026-01-01")),
                expires_on: Some(d("2026-12-31")),
                effective_on: Some(d("2026-03-02")),
                request_id: None,
                memo: Some("  ".into()),
                created_at: ts(at),
            };
            let lines = ledger_lines(
                &[
                    entry(LedgerKind::Grant, 60, "2026-01-01T00:00:00Z"),
                    entry(LedgerKind::Use, 4, "2026-02-27T15:30:00Z"),
                ],
                format::zone("Asia/Seoul"),
            );
            assert_eq!(lines[0].label, "사용");
            assert_eq!(lines[0].amount, "-1일");
            assert_eq!(lines[0].recorded, "2026. 2. 28.", "서울 기준 날짜");
            assert_eq!(lines[0].memo, None, "공백 메모는 숨김");
            assert_eq!(lines[1].amount, "+15일");
            assert!(lines[1].detail.contains("2026. 12. 31."));
        }
    }
}
