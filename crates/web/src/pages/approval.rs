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
    report_error, resource_view, Badge, Card, EmptyState, IconName, LoadMore, PageHeader, Tabs,
    Tone,
};

/// 지금 시각(목록의 "오늘이면 시각만" 표시용).
pub(crate) fn now() -> Timestamp {
    // `Date::now()`는 밀리초 정수를 f64로 돌려주므로 잘림이 없다.
    #[allow(clippy::cast_possible_truncation)]
    let millis = js_sys::Date::now() as i64;
    Timestamp::from_millis(millis)
        .unwrap_or_else(|_| Timestamp::from_utc(chrono::DateTime::UNIX_EPOCH))
}

/// 결재 화면들이 함께 쓰는 순수 함수.
pub(crate) mod logic {
    use pango_api::approval::{ApprovalCounts, DocumentSummary};
    use pango_api::Page;
    use pango_core::approval::{DocumentStatus, Mailbox};

    use crate::text;

    /// 쿼리 값 인코딩(RFC 3986 비예약 문자만 그대로 둔다). 네이티브 테스트에서도 돌도록 순수 구현이다.
    pub fn encode_query(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                out.push(char::from(byte));
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }

    fn with_cursor(mut path: String, cursor: Option<&str>) -> String {
        if let Some(cursor) = cursor.filter(|c| !c.is_empty()) {
            path.push_str("&cursor=");
            path.push_str(&encode_query(cursor));
        }
        path
    }

    /// `GET /approval/documents` 경로.
    pub fn mailbox_path(mailbox: Mailbox, cursor: Option<&str>) -> String {
        with_cursor(
            format!("/approval/documents?box={}", mailbox.as_str()),
            cursor,
        )
    }

    /// `GET /admin/approval/documents` 경로. 상태가 없으면 전체.
    pub fn admin_documents_path(status: Option<DocumentStatus>, cursor: Option<&str>) -> String {
        let mut path = "/admin/approval/documents?limit=50".to_owned();
        if let Some(status) = status {
            path.push_str("&status=");
            path.push_str(status.as_str());
        }
        with_cursor(path, cursor)
    }

    /// "더 보기"로 읽은 페이지를 이어 붙인다. 커서는 마지막으로 읽은 페이지의 것을 쓴다.
    pub fn append_page<T>(acc: Option<Page<T>>, next: Page<T>) -> Page<T> {
        match acc {
            None => next,
            Some(mut acc) => {
                acc.items.extend(next.items);
                acc.next_cursor = next.next_cursor;
                acc
            }
        }
    }

