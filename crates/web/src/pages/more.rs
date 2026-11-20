//! 모바일 "더보기"와 없는 경로 안내.

use dioxus::prelude::*;

use crate::badges::{self, use_badges};
use crate::layout::nav_sections;
use crate::route::Route;
use crate::session::{self, use_session};
use crate::ui::{Card, EmptyState, Icon, IconName, PageHeader};

/// 하단 탭에 다 넣지 못한 메뉴를 모아 보여 준다.
#[component]
pub fn More() -> Element {
    let session = use_session();
    let nav = navigator();
    let badges = use_badges();
    let unread = badges.notifications();
    let Some(me) = session.me() else {
        return rsx! {};
    };
    rsx! {
        PageHeader { title: "더보기" }
        for (title , items) in nav_sections(&me) {
            Card { title: if title.is_empty() { "메뉴" } else { title },
                div { class: "menu-list",
                    for item in items {
                        Link {
                            key: "{item.label}",
                            class: "menu-row",
                            to: item.to.clone(),
                            "aria-label": badges::unread_label(item.label, badges.for_route(&item.to)),
                            Icon { name: item.icon }
                            span { class: "menu-row-label", "{item.label}" }
                            if badges.for_route(&item.to) > 0 {
                                span { class: "nav-count", "aria-hidden": "true",
                                    "{badges::count_text(badges.for_route(&item.to))}"
                                }
                            }
                            Icon { name: IconName::Chevron }
                        }
                    }
                }
            }
        }
        Card { title: "내 계정",
            div { class: "menu-list",
                Link {
                    class: "menu-row",
                    to: Route::Notifications {},
                    "aria-label": badges::unread_label("알림", unread),
                    Icon { name: IconName::Bell }
                    span { class: "menu-row-label", "알림" }
                    if unread > 0 {
                        span { class: "nav-count", "aria-hidden": "true", "{badges::count_text(unread)}" }
                    }
                    Icon { name: IconName::Chevron }
                }
                Link { class: "menu-row", to: Route::Profile {},
                    Icon { name: IconName::Person }
                    span { class: "menu-row-label", "내 정보" }
                    Icon { name: IconName::Chevron }
                }
                button {
                    class: "menu-row menu-row-button",
                    r#type: "button",
                    onclick: move |_| session::logout(session, nav),
                    Icon { name: IconName::Logout }
                    span { class: "menu-row-label", "로그아웃" }
                }
            }
        }
    }
}

#[component]
pub fn NotFound(segments: Vec<String>) -> Element {
    let _ = segments;
    rsx! {
        main { class: "auth",
            EmptyState { title: "페이지를 찾을 수 없어요.", icon: IconName::Alert,
                Link { class: "btn btn-grey", to: Route::Home {}, "홈으로" }
            }
        }
    }
}
