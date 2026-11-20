//! 공용 UI 컴포넌트. 화면은 여기 있는 조각만 조립하고, 모양은 `assets/style.css`가 정한다.

pub mod approval_line;
mod icon;
mod member_picker;
mod select;
pub mod workday_bar;

pub use approval_line::{use_line_draft, ApprovalLineEditor, ApproverEntry};
pub use icon::{Icon, IconName};
pub use member_picker::{MemberPicker, PickedMember};
pub use select::SelectField;
pub use workday_bar::{WeekProgress, WorkdayBar};

use dioxus::prelude::*;

use crate::api::{ApiError, ApiResult};
use crate::format;
use crate::route::Route;
use crate::session::{use_toasts, Toast, ToastKind, Toasts};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonKind {
    /// 주 동작(채움). 영역마다 하나만 둔다.
    #[default]
    Primary,
    /// 옅은 버튼(브랜드 옅은 바탕). 주 동작 옆의 보조 동작.
    Weak,
    /// 회색 버튼. 취소·닫기처럼 물러나는 동작과 행 안의 작은 동작.
    Grey,
    /// 위험 동작(빨간 옅은 바탕). 확인 단계와 함께 쓴다.
    Danger,
}

impl ButtonKind {
    const fn class(self) -> &'static str {
        match self {
            Self::Primary => "btn btn-primary",
            Self::Weak => "btn btn-weak",
            Self::Grey => "btn btn-grey",
            Self::Danger => "btn btn-danger",
        }
    }
}

#[component]
pub fn Button(
    #[props(default)] kind: ButtonKind,
    /// 폼 제출 버튼인지.
    #[props(default)]
    submit: bool,
    #[props(default)] disabled: bool,
    /// 요청 진행 중. 눌리지 않고 진행 표시가 뜬다(중복 제출 방지).
    #[props(default)]
    busy: bool,
    #[props(default)] block: bool,
    /// 높이 32px(행 안의 동작).
    #[props(default)]
    small: bool,
    /// 높이 52px(화면의 대표 동작).
    #[props(default)]
    large: bool,
    #[props(default)] onclick: EventHandler<MouseEvent>,
    children: Element,
) -> Element {
    let mut class = kind.class().to_owned();
    if block {
        class.push_str(" btn-block");
    }
    if small {
        class.push_str(" btn-sm");
    }
    if large {
        class.push_str(" btn-lg");
    }
    rsx! {
        button {
            class,
            r#type: if submit { "submit" } else { "button" },
            disabled: disabled || busy,
            "aria-busy": busy,
            onclick: move |event| onclick.call(event),
            if busy {
                span { class: "spinner", "aria-hidden": "true" }
            }
            {children}
        }
    }
}

/// 흰 패널. 화면의 내용은 모두 패널 안에 놓는다.
#[component]
pub fn Card(
    #[props(into, default)] title: String,
    /// 제목 오른쪽에 놓는 요소(더보기 링크 등).
    action: Option<Element>,
    #[props(into, default)] class: String,
    children: Element,
) -> Element {
    rsx! {
        section { class: "panel {class}",
            if !title.is_empty() || action.is_some() {
                header { class: "panel-header",
                    h2 { class: "panel-title", "{title}" }
                    {action}
                }
            }
            {children}
        }
    }
}

