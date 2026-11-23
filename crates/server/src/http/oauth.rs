//! 원격 MCP의 OAuth 경로: 발견 문서(RFC 9728·8414), 동적 등록, 동의 화면, 토큰.
//!
//! 동의 화면은 서버가 그리는 HTML이다(웹 앱과 별개, 외부 자원 없음). 브라우저와의 약속:
//! - `Referrer-Policy: same-origin` — `no-referrer`면 승인 POST의 `Origin`이 `null`이 된다.
//! - 승인 POST의 `Origin`은 issuer와 같아야 하고, `null`은 거부한다(없으면 통과).
//! - nonce 쿠키는 `Path=/oauth/authorize; SameSite=Lax; HttpOnly`(https면 `Secure`).
//! - 거부는 아이디·비밀번호 없이 누를 수 있다. 틀린 비밀번호는 400이 아니라 화면을 다시 그린다.

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::Deserialize;

use super::extract::ClientMeta;
use crate::service::oauth::{
    self, AuthorizeFailure, AuthorizeParams, Consent, Decided, Decision, RegisterRequest,
    TokenRequest, SCOPES, SCOPE_ADMIN, SCOPE_READ,
};
use crate::state::AppState;

const NONCE_COOKIE: &str = "pango_oauth_nonce";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server),
        )
        .route("/oauth/register", post(register))
        .route("/oauth/authorize", get(authorize).post(decide))
        .route("/oauth/token", post(token))
}

fn issuer(state: &AppState) -> String {
    state
        .config
        .mcp
        .as_ref()
        .map(|m| m.issuer.clone())
        .unwrap_or_default()
}

fn no_store() -> [(header::HeaderName, HeaderValue); 1] {
    [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))]
}

async fn protected_resource(State(state): State<AppState>) -> Response {
    let issuer = issuer(&state);
    Json(serde_json::json!({
        "resource": format!("{issuer}/mcp"),
        "authorization_servers": [issuer],
        "scopes_supported": SCOPES,
        "bearer_methods_supported": ["header"],
        "resource_name": "pango-groupware",
    }))
    .into_response()
}

async fn authorization_server(State(state): State<AppState>) -> Response {
    let issuer = issuer(&state);
    Json(serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/oauth/authorize"),
        "token_endpoint": format!("{issuer}/oauth/token"),
        "registration_endpoint": format!("{issuer}/oauth/register"),
        "scopes_supported": SCOPES,
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "authorization_response_iss_parameter_supported": true,
    }))
    .into_response()
}

fn oauth_error(error: oauth::OAuthError) -> Response {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::BAD_REQUEST);
    (status, no_store(), Json(error)).into_response()
}

async fn register(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    Json(request): Json<RegisterRequest>,
) -> Response {
    match oauth::register(&state, &request, meta.ip.as_deref()).await {
        Ok(created) => (StatusCode::CREATED, no_store(), Json(created)).into_response(),
        Err(error) => oauth_error(error),
    }
}

async fn token(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    Form(request): Form<TokenRequest>,
) -> Response {
    match oauth::token(&state, &request, meta.ip.as_deref()).await {
        Ok(tokens) => (no_store(), Json(tokens)).into_response(),
        Err(error) => oauth_error(error),
    }
}

// ── 동의 화면 ───────────────────────────────────────────────────────────

