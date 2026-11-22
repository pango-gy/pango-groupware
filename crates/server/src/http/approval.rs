//! 전자결재 경로.

use axum::extract::{DefaultBodyLimit, State};
use axum::response::Response;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use http::StatusCode;
use pango_api::approval::{
    AdminDocumentQuery, ApprovalCounts, ApproveRequest, DocumentDetail, DocumentQuery,
    DocumentSummary, EditDocumentRequest, EditFormRequest, FormDto, FormGroup, LineSuggestion,
    NewDocumentRequest, NewFormRequest, RejectRequest,
};
use pango_api::{Empty, Page};
use pango_core::id::{AttachmentId, DocumentId, FormId};

use super::extract::{ApiJson, ApiPath, ApiQuery, Auth, DownloadAuth, FileUpload, IdempotentJson};
use crate::error::ServiceResult;
use crate::service::approval;
use crate::state::AppState;

/// `max_attachment_bytes`: 첨부 올리기 경로의 본문 상한(다른 경로는 공통 상한을 쓴다).
pub fn routes(max_attachment_bytes: usize) -> Router<AppState> {
    Router::new()
        .route("/approval/forms", get(list_forms))
        .route("/approval/counts", get(counts))
        .route("/approval/line-suggestion", get(line_suggestion))
        .route(
            "/approval/documents",
            get(list_documents).post(create_document),
        )
        .route(
            "/approval/documents/{id}",
            get(get_document)
                .patch(update_document)
                .delete(delete_document),
        )
        .route("/approval/documents/{id}/submit", post(submit))
        .route("/approval/documents/{id}/approve", post(approve))
        .route("/approval/documents/{id}/reject", post(reject))
        .route("/approval/documents/{id}/withdraw", post(withdraw))
        .route(
            "/approval/documents/{id}/attachments",
            post(add_attachment).layer(DefaultBodyLimit::max(max_attachment_bytes)),
        )
        .route(
            "/approval/documents/{id}/attachments/{attachment_id}",
            get(download_attachment).delete(remove_attachment),
        )
        .route(
            "/admin/approval/forms",
            get(admin_list_forms).post(admin_create_form),
        )
        .route("/admin/approval/forms/{id}", patch(admin_update_form))
        .route(
            "/admin/approval/forms/{id}/duplicate",
            post(admin_duplicate_form),
        )
        .route("/admin/approval/documents", get(admin_list_documents))
        .route("/admin/approval/documents/{id}/reject", post(admin_reject))
}

async fn list_forms(
    State(state): State<AppState>,
    Auth(_): Auth,
) -> ServiceResult<Json<Vec<FormGroup>>> {
    approval::list_forms(&state).await.map(Json)
}

async fn counts(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<ApprovalCounts>> {
    approval::counts(&state, &ctx).await.map(Json)
}

async fn line_suggestion(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<LineSuggestion>> {
    approval::line_suggestion(&state, &ctx).await.map(Json)
}

async fn list_documents(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<DocumentQuery>,
) -> ServiceResult<Json<Page<DocumentSummary>>> {
    approval::list_documents(&state, &ctx, &query)
        .await
        .map(Json)
}

async fn create_document(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<NewDocumentRequest>,
) -> ServiceResult<(StatusCode, Json<DocumentDetail>)> {
    let detail =
        approval::create_document(&state, &ctx, &request.body, request.idempotency.as_ref())
            .await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

async fn get_document(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<DocumentId>,
) -> ServiceResult<Json<DocumentDetail>> {
    approval::get_document(&state, &ctx, id).await.map(Json)
}

async fn update_document(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<DocumentId>,
    ApiJson(request): ApiJson<EditDocumentRequest>,
) -> ServiceResult<Json<DocumentDetail>> {
    approval::update_document(&state, &ctx, id, &request)
        .await
        .map(Json)
}

async fn delete_document(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<DocumentId>,
) -> ServiceResult<Json<Empty>> {
    approval::delete_document(&state, &ctx, id).await?;
    Ok(Json(Empty {}))
}

async fn submit(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<DocumentId>,
) -> ServiceResult<Json<DocumentDetail>> {
    approval::submit_document(&state, &ctx, id).await.map(Json)
}

async fn approve(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<DocumentId>,
    ApiJson(request): ApiJson<ApproveRequest>,
) -> ServiceResult<Json<DocumentDetail>> {
    approval::approve(&state, &ctx, id, &request)
        .await
        .map(Json)
}

async fn reject(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<DocumentId>,
    ApiJson(request): ApiJson<RejectRequest>,
) -> ServiceResult<Json<DocumentDetail>> {
    approval::reject(&state, &ctx, id, &request).await.map(Json)
}

async fn withdraw(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<DocumentId>,
) -> ServiceResult<Json<DocumentDetail>> {
    approval::withdraw(&state, &ctx, id).await.map(Json)
}

async fn admin_list_forms(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<Vec<FormDto>>> {
    approval::admin_list_forms(&state, &ctx).await.map(Json)
}

async fn admin_create_form(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<NewFormRequest>,
) -> ServiceResult<(StatusCode, Json<FormDto>)> {
    let form =
        approval::admin_create_form(&state, &ctx, &request.body, request.idempotency.as_ref())
            .await?;
    Ok((StatusCode::CREATED, Json(form)))
}

async fn admin_update_form(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<FormId>,
    ApiJson(request): ApiJson<EditFormRequest>,
) -> ServiceResult<Json<FormDto>> {
    approval::admin_update_form(&state, &ctx, id, &request)
        .await
        .map(Json)
}

/// 본문은 빈 객체(`{}`)다.
async fn admin_duplicate_form(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<FormId>,
    request: IdempotentJson<Empty>,
) -> ServiceResult<(StatusCode, Json<FormDto>)> {
    let form =
        approval::admin_duplicate_form(&state, &ctx, id, request.idempotency.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(form)))
}

async fn admin_list_documents(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<AdminDocumentQuery>,
) -> ServiceResult<Json<Page<DocumentSummary>>> {
    approval::admin_list_documents(&state, &ctx, &query)
        .await
        .map(Json)
}

async fn admin_reject(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<DocumentId>,
    ApiJson(request): ApiJson<RejectRequest>,
) -> ServiceResult<Json<DocumentDetail>> {
    approval::admin_reject(&state, &ctx, id, &request)
        .await
        .map(Json)
}

async fn add_attachment(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<DocumentId>,
    upload: FileUpload,
) -> ServiceResult<(StatusCode, Json<pango_api::attachment::Attachment>)> {
    let created = approval::add_attachment(
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
    ApiPath((id, attachment_id)): ApiPath<(DocumentId, AttachmentId)>,
) -> ServiceResult<StatusCode> {
    approval::remove_attachment(&state, &ctx, id, attachment_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn download_attachment(
    State(state): State<AppState>,
    DownloadAuth(ctx): DownloadAuth,
    ApiPath((id, attachment_id)): ApiPath<(DocumentId, AttachmentId)>,
) -> ServiceResult<Response> {
    let file = approval::download_attachment(&state, &ctx, id, attachment_id).await?;
    super::download::attachment_response(file)
}
