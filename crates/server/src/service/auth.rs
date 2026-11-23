//! 로그인·세션·내 계정.

use pango_api::auth::{ChangePasswordRequest, LoginRequest, Me, SessionInfo};
use pango_core::error::{ConflictReason, ValidationReason};
use pango_core::id::SessionId;
use pango_core::password::PlainPassword;
use pango_core::text::LoginId;
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::members::{self, Member};
use pango_db::sessions::{self, Session, SessionVia, Throttle, ThrottleMode};
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

/// 한도를 넘었으면 창이 끝날 때까지 남은 시간을 담은 오류를 돌려준다.
fn check_limit(
    throttle: Throttle,
    limit: i64,
    window_ms: i64,
    now: Timestamp,
) -> ServiceResult<()> {
    if throttle.attempts <= limit {
        return Ok(());
    }
    let elapsed = now
        .since(throttle.window_start)
        .num_milliseconds()
        .clamp(0, window_ms);
    let remaining_secs = ((window_ms - elapsed + 999) / 1000).max(1);
    Err(ServiceError::TooManyAttempts {
        retry_after_secs: remaining_secs.unsigned_abs(),
    })
}

/// IP별 제한의 키. IPv6는 /64 단위로 묶는다(한 가입자가 주소를 바꿔 가며 제한을 피하지 못하게).
pub(crate) fn ip_bucket(ip: &str) -> String {
    // IPv4-매핑 주소(`::ffff:1.2.3.4`)는 IPv4로 되돌린다. 그대로 /64로 묶으면 모든 IPv4
    // 클라이언트가 한 버킷을 나눠 써서, 한 명이 전체의 로그인을 막을 수 있다.
    match ip
        .parse::<std::net::IpAddr>()
        .map(|addr| addr.to_canonical())
    {
        Ok(std::net::IpAddr::V6(v6)) => {
            let prefix = v6.segments();
            format!(
                "ip6:{:x}:{:x}:{:x}:{:x}",
                prefix[0], prefix[1], prefix[2], prefix[3]
            )
        }
        Ok(std::net::IpAddr::V4(v4)) => format!("ip:{v4}"),
        Err(_) => format!("ip:{ip}"),
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
    //    제한에 걸려도 기록은 커밋해야 하므로 판정은 트랜잭션 밖에서 한다.
    let account_mode = ThrottleMode::Lockout {
        limit: config.login_account_limit,
    };
    let (ip_state, account_state) = state
        .write(async |conn: &mut Conn| {
            let ip_state = match &meta.ip {
                Some(ip) => Some(
                    sessions::throttle_hit(
                        conn,
                        &ip_bucket(ip),
                        now,
                        ip_window,
                        ThrottleMode::Fixed,
                    )
                    .await?,
                ),
                None => None,
            };
            let account_state = match &account_bucket {
                Some(bucket) => Some(
                    sessions::throttle_hit(conn, bucket, now, account_window, account_mode).await?,
                ),
                None => None,
            };
            Ok((ip_state, account_state))
        })
        .await?;
    if let Some(throttle) = ip_state {
        check_limit(throttle, config.login_ip_limit, ip_window, now)?;
    }
    if let Some(throttle) = account_state {
        check_limit(throttle, config.login_account_limit, account_window, now)?;
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
    let verified_hash = credentials
        .as_ref()
        .filter(|c| verified && c.member.status.can_sign_in())
        .map(|c| c.password_hash.clone());
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
            let (Some(member), Some(verified_hash)) = (member, verified_hash.as_deref()) else {
                return Ok(None);
            };
            // 검증과 이 쓰기 사이에 비밀번호가 바뀌었거나 계정이 정지됐으면 세션을 주지 않는다.
            // 그러지 않으면 비밀번호 변경이 기존 세션을 모두 끊은 직후 옛 비밀번호로 새 세션이 생긴다.
            if !password_unchanged_tx(conn, member.id, verified_hash).await? {
                return Ok(None);
            }
            // 2단계 인증을 켠 계정은 코드까지 맞아야 한다(틀린 코드는 1)에서 시도로 이미 셌다).
            super::totp::second_factor_tx(conn, member.id, request.totp_code.as_deref(), now)
                .await?;
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
    let mut conn = state.read().await?;
    let session = sessions::find_by_token_hash(&mut conn, &token.hash()).await?;
    let (ctx, last_seen_at) = context_for(&mut conn, session, meta, now).await?;
    drop(conn);

    if now.since(last_seen_at).num_milliseconds() >= TOUCH_INTERVAL_MS {
        let idle = now
            .checked_add(state.config.session_idle)
            .ok_or_else(|| ServiceError::internal("session expiry overflow"))?;
        state
            .write(async |conn: &mut Conn| {
                Ok(sessions::touch(conn, ctx.session_id, now, idle).await?)
            })
            .await?;
    }
    Ok(ctx)
}

/// 세션으로 요청자 정보를 만든다(토큰·내려받기 링크 인증이 함께 쓴다). 세션이 없거나
/// 만료·폐기됐거나 구성원이 로그인할 수 없으면 `Unauthenticated`. 세션의 마지막 사용 시각도
/// 돌려준다.
async fn context_for(
    conn: &mut Conn,
    session: Option<Session>,
    meta: RequestMeta,
    now: Timestamp,
) -> ServiceResult<(AuthContext, Timestamp)> {
    let unauthenticated = || ServiceError::Domain(DomainError::Unauthenticated);
    let session = session
        .filter(|session| session.is_valid_at(now))
        .ok_or_else(unauthenticated)?;
    let member = members::find(conn, session.member_id)
        .await?
        .filter(|member| member.status.can_sign_in())
        .ok_or_else(unauthenticated)?;
    let actor = build_actor(conn, &member).await?;
    Ok((
        AuthContext {
            actor,
            member,
            session_id: session.id,
            via: session.via,
            meta,
        },
        session.last_seen_at,
    ))
}

/// 내려받기 링크의 세션으로 인증한다. 세션이 살아 있고 구성원이 로그인할 수 있어야 한다.
/// 사용 시각은 늘리지 않는다(링크 열기는 사용자가 앱을 쓰는 것이 아니다).
pub async fn authenticate_session(
    state: &AppState,
    session_id: SessionId,
    meta: RequestMeta,
) -> ServiceResult<AuthContext> {
    let mut conn = state.read().await?;
    let session = sessions::find(&mut conn, session_id).await?;
    Ok(context_for(&mut conn, session, meta, state.now()).await?.0)
}

/// 내려받기 링크 유효 시간.
pub const DOWNLOAD_LINK_TTL: chrono::Duration = chrono::Duration::minutes(5);

/// 내려받기 링크로 열 수 있는 경로인지: 공지·결재 문서 첨부파일과 근태 CSV 내보내기만.
fn downloadable(target: &str) -> bool {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if query.contains("download_token") || target.contains("..") || target.contains('#') {
        return false;
    }
    let segments: Vec<&str> = path.split('/').collect();
    match segments.as_slice() {
        ["", "notices", owner, "attachments", attachment] => {
            query.is_empty()
                && pango_core::id::NoticeId::parse(owner).is_ok()
                && pango_core::id::AttachmentId::parse(attachment).is_ok()
        }
        ["", "approval", "documents", owner, "attachments", attachment] => {
            query.is_empty()
                && pango_core::id::DocumentId::parse(owner).is_ok()
                && pango_core::id::AttachmentId::parse(attachment).is_ok()
        }
        ["", "admin", "attendance", "export.csv"] => query
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"=&-_".contains(&b)),
        _ => false,
    }
}

/// 앱이 시스템 브라우저로 열 내려받기 링크를 만든다. 링크는 지금 세션과 그 경로 하나에만
/// 유효하고 [`DOWNLOAD_LINK_TTL`] 뒤에 만료된다. 권한은 링크를 열 때 그 경로가 다시 확인한다.
pub fn download_link(
    state: &AppState,
    ctx: &AuthContext,
    target: &str,
) -> ServiceResult<pango_api::auth::DownloadLink> {
    if !downloadable(target) {
        return Err(
            DomainError::validation("path", pango_core::ValidationReason::InvalidFormat).into(),
        );
    }
    let expires_at = state
        .now()
        .checked_add(DOWNLOAD_LINK_TTL)
        .ok_or_else(|| ServiceError::internal("expiry overflow"))?;
    let token = state.downloads.sign(ctx.session_id, expires_at, target);
    let separator = if target.contains('?') { '&' } else { '?' };
    Ok(pango_api::auth::DownloadLink {
        url: format!(
            "{}{target}{separator}download_token={token}",
            pango_api::API_PREFIX
        ),
        expires_at,
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
        .map_err(|e| e.with_field("new_password"))?;
    if request.new_password == request.current_password {
        return Err(DomainError::Conflict(ConflictReason::PasswordReused).into());
    }

    let verified =
        verify_own_password(state, ctx, &request.current_password, "current_password").await?;

    let new_hash = state.passwords.hash(&new_password).await?;
    state
        .write(async |conn: &mut Conn| {
            verified.still_current_tx(conn, ctx, now).await?;
            let bucket = &verified.bucket;
            members::set_password(conn, ctx.member_id(), &new_hash, false, now).await?;
            sessions::throttle_clear(conn, bucket).await?;
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

/// 로그인한 구성원의 지금 비밀번호를 다시 확인한다(비밀번호 변경, 2단계 인증 켜기·끄기).
///
/// 탈취된 세션으로 비밀번호를 무제한 추측하지 못하게 로그인과 같은 한도를 건다. 틀리면 `field`를
/// 가리키는 검증 오류. 맞으면 쓰기 트랜잭션에서 [`VerifiedPassword::still_current_tx`]로 다시 확인한다.
pub async fn verify_own_password(
    state: &AppState,
    ctx: &AuthContext,
    password: &str,
    field: &'static str,
) -> ServiceResult<VerifiedPassword> {
    let now = state.now();
    let window = state.config.login_account_window.num_milliseconds();
    let limit = state.config.login_account_limit;
    let bucket = format!("pwchg:{}", ctx.member_id());
    let throttle = state
        .write(async |conn: &mut Conn| {
            let mode = ThrottleMode::Lockout { limit };
            Ok(sessions::throttle_hit(conn, &bucket, now, window, mode).await?)
        })
        .await?;
    check_limit(throttle, limit, window, now)?;

    let credentials =
        found(members::find_credentials_by_id(&mut *state.read().await?, ctx.member_id()).await?)?;
    let current = PlainPassword::for_verification(password).ok();
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
        return Err(DomainError::validation(field, ValidationReason::InvalidFormat).into());
    }
    Ok(VerifiedPassword {
        bucket,
        hash: credentials.password_hash,
    })
}

/// 확인을 마친 지금 비밀번호. 확인(읽기)과 쓰기 트랜잭션은 따로라 그 사이에 다른 요청이 비밀번호를
/// 바꾸고 이 세션을 끊었을 수 있다.
pub struct VerifiedPassword {
    /// 시도 기록의 이름. 성공한 쓰기 트랜잭션에서 지운다.
    pub bucket: String,
    hash: String,
}

impl std::fmt::Debug for VerifiedPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedPassword")
            .field("bucket", &self.bucket)
            .finish_non_exhaustive()
    }
}

impl VerifiedPassword {
    /// 쓰기 트랜잭션 안에서: 비밀번호가 확인한 값 그대로이고 이 세션이 아직 유효한지.
    /// 아니면 `Unauthenticated`(다시 로그인해야 한다).
    pub async fn still_current_tx(
        &self,
        conn: &mut Conn,
        ctx: &AuthContext,
        now: Timestamp,
    ) -> ServiceResult<()> {
        let session_valid = sessions::find(conn, ctx.session_id)
            .await?
            .is_some_and(|session| session.is_valid_at(now));
        if session_valid && password_unchanged_tx(conn, ctx.member_id(), &self.hash).await? {
            Ok(())
        } else {
            Err(DomainError::Unauthenticated.into())
        }
    }
}

/// 저장된 비밀번호 해시가 `verified_hash` 그대로이고 로그인할 수 있는 상태인지.
async fn password_unchanged_tx(
    conn: &mut Conn,
    member_id: pango_core::id::MemberId,
    verified_hash: &str,
) -> ServiceResult<bool> {
    Ok(members::find_credentials_by_id(conn, member_id)
        .await?
        .is_some_and(|c| c.password_hash == verified_hash && c.member.status.can_sign_in()))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(millis: i64) -> Timestamp {
        Timestamp::from_millis(millis).expect("timestamp")
    }

    fn retry_after(attempts: i64, started: i64, now: i64) -> Option<u64> {
        let throttle = Throttle {
            attempts,
            window_start: ts(started),
        };
        match check_limit(throttle, 5, 900_000, ts(now)) {
            Ok(()) => None,
            Err(ServiceError::TooManyAttempts { retry_after_secs }) => Some(retry_after_secs),
            Err(other) => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn retry_after_is_the_time_left_in_the_window() {
        assert_eq!(retry_after(5, 0, 0), None, "한도까지는 허용");
        assert_eq!(retry_after(6, 0, 0), Some(900));
        assert_eq!(retry_after(6, 0, 899_001), Some(1), "올림");
        assert_eq!(retry_after(6, 0, 600_000), Some(300));
        assert_eq!(
            retry_after(6, 1_000, 0),
            Some(900),
            "시계가 뒤로 가도 음수가 되지 않는다"
        );
        assert_eq!(
            retry_after(6, 0, 5_000_000),
            Some(1),
            "창이 지난 값이 와도 최소 1초"
        );
    }

    #[test]
    fn ipv6_addresses_share_a_bucket_per_64_prefix() {
        assert_eq!(ip_bucket("203.0.113.9"), "ip:203.0.113.9");
        let a = ip_bucket("2001:db8:1:2:aaaa:bbbb:cccc:dddd");
        let b = ip_bucket("2001:db8:1:2::1");
        let other = ip_bucket("2001:db8:1:3::1");
        assert_eq!(a, b);
        assert_ne!(a, other);
        assert_eq!(a, "ip6:2001:db8:1:2");
        assert_eq!(ip_bucket("not-an-ip"), "ip:not-an-ip");
    }

    #[test]
    fn ipv4_mapped_addresses_are_bucketed_per_ipv4_address() {
        assert_eq!(ip_bucket("::ffff:203.0.113.9"), "ip:203.0.113.9");
        assert_eq!(ip_bucket("::ffff:cb00:7109"), "ip:203.0.113.9");
        assert_ne!(
            ip_bucket("::ffff:203.0.113.9"),
            ip_bucket("::ffff:203.0.113.10")
        );
        assert_eq!(ip_bucket("::ffff:203.0.113.9"), ip_bucket("203.0.113.9"));
    }
}
