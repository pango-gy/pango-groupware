//! 캘린더 경로.

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use http::StatusCode;
use pango_api::calendar::{
    CalendarDto, CalendarView, CalendarViewQuery, ConflictQuery, ConflictReport,
    CreateEventRequest, EventDetail, RespondRequest, UpdateEventRequest,
};
use pango_api::common::VersionQuery;
use pango_api::{Empty, Versioned};
use pango_core::id::CalendarEventId;

use super::extract::{ApiJson, ApiPath, ApiQuery, Auth, IdempotentJson};
use crate::error::ServiceResult;
use crate::service::calendar;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/calendar/calendars", get(list_calendars))
        .route("/calendar/view", get(view))
        .route("/calendar/conflicts", get(conflicts))
        .route("/calendar/events", post(create))
        .route(
            "/calendar/events/{id}",
            get(detail).patch(update).delete(delete),
        )
        .route("/calendar/events/{id}/respond", post(respond))
}

async fn list_calendars(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<Vec<CalendarDto>>> {
    calendar::list_calendars(&state, &ctx).await.map(Json)
}

async fn view(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<CalendarViewQuery>,
) -> ServiceResult<Json<CalendarView>> {
    calendar::view(&state, &ctx, &query).await.map(Json)
}

async fn conflicts(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<ConflictQuery>,
) -> ServiceResult<Json<ConflictReport>> {
    calendar::conflicts(&state, &ctx, &query).await.map(Json)
}

async fn create(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<CreateEventRequest>,
) -> ServiceResult<(StatusCode, Json<EventDetail>)> {
    let event =
        calendar::create_event(&state, &ctx, &request.body, request.idempotency.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(event)))
}

async fn detail(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<CalendarEventId>,
) -> ServiceResult<Json<EventDetail>> {
    calendar::get_event(&state, &ctx, id).await.map(Json)
}

async fn update(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<CalendarEventId>,
    ApiJson(request): ApiJson<Versioned<UpdateEventRequest>>,
) -> ServiceResult<Json<EventDetail>> {
    calendar::update_event(&state, &ctx, id, &request)
        .await
        .map(Json)
}

async fn delete(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<CalendarEventId>,
    ApiQuery(query): ApiQuery<VersionQuery>,
) -> ServiceResult<Json<Empty>> {
    calendar::delete_event(&state, &ctx, id, query.version).await?;
    Ok(Json(Empty {}))
}

async fn respond(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<CalendarEventId>,
    ApiJson(request): ApiJson<RespondRequest>,
) -> ServiceResult<Json<EventDetail>> {
    calendar::respond(&state, &ctx, id, request.status)
        .await
        .map(Json)
}
