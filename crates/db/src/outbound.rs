//! 외부 알림 채널: 연동 설정(단일 행), 구성원별 수신 여부, 보낼 일(아웃박스).

use pango_core::id::{MemberId, NotificationId, OutboundMessageId};
use pango_core::member::MemberStatus;
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

/// 연동 설정. 서명 키는 `Debug`로도 드러나지 않는다.
#[derive(Clone, PartialEq, Eq)]
pub struct OutboundSettings {
    pub webhook_enabled: bool,
    pub webhook_url: Option<String>,
    pub webhook_secret: Option<String>,
    pub version: i64,
    pub updated_at: Timestamp,
}

impl std::fmt::Debug for OutboundSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutboundSettings")
            .field("webhook_enabled", &self.webhook_enabled)
            .field("webhook_url", &self.webhook_url.as_ref().map(|_| "***"))
            .field("has_secret", &self.webhook_secret.is_some())
            .field("version", &self.version)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(sqlx::FromRow)]
struct SettingsRow {
    webhook_enabled: bool,
    webhook_url: Option<String>,
    webhook_secret: Option<String>,
    version: i64,
    updated_at: i64,
}

/// 연동 설정. 마이그레이션이 행을 만들어 두므로 항상 있다.
pub async fn settings(conn: &mut Conn) -> DbResult<OutboundSettings> {
    let row = sqlx::query_as::<_, SettingsRow>(
        "SELECT webhook_enabled, webhook_url, webhook_secret, version, updated_at
         FROM outbound_settings WHERE id = 1",
    )
    .fetch_one(conn)
    .await?;
    Ok(OutboundSettings {
        webhook_enabled: row.webhook_enabled,
        webhook_url: row.webhook_url,
        webhook_secret: row.webhook_secret,
        version: row.version,
        updated_at: codec::timestamp("updated_at", row.updated_at)?,
    })
}

