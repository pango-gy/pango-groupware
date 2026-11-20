//! 휴가 경로.

use axum::extract::State;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use http::StatusCode;
use pango_api::leave::{
    AccrualRunResult, AdjustLeaveRequest, AdminLeaveRequestQuery, BalanceDto, CreateLeaveRequest,
    CreateLeaveTypeRequest, LeaveCalendarEntry, LeaveRequestDto, LeaveRequestQuery, LeaveTypeDto,
    LedgerEntryDto, MemberBalance, MemberScopeQuery, MonthQuery, UpdateLeaveTypeRequest,
};
use pango_api::Page;
use pango_core::id::{LeaveRequestId, LeaveTypeId};

use super::extract::{ApiJson, ApiPath, ApiQuery, Auth, IdempotentJson};
use crate::error::ServiceResult;
use crate::service::leave;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/leave/types", get(list_types))
        .route("/leave/balance", get(balance))
        .route("/leave/ledger", get(ledger))
        .route("/leave/requests", get(list_requests).post(create_request))
        .route("/leave/requests/{id}/cancel", post(cancel_request))
        .route("/leave/calendar", get(calendar))
        .route("/team/leave", get(team_leave))
        .route("/admin/leave/balances", get(admin_balances))
        .route("/admin/leave/adjustments", post(admin_adjust))
        .route("/admin/leave/requests", get(admin_list_requests))
        .route(
            "/admin/leave/requests/{id}/cancel",
            post(admin_cancel_request),
        )
        .route(
            "/admin/leave/types",
            get(admin_list_types).post(admin_create_type),
        )
        .route("/admin/leave/types/{id}", patch(admin_update_type))
        .route("/admin/leave/accrual/run", post(admin_run_accrual))
}

async fn list_types(
    State(state): State<AppState>,
    Auth(_): Auth,
) -> ServiceResult<Json<Vec<LeaveTypeDto>>> {
    leave::list_types(&state).await.map(Json)
}

async fn balance(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<MemberScopeQuery>,
) -> ServiceResult<Json<BalanceDto>> {
    leave::balance(&state, &ctx, query.member_id)
        .await
        .map(Json)
}

async fn ledger(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<MemberScopeQuery>,
) -> ServiceResult<Json<Vec<LedgerEntryDto>>> {
    leave::ledger(&state, &ctx, query.member_id).await.map(Json)
}

async fn list_requests(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<LeaveRequestQuery>,
) -> ServiceResult<Json<Vec<LeaveRequestDto>>> {
    leave::list_requests(&state, &ctx, &query).await.map(Json)
}

async fn create_request(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<CreateLeaveRequest>,
) -> ServiceResult<(StatusCode, Json<LeaveRequestDto>)> {
    let created =
        leave::create_request(&state, &ctx, &request.body, request.idempotency.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn cancel_request(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<LeaveRequestId>,
) -> ServiceResult<Json<LeaveRequestDto>> {
    leave::cancel_request(&state, &ctx, id).await.map(Json)
}

async fn calendar(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<MonthQuery>,
) -> ServiceResult<Json<Vec<LeaveCalendarEntry>>> {
    leave::calendar(&state, &ctx, &query.month).await.map(Json)
}

async fn team_leave(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<MonthQuery>,
) -> ServiceResult<Json<Vec<LeaveRequestDto>>> {
    leave::team_leave(&state, &ctx, &query.month)
        .await
        .map(Json)
}

async fn admin_balances(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<Vec<MemberBalance>>> {
    leave::admin_balances(&state, &ctx).await.map(Json)
}

async fn admin_adjust(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<AdjustLeaveRequest>,
) -> ServiceResult<(StatusCode, Json<BalanceDto>)> {
    let balance =
        leave::admin_adjust(&state, &ctx, &request.body, request.idempotency.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(balance)))
}

async fn admin_list_requests(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<AdminLeaveRequestQuery>,
) -> ServiceResult<Json<Page<LeaveRequestDto>>> {
    leave::admin_list_requests(&state, &ctx, &query)
        .await
        .map(Json)
}

async fn admin_cancel_request(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<LeaveRequestId>,
) -> ServiceResult<Json<LeaveRequestDto>> {
    leave::admin_cancel_request(&state, &ctx, id)
        .await
        .map(Json)
}

async fn admin_list_types(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<Vec<LeaveTypeDto>>> {
    leave::admin_list_types(&state, &ctx).await.map(Json)
}

async fn admin_create_type(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<CreateLeaveTypeRequest>,
) -> ServiceResult<(StatusCode, Json<LeaveTypeDto>)> {
    let created =
        leave::admin_create_type(&state, &ctx, &request.body, request.idempotency.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn admin_update_type(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<LeaveTypeId>,
    ApiJson(request): ApiJson<UpdateLeaveTypeRequest>,
) -> ServiceResult<Json<LeaveTypeDto>> {
    leave::admin_update_type(&state, &ctx, id, &request)
        .await
        .map(Json)
}

async fn admin_run_accrual(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<AccrualRunResult>> {
    leave::admin_run_accrual(&state, &ctx).await.map(Json)
}
