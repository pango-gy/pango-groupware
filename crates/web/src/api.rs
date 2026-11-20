//! 서버 API 클라이언트.
//!
//! 웹에서는 같은 출처의 HttpOnly 쿠키로 인증한다(스크립트가 토큰을 볼 수 없다).
//! 모든 호출은 [`ApiError`]로 실패하며, 화면은 `ApiError::message`로 문구를 얻는다.

use gloo_net::http::{Method, RequestBuilder, Response};
use pango_api::{ErrorBody, FieldErrorDto, API_PREFIX};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// HTTP 상태. 서버에 닿지 못했으면 0.
    pub status: u16,
    pub code: String,
    pub field: Option<String>,
    pub reason: Option<String>,
    /// 한 요청에서 찾은 필드별 오류 전부(결재 양식의 값·필드 정의). 그 밖의 오류에서는 비어 있다.
    pub fields: Vec<FieldErrorDto>,
}

impl ApiError {
    fn network() -> Self {
        Self {
            status: 0,
            code: "network_error".into(),
            field: None,
            reason: None,
            fields: Vec::new(),
        }
    }

    fn unexpected(status: u16) -> Self {
        Self {
            status,
            code: "unexpected_response".into(),
            field: None,
            reason: None,
            fields: Vec::new(),
        }
    }

    pub fn is_unauthenticated(&self) -> bool {
        self.status == 401 && self.code == "unauthenticated"
    }

    /// 사용자에게 보여 줄 문구.
    pub fn message(&self) -> String {
        text::error_message(&self.code, self.field.as_deref(), self.reason.as_deref())
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

fn url(path: &str) -> String {
    format!("{API_PREFIX}{path}")
}

async fn decode<T: DeserializeOwned>(response: Response) -> ApiResult<T> {
    let status = response.status();
    if response.ok() {
        return response
            .json::<T>()
            .await
            .map_err(|_| ApiError::unexpected(status));
    }
    match response.json::<ErrorBody>().await {
        Ok(body) => Err(ApiError {
            status,
            code: body.error.code,
            field: body.error.field,
            reason: body.error.reason,
            fields: body.error.fields,
        }),
        Err(_) => Err(ApiError::unexpected(status)),
    }
}

async fn send<T: DeserializeOwned>(
    method: Method,
    path: &str,
    body: Option<String>,
    idempotency_key: Option<&str>,
) -> ApiResult<T> {
    let mut builder = RequestBuilder::new(&url(path))
        .method(method)
        .credentials(web_sys::RequestCredentials::SameOrigin);
    if let Some(key) = idempotency_key {
        builder = builder.header("Idempotency-Key", key);
    }
    let request = match body {
        Some(json) => builder
            .header("Content-Type", "application/json")
            .body(json),
        None => builder.build(),
    }
    .map_err(|_| ApiError::network())?;
    let result = match request.send().await {
        Ok(response) => decode(response).await,
        Err(_) => Err(ApiError::network()),
    };
    if let Err(error) = &result {
        // 세션 만료(401)·권한 변경(403)을 한곳에서 반영한다. 요청 future 안이라 렌더 중이 아니다.
        if let Some(session) = dioxus::prelude::try_consume_context::<crate::session::Session>() {
            session.observe(error);
        }
    }
    result
}

fn to_json<B: Serialize>(body: &B) -> ApiResult<Option<String>> {
    serde_json::to_string(body)
        .map(Some)
        .map_err(|_| ApiError::unexpected(0))
}

pub async fn get<T: DeserializeOwned>(path: &str) -> ApiResult<T> {
    send(Method::GET, path, None, None).await
}

pub async fn post<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> ApiResult<T> {
    send(Method::POST, path, to_json(body)?, None).await
}

/// 생성 요청. 같은 `key`로 다시 보내면 서버가 처음 응답을 그대로 돌려준다
/// (버튼을 두 번 누르거나 재시도해도 한 번만 만들어진다).
pub async fn post_once<B: Serialize, T: DeserializeOwned>(
    path: &str,
    body: &B,
    key: &str,
) -> ApiResult<T> {
    send(Method::POST, path, to_json(body)?, Some(key)).await
}

pub async fn patch<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> ApiResult<T> {
    send(Method::PATCH, path, to_json(body)?, None).await
}

pub async fn put<B: Serialize, T: DeserializeOwned>(path: &str, body: &B) -> ApiResult<T> {
    send(Method::PUT, path, to_json(body)?, None).await
}

pub async fn delete<T: DeserializeOwned>(path: &str) -> ApiResult<T> {
    send(Method::DELETE, path, None, None).await
}

/// 쿼리 문자열 값을 인코딩한다(RFC 3986 비예약 문자만 그대로 둔다).
/// 네이티브 테스트에서도 돌도록 순수 구현이다.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `(키, 값)` 목록을 `?a=1&b=2`로. 값은 인코딩하고, 목록이 비면 빈 문자열이다.
pub fn query_string(pairs: &[(&str, String)]) -> String {
    if pairs.is_empty() {
        return String::new();
    }
    let joined = pairs
        .iter()
        .map(|(key, value)| format!("{key}={}", encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("?{joined}")
}

/// 목록 경로에 다음 페이지 커서를 붙인다. 커서가 없거나 비었으면 그대로다.
pub fn with_cursor(mut path: String, cursor: Option<&str>) -> String {
    if let Some(cursor) = cursor.filter(|c| !c.is_empty()) {
        path.push(if path.contains('?') { '&' } else { '?' });
        path.push_str("cursor=");
        path.push_str(&encode(cursor));
    }
    path
}

/// 멱등성 키로 쓸 새 무작위 값. 폼을 열 때 하나 만들어 제출이 끝날 때까지 유지한다.
pub fn new_idempotency_key() -> String {
    pango_core::id::NotificationId::new().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_percent_encoded() {
        assert_eq!(encode("abc-_.~09"), "abc-_.~09");
        assert_eq!(encode("a&b=c d"), "a%26b%3Dc%20d");
        assert_eq!(encode("가"), "%EA%B0%80");
        assert_eq!(encode(""), "");
    }

    #[test]
    fn query_strings_skip_empty_lists() {
        assert_eq!(query_string(&[]), "");
        assert_eq!(
            query_string(&[("search", "홍 길동".into()), ("status", "active".into())]),
            "?search=%ED%99%8D%20%EA%B8%B8%EB%8F%99&status=active"
        );
    }

    #[test]
    fn cursor_is_appended_and_escaped() {
        assert_eq!(with_cursor("/a?limit=50".into(), None), "/a?limit=50");
        assert_eq!(with_cursor("/a?limit=50".into(), Some("")), "/a?limit=50");
        assert_eq!(
            with_cursor("/a?limit=50".into(), Some("ff")),
            "/a?limit=50&cursor=ff"
        );
        assert_eq!(
            with_cursor("/a".into(), Some("x&admin=1")),
            "/a?cursor=x%26admin%3D1"
        );
    }
}
