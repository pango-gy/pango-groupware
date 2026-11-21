//! pango-groupware 웹 UI 진입점.

mod api;
mod badges;
mod calendar;
mod company;
mod format;
mod layout;
mod org_tree;
mod pages;
mod platform;
mod route;
mod session;
mod text;
mod ui;

use dioxus::prelude::*;

use crate::company::provide_company;
use crate::route::Route;
use crate::session::{provide_session, provide_toasts};
use crate::ui::{provide_pending_tab, ToastHost};

fn main() {
    dioxus::launch(App);
}

/// 앱이 쓸 서버 주소. 웹은 늘 자기 출처라 쓰지 않는다.
#[derive(Clone, Copy)]
pub struct ServerConfig {
    pub origin: Signal<Option<String>>,
}

impl ServerConfig {
    /// 서버 주소를 지운다(로그인 화면의 "서버 바꾸기"). 서버 주소 화면으로 돌아간다.
    pub fn clear(mut self) {
        platform::set_server_origin(None);
        self.origin.set(None);
    }
}

pub fn use_server_config() -> ServerConfig {
    use_context::<ServerConfig>()
}

#[component]
fn App() -> Element {
    let mut config = use_context_provider(|| ServerConfig {
        origin: Signal::new(platform::server_origin()),
    });
    let needs_server = platform::BEARER && config.origin.read().is_none();
    rsx! {
        AppStyles {}
        if needs_server {
            pages::ServerSetup {
                onsaved: move |origin: String| {
                    platform::set_server_origin(Some(&origin));
                    config.origin.set(Some(origin));
                },
            }
        } else {
            Main {}
        }
    }
}

/// 앱(모바일)은 index.html이 없으므로 글꼴·스타일을 여기서 싣는다. 웹은 index.html이 싣는다.
#[component]
fn AppStyles() -> Element {
    #[cfg(feature = "mobile")]
    {
        const FONTS: Asset = asset!("/assets/fonts", AssetOptions::folder());
        const STYLE: Asset = asset!("/assets/style.css");
        rsx! {
            document::Meta { name: "viewport", content: "width=device-width, initial-scale=1, viewport-fit=cover" }
            document::Stylesheet { href: "{FONTS}/pretendard-1.3.9/pretendard.css" }
            document::Stylesheet { href: STYLE }
        }
    }
    #[cfg(not(feature = "mobile"))]
    {
        rsx! {}
    }
}

#[component]
fn Main() -> Element {
    provide_session();
    provide_company();
    provide_pending_tab();
    provide_toasts();
    rsx! {
        Router::<Route> {}
        ToastHost {}
    }
}
