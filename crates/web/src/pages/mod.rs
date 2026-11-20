//! 화면. 파일 하나가 화면 하나(또는 밀접한 한 묶음)다.

pub mod admin;
mod attendance;
mod auth;
mod more;
mod people;
mod placeholder;
mod profile;
mod team;

pub use attendance::Attendance;
pub use auth::{ChangePassword, Login};
pub use more::{More, NotFound};
pub use people::People;
pub use placeholder::{
    ApprovalDetail, ApprovalList, ApprovalNew, Home, Leave, LeaveNew, NoticeDetail, Notices,
    Notifications,
};
pub use profile::Profile;
pub use team::Team;
