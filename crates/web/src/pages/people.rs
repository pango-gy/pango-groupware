//! 구성원 목록(조직도).
//!
//! 서버(`GET /members`)는 재직자만 돌려주고, 검색어는 **이름**만 비교하며, 부서 필터는 그 부서에
//! **직접** 속한 구성원만 고른다(하위 부서는 포함하지 않는다).

use dioxus::prelude::*;
use pango_api::org::{DepartmentDto, MemberSummary};

use crate::api;
use crate::format;
use crate::org_tree::{self, DepartmentRow};
use crate::ui::{
    loaded_or_empty, resource_view, Avatar, Card, EmptyState, IconName, PageHeader, SelectField,
    TextField,
};

#[component]
pub fn People() -> Element {
    let mut search = use_signal(String::new);
    let mut department = use_signal(String::new);

    let departments =
        use_resource(|| async { api::get::<Vec<DepartmentDto>>("/departments").await });
    let members = use_resource(move || async move {
        let pairs = org_tree::member_query(&search(), &department(), false);
        api::get::<Vec<MemberSummary>>(&format!("/members{}", api::query_string(&pairs))).await
    });

    let department_list = loaded_or_empty(&departments);
    let department_options = org_tree::department_options(&department_list, "전체 부서");
    let rows = org_tree::department_tree(&department_list);

    rsx! {
        PageHeader { title: "구성원" }
        div { class: "people-layout",
            DepartmentFilter {
                rows,
                selected: department(),
                onselect: move |value| department.set(value),
            }
            div { class: "people-main",
                div { class: "filters people-filters",
                    TextField {
                        label: "검색",
                        kind: "search",
                        value: search(),
                        placeholder: "이름으로 찾기",
                        maxlength: 50,
                        oninput: move |value| search.set(value),
                    }
                    div { class: "only-mobile",
                        SelectField {
                            label: "부서",
                            value: department(),
                            options: department_options,
                            onchange: move |value| department.set(value),
                        }
                    }
                }
                {resource_view(members, |list: Vec<MemberSummary>| rsx! {
                    if list.is_empty() {
                        EmptyState { title: "조건에 맞는 구성원이 없습니다.", icon: IconName::People }
                    } else {
                        Card {
                            ul { class: "list",
                                for member in list {
                                    li { key: "{member.id}", class: "list-row",
                                        Avatar { name: member.name.clone() }
                                        div { class: "list-row-main",
                                            strong { "{member.name}" }
                                            small {
                                                {format::org_line(member.department_name.as_deref(), member.position_name.as_deref())}
                                            }
                                        }
                                        div { class: "list-row-side people-contact",
                                            if let Some(email) = &member.email {
                                                a { href: "mailto:{email}", "{email}" }
                                            }
                                            if let Some(phone) = &member.phone {
                                                a { href: "tel:{phone}", "{phone}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                })}
            }
        }
    }
}

/// 데스크톱 왼쪽의 부서 트리 필터. 모바일에서는 숨기고 선택 상자를 쓴다.
#[component]
fn DepartmentFilter(
    rows: Vec<DepartmentRow>,
    /// 선택한 부서 ID. `""`이면 전체.
    selected: String,
    onselect: EventHandler<String>,
) -> Element {
    rsx! {
        nav { class: "dept-nav only-desktop", "aria-label": "부서",
            button {
                class: if selected.is_empty() { "dept-nav-item dept-nav-item-active" } else { "dept-nav-item" },
                r#type: "button",
                "aria-pressed": selected.is_empty(),
                onclick: move |_| onselect.call(String::new()),
                span { class: "dept-nav-name", "전체" }
            }
            for row in rows {
                button {
                    key: "{row.department.id}",
                    class: if selected == row.department.id.to_string() { "dept-nav-item dept-nav-item-active" } else { "dept-nav-item" },
                    style: "--depth: {row.indent()}",
                    r#type: "button",
                    "aria-pressed": selected == row.department.id.to_string(),
                    onclick: {
                        let id = row.department.id.to_string();
                        move |_| onselect.call(id.clone())
                    },
                    span { class: "dept-nav-name", "{row.department.name}" }
                    span { class: "dept-nav-count", "aria-label": "구성원 {row.department.member_count}명",
                        "{row.department.member_count}"
                    }
                }
            }
            p { class: "dept-nav-hint", "선택한 부서에 직접 속한 구성원만 보입니다." }
        }
    }
}
