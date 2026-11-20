//! 화면. 파일 하나가 화면 하나(또는 밀접한 한 묶음)다.

pub mod admin;
mod approval;
mod approval_detail;
mod approval_new;
mod attendance;
mod auth;
mod calendar;
mod calendar_form;
mod home;
mod leave;
mod leave_new;
mod more;
mod notices;
mod notifications;
mod people;
mod profile;
mod team;

pub use approval::ApprovalList;
pub use approval_detail::ApprovalDetail;
pub use approval_new::ApprovalNew;
pub use attendance::Attendance;
pub use auth::{ChangePassword, Login};
pub use calendar::{Calendar, CalendarEvent};
pub use home::Home;
pub use leave::Leave;
pub use leave_new::LeaveNew;
pub use more::{More, NotFound};
pub use notices::{NoticeDetail, Notices};
pub use notifications::Notifications;
pub use people::People;
pub use profile::Profile;
pub use team::Team;
