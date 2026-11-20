//! 팀 현황(조직장·관리자): 하루 출근 현황, 팀원별 월 근태, 팀 휴가(준비 중).

use dioxus::prelude::*;
use pango_api::attendance::TeamAttendance;
use pango_api::org::MemberSummary;
use pango_core::id::MemberId;

use crate::api;
use crate::pages::attendance::{logic as attendance_logic, MonthAttendance};
use crate::session::use_session;
use crate::ui::{
    resource_view, Avatar, Badge, Button, ButtonKind, Card, EmptyState, IconName, PageHeader,
    Spinner, Stat, Tabs, TextField,
};

use logic::TeamTab;

#[component]
pub fn Team() -> Element {
    let session = use_session();
    let tz = session.zone();
    let mut tab = use_signal(|| TeamTab::Today);
    // `None`이면 서버가 정한 오늘 근무일을 쓴다(근무일 전환 시각을 화면이 알 필요가 없다).
    let mut date = use_signal(|| None::<String>);
    let mut selected = use_signal(|| None::<(MemberId, String)>);

    let attendance = use_resource(move || async move {
        api::get::<TeamAttendance>(&logic::team_path(date().as_deref())).await
    });
    let members = use_resource(|| async { api::get::<Vec<MemberSummary>>("/team/members").await });

    // 조직장이 아니면 서버가 403을 준다. 오류 화면 대신 안내를 보여 준다.
    if logic::is_forbidden(&*members.read()) || logic::is_forbidden(&*attendance.read()) {
        return rsx! {
            PageHeader { title: "팀 현황" }
            EmptyState {
                title: "팀 현황은 조직장과 관리자만 볼 수 있습니다.",
                hint: "부서장으로 지정되면 이 화면을 쓸 수 있습니다.",
                icon: IconName::Team,
            }
        };
    }

    let shown_date = date().unwrap_or_else(|| match &*attendance.read() {
        Some(Ok(team)) => team.date.to_string(),
        _ => String::new(),
    });
    let mut pick = move |id: MemberId, name: String| {
        selected.set(Some((id, name)));
        tab.set(TeamTab::Members);
    };

    rsx! {
        PageHeader { title: "팀 현황" }
        Tabs {
            tabs: TeamTab::ALL
                .iter()
                .map(|t| (t.key().to_owned(), t.label().to_owned(), None))
                .collect::<Vec<_>>(),
            active: tab().key(),
            onselect: move |key: String| tab.set(TeamTab::from_key(&key)),
        }
        {match tab() {
            TeamTab::Today => rsx! {
                div { class: "att-filters",
                    TextField {
                        label: "날짜",
                        kind: "date",
                        value: shown_date,
                        oninput: move |value: String| date.set(logic::date_choice(&value)),
                    }
                    if date().is_some() {
                        Button { kind: ButtonKind::Secondary, onclick: move |_| date.set(None), "오늘로" }
                    }
                }
                {resource_view(attendance, move |team: TeamAttendance| rsx! {
                    Card {
                        if !team.is_working_day {
                            p { class: "today-meta", "근무일이 아닌 날입니다." }
                        }
                        div { class: "att-stats",
                            for tile in logic::team_tiles(&team) {
                                Stat { key: "{tile.label}", label: tile.label, value: tile.value, tone: tile.tone }
                            }
                        }
                    }
                    Card { title: "구성원별 현황",
                        if team.members.is_empty() {
                            EmptyState { title: "관할 부서에 재직 중인 구성원이 없습니다.", icon: IconName::People }
                        } else {
                            ul { class: "list",
                                for (entry , cells) in team.members.iter().map(|entry| {
                                    let cells = attendance_logic::record_cells(team.date, entry.record.as_ref(), &entry.evaluation, tz);
                                    (entry, cells)
                                }) {
                                    li { key: "{entry.member_id}", class: "list-row",
                                        Avatar { name: entry.name.clone() }
                                        div { class: "list-row-main",
                                            button {
                                                class: "link-btn",
                                                r#type: "button",
                                                onclick: {
                                                    let (id, name) = (entry.member_id, entry.name.clone());
                                                    move |_| pick(id, name.clone())
                                                },
                                                strong { "{entry.name}" }
                                                span { class: "sr-only", " 월 근태 보기" }
                                            }
                                            small {
                                                {logic::member_line(entry.department_name.as_deref(), &cells)}
                                            }
                                        }
                                        div { class: "list-row-side",
                                            Badge { tone: cells.tone, "{cells.status}" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                })}
            },
            TeamTab::Members => rsx! {
                {resource_view(members, move |list: Vec<MemberSummary>| rsx! {
                    if list.is_empty() {
                        EmptyState { title: "관할 부서에 재직 중인 구성원이 없습니다.", icon: IconName::People }
                    } else {
                        Card { title: "팀원",
                            div { class: "chip-group", role: "group", "aria-label": "팀원 선택",
                                for member in list {
                                    button {
                                        key: "{member.id}",
                                        r#type: "button",
                                        class: if logic::is_selected(selected().as_ref(), member.id) { "chip chip-active" } else { "chip" },
                                        "aria-pressed": logic::is_selected(selected().as_ref(), member.id),
                                        onclick: {
                                            let (id, name) = (member.id, member.name.clone());
                                            move |_| pick(id, name.clone())
                                        },
                                        "{member.name}"
                                    }
                                }
                            }
                        }
                    }
                })}
                if let Some((member_id, name)) = selected() {
                    h2 { class: "att-section-title", "{name}님의 근태" }
                    MonthAttendance { member_id: Some(member_id), revision: 0_u32 }
                } else if members.read().is_none() {
                    Spinner {}
                } else {
                    EmptyState { title: "팀원을 고르면 그 달의 근태를 볼 수 있습니다.", icon: IconName::Person }
                }
            },
            // 휴가 API가 합쳐지면 이 자리에 팀 휴가 달력을 붙인다.
            TeamTab::Leave => rsx! {
                EmptyState { title: "팀 휴가 달력은 준비 중입니다.", icon: IconName::Calendar }
            },
        }}
    }
}

/// 화면이 그리는 값을 만드는 순수 함수.
mod logic {
    use pango_api::attendance::TeamAttendance;
    use pango_core::attendance::DayStatus;
    use pango_core::id::MemberId;

    use crate::api::ApiResult;
    use crate::pages::attendance::logic::{RecordCells, Tile, EMPTY};
    use crate::ui::Tone;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TeamTab {
        Today,
        Members,
        /// 휴가 API가 합쳐질 때 채울 자리.
        Leave,
    }

    impl TeamTab {
        pub const ALL: [TeamTab; 3] = [Self::Today, Self::Members, Self::Leave];

        pub const fn key(self) -> &'static str {
            match self {
                Self::Today => "today",
                Self::Members => "members",
                Self::Leave => "leave",
            }
        }

        pub const fn label(self) -> &'static str {
            match self {
                Self::Today => "출근 현황",
                Self::Members => "팀원 근태",
                Self::Leave => "휴가",
            }
        }

        /// 모르는 키는 첫 탭으로.
        pub fn from_key(key: &str) -> Self {
            Self::ALL
                .into_iter()
                .find(|tab| tab.key() == key)
                .unwrap_or(Self::Today)
        }
    }

    /// `/team/attendance[?date=YYYY-MM-DD]`. 날짜는 형식이 맞을 때만 붙인다.
    pub fn team_path(date: Option<&str>) -> String {
        match date.and_then(|raw| pango_core::calendar::parse_date("date", raw).ok()) {
            Some(date) => format!("/team/attendance?date={date}"),
            None => "/team/attendance".to_owned(),
        }
    }

    /// 날짜 입력값. 비우거나 잘못된 값이면 "오늘"(`None`)로 돌아간다.
    pub fn date_choice(raw: &str) -> Option<String> {
        pango_core::calendar::parse_date("date", raw.trim())
            .ok()
            .map(|date| date.to_string())
    }

    pub fn is_forbidden<T>(state: &Option<ApiResult<T>>) -> bool {
        matches!(state, Some(Err(error)) if error.status == 403)
    }

    pub fn is_selected(selected: Option<&(MemberId, String)>, id: MemberId) -> bool {
        selected.is_some_and(|(chosen, _)| *chosen == id)
    }

    /// 구성원 줄의 보조 문구: `개발팀 · 09:02 – 18:10`.
    pub fn member_line(department: Option<&str>, cells: &RecordCells) -> String {
        let times = if cells.clock_in == EMPTY {
            "출근 기록 없음".to_owned()
        } else {
            format!("{} – {}", cells.clock_in, cells.clock_out)
        };
        match department.map(str::trim).filter(|name| !name.is_empty()) {
            Some(name) => format!("{name} · {times}"),
            None => times,
        }
    }

    fn tile(label: &'static str, count: usize, tone: Tone) -> Tile {
        Tile::count(label, u32::try_from(count).unwrap_or(u32::MAX), "명", tone)
    }

    /// 인원 타일: 전체, 출근, 미출근, 지각, 휴가, 퇴근 누락.
    pub fn team_tiles(team: &TeamAttendance) -> Vec<Tile> {
        let count = |pred: fn(DayStatus) -> bool| {
            team.members
                .iter()
                .filter(|m| pred(m.evaluation.status))
                .count()
        };
        let clocked_in = team.members.iter().filter(|m| m.record.is_some()).count();
        let not_in = team
            .members
            .iter()
            .filter(|m| {
                m.record.is_none()
                    && matches!(
                        m.evaluation.status,
                        DayStatus::Scheduled | DayStatus::Absent
                    )
            })
            .count();
        vec![
            Tile {
                label: "전체",
                value: format!("{}명", team.members.len()),
                tone: Tone::Neutral,
            },
            Tile {
                label: "출근",
                value: format!("{clocked_in}명"),
                tone: Tone::Neutral,
            },
            tile("미출근", not_in, Tone::Danger),
            tile(
                "지각",
                count(|s| matches!(s, DayStatus::Late | DayStatus::LateAndEarlyLeave)),
                Tone::Warning,
            ),
            tile("휴가", count(|s| s == DayStatus::OnLeave), Tone::Info),
            tile(
                "퇴근 누락",
                count(|s| s == DayStatus::MissingClockOut),
                Tone::Danger,
            ),
        ]
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::api::ApiError;
        use crate::pages::attendance::logic::fixtures::{date, evaluation, record};
        use pango_api::attendance::TeamAttendanceEntry;

        fn entry(status: DayStatus, clocked: bool) -> TeamAttendanceEntry {
            TeamAttendanceEntry {
                member_id: MemberId::new(),
                name: "구성원".into(),
                department_id: None,
                department_name: None,
                record: clocked.then(|| record("2026-03-02", "2026-03-02T00:00:00Z", None)),
                evaluation: evaluation(status, 0),
            }
        }

        fn error(status: u16) -> ApiError {
            ApiError {
                status,
                code: "forbidden".into(),
                field: None,
                reason: None,
            }
        }

        #[test]
        fn tabs_round_trip_and_fall_back() {
            for tab in TeamTab::ALL {
                assert_eq!(TeamTab::from_key(tab.key()), tab);
                assert!(!tab.label().is_empty());
            }
            assert_eq!(TeamTab::from_key("bogus"), TeamTab::Today);
            assert_eq!(TeamTab::from_key(""), TeamTab::Today);
        }

        #[test]
        fn team_path_only_sends_valid_dates() {
            assert_eq!(team_path(None), "/team/attendance");
            assert_eq!(
                team_path(Some("2026-03-02")),
                "/team/attendance?date=2026-03-02"
            );
            for bad in ["", "2026-3-2", "2026-02-30", "2026-03-02&x=1", "../admin"] {
                assert_eq!(team_path(Some(bad)), "/team/attendance", "{bad}");
            }
            assert_eq!(date_choice(" 2026-03-02 "), Some("2026-03-02".into()));
            assert_eq!(date_choice(""), None);
            assert_eq!(date_choice("2026-13-01"), None);
        }

        #[test]
        fn forbidden_is_only_a_loaded_403() {
            assert!(is_forbidden::<()>(&Some(Err(error(403)))));
            assert!(!is_forbidden::<()>(&Some(Err(error(404)))));
            assert!(!is_forbidden::<()>(&Some(Err(error(0)))));
            assert!(!is_forbidden(&Some(Ok(()))));
            assert!(!is_forbidden::<()>(&None));
        }

        #[test]
        fn selection_matches_by_id() {
            let id = MemberId::new();
            assert!(is_selected(Some(&(id, "a".into())), id));
            assert!(!is_selected(Some(&(MemberId::new(), "a".into())), id));
            assert!(!is_selected(None, id));
        }

        #[test]
        fn member_line_describes_times() {
            let cells = |clock_in: &str, clock_out: &str| RecordCells {
                work_type: EMPTY.into(),
                clock_in: clock_in.into(),
                clock_out: clock_out.into(),
                worked: EMPTY.into(),
                status: "예정",
                tone: Tone::Neutral,
            };
            assert_eq!(member_line(None, &cells(EMPTY, EMPTY)), "출근 기록 없음");
            assert_eq!(
                member_line(Some("개발팀"), &cells("09:02", EMPTY)),
                "개발팀 · 09:02 – -"
            );
            assert_eq!(
                member_line(Some(" "), &cells("09:02", "18:10")),
                "09:02 – 18:10"
            );
        }

        #[test]
        fn team_tiles_count_by_status() {
            let team = TeamAttendance {
                date: date("2026-03-02"),
                is_working_day: true,
                members: vec![
                    entry(DayStatus::Normal, true),
                    entry(DayStatus::Late, true),
                    entry(DayStatus::LateAndEarlyLeave, true),
                    entry(DayStatus::Scheduled, false),
                    entry(DayStatus::Absent, false),
                    entry(DayStatus::OnLeave, false),
                    entry(DayStatus::MissingClockOut, true),
                ],
            };
            let tiles: Vec<_> = team_tiles(&team)
                .into_iter()
                .map(|t| (t.label, t.value, t.tone))
                .collect();
            assert_eq!(
                tiles,
                vec![
                    ("전체", "7명".to_owned(), Tone::Neutral),
                    ("출근", "4명".to_owned(), Tone::Neutral),
                    ("미출근", "2명".to_owned(), Tone::Danger),
                    ("지각", "2명".to_owned(), Tone::Warning),
                    ("휴가", "1명".to_owned(), Tone::Info),
                    ("퇴근 누락", "1명".to_owned(), Tone::Danger),
                ]
            );
        }

        #[test]
        fn empty_team_has_neutral_tiles() {
            let team = TeamAttendance {
                date: date("2026-03-01"),
                is_working_day: false,
                members: Vec::new(),
            };
            assert!(team_tiles(&team)
                .iter()
                .all(|t| t.tone == Tone::Neutral && t.value == "0명"));
        }
    }
}
