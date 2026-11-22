//! 사내 메신저 Buzz(Nostr 릴레이) DM 발송기.
//!
//! 봇 계정(실행 설정의 비밀키)으로 서명한 Nostr 이벤트를 릴레이의 HTTP API(`POST /events`)로
//! 보낸다. 요청마다 NIP-98 인증 머리글(`Authorization: Nostr <base64(kind 27235)>`, 태그 `u` =
//! `{릴레이}/events`, `method` = `POST`, `payload` = 본문 SHA-256)을 붙인다.
//!
//! 1. DM 열기: kind 41010, 내용 없음, 태그 `["p", 받는 사람]`. 이미 있으면 같은 DM을 돌려준다.
//!    응답 `message`의 `response:{"channel_id": …}`에서 채널 ID를 읽고, 발송 작업이 저장해 둔다.
//! 2. 메시지: kind 9, 내용 = 제목·짧은 본문·링크, 태그 `["h", 채널]`, `["ms", 초 아래 밀리초]`.
//!
//! 401·403(봇이 릴레이 구성원이 아님, 서명 오류)과 그 밖의 4xx는 바로 실패, 408·429·5xx·연결
//! 실패는 다시 보낸다. 저장해 둔 DM에 보내다 거절되면(DM이 사라짐 등) 한 번 다시 열어 본다.
//! 응답 본문은 작은 크기(64KiB)까지만 읽고 어디에도 남기지 않는다.

use pango_core::buzz::message_text;
use pango_core::outbound::{classify_status, ResponseClass};
use pango_core::time::Timestamp;
use serde_json::Value;

use super::guard::{transport_error, GuardedClient};
use super::{BuzzTarget, Delivery, OutboundSender, SendError, SendFuture, Sent, Target};
use crate::config::BuzzConfig;
use crate::error::ServiceResult;
use crate::nostr::{http_auth_header, BotKey, UnsignedEvent};

pub const KIND_DM_OPEN: u32 = 41010;
pub const KIND_MESSAGE: u32 = 9;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// 릴레이가 이해하지 못하는 응답.
pub const BAD_RESPONSE: &str = "buzz_bad_response";
/// 릴레이가 이벤트를 받지 않았다(`accepted: false`).
pub const REJECTED: &str = "buzz_rejected";

pub struct BuzzSender {
    http: GuardedClient,
    events_url: String,
    key: BotKey,
}

impl BuzzSender {
    pub fn new(config: &BuzzConfig, allow_private: bool) -> ServiceResult<Self> {
        Ok(Self {
            http: GuardedClient::new(allow_private)?,
            events_url: format!("{}/events", config.relay_url),
            key: config.bot_key.clone(),
        })
    }

    /// 서명한 이벤트를 보내고 응답 JSON을 돌려준다.
    async fn submit(&self, unsigned: UnsignedEvent) -> Result<Value, SendError> {
        self.http.check_literal_host(&self.events_url)?;
        let created_at = unsigned.created_at;
        let event = self
            .key
            .sign_random(unsigned)
            .map_err(|_| SendError::reject("sign_failed"))?;
        let body = serde_json::to_vec(&event).map_err(|_| SendError::reject("encode_failed"))?;
        let auth = http_auth_header(&self.key, &self.events_url, "POST", &body, created_at)
            .map_err(|_| SendError::reject("sign_failed"))?;
        let mut response = self
            .http
            .client
            .post(&self.events_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::AUTHORIZATION, auth)
            .body(body)
            .send()
            .await
            .map_err(|error| transport_error(&error))?;
        let status = response.status().as_u16();
        match classify_status(status) {
            ResponseClass::Delivered => {}
            ResponseClass::Retry => return Err(SendError::http(status, true)),
            ResponseClass::Reject => return Err(SendError::http(status, false)),
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| transport_error(&error))?
        {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > MAX_RESPONSE_BYTES {
                return Err(SendError::retry(BAD_RESPONSE));
            }
        }
        let json: Value =
            serde_json::from_slice(&bytes).map_err(|_| SendError::retry(BAD_RESPONSE))?;
        if json.get("accepted").and_then(Value::as_bool) != Some(true) {
            return Err(SendError::reject(REJECTED));
        }
        Ok(json)
    }

