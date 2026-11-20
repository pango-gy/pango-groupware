//! 웹 UI 정적 파일. 번들은 실행 파일에 포함된다(디버그 빌드에서는 디스크에서 읽는다).

use axum::response::{IntoResponse, Response};
use http::{header, HeaderValue, Method, StatusCode, Uri};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/web-dist"]
struct WebDist;

const INDEX: &str = "index.html";
/// 내용 해시가 붙은(`?v=`) 자산은 URL이 바뀌기 전까지 다시 받지 않아도 된다.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "no-cache";

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json" | "webmanifest") => "application/json",
        _ => "application/octet-stream",
    }
}

fn file_response(path: &str, cache: &'static str) -> Option<Response> {
    let file = WebDist::get(path)?;
    let headers = [
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static(content_type(path)),
        ),
        (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
    ];
    Some((headers, file.data.into_owned()).into_response())
}

/// API가 아닌 모든 요청을 처리한다.
///
/// - `/assets/…`: 해당 파일. 없으면 404.
/// - 그 외 경로: 화면 경로이므로 `index.html`을 돌려주고, 라우팅은 브라우저에서 한다.
pub async fn serve(method: Method, uri: Uri) -> Response {
    if !matches!(method, Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("assets/") {
        let versioned = uri.query().is_some_and(|query| query.starts_with("v="));
        let cache = if versioned { IMMUTABLE } else { REVALIDATE };
        // 상위 경로 표기는 번들 밖을 가리키려는 시도이므로 없는 파일로 취급한다.
        let safe = !path
            .split('/')
            .any(|segment| segment == ".." || segment.is_empty());
        return safe
            .then(|| file_response(path, cache))
            .flatten()
            .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response());
    }
    // 파일처럼 보이는 경로(확장자 있음)는 화면이 아니다: 잘못된 주소를 index로 가리지 않는다.
    if path
        .rsplit('/')
        .next()
        .is_some_and(|last| last.contains('.'))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    file_response(INDEX, REVALIDATE).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(uri: &'static str) -> Response {
        serve(Method::GET, Uri::from_static(uri)).await
    }

    #[tokio::test]
    async fn screen_paths_get_the_app_shell_without_caching() {
        for uri in [
            "/",
            "/login",
            "/approval/documents/123",
            "/admin/members?x=1",
        ] {
            let response = get(uri).await;
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/html; charset=utf-8"
            );
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
        }
    }

    #[tokio::test]
    async fn missing_assets_and_file_like_paths_are_404() {
        for uri in [
            "/assets/nope.js",
            "/favicon.ico",
            "/robots.txt",
            "/assets/",
            "/.env",
            "/wp-login.php",
        ] {
            assert_eq!(get(uri).await.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[tokio::test]
    async fn traversal_attempts_never_leave_the_bundle() {
        for uri in [
            "/assets/../index.html",
            "/assets/../../Cargo.toml",
            "/assets/%2e%2e/%2e%2e/Cargo.toml",
            "/assets//etc/passwd",
            "/assets/..%2f..%2fCargo.toml",
        ] {
            assert_eq!(get(uri).await.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[tokio::test]
    async fn non_read_methods_are_rejected() {
        let response = serve(Method::POST, Uri::from_static("/login")).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn content_types() {
        assert_eq!(content_type("assets/app_bg.wasm"), "application/wasm");
        assert_eq!(
            content_type("assets/app.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type("noext"), "application/octet-stream");
    }
}
