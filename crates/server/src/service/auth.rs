//! 로그인·세션·내 계정.

use pango_api::auth::{ChangePasswordRequest, LoginRequest, Me, SessionInfo};
use pango_core::error::{ConflictReason, ValidationReason};
use pango_core::id::SessionId;
use pango_core::password::PlainPassword;
use pango_core::text::LoginId;
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::members::{self, Member};
use pango_db::sessions::{self, Session, SessionVia};
use pango_db::{org, Conn};

use super::support::{audit, build_actor, found, Audit, AuthContext, Company, RequestMeta};
use crate::error::{ServiceError, ServiceResult};
use crate::security::SessionToken;
use crate::state::AppState;

/// 마지막 사용 시각은 이 간격보다 자주 갱신하지 않는다(요청마다 쓰기를 하지 않기 위해).
const TOUCH_INTERVAL_MS: i64 = 60_000;

pub struct LoginOutcome {
    pub token: SessionToken,
    pub me: Me,
    pub via: SessionVia,
    /// 쿠키 `Max-Age`에 쓸 절대 만료까지의 초.
    pub max_age_secs: i64,
}

fn retry_after(window_ms: i64) -> ServiceError {
    ServiceError::TooManyAttempts {
        retry_after_secs: (window_ms / 1000).max(1) as u64,
    }
}

/// 로그인.
///
/// 아이디 없음·비밀번호 불일치·비활성 계정을 같은 오류, 같은 소요 시간으로 응답한다.
/// 시도 제한은 IP와 아이디 각각에 걸리며, 아이디 제한은 없는 아이디에도 똑같이 적용된다.
/// 아이디별 횟수는 로그인에 성공하면 초기화된다.
pub async fn login(
    state: &AppState,
    request: &LoginRequest,
    meta: RequestMeta,
) -> ServiceResult<LoginOutcome> {
    let now = state.now();
    let config = &state.config;
    let ip_window = config.login_ip_window.num_milliseconds();
    let account_window = config.login_account_window.num_milliseconds();
    let login_id = LoginId::parse(&request.login_id).ok();
    let account_bucket = login_id.as_ref().map(|id| format!("acct:{id}"));

    // 1) 시도 제한. 비밀번호 검증(비싼 연산) 전에 **시도를 먼저 기록**한다.
    //    확인과 기록이 한 문장이라, 동시에 여러 요청을 보내도 제한을 넘겨 검증받을 수 없다.
    let blocked = state
        .write(async |conn: &mut Conn| {
            if let Some(ip) = &meta.ip {
                let bucket = format!("ip:{ip}");
                if sessions::throttle_hit(conn, &bucket, now, ip_window).await?
                    > config.login_ip_limit
                {
                    return Ok(Some(ip_window));
                }
            }
            if let Some(bucket) = &account_bucket {
                if sessions::throttle_hit(conn, bucket, now, account_window).await?
                    > config.login_account_limit
                {
                    return Ok(Some(account_window));
                }
            }
            Ok(None)
        })
        .await?;
    if let Some(window) = blocked {
        return Err(retry_after(window));
    }

    // 2) 비밀번호 검증. 계정이 없어도 같은 연산을 한다.
    let credentials = match &login_id {
        Some(id) => members::find_credentials(&mut *state.read().await?, id.as_str()).await?,
        None => None,
    };
    let password = PlainPassword::for_verification(&request.password).ok();
    let stored_hash = credentials.as_ref().map(|c| c.password_hash.as_str());
    let verified = match &password {
        Some(password) => state.passwords.verify(password, stored_hash).await?,
        None => false,
    };
    let member = credentials
        .map(|c| c.member)
        .filter(|member| verified && member.status.can_sign_in());

    // 3) 결과 기록과 세션 발급을 한 트랜잭션으로.
    let via = if request.token_in_body {
        SessionVia::Bearer
    } else {
        SessionVia::Cookie
    };
    let token = SessionToken::generate()?;
    let issued = state
        .write(async |conn: &mut Conn| {
            // 실패한 시도는 1)에서 이미 기록됐다.
            let Some(member) = member else {
                return Ok(None);
            };
            if let Some(bucket) = &account_bucket {
                sessions::throttle_clear(conn, bucket).await?;
            }
            let session = new_session(&state.config, &member, via, &meta, now)?;
            sessions::insert(conn, &session, &token.hash()).await?;
            let ctx = AuthContext {
                actor: build_actor(conn, &member).await?,
                member,
                session_id: session.id,
                via,
                meta: meta.clone(),
            };
            audit(
                conn,
                now,
                Audit {
                    action: "auth.login",
                    target_type: "session",
                    target_id: Some(session.id.to_string()),
                    detail: None,
                    ctx: Some(&ctx),
                },
            )
            .await?;
            Ok(Some(me_from(conn, &ctx).await?))
        })
        .await?;

    let me = issued.ok_or(ServiceError::InvalidCredentials)?;
    Ok(LoginOutcome {
        token,
        me,
        via,
        max_age_secs: state.config.session_absolute.num_seconds(),
    })
}

