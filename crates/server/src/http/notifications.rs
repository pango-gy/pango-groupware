//! 앱 내 알림 경로.

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use pango_api::notification::{NotificationDto, NotificationQuery, ReadAllResult, UnreadCount};
use pango_api::{Empty, Page};
use pango_core::id::NotificationId;

use super::extract::{ApiPath, ApiQuery, Auth};
use crate::error::ServiceResult;
use crate::service::notifications;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/notifications", get(list))
        .route("/notifications/unread-count", get(unread_count))
        .route("/notifications/read-all", post(mark_all_read))
        .route("/notifications/{id}/read", post(mark_read))
}

async fn list(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<NotificationQuery>,
) -> ServiceResult<Json<Page<NotificationDto>>> {
    notifications::list(&state, &ctx, &query).await.map(Json)
}

async fn unread_count(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<UnreadCount>> {
    notifications::unread_count(&state, &ctx).await.map(Json)
}

async fn mark_read(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NotificationId>,
) -> ServiceResult<Json<Empty>> {
    notifications::mark_read(&state, &ctx, id).await?;
    Ok(Json(Empty {}))
}

async fn mark_all_read(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<ReadAllResult>> {
    notifications::mark_all_read(&state, &ctx).await.map(Json)
}
