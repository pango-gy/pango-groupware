//! 결재 양식 고르기. 분류별로 묶인 양식을 찾아 하나를 고른다.
//!
//! 최근에 쓴 양식과 별표한 양식은 이 브라우저의 `localStorage`에만 둔다(구성원별 키). 저장소를
//! 쓸 수 없으면(비공개 창 등) 조용히 건너뛰고, 그때는 "자주 쓰는 양식"이 비어 보일 뿐이다.

use dioxus::prelude::*;
use pango_api::approval::{FormGroup, FormListItem};
use pango_core::approval::FormCategory;

use super::{Badge, EmptyState, Icon, IconName, Modal, Tone};
use crate::text;

/// 고른 양식과 그 분류.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedForm {
    pub category: FormCategory,
    pub category_label: String,
    pub form: FormListItem,
}

/// 찾기·묶기·최근 목록. 모두 순수 함수다.
pub mod logic {
    use pango_api::approval::{FormGroup, FormListItem, FormRoute};
    use pango_core::approval::FormCategory;
    use pango_core::id::FormId;

    use super::PickedForm;

    /// "자주 쓰는 양식"에 보여 줄 최근 양식의 수.
    pub const MAX_RECENT: usize = 5;
    /// 별표할 수 있는 양식의 수.
    pub const MAX_STARRED: usize = 20;

