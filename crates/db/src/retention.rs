//! 기록 보존 기간 설정(단일 행)과 기간이 지난 출퇴근 기록 정리.

use chrono::NaiveDate;
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionSettings {
    /// 출퇴근 기록 보존 연수. `None`이면 지우지 않는다.
    pub attendance_years: Option<u8>,
    pub version: i64,
    pub updated_at: Timestamp,
}

pub async fn settings(conn: &mut Conn) -> DbResult<RetentionSettings> {
    let (years, version, updated_at): (Option<i64>, i64, i64) = sqlx::query_as(
        "SELECT attendance_years, version, updated_at FROM retention_settings WHERE id = 1",
    )
    .fetch_one(conn)
    .await?;
    Ok(RetentionSettings {
        attendance_years: years
            .map(|y| codec::narrow("attendance_years", y))
            .transpose()?,
        version,
        updated_at: codec::timestamp("updated_at", updated_at)?,
    })
}

/// `version`이 같을 때만 바꾸고 버전을 올린다. 바꿨으면 `true`.
pub async fn update_settings(
    conn: &mut Conn,
    attendance_years: Option<u8>,
    version: i64,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE retention_settings SET attendance_years = ?, updated_at = ?, version = version + 1
         WHERE id = 1 AND version = ?",
    )
    .bind(attendance_years.map(i64::from))
    .bind(now.as_millis())
    .bind(version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// `before`보다 이른 근무일의 출퇴근 기록 수(정리 미리보기).
pub async fn count_attendance_before(conn: &mut Conn, before: NaiveDate) -> DbResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM attendance_records WHERE work_date < ?")
            .bind(codec::date_text(before))
            .fetch_one(conn)
            .await?,
    )
}

/// 지운 수.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Purged {
    pub records: u64,
    pub events: u64,
}

/// `before`보다 이른 근무일의 출퇴근 기록과 그 이력을 지운다. 쓰기 트랜잭션 안에서 부른다.
///
/// 이력은 추가 전용(삭제 트리거)이므로 정리 표시(`retention_purge_guard`)를 넣은 동안에만 지울
/// 수 있고, 끝나면 같은 트랜잭션에서 표시를 뺀다.
pub async fn purge_attendance_before(conn: &mut Conn, before: NaiveDate) -> DbResult<Purged> {
    let day = codec::date_text(before);
    sqlx::query("INSERT INTO retention_purge_guard (id) VALUES (1)")
        .execute(&mut *conn)
        .await?;
    let events = sqlx::query(
        "DELETE FROM attendance_events WHERE record_id IN
            (SELECT id FROM attendance_records WHERE work_date < ?)",
    )
    .bind(&day)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    let records = sqlx::query("DELETE FROM attendance_records WHERE work_date < ?")
        .bind(&day)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    sqlx::query("DELETE FROM retention_purge_guard")
        .execute(conn)
        .await?;
    Ok(Purged { records, events })
}
