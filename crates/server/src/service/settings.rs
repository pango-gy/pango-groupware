//! 회사 설정과 공휴일.

use std::collections::BTreeSet;

use chrono::{Datelike, NaiveDate};
use pango_api::admin::{
    BreakTierDto, CompanySettingsDto, CompanySettingsInput, HolidayDto, HolidayImportRequest,
    HolidayImportResult, HolidayInput, HolidayQuery, PublicCompanySettings,
};
use pango_api::Versioned;
use pango_core::authz;
use pango_core::calendar::parse_date;
use pango_core::calendar::{DateRange, WorkWeek};
use pango_core::error::ValidationReason;
use pango_core::text::{CompanyName, LabelName};
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::settings::{self, CompanySettings, Holiday};
use pango_db::{codec, Conn};
use serde_json::{Map, Value};

use super::support::{
    audit, found, require_current_admin, require_updated, Audit, AuthContext, Company,
};
use crate::error::{ServiceError, ServiceResult};
use crate::state::AppState;

/// 한 번에 일괄 등록할 수 있는 공휴일 수. 한 해 공휴일(대체 휴일 포함)보다 넉넉하다.
pub const MAX_HOLIDAY_IMPORT: usize = 100;

/// 공휴일 조회에 받는 연도. 날짜를 항상 네 자리 연도로 저장하기 위한 범위다.
const YEARS: std::ops::RangeInclusive<i32> = 1000..=9999;

fn input_of(settings: &CompanySettings) -> CompanySettingsInput {
    CompanySettingsInput {
        company_name: settings.company_name.clone(),
        timezone: settings.timezone.clone(),
        day_cutoff: settings.day_cutoff,
        work_start: settings.work_start,
        work_end: settings.work_end,
        late_grace_minutes: settings.late_grace_minutes,
        half_day_split: settings.half_day_split,
        quarter_minutes: settings.quarter_minutes,
        break_tiers: settings
            .break_tiers
            .iter()
            .map(|&(min_span_minutes, break_minutes)| BreakTierDto {
                min_span_minutes,
                break_minutes,
            })
            .collect(),
        workdays: settings.workdays.bits(),
    }
}

fn dto(settings: &CompanySettings) -> CompanySettingsDto {
    CompanySettingsDto {
        settings: input_of(settings),
        version: settings.version,
        updated_at: settings.updated_at,
    }
}

/// 입력을 저장할 설정으로 바꾼다. 도메인 값 변환은 [`Company::from_settings`]가 한다
/// (저장된 설정을 읽을 때와 같은 검증이므로, 여기서 통과한 값은 다시 읽어도 유효하다).
fn settings_from(
    input: &CompanySettingsInput,
    version: i64,
    now: Timestamp,
) -> ServiceResult<CompanySettings> {
    let settings = CompanySettings {
        company_name: CompanyName::parse(&input.company_name)?.into_string(),
        timezone: input.timezone.clone(),
        day_cutoff: input.day_cutoff,
        work_start: input.work_start,
        work_end: input.work_end,
        late_grace_minutes: input.late_grace_minutes,
        half_day_split: input.half_day_split,
        quarter_minutes: input.quarter_minutes,
        break_tiers: input
            .break_tiers
            .iter()
            .map(|tier| (tier.min_span_minutes, tier.break_minutes))
            .collect(),
        workdays: WorkWeek::from_bits(input.workdays)?,
        version,
        updated_at: now,
    };
    Ok(Company::from_settings(settings)?.settings)
}

/// 바뀐 설정 항목만 `{항목: {before, after}}`로 모은다. 감사 로그의 `detail`에 쓴다.
fn changed_keys(
    before: &CompanySettingsInput,
    after: &CompanySettingsInput,
) -> ServiceResult<Value> {
    let as_map = |value: &CompanySettingsInput| match serde_json::to_value(value) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(ServiceError::internal(
            "settings did not serialize to an object",
        )),
        Err(error) => Err(ServiceError::internal(error)),
    };
    let (before, after) = (as_map(before)?, as_map(after)?);
    let changed: Map<String, Value> = after
        .into_iter()
        .filter_map(|(key, new)| {
            let old = before.get(&key).cloned().unwrap_or(Value::Null);
            (old != new).then(|| (key, serde_json::json!({ "before": old, "after": new })))
        })
        .collect();
    Ok(Value::Object(changed))
}

/// 모든 구성원에게 공개하는 회사 정보.
pub async fn public(state: &AppState, _ctx: &AuthContext) -> ServiceResult<PublicCompanySettings> {
    let company = Company::load(&mut *state.read().await?).await?;
    let s = company.settings;
    Ok(PublicCompanySettings {
        company_name: s.company_name,
        timezone: s.timezone,
        work_start: s.work_start,
        work_end: s.work_end,
        workdays: s.workdays.bits(),
        day_cutoff: s.day_cutoff,
    })
}

pub async fn get(state: &AppState, ctx: &AuthContext) -> ServiceResult<CompanySettingsDto> {
    authz::require_admin(&ctx.actor)?;
    let company = Company::load(&mut *state.read().await?).await?;
    Ok(dto(&company.settings))
}

/// 설정 전체를 바꾼다. 조합이 규칙에 맞지 않으면 문제가 된 필드로 400, 버전이 낡았으면 409.
pub async fn update(
    state: &AppState,
    ctx: &AuthContext,
    request: &Versioned<CompanySettingsInput>,
) -> ServiceResult<CompanySettingsDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let next = settings_from(&request.data, request.version, now)?;
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            let current = found(settings::get(conn).await?)?;
            require_updated(settings::update(conn, &next).await?)?;
            let changed = changed_keys(&input_of(&current), &input_of(&next))?;
            audit(
                conn,
                now,
                Audit {
                    action: "settings.update",
                    target_type: "company_settings",
                    target_id: None,
                    detail: Some(serde_json::json!({ "changed": changed })),
                    ctx: Some(ctx),
                },
            )
            .await?;
            let saved = found(settings::get(conn).await?)?;
            Ok(dto(&saved))
        })
        .await
}

