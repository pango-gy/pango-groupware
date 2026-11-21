//! 외부 알림 채널: 앱 내 알림을 외부 메신저로도 보낸다(아웃박스 + 발송 작업).
//!
//! 1. [`enqueue_tx`]: 알림을 만드는 트랜잭션 안에서 보낼 일을 한 줄 남긴다(`support::notify`).
//! 2. [`dispatch_once`]: 차례가 된 줄을 집어(시도 수를 올리고 다음 재시도 시각으로 미룬 뒤)
//!    트랜잭션 **밖에서** 보내고, 결과를 짧은 트랜잭션으로 남긴다.
//!
//! 보내는 방법은 [`OutboundSender`]로 갈아 끼운다. 지금은 서명한 JSON을 POST하는 범용 웹훅
//! ([`webhook::WebhookSender`])뿐이고, 사내 메신저 전용 발송기도 같은 트레이트로 붙인다.

pub mod webhook;

use std::future::Future;
use std::pin::Pin;

use pango_core::id::{MemberId, NotificationId, OutboundMessageId};
use pango_core::member::MemberStatus;
use pango_core::outbound::{backoff, next_retry, MAX_ATTEMPTS};
use pango_core::time::Timestamp;
use pango_db::outbound::{self, DueMessage, OutboundSettings};
use pango_db::{members, Conn};
use serde::Serialize;

use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 받는 쪽에 보내는 본문. 앱 알림함에 이미 보이는 내용(종류·제목·짧은 본문·링크)과 받는 사람을
/// 메신저에서 찾는 데 필요한 값(아이디·이름·이메일)만 담는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutboundPayload {
    /// `notification`(앱 알림) 또는 `test`(관리자의 테스트 발송).
    pub event: &'static str,
    /// 발송 ID. 같은 메시지를 다시 보내도 같으므로 받는 쪽이 중복을 거를 수 있다.
    pub delivery_id: String,
    /// 알림 종류(`approval.requested` 등). 테스트는 `test`.
    pub kind: String,
    pub title: String,
    pub body: String,
    /// 공개 주소(`--public-origin`)가 있으면 절대 주소, 없으면 앱 안의 경로.
    pub link: Option<String>,
    pub recipient: Recipient,
    /// 알림을 만든 시각(밀리초).
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Recipient {
    pub login_id: String,
    pub name: String,
    pub email: Option<String>,
}

/// 보낼 곳. 주소와 서명 키는 `Debug`로 드러나지 않는다.
#[derive(Clone, PartialEq, Eq)]
pub struct WebhookTarget {
    pub url: String,
    pub secret: String,
}

impl std::fmt::Debug for WebhookTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WebhookTarget(***)")
    }
}

/// 한 번의 발송.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    pub target: WebhookTarget,
    /// 서명에 넣는 시각.
    pub timestamp: Timestamp,
    pub payload: OutboundPayload,
}

/// 보내지 못했다. `reason`은 짧은 코드(`http_503`, `timeout` 등)이고 주소·응답 본문·비밀 값을
/// 담지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendError {
    /// 다시 보내 볼 만한지(연결 실패, 408·429·5xx). 아니면 바로 실패로 끝낸다.
    pub retry: bool,
    /// 받는 쪽의 응답 상태. 연결하지 못했으면 `None`.
    pub status: Option<u16>,
    pub reason: String,
}

impl SendError {
    pub fn retry(reason: impl Into<String>) -> Self {
        Self {
            retry: true,
            status: None,
            reason: reason.into(),
        }
    }

    pub fn reject(reason: impl Into<String>) -> Self {
        Self {
            retry: false,
            status: None,
            reason: reason.into(),
        }
    }

    /// 응답 상태로 만든다. `retry`는 [`pango_core::outbound::classify_status`]가 정한다.
    pub fn http(status: u16, retry: bool) -> Self {
        Self {
            retry,
            status: Some(status),
            reason: format!("http_{status}"),
        }
    }
}

/// 성공하면 받는 쪽의 응답 상태.
pub type SendFuture<'a> = Pin<Box<dyn Future<Output = Result<u16, SendError>> + Send + 'a>>;

/// 외부로 보내는 방법. 테스트는 가짜를 넣는다([`AppState::with_outbound_sender`]).
pub trait OutboundSender: Send + Sync {
    fn send<'a>(&'a self, delivery: &'a Delivery) -> SendFuture<'a>;
}