fn new_session(
    config: &crate::Config,
    member: &Member,
    via: SessionVia,
    meta: &RequestMeta,
    now: Timestamp,
) -> ServiceResult<Session> {
    let expiry = |duration| {
        now.checked_add(duration)
            .ok_or_else(|| ServiceError::internal("session expiry overflow"))
    };
    Ok(Session {
        id: SessionId::new(),
        member_id: member.id,
        via,
        // 헤더 값은 외부 입력이므로 길이를 제한해 저장한다.
        user_agent: meta
            .user_agent
            .as_deref()
            .map(|ua| ua.chars().take(300).collect()),
        ip: meta.ip.clone(),
        created_at: now,
        last_seen_at: now,
        idle_expires_at: expiry(config.session_idle)?,
        absolute_expires_at: expiry(config.session_absolute)?,
        revoked_at: None,
    })
}

/// 토큰으로 요청을 인증한다. 실패 사유는 구분하지 않고 `Unauthenticated`.
pub async fn authenticate(
    state: &AppState,
    token: &SessionToken,
    meta: RequestMeta,
) -> ServiceResult<AuthContext> {
    let now = state.now();
    let unauthenticated = || ServiceError::Domain(DomainError::Unauthenticated);
    let mut conn = state.read().await?;
    let session = sessions::find_by_token_hash(&mut conn, &token.hash())
        .await?
        .filter(|session| session.is_valid_at(now))
        .ok_or_else(unauthenticated)?;
    let member = members::find(&mut conn, session.member_id)
        .await?
        .filter(|member| member.status.can_sign_in())
        .ok_or_else(unauthenticated)?;
    let actor = build_actor(&mut conn, &member).await?;
    drop(conn);

    if now.since(session.last_seen_at).num_milliseconds() >= TOUCH_INTERVAL_MS {
        let idle = now
            .checked_add(state.config.session_idle)
            .ok_or_else(|| ServiceError::internal("session expiry overflow"))?;
        state
            .write(async |conn: &mut Conn| Ok(sessions::touch(conn, session.id, now, idle).await?))
            .await?;
    }
    Ok(AuthContext {
        actor,
        member,
        session_id: session.id,
        via: session.via,
        meta,
    })
}

pub async fn logout(state: &AppState, ctx: &AuthContext) -> ServiceResult<()> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            sessions::revoke(conn, ctx.member_id(), ctx.session_id, now).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "auth.logout",
                    target_type: "session",
                    target_id: Some(ctx.session_id.to_string()),
                    detail: None,
                    ctx: Some(ctx),
                },
            )
            .await
        })
        .await
}

pub async fn me(state: &AppState, ctx: &AuthContext) -> ServiceResult<Me> {
    me_from(&mut *state.read().await?, ctx).await
}

async fn me_from(conn: &mut Conn, ctx: &AuthContext) -> ServiceResult<Me> {
    let member = &ctx.member;
    let department = match member.department_id {
        Some(id) => org::find_department(conn, id).await?,
        None => None,
    };
    let position = match member.position_id {
        Some(id) => org::find_position(conn, id).await?,
        None => None,
    };
    let company = Company::load(conn).await?;
    Ok(Me {
        id: member.id,
        login_id: member.login_id.clone(),
        name: member.name.clone(),
        email: member.email.clone(),
        phone: member.phone.clone(),
        role: member.role,
        department_id: member.department_id,
        department_name: department.map(|d| d.name),
        position_id: member.position_id,
        position_name: position.map(|p| p.name),
        capabilities: ctx.actor.capabilities(),
        must_change_password: member.must_change_password,
        company_name: company.settings.company_name,
        timezone: company.settings.timezone,
    })
}

