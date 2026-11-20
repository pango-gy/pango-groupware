//! 결재선 편집기: 결재자 추가·삭제·순서 바꾸기, 단계별 결재/합의 전환, 추천 결재선 불러오기.
//!
//! 편집기는 상태를 갖지 않는다. 바뀐 결재선 전체를 `onchange`로 돌려주고, 부모가 `steps`로 다시 넘긴다.
//! 결재선 규칙의 최종 판정은 [`pango_core::approval::ApprovalLine::new`](와 서버)가 한다.
//! 여기서는 명백한 실수(본인·중복·개수 초과)만 입력 단계에서 막는다.

use dioxus::prelude::*;
use pango_api::approval::{LineSuggestion, StepInput, SuggestedStep};
use pango_api::org::MemberSummary;
use pango_core::approval::{StepKind, MAX_STEPS};
use pango_core::id::MemberId;

use super::{report_error, Button, ButtonKind, Icon, IconName, MemberPicker};
use crate::api;
use crate::session::use_toasts;
use crate::text;

/// 편집 중인 결재 단계 하나. 이름은 화면 표시용이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineStepDraft {
    pub approver_id: MemberId,
    pub name: String,
    pub kind: StepKind,
}

/// `index` 단계를 한 칸 앞으로. 맨 앞이거나 범위 밖이면 그대로다.
pub fn move_up(steps: &[LineStepDraft], index: usize) -> Vec<LineStepDraft> {
    let mut out = steps.to_vec();
    if index > 0 && index < out.len() {
        out.swap(index - 1, index);
    }
    out
}

/// `index` 단계를 한 칸 뒤로. 맨 뒤이거나 범위 밖이면 그대로다.
pub fn move_down(steps: &[LineStepDraft], index: usize) -> Vec<LineStepDraft> {
    let mut out = steps.to_vec();
    if index.saturating_add(1) < out.len() {
        out.swap(index, index + 1);
    }
    out
}

/// `index` 단계를 뺀다. 범위 밖이면 그대로다.
pub fn remove(steps: &[LineStepDraft], index: usize) -> Vec<LineStepDraft> {
    let mut out = steps.to_vec();
    if index < out.len() {
        out.remove(index);
    }
    out
}

/// `index` 단계의 종류를 바꾼다. 범위 밖이면 그대로다.
pub fn set_kind(steps: &[LineStepDraft], index: usize, kind: StepKind) -> Vec<LineStepDraft> {
    let mut out = steps.to_vec();
    if let Some(step) = out.get_mut(index) {
        step.kind = kind;
    }
    out
}

/// 맨 뒤에 결재 단계를 붙인다. 기안자 본인, 이미 있는 사람, [`MAX_STEPS`]를 넘는 추가는 무시한다.
pub fn add(
    steps: &[LineStepDraft],
    drafter: MemberId,
    approver_id: MemberId,
    name: String,
) -> Vec<LineStepDraft> {
    let mut out = steps.to_vec();
    let present = out.iter().any(|step| step.approver_id == approver_id);
    if approver_id != drafter && !present && out.len() < MAX_STEPS {
        out.push(LineStepDraft {
            approver_id,
            name,
            kind: StepKind::Approve,
        });
    }
    out
}

/// 추천 결재선을 편집용 단계로. [`add`]와 같은 규칙으로 거른다.
pub fn from_suggestion(drafter: MemberId, suggested: &[SuggestedStep]) -> Vec<LineStepDraft> {
    suggested.iter().fold(Vec::new(), |steps, step| {
        let mut next = add(
            &steps,
            drafter,
            step.approver_id,
            step.approver_name.clone(),
        );
        if next.len() > steps.len() {
            if let Some(last) = next.last_mut() {
                last.kind = step.kind;
            }
        }
        next
    })
}

/// 요청 본문의 결재선.
pub fn to_inputs(steps: &[LineStepDraft]) -> Vec<StepInput> {
    steps
        .iter()
        .map(|step| StepInput {
            approver_id: step.approver_id,
            kind: step.kind,
        })
        .collect()
}

