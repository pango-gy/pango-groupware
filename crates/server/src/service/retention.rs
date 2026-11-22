//! 기록 보존 기간: 설정(최고 관리자)과 하루 한 번의 정리.
//!
//! 지우는 것은 보존 연수가 지난 근무일의 출퇴근 기록과 그 수정 이력뿐이다. 휴가 원장과 결재
//! 문서는 지우지 않는다(마이그레이션 0010의 설명). 기본값은 "지우지 않음"이다.

use chrono::{Months, NaiveDate};
use pango_api::admin::{RetentionDto, RetentionInput};
use pango_api::Versioned;
use pango_core::authz;
use pango_core::error::ValidationReason;
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::{retention, Conn};

use super::support::{
    audit, count_u32, current_actor, require_updated, Audit, AuthContext, Changes, Company,
};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 근로기준법의 근로계약 서류 보존 기간(3년)보다 짧게는 정할 수 없다.
pub const MIN_YEARS: u8 = 3;
pub const MAX_YEARS: u8 = 10;

/// 이 날짜보다 이른 근무일이 지울 대상이다: 오늘(회사 근무일)에서 `years`년 전.
fn cutoff(today: NaiveDate, years: u8) -> ServiceResult<NaiveDate> {
    today
        .checked_sub_months(Months::new(u32::from(years) * 12))
        .ok_or_else(|| ServiceError::internal("retention cutoff underflow"))
}

async fn today(conn: &mut Conn, now: Timestamp) -> ServiceResult<Option<NaiveDate>> {
    Ok(Company::load_if_initialized(conn)
        .await?
        .map(|company| company.work_date(now)))
}

async fn dto(conn: &mut Conn, now: Timestamp) -> ServiceResult<RetentionDto> {
    let current = retention::settings(conn).await?;
    let cutoff = match (current.attendance_years, today(conn, now).await?) {
        (Some(years), Some(today)) => Some(cutoff(today, years)?),
        _ => None,
    };
    let to_delete = match cutoff {
        Some(day) => retention::count_attendance_before(conn, day).await?,
        None => 0,
    };
    Ok(RetentionDto {
        attendance_years: current.attendance_years,
        attendance_cutoff: cutoff,
        attendance_to_delete: count_u32(to_delete)?,
        version: current.version,
        updated_at: current.updated_at,
    })
}

pub async fn get(state: &AppState, ctx: &AuthContext) -> ServiceResult<RetentionDto> {
    authz::require_admin(&ctx.actor)?;
    dto(&mut *state.read().await?, state.now()).await
}

/// 보존 기간을 바꾼다. 기록을 되돌릴 수 없게 지우는 정책이므로 최고 관리자만 바꿀 수 있다.
pub async fn update(
    state: &AppState,
    ctx: &AuthContext,
    request: &Versioned<RetentionInput>,
) -> ServiceResult<RetentionDto> {
    authz::require_owner(&ctx.actor)?;
    let years = request.data.attendance_years;
    if years.is_some_and(|y| !(MIN_YEARS..=MAX_YEARS).contains(&y)) {
        return Err(
            DomainError::validation("attendance_years", ValidationReason::OutOfRange).into(),
        );
    }
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let actor = current_actor(conn, ctx).await?;
            authz::require_owner(&actor)?;
            let before = retention::settings(conn).await?;
            let mut changes = Changes::default();
            changes.field("attendance_years", &before.attendance_years, &years);
            require_updated(retention::update_settings(conn, years, request.version, now).await?)?;
            audit(
                conn,
                now,
                Audit {
                    action: "retention.update",
                    target_type: "retention_settings",
                    target_id: None,
                    detail: Some(changes.into_detail()),
                    ctx: Some(ctx),
                },
            )
            .await?;
            dto(conn, now).await
        })
        .await
}

/// 보존 기간이 지난 출퇴근 기록을 지운다. 설정이 없거나 지울 것이 없으면 아무것도 하지 않는다.
/// 지운 기록 수를 돌려준다. 지웠으면 감사 로그(시스템 작업)에 기준일과 수를 남긴다.
pub async fn purge_once(state: &AppState) -> ServiceResult<u64> {
    let now = state.now();
    state
        .write(async |conn: &mut Conn| {
            let Some(years) = retention::settings(conn).await?.attendance_years else {
                return Ok(0);
            };
            let Some(today) = today(conn, now).await? else {
                return Ok(0);
            };
            let before = cutoff(today, years)?;
            let purged = retention::purge_attendance_before(conn, before).await?;
            if purged.records == 0 {
                return Ok(0);
            }
            audit(
                conn,
                now,
                Audit {
                    action: "retention.purge",
                    target_type: "attendance_records",
                    target_id: None,
                    detail: Some(serde_json::json!({
                        "before": before,
                        "years": years,
                        "records": purged.records,
                        "events": purged.events,
                    })),
                    ctx: None,
                },
            )
            .await?;
            Ok(purged.records)
        })
        .await
}

/// 하루 한 번만 정리하기 위한 상태(회사 근무일 기준).
#[derive(Debug, Default)]
pub struct DailyPurge {
    last_run: Option<NaiveDate>,
}

impl DailyPurge {
    pub async fn run_if_due(&mut self, state: &AppState) -> ServiceResult<Option<u64>> {
        let Some(today) = today(&mut *state.read().await?, state.now()).await? else {
            return Ok(None);
        };
        if self.last_run == Some(today) {
            return Ok(None);
        }
        let purged = purge_once(state).await?;
        self.last_run = Some(today);
        Ok(Some(purged))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutoff_is_whole_years_back_and_clamps_leap_days() {
        let day = |s: &str| s.parse::<NaiveDate>().expect("date");
        assert_eq!(cutoff(day("2026-03-02"), 3).ok(), Some(day("2023-03-02")));
        assert_eq!(cutoff(day("2028-02-29"), 3).ok(), Some(day("2025-02-28")));
    }
}
