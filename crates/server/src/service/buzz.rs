//! Buzz 채널: 구성원의 계정 연결(확인 코드)과 관리자의 켜기·끄기·연결 해제.
//!
//! Buzz에서는 이메일로 사용자를 찾을 수 없으므로 구성원이 자기 Buzz ID(공개키)를 직접 넣고,
//! 봇이 그 계정에 DM으로 보낸 6자리 코드를 다시 입력해 소유를 증명한다.
//!
//! - 코드는 해시만 저장한다. 10분 동안 유효(만료 시각 포함 거부), 5번 틀리면 그 코드는 끝.
//! - 다시 보내기는 60초 간격, 시간당 5번까지. 제한은 발송 **전에** 쓰기 트랜잭션에서 걸어 두므로
//!   동시에 눌러도 한 번만 보낸다. 발송이 실패해도 제한은 그대로다(릴레이를 두드리지 않도록).
//! - 확인 코드 DM은 아웃박스를 거치지 않고 바로 보낸다(사람이 기다리고 있다).
//! - 한 Buzz 계정은 한 구성원만 확인할 수 있다(DB 유일 색인으로도 보장).
//! - 감사 로그의 공개키는 줄인 표기(`npub1abcd…wxyz`)만 남긴다.

use pango_api::integration::{
    BuzzLinkStart, BuzzLinkStatus, BuzzLinkVerify, BuzzSettingsDto, BuzzSettingsInput,
    MemberBuzzDto, MyBuzzDto,
};
use pango_api::Versioned;
use pango_core::authz;
use pango_core::buzz::{
    code_usable, next_send, normalize_code, CodeProblem, SendWindow, CODE_TTL, MAX_CODE_ATTEMPTS,
    RESEND_COOLDOWN,
};
use pango_core::error::ConflictReason;
use pango_core::id::{MemberId, OutboundMessageId};
use pango_core::nostr::PublicKey;
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::buzz::{self, AccountStatus, BuzzAccount, NewCode};
use pango_db::{members, outbound, Conn};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::integrations::stats_dto;
use super::outbound::{BuzzTarget, Delivery, OutboundPayload, Recipient, SendError, Target};
use super::support::{
    audit, count_u32, current_member, found, require_current_admin, require_updated, Audit,
    AuthContext,
};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

fn conflict(reason: ConflictReason) -> ServiceError {
    DomainError::Conflict(reason).into()
}

fn short_of(pubkey_hex: &str) -> String {
    PublicKey::parse(pubkey_hex)
        .map(|key| key.short())
        .unwrap_or_default()
}

fn npub_of(pubkey_hex: &str) -> Option<String> {
    PublicKey::parse(pubkey_hex).ok().map(|key| key.to_npub())
}

fn code_hash(member_id: MemberId, pubkey_hex: &str, code: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"pango-buzz-link\0");
    hasher.update(member_id.to_string().as_bytes());
    hasher.update(b"\0");
    hasher.update(pubkey_hex.as_bytes());
    hasher.update(b"\0");
    hasher.update(code.as_bytes());
    hasher.finalize().into()
}

/// 균일한 6자리 숫자(거부 표집으로 치우침 없이).
fn new_code() -> ServiceResult<String> {
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(|e| ServiceError::internal(e.to_string()))?;
        let value = u32::from_le_bytes(bytes);
        // 1_000_000의 배수 아래만 쓴다.
        if value < 4_294_000_000 {
            return Ok(format!("{:06}", value % 1_000_000));
        }
    }
}

fn plus(now: Timestamp, after: chrono::Duration) -> ServiceResult<Timestamp> {
    now.checked_add(after)
        .ok_or_else(|| ServiceError::internal("time overflow"))
}

/// 회사가 Buzz를 쓸 수 있는지: 실행 설정이 있고 관리자가 켰다.
async fn available(state: &AppState, conn: &mut Conn) -> ServiceResult<bool> {
    Ok(state.config.buzz.is_some() && buzz::settings(conn).await?.enabled)
}

fn bot_npub(state: &AppState) -> Option<String> {
    state
        .config
        .buzz
        .as_ref()
        .map(|buzz| buzz.bot_key.public_key().to_npub())
}

