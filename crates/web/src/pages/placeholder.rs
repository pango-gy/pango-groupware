//! 아직 만들지 않은 화면의 자리 표시.

use dioxus::prelude::*;

use crate::ui::{EmptyState, IconName, PageHeader};

#[component]
pub fn ComingSoon(title: String) -> Element {
    rsx! {
        PageHeader { title }
        EmptyState { title: "준비 중인 화면입니다.", icon: IconName::Clock }
    }
}

#[component]
pub fn Home() -> Element {
    rsx! { ComingSoon { title: "홈" } }
}
