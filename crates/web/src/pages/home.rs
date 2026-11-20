//! 홈: 오늘 출퇴근, 오늘 일정, 내 휴가, 처리할 결재, 공지, (조직장·관리자) 팀 현황.
//!
//! `GET /home` 한 번으로 그린다. 출퇴근하면 홈 전체를 다시 읽는다. 오늘 일정은 캘린더 조회를
//! 따로 읽는다(실패해도 홈의 나머지는 보인다).

use dioxus::prelude::*;
use pango_api::home::{HomeNotice, HomeResponse, TeamToday};
use pango_api::leave::BalanceDto;

use crate::api;
use crate::format;
use crate::pages::attendance::TodayBody;
use crate::pages::calendar::TodayEvents;
use crate::pages::leave::{BalanceHeadline, RequestRow};
use crate::pages::notices::NoticeRow;
use crate::route::Route;
use crate::session::use_session;
use crate::ui::{
    now, resource_view, today, Badge, Card, EmptyState, IconName, PageHeader, PanelLink, Stat,
    StatLink, Tone,
};

#[component]
pub fn Home() -> Element {
    let session = use_session();
    let tz = session.zone();
    let mut home = use_resource(|| async { api::get::<HomeResponse>("/home").await });
    let name = session.me().map(|me| me.name).unwrap_or_default();
    let at = now();
    let title = logic::greeting(&name, logic::local_hour(at, tz));
    let description = format::date_md(today(tz));

    rsx! {
        PageHeader { title, description }
        {resource_view(home, move |data: HomeResponse| {
            let has_team = data.team_today.is_some();
            rsx! {
                Card { title: "오늘 근무", class: "today-card",
                    TodayBody { state: data.today.clone(), onchange: move |_| home.restart() }
                }
                div { class: "grid-2",
                    TodayEvents { tz }
                    LeaveCard { balance: data.leave, upcoming: data.upcoming_leaves.clone() }
                    ApprovalCard { counts: data.approval }
                    NoticesCard {
                        notices: data.notices.latest.clone(),
                        unread: data.notices.counts.unread,
                        wide: logic::notices_wide(has_team),
                    }
                    if let Some(team) = data.team_today {
                        TeamCard { team }
                    }
                }
            }
        })}
    }
}

#[component]
fn LeaveCard(balance: BalanceDto, upcoming: Vec<pango_api::leave::LeaveRequestDto>) -> Element {
    let shown = logic::upcoming_shown(&upcoming);
    let hidden = upcoming.len().saturating_sub(shown.len());
    rsx! {
        Card {
            title: "휴가",
            action: rsx! {
                PanelLink { to: Route::Leave {}, label: "휴가 현황" }
            },
            div { class: "home-balance",
                BalanceHeadline { dto: balance }
                Link { class: "btn btn-weak", to: Route::LeaveNew {}, "휴가 신청" }
            }
            h3 { class: "sub-title home-sub", "다가오는 휴가" }
            if shown.is_empty() {
                p { class: "field-hint", "30일 안에 잡힌 휴가가 없어요." }
            } else {
                ul { class: "list",
                    for item in shown {
                        RequestRow { key: "{item.id}", item: item.clone() }
                    }
                }
                if hidden > 0 {
                    p { class: "field-hint", "외 {hidden}건은 휴가 화면에서 볼 수 있어요." }
                }
            }
        }
    }
}

#[component]
fn ApprovalCard(counts: pango_api::approval::ApprovalCounts) -> Element {
    rsx! {
        Card {
            title: "결재",
            action: rsx! {
                PanelLink { to: Route::ApprovalList {}, label: "결재함" }
            },
            div { class: "stats",
                for tile in logic::approval_tiles(&counts) {
                    StatLink {
                        key: "{tile.label}",
                        to: Route::ApprovalList {},
                        label: tile.label,
                        value: tile.value,
                        tone: tile.tone,
                    }
                }
            }
            if logic::approval_all_clear(&counts) {
                p { class: "field-hint home-sub", "처리할 결재가 없어요." }
            }
        }
    }
}

