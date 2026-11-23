//! 웹 UI 정적 파일. 번들은 실행 파일에 포함된다(디버그 빌드에서는 디스크에서 읽는다).

use std::collections::HashMap;
use std::io::Write;
use std::sync::{OnceLock, RwLock};

use axum::body::Bytes;
use axum::response::{IntoResponse, Response};
use flate2::write::GzEncoder;
use flate2::Compression;
use http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/web-dist"]
struct WebDist;

const INDEX: &str = "index.html";
/// 검색 엔진이 페이지의 `X-Robots-Tag: noindex`와 `<meta name="robots">`를 읽고
/// 색인에서 완전히 제외할 수 있도록 루트는 열어 두고 API·OAuth 경로만 막는다.
const ROBOTS_TXT: &str = "User-agent: *\nDisallow: /api/\nDisallow: /oauth/\n";
/// 내용 해시가 붙은(`?v=`) 자산은 URL이 바뀌기 전까지 다시 받지 않아도 된다.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
const REVALIDATE: &str = "no-cache";
/// 이보다 작은 파일은 gzip 헤더 오버헤드가 더 크므로 압축하지 않는다.
const MIN_COMPRESS_BYTES: usize = 256;

/// `(경로, 내용 SHA-256)` → gzip 압축본(원본보다 작을 때만 `Some`).
/// 번들에 들어 있는 정적 파일만 들어가므로 크기가 번들 파일 수로 제한된다.
type GzipCache = RwLock<HashMap<(String, [u8; 32]), Option<Bytes>>>;
static GZIP_CACHE: OnceLock<GzipCache> = OnceLock::new();

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        Some("json" | "webmanifest") => "application/json",
        _ => "application/octet-stream",
    }
}

/// 이미 압축된 형식(`woff2`, `png` 등)을 뺀 텍스트·WASM 정적 자산인지.
fn is_compressible(path: &str) -> bool {
    matches!(
        path.rsplit_once('.').map(|(_, ext)| ext),
        Some("html" | "js" | "css" | "wasm" | "svg" | "txt" | "json" | "webmanifest")
    )
}

/// `Accept-Encoding` 헤더가 gzip을 허용하는지(`q=0`은 거부로 본다).
fn accepts_gzip(headers: &HeaderMap) -> bool {
    let mut allowed = None;
    for value in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(raw) = value.to_str() else {
            continue;
        };
        for entry in raw.split(',') {
            let mut parts = entry.split(';');
            let coding = parts.next().unwrap_or_default().trim();
            if !(coding.eq_ignore_ascii_case("gzip") || coding.eq_ignore_ascii_case("x-gzip")) {
                continue;
            }
            let mut quality = 1.0_f32;
            for param in parts {
                if let Some((key, val)) = param.split_once('=') {
                    if key.trim().eq_ignore_ascii_case("q") {
                        quality = val.trim().parse::<f32>().unwrap_or(0.0);
                    }
                }
            }
            allowed = Some(quality > 0.0);
        }
    }
    allowed.unwrap_or(false)
}

