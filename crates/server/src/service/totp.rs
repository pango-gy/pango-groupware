//! 2단계 인증(TOTP): 본인의 켜기·끄기, 관리자의 초기화, 로그인의 두 번째 확인.
//!
//! - 켜기: 비밀번호 재확인 → 비밀키 발급(켜는 중) → 인증 앱의 첫 코드 확인 → 켜짐 + 복구 코드 10개
//!   (한 번만 보여 주고 해시만 남긴다).
//! - 로그인: 켜진 계정은 비밀번호가 맞아도 코드(또는 복구 코드)가 있어야 세션을 준다. 같은 코드는
//!   두 번 쓸 수 없다(`last_step`). 틀린 코드도 로그인 시도 한도에 들어간다.
//! - 끄기: 비밀번호와 지금 코드(또는 복구 코드). 휴대폰을 잃어버린 사람은 관리자가 초기화한다.

use pango_api::auth::{
    RecoveryCodes, TotpConfirmRequest, TotpDisableRequest, TotpSetup, TotpSetupRequest, TotpStatus,
};
use pango_core::authz;
use pango_core::error::{ConflictReason, ValidationReason};
use pango_core::id::MemberId;
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::{sessions, settings, totp as store, Conn};

use super::auth::verify_own_password;
use super::support::{audit, count_u32, managed_member, Audit, AuthContext};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;
use crate::totp;

pub async fn status(state: &AppState, ctx: &AuthContext) -> ServiceResult<TotpStatus> {
    let mut conn = state.read().await?;
    let enabled = store::find(&mut conn, ctx.member_id())
        .await?
        .is_some_and(|t| t.enabled());
    let left = if enabled {
        store::recovery_codes_left(&mut conn, ctx.member_id()).await?
    } else {
        0
    };
    Ok(TotpStatus {
        enabled,
        recovery_codes_left: count_u32(left)?,
    })
}

/// 켜기 시작: 비밀번호를 다시 확인하고 새 비밀키를 준다. 이미 켜져 있으면 409 `already_exists`.
/// 다시 부르면 켜는 중인 비밀키를 새로 바꾼다.
pub async fn setup(
    state: &AppState,
    ctx: &AuthContext,
    request: &TotpSetupRequest,
) -> ServiceResult<TotpSetup> {
    let verified = verify_own_password(state, ctx, &request.password, "password").await?;
    let secret = totp::generate_secret()?;
    let now = state.now();
    let company = state
        .write(async |conn: &mut Conn| {
            verified.still_current_tx(conn, ctx, now).await?;
            sessions::throttle_clear(conn, &verified.bucket).await?;
            if !store::put_pending(conn, ctx.member_id(), &secret, now).await? {
                return Err(DomainError::Conflict(ConflictReason::AlreadyExists).into());
            }
            Ok(settings::get(conn)
                .await?
                .map(|s| s.company_name)
                .unwrap_or_else(|| "pango".to_owned()))
        })
        .await?;
    Ok(TotpSetup {
        secret: totp::base32(&secret),
        otpauth_uri: totp::otpauth_uri(&company, &ctx.member.login_id, &secret),
    })
}

fn wrong_code() -> ServiceError {
    DomainError::validation("code", ValidationReason::InvalidFormat).into()
}

/// 인증 앱의 첫 코드를 확인해 켠다. 복구 코드를 한 번만 돌려준다.
pub async fn confirm(
    state: &AppState,
    ctx: &AuthContext,
    request: &TotpConfirmRequest,
) -> ServiceResult<RecoveryCodes> {
    let now = state.now();
    let codes = totp::generate_recovery_codes()?;
    state
        .write(async |conn: &mut Conn| {
            // 켜기를 시작하지 않았으면 맞출 코드가 없다.
            let pending = store::find(conn, ctx.member_id())
                .await?
                .ok_or_else(wrong_code)?;
            if pending.enabled() {
                return Err(DomainError::Conflict(ConflictReason::AlreadyExists).into());
            }
            let step =
                totp::verify(&pending.secret, &request.code, now, None).ok_or_else(wrong_code)?;
            if !store::confirm(conn, ctx.member_id(), step, now).await? {
                return Err(wrong_code());
            }
            let hashes: Vec<[u8; 32]> = codes.iter().map(|(_, hash)| *hash).collect();
            store::replace_recovery_codes(conn, ctx.member_id(), &hashes).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "auth.totp_enable",
                    target_type: "member",
                    target_id: Some(ctx.member_id().to_string()),
                    detail: None,
                    ctx: Some(ctx),
                },
            )
            .await?;
            Ok(())
        })
        .await?;
    Ok(RecoveryCodes {
        codes: codes.into_iter().map(|(code, _)| code).collect(),
    })
}

