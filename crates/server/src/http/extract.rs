//! 요청 추출기. 거부 응답은 모두 [`ServiceError`]의 형식을 따른다.

use std::net::SocketAddr;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, FromRequest, FromRequestParts, Path, Query, Request};
use http::request::Parts;
use http::{header, HeaderMap, Method};
use pango_core::DomainError;
use pango_db::sessions::SessionVia;
use serde::de::DeserializeOwned;

use crate::error::ServiceError;
use crate::security::{fingerprint, SessionToken};
use crate::service::support::Idempotency;
use crate::service::{self, AuthContext, RequestMeta};
use crate::state::AppState;

/// 세션 쿠키 이름. `__Host-` 접두사는 Secure·Path=/·Domain 없음을 브라우저가 강제한다.
pub fn cookie_name(secure: bool) -> &'static str {
    if secure {
        "__Host-pango_session"
    } else {
        "pango_session"
    }
}

fn header_str(headers: &HeaderMap, name: impl header::AsHeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// 요청의 출처 정보를 읽는다.
pub fn request_meta(parts: &Parts, trust_proxy: bool) -> RequestMeta {
    let forwarded = trust_proxy
        .then(|| header_str(&parts.headers, "x-forwarded-for"))
        .flatten()
        // 프록시가 덧붙인 마지막 값만 믿는다. 앞쪽 값은 클라이언트가 위조할 수 있다.
        .and_then(|value| value.rsplit(',').next())
        .map(str::trim)
        .filter(|ip| ip.parse::<std::net::IpAddr>().is_ok())
        .map(str::to_owned);
    let direct = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip().to_string());
    RequestMeta {
        ip: forwarded.or(direct),
        user_agent: header_str(&parts.headers, header::USER_AGENT).map(str::to_owned),
    }
}

/// 요청에서 세션 토큰과 전달 방식을 찾는다. `Authorization` 헤더가 쿠키보다 우선한다.
fn session_token(headers: &HeaderMap, cookie: &str) -> Option<(SessionToken, SessionVia)> {
    if let Some(value) = header_str(headers, header::AUTHORIZATION) {
        let token = value
            .strip_prefix("Bearer ")
            .and_then(SessionToken::parse)?;
        return Some((token, SessionVia::Bearer));
    }
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == cookie)
        .and_then(|(_, value)| SessionToken::parse(value))
        .map(|token| (token, SessionVia::Cookie))
}

/// 브라우저가 보낸 요청이 같은 출처에서 왔는지 확인한다(CSRF 방어).
///
/// `Sec-Fetch-Site`가 있으면 그것을, 없으면 `Origin`을 `Host`와 비교한다.
/// `require_header`가 참이면 둘 다 없는 요청도 거부한다(쿠키 인증된 변경 요청).
pub fn check_same_origin(headers: &HeaderMap, require_header: bool) -> Result<(), ServiceError> {
    if let Some(site) = header_str(headers, "sec-fetch-site") {
        return if matches!(site, "same-origin" | "none") {
            Ok(())
        } else {
            Err(ServiceError::CrossOrigin)
        };
    }
    match header_str(headers, header::ORIGIN) {
        Some(origin) => {
            let origin_host = origin.split_once("://").map(|(_, host)| host);
            let host = header_str(headers, header::HOST);
            if origin_host.is_some() && origin_host == host {
                Ok(())
            } else {
                Err(ServiceError::CrossOrigin)
            }
        }
        None if require_header => Err(ServiceError::CrossOrigin),
        None => Ok(()),
    }
}

async fn authenticate(parts: &mut Parts, state: &AppState) -> Result<AuthContext, ServiceError> {
    let cookie = cookie_name(state.config.cookie_secure);
    let (token, via) = session_token(&parts.headers, cookie)
        .ok_or(ServiceError::Domain(DomainError::Unauthenticated))?;
    let safe = matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS);
    if via == SessionVia::Cookie && !safe {
        check_same_origin(&parts.headers, true)?;
    }
    let meta = request_meta(parts, state.config.trust_proxy);
    service::auth::authenticate(state, &token, meta).await
}

