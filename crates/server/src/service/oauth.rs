//! 원격 MCP용 OAuth 2.1 인가 서버(공개 클라이언트 + PKCE S256).
//!
//! - 동적 등록(RFC 7591): 돌아갈 주소는 허용 목록(claude.ai·claude.com 커넥터, 선택적으로 이
//!   컴퓨터)만. 클라이언트 ID는 이름·주소로 정해진다.
//! - 동의: 기존 로그인(`auth::login`, 시도 한도·2단계 인증·감사 로그 포함)으로 Bearer 세션을 하나
//!   만들고, 그 세션에 코드·토큰을 매단다. 세션이 끊기면(로그아웃, 기기 로그아웃, 비밀번호 변경,
//!   비활성화) MCP 연결도 끊긴다. 내 정보의 "로그인된 기기"에 "AI 연결"로 보인다.
//! - scope: 조회(`groupware:read`) / 변경(`groupware:write`) / 관리(`groupware:admin`, 관리자만).
//!   요청에 scope가 없으면 조회만 준다.
//! - 접근 토큰 10분, 갱신 토큰은 한 번 쓰면 새로 받는다. 쓴 갱신 토큰·코드가 다시 오면 세션을 끊는다.

use base64::Engine as _;
use pango_api::auth::LoginRequest;
use pango_core::authz::Role;
use pango_core::id::SessionId;
use pango_core::time::Timestamp;
use pango_db::oauth::{self, AuthRequest, Client, Code, CodeUse, RefreshUse, Token, TokenKind};
use pango_db::{sessions, Conn};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::support::{Audit, AuthContext, RequestMeta};
use crate::config::McpConfig;
use crate::error::{ServiceError, ServiceResult};
use crate::security::{hex, SessionToken};
use crate::state::AppState;

pub const SCOPE_READ: &str = "groupware:read";
pub const SCOPE_WRITE: &str = "groupware:write";
pub const SCOPE_ADMIN: &str = "groupware:admin";
pub const SCOPES: [&str; 3] = [SCOPE_READ, SCOPE_WRITE, SCOPE_ADMIN];

const ACCESS_TTL: chrono::Duration = chrono::Duration::minutes(10);
const REFRESH_TTL: chrono::Duration = chrono::Duration::days(30);
const CODE_TTL: chrono::Duration = chrono::Duration::seconds(60);
/// 같은 갱신 토큰이 이 시간 안에 다시 오면 병렬 갱신·재시도로 보고 연결을 끊지 않는다.
const REFRESH_REUSE_GRACE: chrono::Duration = chrono::Duration::seconds(30);
/// 등록·인가 요청의 IP당 한도(1분).
const REGISTER_PER_MINUTE: i64 = 30;
const AUTHORIZE_PER_MINUTE: i64 = 60;

/// 동의 화면을 열어 둘 수 있는 시간(브라우저 쿠키 `Max-Age`와 같다).
pub const REQUEST_TTL: chrono::Duration = chrono::Duration::minutes(10);

/// claude.ai·claude.com 커넥터의 콜백.
const CONNECTOR_CALLBACKS: [&str; 2] = [
    "https://claude.ai/api/mcp/auth_callback",
    "https://claude.com/api/mcp/auth_callback",
];

/// 동의 화면에 보일 scope 설명(사용자 말로).
pub fn scope_label(scope: &str) -> (&'static str, bool) {
    match scope {
        SCOPE_READ => ("내 근태·휴가·결재함·공지·구성원 정보를 조회", false),
        SCOPE_WRITE => (
            "휴가 신청·결재 기안, 결재 승인·반려·회수, 출퇴근 기록을 나 대신 실행",
            true,
        ),
        SCOPE_ADMIN => (
            "공지 게시 등 관리자 작업을 나 대신 실행(전체 구성원에게 알림이 갈 수 있음)",
            true,
        ),
        _ => ("알 수 없는 권한", true),
    }
}

/// OAuth 오류(RFC 6749 §5.2 모양).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OAuthError {
    pub error: &'static str,
    pub error_description: String,
    #[serde(skip)]
    pub status: u16,
}

impl OAuthError {
    fn new(status: u16, error: &'static str, description: impl Into<String>) -> Self {
        Self {
            error,
            error_description: description.into(),
            status,
        }
    }

    fn invalid_grant(description: impl Into<String>) -> Self {
        Self::new(400, "invalid_grant", description)
    }
}

