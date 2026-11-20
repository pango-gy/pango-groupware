//! 조직·직책 관리: 부서 트리(추가·수정·삭제)와 직책 목록.

use dioxus::prelude::*;
use pango_api::org::{DepartmentDto, MemberSummary, PositionDto};
use pango_api::{Empty, Versioned};
use pango_core::id::{DepartmentId, PositionId};

use crate::api;
use crate::org_tree;
use crate::text;
use crate::ui::{
    loaded_or_empty, report_error, report_form_error, report_success, resource_view, Button,
    ButtonKind, Card, ConfirmModal, EmptyState, FormError, Icon, IconName, MemberPicker, Modal,
    PageHeader, PickedMember, SelectField, Tabs, TextField,
};

use self::logic::{DepartmentForm, PositionForm};

pub mod logic {
    //! 조직 관리 폼의 변환과 검증(순수 함수).

    use std::ops::RangeInclusive;

    use pango_api::org::{DepartmentDto, DepartmentInput, PositionDto, PositionInput};
    use pango_core::error::{DomainError, DomainResult, ValidationReason};
    use pango_core::id::{DepartmentId, MemberId};
    use pango_core::text::LabelName;

    /// 서버와 같은 범위(`service::org`).
    pub const SORT_ORDER_RANGE: RangeInclusive<i64> = -1_000_000..=1_000_000;
    pub const LEVEL_RANGE: RangeInclusive<i64> = 0..=1000;

