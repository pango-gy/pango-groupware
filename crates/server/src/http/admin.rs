//! 회사 설정·공휴일·감사 로그 경로. 조회용 공개 경로(`/settings/company`, `/holidays`)도 둔다.

use axum::extract::State;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use pango_api::admin::{
    AuditEntryDto, AuditLogQuery, CompanySettingsDto, CompanySettingsInput, HolidayDto,
    HolidayImportRequest, HolidayImportResult, HolidayInput, HolidayQuery, PublicCompanySettings,
};
use pango_api::{Empty, Page, Versioned};

use super::extract::{ApiJson, ApiPath, ApiQuery, Auth};
use crate::error::ServiceResult;
use crate::service::{audit_log, settings};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/settings/company", get(public_settings))
        .route("/admin/settings", get(get_settings).put(update_settings))
        .route("/holidays", get(list_holidays))
        .route("/admin/holidays/import", post(import_holidays))
        .route(
            "/admin/holidays/{date}",
            put(upsert_holiday).delete(delete_holiday),
        )
        .route("/admin/audit-log", get(audit_log_list))
}

async fn public_settings(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<PublicCompanySettings>> {
    settings::public(&state, &ctx).await.map(Json)
}

async fn get_settings(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<CompanySettingsDto>> {
    settings::get(&state, &ctx).await.map(Json)
}

async fn update_settings(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiJson(request): ApiJson<Versioned<CompanySettingsInput>>,
) -> ServiceResult<Json<CompanySettingsDto>> {
    settings::update(&state, &ctx, &request).await.map(Json)
}

async fn list_holidays(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<HolidayQuery>,
) -> ServiceResult<Json<Vec<HolidayDto>>> {
    settings::list_holidays(&state, &ctx, &query)
        .await
        .map(Json)
}

/// 날짜는 문자열로 받아 서비스에서 엄격히 검사한다(`2026-2-3` 같은 표기를 400으로 거부하기 위해).
async fn upsert_holiday(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(date): ApiPath<String>,
    ApiJson(input): ApiJson<HolidayInput>,
) -> ServiceResult<Json<HolidayDto>> {
    settings::upsert_holiday(&state, &ctx, &date, &input)
        .await
        .map(Json)
}

async fn delete_holiday(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(date): ApiPath<String>,
) -> ServiceResult<Json<Empty>> {
    settings::delete_holiday(&state, &ctx, &date).await?;
    Ok(Json(Empty {}))
}

async fn import_holidays(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiJson(request): ApiJson<HolidayImportRequest>,
) -> ServiceResult<Json<HolidayImportResult>> {
    settings::import_holidays(&state, &ctx, &request)
        .await
        .map(Json)
}

async fn audit_log_list(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<AuditLogQuery>,
) -> ServiceResult<Json<Page<AuditEntryDto>>> {
    audit_log::list(&state, &ctx, &query).await.map(Json)
}