fn internal(error: ServiceError) -> OAuthError {
    tracing::error!(%error, "oauth internal error");
    OAuthError::new(500, "server_error", "내부 오류")
}

fn config(state: &AppState) -> Result<&McpConfig, OAuthError> {
    state
        .config
        .mcp
        .as_ref()
        .ok_or_else(|| OAuthError::new(503, "temporarily_unavailable", "MCP가 켜져 있지 않습니다"))
}

fn sha(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

/// `resource`(RFC 8707)가 이 서버의 MCP 주소인지. 대소문자와 끝의 `/`는 가리지 않는다.
fn same_resource(config: &McpConfig, given: &str) -> bool {
    given
        .trim_end_matches('/')
        .eq_ignore_ascii_case(config.resource().trim_end_matches('/'))
}

/// IP당 요청 한도(1분 고정 창). 넘었으면 `false`. IP를 모르면 통과(테스트·내부 호출).
async fn within_limit(
    state: &AppState,
    kind: &str,
    ip: Option<&str>,
    limit: i64,
) -> ServiceResult<bool> {
    let Some(ip) = ip else {
        return Ok(true);
    };
    let bucket = format!("oauth-{kind}:{ip}");
    let now = state.now();
    let hit = state
        .write(async |conn: &mut Conn| {
            Ok(
                sessions::throttle_hit(conn, &bucket, now, 60_000, sessions::ThrottleMode::Fixed)
                    .await?,
            )
        })
        .await?;
    Ok(hit.attempts <= limit)
}

/// `S256`: base64url(SHA-256(verifier)) == challenge.
fn pkce_matches(verifier: &str, challenge: &str) -> bool {
    // RFC 7636 §4.1: 43~128자, 비예약 문자만.
    let valid = (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b));
    if !valid {
        return false;
    }
    let computed =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier));
    subtle::ConstantTimeEq::ct_eq(computed.as_bytes(), challenge.as_bytes()).into()
}

/// 이 컴퓨터로 돌아오는 주소(RFC 8252 §7.3): `http://localhost[:포트]/…`, `http://127.0.0.1[:포트]/…`.
fn is_loopback(uri: &str) -> bool {
    let Some(rest) = uri.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let name = host.rsplit_once(':').map_or(host, |(name, port)| {
        if port.bytes().all(|b| b.is_ascii_digit()) && !port.is_empty() {
            name
        } else {
            host
        }
    });
    matches!(name, "localhost" | "127.0.0.1") && !rest.contains('#')
}

fn redirect_allowed(config: &McpConfig, uri: &str) -> bool {
    CONNECTOR_CALLBACKS.contains(&uri) || (config.allow_loopback_redirects && is_loopback(uri))
}

// ── 동적 등록 ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct RegisterRequest {
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub token_endpoint_auth_method: Option<String>,
    #[serde(default)]
    pub grant_types: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisterResponse {
    pub client_id: String,
    pub client_id_issued_at: i64,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
    pub grant_types: [&'static str; 2],
    pub response_types: [&'static str; 1],
    pub token_endpoint_auth_method: &'static str,
    pub scope: String,
}

pub async fn register(
    state: &AppState,
    request: &RegisterRequest,
    ip: Option<&str>,
) -> Result<RegisterResponse, OAuthError> {
    let config = config(state)?;
    if !within_limit(state, "register", ip, REGISTER_PER_MINUTE)
        .await
        .map_err(internal)?
    {
        return Err(OAuthError::new(429, "slow_down", "요청이 너무 많습니다"));
    }
    let invalid = |d: &str| OAuthError::new(400, "invalid_client_metadata", d);
    if request
        .token_endpoint_auth_method
        .as_deref()
        .is_some_and(|m| m != "none")
    {
        return Err(invalid(
            "공개 클라이언트(token_endpoint_auth_method=none)만 받습니다",
        ));
    }
    if request.grant_types.as_ref().is_some_and(|grants| {
        grants
            .iter()
            .any(|g| g != "authorization_code" && g != "refresh_token")
    }) {
        return Err(invalid("authorization_code·refresh_token만 받습니다"));
    }
    if request.redirect_uris.is_empty() || request.redirect_uris.len() > 5 {
        return Err(OAuthError::new(
            400,
            "invalid_redirect_uri",
            "redirect_uris가 필요합니다",
        ));
    }
    if let Some(bad) = request
        .redirect_uris
        .iter()
        .find(|u| !redirect_allowed(config, u))
    {
        return Err(OAuthError::new(
            400,
            "invalid_redirect_uri",
            format!("허용하지 않는 주소입니다: {bad}"),
        ));
    }
    let name: String = request
        .client_name
        .as_deref()
        .unwrap_or("MCP 클라이언트")
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    let mut uris = request.redirect_uris.clone();
    uris.sort();
    uris.dedup();
    let client_id = format!(
        "pgw_{}",
        &hex(&sha(&format!("{name}\n{}", uris.join("\n"))))[..32]
    );
    let client = Client {
        client_id: client_id.clone(),
        client_name: name.clone(),
        redirect_uris: uris.clone(),
    };
    let now = state.now();
    state
        .write(async |conn: &mut Conn| Ok(oauth::upsert_client(conn, &client, now).await?))
        .await
        .map_err(internal)?;
    Ok(RegisterResponse {
        client_id,
        client_id_issued_at: now.as_millis() / 1000,
        client_name: name,
        redirect_uris: uris,
        grant_types: ["authorization_code", "refresh_token"],
        response_types: ["code"],
        token_endpoint_auth_method: "none",
        scope: SCOPES.join(" "),
    })
}

// ── 인가 요청과 동의 ───────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthorizeParams {
    #[serde(default)]
    pub response_type: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub code_challenge: Option<String>,
    #[serde(default)]
    pub code_challenge_method: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
}

/// 인가 요청이 실패했을 때: 돌려보낼 수 없으면 오류 화면, 있으면 `error=`로 돌려보낸다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizeFailure {
    Page(String),
    Redirect(String),
}