/// 패널 제목 오른쪽의 "더 보기" 링크(꺾쇠가 붙는다).
#[component]
pub fn PanelLink(to: Route, #[props(into)] label: String) -> Element {
    rsx! {
        Link { class: "panel-link", to,
            "{label}"
            Icon { name: IconName::Chevron }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    #[default]
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

impl Tone {
    const fn class(self) -> &'static str {
        match self {
            Self::Neutral => "tone-neutral",
            Self::Info => "tone-info",
            Self::Success => "tone-success",
            Self::Warning => "tone-warning",
            Self::Danger => "tone-danger",
        }
    }

    /// 바탕 없이 글자·선의 색만 정하는 클래스(숫자 타일, 타임라인의 점).
    const fn ink(self) -> &'static str {
        match self {
            Self::Neutral => "ink-neutral",
            Self::Info => "ink-info",
            Self::Success => "ink-success",
            Self::Warning => "ink-warning",
            Self::Danger => "ink-danger",
        }
    }
}

#[component]
pub fn Badge(#[props(default)] tone: Tone, children: Element) -> Element {
    rsx! { span { class: "badge {tone.class()}", {children} } }
}

/// 아바타 바탕색의 가짓수.
pub const AVATAR_COLORS: u32 = 6;

/// 구성원마다 고정된 아바타 색 번호(0~5). 같은 키는 언제나 같은 색이다(FNV-1a).
pub fn avatar_color(key: &str) -> u32 {
    let hash = key.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    hash % AVATAR_COLORS
}

/// 이름 첫 글자 아바타. 색은 `seed`(구성원 ID)로 정하고, 없으면 이름으로 정한다.
#[component]
pub fn Avatar(
    #[props(into)] name: String,
    #[props(into, default)] seed: String,
    #[props(default)] large: bool,
) -> Element {
    let color = avatar_color(if seed.is_empty() { &name } else { &seed });
    let size = if large { " avatar-lg" } else { "" };
    rsx! {
        span { class: "avatar avatar-{color}{size}", "aria-hidden": "true", "{format::initial(&name)}" }
    }
}

/// 골격 한 줄의 폭(%). 글줄처럼 보이도록 줄마다 길이를 달리한다.
pub fn skeleton_width(index: usize) -> u32 {
    const WIDTHS: [u32; 4] = [92, 100, 76, 58];
    WIDTHS[index % WIDTHS.len()]
}

/// 기다리는 동안 내용 자리를 대신 보여 주는 골격. `rows`는 줄 수, `title`이면 굵은 첫 줄이 붙는다.
#[component]
pub fn Skeleton(#[props(default = 3)] rows: usize, #[props(default)] title: bool) -> Element {
    rsx! {
        div { class: "skeleton", role: "status",
            if title {
                span { class: "skeleton-bar skeleton-title" }
            }
            for index in 0..rows {
                span { key: "{index}", class: "skeleton-bar", style: "width: {skeleton_width(index)}%" }
            }
            span { class: "sr-only", "불러오는 중" }
        }
    }
}

/// 비어 있지 않은 항목만 앞뒤 공백을 지워 남긴다.
pub fn meta_items<I, S>(items: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    items
        .into_iter()
        .map(|item| item.as_ref().trim().to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

/// 메타 정보 줄. 항목을 가운데 점으로 잇지 않고 간격을 둔 별도 요소로 나열한다.
#[component]
pub fn Meta(items: Vec<String>) -> Element {
    let items = meta_items(items);
    if items.is_empty() {
        return rsx! {};
    }
    rsx! {
        span { class: "meta",
            for (index , item) in items.into_iter().enumerate() {
                span { key: "{index}", class: "meta-item", "{item}" }
            }
        }
    }
}

#[component]
pub fn EmptyState(
    #[props(into)] title: String,
    #[props(into, default)] hint: String,
    #[props(default = IconName::Inbox)] icon: IconName,
    children: Element,
) -> Element {
    rsx! {
        div { class: "empty",
            span { class: "empty-icon", Icon { name: icon } }
            p { class: "empty-title", "{title}" }
            if !hint.is_empty() {
                p { class: "empty-hint", "{hint}" }
            }
            {children}
        }
    }
}

/// 조회 실패 안내 + 다시 시도.
#[component]
pub fn ErrorState(#[props(into)] message: String, onretry: EventHandler<()>) -> Element {
    rsx! {
        div { class: "empty", role: "alert",
            span { class: "empty-icon", Icon { name: IconName::Alert } }
            p { class: "empty-title", "{message}" }
            Button { kind: ButtonKind::Weak, onclick: move |_| onretry.call(()), "다시 시도" }
        }
    }
}

/// 조회 결과의 세 상태(불러오는 중·실패·성공)를 한 가지 방식으로 그린다.
pub fn resource_view<T: Clone + 'static>(
    mut resource: Resource<ApiResult<T>>,
    render: impl FnOnce(T) -> Element,
) -> Element {
    // 세션 만료·권한 변경은 요청 future가 이미 반영했다(`api`). 여기서는 그리기만 한다.
    let state = resource.read().clone();
    match state {
        None => rsx! { Skeleton { rows: 4 } },
        Some(Ok(value)) => render(value),
        Some(Err(error)) => {
            rsx! { ErrorState { message: error.message(), onretry: move |_| resource.restart() } }
        }
    }
}

/// 목록 조회 결과. 아직 읽는 중이거나 실패했으면 빈 목록이다(선택 상자·필터의 보조 목록용.
/// 본 목록은 [`resource_view`]로 세 상태를 그린다).
pub fn loaded_or_empty<T: Clone>(resource: &Resource<ApiResult<Vec<T>>>) -> Vec<T> {
    match &*resource.read() {
        Some(Ok(list)) => list.clone(),
        _ => Vec::new(),
    }
}

#[component]
pub fn PageHeader(
    #[props(into)] title: String,
    /// 제목 아래 한 줄 설명.
    #[props(into, default)]
    description: String,
    /// 제목 오른쪽의 동작 버튼(주 동작은 하나).
    actions: Option<Element>,
) -> Element {
    rsx! {
        header { class: "page-header",
            div { class: "page-heading",
                h1 { class: "page-title", "{title}" }
                if !description.is_empty() {
                    p { class: "page-description", "{description}" }
                }
            }
            if let Some(actions) = actions {
                div { class: "page-actions", {actions} }
            }
        }
    }
}

/// 라벨이 붙은 한 줄 입력.
#[component]
pub fn TextField(
    #[props(into)] label: String,
    #[props(into)] value: String,
    oninput: EventHandler<String>,
    #[props(into, default = "text".to_owned())] kind: String,
    #[props(into, default)] placeholder: String,
    #[props(into, default)] autocomplete: String,
    #[props(into, default)] hint: String,
    #[props(into, default)] error: String,
    #[props(default)] required: bool,
    #[props(default)] disabled: bool,
    #[props(default)] maxlength: Option<i64>,
) -> Element {
    rsx! {
        label { class: "field",
            span { class: "field-label",
                "{label}"
                if required {
                    span { class: "field-required", "aria-hidden": "true", " *" }
                }
            }
            input {
                class: if error.is_empty() { "input" } else { "input input-error" },
                r#type: "{kind}",
                value: "{value}",
                placeholder: "{placeholder}",
                autocomplete: "{autocomplete}",
                required,
                disabled,
                maxlength,
                "aria-invalid": !error.is_empty(),
                oninput: move |event| oninput.call(event.value()),
            }
            if !error.is_empty() {
                span { class: "field-error", role: "alert", "{error}" }
            } else if !hint.is_empty() {
                span { class: "field-hint", "{hint}" }
            }
        }
    }
}

#[component]
pub fn TextArea(
    #[props(into)] label: String,
    #[props(into)] value: String,
    oninput: EventHandler<String>,
    #[props(into, default)] placeholder: String,
    #[props(default = 5)] rows: i64,
    #[props(default)] required: bool,
    #[props(default)] maxlength: Option<i64>,
) -> Element {
    rsx! {
        label { class: "field",
            span { class: "field-label",
                "{label}"
                if required {
                    span { class: "field-required", "aria-hidden": "true", " *" }
                }
            }
            textarea {
                class: "input textarea",
                rows,
                value: "{value}",
                placeholder: "{placeholder}",
                required,
                maxlength,
                oninput: move |event| oninput.call(event.value()),
            }
        }
    }
}

/// 화면 위에 뜨는 대화 상자. 바깥을 누르거나 닫기 버튼·Esc로 닫는다.
///
/// 열리면 초점을 대화 상자로 옮기고, Tab·Shift+Tab은 대화 상자 안에서만 돈다. 닫히면 연 버튼
/// (열기 전 초점)으로 초점을 돌려준다.
#[component]
pub fn Modal(
    #[props(into)] title: String,
    onclose: EventHandler<()>,
    /// 대화 상자에 덧붙일 클래스(`modal-wide`: 넓게, `modal-full`: 모바일에서 화면 가득).
    #[props(into, default)]
    class: String,
    children: Element,
) -> Element {
    let id = use_hook(dialog::next_id);
    // 첫 렌더는 대화 상자가 그려지기 전이라 아직 연 버튼에 초점이 있다.
    let opener = use_hook(dialog::active_element);
    {
        let id = id.clone();
        use_effect(move || dialog::focus(&id));
    }
    use_drop(move || dialog::restore(opener.as_ref()));
    let keydown = {
        let id = id.clone();
        move |event: KeyboardEvent| match event.key() {
            Key::Escape => {
                event.stop_propagation();
                onclose.call(());
            }
            Key::Tab => {
                let backward = event.modifiers().contains(Modifiers::SHIFT);
                if dialog::trap_tab(&id, backward) {
                    event.prevent_default();
                }
            }
            _ => {}
        }
    };
    rsx! {
        div { class: "modal-backdrop", onclick: move |_| onclose.call(()),
            div {
                id: "{id}",
                class: "modal {class}",
                role: "dialog",
                "aria-modal": "true",
                "aria-label": "{title}",
                tabindex: "-1",
                onclick: move |event| event.stop_propagation(),
                onkeydown: keydown,
                header { class: "modal-header",
                    h2 { class: "modal-title", "{title}" }
                    button {
                        class: "icon-btn",
                        r#type: "button",
                        "aria-label": "닫기",
                        onclick: move |_| onclose.call(()),
                        Icon { name: IconName::Close }
                    }
                }
                div { class: "modal-body", {children} }
            }
        }
    }
}

/// Tab을 눌렀을 때 초점을 어디로 둘지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabMove {
    /// 브라우저 기본 이동에 맡긴다(대화 상자 안에서 다음·이전 칸으로 간다).
    Browser,
    /// 이 순서의 칸으로 옮긴다(끝에서 처음으로, 처음에서 끝으로 돈다).
    Focus(usize),
    /// 옮길 칸이 없다. 대화 상자 밖으로 나가지 않게 그대로 둔다.
    Stay,
}

/// 대화 상자 안의 초점 칸 `count`개 중 지금 `current`에 있을 때 Tab(`backward`면 Shift+Tab).
/// 지금 초점이 칸이 아니면(대화 상자 자체 등) 앞으로는 첫 칸, 뒤로는 마지막 칸이다.
pub fn tab_move(count: usize, current: Option<usize>, backward: bool) -> TabMove {
    let Some(last) = count.checked_sub(1) else {
        return TabMove::Stay;
    };
    match (current, backward) {
        (None, false) => TabMove::Focus(0),
        (None, true) => TabMove::Focus(last),
        (Some(0), true) => TabMove::Focus(last),
        (Some(index), false) if index >= last => TabMove::Focus(0),
        (Some(index), true) if index > last => TabMove::Focus(last),
        _ => TabMove::Browser,
    }
}

/// 대화 상자의 초점 처리(브라우저 DOM). 계산은 [`tab_move`]가 한다.
mod dialog {
    use std::sync::atomic::{AtomicU32, Ordering};

    use wasm_bindgen::JsCast;
    use web_sys::HtmlElement;

    use super::{tab_move, TabMove};

    /// 초점을 받을 수 있는 칸.
    const FOCUSABLE: &str =
        "a[href], button:not([disabled]), input:not([disabled]):not([type=\"hidden\"]), \
        select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex=\"-1\"])";

    /// 겹쳐 뜬 대화 상자도 서로 구분하는 요소 ID.
    pub fn next_id() -> String {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        format!("dialog-{}", NEXT.fetch_add(1, Ordering::Relaxed))
    }

    fn document() -> Option<web_sys::Document> {
        web_sys::window()?.document()
    }

    pub fn active_element() -> Option<HtmlElement> {
        document()?.active_element()?.dyn_into::<HtmlElement>().ok()
    }

    fn element(id: &str) -> Option<HtmlElement> {
        document()?
            .get_element_by_id(id)?
            .dyn_into::<HtmlElement>()
            .ok()
    }

    /// 열릴 때: 대화 상자 자체에 초점(화면 낭독기가 제목을 읽고, Tab은 첫 칸부터).
    /// 입력 칸에 바로 두지 않는 것은 모바일에서 키보드가 바로 올라오지 않게 하려는 것이다.
    pub fn focus(id: &str) {
        if let Some(dialog) = element(id) {
            let _ = dialog.focus();
        }
    }

    /// 닫힐 때: 연 버튼이 아직 화면에 있으면 그리로 돌려준다.
    pub fn restore(opener: Option<&HtmlElement>) {
        if let Some(opener) = opener.filter(|el| el.is_connected()) {
            let _ = opener.focus();
        }
    }

    /// Tab을 대화 상자 안에 가둔다. 초점을 직접 옮겼으면(기본 동작을 막아야 하면) `true`.
    pub fn trap_tab(id: &str, backward: bool) -> bool {
        let Some(dialog) = element(id) else {
            return false;
        };
        let Ok(nodes) = dialog.query_selector_all(FOCUSABLE) else {
            return false;
        };
        let items: Vec<HtmlElement> = (0..nodes.length())
            .filter_map(|index| nodes.item(index))
            .filter_map(|node| node.dyn_into::<HtmlElement>().ok())
            // 숨겨진 칸(`display: none` 등)은 건너뛴다.
            .filter(|el| el.get_client_rects().length() > 0)
            .collect();
        let active = active_element();
        let current = active
            .as_ref()
            .and_then(|active| items.iter().position(|el| el == active));
        match tab_move(items.len(), current, backward) {
            TabMove::Browser => false,
            TabMove::Focus(index) => {
                if let Some(target) = items.get(index) {
                    let _ = target.focus();
                }
                true
            }
            TabMove::Stay => true,
        }
    }
}

/// 탭 목록. `tabs`는 `(키, 문구, 배지 숫자)`.
#[component]
pub fn Tabs(
    tabs: Vec<(String, String, Option<u32>)>,
    #[props(into)] active: String,
    onselect: EventHandler<String>,
) -> Element {
    rsx! {
        div { class: "tabs", role: "tablist",
            for (key , label , count) in tabs {
                button {
                    key: "{key}",
                    class: if key == active { "tab tab-active" } else { "tab" },
                    r#type: "button",
                    role: "tab",
                    "aria-selected": key == active,
                    onclick: {
                        let key = key.clone();
                        move |_| onselect.call(key.clone())
                    },
                    "{label}"
                    if let Some(count) = count.filter(|n| *n > 0) {
                        span { class: "tab-count", "{count}" }
                    }
                }
            }
        }
    }
}

/// 숫자 하나를 크게 보여 주는 타일.
#[component]
pub fn Stat(
    #[props(into)] label: String,
    #[props(into)] value: String,
    #[props(into, default)] hint: String,
    #[props(default)] tone: Tone,
) -> Element {
    rsx! {
        div { class: "stat",
            span { class: "stat-label", "{label}" }
            strong { class: "stat-value tnum {tone.ink()}",
                for (index , part) in stat_parts(&value).into_iter().enumerate() {
                    if part.unit {
                        span { key: "{index}", class: "stat-unit", "{part.text}" }
                    } else {
                        "{part.text}"
                    }
                }
            }
            if !hint.is_empty() {
                span { class: "stat-hint", "{hint}" }
            }
        }
    }
}

/// 숫자 타일 값의 한 조각. 숫자는 크게, 단위(`건`, `명`, `시간`)는 작고 흐리게 그린다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatPart {
    pub text: String,
    pub unit: bool,
}

