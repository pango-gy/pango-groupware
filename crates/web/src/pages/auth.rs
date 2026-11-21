//! 로그인과 비밀번호 변경.

use dioxus::prelude::*;
use pango_api::auth::{ChangePasswordRequest, LoginRequest, LoginResponse};
use pango_api::Empty;

use crate::api;
use crate::platform::{self, server_url};
use crate::route::Route;
use crate::session::{self, use_session, SessionState};
use crate::ui::{Button, ButtonKind, ErrorState, FormError, Skeleton, TextField};

#[component]
pub fn Login() -> Element {
    let mut session = use_session();
    let nav = navigator();
    let mut login_id = use_signal(String::new);
    let mut password = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);

    // 이미 로그인돼 있으면 홈으로.
    use_effect(move || {
        if session.me().is_some() {
            nav.replace(Route::Home {});
        }
    });

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        let request = LoginRequest {
            login_id: login_id(),
            password: password(),
            // 앱은 토큰을 본문으로 받아 키체인 등에 둔다. 웹은 HttpOnly 쿠키.
            token_in_body: platform::BEARER,
        };
        match api::post::<_, LoginResponse>("/auth/login", &request).await {
            Ok(response) => {
                password.set(String::new());
                if let Some(token) = response.token {
                    platform::set_token(Some(token));
                }
                session.set(Some(response.me));
            }
            Err(failure) => error.set(failure.message()),
        }
        busy.set(false);
    };

    match session.state() {
        SessionState::SignedOut => {}
        // 쿠키 세션이 살아 있을 수 있다. 다시 로그인하면 서버 세션이 하나 더 생기므로 다시 시도를 권한다.
        SessionState::Unreachable(message) => {
            return rsx! {
                main { class: "auth",
                    ErrorState { message, onretry: move |_| session.retry() }
                }
            };
        }
        SessionState::Loading | SessionState::SignedIn(_) => {
            return rsx! { div { class: "boot", Skeleton {} } };
        }
    }
    rsx! {
        main { class: "auth",
            form { class: "auth-card", onsubmit: submit,
                p { class: "auth-brand", "pango" }
                h1 { class: "auth-title", "로그인" }
                FormError { message: error() }
                TextField {
                    label: "아이디",
                    value: login_id(),
                    autocomplete: "username",
                    required: true,
                    maxlength: 32,
                    oninput: move |value| login_id.set(value),
                }
                TextField {
                    label: "비밀번호",
                    kind: "password",
                    value: password(),
                    autocomplete: "current-password",
                    required: true,
                    maxlength: 128,
                    oninput: move |value| password.set(value),
                }
                Button { submit: true, block: true, large: true, busy: busy(), "로그인" }
                p { class: "auth-hint", "계정은 관리자가 발급해요. 비밀번호를 잊었다면 관리자에게 초기화를 요청해 주세요." }
                if let Some(origin) = platform::server_origin() {
                    p { class: "auth-server",
                        span { class: "auth-server-origin", "{origin}" }
                        button {
                            r#type: "button",
                            class: "link-btn",
                            onclick: move |_| crate::use_server_config().clear(),
                            "서버 바꾸기"
                        }
                    }
                }
            }
        }
    }
}

/// 앱 첫 실행: 접속할 회사 서버 주소를 받는다. 주소가 맞는지는 서버에 한 번 물어 확인한다.
#[component]
pub fn ServerSetup(onsaved: EventHandler<String>) -> Element {
    let mut input = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        let origin = match server_url::normalize(&input()) {
            Ok(origin) => origin,
            Err(failure) => {
                error.set(failure.message().into());
                return;
            }
        };
        busy.set(true);
        error.set(String::new());
        // 임시로 이 주소를 써서 공개 경로(`/health`)를 물어본다. 아니면 되돌린다.
        let previous = platform::server_origin();
        platform::set_server_origin(Some(&origin));
        let health = api::get::<serde_json::Value>("/health").await;
        match health {
            Ok(body) if body.get("status").and_then(|s| s.as_str()) == Some("ok") => {
                onsaved.call(origin);
            }
            failure => {
                let failure = failure.err();
                platform::set_server_origin(previous.as_deref());
                error.set(if failure.is_some_and(|f| f.status == 0) {
                    "서버에 연결하지 못했어요. 주소와 네트워크를 확인해 주세요.".into()
                } else {
                    "pango 서버가 아닌 것 같아요. 주소를 확인해 주세요.".into()
                });
            }
        }
        busy.set(false);
    };

    rsx! {
        main { class: "auth",
            form { class: "auth-card", onsubmit: submit,
                p { class: "auth-brand", "pango" }
                h1 { class: "auth-title", "서버 연결" }
                p { class: "auth-lead", "회사에서 쓰는 그룹웨어 주소를 입력해 주세요." }
                FormError { message: error() }
                TextField {
                    label: "서버 주소",
                    value: input(),
                    placeholder: "https://gw.example.com",
                    autocomplete: "url",
                    required: true,
                    maxlength: server_url::MAX_LEN as i64,
                    oninput: move |value| input.set(value),
                }
                Button { submit: true, block: true, large: true, busy: busy(), "연결" }
                p { class: "auth-hint", "주소를 모르면 관리자에게 물어봐 주세요." }
            }
        }
    }
}