fn my_dto(
    state: &AppState,
    available: bool,
    account: Option<&BuzzAccount>,
) -> ServiceResult<MyBuzzDto> {
    let status = match account.map(|a| a.status) {
        None => BuzzLinkStatus::None,
        Some(AccountStatus::Pending) => BuzzLinkStatus::Pending,
        Some(AccountStatus::Verified) => BuzzLinkStatus::Verified,
    };
    let pending = account.filter(|a| a.status == AccountStatus::Pending);
    let resend_available_at = match pending.and_then(|a| a.code_sent_at) {
        Some(sent) => Some(plus(sent, RESEND_COOLDOWN)?),
        None => None,
    };
    Ok(MyBuzzDto {
        available,
        status,
        npub: account.and_then(|a| npub_of(&a.pubkey)),
        code_expires_at: pending.and_then(|a| a.code_expires_at),
        attempts_left: pending.map(|a| MAX_CODE_ATTEMPTS.saturating_sub(a.code_attempts)),
        resend_available_at,
        bot_npub: bot_npub(state),
    })
}

pub async fn my_link(state: &AppState, ctx: &AuthContext) -> ServiceResult<MyBuzzDto> {
    let mut conn = state.read().await?;
    let available = available(state, &mut conn).await?;
    let account = buzz::find(&mut conn, ctx.member_id()).await?;
    my_dto(state, available, account.as_ref())
}

/// 바깥 발송 오류를 사용자에게 보여 줄 짧은 코드로 바꾼다.
fn upstream_error(error: &SendError) -> ServiceError {
    let code = match error.status {
        Some(401 | 403) => "buzz_bot_rejected",
        _ if error.reason == super::outbound::buzz::REJECTED => "buzz_recipient_rejected",
        Some(400..=499) if !error.retry => "buzz_recipient_rejected",
        _ => "buzz_unreachable",
    };
    ServiceError::Upstream(code)
}