/// `12시간 30분` → `12` `시간 ` `30` `분`. 시각(`16:15`)은 통째로 숫자이고, 숫자가 하나도 없으면
/// (`-`, `없음`) 통째로 값이다.
pub fn stat_parts(value: &str) -> Vec<StatPart> {
    let is_number = |c: char| c.is_ascii_digit() || matches!(c, '.' | ',' | '-' | '+' | ':');
    if !value.chars().any(|c| c.is_ascii_digit()) {
        return vec![StatPart {
            text: value.to_owned(),
            unit: false,
        }];
    }
    let mut parts: Vec<StatPart> = Vec::new();
    for c in value.chars() {
        let unit = !is_number(c);
        match parts.last_mut() {
            Some(last) if last.unit == unit => last.text.push(c),
            _ => parts.push(StatPart {
                text: c.to_string(),
                unit,
            }),
        }
    }
    parts
}

/// 누르면 다른 화면으로 가는 숫자 타일(홈·관리 홈의 "처리할 일").
///
/// `tab`을 주면 그 화면을 이 탭으로 연다([`use_initial_tab`]).
#[component]
pub fn StatLink(
    to: Route,
    #[props(into)] label: String,
    #[props(into)] value: String,
    #[props(into, default)] hint: String,
    #[props(default)] tone: Tone,
    #[props(default)] tab: Option<&'static str>,
) -> Element {
    let pending = use_context::<PendingTab>();
    let target = to.clone();
    rsx! {
        Link {
            class: "stat-link",
            to,
            onclick: move |_| pending.open(target.clone(), tab),
            Stat { label, value, hint, tone }
            Icon { name: IconName::Chevron }
        }
    }
}

