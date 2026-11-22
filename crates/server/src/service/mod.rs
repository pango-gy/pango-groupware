//! 유스케이스 계층.
//!
//! 규칙:
//! - 쓰기 유스케이스는 [`AppState::write`](crate::AppState) 한 번 = 트랜잭션 하나.
//! - 첫 줄에서 권한을 확인한다(`pango_core::authz`).
//! - 계정·권한·기록을 바꾸면 같은 트랜잭션에서 [`support::audit`]을 남긴다.
//! - 다른 유스케이스와 한 트랜잭션으로 묶여야 하는 로직은 `*_tx(conn, …)` 함수로 둔다.

pub mod approval;
pub mod attachments;
pub mod attendance;
pub mod audit_log;
pub mod auth;
pub mod buzz;
pub mod calendar;
pub mod form_catalog;
pub mod home;
pub mod integrations;
pub mod leave;
pub mod maintenance;
pub mod members;
pub mod notices;
pub mod notifications;
pub mod org;
pub mod outbound;
pub mod reminders;
pub mod retention;
pub mod settings;
pub mod setup;
pub mod support;
pub mod totp;

pub use support::{AuthContext, RequestMeta};