    /// 첫 페이지와 더 읽은 페이지들을 합친 목록과 다음 커서.
    pub fn merge_pages<T: Clone>(
        first: &Page<T>,
        extra: Option<&Page<T>>,
    ) -> (Vec<T>, Option<String>) {
        match extra {
            None => (first.items.clone(), first.next_cursor.clone()),
            Some(extra) => {
                let mut items = first.items.clone();
                items.extend(extra.items.iter().cloned());
                (items, extra.next_cursor.clone())
            }
        }
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
            Mailbox::Todo => "결재할 문서가 없습니다.",
            Mailbox::Upcoming => "곧 차례가 올 문서가 없습니다.",
            Mailbox::Drafted => "기안한 문서가 없습니다.",
            Mailbox::Done => "처리한 문서가 없습니다.",
            Mailbox::Reference => "참조로 받은 문서가 없습니다.",
        }
    }

    /// 목록 한 줄의 보조 문구: `품의서 · 홍길동 · 2026-000123 · 현재 김결재`.
    pub fn row_meta(doc: &DocumentSummary) -> String {
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
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

#[component]
pub fn ApprovalList() -> Element {
    let session = use_session();
    let mut mailbox = use_signal(|| Mailbox::Todo);
    let mut extra = use_signal(|| None::<Page<DocumentSummary>>);
    let mut loading_more = use_signal(|| false);
    let mut counts =
        use_resource(|| async { api::get::<ApprovalCounts>("/approval/counts").await });
    let first = use_resource(move || async move {
        api::get::<Page<DocumentSummary>>(&logic::mailbox_path(mailbox(), None)).await
    });

    let tz = format::zone(&session.me().map(|me| me.timezone).unwrap_or_default());
    let now = now();
    let current = mailbox();
    let tabs = logic::mailbox_tabs(
        counts
            .read()
            .as_ref()
            .and_then(|r| r.as_ref().ok())
            .copied(),
    );

    let load_more = move |cursor: String| async move {
        if loading_more() {
            return;
        }
        loading_more.set(true);
        let at_start = mailbox();
        match api::get::<Page<DocumentSummary>>(&logic::mailbox_path(at_start, Some(&cursor))).await
        {
            // 읽는 사이 탭을 바꿨으면 버린다.
            Ok(page) if mailbox() == at_start => {
                let acc = extra.peek().clone();
                extra.set(Some(logic::append_page(acc, page)));
            }
            Ok(_) => {}
            Err(error) => report_error(&error),
        }
        loading_more.set(false);
    };

    rsx! {
        PageHeader {
            title: "결재",
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
                        extra.set(None);
                        mailbox.set(next);
                        counts.restart();
                    }
                }
            },
        }
        {resource_view(first, move |page: Page<DocumentSummary>| {
            let (items, next) = logic::merge_pages(&page, extra().as_ref());
            let has_more = next.is_some();
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
                        busy: loading_more(),
                        onclick: move |_| {
                            if let Some(cursor) = next.clone() {
                                spawn(load_more(cursor));
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
            class: "list-row doc-row",
            to: Route::ApprovalDetail { id: doc.id.to_string() },
            div { class: "list-row-main",
                strong { "{doc.title}" }
                small { {logic::row_meta(&doc)} }
            }
            div { class: "list-row-side",
                span { class: "doc-badges",
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
    use pango_api::Page;
    use pango_core::approval::{DocumentKind, DocumentStatus, Mailbox};
    use pango_core::id::{DocumentId, MemberId};

    fn page(items: Vec<i32>, next: Option<&str>) -> Page<i32> {
        Page {
            items,
            next_cursor: next.map(str::to_owned),
        }
    }

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
            admin_documents_path(None, None),
            "/admin/approval/documents?limit=50"
        );
        assert_eq!(
            admin_documents_path(Some(DocumentStatus::InProgress), Some("ff")),
            "/admin/approval/documents?limit=50&status=in_progress&cursor=ff"
        );
        assert_eq!(encode_query("가"), "%EA%B0%80");
    }

    #[test]
    fn pages_are_appended_in_order_with_latest_cursor() {
        let acc = append_page(None, page(vec![3, 4], Some("b")));
        let acc = append_page(Some(acc), page(vec![5], None));
        assert_eq!(acc.items, vec![3, 4, 5]);
        assert_eq!(acc.next_cursor, None);

        let first = page(vec![1, 2], Some("a"));
        assert_eq!(merge_pages(&first, None), (vec![1, 2], Some("a".into())));
        assert_eq!(merge_pages(&first, Some(&acc)), (vec![1, 2, 3, 4, 5], None));
        let empty: Page<i32> = page(vec![], None);
        assert_eq!(merge_pages(&empty, None), (vec![], None));
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
            "품의서 · 홍길동 · 2026-000001 · 현재 김결재"
        );
        let draft = DocumentSummary {
            doc_no: None,
            status: DocumentStatus::Draft,
            ..summary()
        };
        assert_eq!(row_meta(&draft), "품의서 · 홍길동");
        let odd = DocumentSummary {
            drafter_name: String::new(),
            current_approver_name: Some(String::new()),
            ..summary()
        };
        assert_eq!(row_meta(&odd), "품의서 · 2026-000001");
        let closed = DocumentSummary {
            status: DocumentStatus::Approved,
            ..summary()
        };
        assert!(!row_meta(&closed).contains("현재"));
    }
}