/// 다른 화면으로 가면서 열어 둘 탭. 경로를 늘리지 않고, 받는 화면이 처음 그릴 때 한 번 쓴다.
#[derive(Clone, Copy)]
pub struct PendingTab(Signal<Option<(Route, &'static str)>>);

impl PendingTab {
    /// `route`로 가면서 `tab`을 열게 한다. `tab`이 없으면 남아 있던 요청을 지운다.
    pub fn open(mut self, route: Route, tab: Option<&'static str>) {
        self.0.set(tab.map(|tab| (route, tab)));
    }
}

pub fn provide_pending_tab() -> PendingTab {
    use_context_provider(|| PendingTab(Signal::new(None)))
}

/// 열 탭: 이 화면(`route`)으로 오면서 요청한 탭이 있으면 그것, 없으면 `default`.
pub fn initial_tab(
    pending: Option<&(Route, &'static str)>,
    route: &Route,
    default: &'static str,
) -> &'static str {
    match pending {
        Some((target, tab)) if target == route => tab,
        _ => default,
    }
}

/// 탭 상태. 처음 값은 [`initial_tab`]이고, 요청은 그린 뒤(렌더 밖에서) 지운다.
pub fn use_initial_tab(route: Route, default: &'static str) -> Signal<String> {
    let mut pending = use_context::<PendingTab>();
    let tab = use_signal(|| initial_tab(pending.0.peek().as_ref(), &route, default).to_owned());
    use_effect(move || {
        if pending.0.peek().is_some() {
            pending.0.set(None);
        }
    });
    tab
}

/// 폼 상단에 띄우는 오류 문구.
#[component]
pub fn FormError(#[props(into)] message: String) -> Element {
    if message.is_empty() {
        return rsx! {};
    }
    rsx! { p { class: "form-error", role: "alert", "{message}" } }
}

/// 전역 토스트 표시 영역. 앱 최상단에 한 번만 둔다.
///
/// 알림 영역(`aria-live`)은 처음부터 있어야 화면 낭독기가 새 내용을 읽으므로 두 영역을 늘 둔다.
/// 성공은 차례를 기다려(`polite`), 실패는 바로(`assertive`) 읽는다. 토스트는 버튼이라 눌러서나
/// 키보드(Enter·Space)로 닫을 수 있다.
#[component]
pub fn ToastHost() -> Element {
    let toasts = use_toasts();
    let (errors, others): (Vec<Toast>, Vec<Toast>) = toasts
        .items()
        .into_iter()
        .partition(|toast| toast.kind == ToastKind::Error);
    rsx! {
        div { class: "toasts",
            div { class: "toast-region", role: "status", "aria-live": "polite",
                for toast in others {
                    ToastItem { key: "{toast.id}", toast }
                }
            }
            div { class: "toast-region", role: "alert", "aria-live": "assertive",
                for toast in errors {
                    ToastItem { key: "{toast.id}", toast }
                }
            }
        }
    }
}

#[component]
fn ToastItem(toast: Toast) -> Element {
    let mut toasts = use_toasts();
    let id = toast.id;
    rsx! {
        button {
            class: if toast.kind == ToastKind::Error { "toast toast-error" } else { "toast" },
            r#type: "button",
            onclick: move |_| toasts.dismiss(id),
            if toast.kind == ToastKind::Error {
                Icon { name: IconName::Alert }
            }
            span { "{toast.message}" }
            span { class: "sr-only", " (눌러서 닫기)" }
        }
    }
}

/// 요청 실패를 토스트로 알린다. 이벤트 핸들러 안에서 부른다.
/// 세션 만료·권한 변경은 요청이 실패할 때 `api`가 이미 반영했다.
pub fn report_error(error: &ApiError) {
    consume_context::<Toasts>().error(error.message());
}

pub fn report_success(message: impl Into<String>) {
    consume_context::<Toasts>().success(message);
}

/// 달 이동 머리: `‹ 2026년 3월 ›`.
#[component]
pub fn MonthSwitcher(
    month: crate::calendar::YearMonth,
    onchange: EventHandler<crate::calendar::YearMonth>,
) -> Element {
    rsx! {
        div { class: "month-switcher",
            button {
                class: "icon-btn month-prev",
                r#type: "button",
                "aria-label": "이전 달",
                disabled: !month.can_go_prev(),
                onclick: move |_| onchange.call(month.prev()),
                Icon { name: IconName::Chevron }
            }
            strong { class: "month-label", "{month.label()}" }
            button {
                class: "icon-btn",
                r#type: "button",
                "aria-label": "다음 달",
                disabled: !month.can_go_next(),
                onclick: move |_| onchange.call(month.next()),
                Icon { name: IconName::Chevron }
            }
        }
    }
}

/// 한 달 달력. 날짜 칸의 내용은 `day`가 그린다.
///
/// `outside`면 앞뒤 달의 날짜로 첫 주와 마지막 주를 채우고(그 칸에도 `day`를 부른다),
/// 아니면 그 칸을 비워 둔다.
#[component]
pub fn MonthCalendar(
    month: crate::calendar::YearMonth,
    day: Callback<chrono::NaiveDate, Element>,
    #[props(default)] outside: bool,
    /// 격자의 접근성 이름(예: "2026년 11월 일정").
    #[props(into, default)]
    label: String,
    #[props(into, default)] class: String,
) -> Element {
    const WEEKDAYS: [&str; 7] = ["일", "월", "화", "수", "목", "금", "토"];
    let weeks: Vec<[Option<chrono::NaiveDate>; 7]> = if outside {
        month
            .full_weeks()
            .into_iter()
            .map(|week| week.map(Some))
            .collect()
    } else {
        month.weeks()
    };
    rsx! {
        div {
            class: "calendar {class}",
            role: "grid",
            "aria-label": if !label.is_empty() { "{label}" },
            div { class: "calendar-row", role: "row",
                for name in WEEKDAYS {
                    span { class: "calendar-weekday", role: "columnheader", "{name}" }
                }
            }
            for week in weeks {
                div { class: "calendar-row", role: "row",
                    for cell in week {
                        if let Some(date) = cell {
                            div {
                                class: if crate::calendar::YearMonth::of(date) == month { "calendar-cell" } else { "calendar-cell calendar-cell-outside" },
                                role: "gridcell",
                                {day.call(date)}
                            }
                        } else {
                            div { class: "calendar-cell calendar-cell-empty", role: "gridcell" }
                        }
                    }
                }
            }
        }
    }
}

/// 지금 시각(브라우저 시계). 서버가 다시 판정하므로 표시·기본값 용도로만 쓴다.
pub fn now() -> pango_core::time::Timestamp {
    let millis = js_sys::Date::now() as i64;
    pango_core::time::Timestamp::from_millis(millis)
        .unwrap_or_else(|_| pango_core::time::Timestamp::from_utc(chrono::DateTime::UNIX_EPOCH))
}

/// 회사 시간대의 오늘(브라우저 시계 기준).
pub fn today(tz: chrono_tz::Tz) -> chrono::NaiveDate {
    format::local_date(now(), tz)
}

/// 라벨이 붙은 체크 상자.
#[component]
pub fn Checkbox(
    #[props(into)] label: String,
    checked: bool,
    onchange: EventHandler<bool>,
    #[props(into, default)] hint: String,
    #[props(default)] disabled: bool,
) -> Element {
    rsx! {
        label { class: "check",
            input {
                class: "check-input",
                r#type: "checkbox",
                checked,
                disabled,
                onchange: move |event| onchange.call(event.checked()),
            }
            span { class: "check-text",
                span { class: "check-label", "{label}" }
                if !hint.is_empty() {
                    small { class: "field-hint", "{hint}" }
                }
            }
        }
    }
}

/// 되돌리기 어려운 동작 앞의 확인 단계.
///
/// 한 줄 설명이면 `message`를, 더 자세한 안내가 필요하면 내용(`children`)을 준다.
#[component]
pub fn ConfirmModal(
    #[props(into)] title: String,
    #[props(into, default)] message: String,
    #[props(into, default = "확인".to_owned())] confirm_label: String,
    /// 확인 버튼을 위험 색으로.
    #[props(default)]
    danger: bool,
    #[props(default)] busy: bool,
    onconfirm: EventHandler<()>,
    oncancel: EventHandler<()>,
    children: Element,
) -> Element {
    rsx! {
        Modal { title, onclose: move |_| oncancel.call(()),
            div { class: "confirm-body",
                if !message.is_empty() {
                    p { class: "confirm-message", "{message}" }
                }
                {children}
            }
            div { class: "modal-actions",
                Button {
                    kind: ButtonKind::Grey,
                    disabled: busy,
                    onclick: move |_| oncancel.call(()),
                    "취소"
                }
                Button {
                    kind: if danger { ButtonKind::Danger } else { ButtonKind::Primary },
                    busy,
                    onclick: move |_| onconfirm.call(()),
                    "{confirm_label}"
                }
            }
        }
    }
}

/// 목록 아래의 "더 보기" 버튼. 다음 페이지가 없으면(`has_more = false`) 아무것도 그리지 않는다.
#[component]
pub fn LoadMore(
    #[props(default = true)] has_more: bool,
    #[props(default)] busy: bool,
    onclick: EventHandler<()>,
) -> Element {
    if !has_more {
        return rsx! {};
    }
    rsx! {
        div { class: "load-more",
            Button { kind: ButtonKind::Grey, busy, onclick: move |_| onclick.call(()), "더 보기" }
        }
    }
}

/// 커서 목록에서 첫 페이지 뒤에 이어 붙인 부분. 첫 페이지는 `use_resource`가 맡고,
/// 첫 페이지를 다시 불러올 때(필터 변경 등) [`MorePages::reset`]으로 비운다.
pub struct MorePages<T: 'static> {
    items: Signal<Vec<T>>,
    /// 마지막으로 이어 붙인 페이지의 다음 커서. 아직 안 붙였으면 `None`.
    after: Signal<Option<Option<String>>>,
    busy: Signal<bool>,
    /// [`MorePages::reset`]할 때마다 늘린다. 읽는 사이 목록이 바뀌었으면 받은 페이지를 버린다.
    generation: Signal<u32>,
}

impl<T: 'static> Clone for MorePages<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: 'static> Copy for MorePages<T> {}

pub fn use_more_pages<T: 'static>() -> MorePages<T> {
    MorePages {
        items: use_signal(Vec::new),
        after: use_signal(|| None),
        busy: use_signal(|| false),
        generation: use_signal(|| 0),
    }
}

