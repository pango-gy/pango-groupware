//! 화면. 파일 하나가 화면 하나(또는 밀접한 한 묶음)다.

pub mod admin;
mod approval;
mod approval_detail;
mod approval_new;
mod auth;
mod more;
mod people;
mod placeholder;
mod profile;

pub use approval::ApprovalList;
pub use approval_detail::ApprovalDetail;
pub use approval_new::ApprovalNew;
pub use auth::{ChangePassword, Login};
pub use more::{More, NotFound};
pub use people::People;
pub use placeholder::{
    Attendance, Home, Leave, LeaveNew, NoticeDetail, Notices, Notifications, Team,
};
pub use profile::Profile;
