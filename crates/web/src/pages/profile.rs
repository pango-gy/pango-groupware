//! 내 정보와 로그인 기기 관리.

use dioxus::prelude::*;
use pango_api::auth::SessionInfo;
use pango_api::Empty;

use crate::api;
use crate::format;
use crate::route::Route;
use crate::session::{self, use_session};
use crate::text;
use crate::ui::{
    report_error, report_success, resource_view, Avatar, Badge, Button, ButtonKind, Card,
    PageHeader, Tone,
};

#[component]
pub fn Profile() -> Element {
    let session = use_session();
    let nav = navigator();
    let mut sessions =
        use_resource(|| async { api::get::<Vec<SessionInfo>>("/me/sessions").await });
    let Some(me) = session.me() else {
        return rsx! {};
    };
    let tz = format::member_zone(Some(&me));

    let logout = move |_| session::logout(session, nav);
    let revoke = move |id: String| async move {
        match api::delete::<Empty>(&format!("/me/sessions/{id}")).await {
            Ok(_) => {
                report_success("기기에서 로그아웃했습니다.");
                sessions.restart();
            }
            Err(error) => report_error(&error),
        }
    };

    rsx! {
        PageHeader { title: "내 정보" }
        Card {
            div { class: "profile",
                Avatar { name: me.name.clone(), large: true }
                div {
                    h2 { class: "profile-name", "{me.name}" }
                    p { class: "profile-meta",
                        {[me.department_name.clone(), me.position_name.clone()]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" · ")}
                    }
                    Badge { tone: Tone::Info, {text::role_label(me.role)} }
                }
            }
            dl { class: "detail-list",
                dt { "아이디" }
                dd { "{me.login_id}" }
                dt { "이메일" }
                dd { {me.email.clone().unwrap_or_else(|| "-".into())} }
                dt { "전화번호" }
                dd { {me.phone.clone().unwrap_or_else(|| "-".into())} }
            }
            div { class: "card-actions",
                Link { class: "btn btn-secondary", to: Route::ChangePassword {}, "비밀번호 변경" }
                Button { kind: ButtonKind::Ghost, onclick: logout, "로그아웃" }
            }
        }
        Card { title: "로그인된 기기",
            {resource_view(sessions, |list: Vec<SessionInfo>| rsx! {
                ul { class: "list",
                    for item in list {
                        li { key: "{item.id}", class: "list-row",
                            div { class: "list-row-main",
                                strong {
                                    {item.user_agent.clone().unwrap_or_else(|| "알 수 없는 기기".into())}
                                }
                                small {
                                    "마지막 사용 {format::datetime(item.last_seen_at, tz)}"
                                    if let Some(ip) = &item.ip {
                                        " · {ip}"
                                    }
                                }
                            }
                            div { class: "list-row-side",
                                if item.current {
                                    Badge { tone: Tone::Success, "현재 기기" }
                                } else {
                                    Button {
                                        kind: ButtonKind::Secondary,
                                        small: true,
                                        onclick: {
                                            let id = item.id.to_string();
                                            move |_| revoke(id.clone())
                                        },
                                        "로그아웃"
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