/// `first` 뒤에 `more`를 붙인다. `key`가 이미 있는 항목은 건너뛴다(페이지 사이에 새 항목이
/// 생겨 한 칸씩 밀리면 같은 항목이 두 페이지에 걸쳐 온다).
pub fn merge_unique<T, K: PartialEq>(first: Vec<T>, more: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
    let mut items = first;
    for item in more {
        let id = key(&item);
        if !items.iter().any(|existing| key(existing) == id) {
            items.push(item);
        }
    }
    items
}

/// 다음에 불러올 커서. 이어 붙인 페이지가 있으면 그 커서가, 없으면 첫 페이지의 커서가 기준이다.
pub fn next_cursor(first: Option<&str>, later: Option<Option<String>>) -> Option<String> {
    match later {
        Some(cursor) => cursor,
        None => first.map(str::to_owned),
    }
}

impl<T: Clone + serde::de::DeserializeOwned + 'static> MorePages<T> {
    pub fn reset(mut self) {
        self.items.set(Vec::new());
        self.after.set(None);
        let next = self.generation.peek().wrapping_add(1);
        self.generation.set(next);
    }

    /// 첫 페이지 뒤에 이어 붙인 항목까지 모두. 읽는 사이 목록이 바뀌어 겹친 항목은 한 번만
    /// 나온다([`merge_unique`]).
    pub fn all<K: PartialEq>(&self, first: Vec<T>, key: impl Fn(&T) -> K) -> Vec<T> {
        merge_unique(first, self.items.read().clone(), key)
    }

    pub fn busy(&self) -> bool {
        *self.busy.read()
    }

    pub fn cursor(&self, first: &pango_api::Page<T>) -> Option<String> {
        next_cursor(first.next_cursor.as_deref(), self.after.read().clone())
    }

    /// 다음 페이지를 불러와 붙인다. `path`는 커서로 요청 경로를 만든다([`crate::api::with_cursor`]).
    pub async fn load(mut self, cursor: String, path: impl Fn(&str) -> String) {
        if self.busy() {
            return;
        }
        self.busy.set(true);
        let generation = *self.generation.peek();
        match crate::api::get::<pango_api::Page<T>>(&path(&cursor)).await {
            // 읽는 사이 목록이 바뀌었으면(필터·탭 변경) 버린다.
            Ok(_) if *self.generation.peek() != generation => {}
            Ok(page) => {
                self.items.write().extend(page.items);
                self.after.set(Some(page.next_cursor));
            }
            Err(error) => report_error(&error),
        }
        self.busy.set(false);
    }
}

