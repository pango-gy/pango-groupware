//! 근태 관리(관리자): 기간·부서·구성원으로 기록을 찾고, 추가·수정하고, CSV로 내보낸다.

use dioxus::prelude::*;
use pango_api::attendance::{AdminAttendanceItem, AttendanceRecordDto};
use pango_api::org::{DepartmentDto, MemberSummary};
use pango_api::Page;
use pango_core::attendance::WorkType;
use pango_core::id::AttendanceRecordId;

use crate::api;
use crate::calendar::YearMonth;
use crate::format;
use crate::pages::attendance::{company_zone, current_time, RecordHistory};
use crate::session::use_session;
use crate::ui::{
    report_error, report_success, resource_view, Avatar, Badge, Button, ButtonKind, Card,
    EmptyState, FormError, Icon, IconName, LoadMore, MemberPicker, Modal, PageHeader, SelectField,
    TextArea, TextField,
};

use logic::{AdminRow, FailureAction, Filter};

/// 열려 있는 기록 편집 창.
#[derive(Debug, Clone, PartialEq)]
enum Editor {
    Create,
    Edit(AdminAttendanceItem),
}

#[component]
pub fn AdminAttendance() -> Element {
    let session = use_session();
    let tz = company_zone(&session);
    let this_month = YearMonth::of(format::local_date(current_time(), tz));
    let mut from = use_signal(move || this_month.first_day().to_string());
    let mut to = use_signal(move || this_month.last_day().to_string());
    let mut department = use_signal(String::new);
    let mut member = use_signal(|| None::<MemberSummary>);
    let mut extra = use_signal(Vec::<AdminAttendanceItem>::new);
    let mut next_cursor = use_signal(|| None::<String>);
    let mut loading_more = use_signal(|| false);
    let mut editor = use_signal(|| None::<Editor>);
    let mut history = use_signal(|| None::<(AttendanceRecordId, String)>);

    let filter = move || Filter {
        from: from(),
        to: to(),
        department: department(),
        member: member().map(|m| m.id),
    };

    let departments =
        use_resource(|| async { api::get::<Vec<DepartmentDto>>("/departments").await });
    // 첫 페이지. "더 보기"로 읽은 뒤 페이지는 `extra`에 붙인다.
    let mut list = use_resource(move || async move {
        let Ok(path) = logic::list_path(&filter(), None) else {
            extra.set(Vec::new());
            next_cursor.set(None);
            return Ok(None);
        };
        let page = api::get::<Page<AdminAttendanceItem>>(&path).await?;
        extra.set(Vec::new());
        next_cursor.set(page.next_cursor);
        Ok(Some(page.items))
    });

    let load_more = move |_| async move {
        if loading_more() {
            return;
        }
        let requested = filter();
        let Some(cursor) = next_cursor() else {
            return;
        };
        let Ok(path) = logic::list_path(&requested, Some(&cursor)) else {
            return;
        };
        loading_more.set(true);
        let result = api::get::<Page<AdminAttendanceItem>>(&path).await;
        loading_more.set(false);
        match result {
            // 기다리는 동안 조건이 바뀌었으면 버린다(새 첫 페이지가 이미 다시 읽힌다).
            Ok(page) if filter() == requested => {
                extra.write().extend(page.items);
                next_cursor.set(page.next_cursor);
            }
            Ok(_) => {}
            Err(error) => report_error(&error),
        }
    };

    let current = filter();
    let filter_error = logic::validate(&current)
        .err()
        .map(|issue| issue.message().to_owned())
        .unwrap_or_default();
    let export_href = logic::export_href(&current).ok();
    let department_options = match &*departments.read() {
        Some(Ok(list)) => logic::department_options(list),
        _ => logic::department_options(&[]),
    };

    rsx! {
        PageHeader {
            title: "근태 관리",
            subtitle: "기록을 고치면 사유와 함께 이력이 남습니다.",
            actions: rsx! {
                if let Some(href) = export_href {
                    a { class: "btn btn-secondary", href: "{href}", "CSV 내보내기" }
                } else {
                    Button { kind: ButtonKind::Secondary, disabled: true, "CSV 내보내기" }
                }
                Button { onclick: move |_| editor.set(Some(Editor::Create)),
                    Icon { name: IconName::Plus }
                    "기록 추가"
                }
            },
        }
        Card {
            div { class: "att-admin-filters",
                TextField {
                    label: "시작일",
                    kind: "date",
                    value: from(),
                    oninput: move |value| from.set(value),
                }
                TextField {
                    label: "종료일",
                    kind: "date",
                    value: to(),
                    oninput: move |value| to.set(value),
                }
                SelectField {
                    label: "부서",
                    value: department(),
                    options: department_options,
                    onchange: move |value| department.set(value),
                }
                if let Some(chosen) = member() {
                    PickedMember { label: "구성원", member: chosen, onclear: move |_| member.set(None) }
                } else {
                    MemberPicker { label: "구성원", onpick: move |picked| member.set(Some(picked)) }
                }
            }
            FormError { message: filter_error }
            p { class: "field-hint", "CSV에는 기간만 적용되고 부서·구성원 조건은 적용되지 않습니다." }
        }
        {resource_view(list, move |first: Option<Vec<AdminAttendanceItem>>| {
            let Some(first) = first else {
                return rsx! {};
            };
            let rows: Vec<AdminRow> = first
                .iter()
                .chain(extra.read().iter())
                .map(|item| logic::admin_row(item, tz))
                .collect();
            if rows.is_empty() {
                return rsx! { EmptyState { title: "조건에 맞는 기록이 없습니다.", icon: IconName::Clock } };
            }
            rsx! {
                Card {
                    div { class: "table-wrap att-desktop",
                        table { class: "table att-table",
                            thead {
                                tr {
                                    th { "날짜" }
                                    th { "구성원" }
                                    th { "근무 형태" }
                                    th { "출근" }
                                    th { "퇴근" }
                                    th { class: "num", "근무시간" }
                                    th { "상태" }
                                    th { span { class: "sr-only", "동작" } }
                                }
                            }
                            tbody {
                                for row in rows.clone() {
                                    tr { key: "{row.item.record.id}",
                                        td { "{row.date_label}" }
                                        td {
                                            strong { "{row.item.member_name}" }
                                            span { class: "att-note", "{row.member_meta}" }
                                        }
                                        td { "{row.cells.work_type}" }
                                        td { "{row.cells.clock_in}" }
                                        td { "{row.cells.clock_out}" }
                                        td { class: "num", "{row.cells.worked}" }
                                        td {
                                            Badge { tone: row.cells.tone, "{row.cells.status}" }
                                        }
                                        td { class: "att-row-actions",
                                            RowActions { row: row.clone(), editor, history }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    ul { class: "list att-mobile",
                        for row in rows {
                            li { key: "{row.item.record.id}", class: "list-row att-admin-item",
                                div { class: "list-row-main",
                                    strong { "{row.item.member_name} · {row.date_label}" }
                                    small { "{row.cells.clock_in} – {row.cells.clock_out} · {row.cells.work_type}" }
                                    small { "{row.member_meta}" }
                                }
                                div { class: "list-row-side",
                                    Badge { tone: row.cells.tone, "{row.cells.status}" }
                                    div { class: "att-row-actions",
                                        RowActions { row: row.clone(), editor, history }
                                    }
                                }
                            }
                        }
                    }
                    LoadMore {
                        has_more: next_cursor.read().is_some(),
                        busy: loading_more(),
                        onclick: load_more,
                    }
                }
            }
        })}
        if let Some(open) = editor() {
            RecordEditor {
                editor: open,
                onclose: move |_| editor.set(None),
                onsaved: move |_| {
                    editor.set(None);
                    list.restart();
                },
            }
        }
        if let Some((record_id, title)) = history() {
            RecordHistory { record_id, title, onclose: move |_| history.set(None) }
        }
    }
}

/// 표·목록 한 줄의 "수정"·"이력" 버튼.
#[component]
fn RowActions(
    row: AdminRow,
    editor: Signal<Option<Editor>>,
    history: Signal<Option<(AttendanceRecordId, String)>>,
) -> Element {
    let item = row.item.clone();
    let target = (row.item.record.id, row.title.clone());
    rsx! {
        Button {
            kind: ButtonKind::Secondary,
            small: true,
            onclick: move |_| editor.set(Some(Editor::Edit(item.clone()))),
            "수정"
            span { class: "sr-only", " {row.title}" }
        }
        Button {
            kind: ButtonKind::Ghost,
            small: true,
            onclick: move |_| history.set(Some(target.clone())),
            "이력"
            span { class: "sr-only", " {row.title}" }
        }
    }
}

/// 고른 구성원 표시 + 다시 고르기.
#[component]
fn PickedMember(
    #[props(into)] label: String,
    member: MemberSummary,
    onclear: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "field",
            span { class: "field-label", "{label}" }
            div { class: "att-picked",
                Avatar { name: member.name.clone() }
                span { class: "att-picked-name", "{member.name}" }
                button {
                    class: "icon-btn",
                    r#type: "button",
                    "aria-label": "{label} 다시 고르기",
                    onclick: move |_| onclear.call(()),
                    Icon { name: IconName::Close }
                }
            }
        }
    }
}

/// 기록 추가·수정 창. 시각은 회사 시간대의 벽시계로 입력받는다.
#[component]
fn RecordEditor(editor: Editor, onclose: EventHandler<()>, onsaved: EventHandler<()>) -> Element {
    let session = use_session();
    let tz = company_zone(&session);
    let zone_name = session.me().map(|me| me.timezone).unwrap_or_default();
    let initial = match &editor {
        Editor::Create => logic::RecordForm::blank(format::local_date(current_time(), tz)),
        Editor::Edit(item) => logic::RecordForm::from_record(&item.record, tz),
    };
    let mut form = use_signal(move || initial);
    let mut picked = use_signal(|| None::<MemberSummary>);
    // 창을 열 때 한 번 만든다. 다시 눌러도 기록은 하나만 생긴다.
    let key = use_signal(api::new_idempotency_key);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);

    let creating = matches!(editor, Editor::Create);
    let (title, fixed_member) = match &editor {
        Editor::Create => ("기록 추가".to_owned(), None),
        Editor::Edit(item) => (
            "기록 수정".to_owned(),
            Some(format!(
                "{} · {}",
                item.member_name,
                format::date_md(item.record.work_date)
            )),
        ),
    };

    let submit = move |event: FormEvent| {
        let editor = editor.clone();
        async move {
            event.prevent_default();
            if busy() {
                return;
            }
            let current = form();
            let outcome = match &editor {
                Editor::Create => match logic::create_request(&current, tz) {
                    Ok(request) => {
                        busy.set(true);
                        error.set(String::new());
                        api::post_once::<_, AttendanceRecordDto>(
                            "/admin/attendance",
                            &request,
                            &key(),
                        )
                        .await
                        .map(|_| ())
                    }
                    Err(issue) => {
                        error.set(issue.message().to_owned());
                        return;
                    }
                },
                Editor::Edit(item) => {
                    match logic::edit_request(&current, item.record.version, tz) {
                        Ok(request) => {
                            busy.set(true);
                            error.set(String::new());
                            api::patch::<_, AttendanceRecordDto>(
                                &format!("/admin/attendance/{}", item.record.id),
                                &request,
                            )
                            .await
                            .map(|_| ())
                        }
                        Err(issue) => {
                            error.set(issue.message().to_owned());
                            return;
                        }
                    }
                }
            };
            busy.set(false);
            match outcome {
                Ok(()) => {
                    report_success(if creating {
                        "기록을 추가했습니다."
                    } else {
                        "기록을 수정했습니다."
                    });
                    onsaved.call(());
                }
                Err(failure) => match logic::failure_action(&failure.code) {
                    FailureAction::ShowInForm => error.set(failure.message()),
                    FailureAction::Reload => {
                        report_error(&failure);
                        onsaved.call(());
                    }
                    FailureAction::Toast => report_error(&failure),
                },
            }
        }
    };

    let current = form();
    rsx! {
        Modal { title, onclose: move |_| onclose.call(()),
            form { class: "form", onsubmit: submit,
                FormError { message: error() }
                if let Some(fixed) = fixed_member {
                    div { class: "field",
                        span { class: "field-label", "대상" }
                        strong { "{fixed}" }
                    }
                } else {
                    if let Some(chosen) = picked() {
                        PickedMember {
                            label: "구성원",
                            member: chosen,
                            onclear: move |_| {
                                picked.set(None);
                                form.write().member = None;
                            },
                        }
                    } else {
                        MemberPicker {
                            label: "구성원",
                            onpick: move |chosen: MemberSummary| {
                                form.write().member = Some(chosen.id);
                                picked.set(Some(chosen));
                            },
                        }
                    }
                    TextField {
                        label: "근무일",
                        kind: "date",
                        value: current.work_date.clone(),
                        required: true,
                        hint: "출근 시각이 이 근무일에 속해야 합니다.",
                        oninput: move |value| form.write().work_date = value,
                    }
                }
                div { class: "form-row",
                    TextField {
                        label: "출근 시각",
                        kind: "datetime-local",
                        value: current.clock_in.clone(),
                        required: true,
                        hint: "{zone_name} 기준",
                        oninput: move |value| form.write().clock_in = value,
                    }
                    TextField {
                        label: "퇴근 시각",
                        kind: "datetime-local",
                        value: current.clock_out.clone(),
                        hint: "비워 두면 퇴근 전으로 남습니다.",
                        oninput: move |value| form.write().clock_out = value,
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
                if creating {
                    TextField {
                        label: "메모",
                        value: current.note.clone(),
                        maxlength: logic::TEXT_MAX_CHARS as i64,
                        oninput: move |value| form.write().note = value,
                    }
                }
                TextArea {
                    label: "사유",
                    value: current.reason.clone(),
                    rows: 3,
                    required: true,
                    maxlength: logic::TEXT_MAX_CHARS as i64,
                    placeholder: "예: 지문 인식기 고장으로 출근 누락",
                    oninput: move |value| form.write().reason = value,
                }
                div { class: "card-actions",
                    Button { kind: ButtonKind::Ghost, onclick: move |_| onclose.call(()), "취소" }
                    Button { submit: true, busy: busy(), "저장" }
                }
            }
        }
    }
}

/// 화면이 그리는 값과 요청을 만드는 순수 함수.
mod logic {
    use chrono::NaiveDate;
    use chrono_tz::Tz;
    use pango_api::attendance::{
        AdminAttendanceItem, AdminCreateRecordRequest, AdminEditRecordRequest, AttendanceRecordDto,
    };
    use pango_api::org::DepartmentDto;
    use pango_api::API_PREFIX;
    use pango_core::attendance::{WorkType, MAX_SHIFT};
    use pango_core::calendar::{parse_date, DateRange};
    use pango_core::id::{DepartmentId, MemberId};
    use pango_core::time::Timestamp;

    use crate::format::{self, LocalInputError};
    use crate::pages::attendance::logic::{record_cells, RecordCells};
    use crate::text;

    /// 사유·메모 최대 글자 수(서버 `Reason`·`Comment` 규칙과 같다).
    pub const TEXT_MAX_CHARS: usize = 1_000;
    /// 한 번에 읽는 기록 수.
    const PAGE_SIZE: u32 = 50;

    /// 목록 조건. 입력값을 그대로 담고, 요청 직전에 [`validate`]로 검사한다.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Filter {
        pub from: String,
        pub to: String,
        /// 빈 문자열이면 전체 부서.
        pub department: String,
        pub member: Option<MemberId>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FilterError {
        InvalidFrom,
        InvalidTo,
        Reversed,
        TooLong,
        InvalidDepartment,
    }

    impl FilterError {
        pub const fn message(self) -> &'static str {
            match self {
                Self::InvalidFrom => "시작일을 확인해 주세요.",
                Self::InvalidTo => "종료일을 확인해 주세요.",
                Self::Reversed => "종료일이 시작일보다 빠릅니다.",
                Self::TooLong => "기간은 최대 366일까지 조회할 수 있습니다.",
                Self::InvalidDepartment => "부서를 다시 선택해 주세요.",
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ValidFilter {
        pub range: DateRange,
        pub department: Option<DepartmentId>,
        pub member: Option<MemberId>,
    }

    pub fn validate(filter: &Filter) -> Result<ValidFilter, FilterError> {
        let from = parse_date("from", filter.from.trim()).map_err(|_| FilterError::InvalidFrom)?;
        let to = parse_date("to", filter.to.trim()).map_err(|_| FilterError::InvalidTo)?;
        if to < from {
            return Err(FilterError::Reversed);
        }
        let range = DateRange::new(from, to).map_err(|_| FilterError::TooLong)?;
        let department = match filter.department.trim() {
            "" => None,
            raw => Some(
                raw.parse::<DepartmentId>()
                    .map_err(|_| FilterError::InvalidDepartment)?,
            ),
        };
        Ok(ValidFilter {
            range,
            department,
            member: filter.member,
        })
    }

    /// 쿼리 값 인코딩(RFC 3986 비예약 문자만 그대로 둔다).
    pub fn escape(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for byte in value.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    out.push(char::from(byte));
                }
                _ => out.push_str(&format!("%{byte:02X}")),
            }
        }
        out
    }

    fn range_query(range: DateRange) -> String {
        format!("from={}&to={}", range.start(), range.end())
    }

    /// `/admin/attendance?from=…&to=…&limit=50[&department_id=…][&member_id=…][&cursor=…]`
    pub fn list_path(filter: &Filter, cursor: Option<&str>) -> Result<String, FilterError> {
        let valid = validate(filter)?;
        let mut path = format!(
            "/admin/attendance?{}&limit={PAGE_SIZE}",
            range_query(valid.range)
        );
        if let Some(department) = valid.department {
            path.push_str(&format!("&department_id={department}"));
        }
        if let Some(member) = valid.member {
            path.push_str(&format!("&member_id={member}"));
        }
        if let Some(cursor) = cursor.filter(|c| !c.is_empty()) {
            path.push_str(&format!("&cursor={}", escape(cursor)));
        }
        Ok(path)
    }

    /// CSV 내려받기 주소. 쿠키로 인증되므로 일반 링크로 연다.
    pub fn export_href(filter: &Filter) -> Result<String, FilterError> {
        let valid = validate(filter)?;
        Ok(format!(
            "{API_PREFIX}/admin/attendance/export.csv?{}",
            range_query(valid.range)
        ))
    }

    pub fn department_options(list: &[DepartmentDto]) -> Vec<(String, String)> {
        std::iter::once((String::new(), "전체 부서".to_owned()))
            .chain(list.iter().map(|d| (d.id.to_string(), d.name.clone())))
            .collect()
    }

    pub fn work_type_options() -> Vec<(String, String)> {
        WorkType::ALL
            .into_iter()
            .map(|t| (t.as_str().to_owned(), text::work_type_label(t).to_owned()))
            .collect()
    }

    /// 관리 목록 한 줄.
    #[derive(Debug, Clone, PartialEq)]
    pub struct AdminRow {
        pub item: AdminAttendanceItem,
        /// 이력 창 제목 등에 쓰는 `홍길동 · 3월 2일 (월)`.
        pub title: String,
        pub date_label: String,
        /// `hong · 개발팀`
        pub member_meta: String,
        pub cells: RecordCells,
    }

    pub fn admin_row(item: &AdminAttendanceItem, tz: Tz) -> AdminRow {
        let record = &item.record;
        let date_label = format::date_md(record.work_date);
        let member_meta = std::iter::once(item.login_id.clone())
            .chain(item.department_name.clone())
            .collect::<Vec<_>>()
            .join(" · ");
        AdminRow {
            item: item.clone(),
            title: format!("{} · {date_label}", item.member_name),
            date_label,
            member_meta,
            cells: record_cells(record.work_date, Some(record), &item.evaluation, tz),
        }
    }

    /// 기록 편집 창의 입력값.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RecordForm {
        pub member: Option<MemberId>,
        pub work_date: String,
        /// `datetime-local` 값.
        pub clock_in: String,
        pub clock_out: String,
        pub work_type: WorkType,
        pub note: String,
        pub reason: String,
    }

    impl RecordForm {
        pub fn blank(work_date: NaiveDate) -> Self {
            Self {
                member: None,
                work_date: work_date.to_string(),
                clock_in: String::new(),
                clock_out: String::new(),
                work_type: WorkType::Office,
                note: String::new(),
                reason: String::new(),
            }
        }

        pub fn from_record(record: &AttendanceRecordDto, tz: Tz) -> Self {
            Self {
                member: Some(record.member_id),
                work_date: record.work_date.to_string(),
                clock_in: format::local_input(record.clock_in_at, tz),
                clock_out: record
                    .clock_out_at
                    .map(|at| format::local_input(at, tz))
                    .unwrap_or_default(),
                work_type: record.work_type,
                note: record.note.clone().unwrap_or_default(),
                reason: String::new(),
            }
        }
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
                Self::ClockInInvalid => "출근 시각의 형식이 올바르지 않습니다.",
                Self::ClockInNonexistent => {
                    "출근 시각이 서머타임 전환으로 존재하지 않는 시각입니다."
                }
                Self::ClockOutInvalid => "퇴근 시각의 형식이 올바르지 않습니다.",
                Self::ClockOutNonexistent => {
                    "퇴근 시각이 서머타임 전환으로 존재하지 않는 시각입니다."
                }
                Self::ClockOutNotAfterClockIn => "퇴근 시각은 출근 시각보다 늦어야 합니다.",
                Self::ShiftTooLong => "한 번의 근무는 24시간을 넘을 수 없습니다.",
                Self::ReasonMissing => "사유를 입력해 주세요. 이력에 남습니다.",
                Self::ReasonTooLong => "사유는 1,000자까지 쓸 수 있습니다.",
                Self::NoteTooLong => "메모는 1,000자까지 쓸 수 있습니다.",
            }
        }
    }

    fn times(form: &RecordForm, tz: Tz) -> Result<(Timestamp, Option<Timestamp>), FormIssue> {
        let clock_in = format::parse_local_input(&form.clock_in, tz).map_err(|e| match e {
            LocalInputError::Empty => FormIssue::ClockInMissing,
            LocalInputError::Invalid => FormIssue::ClockInInvalid,
            LocalInputError::Nonexistent => FormIssue::ClockInNonexistent,
        })?;
        let clock_out = match format::parse_local_input(&form.clock_out, tz) {
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

    fn reason(form: &RecordForm) -> Result<String, FormIssue> {
        let reason = form.reason.trim();
        if reason.is_empty() {
            return Err(FormIssue::ReasonMissing);
        }
        if reason.chars().count() > TEXT_MAX_CHARS {
            return Err(FormIssue::ReasonTooLong);
        }
        Ok(reason.to_owned())
    }

    pub fn create_request(
        form: &RecordForm,
        tz: Tz,
    ) -> Result<AdminCreateRecordRequest, FormIssue> {
        let member_id = form.member.ok_or(FormIssue::MemberMissing)?;
        let work_date = parse_date("work_date", form.work_date.trim())
            .map_err(|_| FormIssue::WorkDateInvalid)?;
        let (clock_in_at, clock_out_at) = times(form, tz)?;
        let note = form.note.trim();
        if note.chars().count() > TEXT_MAX_CHARS {
            return Err(FormIssue::NoteTooLong);
        }
        Ok(AdminCreateRecordRequest {
            member_id,
            work_date: work_date.to_string(),
            clock_in_at,
            clock_out_at,
            work_type: form.work_type,
            note: (!note.is_empty()).then(|| note.to_owned()),
            reason: reason(form)?,
        })
    }

    pub fn edit_request(
        form: &RecordForm,
        version: i64,
        tz: Tz,
    ) -> Result<AdminEditRecordRequest, FormIssue> {
        let (clock_in_at, clock_out_at) = times(form, tz)?;
        Ok(AdminEditRecordRequest {
            version,
            clock_in_at,
            clock_out_at,
            work_type: form.work_type,
            reason: reason(form)?,
        })
    }

    /// 저장 실패를 어떻게 보여 줄지.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FailureAction {
        /// 입력을 고치면 되는 오류: 창 안에 보여 준다.
        ShowInForm,
        /// 대상이 바뀌었거나 사라짐: 알리고 창을 닫은 뒤 목록을 다시 읽는다.
        Reload,
        /// 그 밖(권한, 네트워크, 세션 만료 등): 토스트.
        Toast,
    }

    pub fn failure_action(code: &str) -> FailureAction {
        match code {
            "validation_failed" | "already_exists" | "member_inactive" | "not_working_member" => {
                FailureAction::ShowInForm
            }
            "version_conflict" | "not_found" => FailureAction::Reload,
            _ => FailureAction::Toast,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::pages::attendance::logic::fixtures::{date, evaluation, record, seoul, ts};
        use pango_core::attendance::DayStatus;

        fn filter(from: &str, to: &str) -> Filter {
            Filter {
                from: from.into(),
                to: to.into(),
                department: String::new(),
                member: None,
            }
        }

        fn form() -> RecordForm {
            RecordForm {
                member: Some(MemberId::new()),
                work_date: "2026-03-02".into(),
                clock_in: "2026-03-02T09:00".into(),
                clock_out: "2026-03-02T18:00".into(),
                work_type: WorkType::Field,
                note: "  ".into(),
                reason: " 누락 보정 ".into(),
            }
        }

        #[test]
        fn filter_validation_boundaries() {
            assert!(validate(&filter("2026-03-01", "2026-03-31")).is_ok());
            assert!(validate(&filter("2026-03-01", "2026-03-01")).is_ok());
            // 366일(양 끝 포함)까지 된다.
            assert!(validate(&filter("2024-01-01", "2024-12-31")).is_ok());
            assert_eq!(
                validate(&filter("2025-01-01", "2026-01-02")),
                Err(FilterError::TooLong)
            );
            assert_eq!(
                validate(&filter("2026-03-02", "2026-03-01")),
                Err(FilterError::Reversed)
            );
            assert_eq!(
                validate(&filter("", "2026-03-01")),
                Err(FilterError::InvalidFrom)
            );
            assert_eq!(
                validate(&filter("2026-03-01", "2026-3-9")),
                Err(FilterError::InvalidTo)
            );
            let mut bad_department = filter("2026-03-01", "2026-03-31");
            bad_department.department = "not-an-id".into();
            assert_eq!(
                validate(&bad_department),
                Err(FilterError::InvalidDepartment)
            );
            for issue in [
                FilterError::InvalidFrom,
                FilterError::InvalidTo,
                FilterError::Reversed,
                FilterError::TooLong,
                FilterError::InvalidDepartment,
            ] {
                assert!(!issue.message().is_empty());
            }
        }

        #[test]
        fn list_path_carries_every_condition() {
            let department = DepartmentId::new();
            let member = MemberId::new();
            let full = Filter {
                department: department.to_string(),
                member: Some(member),
                ..filter("2026-03-01", "2026-03-31")
            };
            assert_eq!(
                list_path(&full, Some("7b2261")),
                Ok(format!(
                    "/admin/attendance?from=2026-03-01&to=2026-03-31&limit=50\
                     &department_id={department}&member_id={member}&cursor=7b2261"
                ))
            );
            assert_eq!(
                list_path(&filter("2026-03-01", "2026-03-31"), Some("")),
                Ok("/admin/attendance?from=2026-03-01&to=2026-03-31&limit=50".into())
            );
            assert_eq!(
                list_path(&filter("x", "2026-03-31"), None),
                Err(FilterError::InvalidFrom)
            );
        }

        #[test]
        fn cursor_and_query_values_are_escaped() {
            assert_eq!(escape("abc-_.~09"), "abc-_.~09");
            assert_eq!(escape("a&b=c d"), "a%26b%3Dc%20d");
            assert_eq!(escape("가"), "%EA%B0%80");
            let path =
                list_path(&filter("2026-03-01", "2026-03-31"), Some("x&admin=1")).expect("path");
            assert!(path.ends_with("&cursor=x%26admin%3D1"), "{path}");
        }

        #[test]
        fn export_href_uses_only_the_range() {
            let with_member = Filter {
                member: Some(MemberId::new()),
                ..filter("2026-03-01", "2026-03-31")
            };
            assert_eq!(
                export_href(&with_member),
                Ok("/api/v1/admin/attendance/export.csv?from=2026-03-01&to=2026-03-31".into())
            );
            assert_eq!(
                export_href(&filter("2026-04-01", "2026-03-01")),
                Err(FilterError::Reversed)
            );
        }

        #[test]
        fn options_lists() {
            assert_eq!(
                department_options(&[]),
                vec![(String::new(), "전체 부서".into())]
            );
            let work = work_type_options();
            assert_eq!(work.len(), WorkType::ALL.len());
            for (value, label) in &work {
                assert!(WorkType::parse(value).is_ok());
                assert!(!label.is_empty());
            }
        }

        #[test]
        fn create_request_converts_company_wall_clock() {
            let tz = seoul();
            let form = form();
            let request = create_request(&form, tz).expect("valid");
            assert_eq!(request.clock_in_at, ts("2026-03-02T00:00:00Z"));
            assert_eq!(request.clock_out_at, Some(ts("2026-03-02T09:00:00Z")));
            assert_eq!(request.work_date, "2026-03-02");
            assert_eq!(request.note, None);
            assert_eq!(request.reason, "누락 보정");
            assert_eq!(request.work_type, WorkType::Field);
            assert_eq!(Some(request.member_id), form.member);
        }

        #[test]
        fn create_request_rejects_bad_input() {
            let tz = seoul();
            let check = |change: fn(&mut RecordForm), expected: FormIssue| {
                let mut f = form();
                change(&mut f);
                assert_eq!(create_request(&f, tz), Err(expected));
            };
            check(|f| f.member = None, FormIssue::MemberMissing);
            check(
                |f| f.work_date = "2026-02-30".into(),
                FormIssue::WorkDateInvalid,
            );
            check(|f| f.clock_in = String::new(), FormIssue::ClockInMissing);
            check(
                |f| f.clock_in = "2026-03-02 09:00".into(),
                FormIssue::ClockInInvalid,
            );
            check(|f| f.clock_out = "18:00".into(), FormIssue::ClockOutInvalid);
            check(
                |f| f.clock_out = "2026-03-02T09:00".into(),
                FormIssue::ClockOutNotAfterClockIn,
            );
            check(
                |f| f.clock_out = "2026-03-02T08:59".into(),
                FormIssue::ClockOutNotAfterClockIn,
            );
            check(
                |f| f.clock_out = "2026-03-03T09:01".into(),
                FormIssue::ShiftTooLong,
            );
            check(|f| f.reason = "   ".into(), FormIssue::ReasonMissing);
            check(|f| f.reason = "가".repeat(1_001), FormIssue::ReasonTooLong);
            check(|f| f.note = "가".repeat(1_001), FormIssue::NoteTooLong);
        }

        #[test]
        fn shift_of_exactly_24_hours_is_allowed_and_clock_out_is_optional() {
            let tz = seoul();
            let mut f = form();
            f.clock_out = "2026-03-03T09:00".into();
            assert!(create_request(&f, tz).is_ok());
            f.clock_out = "  ".into();
            assert_eq!(create_request(&f, tz).map(|r| r.clock_out_at), Ok(None));
            f.reason = "가".repeat(1_000);
            assert!(create_request(&f, tz).is_ok());
        }

        #[test]
        fn dst_gap_is_rejected_in_non_korean_zone() {
            let new_york = format::zone("America/New_York");
            let mut f = form();
            f.work_date = "2026-03-08".into();
            f.clock_in = "2026-03-08T02:30".into();
            f.clock_out = String::new();
            assert_eq!(
                create_request(&f, new_york),
                Err(FormIssue::ClockInNonexistent)
            );
            f.clock_in = "2026-03-08T01:30".into();
            f.clock_out = "2026-03-08T02:15".into();
            assert_eq!(
                create_request(&f, new_york),
                Err(FormIssue::ClockOutNonexistent)
            );
            // 01:30 EST → 03:30 EDT는 실제로 1시간이다.
            f.clock_out = "2026-03-08T03:30".into();
            let request = create_request(&f, new_york).expect("valid");
            assert_eq!(
                request
                    .clock_out_at
                    .map(|out| out.since(request.clock_in_at).num_minutes()),
                Some(60)
            );
        }

        #[test]
        fn edit_round_trips_a_record() {
            let tz = seoul();
            let original = record(
                "2026-03-02",
                "2026-03-02T00:05:00Z",
                Some("2026-03-02T17:10:00Z"),
            );
            let mut f = RecordForm::from_record(&original, tz);
            assert_eq!(f.clock_in, "2026-03-02T09:05");
            assert_eq!(f.clock_out, "2026-03-03T02:10");
            assert_eq!(f.reason, "");
            assert_eq!(edit_request(&f, 3, tz), Err(FormIssue::ReasonMissing));
            f.reason = "퇴근 시각 정정".into();
            let request = edit_request(&f, 3, tz).expect("valid");
            assert_eq!(request.version, 3);
            assert_eq!(request.clock_in_at, original.clock_in_at);
            assert_eq!(request.clock_out_at, original.clock_out_at);
            assert_eq!(request.work_type, original.work_type);

            let open = record("2026-03-02", "2026-03-02T00:05:00Z", None);
            assert_eq!(RecordForm::from_record(&open, tz).clock_out, "");
        }

        #[test]
        fn every_form_issue_has_a_message() {
            for issue in [
                FormIssue::MemberMissing,
                FormIssue::WorkDateInvalid,
                FormIssue::ClockInMissing,
                FormIssue::ClockInInvalid,
                FormIssue::ClockInNonexistent,
                FormIssue::ClockOutInvalid,
                FormIssue::ClockOutNonexistent,
                FormIssue::ClockOutNotAfterClockIn,
                FormIssue::ShiftTooLong,
                FormIssue::ReasonMissing,
                FormIssue::ReasonTooLong,
                FormIssue::NoteTooLong,
            ] {
                assert!(!issue.message().is_empty(), "{issue:?}");
            }
        }

        #[test]
        fn failures_are_routed() {
            assert_eq!(
                failure_action("validation_failed"),
                FailureAction::ShowInForm
            );
            assert_eq!(failure_action("already_exists"), FailureAction::ShowInForm);
            assert_eq!(failure_action("version_conflict"), FailureAction::Reload);
            assert_eq!(failure_action("not_found"), FailureAction::Reload);
            assert_eq!(failure_action("forbidden"), FailureAction::Toast);
            assert_eq!(failure_action("unauthenticated"), FailureAction::Toast);
            assert_eq!(failure_action(""), FailureAction::Toast);
        }

        #[test]
        fn admin_rows_describe_member_and_record() {
            let item = AdminAttendanceItem {
                record: record(
                    "2026-03-02",
                    "2026-03-02T00:20:00Z",
                    Some("2026-03-02T09:30:00Z"),
                ),
                member_name: "홍길동".into(),
                login_id: "hong".into(),
                department_name: Some("개발팀".into()),
                evaluation: evaluation(DayStatus::Late, 490),
            };
            let row = admin_row(&item, seoul());
            assert_eq!(row.title, "홍길동 · 3월 2일 (월)");
            assert_eq!(row.member_meta, "hong · 개발팀");
            assert_eq!(row.cells.clock_in, "09:20");
            assert_eq!(row.cells.status, "지각");
            assert_eq!(row.date_label, format::date_md(date("2026-03-02")));
            let no_department = AdminAttendanceItem {
                department_name: None,
                ..item
            };
            assert_eq!(admin_row(&no_department, seoul()).member_meta, "hong");
        }
    }
}
