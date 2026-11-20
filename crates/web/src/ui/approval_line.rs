//! 결재선 편집기. 휴가 신청과 결재 문서 작성이 함께 쓴다.
//!
//! 값은 부모가 들고 있고(제어 컴포넌트), 바뀔 때마다 `onchange`로 새 목록을 받는다.
//! 순서가 곧 결재 순서다. 서버로 보낼 때는 [`to_step_inputs`]로 바꾼다.

use dioxus::prelude::*;
use pango_api::approval::{LineSuggestion, StepInput};
use pango_api::org::MemberSummary;
use pango_core::approval::{StepKind, MAX_STEPS};
use pango_core::id::MemberId;

use super::{Avatar, Icon, IconName, MemberPicker};

/// 결재선 한 단계. 화면에 이름을 보여 주려고 `StepInput`에 이름을 더했다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproverEntry {
    pub approver_id: MemberId,
    pub approver_name: String,
    pub kind: StepKind,
}

/// 추천 결재선을 편집 상태로.
pub fn from_suggestion(suggestion: &LineSuggestion) -> Vec<ApproverEntry> {
    suggestion
        .steps
        .iter()
        .take(MAX_STEPS)
        .map(|step| ApproverEntry {
            approver_id: step.approver_id,
            approver_name: step.approver_name.clone(),
            kind: step.kind,
        })
        .collect()
}

pub fn to_step_inputs(entries: &[ApproverEntry]) -> Vec<StepInput> {
    entries
        .iter()
        .map(|entry| StepInput {
            approver_id: entry.approver_id,
            kind: entry.kind,
        })
        .collect()
}

/// 맨 뒤에 결재자로 추가한다. 이미 있거나 최대 단계 수면 그대로 둔다.
pub fn add_approver(entries: &[ApproverEntry], member: &MemberSummary) -> Vec<ApproverEntry> {
    let mut next = entries.to_vec();
    if next.len() < MAX_STEPS && !next.iter().any(|e| e.approver_id == member.id) {
        next.push(ApproverEntry {
            approver_id: member.id,
            approver_name: member.name.clone(),
            kind: StepKind::Approve,
        });
    }
    next
}

pub fn remove_at(entries: &[ApproverEntry], index: usize) -> Vec<ApproverEntry> {
    let mut next = entries.to_vec();
    if index < next.len() {
        next.remove(index);
    }
    next
}

/// `index` 단계를 한 칸 앞(`up`) 또는 뒤로 옮긴다. 끝에서는 그대로 둔다.
pub fn move_step(entries: &[ApproverEntry], index: usize, up: bool) -> Vec<ApproverEntry> {
    let mut next = entries.to_vec();
    let target = if up {
        index.checked_sub(1)
    } else {
        index.checked_add(1)
    };
    if let Some(target) = target.filter(|t| *t < next.len() && index < next.len()) {
        next.swap(index, target);
    }
    next
}

pub fn set_kind(entries: &[ApproverEntry], index: usize, kind: StepKind) -> Vec<ApproverEntry> {
    let mut next = entries.to_vec();
    if let Some(entry) = next.get_mut(index) {
        entry.kind = kind;
    }
    next
}

/// 서버에 보내기 전에 알 수 있는 결재선 문제. 없으면 `None`.
pub fn line_problem(entries: &[ApproverEntry]) -> Option<&'static str> {
    if entries.is_empty() {
        return Some("결재자를 한 명 이상 지정해 주세요.");
    }
    if entries.len() > MAX_STEPS {
        return Some("결재선은 10단계까지 지정할 수 있습니다.");
    }
    if entries.last().map(|e| e.kind) != Some(StepKind::Approve) {
        return Some("마지막 단계는 결재여야 합니다.");
    }
    None
}

pub fn step_kind_label(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Approve => "결재",
        StepKind::Agree => "합의",
    }
}

