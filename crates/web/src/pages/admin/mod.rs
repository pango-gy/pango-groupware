//! 관리자 화면.

mod attendance;
mod audit;
mod forms;
mod leave;
pub(crate) mod members;
mod notices;
mod org;
mod policy;

use dioxus::prelude::*;

use crate::pages::placeholder::ComingSoon;

pub use attendance::AdminAttendance;
pub use audit::AdminAudit;
pub use forms::AdminApprovalForms;
pub use leave::AdminLeave;
pub use members::AdminMembers;
pub use notices::AdminNotices;
pub use org::AdminOrg;
pub use policy::AdminPolicy;

#[component]
pub fn AdminHome() -> Element {
    rsx! { ComingSoon { title: "관리 홈" } }
}
