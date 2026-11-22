//! pango-groupware 서버.
//!
//! - [`service`]: 유스케이스. 권한 확인 → 규칙(core) → 저장(db) → 감사 로그·알림을
//!   한 트랜잭션으로 처리한다.
//! - [`http`]: 요청 추출과 응답 변환만 한다.

pub mod config;
pub mod error;
pub mod http;
pub mod mcp;
pub mod nostr;
pub mod security;
pub mod service;
pub mod state;
#[cfg(feature = "testing")]
pub mod testing;
pub mod totp;

pub use config::Config;
pub use error::{ServiceError, ServiceResult};
pub use state::AppState;
