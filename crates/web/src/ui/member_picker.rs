//! 구성원 선택기. 결재선·참조·조직장 지정 등에서 함께 쓴다.

use dioxus::prelude::*;
use pango_api::org::MemberSummary;
use pango_core::id::MemberId;

use super::{Avatar, Icon, IconName};
use crate::api;
use crate::format;

/// 이름으로 찾아 한 명을 고른다. 고르면 `onpick`이 불리고 검색어가 비워진다.
///
/// `exclude`에 있는 구성원(이미 고른 사람, 본인 등)은 결과에 나오지 않는다.
#[component]
pub fn MemberPicker(
    #[props(into)] label: String,
    #[props(default)] exclude: Vec<MemberId>,
    onpick: EventHandler<MemberSummary>,
    #[props(into, default = "이름으로 찾기".to_owned())] placeholder: String,
) -> Element {
    let mut term = use_signal(String::new);
    let results = use_resource(move || async move {
        let query = term();
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        api::get::<Vec<MemberSummary>>(&format!("/members?search={}", api::encode(query.trim())))
            .await
    });
    let candidates: Vec<MemberSummary> = match &*results.read() {
        Some(Ok(list)) => list
            .iter()
            .filter(|m| !exclude.contains(&m.id))
            .take(8)
            .cloned()
            .collect(),
        _ => Vec::new(),
    };
    let searching = !term().trim().is_empty();

    rsx! {
        div { class: "picker",
            label { class: "field",
                span { class: "field-label", "{label}" }
                input {
                    class: "input",
                    r#type: "search",
                    value: "{term}",
                    placeholder: "{placeholder}",
                    maxlength: 50,
                    autocomplete: "off",
                    oninput: move |event| term.set(event.value()),
                }
            }
            if searching {
                div { class: "picker-results",
                    if candidates.is_empty() {
                        p { class: "picker-empty", role: "status", "찾는 구성원이 없습니다." }
                    } else {
                        // 목록 상자에는 선택지만 둔다(빈 안내는 밖에).
                        div { role: "listbox", "aria-label": "{label} 검색 결과",
                            for member in candidates {
                                button {
                                    key: "{member.id}",
                                    class: "picker-option",
                                    r#type: "button",
                                    role: "option",
                                    "aria-selected": "false",
                                    onclick: {
                                        let member = member.clone();
                                        move |_| {
                                            term.set(String::new());
                                            onpick.call(member.clone());
                                        }
                                    },
                                    Avatar { name: member.name.clone() }
                                    span { class: "picker-option-text",
                                        strong { "{member.name}" }
                                        small { {format::org_line(member.department_name.as_deref(), member.position_name.as_deref())} }
                                    }
                                    Icon { name: IconName::Plus }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// 고른 구성원 한 명과 선택 해제 버튼. 해제하면 보통 [`MemberPicker`]로 돌아간다.
#[component]
pub fn PickedMember(
    #[props(into)] name: String,
    /// 해제 버튼의 접근성 이름(예: "조직장 지정 해제").
    #[props(into)]
    clear_label: String,
    onclear: EventHandler<()>,
) -> Element {
    rsx! {
        div { class: "picked-member",
            Avatar { name: name.clone() }
            span { class: "picked-member-name", "{name}" }
            button {
                class: "icon-btn",
                r#type: "button",
                "aria-label": "{clear_label}",
                onclick: move |_| onclear.call(()),
                Icon { name: IconName::Close }
            }
        }
    }
}
