//! 외부 알림 연동 설정(관리자)과 구성원의 수신 설정.

use chrono::Duration;
use pango_api::integration::{
    DeliveryStatsDto, NotificationPrefsDto, NotificationPrefsInput, WebhookSecretDto,
    WebhookSettingsDto, WebhookSettingsInput, WebhookTestResult,
};
use pango_api::{Empty, Versioned};
use pango_core::authz;
use pango_core::error::ValidationReason;
use pango_core::id::OutboundMessageId;
use pango_core::outbound::WebhookUrl;
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::outbound::{self, OutboundSettings};
use pango_db::Conn;

use super::outbound::{Delivery, OutboundPayload, Recipient, Target, WebhookTarget};
use super::support::{
    audit, count_u32, current_member, require_current_admin, require_updated, Audit, AuthContext,
    Changes,
};
use crate::error::{ServiceError, ServiceResult};
use crate::security::new_webhook_secret;
use crate::state::AppState;

/// 테스트 발송 사이의 최소 간격.
pub const TEST_COOLDOWN: Duration = Duration::seconds(10);
/// 발송 현황을 세는 기간.
const STATS_WINDOW: Duration = Duration::hours(24);

/// 한 채널의 최근 발송 현황(24시간).
pub(crate) async fn stats_dto(
    conn: &mut Conn,
    channel: &str,
    now: Timestamp,
) -> ServiceResult<DeliveryStatsDto> {
    let since = now
        .checked_sub(STATS_WINDOW)
        .ok_or_else(|| ServiceError::internal("stats window underflow"))?;
    let stats = outbound::stats(conn, channel, since).await?;
    let (last_error, last_error_at) = stats.last_error.unzip();
    Ok(DeliveryStatsDto {
        pending: count_u32(stats.pending)?,
        sent_24h: count_u32(stats.sent)?,
        failed_24h: count_u32(stats.failed)?,
        last_error,
        last_error_at,
    })
}

async fn dto(conn: &mut Conn, now: Timestamp) -> ServiceResult<WebhookSettingsDto> {
    let settings = outbound::settings(conn).await?;
    Ok(WebhookSettingsDto {
        enabled: settings.webhook_enabled,
        url: settings.webhook_url,
        has_secret: settings.webhook_secret.is_some(),
        version: settings.version,
        updated_at: settings.updated_at,
        stats: stats_dto(conn, outbound::CHANNEL_WEBHOOK, now).await?,
    })
}

pub async fn get_webhook(state: &AppState, ctx: &AuthContext) -> ServiceResult<WebhookSettingsDto> {
    authz::require_admin(&ctx.actor)?;
    dto(&mut *state.read().await?, state.now()).await
}

/// 감사 로그에는 주소의 출처(`https://host`)만 남긴다. 경로·쿼리에 받는 쪽의 토큰이 있을 수 있다.
fn origin_of(url: Option<&str>) -> Option<String> {
    url.and_then(|raw| WebhookUrl::parse(raw).ok())
        .map(|url| url.origin())
}

/// 켜기·주소를 바꾼다. 주소는 신뢰 경계인 여기서 검증한다([`WebhookUrl::parse`]). 켜려면 주소와
/// 서명 키가 있어야 한다. 버전이 낡았으면 409.
pub async fn update_webhook(
    state: &AppState,
    ctx: &AuthContext,
    request: &Versioned<WebhookSettingsInput>,
) -> ServiceResult<WebhookSettingsDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let input = &request.data;
    let url = match input.url.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(raw) => Some(WebhookUrl::parse(raw)?.into_string()),
    };
    if input.enabled && url.is_none() {
        return Err(DomainError::validation(WebhookUrl::FIELD, ValidationReason::Empty).into());
    }
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            let current = outbound::settings(conn).await?;
            if input.enabled && current.webhook_secret.is_none() {
                return Err(
                    DomainError::validation("webhook_secret", ValidationReason::Empty).into(),
                );
            }
            require_updated(
                outbound::update_webhook(conn, input.enabled, url.as_deref(), request.version, now)
                    .await?,
            )?;
            let mut changes = Changes::default();
            changes
                .field("enabled", &current.webhook_enabled, &input.enabled)
                .field(
                    "url_origin",
                    &origin_of(current.webhook_url.as_deref()),
                    &origin_of(url.as_deref()),
                );
            let mut detail = changes.into_detail();
            detail["url_changed"] = (current.webhook_url != url).into();
            audit(
                conn,
                now,
                Audit {
                    action: "integration.webhook_update",
                    target_type: "outbound_settings",
                    target_id: None,
                    detail: Some(detail),
                    ctx: Some(ctx),
                },
            )
            .await?;
            dto(conn, now).await
        })
        .await
}