/// 동의 화면에 그릴 내용.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consent {
    pub request_id: String,
    pub client_name: String,
    /// 승인하면 돌아갈 곳(호스트).
    pub return_host: String,
    pub scopes: Vec<String>,
}

fn redirect_with(uri: &str, params: &[(&str, &str)]) -> String {
    let encode = |value: &str| -> String {
        value
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                    char::from(b).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    };
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", encode(v)))
        .collect();
    let separator = if uri.contains('?') { '&' } else { '?' };
    format!("{uri}{separator}{}", query.join("&"))
}

fn host_of(uri: &str) -> String {
    uri.split_once("://")
        .map(|(_, rest)| {
            rest.split(['/', '?', '#'])
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .unwrap_or_default()
}

/// 요청한 scope를 정리한다. 없으면 조회만(fail-closed). 모르는 값은 버린다.
fn requested_scopes(raw: Option<&str>) -> Vec<String> {
    let mut scopes: Vec<String> = raw
        .unwrap_or(SCOPE_READ)
        .split_whitespace()
        .filter(|s| SCOPES.contains(s))
        .map(str::to_owned)
        .collect();
    if scopes.is_empty() {
        scopes.push(SCOPE_READ.to_owned());
    }
    if !scopes.iter().any(|s| s == SCOPE_READ) {
        scopes.insert(0, SCOPE_READ.to_owned());
    }
    scopes.sort_by_key(|s| SCOPES.iter().position(|x| x == s));
    scopes.dedup();
    scopes
}

/// 인가 요청을 확인하고 동의 화면을 연다. `(동의 내용, 브라우저 쿠키에 둘 nonce)`.
pub async fn start_authorize(
    state: &AppState,
    params: &AuthorizeParams,
    ip: Option<&str>,
) -> Result<(Consent, String), AuthorizeFailure> {
    let config = config(state).map_err(|e| AuthorizeFailure::Page(e.error_description))?;
    let page = |m: &str| AuthorizeFailure::Page(m.to_owned());
    if !within_limit(state, "authorize", ip, AUTHORIZE_PER_MINUTE)
        .await
        .unwrap_or(false)
    {
        return Err(page("요청이 너무 많습니다. 잠시 후 다시 시도해 주세요."));
    }
    let client_id = params
        .client_id
        .as_deref()
        .ok_or_else(|| page("client_id가 없습니다."))?;
    let client = {
        let mut conn = state
            .read()
            .await
            .map_err(|_| page("잠시 후 다시 시도해 주세요."))?;
        oauth::find_client(&mut conn, client_id)
            .await
            .map_err(|_| page("잠시 후 다시 시도해 주세요."))?
            .ok_or_else(|| {
                page(
                    "등록되지 않은 클라이언트입니다. AI 앱에서 연결을 처음부터 다시 시작해 주세요.",
                )
            })?
    };
    let redirect_uri = params
        .redirect_uri
        .as_deref()
        .ok_or_else(|| page("redirect_uri가 없습니다."))?;
    if !client.redirect_uris.iter().any(|u| u == redirect_uri)
        || !redirect_allowed(config, redirect_uri)
    {
        return Err(page("등록되지 않은 돌아갈 주소입니다."));
    }
    // 여기부터는 클라이언트에 오류를 돌려보낸다.
    let fail = |error: &str, description: &str| {
        let mut query = vec![
            ("error", error),
            ("error_description", description),
            ("iss", config.issuer.as_str()),
        ];
        if let Some(s) = params.state.as_deref() {
            query.push(("state", s));
        }
        AuthorizeFailure::Redirect(redirect_with(redirect_uri, &query))
    };
    if params.response_type.as_deref() != Some("code") {
        return Err(fail(
            "unsupported_response_type",
            "response_type=code만 지원합니다",
        ));
    }
    let challenge = params.code_challenge.as_deref().unwrap_or_default();
    if params.code_challenge_method.as_deref() != Some("S256") || challenge.len() != 43 {
        return Err(fail("invalid_request", "PKCE(S256)가 필요합니다"));
    }
    if params
        .resource
        .as_deref()
        .is_some_and(|r| !same_resource(config, r))
    {
        return Err(fail("invalid_target", "알 수 없는 resource입니다"));
    }
    if params.state.as_deref().is_some_and(|s| s.len() > 500) {
        return Err(fail("invalid_request", "state가 너무 깁니다"));
    }
    let scopes = requested_scopes(params.scope.as_deref());
    let nonce = SessionToken::generate().map_err(|_| page("잠시 후 다시 시도해 주세요."))?;
    let id = SessionToken::generate().map_err(|_| page("잠시 후 다시 시도해 주세요."))?;
    let now = state.now();
    let request = AuthRequest {
        id: id.expose().to_owned(),
        nonce_hash: nonce.hash(),
        client_id: client.client_id.clone(),
        redirect_uri: redirect_uri.to_owned(),
        scope: scopes.join(" "),
        state: params.state.clone(),
        code_challenge: challenge.to_owned(),
        expires_at: now
            .checked_add(REQUEST_TTL)
            .ok_or_else(|| page("잠시 후 다시 시도해 주세요."))?,
    };
    state
        .write(async |conn: &mut Conn| Ok(oauth::insert_request(conn, &request).await?))
        .await
        .map_err(|_| page("잠시 후 다시 시도해 주세요."))?;
    Ok((
        Consent {
            request_id: request.id,
            client_name: client.client_name,
            return_host: host_of(redirect_uri),
            scopes,
        },
        nonce.expose().to_owned(),
    ))
}

/// 동의 화면을 다시 그릴 때(비밀번호가 틀렸을 때 등) 쓰는 내용.
pub async fn consent_of(state: &AppState, request_id: &str) -> Option<Consent> {
    let mut conn = state.read().await.ok()?;
    let request = oauth::find_request(&mut conn, request_id, state.now())
        .await
        .ok()??;
    let client = oauth::find_client(&mut conn, &request.client_id)
        .await
        .ok()??;
    Some(Consent {
        request_id: request.id,
        client_name: client.client_name,
        return_host: host_of(&request.redirect_uri),
        scopes: request
            .scope
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
    })
}

/// 동의 화면에서 누른 것.
pub enum Decision {
    Deny,
    Approve {
        login_id: String,
        password: String,
        totp_code: Option<String>,
    },
}

/// 동의 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decided {
    /// 클라이언트로 돌려보낸다(코드 또는 `error=access_denied`).
    Redirect(String),
    /// 같은 화면을 다시 그린다(로그인 실패, 2단계 인증 코드 필요 등).
    Retry {
        consent: Consent,
        message: String,
        needs_code: bool,
    },
    /// 요청이 끝났거나 쿠키가 맞지 않는다.
    Page(String),
}

pub async fn decide(
    state: &AppState,
    request_id: &str,
    nonce: Option<&str>,
    decision: Decision,
    meta: RequestMeta,
) -> Decided {
    let Ok(config) = config(state) else {
        return Decided::Page("MCP가 켜져 있지 않습니다.".into());
    };
    let now = state.now();
    let request = match state.read().await {
        Ok(mut conn) => oauth::find_request(&mut conn, request_id, now)
            .await
            .ok()
            .flatten(),
        Err(_) => None,
    };
    let Some(request) = request else {
        return Decided::Page(
            "요청 시간이 지났습니다. AI 앱에서 연결을 다시 시작해 주세요.".into(),
        );
    };
    let nonce_ok = nonce.and_then(SessionToken::parse).is_some_and(|n| {
        bool::from(subtle::ConstantTimeEq::ct_eq(
            &n.hash()[..],
            &request.nonce_hash[..],
        ))
    });
    if !nonce_ok {
        return Decided::Page(
            "이 브라우저에서 시작한 요청이 아닙니다. AI 앱에서 연결을 다시 시작해 주세요.".into(),
        );
    }
    let finish = |params: &[(&str, &str)]| {
        let mut query = params.to_vec();
        query.push(("iss", config.issuer.as_str()));
        if let Some(s) = request.state.as_deref() {
            query.push(("state", s));
        }
        redirect_with(&request.redirect_uri, &query)
    };
    let (login_id, password, totp_code) = match decision {
        Decision::Deny => {
            let _ = state
                .write(async |conn: &mut Conn| Ok(oauth::delete_request(conn, request_id).await?))
                .await;
            return Decided::Redirect(finish(&[
                ("error", "access_denied"),
                ("error_description", "사용자가 거부했습니다"),
            ]));
        }
        Decision::Approve {
            login_id,
            password,
            totp_code,
        } => (login_id, password, totp_code),
    };
    let Some(consent) = consent_of(state, request_id).await else {
        return Decided::Page(
            "요청 시간이 지났습니다. AI 앱에서 연결을 다시 시작해 주세요.".into(),
        );
    };
    let retry = |message: &str, needs_code: bool| Decided::Retry {
        consent: consent.clone(),
        message: message.to_owned(),
        needs_code,
    };
    // 내 정보의 "로그인된 기기"에서 알아볼 수 있게 기기 이름을 붙인다.
    let meta = RequestMeta {
        user_agent: Some(format!("AI 연결(MCP) · {}", consent.client_name)),
        ..meta
    };
    let login = LoginRequest {
        login_id,
        password,
        token_in_body: true,
        totp_code: totp_code.filter(|c| !c.trim().is_empty()),
    };
    let outcome = match super::auth::login(state, &login, meta).await {
        Ok(outcome) => outcome,
        Err(ServiceError::InvalidCredentials) => {
            return retry("아이디 또는 비밀번호가 맞지 않아요.", false)
        }
        Err(ServiceError::MfaRequired) => {
            return retry(
                "2단계 인증을 켠 계정이에요. 인증 앱의 6자리 코드도 넣어 주세요.",
                true,
            )
        }
        Err(ServiceError::MfaInvalid) => {
            return retry("인증 코드가 맞지 않거나 이미 쓴 코드예요.", true)
        }
        Err(ServiceError::TooManyAttempts { .. }) => {
            return retry("시도가 너무 많아요. 잠시 후 다시 시도해 주세요.", false)
        }
        Err(error) => {
            tracing::error!(%error, "oauth consent login failed");
            return retry("로그인하지 못했어요. 잠시 후 다시 시도해 주세요.", false);
        }
    };
    let token_hash = outcome.token.hash();
    let me = outcome.me;
    // 관리 scope는 관리자에게만 준다(서비스도 매 요청 권한을 다시 확인한다).
    let is_admin = matches!(me.role, Role::Admin | Role::Owner);
    let scope: Vec<String> = consent
        .scopes
        .iter()
        .filter(|s| s.as_str() != SCOPE_ADMIN || is_admin)
        .cloned()
        .collect();
    let code = match SessionToken::generate() {
        Ok(code) => code,
        Err(_) => return retry("잠시 후 다시 시도해 주세요.", false),
    };
    let must_change = me.must_change_password;
    let stored = state
        .write(async |conn: &mut Conn| {
            let session = sessions::find_by_token_hash(conn, &token_hash)
                .await?
                .ok_or_else(|| ServiceError::internal("session missing after login"))?;
            if must_change {
                // 임시 비밀번호 상태로는 연결하지 않는다(웹에서 먼저 바꾼다).
                sessions::revoke(conn, session.member_id, session.id, now).await?;
                return Ok(false);
            }
            if !oauth::delete_request(conn, request_id).await? {
                sessions::revoke(conn, session.member_id, session.id, now).await?;
                return Ok(false);
            }
            oauth::insert_code(
                conn,
                &Code {
                    code_hash: code.hash(),
                    client_id: request.client_id.clone(),
                    session_id: session.id,
                    redirect_uri: request.redirect_uri.clone(),
                    scope: scope.join(" "),
                    code_challenge: request.code_challenge.clone(),
                    expires_at: now
                        .checked_add(CODE_TTL)
                        .ok_or_else(|| ServiceError::internal("code expiry overflow"))?,
                },
            )
            .await?;
            oauth::mark_client_used(conn, &request.client_id, now).await?;
            super::support::audit_as(
                conn,
                now,
                session.member_id,
                Audit {
                    action: "oauth.grant",
                    target_type: "session",
                    target_id: Some(session.id.to_string()),
                    detail: Some(serde_json::json!({
                        "client_id": request.client_id,
                        "client_name": consent.client_name,
                        "scope": scope.join(" "),
                    })),
                    ctx: None,
                },
            )
            .await?;
            Ok(true)
        })
        .await;
    match stored {
        Ok(true) => Decided::Redirect(finish(&[("code", code.expose())])),
        Ok(false) if must_change => Decided::Page(
            "임시 비밀번호 상태예요. 웹에서 비밀번호를 먼저 바꾼 뒤 다시 연결해 주세요.".into(),
        ),
        Ok(false) => {
            Decided::Page("이미 처리한 요청입니다. AI 앱에서 연결을 다시 시작해 주세요.".into())
        }
        Err(error) => {
            tracing::error!(%error, "oauth code issue failed");
            // 로그인이 만든 세션은 이미 커밋됐다. 아무도 토큰을 받지 못했으니 끊어 둔다
            // (그러지 않으면 쓸 수 없는 "AI 연결" 기기가 남고 다시 시도할 때마다 늘어난다).
            let revoked = state
                .write(async |conn: &mut Conn| {
                    let Some(session) = sessions::find_by_token_hash(conn, &token_hash).await?
                    else {
                        return Ok(());
                    };
                    sessions::revoke(conn, session.member_id, session.id, now).await?;
                    Ok(())
                })
                .await;
            if let Err(error) = revoked {
                tracing::error!(%error, "could not revoke the session of a failed oauth grant");
            }
            retry("잠시 후 다시 시도해 주세요.", false)
        }
    }
}

// ── 토큰 ─────────────────────────────────────────────────────────────

#[derive(Clone, Default, Deserialize)]
pub struct TokenRequest {
    #[serde(default)]
    pub grant_type: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub redirect_uri: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub code_verifier: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

// 코드·verifier·갱신 토큰이 로그에 찍히지 않게 한다.
impl std::fmt::Debug for TokenRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenRequest")
            .field("grant_type", &self.grant_type)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Serialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub refresh_token: String,
    pub scope: String,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenResponse(***)")
    }
}