fn esc(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn page_headers(state: &AppState) -> Vec<(header::HeaderName, HeaderValue)> {
    let loopback = state
        .config
        .mcp
        .as_ref()
        .is_some_and(|m| m.allow_loopback_redirects);
    // 승인·거부 뒤의 303도 form-action 검사를 받는다(Chrome). 돌아갈 곳을 모두 적는다.
    let form_action = if loopback {
        "'self' https://claude.ai https://claude.com http://localhost:* http://127.0.0.1:*"
    } else {
        "'self' https://claude.ai https://claude.com"
    };
    let csp = format!(
        "default-src 'none'; style-src 'unsafe-inline'; form-action {form_action}; \
         frame-ancestors 'none'; base-uri 'none'"
    );
    vec![
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        ),
        (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        (
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ),
        (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_str(&csp)
                .unwrap_or_else(|_| HeaderValue::from_static("default-src 'none'")),
        ),
    ]
}

fn html(state: &AppState, status: StatusCode, body: String, cookie: Option<String>) -> Response {
    let mut response = (status, body).into_response();
    let headers = response.headers_mut();
    for (name, value) in page_headers(state) {
        headers.insert(name, value);
    }
    if let Some(cookie) = cookie.and_then(|c| HeaderValue::from_str(&c).ok()) {
        headers.insert(header::SET_COOKIE, cookie);
    }
    response
}

const STYLE: &str = "body{margin:0;background:#f2f4f6;color:#191f28;font:15px/1.6 -apple-system,BlinkMacSystemFont,'Apple SD Gothic Neo','Malgun Gothic',sans-serif}\
main{max-width:420px;margin:48px auto;padding:28px;background:#fff;border-radius:16px}\
h1{font-size:20px;margin:0 0 8px}p{margin:8px 0;color:#4e5968}\
ul{padding-left:18px;margin:8px 0 16px}li{margin:4px 0}\
.tag{display:inline-block;margin-left:6px;padding:0 6px;border-radius:6px;background:#fff0e0;color:#b25a00;font-size:12px;font-weight:600}\
label{display:block;margin:12px 0 4px;font-weight:600;font-size:14px}\
input{box-sizing:border-box;width:100%;padding:10px 12px;border:1px solid #d1d6db;border-radius:10px;font:inherit}\
.error{padding:10px 12px;border-radius:10px;background:#ffeeee;color:#d22030}\
.actions{display:flex;gap:8px;margin-top:20px}button{flex:1;padding:12px;border:0;border-radius:10px;font:inherit;font-weight:600;cursor:pointer}\
.approve{background:#2f6fe4;color:#fff}.deny{background:#f2f4f6;color:#333d4b}small{color:#8b95a1}";

fn consent_page(
    consent: &Consent,
    message: Option<&str>,
    needs_code: bool,
    login_id: &str,
) -> String {
    let scopes: String = consent
        .scopes
        .iter()
        .map(|scope| {
            let (label, risky) = oauth::scope_label(scope);
            let tag = if scope == SCOPE_ADMIN {
                "<span class=\"tag\">관리자만</span>"
            } else if risky {
                "<span class=\"tag\">변경</span>"
            } else {
                ""
            };
            format!("<li>{}{tag}</li>", esc(label))
        })
        .collect();
    let error = message
        .map(|m| format!("<p class=\"error\" role=\"alert\">{}</p>", esc(m)))
        .unwrap_or_default();
    let focus_password = if login_id.is_empty() {
        ""
    } else {
        " autofocus"
    };
    let code_hint = if needs_code {
        "인증 앱의 6자리 코드 또는 복구 코드"
    } else {
        "2단계 인증을 켰다면 인증 앱의 6자리 코드"
    };
    let admin_note = if consent.scopes.iter().any(|s| s == SCOPE_ADMIN) {
        "<p><small>관리자 작업 권한은 관리자 계정일 때만 주어져요.</small></p>"
    } else {
        ""
    };
    let read_only = consent.scopes.len() == 1 && consent.scopes[0] == SCOPE_READ;
    let scope_note = if read_only {
        "<p><small>조회만 요청했어요. 대신 실행하려면 AI 앱에서 변경 권한을 요청해야 해요.</small></p>"
    } else {
        ""
    };
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>그룹웨어 연결 승인</title>\
<style>{STYLE}</style></head><body><main>\
<h1>AI 앱이 그룹웨어에 연결하려고 해요</h1>\
<p>요청한 앱: <strong>{name}</strong> <small>(앱이 스스로 밝힌 이름)</small><br>승인하면 <strong>{host}</strong>(으)로 돌아가요.</p>\
<p>허락하면 이 앱이 내 계정으로 다음을 할 수 있어요.</p><ul>{scopes}</ul>{admin_note}{scope_note}\
<p class=\"error\"><strong>내가 직접 내 AI 앱에서 연결을 시작했을 때만 승인하세요.</strong> 다른 사람이 보낸 링크로 이 화면이 열렸다면 거부를 누르세요. 승인하면 그 앱의 주인이 내 계정으로 일할 수 있어요.</p>\
<p><small>내 계정의 권한을 넘는 일은 할 수 없고, 한 일은 내 이름으로 기록돼요. 연결은 내 정보 → 로그인된 기기에서 언제든 끊을 수 있어요.</small></p>\
{error}\
<form method=\"post\" action=\"/oauth/authorize\">\
<input type=\"hidden\" name=\"request_id\" value=\"{request}\">\
<label for=\"login_id\">아이디</label><input id=\"login_id\" name=\"login_id\" autocomplete=\"username\" required value=\"{login}\"{focus_login}>\
<label for=\"password\">비밀번호</label><input id=\"password\" name=\"password\" type=\"password\" autocomplete=\"current-password\" required{focus_password}>\
<label for=\"totp_code\">{code_hint}</label><input id=\"totp_code\" name=\"totp_code\" autocomplete=\"one-time-code\" inputmode=\"text\"{code_required}>\
<div class=\"actions\"><button class=\"deny\" name=\"action\" value=\"deny\" formnovalidate>거부</button>\
<button class=\"approve\" name=\"action\" value=\"approve\">승인</button></div>\
</form></main></body></html>",
        name = esc(&consent.client_name),
        host = esc(&consent.return_host),
        request = esc(&consent.request_id),
        login = esc(login_id),
        focus_login = if login_id.is_empty() { " autofocus" } else { "" },
        code_required = if needs_code { " required" } else { "" },
    )
}

