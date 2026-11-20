//! 로그인 후 화면의 뼈대: 데스크톱은 좌측 사이드바, 모바일은 상단 바 + 하단 탭.

use dioxus::prelude::*;
use pango_api::auth::Me;

use crate::badges::{self, provide_badges, use_badges};
use crate::route::Route;
use crate::session::{self, use_session};
use crate::text;
use crate::ui::{Avatar, Icon, IconName, Spinner};

/// 메뉴 한 줄.
#[derive(Clone, PartialEq)]
pub struct NavItem {
    pub to: Route,
    pub label: &'static str,
    pub icon: IconName,
}

const fn item(to: Route, label: &'static str, icon: IconName) -> NavItem {
    NavItem { to, label, icon }
}

/// 모든 구성원에게 보이는 메뉴.
pub fn main_nav() -> Vec<NavItem> {
    vec![
        item(Route::Home {}, "홈", IconName::Home),
        item(Route::Attendance {}, "근태", IconName::Clock),
        item(Route::Leave {}, "휴가", IconName::Calendar),
        item(Route::ApprovalList {}, "결재", IconName::Document),
        item(Route::Notices {}, "공지", IconName::Megaphone),
        item(Route::People {}, "구성원", IconName::People),
    ]
}

/// 조직장(과 관리자)에게만 보이는 메뉴.
pub fn team_nav() -> Vec<NavItem> {
    vec![item(Route::Team {}, "팀 현황", IconName::Team)]
}

/// 관리자에게만 보이는 메뉴.
pub fn admin_nav() -> Vec<NavItem> {
    vec![
        item(Route::AdminHome {}, "관리 홈", IconName::Shield),
        item(Route::AdminMembers {}, "구성원 관리", IconName::Person),
        item(Route::AdminOrg {}, "조직·직책", IconName::Building),
        item(Route::AdminAttendance {}, "근태 관리", IconName::Clock),
        item(Route::AdminLeave {}, "휴가 관리", IconName::Calendar),
        item(
            Route::AdminApprovalForms {},
            "결재 양식",
            IconName::Document,
        ),
        item(Route::AdminNotices {}, "공지 관리", IconName::Megaphone),
        item(Route::AdminPolicy {}, "근무 정책", IconName::Settings),
        item(Route::AdminAudit {}, "감사 로그", IconName::List),
    ]
}

/// 권한에 따라 보여 줄 메뉴 묶음: `(묶음 제목, 항목들)`.
/// 서버가 권한을 다시 확인하므로, 여기서 숨기는 것은 편의일 뿐이다.
pub fn nav_sections(me: &Me) -> Vec<(&'static str, Vec<NavItem>)> {
    let mut sections = vec![("", main_nav())];
    if me.capabilities.team || me.capabilities.admin {
        sections.push(("팀", team_nav()));
    }
    if me.capabilities.admin {
        sections.push(("관리", admin_nav()));
    }
    sections
}

fn is_active(current: &Route, target: &Route) -> bool {
    if *target == (Route::Home {}) || *target == (Route::AdminHome {}) {
        return current == target;
    }
    let (current, target) = (current.to_string(), target.to_string());
    current == target || current.starts_with(&format!("{target}/"))
}

#[component]
fn NavLink(item: NavItem, #[props(default)] compact: bool) -> Element {
    let current = use_route::<Route>();
    let count = use_badges().for_route(&item.to);
    let active = is_active(&current, &item.to);
    let class = match (compact, active) {
        (true, true) => "tabbar-item tabbar-item-active",
        (true, false) => "tabbar-item",
        (false, true) => "nav-item nav-item-active",
        (false, false) => "nav-item",
    };
    rsx! {
        Link { class, to: item.to.clone(), "aria-current": if active { "page" },
            Icon { name: item.icon }
            span { "{item.label}" }
            if count > 0 {
                span { class: "nav-count",
                    span { class: "sr-only", "읽지 않음 " }
                    "{badges::count_text(count)}"
                }
            }
        }
    }
}