/// 결재자 선택기에서 뺄 사람: 기안자와 이미 고른 결재자.
pub fn excluded(drafter: MemberId, steps: &[LineStepDraft]) -> Vec<MemberId> {
    std::iter::once(drafter)
        .chain(steps.iter().map(|step| step.approver_id))
        .collect()
}

#[component]
pub fn ApprovalLineEditor(
    steps: Vec<LineStepDraft>,
    onchange: EventHandler<Vec<LineStepDraft>>,
    drafter: MemberId,
) -> Element {
    let mut loading = use_signal(|| false);
    let mut toasts = use_toasts();
    let full = steps.len() >= MAX_STEPS;
    let exclude = excluded(drafter, &steps);
    let count = steps.len();

    let suggest = move |_| async move {
        if loading() {
            return;
        }
        loading.set(true);
        match api::get::<LineSuggestion>("/approval/line-suggestion").await {
            Ok(suggestion) => {
                let next = from_suggestion(drafter, &suggestion.steps);
                if next.is_empty() {
                    toasts.success("추천할 결재선이 없습니다. 결재자를 직접 지정해 주세요.");
                } else {
                    onchange.call(next);
                }
            }
            Err(error) => report_error(&error),
        }
        loading.set(false);
    };

    rsx! {
        div { class: "line-editor",
            div { class: "line-editor-head",
                span { class: "field-label", "결재선 ({count} / {MAX_STEPS})" }
                Button {
                    kind: ButtonKind::Ghost,
                    small: true,
                    busy: loading(),
                    onclick: suggest,
                    "추천 결재선 불러오기"
                }
            }
            if steps.is_empty() {
                p { class: "line-empty", "결재자를 추가해 주세요." }
            } else {
                ol { class: "line-steps",
                    for (index , step) in steps.iter().enumerate() {
                        li { key: "{step.approver_id}", class: "line-step",
                            span { class: "line-step-order", "aria-hidden": "true", "{index + 1}" }
                            span { class: "line-step-name",
                                span { class: "sr-only", "{index + 1}번째 " }
                                "{step.name}"
                            }
                            div {
                                class: "segmented",
                                role: "group",
                                "aria-label": "{step.name} 단계 종류",
                                for kind in StepKind::ALL.iter().copied() {
                                    button {
                                        key: "{kind.as_str()}",
                                        class: if step.kind == kind { "segmented-item segmented-item-active" } else { "segmented-item" },
                                        r#type: "button",
                                        "aria-pressed": step.kind == kind,
                                        onclick: {
                                            let steps = steps.clone();
                                            move |_| onchange.call(set_kind(&steps, index, kind))
                                        },
                                        "{text::step_kind_label(kind)}"
                                    }
                                }
                            }
                            div { class: "line-step-actions",
                                button {
                                    class: "icon-btn line-step-up",
                                    r#type: "button",
                                    "aria-label": "{step.name} 앞으로",
                                    disabled: index == 0,
                                    onclick: {
                                        let steps = steps.clone();
                                        move |_| onchange.call(move_up(&steps, index))
                                    },
                                    Icon { name: IconName::Chevron }
                                }
                                button {
                                    class: "icon-btn line-step-down",
                                    r#type: "button",
                                    "aria-label": "{step.name} 뒤로",
                                    disabled: index + 1 == count,
                                    onclick: {
                                        let steps = steps.clone();
                                        move |_| onchange.call(move_down(&steps, index))
                                    },
                                    Icon { name: IconName::Chevron }
                                }
                                button {
                                    class: "icon-btn",
                                    r#type: "button",
                                    "aria-label": "{step.name} 빼기",
                                    onclick: {
                                        let steps = steps.clone();
                                        move |_| onchange.call(remove(&steps, index))
                                    },
                                    Icon { name: IconName::Close }
                                }
                            }
                        }
                    }
                }
            }
            if full {
                p { class: "field-hint", "결재자는 최대 {MAX_STEPS}명까지 지정할 수 있습니다." }
            } else {
                MemberPicker {
                    label: "결재자 추가",
                    exclude,
                    onpick: {
                        let steps = steps.clone();
                        move |member: MemberSummary| {
                            onchange.call(add(&steps, drafter, member.id, member.name))
                        }
                    },
                }
            }
            p { class: "field-hint", "위에서부터 순서대로 처리합니다. 마지막 단계는 결재여야 합니다." }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str) -> LineStepDraft {
        LineStepDraft {
            approver_id: MemberId::new(),
            name: name.into(),
            kind: StepKind::Approve,
        }
    }

    fn names(steps: &[LineStepDraft]) -> Vec<&str> {
        steps.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn moving_swaps_neighbours_and_ignores_edges() {
        let steps = vec![step("a"), step("b"), step("c")];
        assert_eq!(names(&move_up(&steps, 1)), ["b", "a", "c"]);
        assert_eq!(names(&move_up(&steps, 0)), ["a", "b", "c"]);
        assert_eq!(names(&move_up(&steps, 9)), ["a", "b", "c"]);
        assert_eq!(names(&move_down(&steps, 1)), ["a", "c", "b"]);
        assert_eq!(names(&move_down(&steps, 2)), ["a", "b", "c"]);
        assert_eq!(names(&move_down(&steps, usize::MAX)), ["a", "b", "c"]);
        assert!(move_up(&[], 0).is_empty());
        assert!(move_down(&[], 0).is_empty());
    }

    #[test]
    fn removing_and_changing_kind_respect_bounds() {
        let steps = vec![step("a"), step("b")];
        assert_eq!(names(&remove(&steps, 0)), ["b"]);
        assert_eq!(names(&remove(&steps, 5)), ["a", "b"]);
        assert!(remove(&[], 0).is_empty());
        let changed = set_kind(&steps, 1, StepKind::Agree);
        assert_eq!(changed[1].kind, StepKind::Agree);
        assert_eq!(changed[0].kind, StepKind::Approve);
        assert_eq!(set_kind(&steps, 7, StepKind::Agree), steps);
    }

    #[test]
    fn adding_skips_drafter_duplicates_and_overflow() {
        let drafter = MemberId::new();
        let other = MemberId::new();
        let one = add(&[], drafter, other, "b".into());
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].kind, StepKind::Approve);
        assert_eq!(add(&one, drafter, other, "b".into()), one, "중복");
        assert_eq!(add(&one, drafter, drafter, "me".into()), one, "본인");
        let full: Vec<_> = (0..MAX_STEPS).map(|i| step(&i.to_string())).collect();
        assert_eq!(
            add(&full, drafter, MemberId::new(), "x".into()).len(),
            MAX_STEPS
        );
    }

    #[test]
    fn suggestion_is_filtered_like_manual_adds() {
        let drafter = MemberId::new();
        let head = MemberId::new();
        let suggested = |id, kind| SuggestedStep {
            approver_id: id,
            approver_name: "n".into(),
            kind,
        };
        let line = from_suggestion(
            drafter,
            &[
                suggested(head, StepKind::Agree),
                suggested(head, StepKind::Approve),
                suggested(drafter, StepKind::Approve),
            ],
        );
        assert_eq!(line.len(), 1);
        assert_eq!(line[0].approver_id, head);
        assert_eq!(
            line[0].kind,
            StepKind::Agree,
            "처음 나온 단계의 종류를 쓴다"
        );
        let many: Vec<_> = (0..MAX_STEPS + 3)
            .map(|_| suggested(MemberId::new(), StepKind::Approve))
            .collect();
        assert_eq!(from_suggestion(drafter, &many).len(), MAX_STEPS);
        assert!(from_suggestion(drafter, &[]).is_empty());
    }

    #[test]
    fn inputs_keep_order_and_kind() {
        let steps = vec![
            step("a"),
            set_kind(&[step("b")], 0, StepKind::Agree).remove(0),
        ];
        let inputs = to_inputs(&steps);
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].approver_id, steps[0].approver_id);
        assert_eq!(inputs[1].kind, StepKind::Agree);
        let drafter = MemberId::new();
        assert_eq!(
            excluded(drafter, &steps),
            vec![drafter, steps[0].approver_id, steps[1].approver_id]
        );
    }
}
