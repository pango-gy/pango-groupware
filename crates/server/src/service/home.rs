//! 홈과 관리 대시보드: 여러 기능의 요약을 한 번에 모아 주는 읽기 전용 조회.
//!
//! 이 모듈에는 규칙이 없다. 각 부분은 그 기능의 `*_tx` 함수가 계산하므로 값과 **보이는 범위**가
//! 원래 화면과 항상 같다(공지는 대상 범위, 팀 현황은 개인 기록 조회 범위). 전부 읽기 연결
//! 하나로 읽고 아무것도 쓰지 않는다.

use pango_api::home::{
    AdminDashboard, DashboardPending, DashboardToday, HomeNotice, HomeNotices, HomeResponse,
    TeamToday,
};
use pango_api::PageQuery;
use pango_core::authz;
use pango_db::{approval as documents, attendance_corrections, leave as leave_requests, Conn};

use super::support::{AuthContext, Company};
use super::{approval, attendance, audit_log, leave, notices, notifications};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 홈에 보여 줄 최근 공지 수.
const LATEST_NOTICES: u32 = 5;
/// 홈의 "다가오는 휴가" 기간(일).
const UPCOMING_LEAVE_DAYS: u64 = 30;
/// 대시보드에서 퇴근 누락을 찾는 기간(일)과 보여 줄 최대 인원.
const MISSING_CLOCK_OUT_DAYS: u64 = 7;
const MISSING_CLOCK_OUT_MEMBERS: usize = 20;
/// 대시보드에 보여 줄 최근 감사 로그 수.
const RECENT_AUDIT_ENTRIES: u32 = 10;

fn count(value: i64) -> ServiceResult<u32> {
    u32::try_from(value).map_err(ServiceError::internal)
}

/// 구성원 홈. 조직장·관리자에게는 볼 수 있는 구성원의 오늘 현황이 함께 간다.
///
/// 연차 잔여는 저장된 원장 그대로다([`leave::stored_balance_tx`]). 홈은 자주 불리는 읽기
/// 전용 조회라서 자동 부여를 반영(쓰기)하지 않는다.
pub async fn home(state: &AppState, ctx: &AuthContext) -> ServiceResult<HomeResponse> {
    let now = state.now();
    let member_id = ctx.member_id();
    let mut conn = state.read().await?;
    let conn: &mut Conn = &mut conn;

    let today = Company::load(conn).await?.today(now);
    let latest = PageQuery {
        limit: Some(LATEST_NOTICES),
        cursor: None,
    };
    let latest = notices::list_tx(conn, &ctx.actor, &latest)
        .await?
        .items
        .into_iter()
        .map(|notice| HomeNotice {
            id: notice.id,
            title: notice.title,
            pinned: notice.pinned,
            must_read: notice.must_read,
            read: notice.read,
            published_at: notice.published_at,
        })
        .collect();
    let team_today = if authz::require_manager(&ctx.actor).is_ok() {
        let members = attendance::visible_active_members_tx(conn, &ctx.actor).await?;
        let headcount = attendance::headcount_tx(conn, &members, now).await?;
        Some(TeamToday {
            date: headcount.date,
            is_working_day: headcount.is_working_day,
            total: headcount.total,
            present: headcount.present,
            not_yet: headcount.not_yet,
            on_leave: headcount.on_leave,
        })
    } else {
        None
    };
    Ok(HomeResponse {
        today: attendance::today_tx(conn, member_id, now).await?,
        leave: leave::stored_balance_tx(conn, member_id, today).await?,
        approval: approval::counts_tx(conn, member_id).await?,
        unread_notifications: notifications::unread_count_tx(conn, member_id)
            .await?
            .unread,
        notices: HomeNotices {
            counts: notices::unread_count_tx(conn, &ctx.actor).await?,
            latest,
        },
        upcoming_leaves: leave::upcoming_tx(conn, member_id, today, UPCOMING_LEAVE_DAYS).await?,
        team_today,
    })
}

/// 관리 대시보드(관리자 전용): 오늘 현황, 처리 대기 건수, 퇴근 누락, 최근 감사 로그.
pub async fn admin_dashboard(state: &AppState, ctx: &AuthContext) -> ServiceResult<AdminDashboard> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let mut conn = state.read().await?;
    let conn: &mut Conn = &mut conn;

    let members = attendance::visible_active_members_tx(conn, &ctx.actor).await?;
    let headcount = attendance::headcount_tx(conn, &members, now).await?;
    Ok(AdminDashboard {
        today: DashboardToday {
            date: headcount.date,
            is_working_day: headcount.is_working_day,
            active_members: headcount.total,
            clocked_in: headcount.present,
            not_clocked_in: headcount.not_yet,
            on_leave: headcount.on_leave,
            late: headcount.late,
        },
        pending: DashboardPending {
            leave_requests: count(leave_requests::count_pending(conn).await?)?,
            correction_requests: count(attendance_corrections::count_pending(conn).await?)?,
            approval_documents: count(documents::count_in_progress(conn).await?)?,
        },
        missing_clock_out: attendance::missing_clock_outs_tx(
            conn,
            now,
            MISSING_CLOCK_OUT_DAYS,
            MISSING_CLOCK_OUT_MEMBERS,
        )
        .await?,
        recent_audit: audit_log::recent_tx(conn, RECENT_AUDIT_ENTRIES).await?,
    })
}