/// 비밀값(임시 비밀번호 등)을 **한 번만** 보여 주는 대화 상자.
///
/// 실수로 닫지 않도록 바깥을 눌러도 닫히지 않고, 확인 버튼으로만 닫는다.
/// 값은 부모가 들고 있다가 `onclose`에서 바로 버려야 한다.
#[component]
pub fn SecretReveal(
    #[props(into)] title: String,
    #[props(into)] label: String,
    #[props(into)] secret: String,
    onclose: EventHandler<()>,
    children: Element,
) -> Element {
    rsx! {
        div { class: "modal-backdrop",
            div {
                class: "modal",
                role: "alertdialog",
                "aria-modal": "true",
                "aria-label": "{title}",
                header { class: "modal-header",
                    h2 { class: "modal-title", "{title}" }
                }
                div { class: "modal-body",
                    {children}
                    label { class: "field secret-field",
                        span { class: "field-label", "{label}" }
                        input {
                            class: "input secret-value",
                            r#type: "text",
                            readonly: true,
                            value: "{secret}",
                            autocomplete: "off",
                            spellcheck: false,
                            "autocapitalize": "off",
                        }
                    }
                    p { class: "callout callout-warning", role: "note",
                        Icon { name: IconName::Alert }
                        span { "이 창을 닫으면 다시 볼 수 없어요. 지금 복사해 본인에게 안전하게 전달해 주세요." }
                    }
                    div { class: "modal-actions",
                        Button { onclick: move |_| onclose.call(()), "확인했어요" }
                    }
                }
            }
        }
    }
}