/// 연결을 시작한다: Buzz ID를 확인하고, 봇이 DM으로 확인 코드를 보낸다. 같은 ID로 다시 부르면
/// 코드를 다시 보낸다(간격·시간당 제한 안에서).
pub async fn start_link(
    state: &AppState,
    ctx: &AuthContext,
    input: &BuzzLinkStart,
) -> ServiceResult<MyBuzzDto> {
    let pubkey = PublicKey::parse(&input.pubkey)?.to_hex();
    let now = state.now();
    let code = new_code()?;
    let hash = code_hash(ctx.member_id(), &pubkey, &code);
    let expires_at = plus(now, CODE_TTL)?;
    let dm_channel = state
        .write(async |conn: &mut Conn| {
            let member = current_member(conn, ctx).await?;
            if !available(state, conn).await? {
                return Err(conflict(ConflictReason::BuzzUnavailable));
            }
            if let Some(owner) = buzz::verified_owner(conn, &pubkey).await? {
                if owner != member.id {
                    return Err(conflict(ConflictReason::BuzzAccountTaken));
                }
            }
            let current = buzz::find(conn, member.id).await?;
            // 이미 확인을 마친 같은 계정이면 다시 확인할 필요가 없다.
            if let Some(account) = &current {
                if account.status == AccountStatus::Verified && account.pubkey == pubkey {
                    return Ok(None);
                }
            }
            let window = current
                .as_ref()
                .and_then(|a| a.send_window_start.map(|start| (start, a.sends_in_window)))
                .map(|(start, count)| SendWindow { start, count });
            let last_sent = current.as_ref().and_then(|a| a.code_sent_at);
            let window = next_send(last_sent, window, now).map_err(|wait| {
                ServiceError::TooManyAttempts {
                    retry_after_secs: u64::try_from(wait.num_seconds().max(1)).unwrap_or(60),
                }
            })?;
            // 같은 계정으로 다시 보내면 열어 둔 DM을 그대로 쓴다.
            let dm_channel = current
                .as_ref()
                .filter(|a| a.pubkey == pubkey)
                .and_then(|a| a.dm_channel_id.clone());
            buzz::put_code(
                conn,
                &NewCode {
                    member_id: member.id,
                    pubkey: &pubkey,
                    code_hash: hash,
                    expires_at,
                    window_start: window.start,
                    sends_in_window: window.count,
                    dm_channel_id: dm_channel.as_deref(),
                    now,
                },
            )
            .await?;
            audit(
                conn,
                now,
                Audit {
                    action: "buzz.link_start",
                    target_type: "member",
                    target_id: Some(member.id.to_string()),
                    detail: Some(serde_json::json!({ "pubkey": short_of(&pubkey) })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            Ok(Some(dm_channel))
        })
        .await?;
    let Some(dm_channel) = dm_channel else {
        return my_link(state, ctx).await;
    };

    let delivery = Delivery {
        target: Target::Buzz(BuzzTarget {
            pubkey: pubkey.clone(),
            dm_channel,
        }),
        timestamp: now,
        payload: OutboundPayload {
            event: "verification",
            delivery_id: format!("link-{}", OutboundMessageId::new()),
            kind: "buzz.link".to_owned(),
            title: format!("그룹웨어 Buzz 연결 확인 코드: {code}"),
            body: "10분 안에 그룹웨어의 내 정보 화면에 입력해 주세요. 직접 요청하지 않았다면 이 메시지를 무시해 주세요."
                .to_owned(),
            link: None,
            recipient: Recipient {
                login_id: ctx.member.login_id.clone(),
                name: ctx.member.name.clone(),
                email: None,
            },
            created_at: now,
        },
    };
    match state.outbound.send(&delivery).await {
        Ok(sent) => {
            if let Some(channel) = sent.dm_channel {
                state
                    .write(async |conn: &mut Conn| {
                        Ok(
                            buzz::cache_dm_channel(conn, ctx.member_id(), &pubkey, &channel)
                                .await?,
                        )
                    })
                    .await?;
            }
            my_link(state, ctx).await
        }
        Err(error) => {
            tracing::info!(reason = %error.reason, "buzz link code was not delivered");
            Err(upstream_error(&error))
        }
    }
}

enum VerifyOutcome {
    Verified,
    Wrong,
}

/// 받은 코드를 확인한다. 틀리면 횟수를 센다(그 기록은 남는다).
pub async fn verify_link(
    state: &AppState,
    ctx: &AuthContext,
    input: &BuzzLinkVerify,
) -> ServiceResult<MyBuzzDto> {
    let code = normalize_code(&input.code)?;
    let now = state.now();
    let outcome = state
        .write(async |conn: &mut Conn| {
            let member = current_member(conn, ctx).await?;
            if !available(state, conn).await? {
                return Err(conflict(ConflictReason::BuzzUnavailable));
            }
            let account = buzz::find(conn, member.id)
                .await?
                .filter(|a| a.status == AccountStatus::Pending)
                .ok_or_else(|| conflict(ConflictReason::BuzzCodeMissing))?;
            code_usable(account.code_expires_at, account.code_attempts, now).map_err(
                |problem| {
                    conflict(match problem {
                        CodeProblem::Missing => ConflictReason::BuzzCodeMissing,
                        CodeProblem::Expired => ConflictReason::BuzzCodeExpired,
                        CodeProblem::Exhausted => ConflictReason::BuzzCodeExhausted,
                    })
                },
            )?;
            let expected = account
                .code_hash
                .ok_or_else(|| conflict(ConflictReason::BuzzCodeMissing))?;
            let given = code_hash(member.id, &account.pubkey, &code);
            if !bool::from(given.ct_eq(&expected)) {
                buzz::count_wrong_code(conn, member.id, now).await?;
                return Ok(VerifyOutcome::Wrong);
            }
            if buzz::verified_owner(conn, &account.pubkey)
                .await?
                .is_some_and(|owner| owner != member.id)
            {
                return Err(conflict(ConflictReason::BuzzAccountTaken));
            }
            buzz::mark_verified(conn, member.id, now).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "buzz.link",
                    target_type: "member",
                    target_id: Some(member.id.to_string()),
                    detail: Some(serde_json::json!({ "pubkey": short_of(&account.pubkey) })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            Ok(VerifyOutcome::Verified)
        })
        .await;
    match outcome {
        Ok(VerifyOutcome::Verified) => my_link(state, ctx).await,
        Ok(VerifyOutcome::Wrong) => Err(conflict(ConflictReason::BuzzCodeWrong)),
        // 확인하는 사이 다른 구성원이 같은 계정을 먼저 확인했다(유일 색인).
        Err(ServiceError::Internal(source))
            if source
                .downcast_ref::<pango_db::DbError>()
                .is_some_and(buzz::is_duplicate_pubkey) =>
        {
            Err(conflict(ConflictReason::BuzzAccountTaken))
        }
        Err(error) => Err(error),
    }
}

async fn unlink_tx(
    conn: &mut Conn,
    now: Timestamp,
    member_id: MemberId,
    action: &'static str,
    ctx: &AuthContext,
) -> ServiceResult<()> {
    if let Some(removed) = buzz::remove(conn, member_id).await? {
        audit(
            conn,
            now,
            Audit {
                action,
                target_type: "member",
                target_id: Some(member_id.to_string()),
                detail: Some(serde_json::json!({ "pubkey": short_of(&removed.pubkey) })),
                ctx: Some(ctx),
            },
        )
        .await?;
    }
    Ok(())
}

/// 본인의 연결을 끊는다. 아직 보내지 않은 Buzz 메시지는 발송할 때 다시 확인하므로 보내지 않는다.
pub async fn unlink_mine(state: &AppState, ctx: &AuthContext) -> ServiceResult<MyBuzzDto> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let member = current_member(conn, ctx).await?;
            unlink_tx(conn, now, member.id, "buzz.unlink", ctx).await
        })
        .await?;
    my_link(state, ctx).await
}

/// 관리자가 보는 구성원의 연결. 관리자는 해제만 할 수 있고 대신 연결할 수는 없다.
pub async fn member_link(
    state: &AppState,
    ctx: &AuthContext,
    member_id: MemberId,
) -> ServiceResult<MemberBuzzDto> {
    authz::require_admin(&ctx.actor)?;
    let mut conn = state.read().await?;
    found(members::find(&mut conn, member_id).await?)?;
    let account = buzz::find(&mut conn, member_id).await?;
    Ok(MemberBuzzDto {
        status: match account.as_ref().map(|a| a.status) {
            None => BuzzLinkStatus::None,
            Some(AccountStatus::Pending) => BuzzLinkStatus::Pending,
            Some(AccountStatus::Verified) => BuzzLinkStatus::Verified,
        },
        npub_short: account.map(|a| short_of(&a.pubkey)),
    })
}

pub async fn admin_unlink(
    state: &AppState,
    ctx: &AuthContext,
    member_id: MemberId,
) -> ServiceResult<MemberBuzzDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            found(members::find(conn, member_id).await?)?;
            unlink_tx(conn, now, member_id, "buzz.admin_unlink", ctx).await
        })
        .await?;
    member_link(state, ctx, member_id).await
}

