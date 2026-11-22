//! 근태 관리(관리자): 기간·부서·구성원으로 기록을 찾고, 추가·수정하고, CSV로 내보낸다.

use dioxus::prelude::*;
use pango_api::approval::DocumentSummary;
use pango_api::attendance::{AdminAttendanceItem, AttendanceRecordDto};
use pango_api::org::{DepartmentDto, MemberSummary};
use pango_api::Page;
use pango_core::attendance::WorkType;
use pango_core::id::AttendanceRecordId;

use crate::api;
use crate::calendar::YearMonth;
use crate::company::use_company;
use crate::format;
use crate::org_tree;
use crate::pages::admin::logic::can_edit_personal_records;
use crate::pages::approval::DocumentRow;
use crate::pages::attendance::RecordHistory;
use crate::route::Route;
use crate::session::use_session;
use crate::text;
use crate::ui::{
    loaded_or_empty, now, report_error, report_success, resource_view, use_initial_tab,
    use_more_pages, Badge, Button, ButtonKind, Card, DownloadLink, EmptyState, FormError, Icon,
    IconName, LoadMore, MemberPicker, Meta, Modal, PageHeader, PickedMember, SelectField, Tabs,
    TextArea, TextField,
};

use logic::{AdminRow, FailureAction, Filter};

/// 열려 있는 기록 편집 창.
#[derive(Debug, Clone, PartialEq)]
enum Editor {
    Create,
    Edit(AdminAttendanceItem),
}

/// 출퇴근 기록 탭.
const TAB_RECORDS: &str = "records";
/// 정정 요청 탭. 관리 홈의 "근태 정정" 타일이 이 탭으로 연다.
pub(crate) const TAB_CORRECTIONS: &str = "corrections";

