//! 출퇴근·근태 경로.

use axum::extract::State;
use axum::response::Response;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use http::StatusCode;
use pango_api::attendance::{
    AdminAttendanceItem, AdminAttendanceQuery, AdminCreateRecordRequest, AdminEditRecordRequest,
    AttendanceEventDto, AttendanceRecordDto, ClockInRequest, CorrectionDto, CorrectionQuery,
    CreateCorrectionRequest, ExportQuery, MonthQuery, MonthRecords, MonthlySummaryDto,
    TeamAttendance, TeamAttendanceQuery, TodayResponse,
};
use pango_api::Page;
use pango_core::id::{AttendanceCorrectionId, AttendanceRecordId};

use super::extract::{ApiJson, ApiPath, ApiQuery, Auth, DownloadAuth, IdempotentJson};
use crate::error::ServiceResult;
use crate::service::attendance;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/attendance/today", get(today))
        .route("/attendance/clock-in", post(clock_in))
        .route("/attendance/clock-out", post(clock_out))
        .route("/attendance/records", get(records))
        .route("/attendance/records/{id}/events", get(events))
        .route("/attendance/summary", get(summary))
        .route(
            "/attendance/corrections",
            get(list_corrections).post(create_correction),
        )
        .route(
            "/attendance/corrections/{id}/cancel",
            post(cancel_correction),
        )
        .route("/team/attendance", get(team))
        .route("/admin/attendance", get(admin_list).post(admin_create))
        .route("/admin/attendance/export.csv", get(export_csv))
        .route("/admin/attendance/{id}", patch(admin_edit))
}

async fn today(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<TodayResponse>> {
    attendance::today(&state, &ctx).await.map(Json)
}

async fn clock_in(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiJson(request): ApiJson<ClockInRequest>,
) -> ServiceResult<Json<AttendanceRecordDto>> {
    attendance::clock_in(&state, &ctx, &request).await.map(Json)
}

async fn clock_out(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<AttendanceRecordDto>> {
    attendance::clock_out(&state, &ctx).await.map(Json)
}

async fn records(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<MonthQuery>,
) -> ServiceResult<Json<MonthRecords>> {
    attendance::month_records(&state, &ctx, &query)
        .await
        .map(Json)
}

async fn summary(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<MonthQuery>,
) -> ServiceResult<Json<MonthlySummaryDto>> {
    attendance::month_summary(&state, &ctx, &query)
        .await
        .map(Json)
}

async fn events(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<AttendanceRecordId>,
) -> ServiceResult<Json<Vec<AttendanceEventDto>>> {
    attendance::record_events(&state, &ctx, id).await.map(Json)
}

async fn list_corrections(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<CorrectionQuery>,
) -> ServiceResult<Json<Page<CorrectionDto>>> {
    attendance::list_corrections(&state, &ctx, &query)
        .await
        .map(Json)
}

async fn create_correction(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<CreateCorrectionRequest>,
) -> ServiceResult<(StatusCode, Json<CorrectionDto>)> {
    let created =
        attendance::create_correction(&state, &ctx, &request.body, request.idempotency.as_ref())
            .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn cancel_correction(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<AttendanceCorrectionId>,
) -> ServiceResult<Json<CorrectionDto>> {
    attendance::cancel_correction(&state, &ctx, id)
        .await
        .map(Json)
}

async fn team(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<TeamAttendanceQuery>,
) -> ServiceResult<Json<TeamAttendance>> {
    attendance::team(&state, &ctx, &query).await.map(Json)
}

async fn admin_list(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<AdminAttendanceQuery>,
) -> ServiceResult<Json<Page<AdminAttendanceItem>>> {
    attendance::admin_list(&state, &ctx, &query).await.map(Json)
}

async fn admin_create(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<AdminCreateRecordRequest>,
) -> ServiceResult<(StatusCode, Json<AttendanceRecordDto>)> {
    let created =
        attendance::admin_create(&state, &ctx, &request.body, request.idempotency.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn admin_edit(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<AttendanceRecordId>,
    ApiJson(request): ApiJson<AdminEditRecordRequest>,
) -> ServiceResult<Json<AttendanceRecordDto>> {
    attendance::admin_edit(&state, &ctx, id, &request)
        .await
        .map(Json)
}

async fn export_csv(
    State(state): State<AppState>,
    DownloadAuth(ctx): DownloadAuth,
    ApiQuery(query): ApiQuery<ExportQuery>,
) -> ServiceResult<Response> {
    let file = attendance::export_csv(&state, &ctx, &query).await?;
    super::download::file_response("text/csv; charset=utf-8", &file.filename, file.body.into())
}