/// 한 번에 집어 보내는 최대 수. 동시에 보내므로, 한 번의 시도가 끝나는 데 걸리는 시간은
/// 발송기의 제한 시간(10초)을 넘지 않는다. 그래서 집을 때 미뤄 둔 첫 재시도 간격(30초) 안에
/// 결과가 남는다.
const BATCH: u32 = 20;

/// 결과를 남기지 못한 채 시도를 다 쓴 메시지의 사유.
const INTERRUPTED: &str = "interrupted";

/// 연동 설정에서 보낼 곳을 만든다. 꺼져 있거나 주소·서명 키가 없으면 `None`.
pub fn target_of(settings: &OutboundSettings) -> Option<WebhookTarget> {
    if !settings.webhook_enabled {
        return None;
    }
    Some(WebhookTarget {
        url: settings.webhook_url.clone()?,
        secret: settings.webhook_secret.clone()?,
    })
}

/// 앱 안의 경로를 공개 주소 기준 절대 주소로 바꾼다. 공개 주소가 없으면 그대로 둔다.
pub fn absolute_link(public_origin: Option<&str>, link: Option<&str>) -> Option<String> {
    let link = link?;
    match public_origin {
        Some(origin) if link.starts_with('/') => {
            Some(format!("{}{link}", origin.trim_end_matches('/')))
        }
        _ => Some(link.to_owned()),
    }
}

/// 새로 만든 알림을 외부로도 보낼 일로 남긴다(같은 트랜잭션). 연동이 꺼져 있거나, 받는 사람이
/// 재직 중이 아니거나 외부 알림을 끄면 아무것도 하지 않는다. 같은 알림은 한 번만 남는다.
pub async fn enqueue_tx(
    conn: &mut Conn,
    now: Timestamp,
    notification_id: NotificationId,
    member_id: MemberId,
) -> ServiceResult<()> {
    if !outbound::settings(conn).await?.webhook_enabled {
        return Ok(());
    }
    let Some(member) = members::find(conn, member_id).await? else {
        return Ok(());
    };
    if member.status != MemberStatus::Active || !outbound::external_enabled(conn, member_id).await?
    {
        return Ok(());
    }
    outbound::enqueue(
        conn,
        OutboundMessageId::new(),
        notification_id,
        member_id,
        now,
    )
    .await?;
    Ok(())
}

/// 한 번 돌린 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DispatchReport {
    pub sent: u32,
    /// 실패했지만 다시 보낼 메시지.
    pub retrying: u32,
    /// 실패로 끝난 메시지(보내지 않고 끝낸 것 포함).
    pub failed: u32,
}

/// 보내지 않고 끝낼 사유. 집은 뒤에 연동이 꺼졌거나 받는 사람의 상태가 바뀐 경우다.
fn skip_reason(message: &DueMessage, target: Option<&WebhookTarget>) -> Option<&'static str> {
    if target.is_none() {
        Some("integration_disabled")
    } else if message.member_status != MemberStatus::Active {
        Some("recipient_inactive")
    } else if !message.external_enabled {
        Some("recipient_opted_out")
    } else {
        None
    }
}

fn payload_of(state: &AppState, message: &DueMessage) -> OutboundPayload {
    OutboundPayload {
        event: "notification",
        delivery_id: message.id.to_string(),
        kind: message.kind.clone(),
        title: message.title.clone(),
        body: message.body.clone(),
        link: absolute_link(
            state.config.public_origin.as_deref(),
            message.link.as_deref(),
        ),
        recipient: Recipient {
            login_id: message.login_id.clone(),
            name: message.name.clone(),
            email: message.email.clone(),
        },
        created_at: message.notified_at,
    }
}

fn at(now: Timestamp, after: chrono::Duration) -> ServiceResult<Timestamp> {
    now.checked_add(after)
        .ok_or_else(|| ServiceError::internal("retry time overflow"))
}

