//! 공용 UI 컴포넌트. 화면은 여기 있는 조각만 조립하고, 모양은 `assets/style.css`가 정한다.

mod icon;
mod member_picker;

pub use icon::{Icon, IconName};
pub use member_picker::MemberPicker;

use dioxus::prelude::*;

use crate::api::{ApiError, ApiResult};
use crate::format;
use crate::session::{use_toasts, Session, ToastKind, Toasts};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonKind {
    #[default]
    Primary,
    Secondary,
    Ghost,
    Danger,
}

impl ButtonKind {
    const fn class(self) -> &'static str {
        match self {
            Self::Primary => "btn btn-primary",
            Self::Secondary => "btn btn-secondary",
            Self::Ghost => "btn btn-ghost",
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
    #[props(default)] small: bool,
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
    rsx! {
        button {
            class,
            r#type: if submit { "submit" } else { "button" },
            disabled: disabled || busy,
            "aria-busy": busy,
            onclick: move |event| onclick.call(event),
            if busy {
                span { class: "spinner spinner-inline", "aria-hidden": "true" }
            }
            {children}
        }
    }
}

#[component]
pub fn Card(
    #[props(into, default)] title: String,
    /// 제목 오른쪽에 놓는 요소(더보기 링크 등).
    action: Option<Element>,
    #[props(into, default)] class: String,
    children: Element,
) -> Element {
    rsx! {
        section { class: "card {class}",
            if !title.is_empty() || action.is_some() {
                header { class: "card-header",
                    h2 { class: "card-title", "{title}" }
                    {action}
                }
            }
            {children}
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
}

#[component]
pub fn Badge(#[props(default)] tone: Tone, children: Element) -> Element {
    rsx! { span { class: "badge {tone.class()}", {children} } }
}

#[component]
pub fn Avatar(#[props(into)] name: String, #[props(default)] large: bool) -> Element {
    let class = if large { "avatar avatar-lg" } else { "avatar" };
    rsx! { span { class, "aria-hidden": "true", "{format::initial(&name)}" } }
}

#[component]
pub fn Spinner() -> Element {
    rsx! {
        div { class: "loading", role: "status",
            span { class: "spinner" }
            span { class: "sr-only", "불러오는 중" }
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
            Icon { name: icon }
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
            Icon { name: IconName::Alert }
            p { class: "empty-title", "{message}" }
            Button { kind: ButtonKind::Secondary, small: true, onclick: move |_| onretry.call(()), "다시 시도" }
        }
    }
}

/// 조회 결과의 세 상태(불러오는 중·실패·성공)를 한 가지 방식으로 그린다.
pub fn resource_view<T: Clone + 'static>(
    mut resource: Resource<ApiResult<T>>,
    render: impl FnOnce(T) -> Element,
) -> Element {
    // 훅이 아닌 조회를 쓴다(이 함수는 렌더 중 여러 번, 조건부로 불릴 수 있다).
    let mut session = consume_context::<Session>();
    let state = resource.read().clone();
    match state {
        None => rsx! { Spinner {} },
        Some(Ok(value)) => render(value),
        Some(Err(error)) => {
            let message = session.handle(&error);
            rsx! { ErrorState { message, onretry: move |_| resource.restart() } }
        }
    }
}

#[component]
pub fn PageHeader(
    #[props(into)] title: String,
    #[props(into, default)] subtitle: String,
    /// 제목 오른쪽의 주요 동작 버튼.
    actions: Option<Element>,
) -> Element {
    rsx! {
        header { class: "page-header",
            div {
                h1 { class: "page-title", "{title}" }
                if !subtitle.is_empty() {
                    p { class: "page-subtitle", "{subtitle}" }
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

/// 선택 상자. `options`는 `(값, 표시 문구)`.
#[component]
pub fn SelectField(
    #[props(into)] label: String,
    #[props(into)] value: String,
    options: Vec<(String, String)>,
    onchange: EventHandler<String>,
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
            select {
                class: "input",
                required,
                disabled,
                onchange: move |event| onchange.call(event.value()),
                for (option_value , text) in options {
                    option { value: "{option_value}", selected: option_value == value, "{text}" }
                }
            }
        }
    }
}

/// 화면 위에 뜨는 대화 상자. 바깥을 누르거나 닫기 버튼으로 닫는다.
#[component]
pub fn Modal(
    #[props(into)] title: String,
    onclose: EventHandler<()>,
    children: Element,
) -> Element {
    rsx! {
        div { class: "modal-backdrop", onclick: move |_| onclose.call(()),
            div {
                class: "modal",
                role: "dialog",
                "aria-modal": "true",
                "aria-label": "{title}",
                onclick: move |event| event.stop_propagation(),
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
        div { class: "stat {tone.class()}",
            span { class: "stat-label", "{label}" }
            strong { class: "stat-value", "{value}" }
            if !hint.is_empty() {
                span { class: "stat-hint", "{hint}" }
            }
        }
    }
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
#[component]
pub fn ToastHost() -> Element {
    let mut toasts = use_toasts();
    rsx! {
        div { class: "toasts", "aria-live": "polite",
            for toast in toasts.items() {
                div {
                    key: "{toast.id}",
                    class: if toast.kind == ToastKind::Error { "toast toast-error" } else { "toast" },
                    onclick: move |_| toasts.dismiss(toast.id),
                    "{toast.message}"
                }
            }
        }
    }
}

/// 요청 실패를 토스트로 알린다(세션 만료 처리 포함). 이벤트 핸들러 안에서 부른다.
pub fn report_error(error: &ApiError) {
    let message = consume_context::<Session>().handle(error);
    consume_context::<Toasts>().error(message);
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
#[component]
pub fn MonthCalendar(
    month: crate::calendar::YearMonth,
    day: Callback<chrono::NaiveDate, Element>,
) -> Element {
    const WEEKDAYS: [&str; 7] = ["일", "월", "화", "수", "목", "금", "토"];
    rsx! {
        div { class: "calendar", role: "grid",
            div { class: "calendar-row calendar-head", role: "row",
                for name in WEEKDAYS {
                    span { class: "calendar-weekday", role: "columnheader", "{name}" }
                }
            }
            for week in month.weeks() {
                div { class: "calendar-row", role: "row",
                    for cell in week {
                        if let Some(date) = cell {
                            div { class: "calendar-cell", role: "gridcell", {day.call(date)} }
                        } else {
                            div { class: "calendar-cell calendar-cell-empty", role: "gridcell" }
                        }
                    }
                }
            }
        }
    }
}

/// 지금 시각(브라우저 시계). 목록의 상대 시각 표시에만 쓴다.
pub fn now() -> pango_core::time::Timestamp {
    let millis = js_sys::Date::now() as i64;
    pango_core::time::Timestamp::from_millis(millis)
        .unwrap_or_else(|_| pango_core::time::Timestamp::from_utc(chrono::DateTime::UNIX_EPOCH))
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
                Button { kind: ButtonKind::Secondary, onclick: move |_| oncancel.call(()), "취소" }
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
            Button { kind: ButtonKind::Secondary, busy, onclick: move |_| onclick.call(()), "더 보기" }
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
    }
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
    }

    pub fn items(&self) -> Vec<T> {
        self.items.read().clone()
    }

    pub fn busy(&self) -> bool {
        *self.busy.read()
    }

    pub fn cursor(&self, first: &pango_api::Page<T>) -> Option<String> {
        next_cursor(first.next_cursor.as_deref(), self.after.read().clone())
    }

    /// 다음 페이지를 불러와 붙인다. `path`는 인코딩된 커서로 요청 경로를 만든다.
    pub async fn load(mut self, cursor: String, path: impl Fn(&str) -> String) {
        if self.busy() {
            return;
        }
        self.busy.set(true);
        match crate::api::get::<pango_api::Page<T>>(&path(&crate::api::encode(&cursor))).await {
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
                    p { class: "secret-warning", role: "note",
                        Icon { name: IconName::Alert }
                        span { "이 창을 닫으면 다시 볼 수 없습니다. 지금 복사해 본인에게 안전하게 전달하세요." }
                    }
                    div { class: "modal-actions",
                        Button { onclick: move |_| onclose.call(()), "확인했습니다" }
                    }
                }
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

#[cfg(test)]
mod tests {
    use super::*;

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