/// 세션을 끊는다(코드·갱신 토큰 재사용을 탈취로 본다).
async fn revoke_session_tx(
    conn: &mut Conn,
    session_id: SessionId,
    now: Timestamp,
) -> ServiceResult<()> {
    if let Some(session) = sessions::find(conn, session_id).await? {
        sessions::revoke(conn, session.member_id, session.id, now).await?;
        audit_as_system(conn, now, session.member_id, session_id).await?;
    }
    Ok(())
}

async fn audit_as_system(
    conn: &mut Conn,
    now: Timestamp,
    member_id: pango_core::id::MemberId,
    session_id: SessionId,
) -> ServiceResult<()> {
    super::support::audit_as(
        conn,
        now,
        member_id,
        Audit {
            action: "oauth.token_reuse",
            target_type: "session",
            target_id: Some(session_id.to_string()),
            detail: None,
            ctx: None,
        },
    )
    .await
}

/// 세션이 살아 있으면 접근·갱신 토큰을 새로 준다(세션 사용 시각도 늘린다).
async fn issue_tx(
    state: &AppState,
    conn: &mut Conn,
    session_id: SessionId,
    client_id: &str,
    scope: &str,
    now: Timestamp,
) -> ServiceResult<Option<TokenResponse>> {
    let Some(session) = sessions::find(conn, session_id)
        .await?
        .filter(|s| s.is_valid_at(now))
    else {
        return Ok(None);
    };
    let access = SessionToken::generate()?;
    let refresh = SessionToken::generate()?;
    let expiry = |ttl| {
        now.checked_add(ttl)
            .ok_or_else(|| ServiceError::internal("token expiry overflow"))
    };
    let refresh_expires = expiry(REFRESH_TTL)?.min(session.absolute_expires_at);
    for (token, kind, expires_at) in [
        (&access, TokenKind::Access, expiry(ACCESS_TTL)?),
        (&refresh, TokenKind::Refresh, refresh_expires),
    ] {
        oauth::insert_token(
            conn,
            &Token {
                token_hash: token.hash(),
                kind,
                session_id,
                client_id: client_id.to_owned(),
                scope: scope.to_owned(),
                expires_at,
            },
        )
        .await?;
    }
    let idle = expiry(state.config.session_idle)?;
    sessions::touch(conn, session_id, now, idle).await?;
    Ok(Some(TokenResponse {
        access_token: access.expose().to_owned(),
        token_type: "Bearer",
        expires_in: ACCESS_TTL.num_seconds(),
        refresh_token: refresh.expose().to_owned(),
        scope: scope.to_owned(),
    }))
}

