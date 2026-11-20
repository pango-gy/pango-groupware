//! 회사 설정(단일 행)과 공휴일.

use chrono::NaiveDate;
use pango_core::calendar::{DateRange, WorkWeek};
use pango_core::time::{MinuteOfDay, Timestamp};

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompanySettings {
    pub company_name: String,
    /// IANA 시간대 이름.
    pub timezone: String,
    pub day_cutoff: MinuteOfDay,
    pub work_start: MinuteOfDay,
    pub work_end: MinuteOfDay,
    pub late_grace_minutes: u16,
    pub half_day_split: MinuteOfDay,
    pub quarter_minutes: u16,
    /// `(근무 구간 분, 휴게 분)` 목록.
    pub break_tiers: Vec<(u16, u16)>,
    pub workdays: WorkWeek,
    pub version: i64,
    pub updated_at: Timestamp,
}

#[derive(sqlx::FromRow)]
struct SettingsRow {
    company_name: String,
    timezone: String,
    day_cutoff_min: i64,
    work_start_min: i64,
    work_end_min: i64,
    late_grace_min: i64,
    half_day_split_min: i64,
    quarter_min: i64,
    break_tiers: String,
    workdays: i64,
    version: i64,
    updated_at: i64,
}

fn minute(column: &'static str, value: i64) -> DbResult<MinuteOfDay> {
    MinuteOfDay::new(u16::try_from(value).unwrap_or(u16::MAX)).map_err(codec::corrupt(column))
}

fn small(value: i64) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

impl TryFrom<SettingsRow> for CompanySettings {
    type Error = crate::DbError;

    fn try_from(row: SettingsRow) -> DbResult<Self> {
        Ok(Self {
            company_name: row.company_name,
            timezone: row.timezone,
            day_cutoff: minute("day_cutoff_min", row.day_cutoff_min)?,
            work_start: minute("work_start_min", row.work_start_min)?,
            work_end: minute("work_end_min", row.work_end_min)?,
            late_grace_minutes: small(row.late_grace_min),
            half_day_split: minute("half_day_split_min", row.half_day_split_min)?,
            quarter_minutes: small(row.quarter_min),
            break_tiers: codec::json("break_tiers", &row.break_tiers)?,
            workdays: WorkWeek::from_bits(u8::try_from(row.workdays).unwrap_or(0))
                .map_err(codec::corrupt("workdays"))?,
            version: row.version,
            updated_at: codec::timestamp("updated_at", row.updated_at)?,
        })
    }
}

pub async fn get(conn: &mut Conn) -> DbResult<Option<CompanySettings>> {
    sqlx::query_as::<_, SettingsRow>(
        "SELECT company_name, timezone, day_cutoff_min, work_start_min, work_end_min,
                late_grace_min, half_day_split_min, quarter_min, break_tiers, workdays,
                version, updated_at
         FROM company_settings WHERE id = 1",
    )
    .fetch_optional(conn)
    .await?
    .map(CompanySettings::try_from)
    .transpose()
}

/// 설정을 처음 만든다. 이미 있으면 유일 제약 위반.
pub async fn insert(conn: &mut Conn, settings: &CompanySettings) -> DbResult<()> {
    bind_values(
        sqlx::query(
            "INSERT INTO company_settings
                (company_name, timezone, day_cutoff_min, work_start_min, work_end_min,
                 late_grace_min, half_day_split_min, quarter_min, break_tiers, workdays,
                 updated_at, id, version)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 1)",
        ),
        settings,
    )?
    .execute(conn)
    .await?;
    Ok(())
}

/// `settings.version`이 현재 값과 같을 때만 갱신한다. 갱신했으면 `true`.
pub async fn update(conn: &mut Conn, settings: &CompanySettings) -> DbResult<bool> {
    let result = bind_values(
        sqlx::query(
            "UPDATE company_settings SET
                company_name = ?, timezone = ?, day_cutoff_min = ?, work_start_min = ?,
                work_end_min = ?, late_grace_min = ?, half_day_split_min = ?, quarter_min = ?,
                break_tiers = ?, workdays = ?, updated_at = ?, version = version + 1
             WHERE id = 1 AND version = ?",
        ),
        settings,
    )?
    .bind(settings.version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

type Query<'q> = sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>;

fn bind_values<'q>(query: Query<'q>, s: &'q CompanySettings) -> DbResult<Query<'q>> {
    Ok(query
        .bind(&s.company_name)
        .bind(&s.timezone)
        .bind(i64::from(s.day_cutoff.minutes()))
        .bind(i64::from(s.work_start.minutes()))
        .bind(i64::from(s.work_end.minutes()))
        .bind(i64::from(s.late_grace_minutes))
        .bind(i64::from(s.half_day_split.minutes()))
        .bind(i64::from(s.quarter_minutes))
        .bind(codec::json_text("break_tiers", &s.break_tiers)?)
        .bind(i64::from(s.workdays.bits()))
        .bind(s.updated_at.as_millis()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holiday {
    pub date: NaiveDate,
    pub name: String,
}

pub async fn list_holidays(conn: &mut Conn, range: DateRange) -> DbResult<Vec<Holiday>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT date, name FROM holidays WHERE date BETWEEN ? AND ? ORDER BY date",
    )
    .bind(codec::date_text(range.start()))
    .bind(codec::date_text(range.end()))
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|(date, name)| Ok(Holiday { date: codec::date("date", &date)?, name }))
        .collect()
}

/// 공휴일을 넣거나 이름을 바꾼다(같은 날짜는 덮어쓴다).
pub async fn upsert_holiday(conn: &mut Conn, holiday: &Holiday) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO holidays (date, name) VALUES (?, ?)
         ON CONFLICT (date) DO UPDATE SET name = excluded.name",
    )
    .bind(codec::date_text(holiday.date))
    .bind(&holiday.name)
    .execute(conn)
    .await?;
    Ok(())
}

/// 지웠으면 `true`.
pub async fn delete_holiday(conn: &mut Conn, date: NaiveDate) -> DbResult<bool> {
    let result = sqlx::query("DELETE FROM holidays WHERE date = ?")
        .bind(codec::date_text(date))
        .execute(conn)
        .await?;
    Ok(result.rows_affected() == 1)
}
