//! 화면. 파일 하나가 화면 하나(또는 밀접한 한 묶음)다.

pub mod admin;
mod auth;
mod more;
mod notices;
mod notifications;
mod people;
mod placeholder;
mod profile;

pub use auth::{ChangePassword, Login};
pub use more::{More, NotFound};
pub use notices::{NoticeDetail, Notices};
pub use notifications::Notifications;
pub use people::People;
pub use placeholder::{
    ApprovalDetail, ApprovalList, ApprovalNew, Attendance, Home, Leave, LeaveNew, Team,
};
pub use profile::Profile;
