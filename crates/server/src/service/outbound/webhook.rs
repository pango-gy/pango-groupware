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
//!
//! **연결할 주소 확인(SSRF 방어)**: 호스트 이름은 이 발송기의 DNS 해석기가 풀어
//! [`ip_allowed`]를 통과한 주소만 돌려주므로, 확인한 주소와 실제로 연결하는 주소가 같다(DNS
//! 재바인딩 방지). IP를 직접 쓴 주소는 보내기 전에 같은 규칙으로 확인한다. 시스템 프록시는
//! 쓰지 않는다(프록시가 이름을 풀면 확인을 건너뛰게 된다). 막힌 주소는 다시 보내지 않고
//! `blocked_address`로 끝내며, 해석한 IP는 어디에도 남기지 않는다.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use pango_core::outbound::{classify_status, ip_allowed, ResponseClass};

use super::{Delivery, OutboundSender, SendError, SendFuture};
use crate::error::{ServiceError, ServiceResult};
use crate::security::webhook_signature;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// 연결부터 응답 머리글을 받기까지 전체 제한 시간.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
/// 허용되지 않은 주소로 가려 했을 때의 사유.
pub const BLOCKED_ADDRESS: &str = "blocked_address";

/// 해석한 주소가 모두 막혔다.
#[derive(Debug)]
struct BlockedAddress;

impl std::fmt::Display for BlockedAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(BLOCKED_ADDRESS)
    }
}

impl std::error::Error for BlockedAddress {}

/// 허용된 주소만 돌려주는 DNS 해석기.
struct GuardedResolver {
    allow_private: bool,
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let allow_private = self.allow_private;
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let found = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let allowed: Vec<SocketAddr> = found
                .filter(|addr| ip_allowed(addr.ip(), allow_private))
                .collect();
            if allowed.is_empty() {
                return Err(Box::new(BlockedAddress) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(allowed.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

pub struct WebhookSender {
    client: reqwest::Client,
    allow_private: bool,
}

impl WebhookSender {
    /// `allow_private`: 루프백·사설망·CGNAT 주소로도 보낼지(서버 실행 옵션).
    pub fn new(allow_private: bool) -> ServiceResult<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .dns_resolver(Arc::new(GuardedResolver { allow_private }))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .user_agent(concat!("pango-groupware/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| ServiceError::internal("failed to build the webhook HTTP client"))?;
        Ok(Self {
            client,
            allow_private,
        })
    }

    /// IP를 직접 쓴 주소면 그 IP를 확인한다(해석기를 거치지 않으므로). 이름이면 해석기가 확인한다.
    fn check_literal_host(&self, raw: &str) -> Result<(), SendError> {
        let url = reqwest::Url::parse(raw).map_err(|_| SendError::reject("invalid_url"))?;
        let host = url
            .host_str()
            .ok_or_else(|| SendError::reject("invalid_url"))?;
        let literal = host.trim_start_matches('[').trim_end_matches(']');
        match literal.parse::<IpAddr>() {
            Ok(ip) if !ip_allowed(ip, self.allow_private) => {
                Err(SendError::reject(BLOCKED_ADDRESS))
            }
            _ => Ok(()),
        }
    }
}

fn caused_by_blocked_address(error: &reqwest::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if current.is::<BlockedAddress>() {
            return true;
        }
        source = current.source();
    }
    false
}

/// 전송 오류를 짧은 사유로 바꾼다. `reqwest`의 오류 문구에는 주소가 들어 있으므로(주소에 받는
/// 쪽의 토큰이 있을 수 있다) 문구를 그대로 쓰지 않는다.
fn transport_error(error: &reqwest::Error) -> SendError {
    if caused_by_blocked_address(error) {
        SendError::reject(BLOCKED_ADDRESS)
    } else if error.is_builder() {
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
            self.check_literal_host(&delivery.target.url)?;
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