#[component]
fn NoticesCard(notices: Vec<HomeNotice>, unread: u32, wide: bool) -> Element {
    let tz = use_session().zone();
    let at = now();
    rsx! {
        Card {
            title: "공지",
            action: rsx! {
                if unread > 0 {
                    Badge { tone: Tone::Info, {logic::unread_badge(unread)} }
                }
                PanelLink { to: Route::Notices {}, label: "전체 보기" }
            },
            class: if wide { "grid-span" } else { "" },
            if notices.is_empty() {
                EmptyState { title: "게시된 공지가 없어요.", icon: IconName::Megaphone }
            } else {
                ul { class: "list",
                    for notice in notices {
                        NoticeRow {
                            key: "{notice.id}",
                            id: notice.id,
                            title: notice.title.clone(),
                            pinned: notice.pinned,
                            must_read: notice.must_read,
                            read: notice.read,
                            meta: vec![format::short_when(notice.published_at, at, tz)],
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn TeamCard(team: TeamToday) -> Element {
    rsx! {
        Card {
            title: "팀 현황",
            action: rsx! {
                PanelLink { to: Route::Team {}, label: "자세히" }
            },
            if team.total == 0 {
                EmptyState { title: "관할 부서에 재직 중인 구성원이 없어요.", icon: IconName::People }
            } else {
                if !team.is_working_day {
                    p { class: "text-sub panel-note", "오늘은 근무일이 아니에요." }
                }
                div { class: "stats",
                    for tile in logic::team_tiles(&team) {
                        Stat { key: "{tile.label}", label: tile.label, value: tile.value, tone: tile.tone }
                    }
                }
            }
        }
    }
}

/// 화면이 그리는 값을 만드는 순수 함수.
mod logic {
    use chrono::Timelike;
    use chrono_tz::Tz;
    use pango_api::approval::ApprovalCounts;
    use pango_api::home::TeamToday;
    use pango_api::leave::LeaveRequestDto;
    use pango_core::time::Timestamp;

    use crate::badges;
    use crate::pages::attendance::logic::Tile;
    use crate::ui::Tone;

    /// 홈에 바로 보여 줄 다가오는 휴가 수. 나머지는 휴가 화면에서 본다.
    pub const UPCOMING_SHOWN: usize = 3;

    /// 회사 시간대의 지금 시(0~23).
    pub fn local_hour(at: Timestamp, tz: Tz) -> u32 {
        at.to_utc().with_timezone(&tz).hour()
    }

    /// 인사말. 이름이 비었으면 이름 없이.
    pub fn greeting(name: &str, hour: u32) -> String {
        let phrase = match hour {
            5..=11 => "좋은 아침이에요",
            12..=17 => "좋은 오후예요",
            _ => "오늘도 수고 많으셨어요",
        };
        match name.trim() {
            "" => phrase.to_owned(),
            name => format!("{name}님, {phrase}"),
        }
    }

    pub fn upcoming_shown(list: &[LeaveRequestDto]) -> Vec<LeaveRequestDto> {
        list.iter().take(UPCOMING_SHOWN).cloned().collect()
    }

    /// 결재 숫자: 결재 대기(있으면 강조), 진행 중인 내 문서, 안 읽은 참조.
    pub fn approval_tiles(counts: &ApprovalCounts) -> Vec<Tile> {
        vec![
            Tile::count("결재 대기", counts.todo, "건", Tone::Warning),
            Tile::count(
                "진행 중인 내 문서",
                counts.in_progress_drafted,
                "건",
                Tone::Neutral,
            ),
            Tile::count("안 읽은 참조", counts.unread_reference, "건", Tone::Neutral),
        ]
    }

    pub fn approval_all_clear(counts: &ApprovalCounts) -> bool {
        counts.todo == 0 && counts.in_progress_drafted == 0 && counts.unread_reference == 0
    }

    /// 공지 카드 제목 옆 배지 문구(안 읽은 공지가 있을 때만 보인다).
    pub fn unread_badge(unread: u32) -> String {
        format!("안 읽음 {}", badges::count_text(unread))
    }

    /// 팀 카드가 없으면 공지 카드가 줄을 혼자 차지한다(빈 칸이 생기지 않게).
    pub fn notices_wide(has_team: bool) -> bool {
        !has_team
    }

    /// 팀 오늘 숫자: 출근, 미출근(근무일에만 강조), 휴가.
    pub fn team_tiles(team: &TeamToday) -> Vec<Tile> {
        let not_yet = if team.is_working_day {
            Tile::count("미출근", team.not_yet, "명", Tone::Danger)
        } else {
            // 근무일이 아니면 출근하지 않은 것이 정상이다.
            Tile {
                label: "미출근",
                value: "-".to_owned(),
                tone: Tone::Neutral,
            }
        };
        vec![
            Tile {
                label: "출근",
                value: format!("{} / {}명", team.present, team.total),
                tone: Tone::Neutral,
            },
            not_yet,
            Tile::count("휴가", team.on_leave, "명", Tone::Info),
        ]
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::pages::attendance::logic::fixtures::{date, ts};

        fn counts(todo: u32, drafted: u32, reference: u32) -> ApprovalCounts {
            ApprovalCounts {
                todo,
                in_progress_drafted: drafted,
                unread_reference: reference,
            }
        }

        #[test]
        fn greeting_follows_the_company_clock() {
            let tz = crate::format::zone("Asia/Seoul");
            // 2026-03-02 23:30 UTC = 03-03 08:30 KST
            assert_eq!(local_hour(ts("2026-03-02T23:30:00Z"), tz), 8);
            assert_eq!(greeting("홍길동", 5), "홍길동님, 좋은 아침이에요");
            assert_eq!(greeting("홍길동", 11), "홍길동님, 좋은 아침이에요");
            assert_eq!(greeting("홍길동", 12), "홍길동님, 좋은 오후예요");
            assert_eq!(greeting("홍길동", 17), "홍길동님, 좋은 오후예요");
            assert_eq!(greeting("홍길동", 18), "홍길동님, 오늘도 수고 많으셨어요");
            assert_eq!(greeting("홍길동", 4), "홍길동님, 오늘도 수고 많으셨어요");
            assert_eq!(greeting("  ", 9), "좋은 아침이에요");
            assert_eq!(greeting(" 김 ", 99), "김님, 오늘도 수고 많으셨어요");
        }

        #[test]
        fn waiting_approvals_are_emphasised_only_when_present() {
            let tiles = approval_tiles(&counts(3, 2, 1));
            assert_eq!(tiles[0].value, "3건");
            assert_eq!(tiles[0].tone, Tone::Warning);
            assert!(tiles[1..].iter().all(|t| t.tone == Tone::Neutral));
            let none = approval_tiles(&counts(0, 0, 0));
            assert!(none
                .iter()
                .all(|t| t.tone == Tone::Neutral && t.value == "0건"));
            assert!(approval_all_clear(&counts(0, 0, 0)));
            assert!(!approval_all_clear(&counts(0, 0, 1)));
            assert!(!approval_all_clear(&counts(0, 1, 0)));
            assert!(!approval_all_clear(&counts(1, 0, 0)));
            let huge = approval_tiles(&counts(u32::MAX, 0, 0));
            assert_eq!(huge[0].value, format!("{}건", u32::MAX));
        }

        #[test]
        fn notices_title_and_width() {
            assert_eq!(unread_badge(4), "안 읽음 4");
            assert_eq!(unread_badge(150), "안 읽음 99+");
            assert!(notices_wide(false));
            assert!(!notices_wide(true));
        }

        fn team(working: bool, present: u32, not_yet: u32, on_leave: u32) -> TeamToday {
            TeamToday {
                date: date("2026-03-02"),
                is_working_day: working,
                total: present + not_yet + on_leave,
                present,
                not_yet,
                on_leave,
            }
        }

        #[test]
        fn team_tiles_do_not_alarm_on_days_off() {
            let tiles = team_tiles(&team(true, 5, 2, 1));
            let shown: Vec<_> = tiles
                .iter()
                .map(|t| (t.label, t.value.as_str(), t.tone))
                .collect();
            assert_eq!(
                shown,
                vec![
                    ("출근", "5 / 8명", Tone::Neutral),
                    ("미출근", "2명", Tone::Danger),
                    ("휴가", "1명", Tone::Info),
                ]
            );
            let off = team_tiles(&team(false, 0, 0, 0));
            assert_eq!(off[1].value, "-");
            assert_eq!(off[1].tone, Tone::Neutral);
            assert_eq!(team_tiles(&team(true, 3, 0, 0))[1].tone, Tone::Neutral);
        }

        fn leave(day: &str) -> LeaveRequestDto {
            use pango_core::id::{DocumentId, LeaveRequestId, LeaveTypeId, MemberId};
            use pango_core::leave::{LeaveAmount, LeaveRequestStatus, LeaveUnit};
            LeaveRequestDto {
                id: LeaveRequestId::new(),
                member_id: MemberId::new(),
                member_name: "나".into(),
                leave_type_id: LeaveTypeId::new(),
                leave_type_name: "연차".into(),
                start_date: date(day),
                end_date: date(day),
                unit: LeaveUnit::FullDay,
                amount: LeaveAmount::from_quarters(4),
                reason: "쉼".into(),
                status: LeaveRequestStatus::Approved,
                document_id: DocumentId::new(),
                created_at: ts("2026-03-01T00:00:00Z"),
                decided_at: None,
            }
        }

        #[test]
        fn upcoming_list_is_capped_in_server_order() {
            assert!(upcoming_shown(&[]).is_empty());
            let list: Vec<_> = ["2026-03-02", "2026-03-03", "2026-03-04", "2026-03-05"]
                .into_iter()
                .map(leave)
                .collect();
            let shown = upcoming_shown(&list);
            assert_eq!(shown.len(), UPCOMING_SHOWN);
            assert_eq!(shown[0].start_date, date("2026-03-02"));
            assert_eq!(upcoming_shown(&list[..1]).len(), 1);
        }
    }
}
