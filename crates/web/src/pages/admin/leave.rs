//! 휴가 관리: 잔여 현황과 수동 조정, 신청 내역, 휴가 종류, 연차 자동 부여.

use chrono::NaiveDate;
use dioxus::prelude::*;
use pango_api::leave::{
    AccrualRunResult, BalanceDto, LeaveRequestDto, LeaveTypeDto, MemberBalance,
};
use pango_api::{Empty, Page};
use pango_core::leave::LeaveRequestStatus;

use crate::api::{self, ApiResult};
use crate::format;
use crate::pages::leave::{company_zone, logic as leave_logic, today};
use crate::route::Route;
use crate::session::{use_session, use_toasts};
use crate::text;
use crate::ui::{
    report_error, report_success, resource_view, Badge, Button, ButtonKind, Card, Checkbox,
    ConfirmModal, DateField, EmptyState, FormError, IconName, LoadMore, Modal, PageHeader,
    SelectField, Tabs, TextArea, TextField, Tone,
};

const TAB_BALANCES: &str = "balances";
const TAB_REQUESTS: &str = "requests";
const TAB_TYPES: &str = "types";

#[component]
pub fn AdminLeave() -> Element {
    let tz = company_zone();
    let today = today(tz);
    let mut tab = use_signal(|| TAB_BALANCES.to_owned());
    let mut running = use_signal(|| false);
    let mut last_run = use_signal(|| None::<AccrualRunResult>);
    let mut balances =
        use_resource(|| async { api::get::<Vec<MemberBalance>>("/admin/leave/balances").await });

    let run_accrual = move |_| async move {
        if running() {
            return;
        }
        running.set(true);
        match api::post::<_, AccrualRunResult>("/admin/leave/accrual/run", &Empty {}).await {
            Ok(result) => {
                report_success(logic::accrual_message(&result));
                last_run.set(Some(result));
                balances.restart();
            }
            Err(error) => report_error(&error),
        }
        running.set(false);
    };

    let tabs = vec![
        (TAB_BALANCES.to_owned(), "잔여 현황".to_owned(), None),
        (TAB_REQUESTS.to_owned(), "신청 내역".to_owned(), None),
        (TAB_TYPES.to_owned(), "휴가 종류".to_owned(), None),
    ];

    rsx! {
        PageHeader {
            title: "휴가 관리",
            subtitle: "연차는 입사일 기준으로 매일 자동 부여됩니다. 바로 반영하려면 부여를 실행하세요.",
            actions: rsx! {
                Button { kind: ButtonKind::Secondary, busy: running(), onclick: run_accrual, "연차 부여 실행" }
            },
        }
        if let Some(result) = last_run() {
            p { class: "leave-notice", role: "status", {logic::accrual_message(&result)} }
        }
        Tabs { tabs, active: tab(), onselect: move |key| tab.set(key) }
        match tab().as_str() {
            TAB_REQUESTS => rsx! { RequestsTab {} },
            TAB_TYPES => rsx! { TypesTab {} },
            _ => rsx! { BalancesTab { balances, today } },
        }
    }
}

// ───────────── 잔여 현황 ─────────────

