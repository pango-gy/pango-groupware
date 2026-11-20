use std::sync::Arc;

use pango_core::time::{Clock, Timestamp};
use pango_db::Db;

use crate::config::Config;
use crate::security::PasswordHasher;

/// 요청 처리에 필요한 공유 자원. 복제 비용이 작다.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub clock: Arc<dyn Clock>,
    pub config: Arc<Config>,
    pub passwords: PasswordHasher,
}

impl AppState {
    pub fn new(db: Db, clock: Arc<dyn Clock>, config: Config) -> crate::ServiceResult<Self> {
        let passwords = PasswordHasher::new(&config.password)?;
        Ok(Self {
            db,
            clock,
            config: Arc::new(config),
            passwords,
        })
    }

    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }
}
