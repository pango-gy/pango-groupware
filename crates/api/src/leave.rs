//! 휴가: 종류, 잔여, 원장, 신청, 달력.

use chrono::NaiveDate;
use pango_core::id::{DocumentId, LeaveLedgerEntryId, LeaveRequestId, LeaveTypeId, MemberId};
pub use pango_core::leave::LedgerKind;
use pango_core::leave::{Balance, LeaveAmount, LeaveRequestStatus, LeaveUnit};
use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::approval::StepInput;
use crate::common::PageQuery;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaveTypeDto {
    pub id: LeaveTypeId,
    pub code: String,
    pub name: String,
    /// 연차 잔여에서 차감하는지.
    pub deducts_balance: bool,
    /// 반차·반반차로 쓸 수 있는지.
    pub allow_partial: bool,
    pub active: bool,
    pub sort_order: i64,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateLeaveTypeRequest {
    pub code: String,
    pub name: String,
    pub deducts_balance: bool,
    pub allow_partial: bool,
    #[serde(default)]
    pub sort_order: i64,
}

/// `code`와 차감 여부는 바꿀 수 없다. 이미 접수된 신청의 차감·복원 기준이 달라지기 때문이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateLeaveTypeRequest {
    pub version: i64,
    pub name: String,
    pub allow_partial: bool,
    pub active: bool,
    #[serde(default)]
    pub sort_order: i64,
}

/// 기준일(회사 시간대의 오늘)의 연차 잔여.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BalanceDto {
    pub member_id: MemberId,
    pub as_of: NaiveDate,
    pub balance: Balance,
    /// 30일 안에 소멸하는 수량(쓰지 않으면 사라진다).
    pub expiring_soon: LeaveAmount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntryDto {
    pub id: LeaveLedgerEntryId,
    pub kind: LedgerKind,
    pub amount: LeaveAmount,
    pub valid_from: Option<NaiveDate>,
    pub expires_on: Option<NaiveDate>,
    pub effective_on: Option<NaiveDate>,
    pub request_id: Option<LeaveRequestId>,
    pub memo: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaveRequestDto {
    pub id: LeaveRequestId,
    pub member_id: MemberId,
    pub member_name: String,
    pub leave_type_id: LeaveTypeId,
    pub leave_type_name: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    /// 실제로 쉬는 첫 근무일(주말·공휴일을 건너뛴 날). 본인은 승인된 휴가를 이 날 전까지
    /// 취소할 수 있다. 예전 서버의 응답에는 없다.
    #[serde(default)]
    pub first_working_day: Option<NaiveDate>,
    pub unit: LeaveUnit,
    pub amount: LeaveAmount,
    pub reason: String,
    pub status: LeaveRequestStatus,
    pub document_id: DocumentId,
    pub created_at: Timestamp,
    pub decided_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateLeaveRequest {
    pub leave_type_id: LeaveTypeId,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub unit: LeaveUnit,
    pub reason: String,
    /// 생략하면 추천 결재선(상위 조직장)을 쓴다.
    #[serde(default)]
    pub steps: Option<Vec<StepInput>>,
    #[serde(default)]
    pub references: Vec<MemberId>,
}

/// 휴가 달력의 한 줄. 누가 언제 쉬는지만 담는다(사유와 휴가 종류는 넣지 않는다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaveCalendarEntry {
    pub member_id: MemberId,
    pub member_name: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub unit: LeaveUnit,
    pub status: LeaveRequestStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberScopeQuery {
    /// 생략하면 본인.
    #[serde(default)]
    pub member_id: Option<MemberId>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaveRequestQuery {
    /// 생략하면 올해.
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub member_id: Option<MemberId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonthQuery {
    /// `YYYY-MM`.
    pub month: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminLeaveRequestQuery {
    #[serde(default)]
    pub status: Option<LeaveRequestStatus>,
    #[serde(default)]
    pub member_id: Option<MemberId>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl AdminLeaveRequestQuery {
    pub fn page(&self) -> PageQuery {
        PageQuery::of(self.limit, &self.cursor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberBalance {
    pub member_id: MemberId,
    pub member_name: String,
    pub department_name: Option<String>,
    pub hired_on: NaiveDate,
    pub balance: Balance,
}

/// 관리자의 수동 조정. `amount_q`는 1/4일 단위이며 0이 아니고 절댓값이 400 이하다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdjustLeaveRequest {
    pub member_id: MemberId,
    pub amount_q: i32,
    pub effective_on: NaiveDate,
    pub memo: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccrualRunResult {
    /// 확인한 재직 구성원 수.
    pub members: u32,
    /// 이번 실행에서 새로 넣은 부여 건수. 다시 실행하면 0이다.
    pub grants_inserted: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_entry_has_no_reason_or_type_fields() {
        let entry = LeaveCalendarEntry {
            member_id: MemberId::new(),
            member_name: "홍길동".into(),
            start_date: NaiveDate::MIN,
            end_date: NaiveDate::MIN,
            unit: LeaveUnit::FullDay,
            status: LeaveRequestStatus::Pending,
        };
        let json = serde_json::to_value(&entry).expect("serialize");
        let keys: Vec<&str> = json
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "end_date",
                "member_id",
                "member_name",
                "start_date",
                "status",
                "unit"
            ]
        );
    }
}