/// 서명 키를 새로 만든다. 원문은 이 응답에서만 돌려주고 감사 로그·로그에는 남기지 않는다.
/// 받는 쪽은 새 키로 바꿔야 한다(바로 다음 발송부터 새 키로 서명한다).
pub async fn rotate_webhook_secret(
    state: &AppState,
    ctx: &AuthContext,
    request: &Versioned<Empty>,
) -> ServiceResult<WebhookSecretDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let secret = new_webhook_secret()?;
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            let current = outbound::settings(conn).await?;
            require_updated(current.version == request.version)?;
            outbound::set_webhook_secret(conn, &secret, now).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "integration.webhook_secret_rotate",
                    target_type: "outbound_settings",
                    target_id: None,
                    detail: Some(serde_json::json!({
                        "replaced": current.webhook_secret.is_some(),
                    })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            Ok(WebhookSecretDto {
                secret: secret.clone(),
                settings: dto(conn, now).await?,
            })
        })
        .await
}

/// 프로세스 안의 간단한 횟수 제한. 통과하면 지금 시각을 기록한다.
fn take_test_slot(state: &AppState, now: Timestamp) -> ServiceResult<()> {
    let mut last = state
        .webhook_test_at
        .lock()
        .map_err(|_| ServiceError::internal("webhook test lock poisoned"))?;
    if let Some(previous) = *last {
        let elapsed = now.since(previous);
        if elapsed < TEST_COOLDOWN && elapsed >= Duration::zero() {
            let wait = (TEST_COOLDOWN - elapsed).num_seconds().max(1);
            return Err(ServiceError::TooManyAttempts {
                retry_after_secs: u64::try_from(wait).unwrap_or(1),
            });
        }
    }
    *last = Some(now);
    Ok(())
}

/// 지금 설정(꺼져 있어도)의 주소로 테스트 메시지를 바로 보내고 결과를 알려 준다. 받는 사람은
/// 요청한 관리자 본인이다. 주소나 서명 키가 없으면 400. 결과는 감사 로그에 남는다.
pub async fn test_webhook(state: &AppState, ctx: &AuthContext) -> ServiceResult<WebhookTestResult> {
    authz::require_admin(&ctx.actor)?;
    let settings: OutboundSettings = {
        let mut conn = state.read().await?;
        require_current_admin(&mut conn, ctx).await?;
        outbound::settings(&mut conn).await?
    };
    let Some(url) = settings.webhook_url else {
        return Err(DomainError::validation(WebhookUrl::FIELD, ValidationReason::Empty).into());
    };
    let Some(secret) = settings.webhook_secret else {
        return Err(DomainError::validation("webhook_secret", ValidationReason::Empty).into());
    };
    let now = state.now();
    take_test_slot(state, now)?;
    let member = &ctx.member;
    let delivery = Delivery {
        target: Target::Webhook(WebhookTarget { url, secret }),
        timestamp: now,
        payload: OutboundPayload {
            event: "test",
            delivery_id: format!("test-{}", OutboundMessageId::new()),
            kind: "test".to_owned(),
            title: "외부 알림 연동 테스트".to_owned(),
            body: "이 메시지가 보이면 연동이 잘 되고 있어요.".to_owned(),
            link: None,
            recipient: Recipient {
                login_id: member.login_id.clone(),
                name: member.name.clone(),
                email: member.email.clone(),
            },
            created_at: now,
        },
    };
    let result = match state.outbound.send(&delivery).await {
        Ok(sent) => WebhookTestResult {
            delivered: true,
            status: Some(sent.status),
            error: None,
        },
        Err(error) => WebhookTestResult {
            delivered: false,
            status: error.status,
            error: Some(error.reason),
        },
    };
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            audit(
                conn,
                state.now(),
                Audit {
                    action: "integration.webhook_test",
                    target_type: "outbound_settings",
                    target_id: None,
                    detail: Some(serde_json::json!({
                        "delivered": result.delivered,
                        "status": result.status,
                        "error": result.error,
                    })),
                    ctx: Some(ctx),
                },
            )
            .await
        })
        .await?;
    Ok(result)
}

/// 외부 채널(웹훅, 또는 실행 설정이 있는 Buzz)이 하나라도 켜져 있는지.
async fn external_available(state: &AppState, conn: &mut Conn) -> ServiceResult<bool> {
    Ok(outbound::settings(conn).await?.webhook_enabled
        || (state.config.buzz.is_some() && pango_db::buzz::settings(conn).await?.enabled))
}

pub async fn my_prefs(state: &AppState, ctx: &AuthContext) -> ServiceResult<NotificationPrefsDto> {
    let mut conn = state.read().await?;
    Ok(NotificationPrefsDto {
        external_available: external_available(state, &mut conn).await?,
        external_enabled: outbound::external_enabled(&mut conn, ctx.member_id()).await?,
    })
}

/// 본인의 외부 알림 수신 여부를 바꾼다. 연동이 꺼져 있어도 저장해 두고, 켜지면 그대로 따른다.
/// 이미 만들어 둔 보낼 일은 발송할 때 다시 확인하므로, 끄면 아직 보내지 않은 것도 보내지 않는다.
pub async fn update_my_prefs(
    state: &AppState,
    ctx: &AuthContext,
    input: &NotificationPrefsInput,
) -> ServiceResult<NotificationPrefsDto> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let member = current_member(conn, ctx).await?;
            outbound::set_external_enabled(conn, member.id, input.external_enabled, now).await?;
            Ok(NotificationPrefsDto {
                external_available: external_available(state, conn).await?,
                external_enabled: input.external_enabled,
            })
        })
        .await
}
