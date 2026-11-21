//! 범용 웹훅 발송기: 서명한 JSON을 POST한다.
//!
//! 요청 머리글:
//! - `Content-Type: application/json`
//! - `X-Pango-Event`: `notification` 또는 `test`
//! - `X-Pango-Delivery`: 발송 ID(다시 보내도 같다. 받는 쪽이 중복을 거른다)
//! - `X-Pango-Timestamp`: 서명한 시각(유닉스 초)
//! - `X-Pango-Signature`: `sha256=<hex>` = `"{timestamp}.{body}"`의 HMAC-SHA256(키 = 서명 키)
//!
//! 리다이렉트는 따라가지 않고(3xx는 실패), 응답 본문은 읽지 않는다(상태 코드만 본다). 연결할
//! 주소는 [`super::guard`]가 확인한다.

use pango_core::outbound::{classify_status, ResponseClass};

pub use super::guard::BLOCKED_ADDRESS;
use super::guard::{transport_error, GuardedClient};
use super::{Delivery, OutboundSender, SendError, SendFuture, Sent, Target};
use crate::error::ServiceResult;
use crate::security::webhook_signature;

pub struct WebhookSender {
    http: GuardedClient,
}

impl WebhookSender {
    /// `allow_private`: 루프백·사설망·CGNAT 주소로도 보낼지(서버 실행 옵션).
    pub fn new(allow_private: bool) -> ServiceResult<Self> {
        Ok(Self {
            http: GuardedClient::new(allow_private)?,
        })
    }
}

impl OutboundSender for WebhookSender {
    fn send<'a>(&'a self, delivery: &'a Delivery) -> SendFuture<'a> {
        Box::pin(async move {
            let Target::Webhook(target) = &delivery.target else {
                return Err(SendError::reject("wrong_channel"));
            };
            self.http.check_literal_host(&target.url)?;
            let body = serde_json::to_vec(&delivery.payload)
                .map_err(|_| SendError::reject("encode_failed"))?;
            let timestamp = delivery.timestamp.as_millis().div_euclid(1000);
            let signature = webhook_signature(&target.secret, timestamp, &body);
            let response = self
                .http
                .client
                .post(&target.url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header("X-Pango-Event", delivery.payload.event)
                .header("X-Pango-Delivery", &delivery.payload.delivery_id)
                .header("X-Pango-Timestamp", timestamp.to_string())
                .header("X-Pango-Signature", signature)
                .body(body)
                .send()
                .await
                .map_err(|error| transport_error(&error))?;
            let status = response.status().as_u16();
            // 본문은 읽지 않고 버린다.
            drop(response);
            match classify_status(status) {
                ResponseClass::Delivered => Ok(Sent::status(status)),
                ResponseClass::Retry => Err(SendError::http(status, true)),
                ResponseClass::Reject => Err(SendError::http(status, false)),
            }
        })
    }
}
