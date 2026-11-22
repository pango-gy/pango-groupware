//! 2단계 인증(TOTP) 설정과 복구 코드.

use pango_core::id::MemberId;
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

#[derive(Clone, PartialEq, Eq)]
pub struct Totp {
    pub secret: Vec<u8>,
    pub confirmed_at: Option<Timestamp>,
    pub last_step: Option<i64>,
}

// 비밀키가 로그에 찍히지 않게 한다.
impl std::fmt::Debug for Totp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Totp")
            .field("confirmed_at", &self.confirmed_at)
            .field("last_step", &self.last_step)
            .finish_non_exhaustive()
    }
}

impl Totp {
    pub fn enabled(&self) -> bool {
        self.confirmed_at.is_some()
    }
}

pub async fn find(conn: &mut Conn, member_id: MemberId) -> DbResult<Option<Totp>> {
    let row: Option<(Vec<u8>, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT secret, confirmed_at, last_step FROM member_totp WHERE member_id = ?",
    )
    .bind(member_id.to_string())
    .fetch_optional(conn)
    .await?;
    row.map(|(secret, confirmed_at, last_step)| {
        Ok(Totp {
            secret,
            confirmed_at: codec::timestamp_opt("confirmed_at", confirmed_at)?,
            last_step,
        })
    })
    .transpose()
}

/// 켜는 중인 비밀키를 둔다(이미 켜져 있으면 바꾸지 않는다). 넣었거나 바꿨으면 `true`.
pub async fn put_pending(
    conn: &mut Conn,
    member_id: MemberId,
    secret: &[u8],
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "INSERT INTO member_totp (member_id, secret, created_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (member_id) DO UPDATE SET secret = excluded.secret,
             created_at = excluded.created_at, last_step = NULL
         WHERE member_totp.confirmed_at IS NULL",
    )
    .bind(member_id.to_string())
    .bind(secret)
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 첫 코드를 확인해 켠다. 켜는 중일 때만. 켰으면 `true`.
pub async fn confirm(
    conn: &mut Conn,
    member_id: MemberId,
    step: i64,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE member_totp SET confirmed_at = ?, last_step = ?
         WHERE member_id = ? AND confirmed_at IS NULL",
    )
    .bind(now.as_millis())
    .bind(step)
    .bind(member_id.to_string())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 받은 코드의 단계를 기록한다. 그보다 이전 단계일 때만(같은 코드를 두 번 쓰지 못하게). 기록했으면 `true`.
pub async fn use_step(conn: &mut Conn, member_id: MemberId, step: i64) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE member_totp SET last_step = ?1
         WHERE member_id = ?2 AND confirmed_at IS NOT NULL
           AND (last_step IS NULL OR last_step < ?1)",
    )
    .bind(step)
    .bind(member_id.to_string())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 끈다(복구 코드도 지운다). 지웠으면 `true`.
pub async fn remove(conn: &mut Conn, member_id: MemberId) -> DbResult<bool> {
    sqlx::query("DELETE FROM member_recovery_codes WHERE member_id = ?")
        .bind(member_id.to_string())
        .execute(&mut *conn)
        .await?;
    let result = sqlx::query("DELETE FROM member_totp WHERE member_id = ?")
        .bind(member_id.to_string())
        .execute(conn)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// 복구 코드를 새로 둔다(이전 코드는 모두 지운다).
pub async fn replace_recovery_codes(
    conn: &mut Conn,
    member_id: MemberId,
    hashes: &[[u8; 32]],
) -> DbResult<()> {
    sqlx::query("DELETE FROM member_recovery_codes WHERE member_id = ?")
        .bind(member_id.to_string())
        .execute(&mut *conn)
        .await?;
    for hash in hashes {
        sqlx::query("INSERT INTO member_recovery_codes (member_id, code_hash) VALUES (?, ?)")
            .bind(member_id.to_string())
            .bind(hash.as_slice())
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// 쓰지 않은 복구 코드면 쓴 것으로 표시한다. 표시했으면 `true`.
pub async fn use_recovery_code(
    conn: &mut Conn,
    member_id: MemberId,
    hash: &[u8; 32],
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE member_recovery_codes SET used_at = ?
         WHERE member_id = ? AND code_hash = ? AND used_at IS NULL",
    )
    .bind(now.as_millis())
    .bind(member_id.to_string())
    .bind(hash.as_slice())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 남은(쓰지 않은) 복구 코드 수.
pub async fn recovery_codes_left(conn: &mut Conn, member_id: MemberId) -> DbResult<i64> {
    Ok(sqlx::query_scalar(
        "SELECT COUNT(*) FROM member_recovery_codes WHERE member_id = ? AND used_at IS NULL",
    )
    .bind(member_id.to_string())
    .fetch_one(conn)
    .await?)
}