/// 차례가 된 메시지를 한 번 보낸다. 여러 번(동시에) 실행해도 같은 메시지를 두 번 집지 않는다:
/// 집기는 쓰기 트랜잭션(직렬화됨) 안에서 `next_attempt_at`을 미루기 때문이다.
pub async fn dispatch_once(state: &AppState) -> ServiceResult<DispatchReport> {
    let now = state.now();
    let mut report = DispatchReport::default();
    let claimed: Vec<(OutboundMessageId, u32, Delivery)> = state
        .write(async |conn: &mut Conn| {
            let exhausted = outbound::fail_exhausted(conn, now, MAX_ATTEMPTS, INTERRUPTED).await?;
            report.failed += u32::try_from(exhausted).unwrap_or(u32::MAX);
            let due = outbound::due(conn, now, MAX_ATTEMPTS, BATCH).await?;
            if due.is_empty() {
                return Ok(Vec::new());
            }
            let target = target_of(&outbound::settings(conn).await?);
            let mut claimed = Vec::with_capacity(due.len());
            for message in due {
                let target = match (skip_reason(&message, target.as_ref()), &target) {
                    (None, Some(target)) => target.clone(),
                    (reason, _) => {
                        let reason = reason.unwrap_or("integration_disabled");
                        outbound::mark_failed(conn, message.id, reason).await?;
                        report.failed += 1;
                        continue;
                    }
                };
                let attempt = message.attempts + 1;
                // 결과를 남기기 전에 멈추면 이 시각에 다시 집힌다(그 전에는 집히지 않는다).
                let lease = at(now, backoff(attempt))?;
                if !outbound::claim(conn, message.id, now, lease).await? {
                    continue;
                }
                let delivery = Delivery {
                    target,
                    timestamp: now,
                    payload: payload_of(state, &message),
                };
                claimed.push((message.id, attempt, delivery));
            }
            Ok(claimed)
        })
        .await?;
    if claimed.is_empty() {
        return Ok(report);
    }

    let mut sends = tokio::task::JoinSet::new();
    for (id, attempt, delivery) in claimed {
        let sender = state.outbound.clone();
        sends.spawn(async move {
            let result = sender.send(&delivery).await;
            (id, attempt, result)
        });
    }
    let mut results = Vec::new();
    while let Some(joined) = sends.join_next().await {
        // 발송기가 패닉하면 그 메시지는 집을 때 미뤄 둔 시각에 다시 시도된다.
        match joined {
            Ok(result) => results.push(result),
            Err(error) => tracing::warn!(%error, "outbound send task failed"),
        }
    }

    let done = state.now();
    state
        .write(async |conn: &mut Conn| {
            for (id, attempt, result) in &results {
                match result {
                    Ok(_) => {
                        outbound::mark_sent(conn, *id, done).await?;
                        report.sent += 1;
                    }
                    Err(error) => match next_retry(*attempt).filter(|_| error.retry) {
                        Some(delay) => {
                            outbound::mark_retry(conn, *id, at(done, delay)?, &error.reason)
                                .await?;
                            report.retrying += 1;
                        }
                        None => {
                            outbound::mark_failed(conn, *id, &error.reason).await?;
                            report.failed += 1;
                        }
                    },
                }
                if let Err(error) = result {
                    tracing::info!(delivery = %id, attempt, reason = %error.reason, "outbound send failed");
                }
            }
            Ok(())
        })
        .await?;
    Ok(report)
}

/// 서버가 떠 있는 동안 주기적으로 보낸다. 실패해도 서버는 계속 돈다.
pub async fn run_forever(state: AppState, every: std::time::Duration) {
    let mut ticker = tokio::time::interval(every);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match dispatch_once(&state).await {
            Ok(report) if report != DispatchReport::default() => {
                tracing::debug!(?report, "outbound dispatch done");
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "outbound dispatch failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_become_absolute_only_with_a_public_origin() {
        let link = Some("/approval/documents/1");
        assert_eq!(
            absolute_link(Some("https://gw.example.com/"), link).as_deref(),
            Some("https://gw.example.com/approval/documents/1")
        );
        assert_eq!(
            absolute_link(Some("https://gw.example.com"), link).as_deref(),
            Some("https://gw.example.com/approval/documents/1")
        );
        assert_eq!(
            absolute_link(None, link).as_deref(),
            Some("/approval/documents/1")
        );
        assert_eq!(absolute_link(Some("https://gw.example.com"), None), None);
    }

    #[test]
    fn target_needs_enabled_url_and_secret() {
        let settings = OutboundSettings {
            webhook_enabled: true,
            webhook_url: Some("https://hooks.example.com".into()),
            webhook_secret: Some("s".into()),
            version: 1,
            updated_at: Timestamp::from_millis(0).expect("ts"),
        };
        assert!(target_of(&settings).is_some());
        let off = OutboundSettings {
            webhook_enabled: false,
            ..settings.clone()
        };
        assert!(target_of(&off).is_none());
        let no_secret = OutboundSettings {
            webhook_secret: None,
            ..settings
        };
        assert!(target_of(&no_secret).is_none());
        assert_eq!(
            format!(
                "{:?}",
                WebhookTarget {
                    url: "u".into(),
                    secret: "s".into()
                }
            ),
            "WebhookTarget(***)"
        );
    }
}
