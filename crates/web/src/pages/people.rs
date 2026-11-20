//! 구성원 목록(조직도).

use dioxus::prelude::*;
use pango_api::org::{DepartmentDto, MemberSummary};

use crate::api;
use crate::ui::{
    resource_view, Avatar, Card, EmptyState, IconName, PageHeader, SelectField, TextField,
};

#[component]
pub fn People() -> Element {
    let mut search = use_signal(String::new);
    let mut department = use_signal(String::new);

    let departments =
        use_resource(|| async { api::get::<Vec<DepartmentDto>>("/departments").await });
    let members = use_resource(move || async move {
        let mut path = format!("/members?search={}", api::encode(&search()));
        if !department().is_empty() {
            path.push_str(&format!("&department_id={}", api::encode(&department())));
        }
        api::get::<Vec<MemberSummary>>(&path).await
    });

    let department_options = match &*departments.read() {
        Some(Ok(list)) => std::iter::once((String::new(), "전체 부서".to_owned()))
            .chain(list.iter().map(|d| (d.id.to_string(), d.name.clone())))
            .collect(),
        _ => vec![(String::new(), "전체 부서".to_owned())],
    };

    rsx! {
        PageHeader { title: "구성원" }
        div { class: "filters",
            TextField {
                label: "검색",
                value: search(),
                placeholder: "이름으로 찾기",
                maxlength: 50,
                oninput: move |value| search.set(value),
            }
            SelectField {
                label: "부서",
                value: department(),
                options: department_options,
                onchange: move |value| department.set(value),
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
                                        {[member.department_name.clone(), member.position_name.clone()]
                                            .into_iter()
                                            .flatten()
                                            .collect::<Vec<_>>()
                                            .join(" · ")}
                                    }
                                }
                                div { class: "list-row-side",
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
