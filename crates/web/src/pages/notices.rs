//! 공지 목록과 상세.

use dioxus::prelude::*;
use pango_api::notice::{NoticeDetail as NoticeDto, NoticeReadReceipt, NoticeSummary};
use pango_api::{Empty, Page};
use pango_core::id::NoticeId;

use crate::api::{self, ApiResult};
use crate::badges::refresh_badges;
use crate::format;
use crate::route::Route;
use crate::session::use_session;
use crate::ui::{
    now, report_error, resource_view, use_more_pages, Badge, Button, ButtonKind, Card, EmptyState,
    Icon, IconName, LoadMore, PageHeader, Tone,
};

use logic::{apply_receipt, audience_text, is_missing, LIST_PATH};

#[component]
pub fn Notices() -> Element {
    let session = use_session();
    let more = use_more_pages::<NoticeSummary>();
    let first = use_resource(move || async move {
        more.reset();
        api::get::<Page<NoticeSummary>>(LIST_PATH).await
    });
    let tz = session.zone();
    let at = now();

    rsx! {
        PageHeader { title: "공지" }
        {resource_view(first, move |page: Page<NoticeSummary>| {
            let cursor = more.cursor(&page);
            let items: Vec<NoticeSummary> = more.all(page.items, |item| item.id);
            rsx! {
                if items.is_empty() {
                    EmptyState { title: "게시된 공지가 없습니다.", icon: IconName::Megaphone }
                } else {
                    Card {
                        ul { class: "list",
                            for notice in items {
                                NoticeRow {
                                    key: "{notice.id}",
                                    id: notice.id,
                                    title: notice.title.clone(),
                                    pinned: notice.pinned,
                                    must_read: notice.must_read,
                                    read: notice.read,
                                    meta: format!("{} · {}", notice.author_name, format::short_when(notice.published_at, at, tz)),
                                }
                            }
                        }
                        if let Some(cursor) = cursor {
                            LoadMore {
                                busy: more.busy(),
                                onclick: move |_| {
                                    more.load(cursor.clone(), |c| api::with_cursor(LIST_PATH.to_owned(), Some(c)))
                                },
                            }
                        }
                    }
                }
            }
        })}
    }
}

/// 공지를 읽어 오고, 아직 읽음 확인 전이면 확인한다(서버는 여러 번 받아도 첫 시각을 유지한다).
/// 주소의 ID 형식이 틀렸거나 볼 수 없는 공지면 `None`.
async fn load_and_mark(id: String) -> ApiResult<Option<NoticeDto>> {
    let Ok(id) = NoticeId::parse(&id) else {
        return Ok(None);
    };
    let mut notice = match api::get::<NoticeDto>(&format!("/notices/{id}")).await {
        Ok(notice) => notice,
        Err(error) if is_missing(&error) => return Ok(None),
        Err(error) => return Err(error),
    };
    if !notice.summary.read {
        match api::post::<_, NoticeReadReceipt>(&format!("/notices/{id}/read"), &Empty {}).await {
            Ok(receipt) => {
                apply_receipt(&mut notice, receipt.read_at);
                refresh_badges();
            }
            Err(error) => report_error(&error),
        }
    }
    Ok(Some(notice))
}

/// 공지 목록의 한 줄: 읽지 않음 점, 고정·필독 배지, 제목, 보조 문구. 공지 목록과 홈이 함께 쓴다.
#[component]
pub(crate) fn NoticeRow(
    id: NoticeId,
    title: String,
    pinned: bool,
    must_read: bool,
    read: bool,
    meta: String,
) -> Element {
    rsx! {
        li { class: "list-row",
            Link { class: "row-link", to: Route::NoticeDetail { id: id.to_string() },
                span {
                    class: if read { "unread-dot unread-dot-off" } else { "unread-dot" },
                    "aria-hidden": "true",
                }
                span { class: "list-row-main",
                    span { class: "notice-title",
                        NoticeFlags { pinned, must_read }
                        strong { "{title}" }
                    }
                    small {
                        if !read {
                            span { class: "sr-only", "읽지 않음, " }
                        }
                        "{meta}"
                    }
                }
            }
        }
    }
}

/// 공지의 "고정"·"필독" 배지. 공지 목록·상세·관리가 함께 쓴다.
#[component]
pub(crate) fn NoticeFlags(pinned: bool, must_read: bool) -> Element {
    rsx! {
        if pinned {
            Badge { tone: Tone::Info, "고정" }
        }
        if must_read {
            Badge { tone: Tone::Danger, "필독" }
        }
    }
}

