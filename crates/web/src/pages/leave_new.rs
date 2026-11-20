//! 휴가 신청 폼.

use dioxus::prelude::*;
use pango_api::approval::LineSuggestion;
use pango_api::leave::{BalanceDto, LeaveRequestDto, LeaveTypeDto};
use pango_core::leave::LeaveUnit;

use crate::api;
use crate::route::Route;
use crate::session::use_session;
use crate::text;
use crate::ui::approval_line::from_suggestion;
use crate::ui::{
    report_success, today, ApprovalLineEditor, ApproverEntry, Button, ButtonKind, Card, DateField,
    ErrorState, FormError, PageHeader, SelectField, Spinner, TextArea,
};

use logic::Slot;

#[component]
pub fn LeaveNew() -> Element {
    let mut session = use_session();
    let nav = navigator();
    let tz = session.zone();
    let today = today(tz);
    let me_id = session.me().map(|me| me.id);

    let key = use_signal(api::new_idempotency_key);
    let mut type_id = use_signal(String::new);
    let mut unit = use_signal(|| LeaveUnit::FullDay);
    let mut start = use_signal(|| today.to_string());
    let mut end = use_signal(|| today.to_string());
    let mut reason = use_signal(String::new);
    let mut line = use_signal(Vec::<ApproverEntry>::new);
    let mut line_touched = use_signal(|| false);
    let mut busy = use_signal(|| false);
    let mut server_error = use_signal(|| None::<(Slot, String)>);

    let mut types = use_resource(|| async { api::get::<Vec<LeaveTypeDto>>("/leave/types").await });
    let balance = use_resource(|| async { api::get::<BalanceDto>("/leave/balance").await });
    let suggestion = use_resource(move || async move {
        let result = api::get::<LineSuggestion>("/approval/line-suggestion").await;
        if let Ok(found) = &result {
            // 사용자가 이미 고쳤다면 추천으로 덮어쓰지 않는다.
            if !*line_touched.peek() {
                let exclude: Vec<_> = me_id.into_iter().collect();
                line.set(from_suggestion(&exclude, &found.steps));
            }
        }
        result
    });

    let type_list: Vec<LeaveTypeDto> = match &*types.read() {
        Some(Ok(list)) => logic::usable_types(list),
        _ => Vec::new(),
    };
    // 종류를 아직 고르지 않았으면 첫 번째(정렬 순서상 기본 종류)를 쓴다.
    let chosen = logic::chosen_type(&type_list, &type_id());
    let remaining = match &*balance.read() {
        Some(Ok(dto)) => Some(dto.balance.remaining.quarters()),
        _ => None,
    };
    let preview = logic::preview(&start(), &end(), unit(), chosen.as_ref(), remaining);
    let line_problem = crate::ui::approval_line::line_problem(&line());
    let reason_problem = logic::reason_problem(&reason());
    let can_submit =
        chosen.is_some() && preview.is_ok() && line_problem.is_none() && reason_problem.is_none();

    let error_for = move |slot: Slot| -> String {
        match server_error() {
            Some((s, message)) if s == slot => message,
            _ => String::new(),
        }
    };

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let chosen = logic::chosen_type(&type_list_snapshot(types), &type_id());
        let request = match logic::to_request(
            chosen.as_ref(),
            unit(),
            &start(),
            &end(),
            &reason(),
            &line(),
        ) {
            Ok(request) => request,
            Err(problem) => {
                server_error.set(Some((problem.slot(), problem.message().to_owned())));
                return;
            }
        };
        busy.set(true);
        server_error.set(None);
        match api::post_once::<_, LeaveRequestDto>("/leave/requests", &request, &key()).await {
            Ok(_) => {
                report_success("휴가를 신청했습니다. 결재가 끝나면 알려 드립니다.");
                nav.push(Route::Leave {});
                // 화면을 떠나므로 버튼을 다시 풀지 않는다(이동 전 두 번째 제출 방지).
                return;
            }
            Err(error) => {
                let message = session.handle(&error);
                server_error.set(Some((
                    logic::error_slot(&error.code, error.field.as_deref()),
                    message,
                )));
            }
        }
        busy.set(false);
    };

    let type_options: Vec<(String, String)> = type_list
        .iter()
        .map(|t| (t.id.to_string(), t.name.clone()))
        .collect();
    let allow_partial = chosen.as_ref().is_some_and(|t| t.allow_partial);
    let unit_options: Vec<(String, String)> = logic::units_for(allow_partial)
        .into_iter()
        .map(|u| (u.as_str().to_owned(), text::leave_unit_label(u).to_owned()))
        .collect();
    let partial = unit().is_partial();
    let min_date = logic::earliest_start(today).to_string();
    let date_problem = match &preview {
        Err(problem) if problem.slot() == Slot::Dates => problem.message().to_owned(),
        _ => String::new(),
    };
    let dates_error = {
        let from_server = error_for(Slot::Dates);
        if from_server.is_empty() {
            date_problem
        } else {
            from_server
        }
    };
    let types_state = match &*types.read() {
        None => logic::Load::Loading,
        Some(Err(error)) => logic::Load::Failed(session.handle(error)),
        Some(Ok(_)) => logic::Load::Ready,
    };

    rsx! {
        PageHeader { title: "휴가 신청" }
        if types_state == logic::Load::Loading {
            Spinner {}
        } else if let logic::Load::Failed(message) = types_state {
            ErrorState { message, onretry: move |_| types.restart() }
        } else if type_list.is_empty() {
            Card {
                FormError { message: "신청할 수 있는 휴가 종류가 없습니다. 관리자에게 문의해 주세요." }
            }
        } else {
            form { class: "form leave-form", onsubmit: submit,
                Card {
                    div { class: "form",
                        FormError { message: error_for(Slot::Form) }
                        div { class: "form-row",
                            div { class: "field",
                                SelectField {
                                    label: "휴가 종류",
                                    value: chosen.as_ref().map(|t| t.id.to_string()).unwrap_or_default(),
                                    options: type_options,
                                    required: true,
                                    onchange: move |value: String| {
                                        let list = type_list_snapshot(types);
                                        let allow = logic::chosen_type(&list, &value).is_some_and(|t| t.allow_partial);
                                        type_id.set(value);
                                        unit.set(logic::unit_for_type(unit(), allow));
                                        server_error.set(None);
                                    },
                                }
                                FieldError { message: error_for(Slot::Type) }
                            }
                            div { class: "field",
                                SelectField {
                                    label: "단위",
                                    value: unit().as_str(),
                                    options: unit_options,
                                    required: true,
                                    onchange: move |value: String| {
                                        let next = LeaveUnit::parse(&value).unwrap_or(LeaveUnit::FullDay);
                                        unit.set(next);
                                        end.set(logic::end_for(next, &start(), &end()));
                                        server_error.set(None);
                                    },
                                }
                                FieldError { message: error_for(Slot::Unit) }
                            }
                        }
                        div { class: "form-row",
                            DateField {
                                label: if partial { "날짜" } else { "시작일" },
                                value: start(),
                                min: min_date.clone(),
                                required: true,
                                oninput: move |value: String| {
                                    end.set(logic::end_after_start_change(unit(), &value, &end()));
                                    start.set(value);
                                    server_error.set(None);
                                },
                            }
                            DateField {
                                label: "종료일",
                                value: end(),
                                min: start(),
                                required: true,
                                disabled: partial,
                                hint: if partial { "반차·반반차는 하루만 신청할 수 있습니다." } else { "" },
                                error: dates_error,
                                oninput: move |value: String| {
                                    end.set(value);
                                    server_error.set(None);
                                },
                            }
                        }
                        div { class: "field",
                            TextArea {
                                label: "사유",
                                value: reason(),
                                rows: 3,
                                required: true,
                                maxlength: logic::REASON_MAX_CHARS,
                                placeholder: "결재자에게 보여 줄 사유를 적어 주세요.",
                                oninput: move |value: String| {
                                    reason.set(value);
                                    if server_error().is_some_and(|(slot, _)| slot == Slot::Reason) {
                                        server_error.set(None);
                                    }
                                },
                            }
                            FieldError { message: error_for(Slot::Reason) }
                        }
                        LeaveSummary { preview }
                    }
                }
                Card { title: "결재선",
                    if suggestion.read().is_none() {
                        Spinner {}
                    } else {
                        ApprovalLineEditor {
                            steps: line(),
                            exclude: me_id.into_iter().collect::<Vec<_>>(),
                            disabled: busy(),
                            error: logic::line_error(error_for(Slot::Line), line_problem),
                            onchange: move |next: Vec<ApproverEntry>| {
                                line_touched.set(true);
                                line.set(next);
                                server_error.set(None);
                            },
                        }
                    }
                }
                div { class: "form-actions",
                    Link { class: "btn btn-ghost", to: Route::Leave {}, "취소" }
                    Button {
                        kind: ButtonKind::Primary,
                        submit: true,
                        busy: busy(),
                        disabled: !can_submit,
                        "신청"
                    }
                }
            }
        }
    }
}

