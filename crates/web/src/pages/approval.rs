//! 결재함: 결재 대기·결재 예정·기안함·완료·참조 탭과 문서 목록.

use dioxus::prelude::*;
use pango_api::approval::{ApprovalCounts, DocumentSummary, FormGroup};
use pango_api::Page;
use pango_core::approval::{FormCategory, Mailbox};
use pango_core::time::Timestamp;

use crate::api;
use crate::format;
use crate::route::Route;
use crate::session::use_session;
use crate::text;
use crate::ui::{
    now, resource_view, use_more_pages, Badge, Card, EmptyState, IconName, LoadMore, Meta,
    PageHeader, SelectField, Tabs, Tone,
};

/// 결재 화면들이 함께 쓰는 순수 함수.
pub(crate) mod logic {
    use std::collections::BTreeMap;

    use pango_api::approval::{ApprovalCounts, DocumentSummary, FormGroup};
    use pango_core::approval::{DocumentKind, DocumentStatus, FormCategory, Mailbox};
    use pango_core::id::FormId;

    use crate::text;

    /// 문서함의 거르기 조건. 둘 다 없으면 전체다.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct DocFilter {
        pub category: Option<FormCategory>,
        pub form_id: Option<FormId>,
    }

    /// `GET /approval/documents` 경로.
    pub fn mailbox_path(mailbox: Mailbox, filter: DocFilter, cursor: Option<&str>) -> String {
        let mut path = format!("/approval/documents?box={}", mailbox.as_str());
        if let Some(category) = filter.category {
            path.push_str("&category=");
            path.push_str(category.as_str());
        }
        if let Some(form_id) = filter.form_id {
            path.push_str("&form_id=");
            path.push_str(&form_id.to_string());
        }
        crate::api::with_cursor(path, cursor)
    }

    /// 분류 칩: 맨 앞은 전체, 이어서 분류 순서대로.
    pub fn category_chips() -> Vec<(Option<FormCategory>, &'static str)> {
        std::iter::once((None, "전체"))
            .chain(
                FormCategory::ALL
                    .iter()
                    .map(|category| (Some(*category), text::form_category_label(*category))),
            )
            .collect()
    }

    /// 양식 선택 상자의 `(값, 문구)`. 분류를 골랐으면 그 분류의 양식만 나온다.
    pub fn form_options(
        groups: &[FormGroup],
        category: Option<FormCategory>,
    ) -> Vec<(String, String)> {
        std::iter::once((String::new(), "모든 양식".to_owned()))
            .chain(
                groups
                    .iter()
                    .filter(|group| category.is_none_or(|chosen| group.category == chosen))
                    .flat_map(|group| group.forms.iter())
                    .map(|form| (form.id.to_string(), form.name.clone())),
            )
            .collect()
    }

    /// 선택 상자의 값을 조건으로. 양식 ID가 아니면 고르지 않은 것이다.
    pub fn parse_form(value: &str) -> Option<FormId> {
        FormId::parse(value).ok()
    }

    /// 양식 이름 → 분류 이름. 목록의 문서에는 양식 이름만 있으므로 이름으로 찾는다. 같은 이름의
    /// 양식이 여러 분류에 있으면 어느 쪽인지 알 수 없어 뺀다(꺼진 양식의 문서도 분류가 안 나온다).
    pub fn category_by_form_name(groups: &[FormGroup]) -> BTreeMap<String, String> {
        let mut found: BTreeMap<String, Option<String>> = BTreeMap::new();
        for group in groups {
            for form in &group.forms {
                found
                    .entry(form.name.clone())
                    .and_modify(|label| {
                        if label.as_deref() != Some(group.category_label.as_str()) {
                            *label = None;
                        }
                    })
                    .or_insert_with(|| Some(group.category_label.clone()));
            }
        }
        found
            .into_iter()
            .filter_map(|(name, label)| label.map(|label| (name, label)))
            .collect()
    }

    /// `GET /admin/approval/documents` 경로. 상태·종류가 없으면 전체.
    pub fn admin_documents_path(
        status: Option<DocumentStatus>,
        kind: Option<DocumentKind>,
        cursor: Option<&str>,
    ) -> String {
        let mut path = "/admin/approval/documents?limit=50".to_owned();
        if let Some(status) = status {
            path.push_str("&status=");
            path.push_str(status.as_str());
        }
        if let Some(kind) = kind {
            path.push_str("&kind=");
            path.push_str(kind.as_str());
        }
        crate::api::with_cursor(path, cursor)
    }

    /// 탭 배지 숫자: 결재 대기는 처리할 문서 수, 참조는 읽지 않은 문서 수.
    pub fn mailbox_badge(mailbox: Mailbox, counts: Option<ApprovalCounts>) -> Option<u32> {
        let counts = counts?;
        match mailbox {
            Mailbox::Todo => Some(counts.todo),
            Mailbox::Reference => Some(counts.unread_reference),
            Mailbox::Upcoming | Mailbox::Drafted | Mailbox::Done => None,
        }
    }

    /// `Tabs`에 넘길 `(키, 문구, 배지)`.
    pub fn mailbox_tabs(counts: Option<ApprovalCounts>) -> Vec<(String, String, Option<u32>)> {
        Mailbox::ALL
            .iter()
            .map(|mailbox| {
                (
                    mailbox.as_str().to_owned(),
                    text::mailbox_label(*mailbox).to_owned(),
                    mailbox_badge(*mailbox, counts),
                )
            })
            .collect()
    }

    pub fn empty_title(mailbox: Mailbox) -> &'static str {
        match mailbox {
            Mailbox::Todo => "결재할 문서가 없어요.",
            Mailbox::Upcoming => "곧 차례가 올 문서가 없어요.",
            Mailbox::Drafted => "기안한 문서가 없어요.",
            Mailbox::Done => "처리한 문서가 없어요.",
            Mailbox::Reference => "참조로 받은 문서가 없어요.",
        }
    }

    /// 목록 한 줄의 보조 정보: 양식, 기안자, 문서 번호, (진행 중이면) 현재 결재자.
    pub fn row_meta(doc: &DocumentSummary) -> Vec<String> {
        let current = match (doc.status, &doc.current_approver_name) {
            (DocumentStatus::InProgress, Some(name)) if !name.is_empty() => {
                Some(format!("현재 {name}"))
            }
            _ => None,
        };
        [
            Some(doc.form_name.clone()),
            Some(doc.drafter_name.clone()),
            doc.doc_no.clone(),
            current,
        ]
        .into_iter()
        .flatten()
        .filter(|part| !part.trim().is_empty())
        .collect()
    }
}