/// 본인 비밀번호 변경. 성공하면 현재 세션을 뺀 모든 세션을 폐기한다.
pub async fn change_password(
    state: &AppState,
    ctx: &AuthContext,
    request: &ChangePasswordRequest,
) -> ServiceResult<()> {
    let now = state.now();
    let login_id = LoginId::parse(&ctx.member.login_id).map_err(ServiceError::internal)?;
    let new_password = PlainPassword::parse_new(&request.new_password, &login_id)
        .map_err(|e| rename_field(e, "new_password"))?;
    if request.new_password == request.current_password {
        return Err(DomainError::Conflict(ConflictReason::PasswordReused).into());
    }

    // 탈취된 세션으로 현재 비밀번호를 무제한 추측하지 못하게, 로그인과 같은 한도를 건다.
    let window = state.config.login_account_window.num_milliseconds();
    let bucket = format!("pwchg:{}", ctx.member_id());
    let attempts = state
        .write(
            async |conn: &mut Conn| Ok(sessions::throttle_hit(conn, &bucket, now, window).await?),
        )
        .await?;
    if attempts > state.config.login_account_limit {
        return Err(retry_after(window));
    }

    let credentials =
        found(members::find_credentials_by_id(&mut *state.read().await?, ctx.member_id()).await?)?;
    let current = PlainPassword::for_verification(&request.current_password).ok();
    let verified = match &current {
        Some(current) => {
            state
                .passwords
                .verify(current, Some(&credentials.password_hash))
                .await?
        }
        None => false,
    };
    if !verified {
        return Err(
            DomainError::validation("current_password", ValidationReason::InvalidFormat).into(),
        );
    }

    let new_hash = state.passwords.hash(&new_password).await?;
    state
        .write(async |conn: &mut Conn| {
            members::set_password(conn, ctx.member_id(), &new_hash, false, now).await?;
            sessions::throttle_clear(conn, &bucket).await?;
            let revoked =
                sessions::revoke_all(conn, ctx.member_id(), Some(ctx.session_id), now).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "auth.password_change",
                    target_type: "member",
                    target_id: Some(ctx.member_id().to_string()),
                    detail: Some(serde_json::json!({ "revoked_sessions": revoked })),
                    ctx: Some(ctx),
                },
            )
            .await
        })
        .await
}

fn rename_field(error: DomainError, field: &'static str) -> DomainError {
    match error {
        DomainError::Validation { reason, .. } => DomainError::Validation { field, reason },
        other => other,
    }
}

pub async fn list_sessions(state: &AppState, ctx: &AuthContext) -> ServiceResult<Vec<SessionInfo>> {
    let sessions =
        sessions::list_active(&mut *state.read().await?, ctx.member_id(), state.now()).await?;
    Ok(sessions
        .into_iter()
        .map(|s| SessionInfo {
            id: s.id,
            current: s.id == ctx.session_id,
            user_agent: s.user_agent,
            ip: s.ip,
            created_at: s.created_at,
            last_seen_at: s.last_seen_at,
        })
        .collect())
}

/// 본인의 다른 기기 세션을 끊는다. 이미 끊겼으면 성공(멱등), 남의 세션이면 `NotFound`.
pub async fn revoke_session(
    state: &AppState,
    ctx: &AuthContext,
    session_id: SessionId,
) -> ServiceResult<()> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            if sessions::revoke(conn, ctx.member_id(), session_id, now).await? {
                return audit(
                    conn,
                    now,
                    Audit {
                        action: "auth.session_revoke",
                        target_type: "session",
                        target_id: Some(session_id.to_string()),
                        detail: None,
                        ctx: Some(ctx),
                    },
                )
                .await;
            }
            // 폐기할 것이 없었다: 이미 폐기된 내 세션이면 성공, 아니면 없는 것으로 취급.
            let mine = sessions::belongs_to(conn, ctx.member_id(), session_id).await?;
            if mine {
                Ok(())
            } else {
                Err(DomainError::NotFound.into())
            }
        })
        .await
}