pub async fn token(state: &AppState, request: &TokenRequest) -> Result<TokenResponse, OAuthError> {
    let config = config(state)?;
    if request
        .resource
        .as_deref()
        .is_some_and(|r| !same_resource(config, r))
    {
        return Err(OAuthError::new(
            400,
            "invalid_target",
            "알 수 없는 resource입니다",
        ));
    }
    let client_id = request
        .client_id
        .as_deref()
        .ok_or_else(|| OAuthError::new(401, "invalid_client", "client_id가 없습니다"))?;
    let now = state.now();
    match request.grant_type.as_deref() {
        Some("authorization_code") => {
            let code = request
                .code
                .as_deref()
                .and_then(SessionToken::parse)
                .ok_or_else(|| OAuthError::invalid_grant("코드가 맞지 않습니다"))?;
            let verifier = request.code_verifier.as_deref().unwrap_or_default();
            let redirect = request.redirect_uri.as_deref().unwrap_or_default();
            state
                .write(async |conn: &mut Conn| {
                    match oauth::use_code(conn, &code.hash(), now).await? {
                        CodeUse::Invalid => Ok(Err(OAuthError::invalid_grant(
                            "코드가 맞지 않거나 만료됐습니다",
                        ))),
                        CodeUse::Reused(session_id) => {
                            revoke_session_tx(conn, session_id, now).await?;
                            Ok(Err(OAuthError::invalid_grant("이미 쓴 코드입니다")))
                        }
                        CodeUse::Fresh(code) => {
                            if code.client_id != client_id || code.redirect_uri != redirect {
                                return Ok(Err(OAuthError::invalid_grant(
                                    "클라이언트나 돌아갈 주소가 맞지 않습니다",
                                )));
                            }
                            if !pkce_matches(verifier, &code.code_challenge) {
                                return Ok(Err(OAuthError::invalid_grant(
                                    "code_verifier가 맞지 않습니다",
                                )));
                            }
                            Ok(
                                issue_tx(state, conn, code.session_id, client_id, &code.scope, now)
                                    .await?
                                    .ok_or_else(|| {
                                        OAuthError::invalid_grant("로그인이 끝났습니다")
                                    }),
                            )
                        }
                    }
                })
                .await
                .map_err(internal)?
        }
        Some("refresh_token") => {
            let refresh = request
                .refresh_token
                .as_deref()
                .and_then(SessionToken::parse)
                .ok_or_else(|| OAuthError::invalid_grant("갱신 토큰이 맞지 않습니다"))?;
            state
                .write(async |conn: &mut Conn| {
                    match oauth::use_refresh(conn, &refresh.hash(), now).await? {
                        RefreshUse::Invalid => Ok(Err(OAuthError::invalid_grant(
                            "갱신 토큰이 맞지 않거나 만료됐습니다",
                        ))),
                        RefreshUse::Reused {
                            session_id,
                            used_at,
                        } => {
                            // 병렬 갱신·응답 유실 뒤 재시도는 끊지 않는다(앞선 요청이 받은 토큰을 쓴다).
                            if now.since(used_at) < REFRESH_REUSE_GRACE {
                                return Ok(Err(OAuthError::invalid_grant(
                                    "이미 쓴 갱신 토큰입니다",
                                )));
                            }
                            revoke_session_tx(conn, session_id, now).await?;
                            Ok(Err(OAuthError::invalid_grant(
                                "이미 쓴 갱신 토큰입니다. 다시 연결해 주세요",
                            )))
                        }
                        RefreshUse::Fresh(token) => {
                            if token.client_id != client_id {
                                return Ok(Err(OAuthError::invalid_grant(
                                    "클라이언트가 맞지 않습니다",
                                )));
                            }
                            // 줄여 달라는 scope만 받는다(늘릴 수는 없다).
                            let scope = match request.scope.as_deref() {
                                Some(asked) => {
                                    let granted: Vec<&str> =
                                        token.scope.split_whitespace().collect();
                                    let mut narrowed: Vec<&str> = asked
                                        .split_whitespace()
                                        .filter(|s| granted.contains(s))
                                        .collect();
                                    if narrowed.is_empty() {
                                        return Ok(Err(OAuthError::new(
                                            400,
                                            "invalid_scope",
                                            "받은 적 없는 scope입니다",
                                        )));
                                    }
                                    // 조회는 늘 남긴다(받았던 scope 안에서).
                                    if granted.contains(&SCOPE_READ)
                                        && !narrowed.contains(&SCOPE_READ)
                                    {
                                        narrowed.insert(0, SCOPE_READ);
                                    }
                                    narrowed.join(" ")
                                }
                                None => token.scope.clone(),
                            };
                            Ok(
                                issue_tx(state, conn, token.session_id, client_id, &scope, now)
                                    .await?
                                    .ok_or_else(|| {
                                        OAuthError::invalid_grant(
                                            "로그인이 끝났습니다. 다시 연결해 주세요",
                                        )
                                    }),
                            )
                        }
                    }
                })
                .await
                .map_err(internal)?
        }
        _ => Err(OAuthError::new(
            400,
            "unsupported_grant_type",
            "authorization_code·refresh_token만 지원합니다",
        )),
    }
}

