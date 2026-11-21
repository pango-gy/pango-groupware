use std::sync::{Arc, Mutex};

use pango_core::time::{Clock, Timestamp};
use pango_db::Db;

use crate::config::Config;
use crate::security::PasswordHasher;
use crate::service::outbound::webhook::WebhookSender;
use crate::service::outbound::OutboundSender;

/// 요청 처리에 필요한 공유 자원. 복제 비용이 작다.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub clock: Arc<dyn Clock>,
    pub config: Arc<Config>,
    pub passwords: PasswordHasher,
    /// 외부 알림 발송기. 테스트는 가짜로 바꾼다([`AppState::with_outbound_sender`]).
    pub outbound: Arc<dyn OutboundSender>,
    /// 마지막 웹훅 테스트 발송 시각(프로세스 안의 간단한 횟수 제한).
    pub webhook_test_at: Arc<Mutex<Option<Timestamp>>>,
}

impl AppState {
    pub fn new(db: Db, clock: Arc<dyn Clock>, config: Config) -> crate::ServiceResult<Self> {
        let passwords = PasswordHasher::new(&config.password)?;
        let outbound = Arc::new(WebhookSender::new(config.outbound_allow_private)?);
        Ok(Self {
            db,
            clock,
            config: Arc::new(config),
            passwords,
            outbound,
            webhook_test_at: Arc::new(Mutex::new(None)),
        })
    }

    #[must_use]
    pub fn with_outbound_sender(mut self, sender: Arc<dyn OutboundSender>) -> Self {
        self.outbound = sender;
        self
    }

    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }
}
