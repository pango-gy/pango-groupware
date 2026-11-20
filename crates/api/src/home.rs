//! 홈과 관리 대시보드. 여러 기능의 요약을 한 번에 모아 주는 읽기 전용 응답이다.

use chrono::NaiveDate;
use pango_core::id::{MemberId, NoticeId};
use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::admin::AuditEntryDto;
use crate::approval::ApprovalCounts;
use crate::attendance::TodayResponse;
use crate::leave::{BalanceDto, LeaveRequestDto};
use crate::notice::NoticeUnreadCount;

/// `GET /home`: 구성원 홈 화면에 필요한 것 전부.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeResponse {
    /// 오늘의 출퇴근 상태(`GET /attendance/today`와 같은 내용).
    pub today: TodayResponse,
    /// 연차 잔여. **저장된 원장 그대로** 계산한다: 홈은 아무것도 쓰지 않으므로, 오늘 생겼어야
    /// 할 자동 부여가 아직 원장에 없으면(하루 한 번 도는 부여 작업 전) 여기에도 없다.
    /// `GET /leave/balance`는 부여를 먼저 반영한다.
    pub leave: BalanceDto,
    pub approval: ApprovalCounts,
    pub unread_notifications: u32,
    pub notices: HomeNotices,
    /// 오늘부터 30일 안에 걸친 내 휴가(결재 중·승인), 시작일순.
    pub upcoming_leaves: Vec<LeaveRequestDto>,
    /// 조직장(관할 부서)·관리자(전체)에게만 있다.
    pub team_today: Option<TeamToday>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeNotices {
    #[serde(flatten)]
    pub counts: NoticeUnreadCount,
    /// 볼 수 있는 공지 중 최근 5건(고정 공지 먼저, `GET /notices`와 같은 순서).
    pub latest: Vec<HomeNotice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeNotice {
    pub id: NoticeId,
    pub title: String,
    pub pinned: bool,
    pub must_read: bool,
    pub read: bool,
    pub published_at: Timestamp,
}

/// 내가 볼 수 있는 재직 구성원의 오늘 출근 현황(`GET /team/attendance`와 같은 범위).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamToday {
    pub date: NaiveDate,
    pub is_working_day: bool,
    pub total: u32,
    /// 오늘 근무일의 출근 기록이 있는 사람.
    pub present: u32,
    /// 근무일인데 출근 기록도 종일 휴가도 없는 사람. 근무일이 아니면 0.
    pub not_yet: u32,
    /// 종일 휴가(승인)이고 출근 기록이 없는 사람.
    pub on_leave: u32,
}

/// `GET /admin/dashboard`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminDashboard {
    pub today: DashboardToday,
    pub pending: DashboardPending,
    /// 최근 7일 안에 퇴근을 찍지 않은(`missing_clock_out`) 구성원. 최근 날짜순, 최대 20명.
    pub missing_clock_out: Vec<MissingClockOut>,
    /// 최근 감사 로그 10건.
    pub recent_audit: Vec<AuditEntryDto>,
}

/// 재직 구성원 전체의 오늘 현황.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardToday {
    pub date: NaiveDate,
    pub is_working_day: bool,
    pub active_members: u32,
    pub clocked_in: u32,
    /// 근무일인데 출근 기록도 종일 휴가도 없는 사람. 근무일이 아니면 0.
    pub not_clocked_in: u32,
    pub on_leave: u32,
    /// 출근한 사람 중 지각(유예 초과).
    pub late: u32,
}

/// 처리를 기다리는 건수.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DashboardPending {
    pub leave_requests: u32,
    pub correction_requests: u32,
    /// 결재 진행 중인 문서 전체(휴가·근태 정정 문서 포함).
    pub approval_documents: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingClockOut {
    pub member_id: MemberId,
    pub name: String,
    pub department_name: Option<String>,
    /// 퇴근이 빠진 근무일(최근 날짜 먼저).
    pub dates: Vec<NaiveDate>,
}
