//! 관리자 화면.

mod attendance;

use dioxus::prelude::*;

use crate::pages::placeholder::ComingSoon;

pub use attendance::AdminAttendance;

macro_rules! placeholders {
    ($($name:ident => $title:literal),+ $(,)?) => {$(
        #[component]
        pub fn $name() -> Element {
            rsx! { ComingSoon { title: $title } }
        }
    )+};
}

placeholders! {
    AdminHome => "관리 홈",
    AdminMembers => "구성원 관리",
    AdminOrg => "조직·직책",
    AdminLeave => "휴가 관리",
    AdminApprovalForms => "결재 양식",
    AdminNotices => "공지 관리",
    AdminPolicy => "근무 정책",
    AdminAudit => "감사 로그",
}