#[component]
fn Sidebar(me: Me) -> Element {
    let session = use_session();
    let nav = navigator();
    let logout = move |_| session::logout(session, nav);
    rsx! {
        aside { class: "sidebar",
            div { class: "brand",
                span { class: "brand-mark", "P" }
                span { class: "brand-name", "{me.company_name}" }
            }
            nav { class: "nav", "aria-label": "주 메뉴",
                for (title , items) in nav_sections(&me) {
                    if !title.is_empty() {
                        p { class: "nav-section", "{title}" }
                    }
                    for item in items {
                        NavLink { key: "{item.label}", item }
                    }
                }
            }
            NavLink { item: item(Route::Notifications {}, "알림", IconName::Bell) }
            div { class: "sidebar-footer",
                Link { class: "profile-chip", to: Route::Profile {},
                    Avatar { name: me.name.clone() }
                    span { class: "profile-chip-text",
                        strong { "{me.name}" }
                        small {
                            {me.position_name.clone().unwrap_or_else(|| text::role_label(me.role).to_owned())}
                        }
                    }
                }
                button { class: "icon-btn", r#type: "button", "aria-label": "로그아웃", onclick: logout,
                    Icon { name: IconName::Logout }
                }
            }
        }
    }
}

#[component]
fn TopBar(me: Me) -> Element {
    let unread = use_badges().notifications();
    rsx! {
        header { class: "topbar",
            span { class: "topbar-title", "{me.company_name}" }
            div { class: "topbar-actions",
                Link {
                    class: "icon-btn icon-btn-badged",
                    to: Route::Notifications {},
                    "aria-label": badges::unread_label("알림", unread),
                    Icon { name: IconName::Bell }
                    if unread > 0 {
                        span { class: "icon-badge", "aria-hidden": "true", "{badges::count_text(unread)}" }
                    }
                }
                Link { class: "icon-btn", to: Route::Profile {}, "aria-label": "내 정보",
                    Avatar { name: me.name.clone() }
                }
            }
        }
    }
}

#[component]
fn TabBar() -> Element {
    let tabs = [
        item(Route::Home {}, "홈", IconName::Home),
        item(Route::Attendance {}, "근태", IconName::Clock),
        item(Route::ApprovalList {}, "결재", IconName::Document),
        item(Route::Leave {}, "휴가", IconName::Calendar),
        item(Route::More {}, "더보기", IconName::Menu),
    ];
    rsx! {
        nav { class: "tabbar", "aria-label": "하단 메뉴",
            for tab in tabs {
                NavLink { key: "{tab.label}", item: tab, compact: true }
            }
        }
    }
}

/// 로그인이 필요한 화면의 공통 틀. 로그인 상태가 아니면 로그인 화면으로 보낸다.
#[component]
pub fn Shell() -> Element {
    let session = use_session();
    let nav = navigator();
    let badges = provide_badges();
    let me = session.me();
    let loaded = session.is_loaded();

    // 로그인 상태가 정해지면 배지를 한 번 읽는다.
    use_effect(move || {
        if session.me().is_some() {
            spawn(badges.refresh());
        }
    });

    use_effect(move || {
        if !session.is_loaded() {
            return;
        }
        match session.me() {
            None => {
                nav.replace(Route::Login {});
            }
            Some(me) if me.must_change_password => {
                nav.replace(Route::ChangePassword {});
            }
            Some(_) => {}
        }
    });

    let Some(me) = me.filter(|me| loaded && !me.must_change_password) else {
        return rsx! { div { class: "boot", Spinner {} } };
    };
    rsx! {
        div { class: "shell",
            Sidebar { me: me.clone() }
            div { class: "shell-main",
                TopBar { me }
                main { class: "content", Outlet::<Route> {} }
            }
            TabBar {}
        }
    }
}
