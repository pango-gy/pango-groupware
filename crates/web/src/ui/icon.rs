//! 선으로 그린 아이콘. 외부 자산 없이 SVG 경로만 쓴다.

use dioxus::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconName {
    Home,
    Clock,
    Calendar,
    /// 일정이 표시된 달력(캘린더 메뉴). 휴가의 `Calendar`와 구분한다.
    Schedule,
    Document,
    Megaphone,
    Bell,
    People,
    Person,
    Team,
    Settings,
    Shield,
    Chevron,
    Close,
    Plus,
    Check,
    Alert,
    Inbox,
    Menu,
    Logout,
    Building,
    List,
}

impl IconName {
    /// 24×24 기준 경로들.
    const fn paths(self) -> &'static [&'static str] {
        match self {
            Self::Home => &["M3 11l9-8 9 8", "M5 10v10h5v-6h4v6h5V10"],
            Self::Clock => &["M12 3a9 9 0 100 18 9 9 0 000-18z", "M12 7v5l3 2"],
            Self::Calendar => &["M4 6h16v14H4z", "M4 10h16", "M8 3v4", "M16 3v4"],
            Self::Schedule => &[
                "M4 6h16v14H4z",
                "M4 10h16",
                "M8 3v4",
                "M16 3v4",
                "M15.5 13.5a2 2 0 100 4 2 2 0 000-4z",
            ],
            Self::Document => &["M6 3h8l4 4v14H6z", "M14 3v4h4", "M9 12h6", "M9 16h6"],
            Self::Megaphone => &["M4 10v4h3l8 4V6l-8 4z", "M18 9a4 4 0 010 6"],
            Self::Bell => &["M6 17V11a6 6 0 0112 0v6l2 2H4z", "M10 21h4"],
            Self::People => &[
                "M9 11a3.5 3.5 0 100-7 3.5 3.5 0 000 7z",
                "M2.5 20a6.5 6.5 0 0113 0",
                "M16 4.5a3.5 3.5 0 010 6.5",
                "M18 14.5a6.5 6.5 0 013.5 5.5",
            ],
            Self::Person => &["M12 12a4 4 0 100-8 4 4 0 000 8z", "M4 21a8 8 0 0116 0"],
            Self::Team => &["M4 5h7v6H4z", "M13 13h7v6h-7z", "M7.5 11v5H13", "M16.5 13V8H11"],
            Self::Settings => &[
                "M12 15a3 3 0 100-6 3 3 0 000 6z",
                "M12 2v3M12 19v3M4.2 4.2l2.1 2.1M17.7 17.7l2.1 2.1M2 12h3M19 12h3M4.2 19.8l2.1-2.1M17.7 6.3l2.1-2.1",
            ],
            Self::Shield => &["M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z", "M9 12l2 2 4-4"],
            Self::Chevron => &["M9 6l6 6-6 6"],
            Self::Close => &["M6 6l12 12", "M18 6L6 18"],
            Self::Plus => &["M12 5v14", "M5 12h14"],
            Self::Check => &["M5 12l5 5 9-10"],
            Self::Alert => &["M12 3l9.5 17h-19z", "M12 10v4", "M12 17v.5"],
            Self::Inbox => &["M4 13l2-8h12l2 8v6H4z", "M4 13h5a3 3 0 006 0h5"],
            Self::Menu => &["M4 7h16", "M4 12h16", "M4 17h16"],
            Self::Logout => &["M10 4H5v16h5", "M14 8l4 4-4 4", "M18 12H9"],
            Self::Building => &["M5 21V4h9v17", "M14 9h5v12", "M8 8h3M8 12h3M8 16h3", "M3 21h18"],
            Self::List => &["M8 7h12", "M8 12h12", "M8 17h12", "M4 7h.5M4 12h.5M4 17h.5"],
        }
    }
}

#[component]
pub fn Icon(name: IconName) -> Element {
    rsx! {
        svg {
            class: "icon",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.7",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            for d in name.paths() {
                path { d: "{d}" }
            }
        }
    }
}
