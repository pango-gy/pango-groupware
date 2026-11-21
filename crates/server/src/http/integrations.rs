//! 외부 알림 연동(관리자)과 내 알림 수신 설정 경로.

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use pango_api::integration::{
    NotificationPrefsDto, NotificationPrefsInput, WebhookSecretDto, WebhookSettingsDto,
    WebhookSettingsInput, WebhookTestResult,
};
use pango_api::{Empty, Versioned};

use super::extract::{ApiJson, Auth};
use crate::error::ServiceResult;
use crate::service::integrations;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/admin/integrations/webhook",
            get(get_webhook).put(update_webhook),
        )
        .route("/admin/integrations/webhook/secret", post(rotate_secret))
        .route("/admin/integrations/webhook/test", post(test_webhook))
        .route(
            "/me/notification-preferences",
            get(my_prefs).put(update_my_prefs),
        )
}

async fn get_webhook(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<WebhookSettingsDto>> {
    integrations::get_webhook(&state, &ctx).await.map(Json)
}

async fn update_webhook(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiJson(request): ApiJson<Versioned<WebhookSettingsInput>>,
) -> ServiceResult<Json<WebhookSettingsDto>> {
    integrations::update_webhook(&state, &ctx, &request)
        .await
        .map(Json)
}

async fn rotate_secret(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiJson(request): ApiJson<Versioned<Empty>>,
) -> ServiceResult<Json<WebhookSecretDto>> {
    integrations::rotate_webhook_secret(&state, &ctx, &request)
        .await
        .map(Json)
}

async fn test_webhook(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<WebhookTestResult>> {
    integrations::test_webhook(&state, &ctx).await.map(Json)
}

async fn my_prefs(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<NotificationPrefsDto>> {
    integrations::my_prefs(&state, &ctx).await.map(Json)
}

async fn update_my_prefs(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiJson(input): ApiJson<NotificationPrefsInput>,
) -> ServiceResult<Json<NotificationPrefsDto>> {
    integrations::update_my_prefs(&state, &ctx, &input)
        .await
        .map(Json)
}
