//! 전자결재 경로.

use axum::extract::State;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use http::StatusCode;
use pango_api::approval::{
    AdminDocumentQuery, ApprovalCounts, ApproveRequest, CreateDocumentRequest, CreateFormRequest,
    DocumentDetail, DocumentQuery, DocumentSummary, FormDto, LineSuggestion, RejectRequest,
    UpdateDocumentRequest, UpdateFormRequest,
};
use pango_api::{Empty, Page};
use pango_core::id::{DocumentId, FormId};

use super::extract::{ApiJson, ApiPath, ApiQuery, Auth, IdempotentJson};
use crate::error::ServiceResult;
use crate::service::approval;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
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
            "/admin/approval/forms",
            get(admin_list_forms).post(admin_create_form),
        )
        .route("/admin/approval/forms/{id}", patch(admin_update_form))
        .route("/admin/approval/documents", get(admin_list_documents))
        .route("/admin/approval/documents/{id}/reject", post(admin_reject))
}

async fn list_forms(
    State(state): State<AppState>,
    Auth(_): Auth,
) -> ServiceResult<Json<Vec<FormDto>>> {
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
    request: IdempotentJson<CreateDocumentRequest>,
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
    ApiJson(request): ApiJson<UpdateDocumentRequest>,
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
    request: IdempotentJson<CreateFormRequest>,
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
    ApiJson(request): ApiJson<UpdateFormRequest>,
) -> ServiceResult<Json<FormDto>> {
    approval::admin_update_form(&state, &ctx, id, &request)
        .await
        .map(Json)
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
