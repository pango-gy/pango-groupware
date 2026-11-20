//! SQLite 스키마와 저장소.
//!
//! 이 크레이트는 SQL만 담당한다. 비즈니스 규칙은 `pango-core`, 유스케이스는 서버의
//! 서비스 계층에 있다. 저장소 함수는 연결(`&mut Conn`)을 받으므로, 호출하는 쪽이
//! 트랜잭션 경계를 정한다.

pub mod approval;
pub mod attendance;
pub mod attendance_corrections;
pub mod audit;
pub mod codec;
pub mod error;
pub mod idempotency;
pub mod leave;
pub mod members;
pub mod notices;
pub mod notifications;
pub mod org;
pub mod pool;
pub mod sessions;
pub mod settings;
#[cfg(feature = "testing")]
pub mod testing;

pub use error::{DbError, DbResult};
pub use pool::{Conn, Db, DbOptions, ReadConn, ReadSnapshot, WriteTx};