/// 비밀번호 변경. 임시 비밀번호 상태에서는 이 화면만 쓸 수 있다.
#[component]
pub fn ChangePassword() -> Element {
    let mut session = use_session();
    let nav = navigator();
    let mut current = use_signal(String::new);
    let mut new_password = use_signal(String::new);
    let mut confirm = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);

    use_effect(move || {
        if session.state() == SessionState::SignedOut {
            nav.replace(Route::Login {});
        }
    });

    let forced = session.me().is_some_and(|me| me.must_change_password);
    let mismatch = !confirm().is_empty() && confirm() != new_password();

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        if new_password() != confirm() {
            error.set("새 비밀번호가 서로 달라요.".into());
            return;
        }
        busy.set(true);
        error.set(String::new());
        let request = ChangePasswordRequest {
            current_password: current(),
            new_password: new_password(),
        };
        match api::post::<_, Empty>("/me/password", &request).await {
            Ok(_) => {
                session.refresh().await;
                nav.replace(Route::Home {});
            }
            Err(failure) => error.set(failure.message()),
        }
        busy.set(false);
    };

    match session.state() {
        SessionState::SignedIn(_) => {}
        SessionState::Unreachable(message) => {
            return rsx! {
                main { class: "auth",
                    ErrorState { message, onretry: move |_| session.retry() }
                }
            };
        }
        SessionState::Loading | SessionState::SignedOut => {
            return rsx! { div { class: "boot", Skeleton {} } };
        }
    }
    rsx! {
        main { class: "auth",
            form { class: "auth-card", onsubmit: submit,
                h1 { class: "auth-title", "비밀번호 변경" }
                if forced {
                    p { class: "auth-hint", "임시 비밀번호로 로그인했어요. 계속하려면 새 비밀번호를 정해 주세요." }
                }
                FormError { message: error() }
                TextField {
                    label: if forced { "임시 비밀번호" } else { "현재 비밀번호" },
                    kind: "password",
                    value: current(),
                    autocomplete: "current-password",
                    required: true,
                    maxlength: 128,
                    oninput: move |value| current.set(value),
                }
                TextField {
                    label: "새 비밀번호",
                    kind: "password",
                    value: new_password(),
                    autocomplete: "new-password",
                    hint: "10자 이상이어야 하고, 아이디가 들어가면 안 돼요.",
                    required: true,
                    maxlength: 128,
                    oninput: move |value| new_password.set(value),
                }
                TextField {
                    label: "새 비밀번호 확인",
                    kind: "password",
                    value: confirm(),
                    autocomplete: "new-password",
                    error: if mismatch { "새 비밀번호와 달라요." } else { "" },
                    required: true,
                    maxlength: 128,
                    oninput: move |value| confirm.set(value),
                }
                Button { submit: true, block: true, large: true, busy: busy(), "변경" }
                if forced {
                    // 임시 비밀번호를 받은 사람이 아니면(공용 PC 등) 여기서 빠져나갈 수 있어야 한다.
                    Button {
                        kind: ButtonKind::Grey,
                        block: true,
                        disabled: busy(),
                        onclick: move |_| session::logout(session, nav),
                        "로그아웃"
                    }
                } else {
                    Link { class: "auth-link", to: Route::Profile {}, "돌아가기" }
                }
            }
        }
    }
}
