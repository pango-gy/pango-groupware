//! 근무 정책(회사 설정)과 공휴일 관리.

use chrono::Datelike;
use dioxus::prelude::*;
use pango_api::admin::{CompanySettingsDto, HolidayDto, HolidayImportRequest, HolidayImportResult};
use pango_api::{Empty, Versioned};
use pango_core::attendance::MAX_BREAK_TIERS;
use pango_core::text::{CompanyName, LabelName};

use crate::api;
use crate::company::refresh_company;
use crate::format;
use crate::session::use_session;
use crate::ui::{
    report_error, report_success, resource_view, today, Button, ButtonKind, Card, ConfirmModal,
    EmptyState, FormError, Icon, IconName, Modal, PageHeader, TextArea, TextField, YearSwitcher,
};

use logic::{
    build_settings, form_of, holiday_request, parse_holiday_lines, FieldErrors, TierRow,
    COMMON_TIMEZONES, HOLIDAY_YEARS, MAX_IMPORT, WEEKDAY_LABELS,
};

#[component]
pub fn AdminPolicy() -> Element {
    let mut settings =
        use_resource(|| async { api::get::<CompanySettingsDto>("/admin/settings").await });
    rsx! {
        PageHeader { title: "근무 정책", description: "회사 기본 정보, 근무 시간, 공휴일을 관리해요." }
        Card { title: "회사와 근무 시간",
            {resource_view(settings, move |dto: CompanySettingsDto| rsx! {
                PolicyEditor { key: "{dto.version}", dto, onreload: move |_| settings.restart() }
            })}
        }
        HolidaysCard {}
    }
}