/// 켜기·주소를 바꾼다. `version`이 현재 값과 같을 때만 바꾸고 버전을 올린다. 바꿨으면 `true`.
pub async fn update_webhook(
    conn: &mut Conn,
    enabled: bool,
    url: Option<&str>,
    version: i64,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE outbound_settings
         SET webhook_enabled = ?, webhook_url = ?, updated_at = ?, version = version + 1
         WHERE id = 1 AND version = ?",
    )
    .bind(enabled)
    .bind(url)
    .bind(now.as_millis())
    .bind(version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 서명 키를 바꾸고 버전을 올린다.
pub async fn set_webhook_secret(conn: &mut Conn, secret: &str, now: Timestamp) -> DbResult<()> {
    sqlx::query(
        "UPDATE outbound_settings
         SET webhook_secret = ?, updated_at = ?, version = version + 1 WHERE id = 1",
    )
    .bind(secret)
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

/// 구성원이 외부 알림을 받는지. 정한 적이 없으면 받는다.
pub async fn external_enabled(conn: &mut Conn, member_id: MemberId) -> DbResult<bool> {
    let stored: Option<bool> = sqlx::query_scalar(
        "SELECT external_enabled FROM member_notification_prefs WHERE member_id = ?",
    )
    .bind(member_id.to_string())
    .fetch_optional(conn)
    .await?;
    Ok(stored.unwrap_or(true))
}

pub async fn set_external_enabled(
    conn: &mut Conn,
    member_id: MemberId,
    enabled: bool,
    now: Timestamp,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO member_notification_prefs (member_id, external_enabled, updated_at)
         VALUES (?, ?, ?)
         ON CONFLICT (member_id)
         DO UPDATE SET external_enabled = excluded.external_enabled,
                       updated_at = excluded.updated_at",
    )
    .bind(member_id.to_string())
    .bind(enabled)
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

/// 채널: 범용 웹훅과 사내 메신저 Buzz DM.
pub const CHANNEL_WEBHOOK: &str = "webhook";
pub const CHANNEL_BUZZ: &str = "buzz";

/// 보낼 일을 남긴다. 같은 알림·채널의 줄이 이미 있으면 아무 일도 하지 않는다. 넣었으면 `true`.
pub async fn enqueue(
    conn: &mut Conn,
    id: OutboundMessageId,
    notification_id: NotificationId,
    member_id: MemberId,
    channel: &str,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "INSERT INTO outbound_messages
            (id, notification_id, member_id, channel, status, attempts, next_attempt_at,
             created_at)
         VALUES (?, ?, ?, ?, 'pending', 0, ?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(id.to_string())
    .bind(notification_id.to_string())
    .bind(member_id.to_string())
    .bind(channel)
    .bind(now.as_millis())
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 보낼 차례가 된 메시지와 보내는 데 필요한 값(알림 내용, 받는 사람).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueMessage {
    pub id: OutboundMessageId,
    pub channel: String,
    pub member_id: MemberId,
    /// 이번 시도를 포함하지 않은 지금까지의 시도 수.
    pub attempts: u32,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub link: Option<String>,
    pub notified_at: Timestamp,
    pub login_id: String,
    pub name: String,
    pub email: Option<String>,
    pub member_status: MemberStatus,
    pub external_enabled: bool,
    /// Buzz 채널: 확인을 마친 계정의 공개키(16진수)와 캐시한 DM 채널 ID.
    pub buzz_pubkey: Option<String>,
    pub buzz_dm_channel: Option<String>,
}

#[derive(sqlx::FromRow)]
struct DueRow {
    id: String,
    channel: String,
    member_id: String,
    buzz_pubkey: Option<String>,
    buzz_dm_channel: Option<String>,
    attempts: i64,
    kind: String,
    title: String,
    body: String,
    link: Option<String>,
    notified_at: i64,
    login_id: String,
    name: String,
    email: Option<String>,
    member_status: String,
    external_enabled: bool,
}

/// `pending`이고 `next_attempt_at <= now`이며 시도가 `max_attempts`보다 적은 메시지(오래된 순).
pub async fn due(
    conn: &mut Conn,
    now: Timestamp,
    max_attempts: u32,
    limit: u32,
) -> DbResult<Vec<DueMessage>> {
    let rows = sqlx::query_as::<_, DueRow>(
        "SELECT o.id, o.channel, o.member_id, b.pubkey AS buzz_pubkey,
                b.dm_channel_id AS buzz_dm_channel, o.attempts, n.kind, n.title, n.body, n.link, n.created_at AS notified_at,
                m.login_id, m.name, m.email, m.status AS member_status,
                COALESCE(p.external_enabled, 1) AS external_enabled
         FROM outbound_messages o
         JOIN notifications n ON n.id = o.notification_id
         JOIN members m ON m.id = o.member_id
         LEFT JOIN member_notification_prefs p ON p.member_id = o.member_id
         LEFT JOIN buzz_accounts b ON b.member_id = o.member_id AND b.status = 'verified'
         WHERE o.status = 'pending' AND o.next_attempt_at <= ? AND o.attempts < ?
         ORDER BY o.next_attempt_at, o.id
         LIMIT ?",
    )
    .bind(now.as_millis())
    .bind(i64::from(max_attempts))
    .bind(i64::from(limit))
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(DueMessage {
                id: codec::parse("id", &row.id)?,
                channel: row.channel,
                member_id: codec::parse("member_id", &row.member_id)?,
                buzz_pubkey: row.buzz_pubkey,
                buzz_dm_channel: row.buzz_dm_channel,
                attempts: codec::narrow("attempts", row.attempts)?,
                kind: row.kind,
                title: row.title,
                body: row.body,
                link: row.link,
                notified_at: codec::timestamp("notified_at", row.notified_at)?,
                login_id: row.login_id,
                name: row.name,
                email: row.email,
                member_status: codec::parse_with(
                    "status",
                    &row.member_status,
                    MemberStatus::parse,
                )?,
                external_enabled: row.external_enabled,
            })
        })
        .collect()
}

/// 보내기 직전에 메시지를 집는다: 시도 수를 올리고, 결과를 남기지 못한 채 멈춰도 `retry_at`
/// 전에는 다시 집히지 않게 미룬다. 아직 `pending`이었으면 `true`.
pub async fn claim(
    conn: &mut Conn,
    id: OutboundMessageId,
    now: Timestamp,
    retry_at: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE outbound_messages
         SET attempts = attempts + 1, last_attempt_at = ?, next_attempt_at = ?
         WHERE id = ? AND status = 'pending'",
    )
    .bind(now.as_millis())
    .bind(retry_at.as_millis())
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn mark_sent(conn: &mut Conn, id: OutboundMessageId, now: Timestamp) -> DbResult<()> {
    sqlx::query(
        "UPDATE outbound_messages SET status = 'sent', sent_at = ?, last_error = NULL
         WHERE id = ? AND status = 'pending'",
    )
    .bind(now.as_millis())
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// 다시 보낼 시각과 실패 사유를 남긴다.
pub async fn mark_retry(
    conn: &mut Conn,
    id: OutboundMessageId,
    retry_at: Timestamp,
    error: &str,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE outbound_messages SET next_attempt_at = ?, last_error = ?
         WHERE id = ? AND status = 'pending'",
    )
    .bind(retry_at.as_millis())
    .bind(error)
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn mark_failed(conn: &mut Conn, id: OutboundMessageId, error: &str) -> DbResult<()> {
    sqlx::query(
        "UPDATE outbound_messages SET status = 'failed', last_error = ?
         WHERE id = ? AND status = 'pending'",
    )
    .bind(error)
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// 마지막 시도의 결과를 남기지 못한 채(서버 중단) 시도를 다 쓴 메시지를 실패로 끝낸다.
pub async fn fail_exhausted(
    conn: &mut Conn,
    now: Timestamp,
    max_attempts: u32,
    error: &str,
) -> DbResult<u64> {
    let result = sqlx::query(
        "UPDATE outbound_messages SET status = 'failed', last_error = COALESCE(last_error, ?)
         WHERE status = 'pending' AND attempts >= ? AND next_attempt_at <= ?",
    )
    .bind(error)
    .bind(i64::from(max_attempts))
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(result.rows_affected())
}

/// 발송 현황.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryStats {
    /// 아직 보내지 않았거나 다시 보낼 메시지.
    pub pending: i64,
    /// `since` 뒤에 시도해 보낸 메시지.
    pub sent: i64,
    /// `since` 뒤에 시도해 실패로 끝난 메시지.
    pub failed: i64,
    /// 가장 최근 시도의 실패 사유와 그 시각.
    pub last_error: Option<(String, Timestamp)>,
}

/// 한 채널의 발송 현황.
pub async fn stats(conn: &mut Conn, channel: &str, since: Timestamp) -> DbResult<DeliveryStats> {
    let (pending, sent, failed): (i64, i64, i64) = sqlx::query_as(
        "SELECT
            COALESCE(SUM(status = 'pending'), 0),
            COALESCE(SUM(status = 'sent' AND last_attempt_at >= ?1), 0),
            COALESCE(SUM(status = 'failed' AND last_attempt_at >= ?1), 0)
         FROM outbound_messages WHERE channel = ?2",
    )
    .bind(since.as_millis())
    .bind(channel)
    .fetch_one(&mut *conn)
    .await?;
    let last: Option<(String, i64)> = sqlx::query_as(
        "SELECT last_error, last_attempt_at FROM outbound_messages
         WHERE channel = ? AND last_error IS NOT NULL AND last_attempt_at IS NOT NULL
         ORDER BY last_attempt_at DESC LIMIT 1",
    )
    .bind(channel)
    .fetch_optional(conn)
    .await?;
    let last_error = match last {
        Some((error, at)) => Some((error, codec::timestamp("last_attempt_at", at)?)),
        None => None,
    };
    Ok(DeliveryStats {
        pending,
        sent,
        failed,
        last_error,
    })
}

/// `before`보다 먼저 만든, 끝난(`sent`·`failed`) 메시지를 지우고 그 수를 돌려준다.
pub async fn purge(conn: &mut Conn, before: Timestamp) -> DbResult<u64> {
    let result = sqlx::query(
        "DELETE FROM outbound_messages WHERE status IN ('sent', 'failed') AND created_at < ?",
    )
    .bind(before.as_millis())
    .execute(conn)
    .await?;
    Ok(result.rows_affected())
}