#[component]
pub fn ApprovalList() -> Element {
    let session = use_session();
    let mut mailbox = use_signal(|| Mailbox::Todo);
    let mut category = use_signal(|| None::<FormCategory>);
    let mut form_id = use_signal(String::new);
    let more = use_more_pages::<DocumentSummary>();
    let mut counts =
        use_resource(|| async { api::get::<ApprovalCounts>("/approval/counts").await });
    // 거르기와 분류 표시에 쓰는 양식 목록. 못 읽어도 문서함은 그대로 쓸 수 있다.
    let forms = use_resource(|| async { api::get::<Vec<FormGroup>>("/approval/forms").await });
    let first = use_resource(move || async move {
        let current = mailbox();
        let filter = logic::DocFilter {
            category: category(),
            form_id: logic::parse_form(&form_id()),
        };
        more.reset();
        api::get::<Page<DocumentSummary>>(&logic::mailbox_path(current, filter, None)).await
    });
    let groups = crate::ui::loaded_or_empty(&forms);
    let categories = logic::category_by_form_name(&groups);
    let filter = logic::DocFilter {
        category: category(),
        form_id: logic::parse_form(&form_id()),
    };
    let filtered = filter != logic::DocFilter::default();

    let tz = session.zone();
    let now = now();
    let current = mailbox();
    let tabs = logic::mailbox_tabs(
        counts
            .read()
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .copied(),
    );

    rsx! {
        PageHeader {
            title: "결재",
            description: "결재할 문서와 내가 올린 문서를 확인해요.",
            actions: rsx! {
                Link { class: "btn btn-primary", to: Route::ApprovalNew {}, "새 문서" }
            },
        }
        Tabs {
            tabs,
            active: current.as_str(),
            onselect: move |key: String| {
                if let Ok(next) = Mailbox::parse(&key) {
                    if next != mailbox() {
                        mailbox.set(next);
                        counts.restart();
                    }
                }
            },
        }
        Card {
            div { class: "doc-filters",
                div { class: "chip-group chip-scroll", role: "group", "aria-label": "양식 분류",
                    for (value , label) in logic::category_chips() {
                        button {
                            key: "{label}",
                            class: if category() == value { "chip chip-active" } else { "chip" },
                            r#type: "button",
                            "aria-pressed": category() == value,
                            onclick: move |_| {
                                if category() != value {
                                    category.set(value);
                                    // 분류가 바뀌면 그 분류에 없는 양식 조건이 남지 않게 지운다.
                                    form_id.set(String::new());
                                }
                            },
                            span { "{label}" }
                        }
                    }
                }
                if !groups.is_empty() {
                    div { class: "doc-filter-form",
                        SelectField {
                            label: "양식",
                            value: form_id(),
                            options: logic::form_options(&groups, category()),
                            onchange: move |value: String| form_id.set(value),
                        }
                    }
                }
            }
            {resource_view(first, move |page: Page<DocumentSummary>| {
                let next = more.cursor(&page);
                let has_more = next.is_some();
                let items: Vec<DocumentSummary> = more.all(page.items, |item| item.id);
                rsx! {
                    if items.is_empty() {
                        EmptyState {
                            title: if filtered { "조건에 맞는 문서가 없어요." } else { logic::empty_title(current) },
                            hint: if filtered { "분류나 양식을 바꿔 보세요." } else { "" },
                            icon: IconName::Inbox,
                        }
                    } else {
                        div { class: "list",
                            for doc in items {
                                DocumentRow {
                                    key: "{doc.id}",
                                    category: categories.get(&doc.form_name).cloned().unwrap_or_default(),
                                    doc,
                                    now,
                                    tz,
                                }
                            }
                        }
                        LoadMore {
                            has_more,
                            busy: more.busy(),
                            onclick: move |_| {
                                if let Some(cursor) = next.clone() {
                                    spawn(more.load(cursor, move |c| logic::mailbox_path(current, filter, Some(c))));
                                }
                            },
                        }
                    }
                }
            })}
        }
    }
}