/// 인증된 요청. 임시 비밀번호 상태면 거부한다.
pub struct Auth(pub AuthContext);

impl FromRequestParts<AppState> for Auth {
    type Rejection = ServiceError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let ctx = authenticate(parts, state).await?;
        if ctx.member.must_change_password {
            return Err(ServiceError::PasswordChangeRequired);
        }
        Ok(Self(ctx))
    }
}

/// 인증만 확인한다. 임시 비밀번호 상태에서도 써야 하는 경로(내 정보, 비밀번호 변경, 로그아웃)용.
pub struct AuthPending(pub AuthContext);

impl FromRequestParts<AppState> for AuthPending {
    type Rejection = ServiceError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        authenticate(parts, state).await.map(Self)
    }
}

/// 인증 전 요청(로그인)의 출처 정보.
pub struct Meta(pub RequestMeta);

impl FromRequestParts<AppState> for Meta {
    type Rejection = ServiceError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        check_same_origin(&parts.headers, false)?;
        Ok(Self(request_meta(parts, state.config.trust_proxy)))
    }
}

fn is_json(headers: &HeaderMap) -> bool {
    header_str(headers, header::CONTENT_TYPE)
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
}

async fn read_json<T: DeserializeOwned>(
    request: Request,
    state: &AppState,
) -> Result<(T, Bytes), ServiceError> {
    if !is_json(request.headers()) {
        return Err(ServiceError::UnsupportedMediaType);
    }
    let bytes = Bytes::from_request(request, state)
        .await
        .map_err(|rejection| {
            if rejection.status() == http::StatusCode::PAYLOAD_TOO_LARGE {
                ServiceError::PayloadTooLarge
            } else {
                ServiceError::Malformed("unreadable body")
            }
        })?;
    let value =
        serde_json::from_slice(&bytes).map_err(|_| ServiceError::Malformed("invalid json"))?;
    Ok((value, bytes))
}

/// JSON 본문.
pub struct ApiJson<T>(pub T);

impl<T: DeserializeOwned> FromRequest<AppState> for ApiJson<T> {
    type Rejection = ServiceError;

    async fn from_request(request: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        read_json(request, state)
            .await
            .map(|(value, _)| Self(value))
    }
}

/// JSON 본문 + 선택적 멱등성 키(`Idempotency-Key` 헤더).
pub struct IdempotentJson<T> {
    pub body: T,
    pub idempotency: Option<Idempotency>,
}

impl<T: DeserializeOwned> FromRequest<AppState> for IdempotentJson<T> {
    type Rejection = ServiceError;

    async fn from_request(request: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        let key = match request.headers().get("idempotency-key") {
            None => None,
            Some(value) => {
                let key = value.to_str().ok().filter(|key| {
                    (1..=128).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_graphic())
                });
                Some(
                    key.ok_or(ServiceError::Malformed("invalid idempotency key"))?
                        .to_owned(),
                )
            }
        };
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        let (body, bytes) = read_json(request, state).await?;
        let idempotency = key.map(|key| Idempotency {
            key,
            fingerprint: fingerprint(&[method.as_str().as_bytes(), path.as_bytes(), &bytes]),
        });
        Ok(Self { body, idempotency })
    }
}

/// 경로 변수. 형식이 틀린 ID는 존재할 수 없는 자원이므로 404.
pub struct ApiPath<T>(pub T);

impl<T: DeserializeOwned + Send> FromRequestParts<AppState> for ApiPath<T> {
    type Rejection = ServiceError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|_| ServiceError::Domain(DomainError::NotFound))
    }
}

/// 쿼리 문자열.
pub struct ApiQuery<T>(pub T);