/// 핸들러 안에서 휴가 종류 목록을 다시 읽는다(렌더 시점의 값이 이미 옮겨졌을 수 있다).
fn type_list_snapshot(types: Resource<api::ApiResult<Vec<LeaveTypeDto>>>) -> Vec<LeaveTypeDto> {
    match &*types.peek() {
        Some(Ok(list)) => logic::usable_types(list),
        _ => Vec::new(),
    }
}

#[component]
fn FieldError(message: String) -> Element {
    if message.is_empty() {
        return rsx! {};
    }
    rsx! { span { class: "field-error", role: "alert", "{message}" } }
}

#[component]
fn LeaveSummary(preview: Result<logic::Preview, logic::Problem>) -> Element {
    match preview {
        Ok(preview) => {
            let class = if preview.short_of_balance() {
                "leave-summary leave-summary-warn"
            } else {
                "leave-summary"
            };
            rsx! {
                div { class, role: "status",
                    strong { {logic::summary_text(&preview)} }
                    if preview.short_of_balance() {
                        span { "지금 잔여보다 많습니다. 휴가 시작일 기준 잔여로 서버가 다시 확인합니다." }
                    }
                    small { class: "field-hint", "주말을 뺀 미리보기입니다. 공휴일은 신청할 때 서버가 반영합니다." }
                }
            }
        }
        Err(problem) => rsx! {
            div { class: "leave-summary leave-summary-muted", role: "status",
                span { {problem.message()} }
            }
        },
    }
}

