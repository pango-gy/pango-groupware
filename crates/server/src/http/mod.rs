//! HTTP 계층: 라우터와 공통 미들웨어.

pub mod admin;
pub mod attendance;
pub mod auth;
pub mod extract;
pub mod members;
pub mod notices;
pub mod notifications;
pub mod org;
pub mod web;

use axum::extract::DefaultBodyLimit;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use http::{header, HeaderName, HeaderValue};
use pango_api::API_PREFIX;
use pango_core::DomainError;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;

use crate::error::ServiceError;
use crate::state::AppState;

/// 기능별 API 경로. 새 기능은 여기에 `merge` 한 줄을 추가한다.
fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .merge(auth::routes())
        .merge(attendance::routes())
        .merge(org::routes())
        .merge(members::routes())
        .merge(notices::routes())
        .merge(notifications::routes())
        .merge(admin::routes())
        .fallback(not_found)
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok" }))
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
    let api = api_routes()
        // API 응답은 개인 정보를 담으므로 어디에도 캐시하지 않는다.
        .layer(static_header("cache-control", "no-store"));
    Router::new()
        .nest(API_PREFIX, api)
        .fallback(web::serve)
        .layer(DefaultBodyLimit::max(state.config.max_body_bytes))
        .layer(static_header("x-content-type-options", "nosniff"))
        .layer(static_header("x-frame-options", "DENY"))
        .layer(static_header("referrer-policy", "no-referrer"))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
                 style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                 frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
            ),
        ))
        .layer(CatchPanicLayer::custom(panic_response))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}