/// 결재선 편집기.
///
/// - `steps`: 현재 결재선(순서대로).
/// - `onchange`: 추가·삭제·순서·종류가 바뀐 새 목록.
/// - `exclude`: 고를 수 없는 구성원(보통 기안자 본인).
#[component]
pub fn ApprovalLineEditor(
    steps: Vec<ApproverEntry>,
    onchange: EventHandler<Vec<ApproverEntry>>,
    #[props(default)] exclude: Vec<MemberId>,
    #[props(default)] disabled: bool,
    /// 서버 검증 오류 등 결재선에 붙여 보여 줄 문구.
    #[props(into, default)]
    error: String,
) -> Element {
    let problem = line_problem(&steps).unwrap_or_default();
    let shown_error = if error.is_empty() {
        problem.to_owned()
    } else {
        error
    };
    let picker_exclude: Vec<MemberId> = exclude
        .iter()
        .copied()
        .chain(steps.iter().map(|e| e.approver_id))
        .collect();
    let count = steps.len();
    let full = count >= MAX_STEPS;

    rsx! {
        div { class: "line-editor",
            if steps.is_empty() {
                p { class: "line-empty", "지정된 결재자가 없습니다." }
            } else {
                ol { class: "line-steps",
                    for (index , entry) in steps.iter().cloned().enumerate() {
                        li { key: "{entry.approver_id}", class: "line-step",
                            span { class: "line-order", "aria-hidden": "true", "{index + 1}" }
                            Avatar { name: entry.approver_name.clone() }
                            span { class: "line-name", "{entry.approver_name}" }
                            select {
                                class: "input line-kind",
                                "aria-label": "{entry.approver_name} 단계 종류",
                                disabled,
                                onchange: {
                                    let steps = steps.clone();
                                    move |event: FormEvent| {
                                        let kind = StepKind::parse(&event.value()).unwrap_or(StepKind::Approve);
                                        onchange.call(set_kind(&steps, index, kind));
                                    }
                                },
                                for kind in StepKind::ALL.iter().copied() {
                                    option {
                                        value: "{kind.as_str()}",
                                        selected: kind == entry.kind,
                                        {step_kind_label(kind)}
                                    }
                                }
                            }
                            div { class: "line-actions",
                                button {
                                    class: "icon-btn line-up",
                                    r#type: "button",
                                    "aria-label": "{entry.approver_name} 앞으로",
                                    disabled: disabled || index == 0,
                                    onclick: {
                                        let steps = steps.clone();
                                        move |_| onchange.call(move_step(&steps, index, true))
                                    },
                                    Icon { name: IconName::Chevron }
                                }
                                button {
                                    class: "icon-btn line-down",
                                    r#type: "button",
                                    "aria-label": "{entry.approver_name} 뒤로",
                                    disabled: disabled || index + 1 == count,
                                    onclick: {
                                        let steps = steps.clone();
                                        move |_| onchange.call(move_step(&steps, index, false))
                                    },
                                    Icon { name: IconName::Chevron }
                                }
                                button {
                                    class: "icon-btn",
                                    r#type: "button",
                                    "aria-label": "{entry.approver_name} 빼기",
                                    disabled,
                                    onclick: {
                                        let steps = steps.clone();
                                        move |_| onchange.call(remove_at(&steps, index))
                                    },
                                    Icon { name: IconName::Close }
                                }
                            }
                        }
                    }
                }
            }
            if !disabled && !full {
                MemberPicker {
                    label: "결재자 추가",
                    exclude: picker_exclude,
                    onpick: {
                        let steps = steps.clone();
                        move |member: MemberSummary| onchange.call(add_approver(&steps, &member))
                    },
                }
            }
            if !shown_error.is_empty() {
                span { class: "field-error", role: "alert", "{shown_error}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pango_api::approval::SuggestedStep;
    use pango_core::member::MemberStatus;

    fn entry(name: &str, kind: StepKind) -> ApproverEntry {
        ApproverEntry {
            approver_id: MemberId::new(),
            approver_name: name.into(),
            kind,
        }
    }

    fn member(name: &str) -> MemberSummary {
        MemberSummary {
            id: MemberId::new(),
            name: name.into(),
            email: None,
            phone: None,
            department_id: None,
            department_name: None,
            position_id: None,
            position_name: None,
            status: MemberStatus::Active,
        }
    }

    fn names(entries: &[ApproverEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.approver_name.as_str()).collect()
    }

    #[test]
    fn add_skips_duplicates_and_stops_at_max() {
        let kim = member("김");
        let line = add_approver(&[], &kim);
        assert_eq!(names(&line), ["김"]);
        assert_eq!(line[0].kind, StepKind::Approve);
        assert_eq!(add_approver(&line, &kim), line, "같은 사람은 한 번만");
        let full: Vec<_> = (0..MAX_STEPS)
            .map(|i| entry(&i.to_string(), StepKind::Approve))
            .collect();
        assert_eq!(add_approver(&full, &member("초과")).len(), MAX_STEPS);
    }

    #[test]
    fn move_and_remove_stay_in_bounds() {
        let line = vec![
            entry("a", StepKind::Agree),
            entry("b", StepKind::Approve),
            entry("c", StepKind::Approve),
        ];
        assert_eq!(names(&move_step(&line, 1, true)), ["b", "a", "c"]);
        assert_eq!(names(&move_step(&line, 1, false)), ["a", "c", "b"]);
        assert_eq!(move_step(&line, 0, true), line);
        assert_eq!(move_step(&line, 2, false), line);
        assert_eq!(move_step(&line, 9, true), line);
        assert_eq!(names(&remove_at(&line, 0)), ["b", "c"]);
        assert_eq!(remove_at(&line, 3), line);
        assert_eq!(
            set_kind(&line, 0, StepKind::Approve)[0].kind,
            StepKind::Approve
        );
        assert_eq!(set_kind(&line, 7, StepKind::Agree), line);
    }

    #[test]
    fn problems_match_server_rules() {
        assert!(line_problem(&[]).is_some());
        assert!(line_problem(&[entry("a", StepKind::Agree)]).is_some());
        assert!(
            line_problem(&[entry("a", StepKind::Agree), entry("b", StepKind::Approve)]).is_none()
        );
        let too_many: Vec<_> = (0..=MAX_STEPS)
            .map(|i| entry(&i.to_string(), StepKind::Approve))
            .collect();
        assert!(line_problem(&too_many).is_some());
    }

    #[test]
    fn suggestion_round_trips_to_step_inputs() {
        let id = MemberId::new();
        let suggestion = LineSuggestion {
            steps: vec![SuggestedStep {
                approver_id: id,
                approver_name: "팀장".into(),
                kind: StepKind::Approve,
            }],
        };
        let line = from_suggestion(&suggestion);
        assert_eq!(names(&line), ["팀장"]);
        assert_eq!(
            to_step_inputs(&line),
            vec![StepInput {
                approver_id: id,
                kind: StepKind::Approve
            }]
        );
        for kind in StepKind::ALL {
            assert!(!step_kind_label(*kind).is_empty());
        }
    }
}
