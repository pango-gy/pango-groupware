//! 공지 관리: 작성·수정·보관과 읽음 현황.

use dioxus::prelude::*;
use pango_api::notice::{NoticeDetail, NoticeReadReport, NoticeSummary};
use pango_api::org::DepartmentDto;
use pango_api::{Page, Versioned};
use pango_core::authz::AudienceKind;
use pango_core::id::NoticeId;
use pango_core::text::{Body, Title};

use crate::api;
use crate::badges::refresh_badges;
use crate::format;
use crate::session::use_session;
use crate::ui::{
    report_error, report_success, resource_view, use_more_pages, Badge, Button, ButtonKind, Card,
    Checkbox, ConfirmModal, EmptyState, FormError, IconName, LoadMore, Modal, PageHeader,
    SelectField, TextArea, TextField, Tone,
};

use logic::{build_input, count_label, NoticeForm, LIST_PATH};

#[derive(Debug, Clone, PartialEq)]
enum Dialog {
    Closed,
    Create,
    Edit(NoticeSummary),
    Archive(NoticeSummary),
    Reads(NoticeSummary),
}

#[component]
pub fn AdminNotices() -> Element {
    let session = use_session();
    let mut dialog = use_signal(|| Dialog::Closed);
    let mut archiving = use_signal(|| false);
    let more = use_more_pages::<NoticeSummary>();
    let mut list = use_resource(move || async move {
        more.reset();
        api::get::<Page<NoticeSummary>>(LIST_PATH).await
    });
    let departments =
        use_resource(|| async { api::get::<Vec<DepartmentDto>>("/departments").await });
    let tz = format::zone(&session.me().map(|me| me.timezone).unwrap_or_default());

    let department_options: Vec<(String, String)> = match &*departments.read() {
        Some(Ok(items)) => items
            .iter()
            .map(|d| (d.id.to_string(), d.name.clone()))
            .collect(),
        _ => Vec::new(),
    };

    let saved = move |_| {
        dialog.set(Dialog::Closed);
        list.restart();
        refresh_badges();
    };
    let archive = move |notice: NoticeSummary| async move {
        if archiving() {
            return;
        }
        archiving.set(true);
        let path = format!("/admin/notices/{}?version={}", notice.id, notice.version);
        match api::delete::<NoticeDetail>(&path).await {
            Ok(_) => {
                report_success("공지를 보관했습니다.");
                dialog.set(Dialog::Closed);
                list.restart();
                refresh_badges();
            }
            Err(error) => {
                report_error(&error);
                if error.code == "version_conflict" {
                    dialog.set(Dialog::Closed);
                    list.restart();
                }
            }
        }
        archiving.set(false);
    };

    rsx! {
        PageHeader {
            title: "공지 관리",
            actions: rsx! {
                Button { onclick: move |_| dialog.set(Dialog::Create), "새 공지" }
            },
        }
        {resource_view(list, move |page: Page<NoticeSummary>| {
            let cursor = more.cursor(&page);
            let items: Vec<NoticeSummary> = page.items.into_iter().chain(more.items()).collect();
            rsx! {
                if items.is_empty() {
                    EmptyState { title: "작성한 공지가 없습니다.", icon: IconName::Megaphone }
                } else {
                    Card {
                        ul { class: "list",
                            for notice in items {
                                li { key: "{notice.id}", class: "list-row admin-notice-row",
                                    div { class: "list-row-main",
                                        span { class: "notice-title",
                                            if notice.archived_at.is_some() {
                                                Badge { tone: Tone::Neutral, "보관됨" }
                                            }
                                            if notice.pinned {
                                                Badge { tone: Tone::Info, "고정" }
                                            }
                                            if notice.must_read {
                                                Badge { tone: Tone::Danger, "필독" }
                                            }
                                            strong { "{notice.title}" }
                                        }
                                        small {
                                            "{notice.author_name} · {format::datetime(notice.published_at, tz)} · "
                                            {crate::pages::notices::logic::audience_text(&notice)}
                                        }
                                    }
                                    div { class: "row-actions",
                                        Button {
                                            kind: ButtonKind::Secondary,
                                            small: true,
                                            onclick: {
                                                let notice = notice.clone();
                                                move |_| dialog.set(Dialog::Reads(notice.clone()))
                                            },
                                            "읽음 현황"
                                        }
                                        if notice.archived_at.is_none() {
                                            Button {
                                                kind: ButtonKind::Secondary,
                                                small: true,
                                                onclick: {
                                                    let notice = notice.clone();
                                                    move |_| dialog.set(Dialog::Edit(notice.clone()))
                                                },
                                                "수정"
                                            }
                                            Button {
                                                kind: ButtonKind::Ghost,
                                                small: true,
                                                onclick: {
                                                    let notice = notice.clone();
                                                    move |_| dialog.set(Dialog::Archive(notice.clone()))
                                                },
                                                "보관"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(cursor) = cursor {
                            LoadMore {
                                busy: more.busy(),
                                onclick: move |_| {
                                    more.load(cursor.clone(), |c| format!("{LIST_PATH}&cursor={c}"))
                                },
                            }
                        }
                    }
                }
            }
        })}
        match dialog() {
            Dialog::Closed => rsx! {},
            Dialog::Create => rsx! {
                Modal { title: "새 공지", onclose: move |_| dialog.set(Dialog::Closed),
                    NoticeEditor {
                        initial: NoticeForm::default(),
                        target: None,
                        departments: department_options,
                        onclose: move |_| dialog.set(Dialog::Closed),
                        onsaved: saved,
                    }
                }
            },
            Dialog::Edit(notice) => rsx! {
                Modal { title: "공지 수정", onclose: move |_| dialog.set(Dialog::Closed),
                    EditLoader {
                        id: notice.id,
                        departments: department_options,
                        onclose: move |_| dialog.set(Dialog::Closed),
                        onsaved: saved,
                    }
                }
            },
            Dialog::Archive(notice) => rsx! {
                ConfirmModal {
                    title: "공지 보관",
                    message: format!(
                        "‘{}’ 공지를 보관합니다. 구성원 목록에서 사라지고 다시 게시할 수 없습니다.",
                        notice.title
                    ),
                    confirm_label: "보관",
                    danger: true,
                    busy: archiving(),
                    onconfirm: move |_| archive(notice.clone()),
                    oncancel: move |_| dialog.set(Dialog::Closed),
                }
            },
            Dialog::Reads(notice) => rsx! {
                Modal { title: "읽음 현황", onclose: move |_| dialog.set(Dialog::Closed),
                    ReadReport { id: notice.id, title: notice.title.clone() }
                }
            },
        }
    }
}

/// 수정할 공지의 본문을 읽어 와 편집기를 연다(목록에는 본문이 없다).
#[component]
fn EditLoader(
    id: NoticeId,
    departments: Vec<(String, String)>,
    onclose: EventHandler<()>,
    onsaved: EventHandler<()>,
) -> Element {
    let detail =
        use_resource(
            move || async move { api::get::<NoticeDetail>(&format!("/notices/{id}")).await },
        );
    resource_view(detail, move |detail: NoticeDetail| {
        rsx! {
            NoticeEditor {
                key: "{detail.summary.version}",
                initial: NoticeForm::from_detail(&detail),
                target: Some((detail.summary.id, detail.summary.version)),
                departments,
                onclose,
                onsaved,
            }
        }
    })
}

#[component]
fn NoticeEditor(
    initial: NoticeForm,
    /// 수정이면 `(ID, 읽어 온 버전)`, 새 공지면 `None`.
    target: Option<(NoticeId, i64)>,
    departments: Vec<(String, String)>,
    onclose: EventHandler<()>,
    onsaved: EventHandler<()>,
) -> Element {
    let mut session = use_session();
    let mut form = use_signal(|| initial.clone());
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    // 편집기를 열 때 한 번 만든다. 두 번 눌러도 공지는 하나만 생긴다.
    let key = use_signal(api::new_idempotency_key);

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let input = match build_input(&form.read()) {
            Ok(input) => input,
            Err(message) => {
                error.set(message);
                return;
            }
        };
        busy.set(true);
        error.set(String::new());
        let result = match target {
            Some((id, version)) => {
                let request = Versioned {
                    version,
                    data: input,
                };
                api::patch::<_, NoticeDetail>(&format!("/admin/notices/{id}"), &request).await
            }
            None => api::post_once::<_, NoticeDetail>("/admin/notices", &input, &key()).await,
        };
        busy.set(false);
        match result {
            Ok(_) => {
                report_success(if target.is_some() {
                    "공지를 수정했습니다."
                } else {
                    "공지를 게시했습니다."
                });
                onsaved.call(());
            }
            Err(failure) if failure.code == "version_conflict" => {
                // 남이 먼저 고쳤다. 안내하고 목록을 다시 불러온다.
                report_error(&failure);
                onsaved.call(());
            }
            Err(failure) => error.set(session.handle(&failure)),
        }
    };

    let current = form.read().clone();
    let mut department_options = vec![(String::new(), "부서 선택".to_owned())];
    department_options.extend(departments);
    let audience_options = vec![
        (
            AudienceKind::All.as_str().to_owned(),
            "전체 구성원".to_owned(),
        ),
        (
            AudienceKind::Department.as_str().to_owned(),
            "특정 부서".to_owned(),
        ),
    ];
    let (body_count, body_over) = count_label(&current.body, Body::RULE.max_chars);

    rsx! {
        form { class: "form", onsubmit: submit,
            FormError { message: error() }
            TextField {
                label: "제목",
                value: current.title.clone(),
                required: true,
                maxlength: Title::RULE.max_chars as i64,
                oninput: move |value| form.write().title = value,
            }
            div { class: "field-with-count",
                TextArea {
                    label: "내용",
                    value: current.body.clone(),
                    rows: 10,
                    required: true,
                    maxlength: Body::RULE.max_chars as i64,
                    oninput: move |value| form.write().body = value,
                }
                span {
                    class: if body_over { "char-count char-count-over" } else { "char-count" },
                    "aria-live": "polite",
                    "{body_count}"
                }
            }
            div { class: "form-row",
                SelectField {
                    label: "대상",
                    value: current.audience.as_str(),
                    options: audience_options,
                    onchange: move |value: String| {
                        if let Ok(kind) = AudienceKind::parse(&value) {
                            form.write().audience = kind;
                        }
                    },
                }
                if current.audience == AudienceKind::Department {
                    SelectField {
                        label: "대상 부서",
                        value: current.department.clone(),
                        options: department_options,
                        required: true,
                        onchange: move |value| form.write().department = value,
                    }
                }
            }
            if current.audience == AudienceKind::Department {
                p { class: "field-hint", "하위 부서 구성원도 함께 받습니다." }
            }
            Checkbox {
                label: "목록 위에 고정",
                checked: current.pinned,
                onchange: move |value| form.write().pinned = value,
            }
            Checkbox {
                label: "필독",
                hint: "구성원에게 읽음 확인을 받습니다.",
                checked: current.must_read,
                onchange: move |value| form.write().must_read = value,
            }
            div { class: "card-actions",
                Button { kind: ButtonKind::Secondary, onclick: move |_| onclose.call(()), "취소" }
                Button { submit: true, busy: busy(),
                    if target.is_some() { "저장" } else { "게시" }
                }
            }
        }
    }
}

#[component]
fn ReadReport(id: NoticeId, title: String) -> Element {
    let session = use_session();
    let report = use_resource(move || async move {
        api::get::<NoticeReadReport>(&format!("/admin/notices/{id}/reads")).await
    });
    let tz = format::zone(&session.me().map(|me| me.timezone).unwrap_or_default());
    rsx! {
        p { class: "read-report-title", "{title}" }
        {resource_view(report, move |report: NoticeReadReport| rsx! {
            section { class: "read-report",
                h3 { class: "read-report-heading", "읽음 {report.read.len()}명" }
                if report.read.is_empty() {
                    p { class: "field-hint", "아직 읽은 사람이 없습니다." }
                }
                ul { class: "list",
                    for reader in report.read {
                        li { key: "{reader.member_id}", class: "list-row",
                            span { class: "list-row-main", strong { "{reader.name}" } }
                            span { class: "list-row-side",
                                {reader.read_at.map(|at| format::datetime(at, tz)).unwrap_or_default()}
                            }
                        }
                    }
                }
            }
            section { class: "read-report",
                h3 { class: "read-report-heading", "안 읽음 {report.unread.len()}명" }
                if report.unread.is_empty() {
                    p { class: "field-hint", "대상자가 모두 읽었습니다." }
                }
                ul { class: "list",
                    for reader in report.unread {
                        li { key: "{reader.member_id}", class: "list-row",
                            span { class: "list-row-main", strong { "{reader.name}" } }
                        }
                    }
                }
            }
        })}
    }
}

pub mod logic {
    use pango_api::notice::{NoticeDetail, NoticeInput};
    use pango_core::authz::{Audience, AudienceKind};
    use pango_core::id::DepartmentId;
    use pango_core::text::{Body, Title};
    use pango_core::DomainError;

    use crate::text;

    pub const LIST_PATH: &str = "/admin/notices?limit=30";

    /// 편집 중인 공지. 대상 부서는 선택 상자의 값(문자열) 그대로 둔다.
    #[derive(Debug, Clone, PartialEq)]
    pub struct NoticeForm {
        pub title: String,
        pub body: String,
        pub pinned: bool,
        pub must_read: bool,
        pub audience: AudienceKind,
        pub department: String,
    }

    impl Default for NoticeForm {
        fn default() -> Self {
            Self {
                title: String::new(),
                body: String::new(),
                pinned: false,
                must_read: false,
                audience: AudienceKind::All,
                department: String::new(),
            }
        }
    }

    impl NoticeForm {
        pub fn from_detail(detail: &NoticeDetail) -> Self {
            Self {
                title: detail.summary.title.clone(),
                body: detail.body.clone(),
                pinned: detail.summary.pinned,
                must_read: detail.summary.must_read,
                audience: detail.summary.audience,
                department: detail
                    .summary
                    .department_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            }
        }
    }

    fn message(error: &DomainError) -> String {
        match error {
            DomainError::Validation { field, reason } => {
                text::error_message("validation_failed", Some(field), Some(reason.code()))
            }
            other => text::error_message(other.code(), None, None),
        }
    }

    /// 폼을 요청으로 바꾼다. 서버와 같은 규칙(제목·본문 길이, 대상과 부서의 짝)을 먼저 확인한다.
    ///
    /// 대상이 전체면 골라 둔 부서가 있어도 보내지 않고, 부서 대상이면 부서가 반드시 있어야 한다.
    pub fn build_input(form: &NoticeForm) -> Result<NoticeInput, String> {
        let title = Title::parse(&form.title).map_err(|e| message(&e))?;
        let body = Body::parse(&form.body).map_err(|e| message(&e))?;
        let department_id = match form.audience {
            AudienceKind::All => None,
            AudienceKind::Department => match form.department.trim() {
                "" => None,
                raw => Some(DepartmentId::parse(raw).map_err(|_| {
                    text::error_message("validation_failed", Some("department_id"), Some("empty"))
                })?),
            },
        };
        let audience = Audience::new(form.audience, department_id).map_err(|e| message(&e))?;
        Ok(NoticeInput {
            title: title.into_string(),
            body: body.into_string(),
            pinned: form.pinned,
            must_read: form.must_read,
            audience: audience.kind(),
            department_id: audience.department_id(),
        })
    }

    /// 글자 수 표시(`12 / 20000자`)와 한도를 넘었는지. 서버처럼 문자(코드 포인트) 단위로 센다.
    pub fn count_label(value: &str, max: usize) -> (String, bool) {
        let count = value.chars().count();
        (format!("{count} / {max}자"), count > max)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn filled() -> NoticeForm {
            NoticeForm {
                title: "  사내 공지  ".into(),
                body: "첫 줄\r\n둘째 줄".into(),
                ..NoticeForm::default()
            }
        }

        #[test]
        fn all_audience_never_sends_a_department() {
            let form = NoticeForm {
                department: DepartmentId::new().to_string(),
                ..filled()
            };
            let input = build_input(&form).expect("valid");
            assert_eq!(input.audience, AudienceKind::All);
            assert_eq!(input.department_id, None);
            assert_eq!(input.title, "사내 공지", "서버처럼 앞뒤 공백을 지운다");
            assert_eq!(input.body, "첫 줄\n둘째 줄");
        }

        #[test]
        fn department_audience_requires_a_valid_department() {
            let id = DepartmentId::new();
            let form = NoticeForm {
                audience: AudienceKind::Department,
                department: id.to_string(),
                ..filled()
            };
            let input = build_input(&form).expect("valid");
            assert_eq!(input.audience, AudienceKind::Department);
            assert_eq!(input.department_id, Some(id));

            for department in ["", "   ", "not-an-id", "'; DROP TABLE notices; --"] {
                let form = NoticeForm {
                    audience: AudienceKind::Department,
                    department: department.into(),
                    ..filled()
                };
                assert_eq!(
                    build_input(&form),
                    Err("대상 부서를 선택해 주세요.".into()),
                    "{department:?}"
                );
            }
        }

        #[test]
        fn title_and_body_follow_server_rules() {
            let empty_title = NoticeForm {
                title: "   ".into(),
                ..filled()
            };
            assert_eq!(
                build_input(&empty_title),
                Err("제목을(를) 입력해 주세요.".into())
            );
            let long_title = NoticeForm {
                title: "가".repeat(Title::RULE.max_chars + 1),
                ..filled()
            };
            assert!(build_input(&long_title).is_err());
            let exact_title = NoticeForm {
                title: "가".repeat(Title::RULE.max_chars),
                ..filled()
            };
            assert!(build_input(&exact_title).is_ok());
            let control = NoticeForm {
                title: "제목\u{0007}".into(),
                ..filled()
            };
            assert!(build_input(&control).is_err());
            let long_body = NoticeForm {
                body: "a".repeat(Body::RULE.max_chars + 1),
                ..filled()
            };
            assert_eq!(
                build_input(&long_body),
                Err("내용이(가) 너무 깁니다.".into())
            );
            let html = NoticeForm {
                body: "<img src=x onerror=alert(1)>".into(),
                ..filled()
            };
            assert_eq!(
                build_input(&html).expect("text is allowed").body,
                "<img src=x onerror=alert(1)>",
                "HTML은 글자 그대로 보낸다(화면은 텍스트로만 그린다)"
            );
        }

        #[test]
        fn char_count_uses_code_points() {
            assert_eq!(count_label("", 10), ("0 / 10자".into(), false));
            assert_eq!(count_label("가나다", 3), ("3 / 3자".into(), false));
            assert_eq!(count_label("😀😀😀😀", 3), ("4 / 3자".into(), true));
        }
    }
}