    /// 봇과 `pubkey`의 DM을 열고(있으면 그대로) 채널 ID를 돌려준다.
    pub async fn open_dm(&self, pubkey: &str, at: Timestamp) -> Result<String, SendError> {
        let response = self
            .submit(UnsignedEvent {
                created_at: seconds(at),
                kind: KIND_DM_OPEN,
                tags: vec![vec!["p".into(), pubkey.into()]],
                content: String::new(),
            })
            .await?;
        dm_channel_of(&response).ok_or_else(|| SendError::retry(BAD_RESPONSE))
    }

    async fn post_message(
        &self,
        channel: &str,
        content: &str,
        at: Timestamp,
    ) -> Result<(), SendError> {
        let millis = at.as_millis().rem_euclid(1000);
        self.submit(UnsignedEvent {
            created_at: seconds(at),
            kind: KIND_MESSAGE,
            tags: vec![
                vec!["h".into(), channel.into()],
                vec!["ms".into(), millis.to_string()],
            ],
            content: content.to_owned(),
        })
        .await
        .map(|_| ())
        .or_else(|error| {
            // 2xx로 받은 뒤 본문만 이해하지 못했다면 릴레이는 이미 메시지를 받았다. 다시 보내면
            // 서명 시각이 달라 다른 이벤트가 되므로 같은 DM이 또 간다.
            if error.reason == BAD_RESPONSE {
                Ok(())
            } else {
                Err(error)
            }
        })
    }

    /// DM으로 한 번 보낸다. 쓴 채널 ID를 돌려준다.
    pub async fn deliver(
        &self,
        target: &BuzzTarget,
        content: &str,
        at: Timestamp,
    ) -> Result<String, SendError> {
        if let Some(cached) = &target.dm_channel {
            match self.post_message(cached, content, at).await {
                Ok(()) => return Ok(cached.clone()),
                // 저장해 둔 DM이 더는 맞지 않으면(지워짐 등) 다시 열어 본다.
                Err(error) if !error.retry && error.status != Some(401) => {}
                Err(error) => return Err(error),
            }
        }
        let channel = self.open_dm(&target.pubkey, at).await?;
        self.post_message(&channel, content, at).await?;
        Ok(channel)
    }
}

fn seconds(at: Timestamp) -> i64 {
    at.as_millis().div_euclid(1000)
}

/// DM 열기 응답의 `message`(`response:{"channel_id":"…","created":…}`)에서 채널 ID를 읽는다.
pub fn dm_channel_of(response: &Value) -> Option<String> {
    let message = response.get("message")?.as_str()?;
    let json: Value = serde_json::from_str(message.strip_prefix("response:")?).ok()?;
    let channel = json.get("channel_id")?.as_str()?;
    // 채널 ID는 UUID다. 이상한 값은 저장하지 않는다.
    let valid = channel.len() == 36 && channel.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
    valid.then(|| channel.to_ascii_lowercase())
}

impl OutboundSender for BuzzSender {
    fn send<'a>(&'a self, delivery: &'a Delivery) -> SendFuture<'a> {
        Box::pin(async move {
            let Target::Buzz(target) = &delivery.target else {
                return Err(SendError::reject("wrong_channel"));
            };
            let payload = &delivery.payload;
            let content = message_text(&payload.title, &payload.body, payload.link.as_deref());
            let channel = self.deliver(target, &content, delivery.timestamp).await?;
            Ok(Sent {
                status: 200,
                dm_channel: Some(channel),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dm_channel_ids_are_read_from_the_relay_message() {
        let ok = serde_json::json!({
            "event_id": "e", "accepted": true,
            "message": "response:{\"channel_id\":\"0F8FAD5B-D9CB-469F-A165-70867728950E\",\"created\":true}"
        });
        assert_eq!(
            dm_channel_of(&ok).as_deref(),
            Some("0f8fad5b-d9cb-469f-a165-70867728950e")
        );
        for message in [
            "duplicate: already processed",
            "response:{\"created\":true}",
            "response:{\"channel_id\":\"../../etc\"}",
            "response:not json",
        ] {
            let value = serde_json::json!({ "accepted": true, "message": message });
            assert_eq!(dm_channel_of(&value), None, "{message}");
        }
    }
}