fn holiday_dto(holiday: Holiday) -> HolidayDto {
    HolidayDto {
        date: holiday.date,
        name: holiday.name,
    }
}

fn year_range(year: i32) -> ServiceResult<DateRange> {
    let out_of_range = || DomainError::validation("year", ValidationReason::OutOfRange);
    if !YEARS.contains(&year) {
        return Err(out_of_range().into());
    }
    let day = |month, day| NaiveDate::from_ymd_opt(year, month, day).ok_or_else(out_of_range);
    Ok(DateRange::new(day(1, 1)?, day(12, 31)?)?)
}

/// 한 해의 공휴일(날짜순). 연도를 생략하면 회사 시간대 기준 올해.
pub async fn list_holidays(
    state: &AppState,
    _ctx: &AuthContext,
    query: &HolidayQuery,
) -> ServiceResult<Vec<HolidayDto>> {
    let mut conn = state.read().await?;
    let year = match query.year {
        Some(year) => year,
        None => Company::load(&mut conn).await?.today(state.now()).year(),
    };
    let holidays = settings::list_holidays(&mut conn, year_range(year)?).await?;
    Ok(holidays.into_iter().map(holiday_dto).collect())
}

async fn find_holiday(conn: &mut Conn, date: NaiveDate) -> ServiceResult<Option<Holiday>> {
    Ok(settings::list_holidays(conn, DateRange::single(date))
        .await?
        .into_iter()
        .next())
}

/// 공휴일을 넣거나 이름을 바꾼다. 같은 값으로 다시 보내면 아무것도 바꾸지 않고 성공한다.
pub async fn upsert_holiday(
    state: &AppState,
    ctx: &AuthContext,
    date: &str,
    input: &HolidayInput,
) -> ServiceResult<HolidayDto> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let holiday = Holiday {
        date: parse_date("date", date)?,
        name: LabelName::parse(&input.name)?.into_string(),
    };
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            let previous = find_holiday(conn, holiday.date).await?;
            if previous.as_ref() != Some(&holiday) {
                settings::upsert_holiday(conn, &holiday).await?;
                audit(
                    conn,
                    now,
                    Audit {
                        action: "holiday.upsert",
                        target_type: "holiday",
                        target_id: Some(codec::date_text(holiday.date)),
                        detail: Some(serde_json::json!({
                            "name": holiday.name,
                            "previous_name": previous.map(|h| h.name),
                        })),
                        ctx: Some(ctx),
                    },
                )
                .await?;
            }
            Ok(holiday_dto(holiday.clone()))
        })
        .await
}

/// 공휴일을 지운다. 없는 날짜여도 성공한다(이미 원하는 상태).
pub async fn delete_holiday(state: &AppState, ctx: &AuthContext, date: &str) -> ServiceResult<()> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    let date = parse_date("date", date)?;
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            let Some(previous) = find_holiday(conn, date).await? else {
                return Ok(());
            };
            settings::delete_holiday(conn, date).await?;
            audit(
                conn,
                now,
                Audit {
                    action: "holiday.delete",
                    target_type: "holiday",
                    target_id: Some(codec::date_text(previous.date)),
                    detail: Some(serde_json::json!({ "name": previous.name })),
                    ctx: Some(ctx),
                },
            )
            .await
        })
        .await
}

/// 공휴일 일괄 등록. 항목을 모두 검증한 뒤 한 트랜잭션으로 넣으므로, 하나라도 틀리면
/// 아무것도 들어가지 않는다. 같은 날짜가 두 번 있으면 어느 이름이 맞는지 알 수 없어 거부한다.
pub async fn import_holidays(
    state: &AppState,
    ctx: &AuthContext,
    request: &HolidayImportRequest,
) -> ServiceResult<HolidayImportResult> {
    authz::require_admin(&ctx.actor)?;
    let now = state.now();
    if request.items.len() > MAX_HOLIDAY_IMPORT {
        return Err(DomainError::validation(
            "items",
            ValidationReason::TooLong {
                max: MAX_HOLIDAY_IMPORT,
            },
        )
        .into());
    }
    let mut dates = BTreeSet::new();
    let mut holidays = Vec::with_capacity(request.items.len());
    for item in &request.items {
        let holiday = Holiday {
            date: parse_date("date", &item.date)?,
            name: LabelName::parse(&item.name)?.into_string(),
        };
        if !dates.insert(holiday.date) {
            return Err(DomainError::validation("date", ValidationReason::Duplicate).into());
        }
        holidays.push(holiday);
    }
    let imported = u32::try_from(holidays.len()).map_err(ServiceError::internal)?;
    state
        .write(async |conn: &mut Conn| {
            require_current_admin(conn, ctx).await?;
            for holiday in &holidays {
                settings::upsert_holiday(conn, holiday).await?;
            }
            let dates: Vec<String> = request.items.iter().map(|i| i.date.clone()).collect();
            audit(
                conn,
                now,
                Audit {
                    action: "holiday.import",
                    target_type: "holiday",
                    target_id: None,
                    detail: Some(serde_json::json!({ "count": imported, "dates": dates })),
                    ctx: Some(ctx),
                },
            )
            .await
        })
        .await?;
    Ok(HolidayImportResult { imported })
}
