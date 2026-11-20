//! 관리 홈(대시보드): 오늘 출근 현황, 처리할 일, 퇴근 누락, 최근 감사 로그, 관리 화면 바로 가기.

use dioxus::prelude::*;
use pango_api::admin::AuditEntryDto;
use pango_api::home::{AdminDashboard, MissingClockOut};

use crate::api;
use crate::format;
use crate::route::Route;
use crate::session::use_session;
use crate::text;
use crate::ui::{now, resource_view, Card, EmptyState, Icon, IconName, PageHeader, Stat, StatLink};

use super::audit::logic as audit_logic;

#[component]
pub fn AdminHome() -> Element {
    let session = use_session();
    let tz = session.zone();
    let dashboard = use_resource(|| async { api::get::<AdminDashboard>("/admin/dashboard").await });
    let at = now();

    rsx! {
        PageHeader { title: "관리 홈" }
        {resource_view(dashboard, move |data: AdminDashboard| {
            let today = logic::today_view(&data.today);
            rsx! {
                div { class: "home-grid",
                    Card { title: "오늘 출근 현황 · {format::date_md(data.today.date)}", class: "home-span",
                        if let Some(note) = today.note {
                            p { class: "today-meta home-note", "{note}" }
                        }
                        div { class: "att-stats dash-stats",
                            for tile in today.tiles {
                                Stat { key: "{tile.label}", label: tile.label, value: tile.value, tone: tile.tone }
                            }
                        }
                    }
                    Card { title: "처리할 일", class: "home-card",
                        div { class: "home-stats",
                            for item in logic::pending_items(&data.pending) {
                                StatLink {
                                    key: "{item.tile.label}",
                                    to: item.to,
                                    label: item.tile.label,
                                    value: item.tile.value,
                                    hint: item.hint,
                                    tone: item.tile.tone,
                                    tab: item.tab,
                                }
                            }
                        }
                    }
                    Card {
                        title: "퇴근 누락 (최근 7일)",
                        action: rsx! {
                            Link { class: "card-link", to: Route::AdminAttendance {}, "근태 관리" }
                        },
                        class: "home-card",
                        MissingList { items: data.missing_clock_out.clone() }
                    }
                    Card {
                        title: "최근 감사 로그",
                        action: rsx! {
                            Link { class: "card-link", to: Route::AdminAudit {}, "전체 보기" }
                        },
                        class: "home-span",
                        if data.recent_audit.is_empty() {
                            EmptyState { title: "기록된 작업이 없습니다.", icon: IconName::List }
                        } else {
                            ul { class: "list",
                                for entry in data.recent_audit.iter() {
                                    AuditLine { key: "{entry.id}", entry: entry.clone(), when: format::short_when(entry.at, at, tz) }
                                }
                            }
                        }
                    }
                }
            }
        })}
        Card { title: "관리 화면",
            div { class: "menu-list dash-links",
                for item in logic::quick_links() {
                    Link { key: "{item.label}", class: "menu-row", to: item.to.clone(),
                        Icon { name: item.icon }
                        span { class: "menu-row-label", "{item.label}" }
                        Icon { name: IconName::Chevron }
                    }
                }
            }
        }
    }
}

#[component]
fn MissingList(items: Vec<MissingClockOut>) -> Element {
    if items.is_empty() {
        return rsx! {
            EmptyState { title: "최근 7일 동안 퇴근 누락이 없습니다.", icon: IconName::Clock }
        };
    }
    rsx! {
        ul { class: "list",
            for item in items {
                li { key: "{item.member_id}", class: "list-row",
                    Link { class: "row-link", to: Route::AdminAttendance {},
                        span { class: "list-row-main",
                            strong { "{item.name}" }
                            small { {logic::missing_line(item.department_name.as_deref(), &item.dates)} }
                        }
                        Icon { name: IconName::Chevron }
                    }
                }
            }
        }
    }
}

