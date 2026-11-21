//! 실행 환경마다 다른 부분(브라우저 WASM ↔ iOS·Android 앱).
//!
//! 화면 코드는 DOM·시계·저장소·HTTP를 직접 부르지 않고 이 모듈만 부른다.
//! - 브라우저: `web_sys`로 DOM을 바로 다루고, 같은 출처의 HttpOnly 쿠키로 인증한다.
//! - 앱: 화면은 시스템 WebView에 그려지고 Rust는 네이티브로 돈다. DOM은
//!   `dioxus::document::eval`로 다루고, HTTP는 Rust(reqwest)가 설정된 서버 주소로
//!   `Authorization: Bearer`를 붙여 보낸다.

pub mod server_url;

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub use web::*;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

/// HTTP 메서드.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// 서버 응답(상태 코드와 본문). 해석은 [`crate::api`]가 한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// 보낼 요청.
pub struct Request<'a> {
    pub method: Method,
    /// `/api/v1/...`처럼 `/`로 시작하는 경로.
    pub path: &'a str,
    pub json: Option<String>,
    pub idempotency_key: Option<&'a str>,
}

/// 서버에 닿지 못했다(오프라인, 주소 미설정, TLS 실패 등).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkError;

/// JS 문자열 리터럴. 요소 ID 등을 `eval` 스크립트에 넣을 때 쓴다(따옴표·역슬래시·줄바꿈 이스케이프).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub fn js_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_strings_cannot_break_out_of_the_literal() {
        assert_eq!(js_string("ff-title"), "\"ff-title\"");
        assert_eq!(js_string("a\");alert(1);//"), "\"a\\\");alert(1);//\"");
        assert_eq!(js_string("x\\\n</script>"), "\"x\\\\\\n</script>\"");
        assert_eq!(js_string(""), "\"\"");
    }
}