fn message_page(message: &str) -> String {
    format!(
        "<!doctype html><html lang=\"ko\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>그룹웨어 연결</title>\
<style>{STYLE}</style></head><body><main><h1>연결할 수 없어요</h1><p>{}</p></main></body></html>",
        esc(message)
    )
}

/// 승인 POST의 출처 확인: 없으면 통과(오래된 브라우저), 있으면 issuer와 같아야 한다(`null` 거부).
fn origin_ok(headers: &HeaderMap, issuer: &str) -> bool {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(origin) => origin.eq_ignore_ascii_case(issuer),
    }
}

fn redirect(location: &str, clear_cookie: Option<String>) -> Response {
    let mut response = StatusCode::SEE_OTHER.into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(cookie) = clear_cookie.and_then(|c| HeaderValue::from_str(&c).ok()) {
        response.headers_mut().insert(header::SET_COOKIE, cookie);
    }
    response
}

fn cookie(state: &AppState, value: &str, max_age: i64) -> String {
    let secure = if issuer(state).starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    format!("{NONCE_COOKIE}={value}; Path=/oauth/authorize; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

async fn authorize(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    headers: HeaderMap,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    if !origin_ok(&headers, &issuer(&state)) {
        return html(
            &state,
            StatusCode::FORBIDDEN,
            message_page("허용되지 않은 요청입니다."),
            None,
        );
    }
    match oauth::start_authorize(&state, &params, meta.ip.as_deref()).await {
        Ok((consent, nonce)) => {
            let max_age = oauth::REQUEST_TTL.num_seconds();
            html(
                &state,
                StatusCode::OK,
                consent_page(&consent, None, false, ""),
                Some(cookie(&state, &nonce, max_age)),
            )
        }
        Err(AuthorizeFailure::Redirect(location)) => redirect(&location, None),
        Err(AuthorizeFailure::Page(message)) => html(
            &state,
            StatusCode::BAD_REQUEST,
            message_page(&message),
            None,
        ),
    }
}

#[derive(Deserialize)]
struct DecideForm {
    request_id: String,
    action: String,
    #[serde(default)]
    login_id: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    totp_code: String,
}

fn nonce_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == NONCE_COOKIE)
        .map(|(_, value)| value.to_owned())
}

async fn decide(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    headers: HeaderMap,
    Form(form): Form<DecideForm>,
) -> Response {
    if !origin_ok(&headers, &issuer(&state)) {
        return html(
            &state,
            StatusCode::FORBIDDEN,
            message_page("허용되지 않은 요청입니다."),
            None,
        );
    }
    let decision = if form.action == "deny" {
        Decision::Deny
    } else {
        Decision::Approve {
            login_id: form.login_id.clone(),
            password: form.password,
            totp_code: Some(form.totp_code),
        }
    };
    let nonce = nonce_from(&headers);
    match oauth::decide(&state, &form.request_id, nonce.as_deref(), decision, meta).await {
        Decided::Redirect(location) => redirect(&location, Some(cookie(&state, "", 0))),
        Decided::Retry {
            consent,
            message,
            needs_code,
        } => html(
            &state,
            StatusCode::OK,
            consent_page(&consent, Some(&message), needs_code, &form.login_id),
            None,
        ),
        Decided::Page(message) => html(
            &state,
            StatusCode::BAD_REQUEST,
            message_page(&message),
            None,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_page_escapes_self_claimed_names() {
        let consent = Consent {
            request_id: "abc".into(),
            client_name: "<script>alert(1)</script>".into(),
            return_host: "claude.ai".into(),
            scopes: vec![SCOPE_READ.into()],
        };
        let page = consent_page(&consent, Some("<b>x</b>"), false, "\"><img>");
        assert!(!page.contains("<script>alert"));
        assert!(!page.contains("<b>x</b>"));
        assert!(!page.contains("\"><img>"));
        assert!(page.contains("formnovalidate"));
    }
}
