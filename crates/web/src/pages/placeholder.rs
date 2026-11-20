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

macro_rules! placeholders {
    ($($name:ident => $title:literal),+ $(,)?) => {$(
        #[component]
        pub fn $name() -> Element {
            rsx! { ComingSoon { title: $title } }
        }
    )+};
}

placeholders! {
    Home => "홈",
    Leave => "휴가",
    LeaveNew => "휴가 신청",
    ApprovalList => "결재",
    ApprovalNew => "새 결재 문서",
    Notices => "공지",
    Notifications => "알림",
}

#[component]
pub fn ApprovalDetail(id: String) -> Element {
    let _ = id;
    rsx! { ComingSoon { title: "결재 문서" } }
}

#[component]
pub fn NoticeDetail(id: String) -> Element {
    let _ = id;
    rsx! { ComingSoon { title: "공지" } }
}
