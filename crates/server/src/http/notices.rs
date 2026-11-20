//! 공지사항 경로(구성원용 + 관리자용).

use axum::extract::State;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use http::StatusCode;
use pango_api::common::VersionQuery;
use pango_api::notice::{
    NoticeDetail, NoticeInput, NoticeReadReceipt, NoticeReadReport, NoticeSummary,
    NoticeUnreadCount,
};
use pango_api::{Page, PageQuery, Versioned};
use pango_core::id::NoticeId;

use super::extract::{ApiJson, ApiPath, ApiQuery, Auth, IdempotentJson};
use crate::error::ServiceResult;
use crate::service::notices;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/notices", get(list))
        .route("/notices/unread-count", get(unread_count))
        .route("/notices/{id}", get(detail))
        .route("/notices/{id}/read", post(mark_read))
        .route("/admin/notices", get(admin_list).post(create))
        .route("/admin/notices/{id}", patch(update).delete(archive))
        .route("/admin/notices/{id}/reads", get(read_report))
}

async fn list(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> ServiceResult<Json<Page<NoticeSummary>>> {
    notices::list(&state, &ctx, &query).await.map(Json)
}

async fn unread_count(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<NoticeUnreadCount>> {
    notices::unread_count(&state, &ctx).await.map(Json)
}

async fn detail(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
) -> ServiceResult<Json<NoticeDetail>> {
    notices::get(&state, &ctx, id).await.map(Json)
}

async fn mark_read(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
) -> ServiceResult<Json<NoticeReadReceipt>> {
    notices::mark_read(&state, &ctx, id).await.map(Json)
}

async fn admin_list(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> ServiceResult<Json<Page<NoticeSummary>>> {
    notices::admin_list(&state, &ctx, &query).await.map(Json)
}

async fn create(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<NoticeInput>,
) -> ServiceResult<(StatusCode, Json<NoticeDetail>)> {
    let notice = notices::create(&state, &ctx, &request.body, request.idempotency.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(notice)))
}

async fn update(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
    ApiJson(request): ApiJson<Versioned<NoticeInput>>,
) -> ServiceResult<Json<NoticeDetail>> {
    notices::update(&state, &ctx, id, &request).await.map(Json)
}

async fn archive(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
    ApiQuery(query): ApiQuery<VersionQuery>,
) -> ServiceResult<Json<NoticeDetail>> {
    notices::archive(&state, &ctx, id, query.version)
        .await
        .map(Json)
}

async fn read_report(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
) -> ServiceResult<Json<NoticeReadReport>> {
    notices::read_report(&state, &ctx, id).await.map(Json)
}
