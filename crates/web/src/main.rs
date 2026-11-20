//! pango-groupware 웹 UI 진입점.

mod api;
mod badges;
mod calendar;
mod format;
mod layout;
mod pages;
mod route;
mod session;
mod text;
mod ui;

use dioxus::prelude::*;

use crate::route::Route;
use crate::session::{provide_session, provide_toasts};
use crate::ui::ToastHost;

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    provide_session();
    provide_toasts();
    rsx! {
        Router::<Route> {}
        ToastHost {}
    }
}
