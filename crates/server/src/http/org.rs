//! 부서·직책 경로.

use axum::extract::State;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use http::StatusCode;
use pango_api::common::VersionQuery;
use pango_api::org::{DepartmentDto, DepartmentInput, PositionDto, PositionInput};
use pango_api::{Empty, Versioned};
use pango_core::id::{DepartmentId, PositionId};

use super::extract::{ApiJson, ApiPath, ApiQuery, Auth, IdempotentJson};
use crate::error::ServiceResult;
use crate::service::org;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/departments", get(list_departments))
        .route("/positions", get(list_positions))
        .route("/admin/departments", post(create_department))
        .route(
            "/admin/departments/{id}",
            patch(update_department).delete(archive_department),
        )
        .route("/admin/positions", post(create_position))
        .route(
            "/admin/positions/{id}",
            patch(update_position).delete(archive_position),
        )
}

async fn list_departments(
    State(state): State<AppState>,
    Auth(_): Auth,
) -> ServiceResult<Json<Vec<DepartmentDto>>> {
    org::list_departments(&state).await.map(Json)
}

async fn list_positions(
    State(state): State<AppState>,
    Auth(_): Auth,
) -> ServiceResult<Json<Vec<PositionDto>>> {
    org::list_positions(&state).await.map(Json)
}

async fn create_department(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<DepartmentInput>,
) -> ServiceResult<(StatusCode, Json<DepartmentDto>)> {
    let created =
        org::create_department(&state, &ctx, &request.body, request.idempotency.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn update_department(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<DepartmentId>,
    ApiJson(request): ApiJson<Versioned<DepartmentInput>>,
) -> ServiceResult<Json<DepartmentDto>> {
    org::update_department(&state, &ctx, id, &request)
        .await
        .map(Json)
}

async fn archive_department(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<DepartmentId>,
    ApiQuery(query): ApiQuery<VersionQuery>,
) -> ServiceResult<Json<Empty>> {
    org::archive_department(&state, &ctx, id, query.version).await?;
    Ok(Json(Empty {}))
}

async fn create_position(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<PositionInput>,
) -> ServiceResult<(StatusCode, Json<PositionDto>)> {
    let created =
        org::create_position(&state, &ctx, &request.body, request.idempotency.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn update_position(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<PositionId>,
    ApiJson(request): ApiJson<Versioned<PositionInput>>,
) -> ServiceResult<Json<PositionDto>> {
    org::update_position(&state, &ctx, id, &request)
        .await
        .map(Json)
}

async fn archive_position(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<PositionId>,
    ApiQuery(query): ApiQuery<VersionQuery>,
) -> ServiceResult<Json<Empty>> {
    org::archive_position(&state, &ctx, id, query.version).await?;
    Ok(Json(Empty {}))
}
