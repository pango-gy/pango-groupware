//! pango-groupware 웹 UI 진입점.

mod api;
mod badges;
mod calendar;
mod company;
mod format;
mod layout;
mod org_tree;
mod pages;
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

#[component]
fn App() -> Element {
    provide_session();
    provide_company();
    provide_pending_tab();
    provide_toasts();
    rsx! {
        Router::<Route> {}
        ToastHost {}
    }
}
