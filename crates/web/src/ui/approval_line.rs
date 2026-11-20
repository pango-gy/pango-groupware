//! 결재선 편집기: 결재자 추가·삭제·순서 바꾸기, 단계별 결재/합의 전환, 추천 결재선 불러오기.
//! 휴가 신청과 결재 문서 작성이 함께 쓴다.
//!
//! 편집기는 상태를 갖지 않는다. 바뀐 결재선 전체를 `onchange`로 돌려주고, 부모가 `steps`로 다시 넘긴다.
//! 순서가 곧 결재 순서다. 서버로 보낼 때는 [`to_inputs`]로 바꾼다.
//! 결재선 규칙의 최종 판정은 [`pango_core::approval::ApprovalLine::new`](와 서버)가 한다.
//! 여기서는 명백한 실수(제외 대상·중복·개수 초과)만 입력 단계에서 막는다.

use dioxus::prelude::*;
use pango_api::approval::{LineSuggestion, StepInput, SuggestedStep};
use pango_api::org::MemberSummary;
use pango_core::approval::{StepKind, MAX_STEPS};
use pango_core::id::MemberId;

use super::{report_error, Avatar, Button, ButtonKind, Icon, IconName, MemberPicker};
use crate::api;
use crate::session::use_toasts;
use crate::text;

/// 결재선 한 단계. 화면에 이름을 보여 주려고 `StepInput`에 이름을 더했다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproverEntry {
    pub approver_id: MemberId,
    pub approver_name: String,
    pub kind: StepKind,
}

/// `index` 단계를 한 칸 앞으로. 맨 앞이거나 범위 밖이면 그대로다.
pub fn move_up(steps: &[ApproverEntry], index: usize) -> Vec<ApproverEntry> {
    let mut out = steps.to_vec();
    if index > 0 && index < out.len() {
        out.swap(index - 1, index);
    }
    out
}

/// `index` 단계를 한 칸 뒤로. 맨 뒤이거나 범위 밖이면 그대로다.
pub fn move_down(steps: &[ApproverEntry], index: usize) -> Vec<ApproverEntry> {
    let mut out = steps.to_vec();
    if index.saturating_add(1) < out.len() {
        out.swap(index, index + 1);
    }
    out
}

/// `index` 단계를 뺀다. 범위 밖이면 그대로다.
pub fn remove(steps: &[ApproverEntry], index: usize) -> Vec<ApproverEntry> {
    let mut out = steps.to_vec();
    if index < out.len() {
        out.remove(index);
    }
    out
}

/// `index` 단계의 종류를 바꾼다. 범위 밖이면 그대로다.
pub fn set_kind(steps: &[ApproverEntry], index: usize, kind: StepKind) -> Vec<ApproverEntry> {
    let mut out = steps.to_vec();
    if let Some(step) = out.get_mut(index) {
        step.kind = kind;
    }
    out
}

/// 맨 뒤에 결재 단계를 붙인다. 제외 대상(보통 기안자 본인), 이미 있는 사람,
/// [`MAX_STEPS`]를 넘는 추가는 무시한다.
pub fn add(
    steps: &[ApproverEntry],
    exclude: &[MemberId],
    approver_id: MemberId,
    approver_name: String,
) -> Vec<ApproverEntry> {
    let mut out = steps.to_vec();
    let present = out.iter().any(|step| step.approver_id == approver_id);
    if !exclude.contains(&approver_id) && !present && out.len() < MAX_STEPS {
        out.push(ApproverEntry {
            approver_id,
            approver_name,
            kind: StepKind::Approve,
        });
    }
    out
}

