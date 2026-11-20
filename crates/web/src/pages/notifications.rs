//! 앱 내 알림 목록.

use std::collections::BTreeSet;

use dioxus::prelude::*;
use pango_api::notification::{NotificationDto, ReadAllResult};
use pango_api::{Empty, Page};
use pango_core::id::NotificationId;

use crate::api;
use crate::badges::{refresh_badges, use_badges};
use crate::format;
use crate::route::Route;
use crate::session::use_session;
use crate::ui::{
    now, report_error, report_success, resource_view, use_more_pages, Button, ButtonKind, Card,
    EmptyState, IconName, LoadMore, PageHeader, Tabs,
};

use logic::{is_unread, list_path, safe_link};

#[component]
pub fn Notifications() -> Element {
    let session = use_session();
    let nav = navigator();
    let badges = use_badges();
    let mut unread_only = use_signal(|| false);
    // 이 화면에서 읽음 처리한 알림. 목록을 다시 받지 않고 바로 읽음으로 보이게 한다.
    let mut read_ids = use_signal(BTreeSet::<NotificationId>::new);
    let mut marking_all = use_signal(|| false);
    let mut opening = use_signal(|| false);
    let more = use_more_pages::<NotificationDto>();
    let mut first = use_resource(move || async move {
        let path = list_path(unread_only());
        more.reset();
        api::get::<Page<NotificationDto>>(&path).await
    });

    let open = move |item: NotificationDto| async move {
        // 읽음 처리 중에 다시 누르면(두 번 누름) 한 번만 처리하고 한 번만 이동한다.
        if opening() {
            return;
        }
        opening.set(true);
        if is_unread(&item, &read_ids.read()) {
            match api::post::<_, Empty>(&format!("/notifications/{}/read", item.id), &Empty {})
                .await
            {
                Ok(_) => {
                    read_ids.write().insert(item.id);
                    refresh_badges();
                }
                Err(error) => report_error(&error),
            }
        }
        if let Some(route) = item
            .link
            .as_deref()
            .and_then(safe_link)
            .and_then(|path| path.parse::<Route>().ok())
        {
            nav.push(route);
        }
        opening.set(false);
    };
    let mark_all = move |_| async move {
        if marking_all() {
            return;
        }
        marking_all.set(true);
        match api::post::<_, ReadAllResult>("/notifications/read-all", &Empty {}).await {
            Ok(result) if result.updated == 0 => report_success("읽지 않은 알림이 없어요."),
            Ok(result) => {
                report_success(format!("알림 {}개를 읽음 처리했어요.", result.updated));
                first.restart();
            }
            Err(error) => report_error(&error),
        }
        refresh_badges();
        marking_all.set(false);
    };

    let tz = session.zone();
    let at = now();
    let tabs = vec![
        ("all".to_owned(), "전체".to_owned(), None),
        (
            "unread".to_owned(),
            "안 읽음만".to_owned(),
            Some(badges.notifications()),
        ),
    ];

    rsx! {
        PageHeader {
            title: "알림",
            actions: rsx! {
                Button {
                    kind: ButtonKind::Weak,
                    busy: marking_all(),
                    onclick: mark_all,
                    "모두 읽음"
                }
            },
        }
        Tabs {
            tabs,
            active: if unread_only() { "unread" } else { "all" },
            onselect: move |key: String| unread_only.set(key == "unread"),
        }
        {resource_view(first, move |page: Page<NotificationDto>| {
            let cursor = more.cursor(&page);
            let items: Vec<NotificationDto> = more.all(page.items, |item| item.id);
            let read = read_ids.read().clone();
            rsx! {
                if items.is_empty() {
                    EmptyState {
                        title: if unread_only() { "읽지 않은 알림이 없어요." } else { "받은 알림이 없어요." },
                        icon: IconName::Bell,
                    }
                } else {
                    Card {
                        ul { class: "list",
                            for item in items {
                                NotificationRow {
                                    key: "{item.id}",
                                    unread: is_unread(&item, &read),
                                    when: format::short_when(item.created_at, at, tz),
                                    item,
                                    onopen: open,
                                }
                            }
                        }
                        if let Some(cursor) = cursor {
                            LoadMore {
                                busy: more.busy(),
                                onclick: move |_| {
                                    let base = list_path(unread_only());
                                    more.load(cursor.clone(), move |c| api::with_cursor(base.clone(), Some(c)))
                                },
                            }
                        }
                    }
                }
            }
        })}
    }
}

#[component]
fn NotificationRow(
    item: NotificationDto,
    unread: bool,
    when: String,
    onopen: EventHandler<NotificationDto>,
) -> Element {
    let has_link = item.link.as_deref().and_then(safe_link).is_some();
    let kind = crate::text::notification_kind_label(&item.kind);
    let class = if unread {
        "notification notification-unread"
    } else {
        "notification"
    };
    let content = rsx! {
        span { class: if unread { "unread-dot" } else { "unread-dot unread-dot-off" }, "aria-hidden": "true" }
        span { class: "list-row-main",
            if let Some(kind) = kind {
                small { "{kind}" }
            }
            strong { "{item.title}" }
            if !item.body.is_empty() {
                small { "{item.body}" }
            }
        }
        span { class: "list-row-side",
            if unread {
                span { class: "sr-only", "읽지 않음" }
            }
            "{when}"
        }
    };
    rsx! {
        li { class: "list-row",
            if has_link || unread {
                button {
                    class,
                    r#type: "button",
                    onclick: {
                        let item = item.clone();
                        move |_| onopen.call(item.clone())
                    },
                    {content}
                }
            } else {
                div { class, {content} }
            }
        }
    }
}