#[component]
fn AuditLine(entry: AuditEntryDto, when: String) -> Element {
    rsx! {
        li { class: "list-row",
            Link { class: "row-link", to: Route::AdminAudit {},
                span { class: "list-row-main",
                    strong { title: "{entry.action}", {text::audit_action_label(&entry.action)} }
                    small { "{audit_logic::actor_text(&entry)} · {audit_logic::target_text(&entry)}" }
                }
                small { class: "nowrap dash-when", "{when}" }
            }
        }
    }
}

/// 화면이 그리는 값을 만드는 순수 함수.
mod logic {
    use chrono::NaiveDate;
    use pango_api::home::{DashboardPending, DashboardToday};

    use crate::format;
    use crate::layout::{admin_nav, NavItem};
    use crate::pages::admin::attendance as admin_attendance;
    use crate::pages::admin::leave as admin_leave;
    use crate::pages::attendance::logic::Tile;
    use crate::route::Route;
    use crate::ui::Tone;

    /// 퇴근 누락 줄에 바로 보여 줄 날짜 수.
    pub const DATES_SHOWN: usize = 3;

    pub struct TodayView {
        /// 근무일이 아니면 그 안내. 그때는 미출근·지각을 보여 주지 않는다(놀랄 숫자가 아니다).
        pub note: Option<String>,
        pub tiles: Vec<Tile>,
    }

    fn people(label: &'static str, count: u32, tone: Tone) -> Tile {
        Tile::count(label, count, "명", tone)
    }

    pub fn today_view(today: &DashboardToday) -> TodayView {
        let base = [
            Tile {
                label: "재직",
                value: format!("{}명", today.active_members),
                tone: Tone::Neutral,
            },
            Tile {
                label: "출근",
                value: format!("{}명", today.clocked_in),
                tone: Tone::Neutral,
            },
        ];
        if !today.is_working_day {
            return TodayView {
                note: Some(format!(
                    "{}은 근무일이 아니라 미출근·지각을 세지 않습니다.",
                    format::date_md(today.date)
                )),
                tiles: base
                    .into_iter()
                    .chain([people("휴가", today.on_leave, Tone::Info)])
                    .collect(),
            };
        }
        TodayView {
            note: None,
            tiles: base
                .into_iter()
                .chain([
                    people("미출근", today.not_clocked_in, Tone::Danger),
                    people("휴가", today.on_leave, Tone::Info),
                    people("지각", today.late, Tone::Warning),
                ])
                .collect(),
        }
    }

    /// 처리할 일 한 칸: 숫자와 처리할 화면(과 그 화면에서 열 탭).
    pub struct PendingItem {
        pub tile: Tile,
        pub hint: &'static str,
        pub to: Route,
        pub tab: Option<&'static str>,
    }

    pub fn pending_items(pending: &DashboardPending) -> Vec<PendingItem> {
        vec![
            PendingItem {
                tile: Tile::count("휴가 신청", pending.leave_requests, "건", Tone::Warning),
                hint: "결재 중인 신청",
                to: Route::AdminLeave {},
                tab: Some(admin_leave::TAB_REQUESTS),
            },
            PendingItem {
                tile: Tile::count(
                    "근태 정정",
                    pending.correction_requests,
                    "건",
                    Tone::Warning,
                ),
                hint: "결재 중인 요청",
                to: Route::AdminAttendance {},
                tab: Some(admin_attendance::TAB_CORRECTIONS),
            },
            PendingItem {
                tile: Tile::count("진행 중 결재", pending.approval_documents, "건", Tone::Info),
                hint: "휴가·정정 문서 포함",
                to: Route::AdminApprovalForms {},
                tab: None,
            },
        ]
    }

    /// `개발팀 · 3월 2일 (월), 2월 27일 (금) 외 2일`. 날짜는 서버가 준 순서(최근 먼저) 그대로.
    pub fn missing_line(department: Option<&str>, dates: &[NaiveDate]) -> String {
        let mut shown: Vec<String> = dates
            .iter()
            .take(DATES_SHOWN)
            .map(|d| format::date_md(*d))
            .collect();
        let rest = dates.len().saturating_sub(DATES_SHOWN);
        if shown.is_empty() {
            shown.push("날짜 정보 없음".to_owned());
        }
        let mut days = shown.join(", ");
        if rest > 0 {
            days.push_str(&format!(" 외 {rest}일"));
        }
        match department.map(str::trim).filter(|name| !name.is_empty()) {
            Some(name) => format!("{name} · {days}"),
            None => days,
        }
    }

