//! 인증·내 계정 경로.

use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use http::{header, HeaderValue};
use pango_api::auth::{ChangePasswordRequest, LoginRequest, LoginResponse, Me, SessionInfo};
use pango_api::Empty;
use pango_core::id::SessionId;
use pango_db::sessions::SessionVia;

use super::extract::{cookie_name, ApiJson, ApiPath, Auth, AuthPending, Meta};
use crate::error::{ServiceError, ServiceResult};
use crate::service::auth;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
        .route("/me/password", post(change_password))
        .route("/me/sessions", get(list_sessions))
        .route("/me/sessions/{id}", delete(revoke_session))
        .route("/download-links", post(download_link))
        .route("/me/totp", get(totp_status))
        .route("/me/totp/setup", post(totp_setup))
        .route("/me/totp/confirm", post(totp_confirm))
        .route("/me/totp/disable", post(totp_disable))
}

async fn totp_status(
    axum::extract::State(state): axum::extract::State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<pango_api::auth::TotpStatus>> {
    crate::service::totp::status(&state, &ctx).await.map(Json)
}

async fn totp_setup(
    axum::extract::State(state): axum::extract::State<AppState>,
    Auth(ctx): Auth,
    ApiJson(request): ApiJson<pango_api::auth::TotpSetupRequest>,
) -> ServiceResult<Json<pango_api::auth::TotpSetup>> {
    crate::service::totp::setup(&state, &ctx, &request)
        .await
        .map(Json)
}

async fn totp_confirm(
    axum::extract::State(state): axum::extract::State<AppState>,
    Auth(ctx): Auth,
    ApiJson(request): ApiJson<pango_api::auth::TotpConfirmRequest>,
) -> ServiceResult<Json<pango_api::auth::RecoveryCodes>> {
    crate::service::totp::confirm(&state, &ctx, &request)
        .await
        .map(Json)
}

async fn totp_disable(
    axum::extract::State(state): axum::extract::State<AppState>,
    Auth(ctx): Auth,
    ApiJson(request): ApiJson<pango_api::auth::TotpDisableRequest>,
) -> ServiceResult<Json<pango_api::auth::TotpStatus>> {
    crate::service::totp::disable(&state, &ctx, &request)
        .await
        .map(Json)
}

async fn download_link(
    axum::extract::State(state): axum::extract::State<AppState>,
    Auth(ctx): Auth,
    ApiJson(request): ApiJson<pango_api::auth::DownloadLinkRequest>,
) -> ServiceResult<Json<pango_api::auth::DownloadLink>> {
    auth::download_link(&state, &ctx, &request.path).map(Json)
}

fn session_cookie(state: &AppState, value: &str, max_age_secs: i64) -> ServiceResult<HeaderValue> {
    let secure = state.config.cookie_secure;
    let cookie = format!(
        "{}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_secs}{}",
        cookie_name(secure),
        if secure { "; Secure" } else { "" },
    );
    HeaderValue::from_str(&cookie).map_err(ServiceError::internal)
}

async fn login(
    axum::extract::State(state): axum::extract::State<AppState>,
    Meta(meta): Meta,
    ApiJson(request): ApiJson<LoginRequest>,
) -> ServiceResult<Response> {
    let outcome = auth::login(&state, &request, meta).await?;
    Ok(match outcome.via {
        SessionVia::Bearer => {
            let token = Some(outcome.token.expose().to_owned());
            Json(LoginResponse {
                token,
                me: outcome.me,
            })
            .into_response()
        }
        SessionVia::Cookie => {
            let cookie = session_cookie(&state, outcome.token.expose(), outcome.max_age_secs)?;
            let body = Json(LoginResponse {
                token: None,
                me: outcome.me,
            });
            ([(header::SET_COOKIE, cookie)], body).into_response()
        }
    })
}

async fn logout(
    axum::extract::State(state): axum::extract::State<AppState>,
    AuthPending(ctx): AuthPending,
) -> ServiceResult<Response> {
    auth::logout(&state, &ctx).await?;
    let clear = session_cookie(&state, "", 0)?;
    Ok(([(header::SET_COOKIE, clear)], Json(Empty {})).into_response())
}

async fn me(
    axum::extract::State(state): axum::extract::State<AppState>,
    AuthPending(ctx): AuthPending,
) -> ServiceResult<Json<Me>> {
    auth::me(&state, &ctx).await.map(Json)
}

async fn change_password(
    axum::extract::State(state): axum::extract::State<AppState>,
    AuthPending(ctx): AuthPending,
    ApiJson(request): ApiJson<ChangePasswordRequest>,
) -> ServiceResult<Json<Empty>> {
    auth::change_password(&state, &ctx, &request).await?;
    Ok(Json(Empty {}))
}

async fn list_sessions(
    axum::extract::State(state): axum::extract::State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<Vec<SessionInfo>>> {
    auth::list_sessions(&state, &ctx).await.map(Json)
}

async fn revoke_session(
    axum::extract::State(state): axum::extract::State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<SessionId>,
) -> ServiceResult<Json<Empty>> {
    auth::revoke_session(&state, &ctx, id).await?;
    Ok(Json(Empty {}))
}
