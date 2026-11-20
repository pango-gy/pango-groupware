//! 멱등성 키 저장소.

use pango_core::id::MemberId;
use pango_core::time::Timestamp;

use crate::error::DbResult;
use crate::pool::Conn;

pub type Fingerprint = [u8; 32];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResponse {
    pub fingerprint: Fingerprint,
    pub status: u16,
    pub body: String,
}

pub async fn find(
    conn: &mut Conn,
    member_id: MemberId,
    key: &str,
) -> DbResult<Option<StoredResponse>> {
    let row: Option<(Vec<u8>, i64, String)> = sqlx::query_as(
        "SELECT fingerprint, status, response FROM idempotency_keys
         WHERE member_id = ? AND key = ?",
    )
    .bind(member_id.to_string())
    .bind(key)
    .fetch_optional(conn)
    .await?;
    Ok(row.and_then(|(fingerprint, status, body)| {
        Some(StoredResponse {
            fingerprint: fingerprint.try_into().ok()?,
            status: u16::try_from(status).ok()?,
            body,
        })
    }))
}

/// 첫 응답을 저장한다. 같은 키가 이미 있으면 유일 제약 위반.
pub async fn insert(
    conn: &mut Conn,
    member_id: MemberId,
    key: &str,
    response: &StoredResponse,
    now: Timestamp,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO idempotency_keys (member_id, key, fingerprint, status, response, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(member_id.to_string())
    .bind(key)
    .bind(&response.fingerprint[..])
    .bind(i64::from(response.status))
    .bind(&response.body)
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn purge(conn: &mut Conn, older_than: Timestamp) -> DbResult<u64> {
    let result = sqlx::query("DELETE FROM idempotency_keys WHERE created_at < ?")
        .bind(older_than.as_millis())
        .execute(conn)
        .await?;
    Ok(result.rows_affected())
}
