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

/// `X-Forwarded-For`의 한 항목을 IP로 읽는다. `1.2.3.4:5678`, `[::1]:443` 같은 포트 표기도 받는다.
fn parse_forwarded_ip(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let ip = raw
        .parse::<std::net::IpAddr>()
        .ok()
        .or_else(|| raw.parse::<SocketAddr>().ok().map(|addr| addr.ip()));
    if ip.is_none() {
        tracing::warn!("unparseable X-Forwarded-For entry; falling back to the peer address");
    }
    ip.map(|ip| ip.to_string())
}

/// 요청의 출처 정보를 읽는다.
pub fn request_meta(parts: &Parts, trust_proxy: bool) -> RequestMeta {
    let forwarded = trust_proxy
        .then(|| header_str(&parts.headers, "x-forwarded-for"))
        .flatten()
        // 프록시가 덧붙인 마지막 값만 믿는다. 앞쪽 값은 클라이언트가 위조할 수 있다.
        .and_then(|value| value.rsplit(',').next())
        .and_then(parse_forwarded_ip);
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

/// 요청이 어느 주소로 들어왔는지. 출처 검사에 쓴다.
pub struct Target<'a> {
    /// 설정된 공개 주소(`https://erp.example.com`). 있으면 이것만 기준으로 삼는다.
    pub public_origin: Option<&'a str>,
    /// `Host` 헤더 또는 (HTTP/2에서는) 요청 URI의 authority.
    pub host: Option<&'a str>,
}

impl<'a> Target<'a> {
    pub fn of(parts: &'a Parts, public_origin: Option<&'a str>) -> Self {
        let host = header_str(&parts.headers, header::HOST)
            .or_else(|| parts.uri.authority().map(|authority| authority.as_str()));
        Self {
            public_origin,
            host,
        }
    }

    fn matches(&self, origin: &str) -> bool {
        match self.public_origin {
            // 스킴·호스트·포트를 모두 비교한다.
            Some(expected) => origin.eq_ignore_ascii_case(expected.trim_end_matches('/')),
            None => match (origin.split_once("://"), self.host) {
                (Some((_, origin_host)), Some(host)) => origin_host.eq_ignore_ascii_case(host),
                _ => false,
            },
        }
    }
}

/// 브라우저가 보낸 요청이 같은 출처에서 왔는지 확인한다(CSRF 방어).
///
/// `Sec-Fetch-Site`가 있으면 그것을, 없으면 `Origin`을 요청 대상과 비교한다.
/// `require_header`가 참이면 둘 다 없는 요청도 거부한다(쿠키 인증된 변경 요청).
pub fn check_same_origin(
    headers: &HeaderMap,
    target: &Target<'_>,
    require_header: bool,
) -> Result<(), ServiceError> {
    if let Some(site) = header_str(headers, "sec-fetch-site") {
        return if matches!(site, "same-origin" | "none") {
            Ok(())
        } else {
            Err(ServiceError::CrossOrigin)
        };
    }
    match header_str(headers, header::ORIGIN) {
        Some(origin) if target.matches(origin) => Ok(()),
        Some(_) => Err(ServiceError::CrossOrigin),
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
        let target = Target::of(parts, state.config.public_origin.as_deref());
        check_same_origin(&parts.headers, &target, true)?;
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
        let target = Target::of(parts, state.config.public_origin.as_deref());
        check_same_origin(&parts.headers, &target, false)?;
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

    fn same_origin(
        pairs: &[(&'static str, &str)],
        public_origin: Option<&str>,
        required: bool,
    ) -> bool {
        let map = headers(pairs);
        let target = Target {
            public_origin,
            host: header_str(&map, header::HOST),
        };
        check_same_origin(&map, &target, required).is_ok()
    }

    #[test]
    fn sec_fetch_site_decides_when_present() {
        let ok = |pairs: &[(&'static str, &str)]| same_origin(pairs, None, true);
        assert!(ok(&[("sec-fetch-site", "same-origin")]));
        assert!(ok(&[("sec-fetch-site", "none")]));
        assert!(!ok(&[("sec-fetch-site", "cross-site")]));
        assert!(
            !ok(&[("sec-fetch-site", "same-site")]),
            "다른 서브도메인도 거부"
        );
        // cross-site면 Origin이 맞아 보여도 거부.
        assert!(!ok(&[
            ("sec-fetch-site", "cross-site"),
            ("origin", "https://a.com"),
            ("host", "a.com")
        ]));
    }

    #[test]
    fn origin_is_compared_with_the_request_host() {
        let ok = |pairs: &[(&'static str, &str)], required| same_origin(pairs, None, required);
        assert!(ok(
            &[
                ("origin", "https://erp.example.com"),
                ("host", "erp.example.com")
            ],
            true
        ));
        assert!(
            ok(
                &[
                    ("origin", "https://ERP.example.com"),
                    ("host", "erp.Example.com")
                ],
                true
            ),
            "대소문자 무시"
        );
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
        assert!(
            !ok(&[("origin", "https://erp.example.com")], true),
            "비교할 호스트가 없으면 거부"
        );
        assert!(!ok(&[], true), "헤더가 없으면 쿠키 인증 변경 요청은 거부");
        assert!(ok(&[], false));
    }

    #[test]
    fn configured_public_origin_is_matched_exactly_including_scheme() {
        let public = Some("https://erp.example.com/");
        let ok = |pairs: &[(&'static str, &str)]| same_origin(pairs, public, true);
        // 프록시가 Host를 바꿔도 공개 주소와 맞으면 통과한다.
        assert!(ok(&[
            ("origin", "https://erp.example.com"),
            ("host", "127.0.0.1:8080")
        ]));
        assert!(ok(&[("origin", "HTTPS://ERP.EXAMPLE.COM")]));
        assert!(
            !ok(&[("origin", "http://erp.example.com")]),
            "스킴이 다르면 거부"
        );
        assert!(!ok(&[("origin", "https://erp.example.com:8443")]));
        // Host만 맞는 것으로는 부족하다.
        assert!(!ok(&[
            ("origin", "https://internal.local"),
            ("host", "internal.local")
        ]));
    }

    #[test]
    fn forwarded_ip_accepts_port_forms_and_rejects_garbage() {
        assert_eq!(
            parse_forwarded_ip(" 203.0.113.9 ").as_deref(),
            Some("203.0.113.9")
        );
        assert_eq!(
            parse_forwarded_ip("203.0.113.9:4711").as_deref(),
            Some("203.0.113.9")
        );
        assert_eq!(
            parse_forwarded_ip("[2001:db8::1]:443").as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(
            parse_forwarded_ip("2001:db8::1").as_deref(),
            Some("2001:db8::1")
        );
        for bad in ["", "unknown", "example.com", "1.2.3.4.5", "' OR 1=1"] {
            assert_eq!(parse_forwarded_ip(bad), None, "{bad:?}");
        }
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
