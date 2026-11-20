//! 화면. 파일 하나가 화면 하나(또는 밀접한 한 묶음)다.

pub mod admin;
mod attendance;
mod auth;
mod leave;
mod leave_new;
mod more;
mod notices;
mod notifications;
mod people;
mod placeholder;
mod profile;
mod team;

pub use attendance::Attendance;
pub use auth::{ChangePassword, Login};
pub use leave::Leave;
pub use leave_new::LeaveNew;
pub use more::{More, NotFound};
pub use notices::{NoticeDetail, Notices};
pub use notifications::Notifications;
pub use people::People;
pub use placeholder::{ApprovalDetail, ApprovalList, ApprovalNew, Home};
pub use profile::Profile;
pub use team::Team;