    /// 양식을 골랐을 때 갈 곳.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Destination {
        /// 일반 기안 화면에서 이 양식으로 문서를 쓴다.
        Editor,
        /// 휴가 신청 화면(`/leave/new`).
        LeaveNew,
        /// 근태 화면(`/attendance`)의 정정 요청.
        Attendance,
    }

    pub const fn destination(route: FormRoute) -> Destination {
        match route {
            FormRoute::ApprovalNew => Destination::Editor,
            FormRoute::LeaveNew => Destination::LeaveNew,
            FormRoute::Attendance => Destination::Attendance,
        }
    }

    /// 왼쪽 목록에서 고른 범위.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Scope {
        All,
        Category(FormCategory),
    }

    /// 찾기용으로 다듬은 글: 소문자, 공백 없음.
    fn fold(raw: &str) -> String {
        raw.chars()
            .filter(|c| !c.is_whitespace())
            .flat_map(char::to_lowercase)
            .collect()
    }

    /// 검색어에 맞는 양식만 남긴다(이름·설명·분류 이름, 대소문자와 공백 무시). 맞는 양식이 없는
    /// 분류는 빠진다. 검색어가 비면 그대로다.
    pub fn search(groups: &[FormGroup], term: &str) -> Vec<FormGroup> {
        let needle = fold(term);
        if needle.is_empty() {
            return groups.to_vec();
        }
        groups
            .iter()
            .filter_map(|group| {
                let whole = fold(&group.category_label).contains(&needle);
                let forms: Vec<FormListItem> = group
                    .forms
                    .iter()
                    .filter(|form| {
                        whole
                            || fold(&form.name).contains(&needle)
                            || fold(&form.description).contains(&needle)
                    })
                    .cloned()
                    .collect();
                (!forms.is_empty()).then(|| FormGroup {
                    category: group.category,
                    category_label: group.category_label.clone(),
                    forms,
                })
            })
            .collect()
    }

    /// 범위에 드는 묶음.
    pub fn in_scope(groups: &[FormGroup], scope: Scope) -> Vec<FormGroup> {
        groups
            .iter()
            .filter(|group| match scope {
                Scope::All => true,
                Scope::Category(category) => group.category == category,
            })
            .cloned()
            .collect()
    }

    pub fn total(groups: &[FormGroup]) -> usize {
        groups.iter().map(|group| group.forms.len()).sum()
    }

    /// 지금 고른 분류가 목록에 없으면(검색으로 사라짐 등) 전체로 돌아간다.
    pub fn effective_scope(groups: &[FormGroup], scope: Scope) -> Scope {
        match scope {
            Scope::Category(category) if !groups.iter().any(|g| g.category == category) => {
                Scope::All
            }
            other => other,
        }
    }

    fn find<'a>(groups: &'a [FormGroup], id: &str) -> Option<(&'a FormGroup, &'a FormListItem)> {
        groups.iter().find_map(|group| {
            group
                .forms
                .iter()
                .find(|form| form.id.to_string() == id)
                .map(|form| (group, form))
        })
    }

    fn picked(group: &FormGroup, form: &FormListItem) -> PickedForm {
        PickedForm {
            category: group.category,
            category_label: group.category_label.clone(),
            form: form.clone(),
        }
    }

    /// ID로 양식을 찾는다(분류와 함께).
    pub fn pick(groups: &[FormGroup], id: &str) -> Option<PickedForm> {
        find(groups, id).map(|(group, form)| picked(group, form))
    }

    /// "자주 쓰는 양식": 별표한 양식을 앞에, 이어서 최근에 쓴 양식(최대 [`MAX_RECENT`]개).
    /// 지금 목록에 없는 양식(꺼졌거나 검색에 안 맞음)과 겹치는 것은 뺀다.
    pub fn pinned(groups: &[FormGroup], starred: &[String], recent: &[String]) -> Vec<PickedForm> {
        let mut seen: Vec<&str> = Vec::new();
        let mut out = Vec::new();
        let recent = recent.iter().take(MAX_RECENT);
        for id in starred.iter().chain(recent) {
            if seen.contains(&id.as_str()) {
                continue;
            }
            if let Some((group, form)) = find(groups, id) {
                seen.push(id.as_str());
                out.push(picked(group, form));
            }
        }
        out
    }

    /// 방금 쓴 양식을 최근 목록 맨 앞에 둔다(중복 없이, 최대 [`MAX_RECENT`]개).
    pub fn push_recent(recent: &[String], id: &str) -> Vec<String> {
        std::iter::once(id.to_owned())
            .chain(recent.iter().filter(|other| other.as_str() != id).cloned())
            .take(MAX_RECENT)
            .collect()
    }

    /// 별표를 켜거나 끈다. 가득 찼으면 더 켜지 않는다.
    pub fn toggle_star(starred: &[String], id: &str) -> Vec<String> {
        if starred.iter().any(|other| other == id) {
            return starred
                .iter()
                .filter(|other| other.as_str() != id)
                .cloned()
                .collect();
        }
        let mut out = starred.to_vec();
        if out.len() < MAX_STARRED {
            out.push(id.to_owned());
        }
        out
    }

    /// 저장해 둔 ID 목록을 읽는다. 저장소의 값은 믿지 않는다: JSON 문자열 배열이 아니면 빈 목록,
    /// 양식 ID가 아닌 항목과 중복은 버리고 `max`개까지만 받는다.
    pub fn parse_ids(raw: &str, max: usize) -> Vec<String> {
        let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(raw) else {
            return Vec::new();
        };
        let mut out: Vec<String> = Vec::new();
        for item in items {
            let Some(id) = item.as_str().and_then(|raw| FormId::parse(raw).ok()) else {
                continue;
            };
            let id = id.to_string();
            if !out.contains(&id) && out.len() < max {
                out.push(id);
            }
        }
        out
    }

    pub fn encode_ids(ids: &[String]) -> String {
        serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_owned())
    }

    /// `localStorage` 키. 한 브라우저를 여러 구성원이 써도 섞이지 않게 구성원별로 둔다.
    pub fn storage_key(list: &str, member: &str) -> String {
        format!("pango.forms.{list}.{member}")
    }
}

/// 이 브라우저에 남기는 양식 ID 목록(최근·별표). 읽기·쓰기 실패는 기능을 막지 않는다.
mod store {
    use super::logic;