#[component]
pub fn AdminAttendance() -> Element {
    let session = use_session();
    let tz = session.zone();
    let me = session.me().map(|me| (me.id, me.role));
    let this_month = YearMonth::of(use_company().work_today(tz));
    let mut tab = use_initial_tab(Route::AdminAttendance {}, TAB_RECORDS);
    let mut from = use_signal(move || this_month.first_day().to_string());
    let mut to = use_signal(move || this_month.last_day().to_string());
    let mut department = use_signal(String::new);
    let mut member = use_signal(|| None::<MemberSummary>);
    let more = use_more_pages::<AdminAttendanceItem>();
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
    // 첫 페이지와 그 요청 경로. 조건이 틀리면 읽지 않는다(`None`). 뒤 페이지는 `more`가 붙인다.
    let mut list = use_resource(move || async move {
        let requested = filter();
        more.reset();
        let Ok(path) = logic::list_path(&requested, None) else {
            return Ok(None);
        };
        let page = api::get::<Page<AdminAttendanceItem>>(&path).await?;
        Ok(Some((path, page)))
    });

    let current = filter();
    let filter_error = logic::validate(&current)
        .err()
        .map(|issue| issue.message().to_owned())
        .unwrap_or_default();
    let export_path = logic::export_path(&current).ok();
    let department_options =
        org_tree::department_options(&loaded_or_empty(&departments), "전체 부서");
    let tabs = vec![
        (TAB_RECORDS.to_owned(), "출퇴근 기록".to_owned(), None),
        (TAB_CORRECTIONS.to_owned(), "정정 요청".to_owned(), None),
    ];

    rsx! {
        PageHeader {
            title: "근태 관리",
            description: "기록을 고치면 사유와 함께 이력이 남아요.",
            actions: rsx! {
                if let Some(path) = export_path {
                    DownloadLink { class: "btn btn-weak", path, "CSV 내보내기" }
                } else {
                    Button { kind: ButtonKind::Weak, disabled: true, "CSV 내보내기" }
                }
                Button { onclick: move |_| editor.set(Some(Editor::Create)),
                    Icon { name: IconName::Plus }
                    "기록 추가"
                }
            },
        }
        Tabs { tabs, active: tab(), onselect: move |key| tab.set(key) }
        if tab() == TAB_CORRECTIONS {
            CorrectionRequests {}
        } else {
            Card {
                div { class: "toolbar",
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
                        PickedMemberField { label: "구성원", member: chosen, onclear: move |_| member.set(None) }
                    } else {
                        MemberPicker { label: "구성원", onpick: move |picked| member.set(Some(picked)) }
                    }
                }
                FormError { message: filter_error }
                p { class: "field-hint panel-note", "CSV에는 기간만 적용되고 부서·구성원 조건은 적용되지 않아요." }
            {resource_view(list, move |loaded: Option<(String, Page<AdminAttendanceItem>)>| {
                let Some((base, page)) = loaded else {
                    return rsx! {};
                };
                let next = more.cursor(&page);
                let rows: Vec<AdminRow> = more
                    .all(page.items, |item| item.record.id)
                    .iter()
                    .map(|item| logic::admin_row(item, tz))
                    .collect();
                if rows.is_empty() {
                    return rsx! { EmptyState { title: "조건에 맞는 기록이 없어요.", icon: IconName::Clock } };
                }
                rsx! {
                    Fragment {
                        div { class: "table-wrap only-desktop",
                            table { class: "table table-nowrap",
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
                                                span { class: "cell-sub", Meta { items: row.member_meta.clone() } }
                                            }
                                            td { "{row.cells.work_type}" }
                                            td { "{row.cells.clock_in}" }
                                            td { "{row.cells.clock_out}" }
                                            td { class: "num", "{row.cells.worked}" }
                                            td {
                                                Badge { tone: row.cells.tone, "{row.cells.status}" }
                                            }
                                            td {
                                                div { class: "row-actions",
                                                    RowActions {
                                                        editable: me.is_none_or(|(id, role)| can_edit_personal_records(id, role, row.item.record.member_id)),
                                                        row: row.clone(),
                                                        editor,
                                                        history,
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
                                li { key: "{row.item.record.id}", class: "list-row list-row-top",
                                    div { class: "list-row-main",
                                        strong { "{row.item.member_name}" }
                                        Meta {
                                            items: vec![
                                                row.date_label.clone(),
                                                format!("{} – {}", row.cells.clock_in, row.cells.clock_out),
                                                row.cells.work_type.clone(),
                                            ],
                                        }
                                        Meta { items: row.member_meta.clone() }
                                    }
                                    div { class: "list-row-side",
                                        Badge { tone: row.cells.tone, "{row.cells.status}" }
                                        div { class: "row-actions",
                                            RowActions {
                                                editable: me.is_none_or(|(id, role)| can_edit_personal_records(id, role, row.item.record.member_id)),
                                                row: row.clone(),
                                                editor,
                                                history,
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        LoadMore {
                            has_more: next.is_some(),
                            busy: more.busy(),
                            onclick: move |_| {
                                if let Some(cursor) = next.clone() {
                                    let base = base.clone();
                                    spawn(more.load(cursor, move |c| api::with_cursor(base.clone(), Some(c))));
                                }
                            },
                        }
                    }
                }
            })}
            }
        }
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

/// 결재 중인 근태 정정 요청(결재 문서). 승인·반려는 결재 문서에서 한다.
#[component]
fn CorrectionRequests() -> Element {
    let tz = use_session().zone();
    let more = use_more_pages::<DocumentSummary>();
    let first = use_resource(move || async move {
        more.reset();
        api::get::<Page<DocumentSummary>>(&logic::corrections_path(None)).await
    });
    let at = now();
    rsx! {
        Card {
            p { class: "field-hint panel-note", "결재 중인 정정 요청이에요. 결재 문서에서 내용을 보고 처리해요. 승인되면 기록에 반영돼요." }
            {resource_view(first, move |page: Page<DocumentSummary>| {
                let next = more.cursor(&page);
                let items = more.all(page.items, |doc| doc.id);
                if items.is_empty() {
                    return rsx! {
                        EmptyState { title: "결재 중인 정정 요청이 없어요.", icon: IconName::Clock }
                    };
                }
                rsx! {
                    div { class: "list",
                        for doc in items {
                            DocumentRow { key: "{doc.id}", doc, now: at, tz }
                        }
                    }
                    LoadMore {
                        has_more: next.is_some(),
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
    }
}

/// 표·목록 한 줄의 "수정"·"이력" 버튼. 고칠 수 없는 기록(관리자 본인의 기록)은 수정 대신 안내를 보인다.
#[component]
fn RowActions(
    row: AdminRow,
    editable: bool,
    editor: Signal<Option<Editor>>,
    history: Signal<Option<(AttendanceRecordId, String)>>,
) -> Element {
    let item = row.item.clone();
    let target = (row.item.record.id, row.title.clone());
    rsx! {
        if editable {
            Button {
                kind: ButtonKind::Grey,
                small: true,
                onclick: move |_| editor.set(Some(Editor::Edit(item.clone()))),
                "수정"
                span { class: "sr-only", " {row.title}" }
            }
        } else {
            small { class: "field-hint", {text::OWN_RECORD_HINT} }
        }
        Button {
            kind: ButtonKind::Grey,
            small: true,
            onclick: move |_| history.set(Some(target.clone())),
            "이력"
            span { class: "sr-only", " {row.title}" }
        }
    }
}

/// 라벨이 붙은 고른 구성원 표시 + 다시 고르기.
#[component]
fn PickedMemberField(
    #[props(into)] label: String,
    member: MemberSummary,
    onclear: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "field",
            span { class: "field-label", "{label}" }
            PickedMember {
                name: member.name,
                clear_label: format!("{label} 다시 고르기"),
                onclear: move |_| onclear.call(()),
            }
        }
    }
}

/// 기록 추가·수정 창. 시각은 회사 시간대의 벽시계로 입력받는다.
#[component]
fn RecordEditor(editor: Editor, onclose: EventHandler<()>, onsaved: EventHandler<()>) -> Element {
    let session = use_session();
    let tz = session.zone();
    let work_today = use_company().work_today(tz);
    let zone_name = session.me().map(|me| me.timezone).unwrap_or_default();
    let me = session.me().map(|me| (me.id, me.role));
    let initial = match &editor {
        Editor::Create => logic::RecordForm::blank(work_today),
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

    // 관리자는 본인 기록을 직접 추가할 수 없다(서버도 403). 다른 관리자나 최고 관리자가 한다.
    let own_record = form.read().member.is_some_and(|target| {
        me.is_some_and(|(id, role)| !can_edit_personal_records(id, role, target))
    });

    let submit = move |event: FormEvent| {
        let editor = editor.clone();
        async move {
            event.prevent_default();
            if busy() || own_record {
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
                        "기록을 추가했어요."
                    } else {
                        "기록을 수정했어요."
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
                        PickedMemberField {
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
                    if own_record {
                        p { class: "callout callout-info", role: "note", {text::OWN_RECORD_HINT} }
                    }
                    TextField {
                        label: "근무일",
                        kind: "date",
                        value: current.work_date.clone(),
                        required: true,
                        hint: "출근 시각이 이 근무일에 속해야 해요.",
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
                        hint: "비워 두면 퇴근 전으로 남아요.",
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
                div { class: "modal-actions",
                    Button { kind: ButtonKind::Grey, onclick: move |_| onclose.call(()), "취소" }
                    Button { submit: true, busy: busy(), disabled: own_record, "저장" }
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
    use pango_core::approval::{DocumentKind, DocumentStatus};
    use pango_core::attendance::WorkType;
    use pango_core::calendar::{parse_date, DateRange};
    use pango_core::id::{DepartmentId, MemberId};

    use crate::format;
    use crate::pages::approval::logic::admin_documents_path;
    use crate::pages::attendance::logic::{parse_reason, parse_times, record_cells, RecordCells};
    pub use crate::pages::attendance::logic::{work_type_options, FormIssue, TEXT_MAX_CHARS};

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
                Self::Reversed => "종료일이 시작일보다 빨라요.",
                Self::TooLong => "기간은 최대 366일까지 조회할 수 있어요.",
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
        let department = crate::format::non_empty(&filter.department)
            .map(|raw| {
                raw.parse::<DepartmentId>()
                    .map_err(|_| FilterError::InvalidDepartment)
            })
            .transpose()?;
        Ok(ValidFilter {
            range,
            department,
            member: filter.member,
        })
    }

    fn range_query(range: DateRange) -> String {
        format!("from={}&to={}", range.start(), range.end())
    }

    /// 결재 중인 근태 정정 문서 목록 경로.
    pub fn corrections_path(cursor: Option<&str>) -> String {
        admin_documents_path(
            Some(DocumentStatus::InProgress),
            Some(DocumentKind::AttendanceCorrection),
            cursor,
        )
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
        Ok(crate::api::with_cursor(path, cursor))
    }

    /// CSV 내려받기 경로(API 접두사 제외). 웹은 일반 링크로, 앱은 서명 링크로 연다.
    pub fn export_path(filter: &Filter) -> Result<String, FilterError> {
        let valid = validate(filter)?;
        Ok(format!(
            "/admin/attendance/export.csv?{}",
            range_query(valid.range)
        ))
    }

    /// 관리 목록 한 줄.
    #[derive(Debug, Clone, PartialEq)]
    pub struct AdminRow {
        pub item: AdminAttendanceItem,
        /// 이력 창 제목 등에 쓰는 `홍길동 · 3월 2일 (월)`.
        pub title: String,
        pub date_label: String,
        /// 아이디, (있으면) 부서.
        pub member_meta: Vec<String>,
        pub cells: RecordCells,
    }

    pub fn admin_row(item: &AdminAttendanceItem, tz: Tz) -> AdminRow {
        let record = &item.record;
        let date_label = format::date_md(record.work_date);
        let member_meta = std::iter::once(item.login_id.clone())
            .chain(item.department_name.clone())
            .collect::<Vec<_>>();
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

    pub fn create_request(
        form: &RecordForm,
        tz: Tz,
    ) -> Result<AdminCreateRecordRequest, FormIssue> {
        let member_id = form.member.ok_or(FormIssue::MemberMissing)?;
        let work_date = parse_date("work_date", form.work_date.trim())
            .map_err(|_| FormIssue::WorkDateInvalid)?;
        let (clock_in_at, clock_out_at) = parse_times(&form.clock_in, &form.clock_out, tz)?;
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
            reason: parse_reason(&form.reason)?,
        })
    }

    pub fn edit_request(
        form: &RecordForm,
        version: i64,
        tz: Tz,
    ) -> Result<AdminEditRecordRequest, FormIssue> {
        let (clock_in_at, clock_out_at) = parse_times(&form.clock_in, &form.clock_out, tz)?;
        Ok(AdminEditRecordRequest {
            version,
            clock_in_at,
            clock_out_at,
            work_type: form.work_type,
            reason: parse_reason(&form.reason)?,
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
            let path =
                list_path(&filter("2026-03-01", "2026-03-31"), Some("x&admin=1")).expect("path");
            assert!(path.ends_with("&cursor=x%26admin%3D1"), "{path}");
        }

        #[test]
        fn export_path_uses_only_the_range() {
            let with_member = Filter {
                member: Some(MemberId::new()),
                ..filter("2026-03-01", "2026-03-31")
            };
            assert_eq!(
                export_path(&with_member),
                Ok("/admin/attendance/export.csv?from=2026-03-01&to=2026-03-31".into())
            );
            assert_eq!(
                export_path(&filter("2026-04-01", "2026-03-01")),
                Err(FilterError::Reversed)
            );
        }

        #[test]
        fn options_lists() {
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
            assert_eq!(row.member_meta, vec!["hong", "개발팀"]);
            assert_eq!(row.cells.clock_in, "09:20");
            assert_eq!(row.cells.status, "지각");
            assert_eq!(row.date_label, format::date_md(date("2026-03-02")));
            let no_department = AdminAttendanceItem {
                department_name: None,
                ..item
            };
            assert_eq!(admin_row(&no_department, seoul()).member_meta, vec!["hong"]);
        }
    }
}