impl<T: DeserializeOwned> FromRequestParts<AppState> for ApiQuery<T> {
    type Rejection = ServiceError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|_| ServiceError::Malformed("invalid query string"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).expect("header value"));
        }
        map
    }

    fn token() -> SessionToken {
        SessionToken::generate().expect("token")
    }

    #[test]
    fn bearer_token_takes_precedence_over_cookie() {
        let (bearer, cookie) = (token(), token());
        let map = headers(&[
            ("authorization", &format!("Bearer {}", bearer.expose())),
            ("cookie", &format!("pango_session={}", cookie.expose())),
        ]);
        assert_eq!(
            session_token(&map, "pango_session"),
            Some((bearer, SessionVia::Bearer))
        );
    }

    #[test]
    fn malformed_authorization_header_does_not_fall_back_to_cookie() {
        let cookie = token();
        for auth in ["Basic abc", "Bearer", "Bearer not-a-token", "bearer x"] {
            let map = headers(&[
                ("authorization", auth),
                ("cookie", &format!("pango_session={}", cookie.expose())),
            ]);
            assert_eq!(session_token(&map, "pango_session"), None, "{auth}");
        }
    }

    #[test]
    fn cookie_is_found_among_others_and_exact_name_only() {
        let t = token();
        let map = headers(&[(
            "cookie",
            &format!("theme=dark; pango_session={}; x=1", t.expose()),
        )]);
        assert_eq!(
            session_token(&map, "pango_session"),
            Some((t.clone(), SessionVia::Cookie))
        );
        let lookalike = headers(&[("cookie", &format!("xpango_session={}", t.expose()))]);
        assert_eq!(session_token(&lookalike, "pango_session"), None);
        assert_eq!(session_token(&HeaderMap::new(), "pango_session"), None);
    }

    #[test]
    fn same_origin_check() {
        let ok = |pairs: &[(&'static str, &str)], required| {
            check_same_origin(&headers(pairs), required).is_ok()
        };
        assert!(ok(&[("sec-fetch-site", "same-origin")], true));
        assert!(ok(&[("sec-fetch-site", "none")], true));
        assert!(!ok(&[("sec-fetch-site", "cross-site")], true));
        assert!(
            !ok(&[("sec-fetch-site", "same-site")], true),
            "다른 서브도메인도 거부"
        );
        assert!(ok(
            &[
                ("origin", "https://erp.example.com"),
                ("host", "erp.example.com")
            ],
            true
        ));
        assert!(!ok(
            &[
                ("origin", "https://evil.example"),
                ("host", "erp.example.com")
            ],
            true
        ));
        assert!(!ok(
            &[
                ("origin", "https://erp.example.com.evil.io"),
                ("host", "erp.example.com")
            ],
            true
        ));
        assert!(!ok(
            &[("origin", "null"), ("host", "erp.example.com")],
            true
        ));
        assert!(!ok(
            &[
                ("origin", "https://erp.example.com:8443"),
                ("host", "erp.example.com")
            ],
            true
        ));
        assert!(!ok(&[], true), "헤더가 없으면 쿠키 인증 변경 요청은 거부");
        assert!(ok(&[], false));
        // Sec-Fetch-Site가 cross-site면 Origin이 맞아 보여도 거부.
        assert!(!ok(
            &[
                ("sec-fetch-site", "cross-site"),
                ("origin", "https://a.com"),
                ("host", "a.com")
            ],
            false
        ));
    }

    #[test]
    fn json_content_type_detection() {
        assert!(is_json(&headers(&[("content-type", "application/json")])));
        assert!(is_json(&headers(&[(
            "content-type",
            "Application/JSON; charset=utf-8"
        )])));
        assert!(!is_json(&headers(&[("content-type", "text/plain")])));
        assert!(!is_json(&headers(&[(
            "content-type",
            "application/x-www-form-urlencoded"
        )])));
        assert!(!is_json(&HeaderMap::new()));
    }
}