fn gzip_bytes(raw: &[u8]) -> Option<Bytes> {
    if raw.len() < MIN_COMPRESS_BYTES {
        return None;
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(raw).ok()?;
    let compressed = encoder.finish().ok()?;
    (compressed.len() < raw.len()).then(|| Bytes::from(compressed))
}

fn gzip_cached(path: &str, hash: [u8; 32], raw: &[u8]) -> Option<Bytes> {
    let cache = GZIP_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    let key = (path.to_owned(), hash);
    if let Ok(guard) = cache.read() {
        if let Some(found) = guard.get(&key) {
            return found.clone();
        }
    }
    let compressed = gzip_bytes(raw);
    if let Ok(mut guard) = cache.write() {
        guard.insert(key, compressed.clone());
    }
    compressed
}

fn file_response(path: &str, cache: &'static str, gzip: bool) -> Option<Response> {
    let file = WebDist::get(path)?;
    let compressible = is_compressible(path);
    let mut response = if compressible && gzip {
        if let Some(compressed) = gzip_cached(path, file.metadata.sha256_hash(), &file.data) {
            let mut resp = compressed.into_response();
            resp.headers_mut()
                .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
            resp
        } else {
            file.data.into_owned().into_response()
        }
    } else {
        file.data.into_owned().into_response()
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if compressible {
        headers.insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
    }
    Some(response)
}

/// 자산의 캐시 정책. 내용 해시(`?v=`)가 붙었거나, 버전이 경로에 들어 있는 서체
/// (`fonts/<이름>-<버전>/`)는 바뀌지 않으므로 오래 캐시한다.
fn cache_policy(uri: &Uri) -> &'static str {
    let path = uri.path().trim_start_matches('/');
    let versioned = uri.query().is_some_and(|query| query.starts_with("v="))
        || path.starts_with("assets/fonts/");
    if versioned {
        IMMUTABLE
    } else {
        REVALIDATE
    }
}

/// API가 아닌 모든 요청을 처리한다.
///
/// - `/assets/…`: 해당 파일. 없으면 404.
/// - 그 외 경로: 화면 경로이므로 `index.html`을 돌려주고, 라우팅은 브라우저에서 한다.
pub async fn serve(method: Method, headers: HeaderMap, uri: Uri) -> Response {
    if !matches!(method, Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let gzip = accepts_gzip(&headers);
    let path = uri.path().trim_start_matches('/');
    if path == "robots.txt" {
        let mut response = ROBOTS_TXT.into_response();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(REVALIDATE));
        return response;
    }
    if path == "favicon.ico" {
        let cache = cache_policy(&uri);
        return file_response("assets/favicon.ico", cache, false)
            .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response());
    }
    if path.starts_with("assets/") {
        let cache = cache_policy(&uri);
        // 상위 경로 표기는 번들 밖을 가리키려는 시도이므로 없는 파일로 취급한다.
        let safe = !path
            .split('/')
            .any(|segment| segment == ".." || segment.is_empty());
        return safe
            .then(|| file_response(path, cache, gzip))
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
    file_response(INDEX, REVALIDATE, gzip).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use flate2::read::GzDecoder;

    use super::*;

    async fn get(uri: &'static str) -> Response {
        serve(Method::GET, HeaderMap::new(), Uri::from_static(uri)).await
    }

    async fn get_gzip(uri: &'static str) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("gzip, deflate, br"),
        );
        serve(Method::GET, headers, Uri::from_static(uri)).await
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
            assert_eq!(response.headers()[header::VARY], "Accept-Encoding");
            assert!(!response.headers().contains_key(header::CONTENT_ENCODING));
        }
    }

    #[tokio::test]
    async fn missing_assets_and_file_like_paths_are_404() {
        for uri in ["/assets/nope.js", "/assets/", "/.env", "/wp-login.php"] {
            assert_eq!(get(uri).await.status(), StatusCode::NOT_FOUND, "{uri}");
        }
    }

    #[tokio::test]
    async fn robots_txt_and_favicon_are_served() {
        let robots = get("/robots.txt").await;
        assert_eq!(robots.status(), StatusCode::OK);
        assert_eq!(
            robots.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        let favicon = get("/favicon.ico").await;
        if WebDist::get("assets/favicon.ico").is_some() {
            assert_eq!(favicon.status(), StatusCode::OK);
            assert_eq!(favicon.headers()[header::CONTENT_TYPE], "image/x-icon");
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
        let response = serve(Method::POST, HeaderMap::new(), Uri::from_static("/login")).await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn gzip_compresses_html_and_round_trips_to_original() {
        let plain = get("/").await;
        let raw = axum::body::to_bytes(plain.into_body(), 1024 * 1024)
            .await
            .expect("plain body");
        let zipped = get_gzip("/").await;
        assert_eq!(zipped.status(), StatusCode::OK);
        assert_eq!(zipped.headers()[header::VARY], "Accept-Encoding");
        let encoded = zipped
            .headers()
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = axum::body::to_bytes(zipped.into_body(), 1024 * 1024)
            .await
            .expect("gzip body");
        if raw.len() >= MIN_COMPRESS_BYTES {
            assert_eq!(encoded.as_deref(), Some("gzip"));
            assert!(body.len() < raw.len());
            let mut decoded = Vec::new();
            GzDecoder::new(&body[..])
                .read_to_end(&mut decoded)
                .expect("gunzip");
            assert_eq!(decoded, raw);
        } else {
            assert_eq!(body, raw);
        }
    }

    #[test]
    fn accept_encoding_parses_gzip_and_rejects_q_zero() {
        let parse = |raw: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::ACCEPT_ENCODING,
                HeaderValue::from_str(raw).expect("hdr"),
            );
            accepts_gzip(&headers)
        };
        assert!(!accepts_gzip(&HeaderMap::new()));
        assert!(parse("gzip"));
        assert!(parse("deflate, gzip, br"));
        assert!(parse("GZIP;q=0.8"));
        assert!(parse("x-gzip"));
        assert!(!parse("br, zstd"));
        assert!(!parse("gzip;q=0"));
        assert!(!parse("gzip; q=0.000, br;q=1"));
        assert!(!parse("*"));
    }

    #[test]
    fn gzip_bytes_skips_tiny_and_incompressible_inputs() {
        assert!(gzip_bytes(b"short").is_none());
        let repeated = "pango-groupware ".repeat(100);
        let zipped = gzip_bytes(repeated.as_bytes()).expect("compressible");
        assert!(zipped.len() < repeated.len());
        let mut decoded = String::new();
        GzDecoder::new(&zipped[..])
            .read_to_string(&mut decoded)
            .expect("gunzip");
        assert_eq!(decoded, repeated);
    }

    #[test]
    fn compressible_file_types_exclude_already_compressed_formats() {
        for yes in ["index.html", "assets/app_bg.wasm", "assets/app.js", "a.css"] {
            assert!(is_compressible(yes), "{yes}");
        }
        for no in [
            "assets/fonts/pretendard-1.3.9/a.woff2",
            "assets/logo.png",
            "favicon.ico",
            "noext",
        ] {
            assert!(!is_compressible(no), "{no}");
        }
    }

    #[test]
    fn font_files_are_cached_long_term() {
        assert_eq!(content_type("assets/fonts/x-1.0/a.woff2"), "font/woff2");
        assert_eq!(
            cache_policy(&Uri::from_static(
                "/assets/fonts/pretendard-1.3.9/pretendard.css"
            )),
            IMMUTABLE
        );
        assert_eq!(
            cache_policy(&Uri::from_static("/assets/app.js")),
            REVALIDATE
        );
        assert_eq!(
            cache_policy(&Uri::from_static("/assets/app.js?v=abc")),
            IMMUTABLE
        );
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