pub mod logic {
    use std::collections::BTreeSet;

    use pango_api::notification::NotificationDto;
    use pango_core::id::NotificationId;

    use crate::route::Route;

    const PAGE_SIZE: u32 = 30;
    const MAX_LINK_LEN: usize = 512;

    /// 알림이 가리킬 수 있는 화면의 첫 경로 조각. 그 밖의 경로로는 보내지 않는다.
    const ROUTE_PREFIXES: &[&str] = &[
        "attendance",
        "leave",
        "approval",
        "notices",
        "notifications",
        "people",
        "team",
        "me",
        "more",
        "admin",
    ];

    pub fn list_path(unread_only: bool) -> String {
        format!("/notifications?limit={PAGE_SIZE}&unread_only={unread_only}")
    }

    pub fn is_unread(item: &NotificationDto, read_here: &BTreeSet<NotificationId>) -> bool {
        item.read_at.is_none() && !read_here.contains(&item.id)
    }

    /// 알림 링크가 이 앱 안의 화면 경로일 때만 그 경로를 돌려준다.
    ///
    /// 링크는 서버가 만든 값이지만 저장된 데이터라 그대로 믿지 않는다(열린 리디렉션·
    /// `javascript:` 실행 방지). 한 개의 `/`로 시작하고, 영문·숫자·`-`·`_`·`.`·`/`만 쓰며,
    /// `.`/`..` 조각이 없고, 알려진 화면으로 해석되는 경로만 받는다.
    pub fn safe_link(link: &str) -> Option<String> {
        if link.is_empty() || link.len() > MAX_LINK_LEN {
            return None;
        }
        let rest = link.strip_prefix('/')?;
        let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/');
        if !rest.chars().all(allowed) {
            return None;
        }
        if !rest.is_empty() {
            let mut segments = rest.split('/');
            let first = segments.next()?;
            if !ROUTE_PREFIXES.contains(&first) {
                return None;
            }
            let bad_segment = |s: &str| s.is_empty() || s == "." || s == "..";
            if rest.split('/').any(bad_segment) {
                return None;
            }
        }
        match link.parse::<Route>() {
            Ok(Route::NotFound { .. }) | Ok(Route::Login {}) | Ok(Route::ChangePassword {}) => None,
            Ok(_) => Some(link.to_owned()),
            Err(_) => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn accepts_in_app_paths() {
            for link in [
                "/",
                "/notices",
                "/notices/0190a1b2-0000-7000-8000-000000000001",
                "/approval/documents/abc-123",
                "/notifications",
                "/admin/notices",
                "/me",
            ] {
                assert_eq!(safe_link(link), Some(link.to_owned()), "{link}");
            }
        }

        #[test]
        fn rejects_other_origins_and_schemes() {
            for link in [
                "",
                "notices",
                "//evil.example",
                "//evil.example/notices",
                "/\\evil.example",
                "/\\/evil.example",
                "\\\\evil.example",
                "https://evil.example/notices",
                "http:/notices",
                "javascript:alert(1)",
                "JavaScript:alert(1)",
                "/javascript:alert(1)",
                "data:text/html,<script>alert(1)</script>",
                " /notices",
                "/notices ",
                "/notices\n",
                "/notices\t/x",
                "/notices\u{0000}",
                "/notices\u{202e}",
                "/notices?next=//evil.example",
                "/notices#x",
                "/notices/%2F%2Fevil.example",
                "/notices/<script>",
                "/공지",
            ] {
                assert_eq!(safe_link(link), None, "{link:?}");
            }
        }

        #[test]
        fn rejects_unknown_or_odd_paths() {
            for link in [
                "/login",
                "/password",
                "/unknown",
                "/noticesx",
                "/notices/",
                "/notices//x",
                "/notices/./x",
                "/notices/../admin",
                "/..",
                "/admin/nope/deeper",
            ] {
                assert_eq!(safe_link(link), None, "{link:?}");
            }
            assert_eq!(safe_link(&format!("/notices/{}", "a".repeat(600))), None);
        }

        #[test]
        fn read_state_combines_server_and_local() {
            let item = NotificationDto {
                id: NotificationId::new(),
                kind: "notice.new".into(),
                title: "t".into(),
                body: String::new(),
                link: None,
                read_at: None,
                created_at: pango_core::time::Timestamp::from_millis(0).expect("ts"),
            };
            let mut local = BTreeSet::new();
            assert!(is_unread(&item, &local));
            local.insert(item.id);
            assert!(!is_unread(&item, &local));
            let read = NotificationDto {
                read_at: Some(item.created_at),
                ..item
            };
            assert!(!is_unread(&read, &BTreeSet::new()));
            assert_eq!(list_path(true), "/notifications?limit=30&unread_only=true");
        }
    }
}