#[component]
pub fn NoticeDetail(id: String) -> Element {
    let session = use_session();
    let mut notice = use_resource(use_reactive!(|id| load_and_mark(id)));
    let tz = session.zone();

    rsx! {
        Link { class: "back-link", to: Route::Notices {}, "← 공지 목록" }
        {resource_view(notice, move |found: Option<NoticeDto>| {
            let Some(notice_dto) = found else {
                return rsx! {
                    EmptyState {
                        title: "공지를 찾을 수 없습니다.",
                        hint: "삭제되었거나 볼 수 없는 공지입니다.",
                        icon: IconName::Megaphone,
                        Link { class: "btn btn-secondary", to: Route::Notices {}, "공지 목록으로" }
                    }
                };
            };
            let summary = &notice_dto.summary;
            rsx! {
                Card { class: "notice-detail",
                    header { class: "notice-head",
                        div { class: "notice-title",
                            NoticeFlags { pinned: summary.pinned, must_read: summary.must_read }
                        }
                        h1 { class: "notice-heading", "{summary.title}" }
                        p { class: "notice-meta",
                            "{summary.author_name} · {format::datetime(summary.published_at, tz)} · {audience_text(summary)}"
                        }
                    }
                    div { class: "notice-body", "{notice_dto.body}" }
                    if summary.must_read {
                        if let Some(read_at) = notice_dto.read_at {
                            div { class: "must-read must-read-done", role: "status",
                                Icon { name: IconName::Check }
                                span {
                                    strong { "확인했습니다" }
                                    small { " {format::datetime(read_at, tz)}" }
                                }
                            }
                        } else {
                            div { class: "must-read", role: "alert",
                                Icon { name: IconName::Alert }
                                span { "필독 공지입니다. 읽음 확인이 아직 저장되지 않았습니다." }
                                Button {
                                    kind: ButtonKind::Secondary,
                                    small: true,
                                    onclick: move |_| notice.restart(),
                                    "다시 확인"
                                }
                            }
                        }
                    }
                }
            }
        })}
    }
}

pub mod logic {
    use pango_api::notice::{NoticeDetail, NoticeSummary};
    use pango_core::authz::AudienceKind;
    use pango_core::time::Timestamp;

    use crate::api::ApiError;

    pub const LIST_PATH: &str = "/notices?limit=30";

    /// 없는 공지(또는 볼 수 없어 없는 것으로 취급된 공지)인지.
    pub fn is_missing(error: &ApiError) -> bool {
        error.status == 404 || error.code == "not_found"
    }

    /// 읽음 확인 결과를 화면의 공지에 반영한다(목록을 다시 받지 않는다).
    pub fn apply_receipt(notice: &mut NoticeDetail, read_at: Timestamp) {
        notice.summary.read = true;
        notice.read_at.get_or_insert(read_at);
    }

    pub fn audience_text(summary: &NoticeSummary) -> String {
        match (summary.audience, &summary.department_name) {
            (AudienceKind::All, _) => "전체 공지".to_owned(),
            (AudienceKind::Department, Some(name)) => format!("{name} 대상"),
            (AudienceKind::Department, None) => "부서 공지".to_owned(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pango_core::id::{MemberId, NoticeId};

        fn ts(millis: i64) -> Timestamp {
            Timestamp::from_millis(millis).expect("ts")
        }

        fn notice() -> NoticeDetail {
            NoticeDetail {
                summary: NoticeSummary {
                    id: NoticeId::new(),
                    title: "<script>alert(1)</script>".into(),
                    author_id: MemberId::new(),
                    author_name: "관리자".into(),
                    pinned: false,
                    must_read: true,
                    audience: AudienceKind::All,
                    department_id: None,
                    department_name: None,
                    published_at: ts(1),
                    read: false,
                    archived_at: None,
                    version: 1,
                },
                body: "첫 줄\n둘째 줄".into(),
                read_at: None,
            }
        }

        fn error(status: u16, code: &str) -> ApiError {
            ApiError {
                status,
                code: code.into(),
                field: None,
                reason: None,
            }
        }

        #[test]
        fn missing_means_404_only() {
            assert!(is_missing(&error(404, "not_found")));
            assert!(!is_missing(&error(403, "forbidden")));
            assert!(!is_missing(&error(0, "network_error")));
            assert!(!is_missing(&error(500, "internal")));
        }

        #[test]
        fn receipt_keeps_the_first_read_time() {
            let mut n = notice();
            apply_receipt(&mut n, ts(10));
            assert!(n.summary.read);
            assert_eq!(n.read_at, Some(ts(10)));
            apply_receipt(&mut n, ts(20));
            assert_eq!(n.read_at, Some(ts(10)));
        }

        #[test]
        fn audience_is_described() {
            let mut s = notice().summary;
            assert_eq!(audience_text(&s), "전체 공지");
            s.audience = AudienceKind::Department;
            assert_eq!(audience_text(&s), "부서 공지");
            s.department_name = Some("개발팀".into());
            assert_eq!(audience_text(&s), "개발팀 대상");
        }
    }
}