    fn storage() -> Option<web_sys::Storage> {
        // 저장소를 막아 둔 브라우저에서는 `local_storage()`가 예외(Err)를 낸다.
        web_sys::window()?.local_storage().ok().flatten()
    }

    pub fn load(list: &str, member: &str, max: usize) -> Vec<String> {
        storage()
            .and_then(|storage| {
                storage
                    .get_item(&logic::storage_key(list, member))
                    .ok()
                    .flatten()
            })
            .map(|raw| logic::parse_ids(&raw, max))
            .unwrap_or_default()
    }

    pub fn save(list: &str, member: &str, ids: &[String]) {
        if let Some(storage) = storage() {
            // 용량 초과·비공개 창 등으로 못 써도 이번 화면에서는 그대로 쓸 수 있으므로 넘어간다.
            let _ = storage.set_item(&logic::storage_key(list, member), &logic::encode_ids(ids));
        }
    }
}

/// 방금 고른 양식을 최근 목록에 남긴다.
pub fn remember_recent(member: &str, form_id: &str) {
    let recent = store::load("recent", member, logic::MAX_RECENT);
    store::save("recent", member, &logic::push_recent(&recent, form_id));
}

/// 양식 고르기 대화 상자(좁은 화면에서는 시트).
///
/// 왼쪽에 분류와 양식 수, 오른쪽에 양식 목록. 맨 위에는 자주 쓰는 양식(별표 + 최근)이 놓인다.
#[component]
pub fn FormPicker(
    groups: Vec<FormGroup>,
    /// 최근·별표 목록을 나눠 두는 키(구성원 ID).
    #[props(into)]
    member: String,
    onpick: EventHandler<PickedForm>,
    onclose: EventHandler<()>,
) -> Element {
    let mut term = use_signal(String::new);
    let mut scope = use_signal(|| logic::Scope::All);
    let recent = use_hook({
        let member = member.clone();
        move || store::load("recent", &member, logic::MAX_RECENT)
    });
    let mut starred = use_signal({
        let member = member.clone();
        move || store::load("star", &member, logic::MAX_STARRED)
    });

    let found = logic::search(&groups, &term());
    let current = logic::effective_scope(&found, scope());
    let shown = logic::in_scope(&found, current);
    let pinned = if current == logic::Scope::All {
        logic::pinned(&found, &starred(), &recent)
    } else {
        Vec::new()
    };
    let star_list = starred();
    let choose = {
        let member = member.clone();
        move |form: PickedForm| {
            remember_recent(&member, &form.form.id.to_string());
            onpick.call(form);
        }
    };
    let toggle = {
        let member = member.clone();
        move |id: String| {
            let next = logic::toggle_star(&starred(), &id);
            store::save("star", &member, &next);
            starred.set(next);
        }
    };

    rsx! {
        Modal { title: "결재 양식 선택", class: "modal-wide", onclose: move |_| onclose.call(()),
            div { class: "form-picker",
                label { class: "field form-picker-search",
                    span { class: "sr-only", "양식 찾기" }
                    input {
                        class: "input",
                        r#type: "search",
                        value: "{term}",
                        placeholder: "양식 이름이나 설명으로 찾기",
                        maxlength: 50,
                        autocomplete: "off",
                        oninput: move |event| {
                            term.set(event.value());
                            scope.set(logic::Scope::All);
                        },
                    }
                }
                nav { class: "form-picker-cats", "aria-label": "양식 분류",
                    ScopeButton {
                        label: "전체",
                        count: logic::total(&found),
                        active: current == logic::Scope::All,
                        onclick: move |_| scope.set(logic::Scope::All),
                    }
                    for group in found.iter() {
                        ScopeButton {
                            key: "{group.category.as_str()}",
                            label: group.category_label.clone(),
                            count: group.forms.len(),
                            active: current == logic::Scope::Category(group.category),
                            onclick: {
                                let category = group.category;
                                move |_| scope.set(logic::Scope::Category(category))
                            },
                        }
                    }
                }
                div { class: "form-picker-list",
                    if shown.is_empty() {
                        EmptyState {
                            title: if groups.is_empty() { "쓸 수 있는 양식이 없어요." } else { "찾는 양식이 없어요." },
                            hint: if groups.is_empty() { "관리자에게 양식을 요청해 주세요." } else { "다른 말로 찾아보세요." },
                            icon: IconName::Document,
                        }
                    }
                    if !pinned.is_empty() {
                        section { class: "form-picker-group",
                            h3 { class: "form-picker-heading", "자주 쓰는 양식" }
                            ul { class: "list",
                                for entry in pinned {
                                    FormRow {
                                        key: "{entry.form.id}",
                                        starred: star_list.contains(&entry.form.id.to_string()),
                                        category: entry.category_label.clone(),
                                        entry: entry.clone(),
                                        onpick: choose.clone(),
                                        onstar: toggle.clone(),
                                    }
                                }
                            }
                        }
                    }
                    for group in shown {
                        section { key: "{group.category.as_str()}", class: "form-picker-group",
                            h3 { class: "form-picker-heading", "{group.category_label}" }
                            ul { class: "list",
                                for form in group.forms.iter() {
                                    FormRow {
                                        key: "{form.id}",
                                        starred: star_list.contains(&form.id.to_string()),
                                        entry: PickedForm {
                                            category: group.category,
                                            category_label: group.category_label.clone(),
                                            form: form.clone(),
                                        },
                                        onpick: choose.clone(),
                                        onstar: toggle.clone(),
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ScopeButton(
    #[props(into)] label: String,
    count: usize,
    active: bool,
    onclick: EventHandler<()>,
) -> Element {
    rsx! {
        button {
            class: if active { "form-picker-cat form-picker-cat-active" } else { "form-picker-cat" },
            r#type: "button",
            "aria-pressed": active,
            onclick: move |_| onclick.call(()),
            span { class: "form-picker-cat-name", "{label}" }
            span { class: "form-picker-cat-count tnum", "{count}" }
        }
    }
}

/// 양식 한 줄: 이름과 설명(누르면 고름), 오른쪽에 별표.
#[component]
fn FormRow(
    entry: PickedForm,
    starred: bool,
    /// 분류 이름을 함께 보여 줄 때(자주 쓰는 양식).
    #[props(into, default)]
    category: String,
    onpick: EventHandler<PickedForm>,
    onstar: EventHandler<String>,
) -> Element {
    let form = entry.form.clone();
    let id = form.id.to_string();
    let hint = text::form_route_hint(form.route);
    rsx! {
        li { class: "list-row form-picker-row",
            button {
                class: "row-link",
                r#type: "button",
                onclick: move |_| onpick.call(entry.clone()),
                span { class: "list-row-main",
                    span { class: "row-title",
                        strong { "{form.name}" }
                        if !category.is_empty() {
                            Badge { tone: Tone::Neutral, "{category}" }
                        }
                    }
                    if !form.description.is_empty() {
                        small { class: "form-picker-desc", "{form.description}" }
                    }
                    if let Some(hint) = hint {
                        small { class: "form-picker-desc", "{hint}" }
                    }
                }
            }
            button {
                class: if starred { "icon-btn star-btn star-btn-on" } else { "icon-btn star-btn" },
                r#type: "button",
                "aria-pressed": starred,
                "aria-label": if starred { format!("{} 별표 지우기", form.name) } else { format!("{} 별표하기", form.name) },
                onclick: move |_| onstar.call(id.clone()),
                Icon { name: IconName::Star }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::logic::*;
    use pango_api::approval::{FormGroup, FormListItem, FormRoute};
    use pango_core::approval::{DocumentKind, FormCategory};
    use pango_core::form_schema::FormSchema;
    use pango_core::id::FormId;

    fn form(name: &str, description: &str) -> FormListItem {
        FormListItem {
            id: FormId::new(),
            code: "c".into(),
            name: name.into(),
            description: description.into(),
            kind: DocumentKind::General,
            body_template: String::new(),
            use_body: true,
            schema: FormSchema::default(),
            schema_version: 1,
            system: false,
            route: FormRoute::ApprovalNew,
        }
    }

    fn group(category: FormCategory, forms: Vec<FormListItem>) -> FormGroup {
        FormGroup {
            category,
            category_label: category.label().to_owned(),
            forms,
        }
    }

    fn groups() -> Vec<FormGroup> {
        vec![
            group(
                FormCategory::General,
                vec![
                    form("품의서", "일반 품의"),
                    form("업무 협조", "다른 부서에 요청"),
                ],
            ),
            group(
                FormCategory::Trip,
                vec![
                    form("국내출장신청", "국내 출장"),
                    form("해외출장신청", "Overseas Trip"),
                ],
            ),
            group(
                FormCategory::Accounting,
                vec![form("지출결의서", "쓴 비용 정산")],
            ),
        ]
    }

    fn names(groups: &[FormGroup]) -> Vec<String> {
        groups
            .iter()
            .flat_map(|group| group.forms.iter().map(|form| form.name.clone()))
            .collect()
    }

    fn id(groups: &[FormGroup], name: &str) -> String {
        groups
            .iter()
            .flat_map(|group| group.forms.iter())
            .find(|form| form.name == name)
            .map(|form| form.id.to_string())
            .expect("form")
    }

    #[test]
    fn search_matches_name_description_and_category() {
        let all = groups();
        assert_eq!(search(&all, ""), all);
        assert_eq!(search(&all, "   "), all);
        assert_eq!(
            names(&search(&all, "출장")),
            vec!["국내출장신청", "해외출장신청"]
        );
        assert_eq!(names(&search(&all, "국내 출장 신청")), vec!["국내출장신청"]);
        assert_eq!(names(&search(&all, "정산")), vec!["지출결의서"]);
        assert_eq!(names(&search(&all, "overseas TRIP")), vec!["해외출장신청"]);
        // 분류 이름이 맞으면 그 분류의 양식이 모두 나온다.
        assert_eq!(names(&search(&all, "일반")), vec!["품의서", "업무 협조"]);
        assert!(search(&all, "없는 양식").is_empty());
        assert!(search(&all, "<script>").is_empty());
        assert!(search(&[], "품의").is_empty());
    }

    #[test]
    fn scope_filters_and_falls_back_when_the_category_is_gone() {
        let all = groups();
        assert_eq!(total(&all), 5);
        assert_eq!(total(&[]), 0);
        assert_eq!(in_scope(&all, Scope::All), all);
        let trip = in_scope(&all, Scope::Category(FormCategory::Trip));
        assert_eq!(names(&trip), vec!["국내출장신청", "해외출장신청"]);
        assert!(in_scope(&all, Scope::Category(FormCategory::Welfare)).is_empty());
        let found = search(&all, "정산");
        assert_eq!(
            effective_scope(&found, Scope::Category(FormCategory::Trip)),
            Scope::All
        );
        assert_eq!(
            effective_scope(&found, Scope::Category(FormCategory::Accounting)),
            Scope::Category(FormCategory::Accounting)
        );
        assert_eq!(effective_scope(&[], Scope::All), Scope::All);
    }

    #[test]
    fn pinned_lists_starred_then_recent_without_repeats() {
        let all = groups();
        let (a, b, c) = (
            id(&all, "품의서"),
            id(&all, "국내출장신청"),
            id(&all, "지출결의서"),
        );
        let gone = FormId::new().to_string();
        let pinned_names = |starred: &[String], recent: &[String]| -> Vec<String> {
            pinned(&all, starred, recent)
                .into_iter()
                .map(|entry| entry.form.name)
                .collect()
        };
        assert!(pinned_names(&[], &[]).is_empty());
        assert_eq!(
            pinned_names(
                std::slice::from_ref(&c),
                &[a.clone(), c.clone(), gone.clone(), b.clone()]
            ),
            vec!["지출결의서", "품의서", "국내출장신청"]
        );
        // 분류도 함께 돌려준다.
        let first = &pinned(&all, std::slice::from_ref(&b), &[])[0];
        assert_eq!(first.category, FormCategory::Trip);
        assert_eq!(first.category_label, "출장");
        // 최근 목록은 다섯 개까지만 본다.
        let many: Vec<String> = std::iter::repeat_with(|| gone.clone())
            .take(MAX_RECENT)
            .chain([a.clone()])
            .collect();
        assert!(pinned_names(&[], &many).is_empty());
        assert_eq!(pick(&all, &a).map(|p| p.form.name), Some("품의서".into()));
        assert_eq!(pick(&all, &gone), None);
        assert_eq!(pick(&all, "not-an-id"), None);
    }

    #[test]
    fn recent_keeps_the_latest_five_first() {
        let ids: Vec<String> = (0..7).map(|_| FormId::new().to_string()).collect();
        let mut recent = Vec::new();
        for id in &ids {
            recent = push_recent(&recent, id);
        }
        assert_eq!(recent.len(), MAX_RECENT);
        assert_eq!(recent[0], ids[6]);
        assert_eq!(recent[4], ids[2]);
        // 다시 쓰면 맨 앞으로 오고 길이는 그대로다.
        let again = push_recent(&recent, &ids[3]);
        assert_eq!(again[0], ids[3]);
        assert_eq!(again.len(), MAX_RECENT);
        assert_eq!(again.iter().filter(|id| **id == ids[3]).count(), 1);
        assert_eq!(push_recent(&[], "x"), vec!["x"]);
    }

    #[test]
    fn stars_toggle_and_stop_at_the_limit() {
        let one = toggle_star(&[], "a");
        assert_eq!(one, vec!["a"]);
        assert_eq!(toggle_star(&one, "b"), vec!["a", "b"]);
        assert!(toggle_star(&one, "a").is_empty());
        let full: Vec<String> = (0..MAX_STARRED).map(|n| n.to_string()).collect();
        assert_eq!(toggle_star(&full, "new"), full);
        assert_eq!(toggle_star(&full, "0").len(), MAX_STARRED - 1);
    }

    #[test]
    fn stored_lists_are_not_trusted() {
        let (a, b) = (FormId::new().to_string(), FormId::new().to_string());
        let raw = encode_ids(&[a.clone(), b.clone()]);
        assert_eq!(parse_ids(&raw, 5), vec![a.clone(), b.clone()]);
        assert_eq!(parse_ids(&raw, 1), vec![a.clone()]);
        assert_eq!(parse_ids(&raw, 0), Vec::<String>::new());
        let dirty =
            format!(r#"["{a}", 3, null, "<img onerror=x>", "{a}", {{"id": "{b}"}}, "{b}"]"#);
        assert_eq!(parse_ids(&dirty, 5), vec![a, b]);
        for bad in ["", "null", "{}", "\"abc\"", "[", "not json", "[[[[["] {
            assert!(parse_ids(bad, 5).is_empty(), "{bad:?}");
        }
        assert_eq!(encode_ids(&[]), "[]");
        assert_eq!(storage_key("recent", "m1"), "pango.forms.recent.m1");
    }

    #[test]
    fn system_forms_go_to_their_own_screens() {
        assert_eq!(destination(FormRoute::ApprovalNew), Destination::Editor);
        assert_eq!(destination(FormRoute::LeaveNew), Destination::LeaveNew);
        assert_eq!(destination(FormRoute::Attendance), Destination::Attendance);
    }
}