/// 폼 계산(순수 함수).
pub(crate) mod logic {
    use chrono::{Days, NaiveDate};
    use pango_api::leave::{CreateLeaveRequest, LeaveTypeDto};
    use pango_core::calendar::{parse_date, DateRange, WorkCalendar, WorkWeek};
    use pango_core::leave::{LeaveAmount, LeaveSpan, LeaveUnit};

    use crate::format;
    use crate::ui::approval_line::{line_problem, to_inputs};
    use crate::ui::ApproverEntry;

    /// 사유 최대 길이(서버의 `Reason` 규칙과 같다).
    pub const REASON_MAX_CHARS: i64 = 1_000;
    /// 이보다 더 지난 날짜로 시작하는 신청은 서버가 받지 않는다.
    pub const MAX_BACKDATE_DAYS: u64 = 30;

    /// 조회 상태(불러오는 중·실패 문구·완료).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Load {
        Loading,
        Failed(String),
        Ready,
    }

    /// 오류를 붙일 자리.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Slot {
        Form,
        Type,
        Unit,
        Dates,
        Reason,
        Line,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Problem {
        NoType,
        InvalidStart,
        InvalidEnd,
        EndBeforeStart,
        TooLong,
        PartialAcrossDays,
        PartialNotAllowed,
        NoWorkingDays,
        BlankReason,
        ReasonTooLong,
        Line(&'static str),
    }

    impl Problem {
        pub fn message(self) -> &'static str {
            match self {
                Self::NoType => "휴가 종류를 골라 주세요.",
                Self::InvalidStart => "시작일을 골라 주세요.",
                Self::InvalidEnd => "종료일을 골라 주세요.",
                Self::EndBeforeStart => "종료일이 시작일보다 빠릅니다.",
                Self::TooLong => "한 번에 1년 넘게 신청할 수 없습니다.",
                Self::PartialAcrossDays => "반차·반반차는 하루만 신청할 수 있습니다.",
                Self::PartialNotAllowed => "이 휴가 종류는 종일로만 쓸 수 있습니다.",
                Self::NoWorkingDays => "선택한 기간이 모두 주말입니다.",
                Self::BlankReason => "사유를 입력해 주세요.",
                Self::ReasonTooLong => "사유는 1,000자까지 쓸 수 있습니다.",
                Self::Line(message) => message,
            }
        }

        pub fn slot(self) -> Slot {
            match self {
                Self::NoType => Slot::Type,
                Self::PartialAcrossDays | Self::PartialNotAllowed => Slot::Unit,
                Self::InvalidStart
                | Self::InvalidEnd
                | Self::EndBeforeStart
                | Self::TooLong
                | Self::NoWorkingDays => Slot::Dates,
                Self::BlankReason | Self::ReasonTooLong => Slot::Reason,
                Self::Line(_) => Slot::Line,
            }
        }
    }

    /// 미리보기 결과.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Preview {
        /// 주말을 뺀 근무일 수.
        pub working_days: u32,
        pub amount: LeaveAmount,
        /// 연차에서 차감하는 종류일 때, 신청 후 잔여(1/4일). 잔여를 아직 모르면 `None`.
        pub remaining_after: Option<i32>,
        pub deducts: bool,
    }

    impl Preview {
        pub fn short_of_balance(&self) -> bool {
            self.remaining_after.is_some_and(|q| q < 0)
        }
    }

    /// 신청할 수 있는 종류(켜져 있는 것)만, 정렬 순서대로.
    pub fn usable_types(types: &[LeaveTypeDto]) -> Vec<LeaveTypeDto> {
        let mut list: Vec<LeaveTypeDto> = types.iter().filter(|t| t.active).cloned().collect();
        list.sort_by(|a, b| (a.sort_order, &a.name).cmp(&(b.sort_order, &b.name)));
        list
    }

    /// 고른 종류. 아직 고르지 않았거나 목록에 없는 값이면 첫 번째.
    pub fn chosen_type(types: &[LeaveTypeDto], id: &str) -> Option<LeaveTypeDto> {
        types
            .iter()
            .find(|t| t.id.to_string() == id)
            .or_else(|| types.first())
            .cloned()
    }

    pub fn units_for(allow_partial: bool) -> Vec<LeaveUnit> {
        if allow_partial {
            LeaveUnit::ALL.to_vec()
        } else {
            vec![LeaveUnit::FullDay]
        }
    }

    /// 종류를 바꿨을 때의 단위: 부분 단위를 못 쓰는 종류면 종일로.
    pub fn unit_for_type(unit: LeaveUnit, allow_partial: bool) -> LeaveUnit {
        if unit.is_partial() && !allow_partial {
            LeaveUnit::FullDay
        } else {
            unit
        }
    }

    /// 단위를 바꿨을 때의 종료일: 부분 단위는 시작일과 같게.
    pub fn end_for(unit: LeaveUnit, start: &str, end: &str) -> String {
        if unit.is_partial() {
            start.to_owned()
        } else {
            end.to_owned()
        }
    }

    /// 시작일을 바꿨을 때의 종료일: 부분 단위이거나 종료일이 더 빠르면 시작일로 맞춘다.
    pub fn end_after_start_change(unit: LeaveUnit, start: &str, end: &str) -> String {
        let ordered = match (parse_date("start_date", start), parse_date("end_date", end)) {
            (Ok(s), Ok(e)) => e >= s,
            _ => false,
        };
        if unit.is_partial() || !ordered {
            start.to_owned()
        } else {
            end.to_owned()
        }
    }

    /// 날짜 입력의 최솟값(서버가 받는 가장 이른 시작일).
    pub fn earliest_start(today: NaiveDate) -> NaiveDate {
        today
            .checked_sub_days(Days::new(MAX_BACKDATE_DAYS))
            .unwrap_or(today)
    }

    pub fn reason_problem(reason: &str) -> Option<Problem> {
        if reason.trim().is_empty() {
            Some(Problem::BlankReason)
        } else if reason.chars().count() > REASON_MAX_CHARS as usize {
            Some(Problem::ReasonTooLong)
        } else {
            None
        }
    }

    /// 날짜·단위·잔여로 사용 일수와 신청 후 잔여를 미리 계산한다.
    /// 주말만 빼며, 공휴일은 서버가 신청할 때 반영한다(이 값은 미리보기일 뿐이다).
    pub fn preview(
        start: &str,
        end: &str,
        unit: LeaveUnit,
        leave_type: Option<&LeaveTypeDto>,
        remaining: Option<i32>,
    ) -> Result<Preview, Problem> {
        let leave_type = leave_type.ok_or(Problem::NoType)?;
        if unit.is_partial() && !leave_type.allow_partial {
            return Err(Problem::PartialNotAllowed);
        }
        let start = parse_date("start_date", start).map_err(|_| Problem::InvalidStart)?;
        let end = parse_date("end_date", end).map_err(|_| Problem::InvalidEnd)?;
        if end < start {
            return Err(Problem::EndBeforeStart);
        }
        if unit.is_partial() && start != end {
            return Err(Problem::PartialAcrossDays);
        }
        let range = DateRange::new(start, end).map_err(|_| Problem::TooLong)?;
        let calendar = WorkCalendar::new(WorkWeek::MON_TO_FRI, []);
        let span = LeaveSpan::new(range, unit, &calendar).map_err(|_| Problem::NoWorkingDays)?;
        let working_days = u32::try_from(span.working_dates().len()).unwrap_or(u32::MAX);
        let amount = span.amount();
        let remaining_after = if leave_type.deducts_balance {
            remaining.and_then(|r| r.checked_sub(amount.quarters()))
        } else {
            None
        };
        Ok(Preview {
            working_days,
            amount,
            remaining_after,
            deducts: leave_type.deducts_balance,
        })
    }

    /// `1.5일 사용, 신청 후 잔여 10일` / `1일 사용 · 연차에서 차감하지 않음`
    pub fn summary_text(preview: &Preview) -> String {
        let used = format::leave_days(preview.amount.quarters());
        match (preview.deducts, preview.remaining_after) {
            (true, Some(after)) => {
                format!("{used} 사용, 신청 후 잔여 {}", format::leave_days(after))
            }
            (true, None) => format!("{used} 사용"),
            (false, _) => format!("{used} 사용 · 연차에서 차감하지 않음"),
        }
    }

    /// 폼 값을 신청 본문으로. 서버로 보내기 전에 알 수 있는 문제를 먼저 돌려준다.
    pub fn to_request(
        leave_type: Option<&LeaveTypeDto>,
        unit: LeaveUnit,
        start: &str,
        end: &str,
        reason: &str,
        line: &[ApproverEntry],
    ) -> Result<CreateLeaveRequest, Problem> {
        preview(start, end, unit, leave_type, None)?;
        if let Some(problem) = reason_problem(reason) {
            return Err(problem);
        }
        if let Some(message) = line_problem(line) {
            return Err(Problem::Line(message));
        }
        let leave_type = leave_type.ok_or(Problem::NoType)?;
        let start_date = parse_date("start_date", start).map_err(|_| Problem::InvalidStart)?;
        let end_date = parse_date("end_date", end).map_err(|_| Problem::InvalidEnd)?;
        Ok(CreateLeaveRequest {
            leave_type_id: leave_type.id,
            start_date,
            end_date,
            unit,
            reason: reason.trim().to_owned(),
            steps: Some(to_inputs(line)),
            references: Vec::new(),
        })
    }

    /// 결재선 아래에 보일 문구: 서버 오류가 있으면 그것을, 없으면 입력으로 알 수 있는 문제를.
    pub fn line_error(server: String, problem: Option<&'static str>) -> String {
        if server.is_empty() {
            problem.unwrap_or_default().to_owned()
        } else {
            server
        }
    }

    /// 서버 오류를 붙일 자리.
    pub fn error_slot(code: &str, field: Option<&str>) -> Slot {
        match (code, field) {
            ("insufficient_leave_balance" | "leave_overlap" | "leave_no_working_days", _) => {
                Slot::Dates
            }
            ("leave_type_inactive", _) => Slot::Type,
            ("member_inactive", _) => Slot::Line,
            ("validation_failed", Some("start_date" | "end_date" | "date_range")) => Slot::Dates,
            ("validation_failed", Some("unit")) => Slot::Unit,
            ("validation_failed", Some("reason")) => Slot::Reason,
            ("validation_failed", Some("steps" | "references")) => Slot::Line,
            ("validation_failed", Some("leave_type_id")) | ("not_found", _) => Slot::Type,
            _ => Slot::Form,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pango_core::approval::StepKind;
        use pango_core::id::{LeaveTypeId, MemberId};

        fn leave_type(name: &str, deducts: bool, partial: bool, order: i64) -> LeaveTypeDto {
            LeaveTypeDto {
                id: LeaveTypeId::new(),
                code: "annual".into(),
                name: name.into(),
                deducts_balance: deducts,
                allow_partial: partial,
                active: true,
                sort_order: order,
                version: 1,
            }
        }

        fn approver() -> Vec<ApproverEntry> {
            vec![ApproverEntry {
                approver_id: MemberId::new(),
                approver_name: "팀장".into(),
                kind: StepKind::Approve,
            }]
        }

        fn d(value: &str) -> NaiveDate {
            value.parse().expect("date")
        }

        #[test]
        fn preview_counts_weekdays_and_remaining() {
            let annual = leave_type("연차", true, true, 0);
            // 2026-03-06(금) ~ 03-09(월): 주말 빼고 2일.
            let p = preview(
                "2026-03-06",
                "2026-03-09",
                LeaveUnit::FullDay,
                Some(&annual),
                Some(40),
            )
            .expect("ok");
            assert_eq!(p.working_days, 2);
            assert_eq!(p.amount.quarters(), 8);
            assert_eq!(p.remaining_after, Some(32));
            assert_eq!(summary_text(&p), "2일 사용, 신청 후 잔여 8일");
            let half = preview(
                "2026-03-06",
                "2026-03-06",
                LeaveUnit::PmHalf,
                Some(&annual),
                Some(1),
            )
            .expect("ok");
            assert_eq!(summary_text(&half), "0.5일 사용, 신청 후 잔여 -0.25일");
            assert!(half.short_of_balance());
            let quarter = preview(
                "2026-03-06",
                "2026-03-06",
                LeaveUnit::AmQuarter,
                Some(&annual),
                None,
            )
            .expect("ok");
            assert_eq!(summary_text(&quarter), "0.25일 사용");
        }

        #[test]
        fn preview_for_non_deducting_type_ignores_balance() {
            let sick = leave_type("병가", false, false, 1);
            let p = preview(
                "2026-03-02",
                "2026-03-02",
                LeaveUnit::FullDay,
                Some(&sick),
                Some(0),
            )
            .expect("ok");
            assert_eq!(p.remaining_after, None);
            assert!(!p.short_of_balance());
            assert!(summary_text(&p).contains("차감하지 않음"));
        }

        #[test]
        fn preview_rejects_obviously_invalid_forms() {
            let annual = leave_type("연차", true, true, 0);
            let full_only = leave_type("경조", false, false, 0);
            let check = |start, end, unit, t: Option<&LeaveTypeDto>| {
                preview(start, end, unit, t, Some(40)).err()
            };
            assert_eq!(
                check("2026-03-02", "2026-03-02", LeaveUnit::FullDay, None),
                Some(Problem::NoType)
            );
            assert_eq!(
                check(
                    "2026-03-03",
                    "2026-03-02",
                    LeaveUnit::FullDay,
                    Some(&annual)
                ),
                Some(Problem::EndBeforeStart)
            );
            assert_eq!(
                check("2026-03-02", "2026-03-03", LeaveUnit::AmHalf, Some(&annual)),
                Some(Problem::PartialAcrossDays)
            );
            assert_eq!(
                check(
                    "2026-03-02",
                    "2026-03-02",
                    LeaveUnit::AmHalf,
                    Some(&full_only)
                ),
                Some(Problem::PartialNotAllowed)
            );
            assert_eq!(
                check(
                    "2026-03-07",
                    "2026-03-08",
                    LeaveUnit::FullDay,
                    Some(&annual)
                ),
                Some(Problem::NoWorkingDays)
            );
            assert_eq!(
                check(
                    "2026-01-01",
                    "2027-01-02",
                    LeaveUnit::FullDay,
                    Some(&annual)
                ),
                Some(Problem::TooLong)
            );
            assert_eq!(
                check("", "2026-03-02", LeaveUnit::FullDay, Some(&annual)),
                Some(Problem::InvalidStart)
            );
            assert_eq!(
                check("2026-03-02", "2026-3-9", LeaveUnit::FullDay, Some(&annual)),
                Some(Problem::InvalidEnd)
            );
            assert_eq!(
                check(
                    "2026-02-30",
                    "2026-03-02",
                    LeaveUnit::FullDay,
                    Some(&annual)
                ),
                Some(Problem::InvalidStart)
            );
            assert!(check(
                "2026-01-01",
                "2026-12-31",
                LeaveUnit::FullDay,
                Some(&annual)
            )
            .is_none());
        }

        #[test]
        fn unit_and_end_follow_each_other() {
            assert_eq!(units_for(false), [LeaveUnit::FullDay]);
            assert_eq!(units_for(true).len(), 5);
            assert_eq!(unit_for_type(LeaveUnit::AmHalf, false), LeaveUnit::FullDay);
            assert_eq!(unit_for_type(LeaveUnit::AmHalf, true), LeaveUnit::AmHalf);
            assert_eq!(
                end_for(LeaveUnit::PmQuarter, "2026-03-02", "2026-03-05"),
                "2026-03-02"
            );
            assert_eq!(
                end_for(LeaveUnit::FullDay, "2026-03-02", "2026-03-05"),
                "2026-03-05"
            );
            assert_eq!(
                end_after_start_change(LeaveUnit::FullDay, "2026-03-06", "2026-03-05"),
                "2026-03-06"
            );
            assert_eq!(
                end_after_start_change(LeaveUnit::FullDay, "2026-03-03", "2026-03-05"),
                "2026-03-05"
            );
            assert_eq!(
                end_after_start_change(LeaveUnit::AmHalf, "2026-03-03", "2026-03-05"),
                "2026-03-03"
            );
            assert_eq!(
                end_after_start_change(LeaveUnit::FullDay, "", "2026-03-05"),
                ""
            );
            assert_eq!(earliest_start(d("2026-03-31")), d("2026-03-01"));
        }

        #[test]
        fn types_are_filtered_and_defaulted() {
            let mut off = leave_type("꺼짐", true, true, 0);
            off.active = false;
            let late = leave_type("나중", true, true, 5);
            let first = leave_type("먼저", true, true, 1);
            let list = usable_types(&[off, late.clone(), first.clone()]);
            assert_eq!(
                list.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
                ["먼저", "나중"]
            );
            assert_eq!(chosen_type(&list, ""), Some(first));
            assert_eq!(chosen_type(&list, &late.id.to_string()), Some(late));
            assert_eq!(chosen_type(&[], "x"), None);
        }

        #[test]
        fn to_request_trims_reason_and_sends_the_line() {
            let annual = leave_type("연차", true, true, 0);
            let line = approver();
            let request = to_request(
                Some(&annual),
                LeaveUnit::FullDay,
                "2026-03-02",
                "2026-03-03",
                "  가족 여행  ",
                &line,
            )
            .expect("valid");
            assert_eq!(request.reason, "가족 여행");
            assert_eq!(request.leave_type_id, annual.id);
            assert_eq!(request.steps.map(|s| s.len()), Some(1));
            let blank = to_request(
                Some(&annual),
                LeaveUnit::FullDay,
                "2026-03-02",
                "2026-03-02",
                " \n ",
                &line,
            );
            assert_eq!(blank.err(), Some(Problem::BlankReason));
            let long = "가".repeat(1_001);
            let too_long = to_request(
                Some(&annual),
                LeaveUnit::FullDay,
                "2026-03-02",
                "2026-03-02",
                &long,
                &line,
            );
            assert_eq!(too_long.err(), Some(Problem::ReasonTooLong));
            let no_line = to_request(
                Some(&annual),
                LeaveUnit::FullDay,
                "2026-03-02",
                "2026-03-02",
                "사유",
                &[],
            );
            assert_eq!(no_line.err().map(Problem::slot), Some(Slot::Line));
        }

        #[test]
        fn server_errors_land_near_fields() {
            assert_eq!(error_slot("insufficient_leave_balance", None), Slot::Dates);
            assert_eq!(error_slot("leave_overlap", None), Slot::Dates);
            assert_eq!(error_slot("leave_no_working_days", None), Slot::Dates);
            assert_eq!(
                error_slot("validation_failed", Some("start_date")),
                Slot::Dates
            );
            assert_eq!(error_slot("validation_failed", Some("unit")), Slot::Unit);
            assert_eq!(
                error_slot("validation_failed", Some("reason")),
                Slot::Reason
            );
            assert_eq!(error_slot("validation_failed", Some("steps")), Slot::Line);
            assert_eq!(error_slot("leave_type_inactive", None), Slot::Type);
            assert_eq!(error_slot("network_error", None), Slot::Form);
            assert_eq!(error_slot("validation_failed", None), Slot::Form);
        }

        #[test]
        fn line_error_prefers_the_server_message() {
            assert_eq!(line_error(String::new(), None), "");
            assert_eq!(line_error(String::new(), Some("빈 결재선")), "빈 결재선");
            assert_eq!(line_error("서버".into(), Some("빈 결재선")), "서버");
        }

        #[test]
        fn every_problem_has_a_message() {
            for problem in [
                Problem::NoType,
                Problem::InvalidStart,
                Problem::InvalidEnd,
                Problem::EndBeforeStart,
                Problem::TooLong,
                Problem::PartialAcrossDays,
                Problem::PartialNotAllowed,
                Problem::NoWorkingDays,
                Problem::BlankReason,
                Problem::ReasonTooLong,
                Problem::Line("x"),
            ] {
                assert!(!problem.message().is_empty());
            }
        }
    }
}