    /// 관리 화면 바로 가기(관리 홈 자신은 뺀다).
    pub fn quick_links() -> Vec<NavItem> {
        admin_nav()
            .into_iter()
            .filter(|item| item.to != Route::AdminHome {})
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::pages::attendance::logic::fixtures::date;

        fn today(working: bool) -> DashboardToday {
            DashboardToday {
                date: date("2026-03-02"),
                is_working_day: working,
                active_members: 20,
                clocked_in: 15,
                not_clocked_in: 3,
                on_leave: 2,
                late: 0,
            }
        }

        #[test]
        fn working_day_shows_every_tile_with_emphasis() {
            let view = today_view(&today(true));
            assert_eq!(view.note, None);
            let shown: Vec<_> = view
                .tiles
                .iter()
                .map(|t| (t.label, t.value.as_str(), t.tone))
                .collect();
            assert_eq!(
                shown,
                vec![
                    ("재직", "20명", Tone::Neutral),
                    ("출근", "15명", Tone::Neutral),
                    ("미출근", "3명", Tone::Danger),
                    ("휴가", "2명", Tone::Info),
                    ("지각", "0명", Tone::Neutral),
                ]
            );
        }

        #[test]
        fn day_off_says_so_instead_of_counting_absentees() {
            let view = today_view(&today(false));
            let note = view.note.expect("note");
            assert!(note.contains("근무일이 아니"), "{note}");
            let labels: Vec<_> = view.tiles.iter().map(|t| t.label).collect();
            assert_eq!(labels, vec!["재직", "출근", "휴가"]);
        }

        #[test]
        fn pending_items_link_to_their_screens() {
            let items = pending_items(&DashboardPending {
                leave_requests: 2,
                correction_requests: 0,
                approval_documents: 7,
            });
            assert_eq!(items.len(), 3);
            assert_eq!(items[0].to, Route::AdminLeave {});
            assert_eq!(
                items[0].tab,
                Some(admin_leave::TAB_REQUESTS),
                "신청 내역 탭"
            );
            assert_eq!(items[0].tile.tone, Tone::Warning);
            assert_eq!(items[1].to, Route::AdminAttendance {});
            assert_eq!(
                items[1].tab,
                Some(admin_attendance::TAB_CORRECTIONS),
                "정정 요청 탭"
            );
            assert_eq!(items[2].tab, None);
            assert_eq!(items[1].tile.tone, Tone::Neutral, "0건은 강조하지 않는다");
            assert_eq!(items[2].to, Route::AdminApprovalForms {});
            assert_eq!(items[2].tile.value, "7건");
        }

        #[test]
        fn missing_line_caps_dates() {
            let days: Vec<_> = [
                "2026-03-06",
                "2026-03-05",
                "2026-03-04",
                "2026-03-03",
                "2026-03-02",
            ]
            .into_iter()
            .map(date)
            .collect();
            assert_eq!(
                missing_line(Some("개발팀"), &days),
                "개발팀 · 3월 6일 (금), 3월 5일 (목), 3월 4일 (수) 외 2일"
            );
            assert_eq!(missing_line(None, &days[..1]), "3월 6일 (금)");
            assert_eq!(
                missing_line(Some("  "), &days[..3]),
                "3월 6일 (금), 3월 5일 (목), 3월 4일 (수)"
            );
            assert_eq!(missing_line(Some("인사팀"), &[]), "인사팀 · 날짜 정보 없음");
        }

        #[test]
        fn quick_links_skip_the_dashboard_itself() {
            let links = quick_links();
            assert!(!links.is_empty());
            assert!(links.iter().all(|item| item.to != Route::AdminHome {}));
            assert_eq!(links.len(), admin_nav().len() - 1);
        }
    }
}
