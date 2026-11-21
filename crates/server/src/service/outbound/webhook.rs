//! 범용 웹훅 발송기: 서명한 JSON을 POST한다.
//!
//! 요청 머리글:
//! - `Content-Type: application/json`
//! - `X-Pango-Event`: `notification` 또는 `test`
//! - `X-Pango-Delivery`: 발송 ID(다시 보내도 같다. 받는 쪽이 중복을 거른다)
//! - `X-Pango-Timestamp`: 서명한 시각(유닉스 초)
//! - `X-Pango-Signature`: `sha256=<hex>` = `"{timestamp}.{body}"`의 HMAC-SHA256(키 = 서명 키)
//!
//! 리다이렉트는 따라가지 않고(3xx는 실패), 응답 본문은 읽지 않는다(상태 코드만 본다).

use std::time::Duration;

use pango_core::outbound::{classify_status, ResponseClass};

use super::{Delivery, OutboundSender, SendError, SendFuture};
use crate::error::{ServiceError, ServiceResult};
use crate::security::webhook_signature;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// 연결부터 응답 머리글을 받기까지 전체 제한 시간.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);

pub struct WebhookSender {
    client: reqwest::Client,
}

impl WebhookSender {
    pub fn new() -> ServiceResult<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .user_agent(concat!("pango-groupware/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ServiceError::internal("failed to build the webhook HTTP client"))?;
        Ok(Self { client })
    }
}

/// 전송 오류를 짧은 사유로 바꾼다. `reqwest`의 오류 문구에는 주소가 들어 있으므로(주소에 받는
/// 쪽의 토큰이 있을 수 있다) 문구를 그대로 쓰지 않는다.
fn transport_error(error: &reqwest::Error) -> SendError {
    if error.is_builder() {
        SendError::reject("invalid_url")
    } else if error.is_timeout() {
        SendError::retry("timeout")
    } else if error.is_connect() {
        SendError::retry("connect_failed")
    } else {
        SendError::retry("request_failed")
    }
}

impl OutboundSender for WebhookSender {
    fn send<'a>(&'a self, delivery: &'a Delivery) -> SendFuture<'a> {
        Box::pin(async move {
            let body = serde_json::to_vec(&delivery.payload)
                .map_err(|_| SendError::reject("encode_failed"))?;
            let timestamp = delivery.timestamp.as_millis().div_euclid(1000);
            let signature = webhook_signature(&delivery.target.secret, timestamp, &body);
            let response = self
                .client
                .post(&delivery.target.url)
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
                ResponseClass::Delivered => Ok(status),
                ResponseClass::Retry => Err(SendError::http(status, true)),
                ResponseClass::Reject => Err(SendError::http(status, false)),
            }
        })
    }
}
