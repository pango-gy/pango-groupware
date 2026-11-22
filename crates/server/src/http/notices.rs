//! 공지사항 경로(구성원용 + 관리자용).

use axum::extract::{DefaultBodyLimit, State};
use axum::response::Response;
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use http::StatusCode;
use pango_api::common::VersionQuery;
use pango_api::notice::{
    NoticeDetail, NoticeInput, NoticeReadReceipt, NoticeReadReport, NoticeSummary,
    NoticeUnreadCount,
};
use pango_api::{Page, PageQuery, Versioned};
use pango_core::id::{AttachmentId, NoticeId};

use super::extract::{ApiJson, ApiPath, ApiQuery, Auth, DownloadAuth, FileUpload, IdempotentJson};
use crate::error::ServiceResult;
use crate::service::notices;
use crate::state::AppState;

/// `max_attachment_bytes`: 첨부 올리기 경로의 본문 상한(다른 경로는 공통 상한을 쓴다).
pub fn routes(max_attachment_bytes: usize) -> Router<AppState> {
    Router::new()
        .route("/notices", get(list))
        .route("/notices/unread-count", get(unread_count))
        .route("/notices/{id}", get(detail))
        .route("/notices/{id}/read", post(mark_read))
        .route(
            "/notices/{id}/attachments/{attachment_id}",
            get(download_attachment),
        )
        .route("/admin/notices", get(admin_list).post(create))
        .route("/admin/notices/{id}", patch(update).delete(archive))
        .route("/admin/notices/{id}/reads", get(read_report))
        .route(
            "/admin/notices/{id}/attachments",
            post(add_attachment).layer(DefaultBodyLimit::max(max_attachment_bytes)),
        )
        .route(
            "/admin/notices/{id}/attachments/{attachment_id}",
            delete(remove_attachment),
        )
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

async fn add_attachment(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
    upload: FileUpload,
) -> ServiceResult<(StatusCode, Json<pango_api::attachment::Attachment>)> {
    let created = notices::add_attachment(
        &state,
        &ctx,
        id,
        &upload.filename,
        &upload.data,
        upload.idempotency.as_ref(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn remove_attachment(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath((id, attachment_id)): ApiPath<(NoticeId, AttachmentId)>,
) -> ServiceResult<StatusCode> {
    notices::remove_attachment(&state, &ctx, id, attachment_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn download_attachment(
    State(state): State<AppState>,
    DownloadAuth(ctx): DownloadAuth,
    ApiPath((id, attachment_id)): ApiPath<(NoticeId, AttachmentId)>,
) -> ServiceResult<Response> {
    let file = notices::download_attachment(&state, &ctx, id, attachment_id).await?;
    super::download::attachment_response(file)
}