/// 코드(6자리) 또는 복구 코드를 확인하고 쓴 것으로 남긴다. 맞으면 `true`.
async fn accept_code_tx(
    conn: &mut Conn,
    member_id: MemberId,
    secret: &[u8],
    last_step: Option<i64>,
    code: &str,
    now: Timestamp,
) -> ServiceResult<bool> {
    if let Some(step) = totp::verify(secret, code, now, last_step) {
        return Ok(store::use_step(conn, member_id, step).await?);
    }
    match totp::recovery_hash(code) {
        Some(hash) => Ok(store::use_recovery_code(conn, member_id, &hash, now).await?),
        None => Ok(false),
    }
}

/// 로그인의 두 번째 확인. 2단계 인증이 꺼져 있으면 통과. 켜져 있으면 코드가 없을 때
/// `MfaRequired`, 틀리거나 이미 쓴 코드면 `MfaInvalid`. 로그인 쓰기 트랜잭션 안에서 부른다.
pub async fn second_factor_tx(
    conn: &mut Conn,
    member_id: MemberId,
    code: Option<&str>,
    now: Timestamp,
) -> ServiceResult<()> {
    let Some(current) = store::find(conn, member_id).await?.filter(|t| t.enabled()) else {
        return Ok(());
    };
    let code = code
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .ok_or(ServiceError::MfaRequired)?;
    if accept_code_tx(
        conn,
        member_id,
        &current.secret,
        current.last_step,
        code,
        now,
    )
    .await?
    {
        Ok(())
    } else {
        Err(ServiceError::MfaInvalid)
    }
}

/// 끄기: 비밀번호와 지금 코드(또는 복구 코드).
pub async fn disable(
    state: &AppState,
    ctx: &AuthContext,
    request: &TotpDisableRequest,
) -> ServiceResult<TotpStatus> {
    let verified = verify_own_password(state, ctx, &request.password, "password").await?;
    let bucket = &verified.bucket;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            verified.still_current_tx(conn, ctx, now).await?;
            let current = store::find(conn, ctx.member_id())
                .await?
                .filter(|t| t.enabled())
                .ok_or_else(|| ServiceError::Domain(DomainError::NotFound))?;
            if !accept_code_tx(
                conn,
                ctx.member_id(),
                &current.secret,
                current.last_step,
                &request.code,
                now,
            )
            .await?
            {
                return Err(wrong_code());
            }
            sessions::throttle_clear(conn, bucket).await?;
            store::remove(conn, ctx.member_id()).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "auth.totp_disable",
                    target_type: "member",
                    target_id: Some(ctx.member_id().to_string()),
                    detail: None,
                    ctx: Some(ctx),
                },
            )
            .await
        })
        .await?;
    status(state, ctx).await
}

/// 관리자의 초기화(휴대폰 분실 등). 계정 관리 권한이 있어야 하고 본인에게는 쓸 수 없다. 끄면 그
/// 구성원은 비밀번호만으로 로그인하고, 필요하면 다시 켠다.
pub async fn admin_reset(
    state: &AppState,
    ctx: &AuthContext,
    member_id: MemberId,
) -> ServiceResult<()> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            managed_member(conn, ctx, member_id, authz::manage_other_account).await?;
            if store::remove(conn, member_id).await? {
                audit(
                    conn,
                    now,
                    Audit {
                        action: "member.totp_reset",
                        target_type: "member",
                        target_id: Some(member_id.to_string()),
                        detail: None,
                        ctx: Some(ctx),
                    },
                )
                .await?;
            }
            Ok(())
        })
        .await
}

/// 관리자가 보는 구성원의 2단계 인증 여부. 계정 관리 권한이 있는 대상만 볼 수 있다(관리자는
/// 일반 구성원만, 최고 관리자는 전부).
pub async fn member_enabled(
    state: &AppState,
    ctx: &AuthContext,
    member_id: MemberId,
) -> ServiceResult<bool> {
    authz::require_admin(&ctx.actor)?;
    let mut conn = state.read().await?;
    managed_member(&mut conn, ctx, member_id, authz::manage_account).await?;
    Ok(store::find(&mut conn, member_id)
        .await?
        .is_some_and(|t| t.enabled()))
}
