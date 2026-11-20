//! 서버와 웹 UI가 공유하는 요청/응답 타입.
//!
//! 여기 있는 타입이 곧 HTTP API의 모양이다. 필드를 바꾸면 API가 바뀐다.

pub mod admin;
pub mod approval;
pub mod attendance;
pub mod auth;
pub mod common;
pub mod home;
pub mod leave;
pub mod notice;
pub mod notification;
pub mod org;

pub use common::{Empty, ErrorBody, ErrorDetail, Page, PageQuery, Versioned};

/// API 경로 접두사.
pub const API_PREFIX: &str = "/api/v1";