/// 추천 결재선을 편집용 단계로. [`add`]와 같은 규칙으로 거른다.
pub fn from_suggestion(exclude: &[MemberId], suggested: &[SuggestedStep]) -> Vec<ApproverEntry> {
    suggested.iter().fold(Vec::new(), |steps, step| {
        let mut next = add(
            &steps,
            exclude,
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
pub fn to_inputs(steps: &[ApproverEntry]) -> Vec<StepInput> {
    steps
        .iter()
        .map(|step| StepInput {
            approver_id: step.approver_id,
            kind: step.kind,
        })
        .collect()
}

/// 결재자 선택기에서 뺄 사람: 제외 대상과 이미 고른 결재자.
pub fn excluded(exclude: &[MemberId], steps: &[ApproverEntry]) -> Vec<MemberId> {
    exclude
        .iter()
        .copied()
        .chain(steps.iter().map(|step| step.approver_id))
        .collect()
}

/// 서버에 보내기 전에 알 수 있는 결재선 문제. 없으면 `None`.
pub fn line_problem(steps: &[ApproverEntry]) -> Option<&'static str> {
    if steps.is_empty() {
        return Some("결재자를 한 명 이상 지정해 주세요.");
    }
    if steps.len() > MAX_STEPS {
        return Some("결재선은 10단계까지 지정할 수 있어요.");
    }
    if steps.last().map(|step| step.kind) != Some(StepKind::Approve) {
        return Some("마지막 단계는 결재여야 해요.");
    }
    None
}

/// 추천 결재선을 읽어 편집용 단계로 바꾼다(제외 대상은 [`from_suggestion`]이 거른다).
async fn fetch_suggestion(exclude: &[MemberId]) -> api::ApiResult<Vec<ApproverEntry>> {
    let suggestion = api::get::<LineSuggestion>("/approval/line-suggestion").await?;
    Ok(from_suggestion(exclude, &suggestion.steps))
}

/// 추천 결재선으로 미리 채워 두는 결재선 상태. 휴가 신청·근태 정정 요청이 함께 쓴다.
///
/// 추천이 오기 전에 사용자가 고쳤으면 덮어쓰지 않는다. 추천을 읽지 못하면 빈 결재선으로
/// 시작한다(편집기의 "추천 결재선 불러오기"로 다시 받을 수 있다).
#[derive(Clone, Copy)]
pub struct LineDraft {
    steps: Signal<Vec<ApproverEntry>>,
    touched: Signal<bool>,
    suggestion: Resource<()>,
}

impl LineDraft {
    pub fn steps(&self) -> Vec<ApproverEntry> {
        self.steps.read().clone()
    }

    /// 추천을 받아 봤는지(성공·실패 모두). 그 전에는 편집기 대신 불러오는 중을 보여 준다.
    pub fn ready(&self) -> bool {
        self.suggestion.read().is_some()
    }

    /// 사용자가 고친 결재선.
    pub fn set(&mut self, next: Vec<ApproverEntry>) {
        self.touched.set(true);
        self.steps.set(next);
    }
}

/// [`LineDraft`]를 만든다. `exclude`는 보통 기안자 본인이다.
pub fn use_line_draft(exclude: Vec<MemberId>) -> LineDraft {
    let mut steps = use_signal(Vec::<ApproverEntry>::new);
    let touched = use_signal(|| false);
    let suggestion = use_resource(move || {
        let exclude = exclude.clone();
        async move {
            if let Ok(suggested) = fetch_suggestion(&exclude).await {
                if !*touched.peek() {
                    steps.set(suggested);
                }
            }
        }
    });
    LineDraft {
        steps,
        touched,
        suggestion,
    }
}

/// 결재선 편집기.
///
/// - `steps`: 현재 결재선(순서대로).
/// - `onchange`: 추가·삭제·순서·종류가 바뀐 새 목록.
/// - `exclude`: 고를 수 없는 구성원(보통 기안자 본인). 추천 결재선에서도 뺀다.
/// - `disabled`: 보내는 중 등 편집을 막을 때.
/// - `error`: 결재선 아래에 붙여 보여 줄 문구(검증·서버 오류).
#[component]
pub fn ApprovalLineEditor(
    steps: Vec<ApproverEntry>,
    onchange: EventHandler<Vec<ApproverEntry>>,
    #[props(default)] exclude: Vec<MemberId>,
    #[props(default)] disabled: bool,
    #[props(into, default)] error: String,
) -> Element {
    let mut loading = use_signal(|| false);
    let mut toasts = use_toasts();
    let full = steps.len() >= MAX_STEPS;
    let picker_exclude = excluded(&exclude, &steps);
    let count = steps.len();

    let suggest = {
        let exclude = exclude.clone();
        move |_| {
            let exclude = exclude.clone();
            async move {
                if loading() {
                    return;
                }
                loading.set(true);
                match fetch_suggestion(&exclude).await {
                    Ok(next) => {
                        if next.is_empty() {
                            toasts.success("추천할 결재선이 없어요. 결재자를 직접 지정해 주세요.");
                        } else {
                            onchange.call(next);
                        }
                    }
                    Err(error) => report_error(&error),
                }
                loading.set(false);
            }
        }
    };

    rsx! {
        div { class: "line-editor",
            div { class: "line-editor-head",
                span { class: "field-label", "결재선 ({count} / {MAX_STEPS})" }
                Button {
                    kind: ButtonKind::Weak,
                    small: true,
                    busy: loading(),
                    disabled,
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
                            Avatar { name: step.approver_name.clone(), seed: step.approver_id.to_string() }
                            span { class: "line-step-name",
                                span { class: "sr-only", "{index + 1}번째 " }
                                "{step.approver_name}"
                            }
                            div {
                                class: "segmented",
                                role: "group",
                                "aria-label": "{step.approver_name} 단계 종류",
                                for kind in StepKind::ALL.iter().copied() {
                                    button {
                                        key: "{kind.as_str()}",
                                        class: if step.kind == kind { "segmented-item segmented-item-active" } else { "segmented-item" },
                                        r#type: "button",
                                        "aria-pressed": step.kind == kind,
                                        disabled,
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
                                    "aria-label": "{step.approver_name} 앞으로",
                                    disabled: disabled || index == 0,
                                    onclick: {
                                        let steps = steps.clone();
                                        move |_| onchange.call(move_up(&steps, index))
                                    },
                                    Icon { name: IconName::Chevron }
                                }
                                button {
                                    class: "icon-btn line-step-down",
                                    r#type: "button",
                                    "aria-label": "{step.approver_name} 뒤로",
                                    disabled: disabled || index + 1 == count,
                                    onclick: {
                                        let steps = steps.clone();
                                        move |_| onchange.call(move_down(&steps, index))
                                    },
                                    Icon { name: IconName::Chevron }
                                }
                                button {
                                    class: "icon-btn",
                                    r#type: "button",
                                    "aria-label": "{step.approver_name} 빼기",
                                    disabled,
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
                p { class: "field-hint", "결재자는 최대 {MAX_STEPS}명까지 지정할 수 있어요." }
            } else if !disabled {
                MemberPicker {
                    label: "결재자 추가",
                    exclude: picker_exclude,
                    onpick: {
                        let steps = steps.clone();
                        let exclude = exclude.clone();
                        move |member: MemberSummary| {
                            onchange.call(add(&steps, &exclude, member.id, member.name))
                        }
                    },
                }
            }
            if !error.is_empty() {
                p { class: "field-error", role: "alert", "{error}" }
            }
            p { class: "field-hint", "위에서부터 순서대로 처리해요. 마지막 단계는 결재여야 해요." }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str) -> ApproverEntry {
        entry(name, StepKind::Approve)
    }

    fn entry(name: &str, kind: StepKind) -> ApproverEntry {
        ApproverEntry {
            approver_id: MemberId::new(),
            approver_name: name.into(),
            kind,
        }
    }

    fn names(steps: &[ApproverEntry]) -> Vec<&str> {
        steps.iter().map(|s| s.approver_name.as_str()).collect()
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
    fn move_and_remove_stay_in_bounds() {
        let line = vec![
            entry("a", StepKind::Agree),
            entry("b", StepKind::Approve),
            entry("c", StepKind::Approve),
        ];
        assert_eq!(move_up(&line, 0), line);
        assert_eq!(move_down(&line, 2), line);
        assert_eq!(move_up(&line, 9), line);
        assert_eq!(names(&remove(&line, 0)), ["b", "c"]);
        assert_eq!(remove(&line, 3), line);
        assert_eq!(
            set_kind(&line, 0, StepKind::Approve)[0].kind,
            StepKind::Approve
        );
        assert_eq!(set_kind(&line, 7, StepKind::Agree), line);
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
    fn adding_skips_excluded_duplicates_and_overflow() {
        let drafter = MemberId::new();
        let other = MemberId::new();
        let one = add(&[], &[drafter], other, "b".into());
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].kind, StepKind::Approve);
        assert_eq!(add(&one, &[drafter], other, "b".into()), one, "중복");
        assert_eq!(add(&one, &[drafter], drafter, "me".into()), one, "본인");
        let full: Vec<_> = (0..MAX_STEPS).map(|i| step(&i.to_string())).collect();
        assert_eq!(
            add(&full, &[drafter], MemberId::new(), "x".into()).len(),
            MAX_STEPS
        );
    }

    #[test]
    fn add_without_exclusions_skips_duplicates_and_stops_at_max() {
        let kim = MemberId::new();
        let line = add(&[], &[], kim, "김".into());
        assert_eq!(names(&line), ["김"]);
        assert_eq!(line[0].kind, StepKind::Approve);
        assert_eq!(
            add(&line, &[], kim, "김".into()),
            line,
            "같은 사람은 한 번만"
        );
        let full: Vec<_> = (0..MAX_STEPS)
            .map(|i| entry(&i.to_string(), StepKind::Approve))
            .collect();
        assert_eq!(
            add(&full, &[], MemberId::new(), "초과".into()).len(),
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
            &[drafter],
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
        assert_eq!(from_suggestion(&[drafter], &many).len(), MAX_STEPS);
        assert!(from_suggestion(&[drafter], &[]).is_empty());
    }

    #[test]
    fn suggestion_round_trips_to_step_inputs() {
        let id = MemberId::new();
        let line = from_suggestion(
            &[],
            &[SuggestedStep {
                approver_id: id,
                approver_name: "팀장".into(),
                kind: StepKind::Approve,
            }],
        );
        assert_eq!(names(&line), ["팀장"]);
        assert_eq!(
            to_inputs(&line),
            vec![StepInput {
                approver_id: id,
                kind: StepKind::Approve
            }]
        );
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
            excluded(&[drafter], &steps),
            vec![drafter, steps[0].approver_id, steps[1].approver_id]
        );
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
}
