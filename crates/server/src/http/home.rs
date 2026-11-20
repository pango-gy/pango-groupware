//! 홈·관리 대시보드 경로.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use pango_api::home::{AdminDashboard, HomeResponse};

use super::extract::Auth;
use crate::error::ServiceResult;
use crate::service::home;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/home", get(home_view))
        .route("/admin/dashboard", get(admin_dashboard))
}

async fn home_view(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<HomeResponse>> {
    home::home(&state, &ctx).await.map(Json)
}

async fn admin_dashboard(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<AdminDashboard>> {
    home::admin_dashboard(&state, &ctx).await.map(Json)
}
