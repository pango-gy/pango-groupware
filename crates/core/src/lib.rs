//! pango-groupware 도메인 타입과 순수 규칙.
//!
//! 이 크레이트는 IO를 하지 않는다. 현재 시각은 [`time::Clock`]으로 주입받는다.

pub mod approval;
pub mod attendance;
pub mod authz;
pub mod calendar;
pub mod error;
pub mod id;
pub mod leave;
pub mod macros;
pub mod member;
pub mod org;
pub mod password;
pub mod text;
pub mod time;

pub use error::{ConflictReason, DomainError, DomainResult, ValidationReason};