#[component]
fn PolicyEditor(dto: CompanySettingsDto, onreload: EventHandler<()>) -> Element {
    let mut session = use_session();
    let mut form = use_signal(|| form_of(&dto.settings));
    let mut errors = use_signal(FieldErrors::new);
    let mut top_error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let version = dto.version;

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let input = match build_settings(&form.read()) {
            Ok(input) => input,
            Err(found) => {
                errors.set(found);
                top_error.set("표시된 항목을 확인해 주세요.".to_owned());
                return;
            }
        };
        errors.set(FieldErrors::new());
        top_error.set(String::new());
        busy.set(true);
        let request = Versioned {
            version,
            data: input,
        };
        let result = api::put::<_, CompanySettingsDto>("/admin/settings", &request).await;
        // 저장에 성공했거나 버전이 어긋났으면 다시 읽는다. 다시 읽으면 이 편집기는 새 버전으로
        // 바뀌므로(`key`) 그때까지 버튼을 막아 둔다(다시 읽는 사이 두 번째 저장 방지).
        match result {
            Ok(_) => {
                report_success("근무 정책을 저장했어요.");
                // 근무 요일·시간은 다른 화면도 쓴다(휴가 달력 등).
                refresh_company();
                // 회사 이름·시간대는 내 정보(`/me`)에도 실려 있다.
                session.refresh().await;
                onreload.call(());
            }
            Err(failure) if failure.code == "version_conflict" => {
                report_error(&failure);
                onreload.call(());
            }
            Err(failure) => {
                busy.set(false);
                let message = failure.message();
                match failure.field {
                    Some(field) => {
                        errors.write().insert(field, message);
                        top_error.set("표시된 항목을 확인해 주세요.".to_owned());
                    }
                    None => top_error.set(message),
                }
            }
        }
    };

    let current = form.read().clone();
    let error_of = move |field: &str| errors.read().get(field).cloned().unwrap_or_default();
    let tier_count = current.break_tiers.len();
    let timezone_error = error_of("timezone");
    let tiers_error = error_of("break_tiers");
    let workdays_error = error_of("workdays");

    rsx! {
        form { class: "form", onsubmit: submit,
            FormError { message: top_error() }
            div { class: "form-row",
                TextField {
                    label: "회사 이름",
                    value: current.company_name.clone(),
                    required: true,
                    maxlength: CompanyName::RULE.max_chars as i64,
                    error: error_of("company_name"),
                    oninput: move |value| form.write().company_name = value,
                }
                label { class: "field",
                    span { class: "field-label", "시간대" }
                    input {
                        class: if timezone_error.is_empty() { "input" } else { "input input-error" },
                        r#type: "text",
                        list: "timezone-options",
                        value: "{current.timezone}",
                        maxlength: 64,
                        autocomplete: "off",
                        spellcheck: "false",
                        "aria-invalid": !timezone_error.is_empty(),
                        oninput: move |event| form.write().timezone = event.value(),
                    }
                    datalist { id: "timezone-options",
                        for zone in COMMON_TIMEZONES {
                            option { key: "{zone}", value: "{zone}" }
                        }
                    }
                    if timezone_error.is_empty() {
                        span { class: "field-hint", "IANA 이름. 예: Asia/Seoul" }
                    } else {
                        span { class: "field-error", role: "alert", "{timezone_error}" }
                    }
                }
            }
            div { class: "form-row",
                TextField {
                    label: "업무 시작 시각",
                    kind: "time",
                    value: current.work_start.clone(),
                    required: true,
                    error: error_of("work_start"),
                    oninput: move |value| form.write().work_start = value,
                }
                TextField {
                    label: "업무 종료 시각",
                    kind: "time",
                    value: current.work_end.clone(),
                    required: true,
                    error: error_of("work_end"),
                    oninput: move |value| form.write().work_end = value,
                }
            }
            div { class: "form-row",
                TextField {
                    label: "지각 유예 시간(분)",
                    kind: "number",
                    value: current.late_grace_minutes.clone(),
                    required: true,
                    hint: "시작 시각에서 이만큼 늦어도 지각이 아니에요.",
                    error: error_of("late_grace_minutes"),
                    oninput: move |value| form.write().late_grace_minutes = value,
                }
                TextField {
                    label: "근무일 전환 시각",
                    kind: "time",
                    value: current.day_cutoff.clone(),
                    required: true,
                    hint: "이 시각 전의 기록은 전날 근무로 봐요.",
                    error: error_of("day_cutoff"),
                    oninput: move |value| form.write().day_cutoff = value,
                }
            }
            div { class: "form-row",
                TextField {
                    label: "반차 기준 시각",
                    kind: "time",
                    value: current.half_day_split.clone(),
                    required: true,
                    hint: "오전 반차와 오후 반차를 나누는 시각이에요.",
                    error: error_of("half_day_split"),
                    oninput: move |value| form.write().half_day_split = value,
                }
                TextField {
                    label: "반반차 길이(분)",
                    kind: "number",
                    value: current.quarter_minutes.clone(),
                    required: true,
                    error: error_of("quarter_minutes"),
                    oninput: move |value| form.write().quarter_minutes = value,
                }
            }
            fieldset { class: "fieldset",
                legend { class: "field-label", "휴게 시간" }
                p { class: "field-hint", "근무 구간이 기준 이상이면 휴게 시간을 빼요. 가장 높은 단계 하나만 적용해요." }
                for (index , tier) in current.break_tiers.iter().enumerate() {
                    div { key: "{index}", class: "tier-row",
                        input {
                            class: "input",
                            r#type: "number",
                            inputmode: "numeric",
                            min: "0",
                            value: "{tier.min_span}",
                            "aria-label": "{index + 1}단계 기준 근무 시간(분)",
                            oninput: move |event| {
                                if let Some(row) = form.write().break_tiers.get_mut(index) {
                                    row.min_span = event.value();
                                }
                            },
                        }
                        span { class: "tier-text", "분 이상이면" }
                        input {
                            class: "input",
                            r#type: "number",
                            inputmode: "numeric",
                            min: "0",
                            value: "{tier.break_minutes}",
                            "aria-label": "{index + 1}단계 휴게 시간(분)",
                            oninput: move |event| {
                                if let Some(row) = form.write().break_tiers.get_mut(index) {
                                    row.break_minutes = event.value();
                                }
                            },
                        }
                        span { class: "tier-text", "분 휴게" }
                        button {
                            class: "icon-btn",
                            r#type: "button",
                            "aria-label": "{index + 1}단계 삭제",
                            onclick: move |_| {
                                let mut form = form.write();
                                if index < form.break_tiers.len() {
                                    form.break_tiers.remove(index);
                                }
                            },
                            Icon { name: IconName::Close }
                        }
                    }
                }
                if tier_count < MAX_BREAK_TIERS {
                    div {
                        Button {
                            kind: ButtonKind::Grey,
                            small: true,
                            onclick: move |_| form.write().break_tiers.push(TierRow::default()),
                            "단계 추가"
                        }
                    }
                }
                if !tiers_error.is_empty() {
                    span { class: "field-error", role: "alert", "{tiers_error}" }
                }
            }
            fieldset { class: "fieldset",
                legend { class: "field-label", "근무 요일" }
                div { class: "day-toggles",
                    for (index , label) in WEEKDAY_LABELS.iter().enumerate() {
                        button {
                            key: "{label}",
                            class: if current.workdays[index] { "day-toggle day-toggle-on" } else { "day-toggle" },
                            r#type: "button",
                            "aria-pressed": current.workdays[index],
                            onclick: move |_| {
                                if let Some(day) = form.write().workdays.get_mut(index) {
                                    *day = !*day;
                                }
                            },
                            if current.workdays[index] {
                                Icon { name: IconName::Check }
                            }
                            "{label}"
                        }
                    }
                }
                if !workdays_error.is_empty() {
                    span { class: "field-error", role: "alert", "{workdays_error}" }
                }
            }
            div { class: "panel-actions",
                Button { submit: true, busy: busy(), "저장" }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum HolidayDialog {
    Closed,
    Rename(HolidayDto),
    Delete(HolidayDto),
    Import,
}

#[component]
fn HolidaysCard() -> Element {
    let session = use_session();
    let tz = session.zone();
    let mut year = use_signal(move || today(tz).year());
    let mut holidays = use_resource(move || async move {
        api::get::<Vec<HolidayDto>>(&format!("/holidays?year={}", year())).await
    });
    let mut dialog = use_signal(|| HolidayDialog::Closed);
    let mut new_date = use_signal(String::new);
    let mut new_name = use_signal(String::new);
    let mut add_error = use_signal(String::new);
    let mut adding = use_signal(|| false);
    let mut deleting = use_signal(|| false);

    let add = move |event: FormEvent| async move {
        event.prevent_default();
        if adding() {
            return;
        }
        let (date, input) = match holiday_request(&new_date(), &new_name()) {
            Ok(request) => request,
            Err(message) => {
                add_error.set(message);
                return;
            }
        };
        add_error.set(String::new());
        adding.set(true);
        match api::put::<_, HolidayDto>(&format!("/admin/holidays/{date}"), &input).await {
            Ok(_) => {
                report_success("공휴일을 저장했어요.");
                new_date.set(String::new());
                new_name.set(String::new());
                if date.year() == year() {
                    holidays.restart();
                } else {
                    year.set(date.year());
                }
            }
            Err(error) => report_error(&error),
        }
        adding.set(false);
    };
    let remove = move |holiday: HolidayDto| async move {
        if deleting() {
            return;
        }
        deleting.set(true);
        match api::delete::<Empty>(&format!("/admin/holidays/{}", holiday.date)).await {
            Ok(_) => {
                report_success("공휴일을 삭제했어요.");
                dialog.set(HolidayDialog::Closed);
                holidays.restart();
            }
            Err(error) => report_error(&error),
        }
        deleting.set(false);
    };
    let saved = move |_| {
        dialog.set(HolidayDialog::Closed);
        holidays.restart();
    };
    let current_year = year();

    rsx! {
        Card {
            title: "공휴일",
            action: rsx! {
                YearSwitcher {
                    year: current_year,
                    min: *HOLIDAY_YEARS.start(),
                    max: *HOLIDAY_YEARS.end(),
                    onchange: move |next| year.set(next),
                }
            },
            form { class: "toolbar", onsubmit: add,
                TextField {
                    label: "날짜",
                    kind: "date",
                    value: new_date(),
                    required: true,
                    oninput: move |value| new_date.set(value),
                }
                div { class: "toolbar-wide",
                    TextField {
                        label: "이름",
                        value: new_name(),
                        required: true,
                        placeholder: "예: 창립기념일",
                        maxlength: LabelName::RULE.max_chars as i64,
                        oninput: move |value| new_name.set(value),
                    }
                }
                div { class: "toolbar-end",
                    Button {
                        kind: ButtonKind::Grey,
                        onclick: move |_| dialog.set(HolidayDialog::Import),
                        "일괄 등록"
                    }
                    Button { kind: ButtonKind::Weak, submit: true, busy: adding(), "추가" }
                }
            }
            FormError { message: add_error() }
            p { class: "field-hint panel-note", "이미 등록된 날짜를 다시 추가하면 이름이 바뀌어요." }
            {resource_view(holidays, move |list: Vec<HolidayDto>| rsx! {
                if list.is_empty() {
                    EmptyState { title: "{current_year}년에 등록된 공휴일이 없어요.", icon: IconName::Calendar }
                } else {
                    ul { class: "list",
                        for holiday in list {
                            li { key: "{holiday.date}", class: "list-row",
                                span { class: "list-row-main",
                                    strong { "{holiday.name}" }
                                    small { {format::date_md(holiday.date)} }
                                }
                                span { class: "row-actions",
                                    Button {
                                        kind: ButtonKind::Grey,
                                        small: true,
                                        onclick: {
                                            let holiday = holiday.clone();
                                            move |_| dialog.set(HolidayDialog::Rename(holiday.clone()))
                                        },
                                        "이름 변경"
                                    }
                                    Button {
                                        kind: ButtonKind::Grey,
                                        small: true,
                                        onclick: {
                                            let holiday = holiday.clone();
                                            move |_| dialog.set(HolidayDialog::Delete(holiday.clone()))
                                        },
                                        "삭제"
                                    }
                                }
                            }
                        }
                    }
                }
            })}
        }
        match dialog() {
            HolidayDialog::Closed => rsx! {},
            HolidayDialog::Rename(holiday) => rsx! {
                HolidayRename {
                    holiday,
                    onclose: move |_| dialog.set(HolidayDialog::Closed),
                    onsaved: saved,
                }
            },
            HolidayDialog::Delete(holiday) => rsx! {
                ConfirmModal {
                    title: "공휴일 삭제",
                    message: format!(
                        "{} ‘{}’을(를) 삭제해요. 근태·휴가 계산에서 근무일로 바뀌어요.",
                        format::date_ymd(holiday.date),
                        holiday.name
                    ),
                    confirm_label: "삭제",
                    danger: true,
                    busy: deleting(),
                    onconfirm: move |_| remove(holiday.clone()),
                    oncancel: move |_| dialog.set(HolidayDialog::Closed),
                }
            },
            HolidayDialog::Import => rsx! {
                HolidayImport {
                    onclose: move |_| dialog.set(HolidayDialog::Closed),
                    onsaved: saved,
                }
            },
        }
    }
}

#[component]
fn HolidayRename(
    holiday: HolidayDto,
    onclose: EventHandler<()>,
    onsaved: EventHandler<()>,
) -> Element {
    let mut name = use_signal(|| holiday.name.clone());
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let date = holiday.date;

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let input = match holiday_request(&date.to_string(), &name()) {
            Ok((_, input)) => input,
            Err(message) => {
                error.set(message);
                return;
            }
        };
        busy.set(true);
        match api::put::<_, HolidayDto>(&format!("/admin/holidays/{date}"), &input).await {
            Ok(_) => {
                report_success("이름을 바꿨어요.");
                onsaved.call(());
            }
            Err(failure) => report_error(&failure),
        }
        busy.set(false);
    };

    rsx! {
        Modal { title: "공휴일 이름 변경", onclose: move |_| onclose.call(()),
            form { class: "form", onsubmit: submit,
                p { class: "field-hint", {format::date_ymd(date)} }
                FormError { message: error() }
                TextField {
                    label: "이름",
                    value: name(),
                    required: true,
                    maxlength: LabelName::RULE.max_chars as i64,
                    oninput: move |value| name.set(value),
                }
                div { class: "modal-actions",
                    Button { kind: ButtonKind::Grey, onclick: move |_| onclose.call(()), "취소" }
                    Button { submit: true, busy: busy(), "저장" }
                }
            }
        }
    }
}

#[component]
fn HolidayImport(onclose: EventHandler<()>, onsaved: EventHandler<()>) -> Element {
    let mut text = use_signal(String::new);
    let mut errors = use_signal(Vec::<String>::new);
    let mut busy = use_signal(|| false);

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let items = match parse_holiday_lines(&text()) {
            Ok(items) => items,
            Err(found) => {
                errors.set(found);
                return;
            }
        };
        errors.set(Vec::new());
        busy.set(true);
        let request = HolidayImportRequest { items };
        match api::post::<_, HolidayImportResult>("/admin/holidays/import", &request).await {
            Ok(result) => {
                report_success(format!("공휴일 {}개를 등록했어요.", result.imported));
                onsaved.call(());
            }
            Err(failure) => report_error(&failure),
        }
        busy.set(false);
    };

    rsx! {
        Modal { title: "공휴일 일괄 등록", onclose: move |_| onclose.call(()),
            form { class: "form", onsubmit: submit,
                p { class: "field-hint",
                    "한 줄에 하나씩 "
                    code { "YYYY-MM-DD 이름" }
                    " 형식으로 입력해요. 최대 {MAX_IMPORT}개, 이미 있는 날짜는 이름이 바뀌어요."
                }
                if !errors.read().is_empty() {
                    ul { class: "form-error import-errors", role: "alert",
                        for (index , message) in errors.read().iter().enumerate() {
                            li { key: "{index}", "{message}" }
                        }
                    }
                }
                TextArea {
                    label: "공휴일 목록",
                    value: text(),
                    rows: 10,
                    placeholder: "2026-01-01 신정\n2026-03-01 삼일절",
                    maxlength: 10_000,
                    oninput: move |value| text.set(value),
                }
                div { class: "modal-actions",
                    Button { kind: ButtonKind::Grey, onclick: move |_| onclose.call(()), "취소" }
                    Button { submit: true, busy: busy(), "등록" }
                }
            }
        }
    }
}

pub mod logic {
    use std::collections::{BTreeMap, HashMap};

    use chrono::{NaiveDate, Weekday};
    use pango_api::admin::{BreakTierDto, CompanySettingsInput, HolidayImportItem, HolidayInput};
    use pango_core::attendance::{BreakTier, WorkPolicy};
    use pango_core::calendar::{parse_date, WorkWeek};
    use pango_core::text::{CompanyName, LabelName};
    use pango_core::time::{parse_timezone, MinuteOfDay, WorkCalendarZone};
    use pango_core::{DomainError, DomainResult};

    use crate::text;

    /// 서버의 일괄 등록 한도(`MAX_HOLIDAY_IMPORT`)와 같다.
    pub const MAX_IMPORT: usize = 100;
    /// 오류 목록이 화면을 덮지 않게 이만큼만 보여 준다.
    const MAX_REPORTED_ERRORS: usize = 10;
    /// 서버가 받는 공휴일 연도 범위.
    pub const HOLIDAY_YEARS: std::ops::RangeInclusive<i32> = 1000..=9999;

    pub const WEEKDAYS: [Weekday; 7] = [
        Weekday::Mon,
        Weekday::Tue,
        Weekday::Wed,
        Weekday::Thu,
        Weekday::Fri,
        Weekday::Sat,
        Weekday::Sun,
    ];
    pub const WEEKDAY_LABELS: [&str; 7] = ["월", "화", "수", "목", "금", "토", "일"];

    pub const COMMON_TIMEZONES: &[&str] = &[
        "Asia/Seoul",
        "Asia/Tokyo",
        "Asia/Shanghai",
        "Asia/Taipei",
        "Asia/Singapore",
        "Asia/Ho_Chi_Minh",
        "Asia/Kolkata",
        "Europe/London",
        "Europe/Berlin",
        "America/New_York",
        "America/Los_Angeles",
        "Australia/Sydney",
        "UTC",
    ];

    /// 필드 이름 → 오류 문구.
    pub type FieldErrors = BTreeMap<String, String>;

    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct TierRow {
        pub min_span: String,
        pub break_minutes: String,
    }

    /// 입력 중인 근무 정책. 입력 상자 값 그대로(문자열) 둔다.
    #[derive(Debug, Clone, PartialEq)]
    pub struct PolicyForm {
        pub company_name: String,
        pub timezone: String,
        pub day_cutoff: String,
        pub work_start: String,
        pub work_end: String,
        pub half_day_split: String,
        pub quarter_minutes: String,
        pub late_grace_minutes: String,
        pub break_tiers: Vec<TierRow>,
        /// 월요일부터 일요일까지.
        pub workdays: [bool; 7],
    }

    pub fn form_of(settings: &CompanySettingsInput) -> PolicyForm {
        let week = WorkWeek::from_bits(settings.workdays).ok();
        PolicyForm {
            company_name: settings.company_name.clone(),
            timezone: settings.timezone.clone(),
            day_cutoff: settings.day_cutoff.to_string(),
            work_start: settings.work_start.to_string(),
            work_end: settings.work_end.to_string(),
            half_day_split: settings.half_day_split.to_string(),
            quarter_minutes: settings.quarter_minutes.to_string(),
            late_grace_minutes: settings.late_grace_minutes.to_string(),
            break_tiers: settings
                .break_tiers
                .iter()
                .map(|tier| TierRow {
                    min_span: tier.min_span_minutes.to_string(),
                    break_minutes: tier.break_minutes.to_string(),
                })
                .collect(),
            workdays: WEEKDAYS.map(|day| week.is_some_and(|w| w.contains(day))),
        }
    }

    fn message(field: &str, reason: &str) -> String {
        text::error_message("validation_failed", Some(field), Some(reason))
    }

    /// 실패하면 `field`에 오류를 적는다. 도메인 오류의 필드 이름(`time_of_day` 등)이 화면 필드와
    /// 다를 수 있어, 어느 입력 상자의 문제인지는 부르는 쪽이 정한다.
    fn check<T>(errors: &mut FieldErrors, field: &str, result: DomainResult<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                let reason = match &error {
                    DomainError::Validation { reason, .. } => reason.code(),
                    _ => "invalid_format",
                };
                errors.insert(field.to_owned(), message(field, reason));
                None
            }
        }
    }

    /// 여러 값을 함께 보는 규칙의 오류는 도메인이 가리킨 필드에 적는다.
    fn record(errors: &mut FieldErrors, error: &DomainError) {
        match error {
            DomainError::Validation { field, reason } => {
                errors.insert((*field).to_owned(), message(field, reason.code()));
            }
            other => {
                errors.insert(String::new(), text::error_message(other.code(), None, None));
            }
        }
    }

    fn minutes(errors: &mut FieldErrors, field: &str, raw: &str) -> Option<u16> {
        let raw = raw.trim();
        match raw.parse::<u16>() {
            Ok(value) => Some(value),
            Err(_) => {
                let reason = if raw.is_empty() {
                    "empty"
                } else {
                    "invalid_format"
                };
                errors.insert(field.to_owned(), message(field, reason));
                None
            }
        }
    }

    fn time(errors: &mut FieldErrors, field: &str, raw: &str) -> Option<MinuteOfDay> {
        check(errors, field, MinuteOfDay::parse(raw.trim()))
    }

    /// 폼을 저장 요청으로 바꾼다. 서버가 쓰는 도메인 생성자로 같은 검증을 먼저 한다
    /// (통과하면 서버도 받는다). 실패하면 필드별 오류를 돌려준다.
    pub fn build_settings(form: &PolicyForm) -> Result<CompanySettingsInput, FieldErrors> {
        let mut errors = FieldErrors::new();
        let company_name = check(
            &mut errors,
            "company_name",
            CompanyName::parse(&form.company_name),
        );
        let timezone = form.timezone.trim();
        let tz = check(&mut errors, "timezone", parse_timezone(timezone));
        let day_cutoff = time(&mut errors, "day_cutoff", &form.day_cutoff);
        let work_start = time(&mut errors, "work_start", &form.work_start);
        let work_end = time(&mut errors, "work_end", &form.work_end);
        let half_day_split = time(&mut errors, "half_day_split", &form.half_day_split);
        let quarter = minutes(&mut errors, "quarter_minutes", &form.quarter_minutes);
        let grace = minutes(&mut errors, "late_grace_minutes", &form.late_grace_minutes);
        let mut tiers = Vec::with_capacity(form.break_tiers.len());
        for row in &form.break_tiers {
            let span = minutes(&mut errors, "break_tiers", &row.min_span);
            let rest = minutes(&mut errors, "break_tiers", &row.break_minutes);
            if let (Some(min_span_minutes), Some(break_minutes)) = (span, rest) {
                tiers.push(BreakTier {
                    min_span_minutes,
                    break_minutes,
                });
            }
        }
        let selected = WEEKDAYS
            .iter()
            .zip(form.workdays)
            .filter(|(_, on)| *on)
            .map(|(day, _)| *day);
        let week = check(&mut errors, "workdays", WorkWeek::from_weekdays(selected));

        let (
            Some(company_name),
            Some(tz),
            Some(day_cutoff),
            Some(work_start),
            Some(work_end),
            Some(half_day_split),
            Some(quarter_minutes),
            Some(late_grace_minutes),
            Some(week),
        ) = (
            company_name,
            tz,
            day_cutoff,
            work_start,
            work_end,
            half_day_split,
            quarter,
            grace,
            week,
        )
        else {
            return Err(errors);
        };
        if !errors.is_empty() {
            return Err(errors);
        }
        if let Err(error) = WorkCalendarZone::new(tz, day_cutoff) {
            record(&mut errors, &error);
        }
        if let Err(error) = WorkPolicy::new(
            work_start,
            work_end,
            late_grace_minutes,
            half_day_split,
            quarter_minutes,
            tiers.clone(),
        ) {
            record(&mut errors, &error);
        }
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(CompanySettingsInput {
            company_name: company_name.into_string(),
            timezone: timezone.to_owned(),
            day_cutoff,
            work_start,
            work_end,
            late_grace_minutes,
            half_day_split,
            quarter_minutes,
            break_tiers: tiers
                .into_iter()
                .map(|tier| BreakTierDto {
                    min_span_minutes: tier.min_span_minutes,
                    break_minutes: tier.break_minutes,
                })
                .collect(),
            workdays: week.bits(),
        })
    }

    /// 공휴일 한 건 추가·이름 변경 요청. 날짜는 경로에 들어가므로 엄격히 검사한다.
    pub fn holiday_request(date: &str, name: &str) -> Result<(NaiveDate, HolidayInput), String> {
        let date = date.trim();
        if date.is_empty() {
            return Err("날짜를 선택해 주세요.".to_owned());
        }
        let date = parse_date("date", date)
            .map_err(|_| "날짜는 YYYY-MM-DD 형식으로 입력해 주세요.".to_owned())?;
        let name = LabelName::parse(name).map_err(|error| match error {
            DomainError::Validation { reason, .. } => message("name", reason.code()),
            other => text::error_message(other.code(), None, None),
        })?;
        Ok((
            date,
            HolidayInput {
                name: name.into_string(),
            },
        ))
    }

    /// 일괄 등록 글을 항목으로 바꾼다. 한 줄에 `YYYY-MM-DD 이름`(사이는 공백이나 탭), 빈 줄은 건너뛴다.
    ///
    /// 서버는 하나라도 틀리면 전부 거부하므로, 보내기 전에 모든 줄을 검사해 줄 번호와 함께 알린다.
    pub fn parse_holiday_lines(input: &str) -> Result<Vec<HolidayImportItem>, Vec<String>> {
        let lines: Vec<(usize, &str)> = input
            .lines()
            .enumerate()
            .map(|(index, line)| (index + 1, line.trim()))
            .filter(|(_, line)| !line.is_empty())
            .collect();
        if lines.is_empty() {
            return Err(vec![
                "등록할 공휴일을 한 줄에 하나씩 입력해 주세요.".to_owned()
            ]);
        }
        if lines.len() > MAX_IMPORT {
            return Err(vec![format!(
                "한 번에 {MAX_IMPORT}개까지 등록할 수 있어요. (지금 {}개)",
                lines.len()
            )]);
        }
        let mut errors = Vec::new();
        let mut items = Vec::with_capacity(lines.len());
        let mut seen: HashMap<NaiveDate, usize> = HashMap::new();
        for (number, line) in lines {
            let Some((date, name)) = line.split_once([' ', '\t']) else {
                errors.push(format!("{number}번째 줄: 날짜와 이름을 띄어 써 주세요."));
                continue;
            };
            let Ok(date) = parse_date("date", date) else {
                errors.push(format!(
                    "{number}번째 줄: 날짜는 YYYY-MM-DD 형식으로 입력해 주세요."
                ));
                continue;
            };
            let name = match LabelName::parse(name) {
                Ok(name) => name.into_string(),
                Err(error) => {
                    let reason = match &error {
                        DomainError::Validation { reason, .. } => reason.code(),
                        _ => "invalid_format",
                    };
                    errors.push(format!("{number}번째 줄: {}", message("name", reason)));
                    continue;
                }
            };
            if let Some(first) = seen.insert(date, number) {
                errors.push(format!("{number}번째 줄: {first}번째 줄과 날짜가 같아요."));
                continue;
            }
            items.push(HolidayImportItem {
                date: date.to_string(),
                name,
            });
        }
        if errors.is_empty() {
            return Ok(items);
        }
        let hidden = errors.len().saturating_sub(MAX_REPORTED_ERRORS);
        errors.truncate(MAX_REPORTED_ERRORS);
        if hidden > 0 {
            errors.push(format!("그 밖에 {hidden}개 줄에 오류가 있어요."));
        }
        Err(errors)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn settings() -> CompanySettingsInput {
            let hm = |raw: &str| MinuteOfDay::parse(raw).expect("time");
            CompanySettingsInput {
                company_name: "판고".into(),
                timezone: "Asia/Seoul".into(),
                day_cutoff: hm("05:00"),
                work_start: hm("09:00"),
                work_end: hm("18:00"),
                late_grace_minutes: 0,
                half_day_split: hm("14:00"),
                quarter_minutes: 120,
                break_tiers: vec![
                    BreakTierDto {
                        min_span_minutes: 240,
                        break_minutes: 30,
                    },
                    BreakTierDto {
                        min_span_minutes: 480,
                        break_minutes: 60,
                    },
                ],
                workdays: 0b001_1111,
            }
        }

        fn errors_of(form: &PolicyForm) -> FieldErrors {
            build_settings(form).expect_err("invalid")
        }

        #[test]
        fn form_round_trips_settings() {
            let form = form_of(&settings());
            assert_eq!(form.work_start, "09:00");
            assert_eq!(form.workdays, [true, true, true, true, true, false, false]);
            assert_eq!(build_settings(&form), Ok(settings()));
        }

        #[test]
        fn field_errors_point_at_the_input() {
            let mut form = form_of(&settings());
            form.company_name = "  ".into();
            form.timezone = "Mars/Olympus".into();
            form.work_start = "9:00".into();
            form.quarter_minutes = "-5".into();
            form.late_grace_minutes = String::new();
            form.workdays = [false; 7];
            let errors = errors_of(&form);
            let fields: Vec<&str> = errors.keys().map(String::as_str).collect();
            assert_eq!(
                fields,
                [
                    "company_name",
                    "late_grace_minutes",
                    "quarter_minutes",
                    "timezone",
                    "work_start",
                    "workdays"
                ]
            );
            assert_eq!(errors["workdays"], "근무 요일을 하루 이상 선택해 주세요.");
            assert_eq!(
                errors["late_grace_minutes"],
                "지각 유예 시간을(를) 입력해 주세요."
            );
        }

        #[test]
        fn combined_rules_use_domain_constructors() {
            let mut form = form_of(&settings());
            form.work_end = "08:00".into();
            assert_eq!(
                errors_of(&form)["work_end"],
                "업무 종료 시각은 시작 시각보다 늦어야 합니다."
            );

            let mut form = form_of(&settings());
            form.day_cutoff = "12:01".into();
            assert!(errors_of(&form).contains_key("day_cutoff"));
            form.day_cutoff = "12:00".into();
            assert!(build_settings(&form).is_ok(), "정오 정각은 허용");

            let mut form = form_of(&settings());
            form.half_day_split = "09:00".into();
            assert!(errors_of(&form).contains_key("half_day_split"));

            let mut form = form_of(&settings());
            form.late_grace_minutes = "121".into();
            assert!(errors_of(&form).contains_key("late_grace_minutes"));
            form.late_grace_minutes = "120".into();
            assert!(build_settings(&form).is_ok());

            let mut form = form_of(&settings());
            form.break_tiers.swap(0, 1);
            assert!(
                errors_of(&form).contains_key("break_tiers"),
                "오름차순이 아님"
            );

            let mut form = form_of(&settings());
            form.break_tiers = vec![TierRow::default(); MAX_BREAK_TIERS_FOR_TEST + 1]
                .into_iter()
                .enumerate()
                .map(|(i, _)| TierRow {
                    min_span: (60 * (i + 1)).to_string(),
                    break_minutes: "10".into(),
                })
                .collect();
            assert!(
                errors_of(&form).contains_key("break_tiers"),
                "단계가 너무 많음"
            );

            let mut form = form_of(&settings());
            form.break_tiers = Vec::new();
            assert!(build_settings(&form).is_ok(), "휴게 없음도 정책이다");
        }

        const MAX_BREAK_TIERS_FOR_TEST: usize = pango_core::attendance::MAX_BREAK_TIERS;

        #[test]
        fn hostile_numbers_and_text_are_rejected() {
            for raw in ["abc", "1e3", "65536", "0x10", "١٢٠", "120분"] {
                let mut form = form_of(&settings());
                form.quarter_minutes = raw.into();
                assert!(errors_of(&form).contains_key("quarter_minutes"), "{raw}");
            }
            let mut form = form_of(&settings());
            form.company_name = "회사\u{202e}".into();
            assert!(errors_of(&form).contains_key("company_name"));
            form = form_of(&settings());
            form.timezone = " Asia/Seoul ".into();
            assert_eq!(
                build_settings(&form).expect("trimmed").timezone,
                "Asia/Seoul"
            );
        }

        #[test]
        fn every_common_timezone_is_valid() {
            for zone in COMMON_TIMEZONES {
                assert!(parse_timezone(zone).is_ok(), "{zone}");
            }
        }

        #[test]
        fn single_holiday_request_is_strict() {
            let (date, input) = holiday_request("2026-05-05", " 어린이날 ").expect("valid");
            assert_eq!(date.to_string(), "2026-05-05");
            assert_eq!(input.name, "어린이날");
            assert!(holiday_request("", "x").is_err());
            assert!(holiday_request("2026-5-5", "x").is_err());
            assert!(holiday_request("2026-02-30", "x").is_err());
            assert!(holiday_request("../../admin", "x").is_err());
            assert!(holiday_request("2026-05-05", "  ").is_err());
            assert!(holiday_request("2026-05-05", &"가".repeat(51)).is_err());
        }

        #[test]
        fn import_accepts_spaces_tabs_and_blank_lines() {
            let text =
                "\n2026-01-01 신정\r\n\n  2026-03-01\t삼일절  \n2026-05-05   어린이 날\n\t\n";
            let items = parse_holiday_lines(text).expect("valid");
            assert_eq!(
                items,
                vec![
                    HolidayImportItem {
                        date: "2026-01-01".into(),
                        name: "신정".into()
                    },
                    HolidayImportItem {
                        date: "2026-03-01".into(),
                        name: "삼일절".into()
                    },
                    HolidayImportItem {
                        date: "2026-05-05".into(),
                        name: "어린이 날".into()
                    },
                ]
            );
        }

        #[test]
        fn import_reports_line_numbers() {
            let text = "2026-01-01 신정\n2026-1-2 잘못\n\n2026-02-30 없는날\n2026-03-01\n2026-01-01 중복\n2026-04-01 \u{0007}벨";
            let errors = parse_holiday_lines(text).expect_err("invalid");
            assert_eq!(
                errors,
                vec![
                    "2번째 줄: 날짜는 YYYY-MM-DD 형식으로 입력해 주세요.",
                    "4번째 줄: 날짜는 YYYY-MM-DD 형식으로 입력해 주세요.",
                    "5번째 줄: 날짜와 이름을 띄어 써 주세요.",
                    "6번째 줄: 1번째 줄과 날짜가 같아요.",
                    "7번째 줄: 이름에 쓸 수 없는 문자가 들어 있습니다.",
                ]
            );
        }

        #[test]
        fn import_limits_count_and_reported_errors() {
            assert!(parse_holiday_lines("").is_err());
            assert!(parse_holiday_lines(" \n\t\n").is_err());

            let line = |i: u32| {
                let date = NaiveDate::from_ymd_opt(2026, 1, 1).expect("date")
                    + chrono::Days::new(u64::from(i));
                format!("{date} 휴일{i}")
            };
            let hundred: Vec<String> = (0..100).map(line).collect();
            assert_eq!(
                parse_holiday_lines(&hundred.join("\n"))
                    .expect("100 ok")
                    .len(),
                100
            );
            let many: Vec<String> = (0..101).map(line).collect();
            assert_eq!(
                parse_holiday_lines(&many.join("\n")),
                Err(vec![
                    "한 번에 100개까지 등록할 수 있어요. (지금 101개)".to_owned()
                ])
            );

            let garbage = vec!["nope"; 30].join("\n");
            let errors = parse_holiday_lines(&garbage).expect_err("invalid");
            assert_eq!(errors.len(), MAX_REPORTED_ERRORS + 1);
            assert_eq!(
                errors[MAX_REPORTED_ERRORS],
                "그 밖에 20개 줄에 오류가 있어요."
            );

            let script = "2026-01-01 <script>alert(1)</script>";
            assert_eq!(
                parse_holiday_lines(script)
                    .expect("text")
                    .first()
                    .map(|i| i.name.as_str()),
                Some("<script>alert(1)</script>")
            );
        }
    }
}
