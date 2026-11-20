//! 로그인 후 화면의 뼈대: 데스크톱은 좌측 사이드바, 모바일은 상단 바 + 하단 탭.

use dioxus::prelude::*;
use pango_api::auth::Me;
use pango_core::authz::Capabilities;

use crate::badges::{self, provide_badges, use_badges};
use crate::route::Route;
use crate::session::{self, use_session, SessionState};
use crate::text;
use crate::ui::{Avatar, EmptyState, ErrorState, Icon, IconName, Spinner};

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
///
/// 서버에 닿지 못했으면(오프라인·서버 재시작) 로그인 화면으로 보내지 않고 다시 시도를 보여 준다.
/// 권한이 없는 화면(관리·팀)은 내용 대신 안내를 보여 준다. 권한은 403을 받거나 앱으로 돌아올 때
/// 다시 읽으므로(`session`), 관리자에서 해제되면 이 안내로 바뀐다.
#[component]
pub fn Shell() -> Element {
    let session = use_session();
    let nav = navigator();
    let route = use_route::<Route>();
    let badges = provide_badges();

    // 로그인 상태가 정해지면 배지를 한 번 읽는다.
    use_effect(move || {
        if session.me().is_some() {
            spawn(badges.refresh());
        }
    });

    use_effect(move || match gate(&session.state()) {
        Gate::Login => {
            nav.replace(Route::Login {});
        }
        Gate::ChangePassword => {
            nav.replace(Route::ChangePassword {});
        }
        Gate::Wait | Gate::Unreachable(_) | Gate::Show(_) => {}
    });

    let me = match gate(&session.state()) {
        Gate::Show(me) => me,
        Gate::Unreachable(message) => {
            return rsx! {
                main { class: "auth",
                    ErrorState { message, onretry: move |_| session.retry() }
                }
            };
        }
        Gate::Wait | Gate::Login | Gate::ChangePassword => {
            return rsx! { div { class: "boot", Spinner {} } };
        }
    };
    let allowed = route_allowed(&route, &me.capabilities);
    rsx! {
        div { class: "shell",
            Sidebar { me: me.clone() }
            div { class: "shell-main",
                TopBar { me }
                main { class: "content",
                    if allowed {
                        Outlet::<Route> {}
                    } else {
                        EmptyState {
                            title: "권한이 없습니다",
                            hint: "이 화면을 볼 권한이 없습니다. 권한이 바뀌었다면 관리자에게 문의해 주세요.",
                            icon: IconName::Shield,
                            Link { class: "btn btn-secondary", to: Route::Home {}, "홈으로" }
                        }
                    }
                }
            }
            TabBar {}
        }
    }
}

/// 로그인 상태에 따라 셸이 할 일.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// 아직 모른다(첫 조회 중).
    Wait,
    Login,
    ChangePassword,
    /// 서버에 닿지 못했다. 이 문구와 다시 시도를 보여 준다.
    Unreachable(String),
    Show(Me),
}

pub fn gate(state: &SessionState) -> Gate {
    match state {
        SessionState::Loading => Gate::Wait,
        SessionState::SignedOut => Gate::Login,
        SessionState::Unreachable(message) => Gate::Unreachable(message.clone()),
        SessionState::SignedIn(me) if me.must_change_password => Gate::ChangePassword,
        SessionState::SignedIn(me) => Gate::Show(me.clone()),
    }
}

/// 이 화면을 메뉴에 보여 주는 권한이 있는지. [`nav_sections`]와 같은 기준이다.
/// 서버가 매 요청 다시 판정하므로 이것은 안내용이다.
pub fn route_allowed(route: &Route, capabilities: &Capabilities) -> bool {
    match route {
        Route::Team {} => capabilities.team || capabilities.admin,
        Route::AdminHome {}
        | Route::AdminMembers {}
        | Route::AdminOrg {}
        | Route::AdminAttendance {}
        | Route::AdminLeave {}
        | Route::AdminApprovalForms {}
        | Route::AdminNotices {}
        | Route::AdminPolicy {}
        | Route::AdminAudit {} => capabilities.admin,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pango_core::authz::Role;

    fn me(capabilities: Capabilities, must_change_password: bool) -> Me {
        Me {
            id: pango_core::id::MemberId::new(),
            login_id: "me".into(),
            name: "나".into(),
            email: None,
            phone: None,
            role: Role::Member,
            department_id: None,
            department_name: None,
            position_id: None,
            position_name: None,
            capabilities,
            must_change_password,
            company_name: "회사".into(),
            timezone: "Asia/Seoul".into(),
        }
    }

    const MEMBER: Capabilities = Capabilities {
        admin: false,
        manage_roles: false,
        team: false,
    };

    #[test]
    fn gate_sends_to_login_only_when_signed_out() {
        assert_eq!(gate(&SessionState::Loading), Gate::Wait);
        assert_eq!(gate(&SessionState::SignedOut), Gate::Login);
        assert_eq!(
            gate(&SessionState::Unreachable("끊김".into())),
            Gate::Unreachable("끊김".into())
        );
        let forced = me(MEMBER, true);
        assert_eq!(gate(&SessionState::SignedIn(forced)), Gate::ChangePassword);
        let normal = me(MEMBER, false);
        assert_eq!(
            gate(&SessionState::SignedIn(normal.clone())),
            Gate::Show(normal)
        );
    }

    #[test]
    fn admin_and_team_routes_need_their_capability() {
        let head = Capabilities {
            team: true,
            ..MEMBER
        };
        let admin = Capabilities {
            admin: true,
            ..MEMBER
        };
        assert!(route_allowed(&Route::Home {}, &MEMBER));
        assert!(route_allowed(&Route::Profile {}, &MEMBER));
        assert!(!route_allowed(&Route::Team {}, &MEMBER));
        assert!(route_allowed(&Route::Team {}, &head));
        assert!(route_allowed(&Route::Team {}, &admin));
        for item in admin_nav() {
            assert!(!route_allowed(&item.to, &MEMBER), "{}", item.label);
            assert!(!route_allowed(&item.to, &head), "{}", item.label);
            assert!(route_allowed(&item.to, &admin), "{}", item.label);
        }
        // 메뉴에 보이는 항목은 모두 열 수 있다.
        for caps in [MEMBER, head, admin] {
            for (_, items) in nav_sections(&me(caps, false)) {
                assert!(items.iter().all(|item| route_allowed(&item.to, &caps)));
            }
        }
    }
}
