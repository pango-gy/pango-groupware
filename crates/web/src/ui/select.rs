//! 선택 상자.
//!
//! 브라우저 기본 `<select>`는 펼친 목록을 꾸밀 수 없어(운영체제가 그린다) 화면마다 모양이
//! 달라진다. 그래서 단추 + 목록(listbox)으로 직접 그린다. 접근성은 WAI-ARIA의 "선택 전용
//! 콤보박스" 패턴을 따른다: 초점은 단추에 두고, 목록에서 고르는 줄은 `aria-activedescendant`로
//! 알린다. 데스크톱은 단추 아래에 뜨는 목록, 모바일은 아래에서 올라오는 시트다.

use dioxus::prelude::*;

use super::{Icon, IconName};

/// 목록 안에서 움직이는 방향.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Next,
    Prev,
    First,
    Last,
}

/// 키 입력이 닫힌 단추·열린 목록에서 무엇을 뜻하는지.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    Open,
    Move(Move),
    Choose,
    Close,
    /// 글자로 시작하는 항목 찾기(타이핑해서 고르기).
    Seek(char),
    /// 아무 일도 하지 않는다(기본 동작을 막지 않음).
    Ignore,
}

/// 눌린 키를 동작으로 바꾼다. `key`는 `KeyboardEvent.key` 값.
pub fn key_action(key: &str, open: bool) -> KeyAction {
    match (key, open) {
        ("ArrowDown" | "ArrowUp" | "Enter" | " ", false) => KeyAction::Open,
        ("ArrowDown", true) => KeyAction::Move(Move::Next),
        ("ArrowUp", true) => KeyAction::Move(Move::Prev),
        ("Home", true) => KeyAction::Move(Move::First),
        ("End", true) => KeyAction::Move(Move::Last),
        ("Enter" | " ", true) => KeyAction::Choose,
        ("Escape" | "Tab", true) => KeyAction::Close,
        (other, _) => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_control() && !c.is_whitespace() => KeyAction::Seek(c),
                _ => KeyAction::Ignore,
            }
        }
    }
}

/// 움직인 뒤의 위치. 끝에서 멈춘다(돌아가지 않음 — 목록 끝을 알 수 있게).
pub fn moved(current: usize, len: usize, step: Move) -> usize {
    if len == 0 {
        return 0;
    }
    let last = len - 1;
    match step {
        Move::Next => (current + 1).min(last),
        Move::Prev => current.saturating_sub(1),
        Move::First => 0,
        Move::Last => last,
    }
}

/// `c`로 시작하는 다음 항목. 현재 다음부터 찾고, 없으면 처음부터 다시 찾는다(같은 글자를
/// 거듭 누르면 그 글자로 시작하는 항목을 차례로 돈다). 대소문자는 구분하지 않는다.
pub fn seek(labels: &[String], current: usize, c: char) -> Option<usize> {
    let wanted: String = c.to_lowercase().collect();
    let starts = |i: &usize| labels[*i].trim_start().to_lowercase().starts_with(&wanted);
    let len = labels.len();
    (1..=len)
        .map(|offset| (current + offset) % len)
        .find(starts)
}

/// 현재 값의 위치. 없으면 0(목록을 열었을 때 첫 줄에서 시작).
pub fn index_of(options: &[(String, String)], value: &str) -> usize {
    options.iter().position(|(v, _)| v == value).unwrap_or(0)
}

/// 단추에 보일 문구. 값이 목록에 없으면 첫 항목 문구(보통 "전체 …"), 목록이 비면 빈 문자열.
pub fn display_label(options: &[(String, String)], value: &str) -> String {
    options
        .iter()
        .find(|(v, _)| v == value)
        .or_else(|| options.first())
        .map(|(_, label)| label.clone())
        .unwrap_or_default()
}

fn next_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!("select-{}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// 고르는 줄이 목록 밖으로 나가면 보이게 굴린다.
fn reveal(id: &str) {
    let element = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id(id));
    if let Some(element) = element {
        element.scroll_into_view_with_bool(false);
    }
}

