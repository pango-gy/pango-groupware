//! 관리: 회사 설정, 공휴일, 감사 로그.

use chrono::NaiveDate;
use pango_core::id::{AuditLogId, MemberId};
use pango_core::time::{MinuteOfDay, Timestamp};
use serde::{Deserialize, Serialize};

use crate::PageQuery;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakTierDto {
    /// 근무 구간이 이 분 이상이면
    pub min_span_minutes: u16,
    /// 이만큼 휴게 시간으로 뺀다.
    pub break_minutes: u16,
}

/// 관리자가 바꿀 수 있는 회사 설정 전체. 수정은 [`crate::Versioned`]로 감싸 보낸다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanySettingsInput {
    pub company_name: String,
    /// IANA 시간대 이름(예: `Asia/Seoul`).
    pub timezone: String,
    /// 근무일 전환 시각(`HH:MM`). 이 시각 전은 전날 근무일이다.
    pub day_cutoff: MinuteOfDay,
    pub work_start: MinuteOfDay,
    pub work_end: MinuteOfDay,
    pub late_grace_minutes: u16,
    /// 오전/오후 반차의 경계 시각.
    pub half_day_split: MinuteOfDay,
    /// 반반차 길이(분).
    pub quarter_minutes: u16,
    pub break_tiers: Vec<BreakTierDto>,
    /// 근무 요일 비트(비트 0 = 월요일 … 비트 6 = 일요일).
    pub workdays: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanySettingsDto {
    #[serde(flatten)]
    pub settings: CompanySettingsInput,
    pub version: i64,
    pub updated_at: Timestamp,
}

/// 모든 구성원이 볼 수 있는 회사 정보. 근태 화면 표시에 필요한 것만 담는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicCompanySettings {
    pub company_name: String,
    pub timezone: String,
    pub work_start: MinuteOfDay,
    pub work_end: MinuteOfDay,
    pub workdays: u8,
    pub day_cutoff: MinuteOfDay,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolidayDto {
    pub date: NaiveDate,
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolidayQuery {
    /// 생략하면 회사 시간대 기준 올해.
    #[serde(default)]
    pub year: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolidayInput {
    pub name: String,
}

/// 일괄 등록 항목. 날짜 표기를 엄격히 검사하려고 문자열로 받는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolidayImportItem {
    pub date: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolidayImportRequest {
    pub items: Vec<HolidayImportItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HolidayImportResult {
    pub imported: u32,
}

/// 감사 로그 조회 조건. `from`은 포함, `until`은 제외(밀리초 시각).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditLogQuery {
    #[serde(default)]
    pub actor_id: Option<MemberId>,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub target_type: Option<String>,
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub from: Option<Timestamp>,
    #[serde(default)]
    pub until: Option<Timestamp>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl AuditLogQuery {
    pub fn page(&self) -> PageQuery {
        PageQuery::of(self.limit, &self.cursor)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntryDto {
    pub id: AuditLogId,
    pub at: Timestamp,
    /// 시스템 작업이면 `None`.
    pub actor_id: Option<MemberId>,
    pub actor_name: Option<String>,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    /// 대상을 사람이 알아볼 이름(구성원 이름, 부서명, 공지 제목, 문서 제목 등).
    /// 이름을 정할 수 없는 대상(로그인 기기 등)이나 지워진 대상은 `None`.
    #[serde(default)]
    pub target_name: Option<String>,
    pub detail: Option<serde_json::Value>,
    pub ip: Option<String>,
}

/// 기록 보존 기간(`GET·PUT /admin/retention`). 바꾸기는 최고 관리자만.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionDto {
    /// 출퇴근 기록 보존 연수(3~10). `None`이면 지우지 않는다.
    pub attendance_years: Option<u8>,
    /// 지금 설정으로 다음 정리 때 지워질 근무일의 기준(이 날짜보다 이른 근무일). 설정이 없으면 `None`.
    pub attendance_cutoff: Option<NaiveDate>,
    /// 다음 정리 때 지워질 출퇴근 기록 수.
    pub attendance_to_delete: u32,
    pub version: i64,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetentionInput {
    pub attendance_years: Option<u8>,
}
