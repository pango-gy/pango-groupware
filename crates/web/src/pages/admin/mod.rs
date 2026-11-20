//! 관리자 화면.

mod audit;
mod notices;
mod policy;

use dioxus::prelude::*;

use crate::pages::placeholder::ComingSoon;

pub(crate) mod members;
mod org;

pub use audit::AdminAudit;
pub use members::AdminMembers;
pub use notices::AdminNotices;
pub use org::AdminOrg;
pub use policy::AdminPolicy;

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
    AdminAttendance => "근태 관리",
    AdminLeave => "휴가 관리",
    AdminApprovalForms => "결재 양식",
}