/// 라벨이 붙은 선택 상자. `options`는 `(값, 표시 문구)`.
#[component]
pub fn SelectField(
    #[props(into)] label: String,
    #[props(into)] value: String,
    options: Vec<(String, String)>,
    onchange: EventHandler<String>,
    #[props(default)] required: bool,
    #[props(default)] disabled: bool,
) -> Element {
    let id = use_hook(next_id);
    let mut open = use_signal(|| false);
    let mut active = use_signal(|| 0usize);

    let label_id = format!("{id}-label");
    let list_id = format!("{id}-list");
    let option_id = |index: usize| format!("{id}-opt-{index}");
    let labels: Vec<String> = options.iter().map(|(_, l)| l.clone()).collect();
    let current = index_of(&options, &value);
    let shown = display_label(&options, &value);
    let len = options.len();

    let mut show = move |at: usize| {
        active.set(at);
        open.set(true);
    };
    let choose = {
        let options = options.clone();
        move |index: usize| {
            if let Some((picked, _)) = options.get(index) {
                onchange.call(picked.clone());
            }
            open.set(false);
        }
    };

    let onkeydown = {
        let id = id.clone();
        let labels = labels.clone();
        let options = options.clone();
        let mut choose = choose.clone();
        move |event: KeyboardEvent| {
            if disabled {
                return;
            }
            let key = event.key().to_string();
            let action = key_action(&key, open());
            let now = active();
            let mut go = |at: usize| {
                active.set(at);
                reveal(&format!("{id}-opt-{at}"));
            };
            match action {
                KeyAction::Ignore => return,
                // Tab은 초점 이동을 막지 않고 목록만 닫는다.
                KeyAction::Close if key == "Tab" => {
                    open.set(false);
                    return;
                }
                KeyAction::Close => {
                    // 열린 목록을 닫는 Esc가 대화 상자까지 닫지 않게 한다.
                    event.stop_propagation();
                    open.set(false);
                }
                KeyAction::Open => show(current),
                KeyAction::Move(step) => go(moved(now, len, step)),
                KeyAction::Choose => choose(now),
                KeyAction::Seek(c) => {
                    let from = if open() { now } else { current };
                    if let Some(at) = seek(&labels, from, c) {
                        if open() {
                            go(at);
                        } else if let Some((picked, _)) = options.get(at) {
                            // 닫힌 채로 글자를 누르면 바로 그 항목을 고른다(기본 select와 같게).
                            onchange.call(picked.clone());
                        }
                    }
                }
            }
            event.prevent_default();
        }
    };

    rsx! {
        div { class: if open() { "field select select-open" } else { "field select" },
            span { id: "{label_id}", class: "field-label",
                "{label}"
                if required {
                    span { class: "field-required", "aria-hidden": "true", " *" }
                }
            }
            button {
                class: "input select-trigger",
                r#type: "button",
                role: "combobox",
                disabled,
                "aria-labelledby": "{label_id}",
                "aria-haspopup": "listbox",
                "aria-expanded": open(),
                "aria-controls": "{list_id}",
                "aria-required": required,
                "aria-activedescendant": if open() { option_id(active()) } else { String::new() },
                onclick: move |_| {
                    if open() {
                        open.set(false);
                    } else {
                        show(current);
                    }
                },
                onkeydown,
                span { class: "select-value", "{shown}" }
                Icon { name: IconName::ChevronDown }
            }
            if open() {
                // 바깥을 누르면 닫는 투명한 층(모바일에서는 어두운 배경).
                div { class: "select-backdrop", onclick: move |_| open.set(false) }
                div { class: "select-popover",
                    p { class: "select-sheet-title", "{label}" }
                    ul {
                        id: "{list_id}",
                        class: "select-list",
                        role: "listbox",
                        "aria-labelledby": "{label_id}",
                        for (index , (option_value , text)) in options.iter().enumerate() {
                            li {
                                key: "{option_value}",
                                id: option_id(index),
                                role: "option",
                                class: match (index == active(), *option_value == value) {
                                    (true, true) => "select-option select-option-active select-option-selected",
                                    (true, false) => "select-option select-option-active",
                                    (false, true) => "select-option select-option-selected",
                                    (false, false) => "select-option",
                                },
                                "aria-selected": *option_value == value,
                                // 누르는 순간 단추의 초점이 빠지지 않게 한다.
                                onmousedown: move |event| event.prevent_default(),
                                onmouseenter: move |_| active.set(index),
                                onclick: {
                                    let mut choose = choose.clone();
                                    move |_| choose(index)
                                },
                                span { class: "select-option-text", "{text}" }
                                if *option_value == value {
                                    Icon { name: IconName::Check }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn options(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(v, l)| ((*v).to_owned(), (*l).to_owned()))
            .collect()
    }

    #[test]
    fn keys_open_when_closed_and_navigate_when_open() {
        for key in ["ArrowDown", "ArrowUp", "Enter", " "] {
            assert_eq!(key_action(key, false), KeyAction::Open, "{key:?}");
        }
        assert_eq!(key_action("ArrowDown", true), KeyAction::Move(Move::Next));
        assert_eq!(key_action("ArrowUp", true), KeyAction::Move(Move::Prev));
        assert_eq!(key_action("Home", true), KeyAction::Move(Move::First));
        assert_eq!(key_action("End", true), KeyAction::Move(Move::Last));
        assert_eq!(key_action("Enter", true), KeyAction::Choose);
        assert_eq!(key_action(" ", true), KeyAction::Choose);
        assert_eq!(key_action("Escape", true), KeyAction::Close);
        assert_eq!(key_action("Tab", true), KeyAction::Close);
        // 닫혀 있을 때 Esc·Tab·방향키 외 키는 막지 않는다.
        assert_eq!(key_action("Escape", false), KeyAction::Ignore);
        assert_eq!(key_action("Tab", false), KeyAction::Ignore);
        assert_eq!(key_action("Shift", true), KeyAction::Ignore);
        assert_eq!(key_action("F5", false), KeyAction::Ignore);
    }

    #[test]
    fn single_characters_seek() {
        assert_eq!(key_action("진", true), KeyAction::Seek('진'));
        assert_eq!(key_action("a", false), KeyAction::Seek('a'));
        assert_eq!(key_action("\u{7}", true), KeyAction::Ignore);
        assert_eq!(key_action("ab", true), KeyAction::Ignore);
        assert_eq!(key_action("", true), KeyAction::Ignore);
    }

    #[test]
    fn movement_stops_at_ends() {
        assert_eq!(moved(0, 4, Move::Prev), 0);
        assert_eq!(moved(3, 4, Move::Next), 3);
        assert_eq!(moved(1, 4, Move::Next), 2);
        assert_eq!(moved(2, 4, Move::First), 0);
        assert_eq!(moved(0, 4, Move::Last), 3);
        assert_eq!(moved(0, 0, Move::Next), 0, "빈 목록");
        assert_eq!(moved(7, 1, Move::Next), 0, "범위 밖 위치도 끝으로");
    }

    #[test]
    fn seek_cycles_through_matches_from_after_current() {
        let items = labels(&["전체 상태", "진행 중", "승인", "반려", "회수", "진행 보류"]);
        assert_eq!(seek(&items, 0, '진'), Some(1));
        assert_eq!(
            seek(&items, 1, '진'),
            Some(5),
            "같은 글자를 다시 누르면 다음 항목"
        );
        assert_eq!(seek(&items, 5, '진'), Some(1), "끝에서 처음으로");
        assert_eq!(seek(&items, 0, '없'), None);
        assert_eq!(
            seek(&labels(&["Apple", "banana"]), 1, 'a'),
            Some(0),
            "대소문자 무시"
        );
        assert_eq!(
            seek(&labels(&["  공백"]), 0, '공'),
            Some(0),
            "앞 공백 무시, 하나뿐이면 자기 자신"
        );
        assert_eq!(seek(&[], 0, 'a'), None);
    }

    #[test]
    fn current_index_and_label() {
        let opts = options(&[
            ("", "전체 상태"),
            ("in_progress", "진행 중"),
            ("approved", "승인"),
        ]);
        assert_eq!(index_of(&opts, "approved"), 2);
        assert_eq!(index_of(&opts, "missing"), 0);
        assert_eq!(display_label(&opts, "in_progress"), "진행 중");
        assert_eq!(
            display_label(&opts, "missing"),
            "전체 상태",
            "모르는 값이면 첫 항목"
        );
        assert_eq!(display_label(&[], "x"), "");
    }
}
