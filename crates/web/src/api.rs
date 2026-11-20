//! 서버 API 클라이언트.
//!
//! 웹에서는 같은 출처의 HttpOnly 쿠키로 인증한다(스크립트가 토큰을 볼 수 없다).
//! 모든 호출은 [`ApiError`]로 실패하며, 화면은 `ApiError::message`로 문구를 얻는다.

use gloo_net::http::{Method, RequestBuilder, Response};
use pango_api::{ErrorBody, API_PREFIX};
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
}

impl ApiError {
    fn network() -> Self {
        Self {
            status: 0,
            code: "network_error".into(),
            field: None,
            reason: None,
        }
    }

    fn unexpected(status: u16) -> Self {
        Self {
            status,
            code: "unexpected_response".into(),
            field: None,
            reason: None,
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
    let response = request.send().await.map_err(|_| ApiError::network())?;
    decode(response).await
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

/// 쿼리 문자열 값을 인코딩한다.
pub fn encode(value: &str) -> String {
    js_sys::encode_uri_component(value).into()
}

/// 멱등성 키로 쓸 새 무작위 값. 폼을 열 때 하나 만들어 제출이 끝날 때까지 유지한다.
pub fn new_idempotency_key() -> String {
    pango_core::id::NotificationId::new().to_string()
}