/// `/mcp` 요청의 접근 토큰을 확인한다. `(요청자, 받은 scope)`. 세션이 끊겼거나 임시 비밀번호
/// 상태면 `None`.
pub async fn authenticate(
    state: &AppState,
    bearer: &str,
    meta: RequestMeta,
) -> ServiceResult<Option<(AuthContext, Vec<String>)>> {
    let Some(token) = SessionToken::parse(bearer) else {
        return Ok(None);
    };
    let found = oauth::find_access(&mut *state.read().await?, &token.hash(), state.now()).await?;
    let Some((session_id, _client, scope)) = found else {
        return Ok(None);
    };
    let ctx = match super::auth::authenticate_session(state, session_id, meta).await {
        Ok(ctx) => ctx,
        Err(ServiceError::Domain(pango_core::DomainError::Unauthenticated)) => return Ok(None),
        Err(error) => return Err(error),
    };
    if ctx.member.must_change_password {
        return Ok(None);
    }
    Ok(Some((
        ctx,
        scope.split_whitespace().map(str::to_owned).collect(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_s256_matches_the_rfc_example() {
        // RFC 7636 부록 B.
        assert!(pkce_matches(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
        assert!(!pkce_matches(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            "x"
        ));
        assert!(!pkce_matches(
            "short",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        ));
    }

    #[test]
    fn loopback_redirects_are_only_localhost_http() {
        for ok in [
            "http://localhost:3334/callback",
            "http://127.0.0.1/cb",
            "http://localhost/cb",
        ] {
            assert!(is_loopback(ok), "{ok}");
        }
        for bad in [
            "https://localhost/cb",
            "http://localhost.evil.com/cb",
            "http://127.0.0.1.nip.io/cb",
            "http://evil.com/?localhost",
            "http://localhost:abc/cb",
        ] {
            assert!(!is_loopback(bad), "{bad}");
        }
    }

    #[test]
    fn scopes_default_to_read_and_drop_unknown_values() {
        assert_eq!(requested_scopes(None), [SCOPE_READ]);
        assert_eq!(requested_scopes(Some("")), [SCOPE_READ]);
        assert_eq!(requested_scopes(Some("openid profile")), [SCOPE_READ]);
        assert_eq!(
            requested_scopes(Some("groupware:admin groupware:write")),
            [SCOPE_READ, SCOPE_WRITE, SCOPE_ADMIN]
        );
    }

    #[test]
    fn redirect_query_is_encoded() {
        assert_eq!(
            redirect_with(
                "https://claude.ai/api/mcp/auth_callback",
                &[("code", "a b"), ("state", "x&y=z")]
            ),
            "https://claude.ai/api/mcp/auth_callback?code=a%20b&state=x%26y%3Dz"
        );
    }
}
