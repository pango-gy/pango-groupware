//! 결재함: 결재 대기·결재 예정·기안함·완료·참조 탭과 문서 목록.

use dioxus::prelude::*;
use pango_api::approval::{ApprovalCounts, DocumentSummary};
use pango_api::Page;
use pango_core::approval::Mailbox;
use pango_core::time::Timestamp;

use crate::api;
use crate::format;
use crate::route::Route;
use crate::session::use_session;
use crate::text;
use crate::ui::{
    now, resource_view, use_more_pages, Badge, Card, EmptyState, IconName, LoadMore, Meta,
    PageHeader, Tabs, Tone,
};

/// 결재 화면들이 함께 쓰는 순수 함수.
pub(crate) mod logic {
    use pango_api::approval::{ApprovalCounts, DocumentSummary};
    use pango_core::approval::{DocumentKind, DocumentStatus, Mailbox};

    use crate::text;

    /// `GET /approval/documents` 경로.
    pub fn mailbox_path(mailbox: Mailbox, cursor: Option<&str>) -> String {
        crate::api::with_cursor(
            format!("/approval/documents?box={}", mailbox.as_str()),
            cursor,
        )
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
    let more = use_more_pages::<DocumentSummary>();
    let mut counts =
        use_resource(|| async { api::get::<ApprovalCounts>("/approval/counts").await });
    let first = use_resource(move || async move {
        let current = mailbox();
        more.reset();
        api::get::<Page<DocumentSummary>>(&logic::mailbox_path(current, None)).await
    });

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
        {resource_view(first, move |page: Page<DocumentSummary>| {
            let next = more.cursor(&page);
            let has_more = next.is_some();
            let items: Vec<DocumentSummary> = more.all(page.items, |item| item.id);
            rsx! {
                if items.is_empty() {
                    EmptyState { title: logic::empty_title(current), icon: IconName::Inbox }
                } else {
                    Card {
                        div { class: "list",
                            for doc in items {
                                DocumentRow { key: "{doc.id}", doc, now, tz }
                            }
                        }
                    }
                    LoadMore {
                        has_more,
                        busy: more.busy(),
                        onclick: move |_| {
                            if let Some(cursor) = next.clone() {
                                spawn(more.load(cursor, move |c| logic::mailbox_path(current, Some(c))));
                            }
                        },
                    }
                }
            }
        })}
    }
}

/// 문서 목록의 한 줄. 결재함과 관리자 "전체 문서"가 함께 쓴다.
#[component]
pub(crate) fn DocumentRow(doc: DocumentSummary, now: Timestamp, tz: chrono_tz::Tz) -> Element {
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
    use pango_api::approval::{ApprovalCounts, DocumentSummary};
    use pango_core::approval::{DocumentKind, DocumentStatus, Mailbox};
    use pango_core::id::{DocumentId, MemberId};

    #[test]
    fn paths_carry_mailbox_status_and_encoded_cursor() {
        assert_eq!(
            mailbox_path(Mailbox::Todo, None),
            "/approval/documents?box=todo"
        );
        assert_eq!(
            mailbox_path(Mailbox::Reference, Some("7b22")),
            "/approval/documents?box=reference&cursor=7b22"
        );
        assert_eq!(
            mailbox_path(Mailbox::Done, Some("")),
            "/approval/documents?box=done"
        );
        assert_eq!(
            mailbox_path(Mailbox::Drafted, Some("a&b=c d")),
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
