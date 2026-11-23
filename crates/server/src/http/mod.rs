//! HTTP 계층: 라우터와 공통 미들웨어.

pub mod admin;
pub mod approval;
pub mod attendance;
pub mod auth;
pub mod calendar;
pub mod download;
pub mod extract;
pub mod home;
pub mod integrations;
pub mod leave;
pub mod mcp;
pub mod members;
pub mod notices;
pub mod notifications;
pub mod oauth;
pub mod org;
pub mod web;

use axum::extract::{DefaultBodyLimit, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use http::{header, HeaderName, HeaderValue, StatusCode};
use pango_api::API_PREFIX;
use pango_core::DomainError;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::error::ServiceError;
use crate::state::AppState;

/// 기능별 API 경로. 새 기능은 여기에 `merge` 한 줄을 추가한다.
fn api_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .merge(auth::routes())
        .merge(attendance::routes())
        .merge(org::routes())
        .merge(members::routes())
        .merge(notices::routes(state.config.max_attachment_bytes))
        .merge(notifications::routes())
        .merge(admin::routes())
        .merge(approval::routes(state.config.max_attachment_bytes))
        .merge(leave::routes())
        .merge(home::routes())
        .merge(calendar::routes())
        .merge(integrations::routes())
        .fallback(not_found)
}

async fn health(State(state): State<AppState>) -> Response {
    match state.db.ping().await {
        Ok(()) => Json(serde_json::json!({ "status": "ok" })).into_response(),
        Err(error) => {
            tracing::error!(%error, "health check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "status": "unavailable" })),
            )
                .into_response()
        }
    }
}

async fn not_found() -> ServiceError {
    ServiceError::Domain(DomainError::NotFound)
}

fn panic_response(_: Box<dyn std::any::Any + Send + 'static>) -> Response {
    ServiceError::internal("handler panicked").into_response()
}

fn static_header(name: &'static str, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::if_not_present(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    )
}

pub fn router(state: AppState) -> Router {
    let api = api_routes(&state)
        // API 응답은 개인 정보를 담으므로 어디에도 캐시하지 않는다.
        .layer(static_header("cache-control", "no-store"));
    // 원격 MCP(`/mcp`)는 늘 두고(꺼져 있으면 503), OAuth 경로는 켰을 때만 둔다.
    let mut root = Router::new().nest(API_PREFIX, api).merge(mcp::routes());
    if state.config.mcp.is_some() {
        root = root.merge(oauth::routes());
    }
    root.fallback(web::serve)
        .layer(DefaultBodyLimit::max(state.config.max_body_bytes))
        // 패닉 응답에도 아래의 보안 헤더가 붙도록 헤더 계층보다 안쪽에 둔다.
        .layer(CatchPanicLayer::custom(panic_response))
        .layer(static_header("x-content-type-options", "nosniff"))
        .layer(static_header("x-frame-options", "DENY"))
        .layer(static_header("referrer-policy", "no-referrer"))
        // 사내 그룹웨어는 어떤 검색 엔진에도 색인되지 않게 모든 응답에 noindex를 붙인다.
        .layer(static_header(
            "x-robots-tag",
            "noindex, nofollow, noarchive",
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
                 style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                 frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
            ),
        ))
        // 쿼리 문자열에는 몇 분짜리 내려받기 서명이 올 수 있으므로 로그에는 경로만 남긴다.
        .layer(TraceLayer::new_for_http().make_span_with(|request: &http::Request<_>| {
            tracing::debug_span!("request", method = %request.method(), path = %request.uri().path())
        }))
        .with_state(state)
}