#[component]
fn BalancesTab(balances: Resource<ApiResult<Vec<MemberBalance>>>, today: NaiveDate) -> Element {
    let mut target = use_signal(|| None::<MemberBalance>);
    let mut balances = balances;
    rsx! {
        {resource_view(balances, move |rows: Vec<MemberBalance>| {
            let rows = logic::sorted_balances(rows);
            rsx! {
                if rows.is_empty() {
                    EmptyState { title: "재직 중인 구성원이 없습니다.", icon: IconName::People }
                } else {
                    Card {
                        div { class: "table-wrap",
                            table { class: "table leave-table",
                                thead {
                                    tr {
                                        th { "구성원" }
                                        th { class: "num", "부여" }
                                        th { class: "num", "사용" }
                                        th { class: "num", "잔여" }
                                        th { span { class: "sr-only", "동작" } }
                                    }
                                }
                                tbody {
                                    for row in rows {
                                        tr { key: "{row.member_id}",
                                            td {
                                                strong { "{row.member_name}" }
                                                if let Some(department) = &row.department_name {
                                                    small { class: "leave-cell-sub", "{department}" }
                                                }
                                            }
                                            td { class: "num", {format::leave_days(row.balance.granted.quarters())} }
                                            td { class: "num", {format::leave_days(row.balance.used.quarters())} }
                                            td {
                                                class: if row.balance.remaining.is_negative() { "num leave-negative" } else { "num" },
                                                {format::leave_days(row.balance.remaining.quarters())}
                                            }
                                            td { class: "num",
                                                Button {
                                                    kind: ButtonKind::Secondary,
                                                    small: true,
                                                    onclick: {
                                                        let row = row.clone();
                                                        move |_| target.set(Some(row.clone()))
                                                    },
                                                    "조정"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })}
        if let Some(member) = target() {
            AdjustModal {
                member,
                today,
                onclose: move |_| target.set(None),
                ondone: move |_| {
                    target.set(None);
                    balances.restart();
                },
            }
        }
    }
}

#[component]
fn AdjustModal(
    member: MemberBalance,
    today: NaiveDate,
    onclose: EventHandler<()>,
    ondone: EventHandler<()>,
) -> Element {
    let mut session = use_session();
    let mut days = use_signal(String::new);
    let mut effective_on = use_signal(|| today.to_string());
    let mut memo = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);

    let parsed = logic::parse_adjust_days(&days());
    let days_error = match (&parsed, days().trim().is_empty()) {
        (Err(problem), false) => problem.message().to_owned(),
        _ => String::new(),
    };
    let can_submit = parsed.is_ok() && !memo().trim().is_empty();
    let member_id = member.member_id;

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let request = match logic::to_adjust_request(member_id, &days(), &effective_on(), &memo()) {
            Ok(request) => request,
            Err(message) => {
                error.set(message.to_owned());
                return;
            }
        };
        busy.set(true);
        error.set(String::new());
        match api::post::<_, BalanceDto>("/admin/leave/adjustments", &request).await {
            Ok(balance) => {
                report_success(format!(
                    "조정했습니다. 잔여 {}",
                    format::leave_days(balance.balance.remaining.quarters())
                ));
                ondone.call(());
            }
            Err(failure) => error.set(session.handle(&failure)),
        }
        busy.set(false);
    };

    rsx! {
        Modal { title: "{member.member_name} 연차 조정", onclose: move |_| onclose.call(()),
            form { class: "form", onsubmit: submit,
                p { class: "field-hint",
                    "현재 잔여 {format::leave_days(member.balance.remaining.quarters())}. 더하려면 양수, 빼려면 음수(예: -0.5)를 입력합니다."
                }
                FormError { message: error() }
                TextField {
                    label: "조정 일수",
                    value: days(),
                    placeholder: "예: 1, 0.5, -0.25",
                    hint: "0.25일 단위, 최대 ±100일",
                    error: days_error,
                    required: true,
                    maxlength: 10,
                    oninput: move |value: String| days.set(value),
                }
                DateField {
                    label: "적용일",
                    value: effective_on(),
                    required: true,
                    hint: "더하는 조정은 이날부터 쓸 수 있고, 빼는 조정은 이날 사용한 것으로 계산됩니다.",
                    oninput: move |value: String| effective_on.set(value),
                }
                TextArea {
                    label: "메모",
                    value: memo(),
                    rows: 3,
                    required: true,
                    maxlength: 1000,
                    placeholder: "조정 사유(구성원의 사용 내역에 보입니다)",
                    oninput: move |value: String| memo.set(value),
                }
                div { class: "modal-actions",
                    Button { kind: ButtonKind::Ghost, onclick: move |_| onclose.call(()), "닫기" }
                    Button { submit: true, busy: busy(), disabled: !can_submit, "조정" }
                }
            }
        }
    }
}

// ───────────── 신청 내역 ─────────────

#[component]
fn RequestsTab() -> Element {
    let mut status = use_signal(String::new);
    let mut more = use_signal(Vec::<LeaveRequestDto>::new);
    let mut more_cursor = use_signal(|| None::<Option<String>>);
    let mut loading_more = use_signal(|| false);
    let mut confirm = use_signal(|| None::<LeaveRequestDto>);
    let mut busy = use_signal(|| false);
    let mut first = use_resource(move || async move {
        let path = logic::requests_path(&status(), None);
        api::get::<Page<LeaveRequestDto>>(&path).await
    });

    let mut reset = move || {
        more.set(Vec::new());
        more_cursor.set(None);
    };

    let load_more = move |cursor: String| async move {
        if loading_more() {
            return;
        }
        loading_more.set(true);
        match api::get::<Page<LeaveRequestDto>>(&logic::requests_path(&status(), Some(&cursor)))
            .await
        {
            Ok(page) => {
                more.write().extend(page.items);
                more_cursor.set(Some(page.next_cursor));
            }
            Err(error) => report_error(&error),
        }
        loading_more.set(false);
    };

    let cancel = move |id: String| async move {
        if busy() {
            return;
        }
        busy.set(true);
        let path = format!("/admin/leave/requests/{id}/cancel");
        match api::post::<_, LeaveRequestDto>(&path, &Empty {}).await {
            Ok(_) => report_success("휴가를 취소했습니다. 연차가 복원되었습니다."),
            Err(error) => report_error(&error),
        }
        confirm.set(None);
        reset();
        first.restart();
        busy.set(false);
    };

    let status_options: Vec<(String, String)> =
        std::iter::once((String::new(), "전체 상태".to_owned()))
            .chain(LeaveRequestStatus::ALL.iter().map(|s| {
                (
                    s.as_str().to_owned(),
                    text::leave_status_label(*s).to_owned(),
                )
            }))
            .collect();

    rsx! {
        div { class: "filters leave-filters",
            SelectField {
                label: "상태",
                value: status(),
                options: status_options,
                onchange: move |value: String| {
                    reset();
                    status.set(value);
                },
            }
        }
        {resource_view(first, move |page: Page<LeaveRequestDto>| {
            let extra = more();
            let next = logic::next_cursor(page.next_cursor.as_deref(), more_cursor().as_ref());
            let items: Vec<LeaveRequestDto> = logic::merge_pages(page.items, extra);
            rsx! {
                if items.is_empty() {
                    EmptyState { title: "조건에 맞는 신청이 없습니다.", icon: IconName::Calendar }
                } else {
                    Card {
                        ul { class: "list",
                            for item in items {
                                li { key: "{item.id}", class: "list-row leave-row",
                                    div { class: "list-row-main",
                                        div { class: "leave-row-title",
                                            strong { "{item.member_name}" }
                                            Badge { tone: leave_logic::status_tone(item.status), {text::leave_status_label(item.status)} }
                                        }
                                        small {
                                            "{item.leave_type_name} · {leave_logic::period_label(item.start_date, item.end_date)} · {text::leave_unit_label(item.unit)} · {format::leave_days(item.amount.quarters())}"
                                        }
                                        p { class: "leave-row-reason", "{item.reason}" }
                                    }
                                    div { class: "list-row-side",
                                        Link { to: Route::ApprovalDetail { id: item.document_id.to_string() }, "결재 문서" }
                                        if logic::admin_can_cancel(item.status) {
                                            Button {
                                                kind: ButtonKind::Secondary,
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
                            has_more: next.is_some(),
                            busy: loading_more(),
                            onclick: move |_| {
                                if let Some(cursor) = next.clone() {
                                    spawn(load_more(cursor));
                                }
                            },
                        }
                    }
                }
            }
        })}
        if let Some(target) = confirm() {
            ConfirmModal {
                title: "휴가 취소",
                message: logic::admin_cancel_message(&target),
                confirm_label: "휴가 취소",
                busy: busy(),
                oncancel: move |_| confirm.set(None),
                onconfirm: {
                    let id = target.id.to_string();
                    move |_| {
                        spawn(cancel(id.clone()));
                    }
                },
            }
        }
    }
}

// ───────────── 휴가 종류 ─────────────

#[component]
fn TypesTab() -> Element {
    let mut editing = use_signal(|| None::<Option<LeaveTypeDto>>);
    let mut types =
        use_resource(|| async { api::get::<Vec<LeaveTypeDto>>("/admin/leave/types").await });
    rsx! {
        div { class: "leave-toolbar",
            p { class: "field-hint", "결재 중인 신청이 있는 종류는 끌 수 없습니다. 차감 여부는 만든 뒤 바꿀 수 없습니다." }
            Button { small: true, onclick: move |_| editing.set(Some(None)), "종류 추가" }
        }
        {resource_view(types, move |list: Vec<LeaveTypeDto>| rsx! {
            if list.is_empty() {
                EmptyState { title: "휴가 종류가 없습니다.", icon: IconName::List }
            } else {
                Card {
                    ul { class: "list",
                        for item in logic::sorted_types(list) {
                            li { key: "{item.id}", class: "list-row",
                                div { class: "list-row-main",
                                    div { class: "leave-row-title",
                                        strong { "{item.name}" }
                                        if !item.active {
                                            Badge { tone: Tone::Neutral, "꺼짐" }
                                        }
                                    }
                                    small { {logic::type_summary(&item)} }
                                }
                                div { class: "list-row-side",
                                    Button {
                                        kind: ButtonKind::Secondary,
                                        small: true,
                                        onclick: {
                                            let item = item.clone();
                                            move |_| editing.set(Some(Some(item.clone())))
                                        },
                                        "수정"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })}
        if let Some(target) = editing() {
            TypeModal {
                existing: target,
                onclose: move |_| editing.set(None),
                ondone: move |_| {
                    editing.set(None);
                    types.restart();
                },
            }
        }
    }
}

#[component]
fn TypeModal(
    existing: Option<LeaveTypeDto>,
    onclose: EventHandler<()>,
    ondone: EventHandler<()>,
) -> Element {
    let mut session = use_session();
    let mut toasts = use_toasts();
    let initial = logic::TypeForm::from_existing(existing.as_ref());
    let mut form = use_signal(|| initial);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let key = use_signal(api::new_idempotency_key);
    let creating = existing.is_none();
    let existing_id = existing.as_ref().map(|t| t.id.to_string());
    let version = existing.as_ref().map(|t| t.version);

    let submit = move |event: FormEvent| {
        let existing_id = existing_id.clone();
        async move {
            event.prevent_default();
            if busy() {
                return;
            }
            let current = form();
            busy.set(true);
            error.set(String::new());
            let result = match (&existing_id, version) {
                (Some(id), Some(version)) => match current.to_update(version) {
                    Ok(body) => {
                        api::patch::<_, LeaveTypeDto>(&format!("/admin/leave/types/{id}"), &body)
                            .await
                            .map_err(Some)
                    }
                    Err(message) => {
                        error.set(message.to_owned());
                        Err(None)
                    }
                },
                _ => match current.to_create() {
                    Ok(body) => {
                        api::post_once::<_, LeaveTypeDto>("/admin/leave/types", &body, &key())
                            .await
                            .map_err(Some)
                    }
                    Err(message) => {
                        error.set(message.to_owned());
                        Err(None)
                    }
                },
            };
            match result {
                Ok(_) => {
                    report_success(if creating {
                        "휴가 종류를 추가했습니다."
                    } else {
                        "저장했습니다."
                    });
                    ondone.call(());
                }
                Err(Some(failure)) if failure.code == "version_conflict" => {
                    session.handle(&failure);
                    toasts.error(failure.message());
                    ondone.call(());
                }
                Err(Some(failure)) => {
                    session.handle(&failure);
                    error.set(logic::type_error_message(
                        &failure.code,
                        failure.field.as_deref(),
                        failure.reason.as_deref(),
                    ));
                }
                Err(None) => {}
            }
            busy.set(false);
        }
    };

    let current = form();
    rsx! {
        Modal {
            title: if creating { "휴가 종류 추가" } else { "휴가 종류 수정" },
            onclose: move |_| onclose.call(()),
            form { class: "form", onsubmit: submit,
                FormError { message: error() }
                TextField {
                    label: "이름",
                    value: current.name.clone(),
                    required: true,
                    maxlength: 50,
                    oninput: move |value: String| form.write().name = value,
                }
                TextField {
                    label: "코드",
                    value: current.code.clone(),
                    hint: if creating { "영문 소문자·숫자·_ (예: family_event). 만든 뒤 바꿀 수 없습니다." } else { "만든 뒤 바꿀 수 없습니다." },
                    disabled: !creating,
                    required: true,
                    maxlength: 32,
                    autocomplete: "off",
                    oninput: move |value: String| form.write().code = value,
                }
                Checkbox {
                    label: "연차 잔여에서 차감",
                    checked: current.deducts_balance,
                    disabled: !creating,
                    hint: if creating { "만든 뒤에는 바꿀 수 없습니다." } else { "만든 뒤에는 바꿀 수 없습니다(읽기 전용)." },
                    onchange: move |value: bool| form.write().deducts_balance = value,
                }
                Checkbox {
                    label: "반차·반반차 허용",
                    checked: current.allow_partial,
                    onchange: move |value: bool| form.write().allow_partial = value,
                }
                if !creating {
                    Checkbox {
                        label: "사용",
                        checked: current.active,
                        hint: "끄면 새 신청에서 고를 수 없습니다.",
                        onchange: move |value: bool| form.write().active = value,
                    }
                }
                TextField {
                    label: "정렬 순서",
                    kind: "number",
                    value: current.sort_order.clone(),
                    hint: "작을수록 앞에 나옵니다.",
                    maxlength: 6,
                    oninput: move |value: String| form.write().sort_order = value,
                }
                div { class: "modal-actions",
                    Button { kind: ButtonKind::Ghost, onclick: move |_| onclose.call(()), "닫기" }
                    Button { submit: true, busy: busy(), if creating { "추가" } else { "저장" } }
                }
            }
        }
    }
}

/// 화면 계산(순수 함수).
pub(crate) mod logic {
    use pango_api::leave::{
        AccrualRunResult, AdjustLeaveRequest, CreateLeaveTypeRequest, LeaveRequestDto,
        LeaveTypeDto, MemberBalance, UpdateLeaveTypeRequest,
    };
    use pango_core::calendar::parse_date;
    use pango_core::id::MemberId;
    use pango_core::leave::{LeaveAmount, LeaveRequestStatus};

    use crate::api;
    use crate::pages::leave::logic::period_label;
    use crate::text;

    /// 조정 한 건의 최대 크기(1/4일 단위). 서버 규칙(100일)과 같다.
    pub const MAX_ADJUST_QUARTERS: i32 = 400;
    /// 신청 내역 한 번에 읽는 수.
    pub const PAGE_SIZE: u32 = 50;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AdjustInputError {
        Empty,
        NotNumber,
        NotQuarter,
        Zero,
        OutOfRange,
    }

    impl AdjustInputError {
        pub fn message(self) -> &'static str {
            match self {
                Self::Empty => "조정 일수를 입력해 주세요.",
                Self::NotNumber => "숫자로 입력해 주세요(예: 1, 0.5, -0.25).",
                Self::NotQuarter => "0.25일 단위로 입력해 주세요.",
                Self::Zero => "0일은 조정할 수 없습니다.",
                Self::OutOfRange => "한 번에 100일까지 조정할 수 있습니다.",
            }
        }
    }

    /// 일 단위 입력(`1`, `-0.5`, `+2.75`, `1,5`)을 1/4일 단위로 바꾼다.
    ///
    /// 소수 구분자로 `.`과 `,`를 모두 받는다(쉼표를 소수점으로 쓰는 환경). 천 단위 구분은
    /// 받지 않는다. 소수는 두 자리까지이고 0.25의 배수여야 한다.
    pub fn parse_adjust_days(raw: &str) -> Result<i32, AdjustInputError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AdjustInputError::Empty);
        }
        let (negative, rest) = match trimmed.chars().next() {
            Some('-' | '−') => (
                true,
                &trimmed[trimmed.chars().next().map_or(1, char::len_utf8)..],
            ),
            Some('+') => (false, &trimmed[1..]),
            _ => (false, trimmed),
        };
        let rest = rest.trim_start();
        let (whole, fraction) = match rest.find(['.', ',']) {
            Some(index) => (&rest[..index], &rest[index + 1..]),
            None => (rest, ""),
        };
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if (whole.is_empty() && fraction.is_empty()) || !digits(whole) || !digits(fraction) {
            return Err(AdjustInputError::NotNumber);
        }
        let whole_digits = whole.trim_start_matches('0');
        if whole_digits.len() > 4 {
            return Err(AdjustInputError::OutOfRange);
        }
        let days: i32 = if whole_digits.is_empty() {
            0
        } else {
            whole_digits
                .parse()
                .map_err(|_| AdjustInputError::NotNumber)?
        };
        let quarter = match fraction {
            "" | "0" | "00" => 0,
            "25" => 1,
            "5" | "50" => 2,
            "75" => 3,
            _ => return Err(AdjustInputError::NotQuarter),
        };
        let quarters = days
            .checked_mul(LeaveAmount::QUARTERS_PER_DAY)
            .and_then(|q| q.checked_add(quarter))
            .ok_or(AdjustInputError::OutOfRange)?;
        if quarters == 0 {
            return Err(AdjustInputError::Zero);
        }
        if quarters > MAX_ADJUST_QUARTERS {
            return Err(AdjustInputError::OutOfRange);
        }
        Ok(if negative { -quarters } else { quarters })
    }

    pub fn to_adjust_request(
        member_id: MemberId,
        days: &str,
        effective_on: &str,
        memo: &str,
    ) -> Result<AdjustLeaveRequest, &'static str> {
        let amount_q = parse_adjust_days(days).map_err(AdjustInputError::message)?;
        let effective_on =
            parse_date("effective_on", effective_on).map_err(|_| "적용일을 골라 주세요.")?;
        let memo = memo.trim();
        if memo.is_empty() {
            return Err("메모를 입력해 주세요.");
        }
        if memo.chars().count() > 1_000 {
            return Err("메모는 1,000자까지 쓸 수 있습니다.");
        }
        Ok(AdjustLeaveRequest {
            member_id,
            amount_q,
            effective_on,
            memo: memo.to_owned(),
        })
    }

    /// 부서 → 이름 순. 부서 없는 사람은 뒤로.
    pub fn sorted_balances(mut rows: Vec<MemberBalance>) -> Vec<MemberBalance> {
        rows.sort_by(|a, b| {
            (
                a.department_name.is_none(),
                &a.department_name,
                &a.member_name,
            )
                .cmp(&(
                    b.department_name.is_none(),
                    &b.department_name,
                    &b.member_name,
                ))
        });
        rows
    }

    pub fn accrual_message(result: &AccrualRunResult) -> String {
        if result.grants_inserted == 0 {
            format!(
                "재직 구성원 {}명을 확인했습니다. 새로 부여할 연차는 없습니다.",
                result.members
            )
        } else {
            format!(
                "재직 구성원 {}명을 확인해 {}건을 새로 부여했습니다.",
                result.members, result.grants_inserted
            )
        }
    }

    /// `GET /admin/leave/requests` 경로. 상태 값은 알려진 것만 싣는다.
    pub fn requests_path(status: &str, cursor: Option<&str>) -> String {
        let mut path = format!("/admin/leave/requests?limit={PAGE_SIZE}");
        if let Ok(status) = LeaveRequestStatus::parse(status) {
            path.push_str(&format!("&status={}", status.as_str()));
        }
        if let Some(cursor) = cursor.filter(|c| !c.is_empty()) {
            path.push_str(&format!("&cursor={}", api_encode(cursor)));
        }
        path
    }

    /// 쿼리 값 인코딩. 커서는 서버가 만든 16진수라 보통 그대로 쓰지만, 다른 값이면 인코딩한다.
    fn api_encode(value: &str) -> String {
        if value.bytes().all(|b| b.is_ascii_alphanumeric()) {
            value.to_owned()
        } else {
            api::encode(value)
        }
    }

    /// 다음에 읽을 커서. 더 읽은 적이 있으면 그 응답의 커서를, 아니면 첫 페이지의 것을 쓴다.
    pub fn next_cursor(first: Option<&str>, more: Option<&Option<String>>) -> Option<String> {
        match more {
            Some(after_more) => after_more.clone(),
            None => first.map(str::to_owned),
        }
    }

    /// 첫 페이지 뒤에 더 읽은 항목을 붙인다. 그 사이 목록이 바뀌어 겹친 항목은 한 번만.
    pub fn merge_pages(
        first: Vec<LeaveRequestDto>,
        more: Vec<LeaveRequestDto>,
    ) -> Vec<LeaveRequestDto> {
        let mut items = first;
        for item in more {
            if !items.iter().any(|existing| existing.id == item.id) {
                items.push(item);
            }
        }
        items
    }

    /// 관리자는 시작한 휴가도 취소할 수 있다. 끝난 신청(반려·취소)만 제외.
    pub fn admin_can_cancel(status: LeaveRequestStatus) -> bool {
        matches!(
            status,
            LeaveRequestStatus::Pending | LeaveRequestStatus::Approved
        )
    }

    pub fn admin_cancel_message(request: &LeaveRequestDto) -> String {
        let period = period_label(request.start_date, request.end_date);
        format!(
            "{}님의 {period} {}을(를) 취소합니다. 사용한 연차는 복원되고, 결재 중이면 결재 문서가 회수됩니다.",
            request.member_name, request.leave_type_name
        )
    }

    pub fn sorted_types(mut list: Vec<LeaveTypeDto>) -> Vec<LeaveTypeDto> {
        list.sort_by(|a, b| (a.sort_order, &a.name).cmp(&(b.sort_order, &b.name)));
        list
    }

    pub fn type_summary(item: &LeaveTypeDto) -> String {
        [
            Some(item.code.as_str()),
            Some(if item.deducts_balance {
                "연차 차감"
            } else {
                "차감 없음"
            }),
            item.allow_partial.then_some("반차·반반차 가능"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
    }

    pub fn type_error_message(code: &str, field: Option<&str>, reason: Option<&str>) -> String {
        match code {
            "in_use" => {
                "결재 중인 신청이 있어 끌 수 없습니다. 신청이 끝난 뒤 다시 시도해 주세요.".into()
            }
            "already_exists" => "같은 코드의 휴가 종류가 이미 있습니다.".into(),
            _ => text::error_message(code, field, reason),
        }
    }

    /// 휴가 종류 폼의 입력값(문자열 그대로).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TypeForm {
        pub code: String,
        pub name: String,
        pub deducts_balance: bool,
        pub allow_partial: bool,
        pub active: bool,
        pub sort_order: String,
    }

    impl TypeForm {
        pub fn from_existing(existing: Option<&LeaveTypeDto>) -> Self {
            match existing {
                Some(t) => Self {
                    code: t.code.clone(),
                    name: t.name.clone(),
                    deducts_balance: t.deducts_balance,
                    allow_partial: t.allow_partial,
                    active: t.active,
                    sort_order: t.sort_order.to_string(),
                },
                None => Self {
                    code: String::new(),
                    name: String::new(),
                    deducts_balance: false,
                    allow_partial: false,
                    active: true,
                    sort_order: "0".into(),
                },
            }
        }

        fn name(&self) -> Result<String, &'static str> {
            let name = self.name.trim();
            if name.is_empty() {
                Err("이름을 입력해 주세요.")
            } else if name.chars().count() > 50 {
                Err("이름은 50자까지 쓸 수 있습니다.")
            } else {
                Ok(name.to_owned())
            }
        }

        pub fn to_create(&self) -> Result<CreateLeaveTypeRequest, &'static str> {
            let name = self.name()?;
            let code = self.code.trim();
            if !valid_code(code) {
                return Err(
                    "코드는 영문 소문자로 시작하고 소문자·숫자·_만 써서 2~32자로 정해 주세요.",
                );
            }
            Ok(CreateLeaveTypeRequest {
                code: code.to_owned(),
                name,
                deducts_balance: self.deducts_balance,
                allow_partial: self.allow_partial,
                sort_order: parse_sort_order(&self.sort_order)?,
            })
        }

        pub fn to_update(&self, version: i64) -> Result<UpdateLeaveTypeRequest, &'static str> {
            Ok(UpdateLeaveTypeRequest {
                version,
                name: self.name()?,
                allow_partial: self.allow_partial,
                active: self.active,
                sort_order: parse_sort_order(&self.sort_order)?,
            })
        }
    }

    /// 서버의 `Code` 규칙과 같다.
    pub fn valid_code(code: &str) -> bool {
        (2..=32).contains(&code.len())
            && code.as_bytes()[0].is_ascii_lowercase()
            && code
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }

    /// 정렬 순서. 비우면 0, 정수만 받는다.
    pub fn parse_sort_order(raw: &str) -> Result<i64, &'static str> {
        const LIMIT: i64 = 99_999;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(0);
        }
        trimmed
            .parse::<i64>()
            .ok()
            .filter(|n| (-LIMIT..=LIMIT).contains(n))
            .ok_or("정렬 순서는 정수로 입력해 주세요.")
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use chrono::NaiveDate;
        use pango_core::id::{DocumentId, LeaveRequestId, LeaveTypeId};
        use pango_core::leave::{Balance, LeaveUnit};
        use pango_core::time::Timestamp;

        #[test]
        fn adjust_days_accept_quarter_steps_and_signs() {
            for (input, expected) in [
                ("1", 4),
                ("0.25", 1),
                ("0.5", 2),
                (".75", 3),
                ("2.50", 10),
                ("+1", 4),
                ("-0.25", -1),
                ("−1.5", -6),
                (" - 2 ", -8),
                ("1,5", 6),
                ("-0,75", -3),
                ("100", 400),
                ("-100", -400),
                ("007", 28),
                ("3.", 12),
            ] {
                assert_eq!(parse_adjust_days(input), Ok(expected), "{input:?}");
            }
        }

        #[test]
        fn adjust_days_reject_bad_input() {
            use AdjustInputError::*;
            for (input, expected) in [
                ("", Empty),
                ("   ", Empty),
                ("abc", NotNumber),
                ("1e2", NotNumber),
                ("1.2.3", NotNumber),
                ("1,000", NotQuarter),
                ("0.1", NotQuarter),
                ("0.125", NotQuarter),
                ("0.3", NotQuarter),
                ("--1", NotNumber),
                ("+-1", NotNumber),
                ("-", NotNumber),
                (".", NotNumber),
                ("1 000", NotNumber),
                ("１", NotNumber),
                ("0", Zero),
                ("-0.00", Zero),
                ("100.25", OutOfRange),
                ("-101", OutOfRange),
                ("99999999999999999999", OutOfRange),
                ("NaN", NotNumber),
                ("inf", NotNumber),
            ] {
                assert_eq!(parse_adjust_days(input), Err(expected), "{input:?}");
                assert!(!expected.message().is_empty());
            }
        }

        #[test]
        fn adjust_request_requires_memo_and_date() {
            let id = MemberId::new();
            let ok = to_adjust_request(id, "-0.5", "2026-03-02", "  출근율 미달 ").expect("ok");
            assert_eq!(ok.amount_q, -2);
            assert_eq!(ok.memo, "출근율 미달");
            assert_eq!(
                ok.effective_on,
                "2026-03-02".parse::<NaiveDate>().expect("date")
            );
            assert!(to_adjust_request(id, "1", "2026-03-02", "  ").is_err());
            assert!(to_adjust_request(id, "1", "", "memo").is_err());
            assert!(to_adjust_request(id, "0.1", "2026-03-02", "memo").is_err());
        }

        #[test]
        fn request_paths_and_paging() {
            assert_eq!(requests_path("", None), "/admin/leave/requests?limit=50");
            assert_eq!(
                requests_path("pending", Some("7b22")),
                "/admin/leave/requests?limit=50&status=pending&cursor=7b22"
            );
            assert_eq!(
                requests_path("bogus", Some("")),
                "/admin/leave/requests?limit=50"
            );
            assert_eq!(next_cursor(Some("a"), None), Some("a".into()));
            assert_eq!(next_cursor(Some("a"), Some(&None)), None);
            assert_eq!(next_cursor(None, Some(&Some("b".into()))), Some("b".into()));
            assert!(admin_can_cancel(LeaveRequestStatus::Approved));
            assert!(admin_can_cancel(LeaveRequestStatus::Pending));
            assert!(!admin_can_cancel(LeaveRequestStatus::Canceled));
            assert!(!admin_can_cancel(LeaveRequestStatus::Rejected));
        }

        fn request(name: &str) -> LeaveRequestDto {
            LeaveRequestDto {
                id: LeaveRequestId::new(),
                member_id: MemberId::new(),
                member_name: name.into(),
                leave_type_id: LeaveTypeId::new(),
                leave_type_name: "연차".into(),
                start_date: NaiveDate::MIN,
                end_date: NaiveDate::MIN,
                unit: LeaveUnit::FullDay,
                amount: LeaveAmount::from_quarters(4),
                reason: "r".into(),
                status: LeaveRequestStatus::Approved,
                document_id: DocumentId::new(),
                created_at: Timestamp::from_utc(chrono::DateTime::UNIX_EPOCH),
                decided_at: None,
            }
        }

        #[test]
        fn pages_merge_without_duplicates() {
            let a = request("a");
            let b = request("b");
            let merged = merge_pages(vec![a.clone(), b.clone()], vec![b, request("c")]);
            assert_eq!(
                merged
                    .iter()
                    .map(|r| r.member_name.as_str())
                    .collect::<Vec<_>>(),
                ["a", "b", "c"]
            );
            assert!(admin_cancel_message(&a).contains("a님"));
        }

        #[test]
        fn balances_sort_by_department_then_name() {
            let row = |name: &str, dept: Option<&str>| MemberBalance {
                member_id: MemberId::new(),
                member_name: name.into(),
                department_name: dept.map(str::to_owned),
                hired_on: NaiveDate::MIN,
                balance: Balance {
                    granted: LeaveAmount::ZERO,
                    used: LeaveAmount::ZERO,
                    expired: LeaveAmount::ZERO,
                    remaining: LeaveAmount::ZERO,
                },
            };
            let sorted = sorted_balances(vec![
                row("하", None),
                row("나", Some("개발")),
                row("가", Some("영업")),
                row("가", Some("개발")),
            ]);
            let names: Vec<_> = sorted
                .iter()
                .map(|r| (r.department_name.as_deref(), r.member_name.as_str()))
                .collect();
            assert_eq!(
                names,
                [
                    (Some("개발"), "가"),
                    (Some("개발"), "나"),
                    (Some("영업"), "가"),
                    (None, "하")
                ]
            );
        }

        #[test]
        fn type_form_converts_and_validates() {
            let mut form = TypeForm::from_existing(None);
            assert!(form.to_create().is_err(), "빈 이름");
            form.name = " 경조 휴가 ".into();
            form.code = "Family".into();
            assert!(form.to_create().is_err(), "대문자 코드");
            form.code = "family_event".into();
            form.sort_order = " 3 ".into();
            let created = form.to_create().expect("ok");
            assert_eq!(created.name, "경조 휴가");
            assert_eq!(created.sort_order, 3);
            form.sort_order = "1.5".into();
            assert!(form.to_create().is_err());
            form.sort_order = String::new();
            assert_eq!(form.to_update(7).expect("ok").sort_order, 0);
            assert_eq!(form.to_update(7).expect("ok").version, 7);
            assert!(
                valid_code("ab") && !valid_code("a") && !valid_code("1a") && !valid_code("a-b")
            );
            assert!(!valid_code(&"a".repeat(33)));
            assert_eq!(
                parse_sort_order("999999"),
                Err("정렬 순서는 정수로 입력해 주세요.")
            );
        }

        #[test]
        fn messages() {
            let none = AccrualRunResult {
                members: 3,
                grants_inserted: 0,
            };
            assert!(accrual_message(&none).contains("없습니다"));
            let some = AccrualRunResult {
                members: 3,
                grants_inserted: 2,
            };
            assert!(accrual_message(&some).contains("2건"));
            assert!(type_error_message("in_use", None, None).contains("결재 중"));
            assert_eq!(
                type_error_message("forbidden", None, None),
                text::error_message("forbidden", None, None)
            );
            let t = LeaveTypeDto {
                id: LeaveTypeId::new(),
                code: "annual".into(),
                name: "연차".into(),
                deducts_balance: true,
                allow_partial: true,
                active: true,
                sort_order: 0,
                version: 1,
            };
            assert_eq!(type_summary(&t), "annual · 연차 차감 · 반차·반반차 가능");
        }
    }
}