async fn settings_dto(
    state: &AppState,
    conn: &mut Conn,
    now: Timestamp,
) -> ServiceResult<BuzzSettingsDto> {
    let settings = buzz::settings(conn).await?;
    Ok(BuzzSettingsDto {
        configured: state.config.buzz.is_some(),
        enabled: settings.enabled,
        relay_url: state.config.buzz.as_ref().map(|b| b.relay_url.clone()),
        bot_npub: bot_npub(state),
        linked_members: count_u32(buzz::count_verified(conn).await?)?,
        version: settings.version,
        updated_at: settings.updated_at,
        stats: stats_dto(conn, outbound::CHANNEL_BUZZ, now).await?,
    })
}

pub async fn get_settings(state: &AppState, ctx: &AuthContext) -> ServiceResult<BuzzSettingsDto> {
    authz::require_admin(&ctx.actor)?;
    settings_dto(state, &mut *state.read().await?, state.now()).await
}

/// Buzz 채널을 켜거나 끈다. 서버 실행 설정이 없으면 켤 수 없다(409 `buzz_unavailable`).
pub async fn update_settings(
    state: &AppState,
    ctx: &AuthContext,
    request: &Versioned<BuzzSettingsInput>,
) -> ServiceResult<BuzzSettingsDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let enabled = request.data.enabled;
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            if enabled && state.config.buzz.is_none() {
                return Err(conflict(ConflictReason::BuzzUnavailable));
            }
            let current = buzz::settings(conn).await?;
            require_updated(buzz::update_settings(conn, enabled, request.version, now).await?)?;
            audit(
                conn,
                now,
                Audit {
                    action: "integration.buzz_update",
                    target_type: "buzz_settings",
                    target_id: None,
                    detail: Some(serde_json::json!({
                        "changed": { "enabled": { "from": current.enabled, "to": enabled } },
                    })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            settings_dto(state, conn, now).await
        })
        .await
}
