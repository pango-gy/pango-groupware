//! 조직: 부서, 직책, 구성원.

use chrono::NaiveDate;
use pango_core::authz::Role;
use pango_core::id::{DepartmentId, MemberId, PositionId};
use pango_core::member::MemberStatus;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepartmentDto {
    pub id: DepartmentId,
    pub parent_id: Option<DepartmentId>,
    pub name: String,
    pub head_member_id: Option<MemberId>,
    pub head_name: Option<String>,
    pub sort_order: i64,
    /// 이 부서에 직접 속한 재직 구성원 수.
    pub member_count: u32,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepartmentInput {
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<DepartmentId>,
    #[serde(default)]
    pub head_member_id: Option<MemberId>,
    #[serde(default)]
    pub sort_order: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionDto {
    pub id: PositionId,
    pub name: String,
    pub level: i64,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionInput {
    pub name: String,
    #[serde(default)]
    pub level: i64,
}

/// 구성원 목록·조직도에 쓰는 공개 정보. 모든 구성원이 볼 수 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberSummary {
    pub id: MemberId,
    pub name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub department_id: Option<DepartmentId>,
    pub department_name: Option<String>,
    pub position_id: Option<PositionId>,
    pub position_name: Option<String>,
    pub status: MemberStatus,
}

/// 관리자가 보는 구성원 상세.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberDetail {
    #[serde(flatten)]
    pub summary: MemberSummary,
    pub login_id: String,
    pub role: Role,
    pub hired_on: NaiveDate,
    pub retired_on: Option<NaiveDate>,
    pub must_change_password: bool,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateMemberRequest {
    pub login_id: String,
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub department_id: Option<DepartmentId>,
    #[serde(default)]
    pub position_id: Option<PositionId>,
    pub hired_on: NaiveDate,
}

/// 계정을 만들거나 비밀번호를 초기화했을 때 한 번만 보여 주는 임시 비밀번호.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedCredentials {
    pub member: MemberDetail,
    pub temporary_password: String,
}

impl std::fmt::Debug for IssuedCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedCredentials")
            .field("member", &self.member)
            .field("temporary_password", &"***")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateMemberRequest {
    pub version: i64,
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub department_id: Option<DepartmentId>,
    #[serde(default)]
    pub position_id: Option<PositionId>,
    pub hired_on: NaiveDate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeStatusRequest {
    pub version: i64,
    pub status: MemberStatus,
    /// 퇴사 처리 시 퇴사일. 생략하면 오늘.
    #[serde(default)]
    pub retired_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeRoleRequest {
    pub version: i64,
    pub role: Role,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberQuery {
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub department_id: Option<DepartmentId>,
    /// 퇴사·정지 계정도 포함할지(관리자 전용).
    #[serde(default)]
    pub include_inactive: bool,
}