/// 세로 타임라인. 항목은 [`TimelineItem`]으로 넣는다.
#[component]
pub fn Timeline(children: Element) -> Element {
    rsx! { ol { class: "timeline", {children} } }
}

/// 타임라인의 한 칸. 점의 색은 `tone`, 상태 문구는 내용에 함께 적는다(색만으로 알리지 않는다).
#[component]
pub fn TimelineItem(
    #[props(default)] tone: Tone,
    /// 지금 진행 중인 칸인지.
    #[props(default)]
    current: bool,
    children: Element,
) -> Element {
    rsx! {
        li {
            class: if current { "timeline-item timeline-item-current" } else { "timeline-item" },
            "aria-current": if current { "step" },
            span { class: "timeline-dot {tone.ink()}", "aria-hidden": "true" }
            div { class: "timeline-body", {children} }
        }
    }
}

/// 입력 글자 수 `12 / 200`. 넘치면 경고 색과 함께 "초과" 문구를 붙인다.
#[component]
pub fn CharCount(count: usize, max: usize) -> Element {
    let over = count > max;
    rsx! {
        span { class: if over { "char-count char-count-over" } else { "char-count" }, "aria-live": "polite",
            "{count} / {max}"
            if over {
                " (초과)"
            }
        }
    }
}

/// 폼 제출 실패를 처리한다. 입력 오류는 폼 안(`target`)에, 그 밖의 오류는 토스트로 알린다.
/// 이벤트 핸들러 안에서 부른다.
pub fn report_form_error(error: &ApiError, mut target: Signal<String>) {
    if error.code == "validation_failed" {
        target.set(error.message());
    } else {
        target.set(String::new());
        report_error(error);
    }
}

/// 날짜 입력(`YYYY-MM-DD`). 브라우저의 날짜 선택기를 쓴다.
#[component]
pub fn DateField(
    #[props(into)] label: String,
    #[props(into)] value: String,
    oninput: EventHandler<String>,
    #[props(into, default)] min: String,
    #[props(into, default)] max: String,
    #[props(into, default)] hint: String,
    #[props(into, default)] error: String,
    #[props(default)] required: bool,
    #[props(default)] disabled: bool,
) -> Element {
    rsx! {
        label { class: "field",
            span { class: "field-label",
                "{label}"
                if required {
                    span { class: "field-required", "aria-hidden": "true", " *" }
                }
            }
            input {
                class: if error.is_empty() { "input" } else { "input input-error" },
                r#type: "date",
                value: "{value}",
                min: "{min}",
                max: "{max}",
                required,
                disabled,
                "aria-invalid": !error.is_empty(),
                oninput: move |event| oninput.call(event.value()),
            }
            if !error.is_empty() {
                span { class: "field-error", role: "alert", "{error}" }
            } else if !hint.is_empty() {
                span { class: "field-hint", "{hint}" }
            }
        }
    }
}

