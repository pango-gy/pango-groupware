//! 출퇴근·근태.
//!
//! 요청의 날짜·월은 문자열로 받아 서비스에서 `pango_core::calendar::parse_date`/`parse_month`로
//! 엄격하게 검증한다(`2026-3-2` 같은 다른 표기를 받지 않기 위해서다).

use chrono::NaiveDate;
use pango_core::attendance::{
    AttendanceEventKind, CorrectionStatus, DayEvaluation, MonthlySummary, WorkType,
};
use pango_core::id::{
    AttendanceCorrectionId, AttendanceEventId, AttendanceRecordId, DepartmentId, DocumentId,
    MemberId,
};
use pango_core::time::{MinuteOfDay, Timestamp};
use serde::{Deserialize, Serialize};

use crate::approval::StepInput;
use crate::common::PageQuery;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttendanceRecordDto {
    pub id: AttendanceRecordId,
    pub member_id: MemberId,
    pub work_date: NaiveDate,
    pub clock_in_at: Timestamp,
    pub clock_out_at: Option<Timestamp>,
    pub work_type: WorkType,
    pub note: Option<String>,
    pub version: i64,
}

/// 홈의 출퇴근 버튼 상태.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodayResponse {
    /// 지금 시각이 속한 근무일.
    pub work_date: NaiveDate,
    pub is_working_day: bool,
    pub holiday_name: Option<String>,
    /// 오늘 근무일의 기록, 또는 아직 퇴근으로 닫을 수 있는 전날 야간 근무 기록.
    pub record: Option<AttendanceRecordDto>,
    /// `record`가 있으면 그 기록 근무일의, 없으면 오늘의 판정.
    pub evaluation: DayEvaluation,
    /// 회사 근무 시작·종료(`HH:MM`).
    pub expected_start: MinuteOfDay,
    pub expected_end: MinuteOfDay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockInRequest {
    pub work_type: WorkType,
    #[serde(default)]
    pub note: Option<String>,
}

/// `GET /attendance/records`, `GET /attendance/summary`의 쿼리.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonthQuery {
    /// `YYYY-MM`.
    pub month: String,
    /// 생략하면 본인.
    #[serde(default)]
    pub member_id: Option<MemberId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DayEntry {
    pub date: NaiveDate,
    pub is_working_day: bool,
    pub holiday_name: Option<String>,
    pub record: Option<AttendanceRecordDto>,
    pub evaluation: DayEvaluation,
}

/// 한 달의 날짜별 기록. 달의 모든 날짜가 한 줄씩 들어 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonthRecords {
    pub member_id: MemberId,
    /// `YYYY-MM`.
    pub month: String,
    pub days: Vec<DayEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonthlySummaryDto {
    pub member_id: MemberId,
    /// `YYYY-MM`.
    pub month: String,
    #[serde(flatten)]
    pub summary: MonthlySummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttendanceEventDto {
    pub id: AttendanceEventId,
    pub record_id: AttendanceRecordId,
    pub kind: AttendanceEventKind,
    /// 변경 전 기록(`AttendanceRecordDto` 모양). 새로 만든 경우 `null`.
    pub before: Option<serde_json::Value>,
    pub after: serde_json::Value,
    pub reason: Option<String>,
    pub actor_id: MemberId,
    pub actor_name: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamAttendanceQuery {
    /// `YYYY-MM-DD`. 생략하면 오늘 근무일.
    #[serde(default)]
    pub date: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamAttendanceEntry {
    pub member_id: MemberId,
    pub name: String,
    pub department_id: Option<DepartmentId>,
    pub department_name: Option<String>,
    pub record: Option<AttendanceRecordDto>,
    pub evaluation: DayEvaluation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamAttendance {
    pub date: NaiveDate,
    pub is_working_day: bool,
    pub members: Vec<TeamAttendanceEntry>,
}

/// `GET /admin/attendance`의 쿼리. `from`~`to`는 최대 366일.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminAttendanceQuery {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub member_id: Option<MemberId>,
    /// 하위 부서 구성원도 포함한다.
    #[serde(default)]
    pub department_id: Option<DepartmentId>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminAttendanceItem {
    pub record: AttendanceRecordDto,
    pub member_name: String,
    pub login_id: String,
    pub department_name: Option<String>,
    pub evaluation: DayEvaluation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminCreateRecordRequest {
    pub member_id: MemberId,
    /// `YYYY-MM-DD`. 출근 시각이 이 근무일에 속해야 한다.
    pub work_date: String,
    pub clock_in_at: Timestamp,
    #[serde(default)]
    pub clock_out_at: Option<Timestamp>,
    pub work_type: WorkType,
    #[serde(default)]
    pub note: Option<String>,
    /// 기록을 직접 만드는 사유. 이력에 남는다.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminEditRecordRequest {
    pub version: i64,
    pub clock_in_at: Timestamp,
    #[serde(default)]
    pub clock_out_at: Option<Timestamp>,
    pub work_type: WorkType,
    /// 수정 사유. 이력에 남는다.
    pub reason: String,
}

/// `GET /admin/attendance/export.csv`의 쿼리. 최대 366일.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportQuery {
    pub from: String,
    pub to: String,
}

/// 근태 정정 요청(`POST /attendance/corrections`). 고칠 값 전체를 보낸다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateCorrectionRequest {
    /// `YYYY-MM-DD`. 오늘 근무일까지, 31일 전까지. 출근 시각이 이 근무일에 속해야 한다.
    pub work_date: String,
    pub clock_in_at: Timestamp,
    #[serde(default)]
    pub clock_out_at: Option<Timestamp>,
    pub work_type: WorkType,
    /// 정정 사유. 결재 문서의 본문과 기록 이력에 남는다.
    pub reason: String,
    /// 생략하면 추천 결재선(상위 조직장)을 쓴다.
    #[serde(default)]
    pub steps: Option<Vec<StepInput>>,
    #[serde(default)]
    pub references: Vec<MemberId>,
}

/// 근태 정정 요청. 상태는 연결된 결재 문서(`document_id`)가 종료될 때 바뀐다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectionDto {
    pub id: AttendanceCorrectionId,
    pub member_id: MemberId,
    pub member_name: String,
    pub work_date: NaiveDate,
    /// 요청한 값.
    pub clock_in_at: Timestamp,
    pub clock_out_at: Option<Timestamp>,
    pub work_type: WorkType,
    pub reason: String,
    pub status: CorrectionStatus,
    pub document_id: DocumentId,
    pub created_at: Timestamp,
    pub decided_at: Option<Timestamp>,
}

/// `GET /attendance/corrections`의 쿼리. 최신 접수순.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorrectionQuery {
    /// 생략하면 본인.
    #[serde(default)]
    pub member_id: Option<MemberId>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl AdminAttendanceQuery {
    pub fn page(&self) -> PageQuery {
        PageQuery {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }
}

impl CorrectionQuery {
    pub fn page(&self) -> PageQuery {
        PageQuery {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }
}
