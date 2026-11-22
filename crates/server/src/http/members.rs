//! 구성원 디렉터리·팀·구성원 관리 경로.

use axum::extract::State;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use http::StatusCode;
use pango_api::auth::{RevokedSessions, SessionInfo};
use pango_api::org::{
    ChangeRoleRequest, ChangeStatusRequest, CreateMemberRequest, IssuedCredentials, MemberDetail,
    MemberQuery, MemberSummary, UpdateMemberRequest,
};
use pango_core::id::{MemberId, SessionId};

use super::extract::{ApiJson, ApiPath, ApiQuery, Auth};
use crate::error::ServiceResult;
use crate::service::members;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/members", get(list_directory))
        .route("/members/{id}", get(get_member))
        .route("/team/members", get(team_members))
        .route("/admin/members", get(admin_list).post(create_member))
        .route("/admin/members/{id}", get(admin_get).patch(update_member))
        .route("/admin/members/{id}/status", post(change_status))
        .route("/admin/members/{id}/role", post(change_role))
        .route("/admin/members/{id}/reset-password", post(reset_password))
        .route(
            "/admin/members/{id}/sessions",
            get(member_sessions).delete(revoke_member_sessions),
        )
        .route(
            "/admin/members/{id}/sessions/{session_id}",
            delete(revoke_member_session),
        )
        .route(
            "/admin/members/{id}/totp",
            get(member_totp).delete(reset_member_totp),
        )
}

async fn list_directory(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<MemberQuery>,
) -> ServiceResult<Json<Vec<MemberSummary>>> {
    members::list_directory(&state, &ctx, &query)
        .await
        .map(Json)
}

async fn get_member(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
) -> ServiceResult<Json<MemberSummary>> {
    members::get_member(&state, &ctx, id).await.map(Json)
}

async fn team_members(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<Vec<MemberSummary>>> {
    members::team_members(&state, &ctx).await.map(Json)
}

async fn admin_list(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<MemberQuery>,
) -> ServiceResult<Json<Vec<MemberDetail>>> {
    members::admin_list(&state, &ctx, &query).await.map(Json)
}

async fn admin_get(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
) -> ServiceResult<Json<MemberDetail>> {
    members::admin_get(&state, &ctx, id).await.map(Json)
}

async fn create_member(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiJson(request): ApiJson<CreateMemberRequest>,
) -> ServiceResult<(StatusCode, Json<IssuedCredentials>)> {
    let issued = members::create_member(&state, &ctx, &request).await?;
    Ok((StatusCode::CREATED, Json(issued)))
}

async fn update_member(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
    ApiJson(request): ApiJson<UpdateMemberRequest>,
) -> ServiceResult<Json<MemberDetail>> {
    members::update_member(&state, &ctx, id, &request)
        .await
        .map(Json)
}

async fn change_status(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
    ApiJson(request): ApiJson<ChangeStatusRequest>,
) -> ServiceResult<Json<MemberDetail>> {
    members::change_status(&state, &ctx, id, &request)
        .await
        .map(Json)
}

async fn change_role(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
    ApiJson(request): ApiJson<ChangeRoleRequest>,
) -> ServiceResult<Json<MemberDetail>> {
    members::change_role(&state, &ctx, id, &request)
        .await
        .map(Json)
}

async fn reset_password(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
) -> ServiceResult<Json<IssuedCredentials>> {
    members::reset_password(&state, &ctx, id).await.map(Json)
}

async fn member_sessions(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
) -> ServiceResult<Json<Vec<SessionInfo>>> {
    members::list_member_sessions(&state, &ctx, id)
        .await
        .map(Json)
}

async fn revoke_member_session(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath((id, session_id)): ApiPath<(MemberId, SessionId)>,
) -> ServiceResult<StatusCode> {
    members::revoke_member_session(&state, &ctx, id, session_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_member_sessions(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
) -> ServiceResult<Json<RevokedSessions>> {
    let revoked = members::revoke_member_sessions(&state, &ctx, id).await?;
    Ok(Json(RevokedSessions { revoked }))
}

async fn member_totp(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
) -> ServiceResult<Json<pango_api::auth::MemberTotpDto>> {
    let enabled = crate::service::totp::member_enabled(&state, &ctx, id).await?;
    Ok(Json(pango_api::auth::MemberTotpDto { enabled }))
}

async fn reset_member_totp(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<MemberId>,
) -> ServiceResult<StatusCode> {
    crate::service::totp::admin_reset(&state, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
