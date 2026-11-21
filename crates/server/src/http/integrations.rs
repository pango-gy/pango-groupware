//! 외부 알림 연동(관리자)과 내 알림 수신 설정 경로.

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use pango_api::integration::{
    BuzzLinkStart, BuzzLinkVerify, BuzzSettingsDto, BuzzSettingsInput, MemberBuzzDto, MyBuzzDto,
    NotificationPrefsDto, NotificationPrefsInput, WebhookSecretDto, WebhookSettingsDto,
    WebhookSettingsInput, WebhookTestResult,
};
use pango_api::{Empty, Versioned};

use pango_core::id::MemberId;

use super::extract::{ApiJson, ApiPath, Auth};
use crate::error::ServiceResult;
use crate::service::{buzz, integrations};
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
        .route(
            "/admin/integrations/buzz",
            get(buzz_settings).put(update_buzz_settings),
        )
        .route(
            "/admin/members/{id}/buzz",
            get(member_buzz).delete(admin_unlink_buzz),
        )
        .route(
            "/me/buzz",
            get(my_buzz).post(start_buzz_link).delete(unlink_my_buzz),
        )
        .route("/me/buzz/verify", post(verify_buzz_link))
}

async fn buzz_settings(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<BuzzSettingsDto>> {
    buzz::get_settings(&state, &ctx).await.map(Json)
}

async fn update_buzz_settings(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiJson(request): ApiJson<Versioned<BuzzSettingsInput>>,
) -> ServiceResult<Json<BuzzSettingsDto>> {
    buzz::update_settings(&state, &ctx, &request)
        .await
        .map(Json)
}

async fn member_buzz(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
) -> ServiceResult<Json<MemberBuzzDto>> {
    buzz::member_link(&state, &ctx, id).await.map(Json)
}

async fn admin_unlink_buzz(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
) -> ServiceResult<Json<MemberBuzzDto>> {
    buzz::admin_unlink(&state, &ctx, id).await.map(Json)
}

async fn my_buzz(State(state): State<AppState>, Auth(ctx): Auth) -> ServiceResult<Json<MyBuzzDto>> {
    buzz::my_link(&state, &ctx).await.map(Json)
}

async fn start_buzz_link(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiJson(input): ApiJson<BuzzLinkStart>,
) -> ServiceResult<Json<MyBuzzDto>> {
    buzz::start_link(&state, &ctx, &input).await.map(Json)
}

async fn verify_buzz_link(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiJson(input): ApiJson<BuzzLinkVerify>,
) -> ServiceResult<Json<MyBuzzDto>> {
    buzz::verify_link(&state, &ctx, &input).await.map(Json)
}

async fn unlink_my_buzz(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<MyBuzzDto>> {
    buzz::unlink_mine(&state, &ctx).await.map(Json)
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