/// 문서 목록의 한 줄. 결재함과 관리자 "전체 문서"가 함께 쓴다.
#[component]
pub(crate) fn DocumentRow(
    doc: DocumentSummary,
    now: Timestamp,
    tz: chrono_tz::Tz,
    /// 양식의 분류 이름(알 때만).
    #[props(into, default)]
    category: String,
) -> Element {
    rsx! {
        Link {
            class: "list-row list-row-top",
            to: Route::ApprovalDetail { id: doc.id.to_string() },
            div { class: "list-row-main",
                strong { "{doc.title}" }
                Meta { items: logic::row_meta(&doc) }
            }
            div { class: "list-row-side",
                span { class: "badge-row",
                    if !category.is_empty() {
                        Badge { tone: Tone::Neutral, "{category}" }
                    }
                    if doc.my_turn {
                        Badge { tone: Tone::Warning, "내 차례" }
                    }
                    Badge { tone: text::document_status_tone(doc.status),
                        {text::document_status_label(doc.status)}
                    }
                }
                small {
                    {doc.submitted_at.map(|at| format::short_when(at, now, tz)).unwrap_or_default()}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::logic::*;
    use pango_api::approval::{
        ApprovalCounts, DocumentSummary, FormGroup, FormListItem, FormRoute,
    };
    use pango_core::approval::{DocumentKind, DocumentStatus, FormCategory, Mailbox};
    use pango_core::form_schema::FormSchema;
    use pango_core::id::{DocumentId, FormId, MemberId};

    const ALL: DocFilter = DocFilter {
        category: None,
        form_id: None,
    };

    fn form(name: &str) -> FormListItem {
        FormListItem {
            id: FormId::new(),
            code: "c".into(),
            name: name.into(),
            description: String::new(),
            kind: DocumentKind::General,
            body_template: String::new(),
            use_body: true,
            schema: FormSchema::default(),
            schema_version: 1,
            system: false,
            route: FormRoute::ApprovalNew,
        }
    }

    fn group(category: FormCategory, names: &[&str]) -> FormGroup {
        FormGroup {
            category,
            category_label: category.label().to_owned(),
            forms: names.iter().map(|name| form(name)).collect(),
        }
    }

    #[test]
    fn filters_are_added_to_the_mailbox_path() {
        let form_id = FormId::new();
        let by_category = DocFilter {
            category: Some(FormCategory::Accounting),
            form_id: None,
        };
        assert_eq!(
            mailbox_path(Mailbox::Drafted, by_category, None),
            "/approval/documents?box=drafted&category=accounting"
        );
        let both = DocFilter {
            category: Some(FormCategory::Trip),
            form_id: Some(form_id),
        };
        assert_eq!(
            mailbox_path(Mailbox::Done, both, Some("ff")),
            format!("/approval/documents?box=done&category=trip&form_id={form_id}&cursor=ff")
        );
        assert_eq!(parse_form(&form_id.to_string()), Some(form_id));
        assert_eq!(parse_form(""), None);
        assert_eq!(parse_form("1 OR 1=1"), None);
    }

    #[test]
    fn chips_and_form_options_follow_the_category() {
        let chips = category_chips();
        assert_eq!(chips.len(), FormCategory::ALL.len() + 1);
        assert_eq!(chips[0], (None, "전체"));
        assert_eq!(chips[1], (Some(FormCategory::General), "일반"));
        // 화면의 분류 이름은 서버가 내려 주는 이름과 같다.
        for (category, label) in chips.iter().skip(1) {
            assert_eq!(category.map(FormCategory::label), Some(*label));
        }

        let groups = [
            group(FormCategory::General, &["품의서"]),
            group(FormCategory::Trip, &["국내출장신청", "해외출장신청"]),
        ];
        let labels = |category| -> Vec<String> {
            form_options(&groups, category)
                .into_iter()
                .map(|(_, label)| label)
                .collect()
        };
        assert_eq!(
            labels(None),
            vec!["모든 양식", "품의서", "국내출장신청", "해외출장신청"]
        );
        assert_eq!(
            labels(Some(FormCategory::Trip)),
            vec!["모든 양식", "국내출장신청", "해외출장신청"]
        );
        assert_eq!(labels(Some(FormCategory::Welfare)), vec!["모든 양식"]);
        assert_eq!(form_options(&[], None)[0].0, "");
    }

    #[test]
    fn category_is_found_by_form_name_only_when_unambiguous() {
        let groups = [
            group(FormCategory::General, &["품의서", "신청서"]),
            group(FormCategory::Trip, &["국내출장신청", "신청서"]),
            group(FormCategory::Trip, &["국내출장신청"]),
        ];
        let found = category_by_form_name(&groups);
        assert_eq!(found.get("품의서").map(String::as_str), Some("일반"));
        assert_eq!(found.get("국내출장신청").map(String::as_str), Some("출장"));
        assert_eq!(found.get("신청서"), None);
        assert_eq!(found.get("없는 양식"), None);
        assert!(category_by_form_name(&[]).is_empty());
    }

    #[test]
    fn paths_carry_mailbox_status_and_encoded_cursor() {
        assert_eq!(
            mailbox_path(Mailbox::Todo, ALL, None),
            "/approval/documents?box=todo"
        );
        assert_eq!(
            mailbox_path(Mailbox::Reference, ALL, Some("7b22")),
            "/approval/documents?box=reference&cursor=7b22"
        );
        assert_eq!(
            mailbox_path(Mailbox::Done, ALL, Some("")),
            "/approval/documents?box=done"
        );
        assert_eq!(
            mailbox_path(Mailbox::Drafted, ALL, Some("a&b=c d")),
            "/approval/documents?box=drafted&cursor=a%26b%3Dc%20d"
        );
        assert_eq!(
            admin_documents_path(None, None, None),
            "/admin/approval/documents?limit=50"
        );
        assert_eq!(
            admin_documents_path(Some(DocumentStatus::InProgress), None, Some("ff")),
            "/admin/approval/documents?limit=50&status=in_progress&cursor=ff"
        );
        assert_eq!(
            admin_documents_path(
                Some(DocumentStatus::InProgress),
                Some(DocumentKind::AttendanceCorrection),
                None
            ),
            "/admin/approval/documents?limit=50&status=in_progress&kind=attendance_correction"
        );
    }

    #[test]
    fn tabs_show_counts_only_where_they_mean_something() {
        let counts = ApprovalCounts {
            todo: 3,
            in_progress_drafted: 9,
            unread_reference: 2,
        };
        let tabs = mailbox_tabs(Some(counts));
        assert_eq!(tabs.len(), Mailbox::ALL.len());
        assert_eq!(tabs[0], ("todo".into(), "결재 대기".into(), Some(3)));
        let reference = tabs.iter().find(|t| t.0 == "reference").expect("tab");
        assert_eq!(reference.2, Some(2));
        let drafted = tabs.iter().find(|t| t.0 == "drafted").expect("tab");
        assert_eq!(drafted.2, None);
        assert!(mailbox_tabs(None).iter().all(|t| t.2.is_none()));
        for tab in &tabs {
            assert!(Mailbox::parse(&tab.0).is_ok());
        }
        for mailbox in Mailbox::ALL {
            assert!(!empty_title(*mailbox).is_empty());
        }
    }

    fn summary() -> DocumentSummary {
        DocumentSummary {
            id: DocumentId::new(),
            doc_no: Some("2026-000001".into()),
            title: "t".into(),
            form_name: "품의서".into(),
            kind: DocumentKind::General,
            status: DocumentStatus::InProgress,
            drafter_id: MemberId::new(),
            drafter_name: "홍길동".into(),
            current_approver_id: Some(MemberId::new()),
            current_approver_name: Some("김결재".into()),
            submitted_at: None,
            my_turn: false,
        }
    }

    #[test]
    fn row_meta_skips_missing_parts() {
        assert_eq!(
            row_meta(&summary()),
            vec!["품의서", "홍길동", "2026-000001", "현재 김결재"]
        );
        let draft = DocumentSummary {
            doc_no: None,
            status: DocumentStatus::Draft,
            ..summary()
        };
        assert_eq!(row_meta(&draft), vec!["품의서", "홍길동"]);
        let odd = DocumentSummary {
            drafter_name: String::new(),
            current_approver_name: Some(String::new()),
            ..summary()
        };
        assert_eq!(row_meta(&odd), vec!["품의서", "2026-000001"]);
        let closed = DocumentSummary {
            status: DocumentStatus::Approved,
            ..summary()
        };
        assert!(!row_meta(&closed).iter().any(|part| part.contains("현재")));
    }
}