/// 연도 이동 머리: `‹ 2026년 ›`. 범위 밖으로는 이동할 수 없다.
#[component]
pub fn YearSwitcher(year: i32, min: i32, max: i32, onchange: EventHandler<i32>) -> Element {
    rsx! {
        div { class: "month-switcher",
            button {
                class: "icon-btn month-prev",
                r#type: "button",
                "aria-label": "이전 해",
                disabled: year <= min,
                onclick: move |_| onchange.call(year - 1),
                Icon { name: IconName::Chevron }
            }
            strong { class: "month-label", "{year}년" }
            button {
                class: "icon-btn",
                r#type: "button",
                "aria-label": "다음 해",
                disabled: year >= max,
                onclick: move |_| onchange.call(year + 1),
                Icon { name: IconName::Chevron }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_wraps_inside_the_dialog() {
        assert_eq!(
            tab_move(0, None, false),
            TabMove::Stay,
            "칸이 없으면 그대로"
        );
        assert_eq!(tab_move(0, None, true), TabMove::Stay);
        // 대화 상자 자체에 초점이 있을 때(처음 열림).
        assert_eq!(tab_move(3, None, false), TabMove::Focus(0));
        assert_eq!(tab_move(3, None, true), TabMove::Focus(2));
        // 중간에서는 브라우저가 옮긴다.
        assert_eq!(tab_move(3, Some(0), false), TabMove::Browser);
        assert_eq!(tab_move(3, Some(1), true), TabMove::Browser);
        // 끝에서 돈다.
        assert_eq!(tab_move(3, Some(2), false), TabMove::Focus(0));
        assert_eq!(tab_move(3, Some(0), true), TabMove::Focus(2));
        // 한 칸뿐이면 그 칸에 머문다.
        assert_eq!(tab_move(1, Some(0), false), TabMove::Focus(0));
        assert_eq!(tab_move(1, Some(0), true), TabMove::Focus(0));
        // 목록이 줄어 위치가 범위를 벗어났으면 끝으로 본다.
        assert_eq!(tab_move(2, Some(5), false), TabMove::Focus(0));
        assert_eq!(tab_move(2, Some(5), true), TabMove::Focus(1));
    }

    #[test]
    fn avatar_color_is_stable_and_in_range() {
        let id = "018f3c1e-7b2a-7c3d-9e4f-0a1b2c3d4e5f";
        assert_eq!(avatar_color(id), avatar_color(id), "같은 구성원은 같은 색");
        assert_eq!(avatar_color(""), 0x811c_9dc5_u32 % AVATAR_COLORS);
        // 알려진 FNV-1a 값: "a" → 0xe40c292c.
        assert_eq!(avatar_color("a"), 0xe40c_292c_u32 % AVATAR_COLORS);
        let keys: Vec<String> = (0..200).map(|n| format!("member-{n}")).collect();
        let mut seen = [0_u32; AVATAR_COLORS as usize];
        for key in &keys {
            let color = avatar_color(key);
            assert!(color < AVATAR_COLORS);
            seen[color as usize] += 1;
        }
        assert!(
            seen.iter().all(|count| *count > 0),
            "여섯 색을 모두 쓴다: {seen:?}"
        );
    }

    #[test]
    fn skeleton_rows_vary_in_width() {
        let widths: Vec<u32> = (0..5).map(skeleton_width).collect();
        assert_eq!(widths, vec![92, 100, 76, 58, 92]);
        assert!(widths.iter().all(|w| (1..=100).contains(w)));
    }

    #[test]
    fn meta_drops_blank_items() {
        assert_eq!(
            meta_items(["개발팀", " 팀장 ", "", "  "]),
            vec!["개발팀", "팀장"]
        );
        assert!(meta_items(Vec::<String>::new()).is_empty());
    }

    #[test]
    fn stat_values_split_numbers_from_units() {
        let shown = |value: &str| -> Vec<(String, bool)> {
            stat_parts(value)
                .into_iter()
                .map(|p| (p.text, p.unit))
                .collect()
        };
        assert_eq!(shown("3건"), vec![("3".into(), false), ("건".into(), true)]);
        assert_eq!(
            shown("12시간 30분"),
            vec![
                ("12".into(), false),
                ("시간 ".into(), true),
                ("30".into(), false),
                ("분".into(), true)
            ]
        );
        assert_eq!(
            shown("5 / 8명"),
            vec![
                ("5".into(), false),
                (" / ".into(), true),
                ("8".into(), false),
                ("명".into(), true)
            ]
        );
        assert_eq!(
            shown("12.5일"),
            vec![("12.5".into(), false), ("일".into(), true)]
        );
        assert_eq!(
            shown("16:15"),
            vec![("16:15".into(), false)],
            "시각은 통째로 숫자"
        );
        assert_eq!(shown("-"), vec![("-".into(), false)]);
        assert_eq!(shown("없음"), vec![("없음".into(), false)]);
        assert_eq!(shown(""), vec![("".into(), false)]);
    }

    #[test]
    fn pending_tab_applies_only_to_its_screen() {
        let pending = (Route::AdminLeave {}, "requests");
        assert_eq!(
            initial_tab(Some(&pending), &Route::AdminLeave {}, "balances"),
            "requests"
        );
        assert_eq!(
            initial_tab(Some(&pending), &Route::AdminAttendance {}, "records"),
            "records",
            "다른 화면으로 온 요청"
        );
        assert_eq!(
            initial_tab(None, &Route::AdminLeave {}, "balances"),
            "balances"
        );
    }

    #[test]
    fn later_pages_skip_items_already_shown() {
        let merged = merge_unique(vec![(1, "a"), (2, "b")], vec![(2, "b2"), (3, "c")], |x| x.0);
        assert_eq!(
            merged,
            vec![(1, "a"), (2, "b"), (3, "c")],
            "먼저 온 것을 둔다"
        );
        assert_eq!(
            merge_unique(Vec::<(u8, u8)>::new(), vec![], |x| x.0),
            vec![]
        );
        assert_eq!(
            merge_unique(vec![], vec![(1, 1), (1, 2)], |x| x.0),
            vec![(1, 1)]
        );
    }

    #[test]
    fn next_cursor_prefers_the_latest_page() {
        assert_eq!(next_cursor(None, None), None);
        assert_eq!(next_cursor(Some("a"), None), Some("a".into()));
        assert_eq!(
            next_cursor(Some("a"), Some(Some("b".into()))),
            Some("b".into())
        );
        assert_eq!(
            next_cursor(Some("a"), Some(None)),
            None,
            "마지막 페이지까지 왔다"
        );
    }
}