    /// 정수 입력. 비어 있으면 0.
    pub fn parse_number(
        raw: &str,
        field: &'static str,
        range: RangeInclusive<i64>,
    ) -> DomainResult<i64> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(0);
        }
        let value: i64 = trimmed
            .parse()
            .map_err(|_| DomainError::validation(field, ValidationReason::InvalidFormat))?;
        if range.contains(&value) {
            Ok(value)
        } else {
            Err(DomainError::validation(field, ValidationReason::OutOfRange))
        }
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct DepartmentForm {
        pub name: String,
        /// 상위 부서 ID 또는 `""`(최상위).
        pub parent_id: String,
        pub head_member_id: Option<MemberId>,
        /// 화면 표시용 조직장 이름.
        pub head_name: String,
        pub sort_order: String,
    }

    impl DepartmentForm {
        pub fn from_dto(d: &DepartmentDto) -> Self {
            Self {
                name: d.name.clone(),
                parent_id: d.parent_id.map(|id| id.to_string()).unwrap_or_default(),
                head_member_id: d.head_member_id,
                head_name: d.head_name.clone().unwrap_or_default(),
                sort_order: d.sort_order.to_string(),
            }
        }

        pub fn to_input(&self) -> DomainResult<DepartmentInput> {
            let parent_id = crate::format::non_empty(&self.parent_id)
                .map(|raw| {
                    DepartmentId::parse(raw).map_err(|_| {
                        DomainError::validation("parent_id", ValidationReason::InvalidFormat)
                    })
                })
                .transpose()?;
            Ok(DepartmentInput {
                name: LabelName::parse(&self.name)?.into_string(),
                parent_id,
                head_member_id: self.head_member_id,
                sort_order: parse_number(&self.sort_order, "sort_order", SORT_ORDER_RANGE)?,
            })
        }
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct PositionForm {
        pub name: String,
        pub level: String,
    }

    impl PositionForm {
        pub fn from_dto(p: &PositionDto) -> Self {
            Self {
                name: p.name.clone(),
                level: p.level.to_string(),
            }
        }

        pub fn to_input(&self) -> DomainResult<PositionInput> {
            Ok(PositionInput {
                name: LabelName::parse(&self.name)?.into_string(),
                level: parse_number(&self.level, "level", LEVEL_RANGE)?,
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::org_tree::tests::dept;

        fn field_of<T>(result: DomainResult<T>) -> Option<(&'static str, ValidationReason)> {
            match result {
                Err(DomainError::Validation { field, reason }) => Some((field, reason)),
                _ => None,
            }
        }

        #[test]
        fn numbers_default_to_zero_and_respect_bounds() {
            assert_eq!(parse_number("", "level", LEVEL_RANGE), Ok(0));
            assert_eq!(parse_number(" 1000 ", "level", LEVEL_RANGE), Ok(1000));
            assert_eq!(
                field_of(parse_number("1001", "level", LEVEL_RANGE)),
                Some(("level", ValidationReason::OutOfRange))
            );
            assert_eq!(
                field_of(parse_number("-1", "level", LEVEL_RANGE)),
                Some(("level", ValidationReason::OutOfRange))
            );
            assert_eq!(
                parse_number("-1000000", "sort_order", SORT_ORDER_RANGE),
                Ok(-1_000_000)
            );
            for bad in ["1.5", "abc", "1e3", "99999999999999999999"] {
                assert_eq!(
                    field_of(parse_number(bad, "sort_order", SORT_ORDER_RANGE)),
                    Some(("sort_order", ValidationReason::InvalidFormat)),
                    "{bad}"
                );
            }
        }

        #[test]
        fn department_form_round_trips() {
            let parent = dept("본부", None, 0);
            let mut team = dept("팀", Some(parent.id), 7);
            team.head_member_id = Some(MemberId::new());
            team.head_name = Some("홍길동".into());
            let form = DepartmentForm::from_dto(&team);
            assert_eq!(form.head_name, "홍길동");
            let input = form.to_input().expect("valid");
            assert_eq!(input.parent_id, Some(parent.id));
            assert_eq!(input.head_member_id, team.head_member_id);
            assert_eq!(input.sort_order, 7);
            assert_eq!(input.name, "팀");
        }

        #[test]
        fn department_form_rejects_bad_input() {
            let blank = DepartmentForm {
                name: "   ".into(),
                ..DepartmentForm::default()
            };
            assert_eq!(field_of(blank.to_input()).map(|f| f.0), Some("name"));
            let long = DepartmentForm {
                name: "가".repeat(51),
                ..DepartmentForm::default()
            };
            assert_eq!(field_of(long.to_input()).map(|f| f.0), Some("name"));
            let bad_parent = DepartmentForm {
                name: "팀".into(),
                parent_id: "nope".into(),
                ..DepartmentForm::default()
            };
            assert_eq!(
                field_of(bad_parent.to_input()).map(|f| f.0),
                Some("parent_id")
            );
            let new = DepartmentForm {
                name: " 새 부서 ".into(),
                ..DepartmentForm::default()
            };
            let input = new.to_input().expect("valid");
            assert_eq!(
                (input.name.as_str(), input.parent_id, input.sort_order),
                ("새 부서", None, 0)
            );
        }

        #[test]
        fn position_form_validates() {
            let ok = PositionForm {
                name: "팀장".into(),
                level: "50".into(),
            };
            assert_eq!(ok.to_input().expect("valid").level, 50);
            let empty = PositionForm::default();
            assert_eq!(field_of(empty.to_input()).map(|f| f.0), Some("name"));
            let high = PositionForm {
                name: "회장".into(),
                level: "2000".into(),
            };
            assert_eq!(field_of(high.to_input()).map(|f| f.0), Some("level"));
        }
    }
}

const DEPARTMENTS: &str = "departments";
const POSITIONS: &str = "positions";

/// 삭제 확인을 기다리는 대상.
#[derive(Debug, Clone, PartialEq)]
enum ArchiveTarget {
    Department(DepartmentDto),
    Position(PositionDto),
}

impl ArchiveTarget {
    fn name(&self) -> &str {
        match self {
            Self::Department(d) => &d.name,
            Self::Position(p) => &p.name,
        }
    }

    fn path(&self) -> String {
        match self {
            Self::Department(d) => format!("/admin/departments/{}?version={}", d.id, d.version),
            Self::Position(p) => format!("/admin/positions/{}?version={}", p.id, p.version),
        }
    }
}

/// 열려 있는 편집 창. `None`이면 새로 만들기.
#[derive(Debug, Clone, PartialEq)]
enum Editing {
    Department(Option<DepartmentDto>),
    Position(Option<PositionDto>),
}

#[component]
pub fn AdminOrg() -> Element {
    let mut tab = use_signal(|| DEPARTMENTS.to_owned());
    let mut editing = use_signal(|| None::<Editing>);
    let mut archiving = use_signal(|| None::<ArchiveTarget>);
    let mut busy = use_signal(|| false);
    let mut departments =
        use_resource(|| async { api::get::<Vec<DepartmentDto>>("/departments").await });
    let mut positions = use_resource(|| async { api::get::<Vec<PositionDto>>("/positions").await });

    let archive = move |_| async move {
        let Some(target) = archiving() else {
            return;
        };
        if busy() {
            return;
        }
        busy.set(true);
        let result = api::delete::<Empty>(&target.path()).await;
        busy.set(false);
        archiving.set(None);
        match result {
            Ok(_) => report_success(format!("{}을(를) 삭제했습니다.", target.name())),
            Err(failure) => report_error(&failure),
        }
        match target {
            ArchiveTarget::Department(_) => departments.restart(),
            ArchiveTarget::Position(_) => positions.restart(),
        }
    };

    let department_list = loaded_or_empty(&departments);
    let on_departments = tab() == DEPARTMENTS;

    rsx! {
        PageHeader {
            title: "조직·직책",
            subtitle: "부서 구조와 조직장, 직책을 관리합니다.",
            actions: rsx! {
                if on_departments {
                    Button { onclick: move |_| editing.set(Some(Editing::Department(None))),
                        Icon { name: IconName::Plus }
                        "부서 추가"
                    }
                } else {
                    Button { onclick: move |_| editing.set(Some(Editing::Position(None))),
                        Icon { name: IconName::Plus }
                        "직책 추가"
                    }
                }
            },
        }
        Tabs {
            tabs: vec![
                (DEPARTMENTS.to_owned(), "부서".to_owned(), None),
                (POSITIONS.to_owned(), "직책".to_owned(), None),
            ],
            active: tab(),
            onselect: move |key| tab.set(key),
        }
        if on_departments {
            {resource_view(departments, move |list: Vec<DepartmentDto>| {
                let rows = org_tree::department_tree(&list);
                rsx! {
                    if rows.is_empty() {
                        EmptyState { title: "아직 부서가 없습니다.", hint: "부서를 추가해 조직을 구성하세요.", icon: IconName::Building }
                    } else {
                        Card {
                            ul { class: "list org-tree", "aria-label": "부서 목록",
                                for row in rows {
                                    li {
                                        key: "{row.department.id}",
                                        class: "list-row org-row",
                                        style: "--depth: {row.indent()}",
                                        div { class: "list-row-main",
                                            strong {
                                                if row.depth > 0 {
                                                    span { class: "org-branch", "aria-hidden": "true", "└" }
                                                }
                                                "{row.department.name}"
                                            }
                                            small {
                                                {row.department.head_name.clone().map(|name| format!("조직장 {name}")).unwrap_or_else(|| "조직장 없음".into())}
                                                " · 구성원 {row.department.member_count}명"
                                            }
                                        }
                                        div { class: "row-actions",
                                            button {
                                                class: "btn btn-ghost btn-sm",
                                                r#type: "button",
                                                "aria-label": "{row.department.name} 수정",
                                                onclick: {
                                                    let d = row.department.clone();
                                                    move |_| editing.set(Some(Editing::Department(Some(d.clone()))))
                                                },
                                                "수정"
                                            }
                                            button {
                                                class: "btn btn-ghost btn-sm",
                                                r#type: "button",
                                                "aria-label": "{row.department.name} 삭제",
                                                onclick: {
                                                    let d = row.department.clone();
                                                    move |_| archiving.set(Some(ArchiveTarget::Department(d.clone())))
                                                },
                                                "삭제"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            })}
        } else {
            {resource_view(positions, move |list: Vec<PositionDto>| {
                let sorted = org_tree::sorted_positions(&list);
                rsx! {
                    if sorted.is_empty() {
                        EmptyState { title: "아직 직책이 없습니다.", hint: "팀장·매니저처럼 구성원에게 붙일 직책을 추가하세요.", icon: IconName::List }
                    } else {
                        Card {
                            ul { class: "list", "aria-label": "직책 목록",
                                for position in sorted {
                                    li { key: "{position.id}", class: "list-row",
                                        div { class: "list-row-main",
                                            strong { "{position.name}" }
                                            small { "등급 {position.level}" }
                                        }
                                        div { class: "row-actions",
                                            button {
                                                class: "btn btn-ghost btn-sm",
                                                r#type: "button",
                                                "aria-label": "{position.name} 수정",
                                                onclick: {
                                                    let p = position.clone();
                                                    move |_| editing.set(Some(Editing::Position(Some(p.clone()))))
                                                },
                                                "수정"
                                            }
                                            button {
                                                class: "btn btn-ghost btn-sm",
                                                r#type: "button",
                                                "aria-label": "{position.name} 삭제",
                                                onclick: {
                                                    let p = position.clone();
                                                    move |_| archiving.set(Some(ArchiveTarget::Position(p.clone())))
                                                },
                                                "삭제"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            })}
        }
        {match editing() {
            Some(Editing::Department(target)) => rsx! {
                DepartmentModal {
                    target,
                    departments: department_list,
                    onclose: move |_| editing.set(None),
                    onsaved: move |_| {
                        editing.set(None);
                        departments.restart();
                    },
                }
            },
            Some(Editing::Position(target)) => rsx! {
                PositionModal {
                    target,
                    onclose: move |_| editing.set(None),
                    onsaved: move |_| {
                        editing.set(None);
                        positions.restart();
                    },
                }
            },
            None => rsx! {},
        }}
        if let Some(target) = archiving() {
            ConfirmModal {
                title: "{target.name()}을(를) 삭제할까요?",
                confirm_label: "삭제",
                danger: true,
                busy: busy(),
                onconfirm: archive,
                oncancel: move |_| archiving.set(None),
                {match target {
                    ArchiveTarget::Department(_) => rsx! {
                        p { "구성원이나 하위 부서가 남아 있으면 삭제할 수 없습니다. 먼저 구성원의 부서를 옮기고 하위 부서를 정리해 주세요." }
                    },
                    ArchiveTarget::Position(_) => rsx! {
                        p { "이 직책을 쓰는 구성원이 있으면 삭제할 수 없습니다. 먼저 구성원의 직책을 바꿔 주세요." }
                    },
                }}
            }
        }
    }
}

/// 저장 실패 처리. 다른 사람이 먼저 고쳤으면 창을 닫고 목록을 새로 읽는다.
fn handle_save_failure(failure: &api::ApiError, error: Signal<String>, onsaved: EventHandler<()>) {
    report_form_error(failure, error);
    if failure.code == "version_conflict" {
        onsaved.call(());
    }
}

#[component]
fn DepartmentModal(
    /// 수정할 부서. `None`이면 새 부서.
    target: Option<DepartmentDto>,
    departments: Vec<DepartmentDto>,
    onclose: EventHandler<()>,
    onsaved: EventHandler<()>,
) -> Element {
    let mut form = use_signal(|| {
        target
            .as_ref()
            .map(DepartmentForm::from_dto)
            .unwrap_or_default()
    });
    // 새로 만들 때 쓰는 멱등성 키. 창이 열려 있는 동안 유지한다(두 번 눌러도 한 번만 생성).
    let key = use_signal(api::new_idempotency_key);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let existing: Option<(DepartmentId, i64)> = target.as_ref().map(|d| (d.id, d.version));

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let input = match form.read().to_input() {
            Ok(input) => input,
            Err(invalid) => {
                error.set(text::domain_error_message(&invalid));
                return;
            }
        };
        busy.set(true);
        error.set(String::new());
        let result = match existing {
            Some((id, version)) => {
                api::patch::<_, DepartmentDto>(
                    &format!("/admin/departments/{id}"),
                    &Versioned {
                        version,
                        data: input,
                    },
                )
                .await
            }
            None => {
                let key = key.peek().clone();
                api::post_once::<_, DepartmentDto>("/admin/departments", &input, &key).await
            }
        };
        busy.set(false);
        match result {
            Ok(saved) => {
                report_success(format!("{}을(를) 저장했습니다.", saved.name));
                onsaved.call(());
            }
            Err(failure) => handle_save_failure(&failure, error, onsaved),
        }
    };

    let current = form();
    let parent_options = org_tree::parent_options(&departments, existing.map(|(id, _)| id));
    let head_excluded: Vec<_> = current.head_member_id.into_iter().collect();

    rsx! {
        Modal { title: if existing.is_some() { "부서 수정" } else { "부서 추가" }, onclose,
            form { class: "form", onsubmit: submit, novalidate: true,
                FormError { message: error() }
                TextField {
                    label: "부서 이름",
                    value: current.name.clone(),
                    required: true,
                    maxlength: 50,
                    oninput: move |value| form.write().name = value,
                }
                SelectField {
                    label: "상위 부서",
                    value: current.parent_id.clone(),
                    options: parent_options,
                    onchange: move |value| form.write().parent_id = value,
                }
                div { class: "field",
                    span { class: "field-label", "조직장" }
                    if current.head_member_id.is_some() {
                        PickedMember {
                            name: current.head_name.clone(),
                            clear_label: "조직장 지정 해제",
                            onclear: move |_| {
                                let mut f = form.write();
                                f.head_member_id = None;
                                f.head_name = String::new();
                            },
                        }
                    } else {
                        span { class: "field-hint", "지정하지 않음. 조직장은 이 부서와 하위 부서의 팀 현황을 봅니다." }
                    }
                }
                MemberPicker {
                    label: if current.head_member_id.is_some() { "조직장 바꾸기" } else { "조직장 지정" },
                    exclude: head_excluded,
                    onpick: move |member: MemberSummary| {
                        let mut f = form.write();
                        f.head_member_id = Some(member.id);
                        f.head_name = member.name;
                    },
                }
                TextField {
                    label: "정렬 순서",
                    kind: "number",
                    value: current.sort_order.clone(),
                    hint: "작은 숫자가 위에 보입니다. 같으면 이름 순입니다.",
                    maxlength: 8,
                    oninput: move |value| form.write().sort_order = value,
                }
                div { class: "modal-actions",
                    Button { kind: ButtonKind::Secondary, onclick: move |_| onclose.call(()), "취소" }
                    Button { submit: true, busy: busy(), "저장" }
                }
            }
        }
    }
}

#[component]
fn PositionModal(
    /// 수정할 직책. `None`이면 새 직책.
    target: Option<PositionDto>,
    onclose: EventHandler<()>,
    onsaved: EventHandler<()>,
) -> Element {
    let mut form = use_signal(|| {
        target
            .as_ref()
            .map(PositionForm::from_dto)
            .unwrap_or_default()
    });
    let key = use_signal(api::new_idempotency_key);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let existing: Option<(PositionId, i64)> = target.as_ref().map(|p| (p.id, p.version));

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let input = match form.read().to_input() {
            Ok(input) => input,
            Err(invalid) => {
                error.set(text::domain_error_message(&invalid));
                return;
            }
        };
        busy.set(true);
        error.set(String::new());
        let result = match existing {
            Some((id, version)) => {
                api::patch::<_, PositionDto>(
                    &format!("/admin/positions/{id}"),
                    &Versioned {
                        version,
                        data: input,
                    },
                )
                .await
            }
            None => {
                let key = key.peek().clone();
                api::post_once::<_, PositionDto>("/admin/positions", &input, &key).await
            }
        };
        busy.set(false);
        match result {
            Ok(saved) => {
                report_success(format!("{}을(를) 저장했습니다.", saved.name));
                onsaved.call(());
            }
            Err(failure) => handle_save_failure(&failure, error, onsaved),
        }
    };

    let current = form();
    rsx! {
        Modal { title: if existing.is_some() { "직책 수정" } else { "직책 추가" }, onclose,
            form { class: "form", onsubmit: submit, novalidate: true,
                FormError { message: error() }
                TextField {
                    label: "직책 이름",
                    value: current.name.clone(),
                    required: true,
                    maxlength: 50,
                    oninput: move |value| form.write().name = value,
                }
                TextField {
                    label: "등급",
                    kind: "number",
                    value: current.level.clone(),
                    hint: "0~1000. 숫자가 클수록 높은 직책으로 목록 위에 보입니다.",
                    maxlength: 4,
                    oninput: move |value| form.write().level = value,
                }
                div { class: "modal-actions",
                    Button { kind: ButtonKind::Secondary, onclick: move |_| onclose.call(()), "취소" }
                    Button { submit: true, busy: busy(), "저장" }
                }
            }
        }
    }
}
