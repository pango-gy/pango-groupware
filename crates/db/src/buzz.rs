//! 사내 메신저 Buzz 채널: 켜기·끄기(단일 행)와 구성원별 계정 연결.

use pango_core::id::MemberId;
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuzzSettings {
    pub enabled: bool,
    pub version: i64,
    pub updated_at: Timestamp,
}

pub async fn settings(conn: &mut Conn) -> DbResult<BuzzSettings> {
    let (enabled, version, updated_at): (bool, i64, i64) =
        sqlx::query_as("SELECT enabled, version, updated_at FROM buzz_settings WHERE id = 1")
            .fetch_one(conn)
            .await?;
    Ok(BuzzSettings {
        enabled,
        version,
        updated_at: codec::timestamp("updated_at", updated_at)?,
    })
}

/// `version`이 같을 때만 바꾸고 버전을 올린다. 바꿨으면 `true`.
pub async fn update_settings(
    conn: &mut Conn,
    enabled: bool,
    version: i64,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE buzz_settings SET enabled = ?, updated_at = ?, version = version + 1
         WHERE id = 1 AND version = ?",
    )
    .bind(enabled)
    .bind(now.as_millis())
    .bind(version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    Pending,
    Verified,
}

/// 구성원의 Buzz 계정 연결. 코드 해시는 `Debug`에 드러나지 않는다.
#[derive(Clone, PartialEq, Eq)]
pub struct BuzzAccount {
    pub member_id: MemberId,
    /// 16진수 64자.
    pub pubkey: String,
    pub status: AccountStatus,
    pub code_hash: Option<[u8; 32]>,
    pub code_expires_at: Option<Timestamp>,
    pub code_attempts: u32,
    pub code_sent_at: Option<Timestamp>,
    pub send_window_start: Option<Timestamp>,
    pub sends_in_window: u32,
    pub dm_channel_id: Option<String>,
    pub verified_at: Option<Timestamp>,
}

impl std::fmt::Debug for BuzzAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuzzAccount")
            .field("member_id", &self.member_id)
            .field("status", &self.status)
            .field("has_code", &self.code_hash.is_some())
            .field("code_attempts", &self.code_attempts)
            .finish_non_exhaustive()
    }
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    member_id: String,
    pubkey: String,
    status: String,
    code_hash: Option<Vec<u8>>,
    code_expires_at: Option<i64>,
    code_attempts: i64,
    code_sent_at: Option<i64>,
    send_window_start: Option<i64>,
    sends_in_window: i64,
    dm_channel_id: Option<String>,
    verified_at: Option<i64>,
}

impl TryFrom<AccountRow> for BuzzAccount {
    type Error = crate::DbError;

    fn try_from(row: AccountRow) -> DbResult<Self> {
        let status = match row.status.as_str() {
            "verified" => AccountStatus::Verified,
            _ => AccountStatus::Pending,
        };
        let code_hash = match row.code_hash {
            None => None,
            Some(bytes) => Some(bytes.try_into().map_err(|_| {
                codec::corrupt("code_hash")(pango_core::DomainError::validation(
                    "code_hash",
                    pango_core::ValidationReason::InvalidFormat,
                ))
            })?),
        };
        Ok(Self {
            member_id: codec::parse("member_id", &row.member_id)?,
            pubkey: row.pubkey,
            status,
            code_hash,
            code_expires_at: codec::timestamp_opt("code_expires_at", row.code_expires_at)?,
            code_attempts: codec::narrow("code_attempts", row.code_attempts)?,
            code_sent_at: codec::timestamp_opt("code_sent_at", row.code_sent_at)?,
            send_window_start: codec::timestamp_opt("send_window_start", row.send_window_start)?,
            sends_in_window: codec::narrow("sends_in_window", row.sends_in_window)?,
            dm_channel_id: row.dm_channel_id,
            verified_at: codec::timestamp_opt("verified_at", row.verified_at)?,
        })
    }
}

const COLUMNS: &str = "member_id, pubkey, status, code_hash, code_expires_at, code_attempts,
     code_sent_at, send_window_start, sends_in_window, dm_channel_id, verified_at";

pub async fn find(conn: &mut Conn, member_id: MemberId) -> DbResult<Option<BuzzAccount>> {
    sqlx::query_as::<_, AccountRow>(&format!(
        "SELECT {COLUMNS} FROM buzz_accounts WHERE member_id = ?"
    ))
    .bind(member_id.to_string())
    .fetch_optional(conn)
    .await?
    .map(BuzzAccount::try_from)
    .transpose()
}

/// 이 공개키를 확인한 구성원(있으면).
pub async fn verified_owner(conn: &mut Conn, pubkey: &str) -> DbResult<Option<MemberId>> {
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT member_id FROM buzz_accounts WHERE pubkey = ? AND status = 'verified'",
    )
    .bind(pubkey)
    .fetch_optional(&mut *conn)
    .await?;
    owner.map(|id| codec::parse("member_id", &id)).transpose()
}

/// 새 확인 코드를 건다. 다른 공개키로 바꾸면 이전 연결(확인 여부·DM 채널)은 없어진다.
pub struct NewCode<'a> {
    pub member_id: MemberId,
    pub pubkey: &'a str,
    pub code_hash: [u8; 32],
    pub expires_at: Timestamp,
    pub window_start: Timestamp,
    pub sends_in_window: u32,
    /// 같은 공개키로 다시 보낼 때 그대로 둘 DM 채널.
    pub dm_channel_id: Option<&'a str>,
    pub now: Timestamp,
}

pub async fn put_code(conn: &mut Conn, code: &NewCode<'_>) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO buzz_accounts
            (member_id, pubkey, status, code_hash, code_expires_at, code_attempts, code_sent_at,
             send_window_start, sends_in_window, dm_channel_id, verified_at, created_at,
             updated_at)
         VALUES (?1, ?2, 'pending', ?3, ?4, 0, ?5, ?6, ?7, ?8, NULL, ?5, ?5)
         ON CONFLICT (member_id) DO UPDATE SET
            pubkey = excluded.pubkey, status = 'pending', code_hash = excluded.code_hash,
            code_expires_at = excluded.code_expires_at, code_attempts = 0,
            code_sent_at = excluded.code_sent_at, send_window_start = excluded.send_window_start,
            sends_in_window = excluded.sends_in_window, dm_channel_id = excluded.dm_channel_id,
            verified_at = NULL, updated_at = excluded.updated_at",
    )
    .bind(code.member_id.to_string())
    .bind(code.pubkey)
    .bind(code.code_hash.as_slice())
    .bind(code.expires_at.as_millis())
    .bind(code.now.as_millis())
    .bind(code.window_start.as_millis())
    .bind(i64::from(code.sends_in_window))
    .bind(code.dm_channel_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// 관리자가 정한 공개키로 바로 확인된 연결을 만든다. 같은 공개키면 열어 둔 DM 채널을 그대로
/// 두고, 다른 공개키면 비운다. 확인 코드는 지운다(재발송 제한 기록은 남긴다).
pub async fn put_verified(
    conn: &mut Conn,
    member_id: MemberId,
    pubkey: &str,
    now: Timestamp,
) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO buzz_accounts
            (member_id, pubkey, status, code_attempts, sends_in_window, verified_at, created_at,
             updated_at)
         VALUES (?1, ?2, 'verified', 0, 0, ?3, ?3, ?3)
         ON CONFLICT (member_id) DO UPDATE SET
            dm_channel_id = CASE WHEN buzz_accounts.pubkey = excluded.pubkey
                                 THEN buzz_accounts.dm_channel_id END,
            pubkey = excluded.pubkey, status = 'verified', code_hash = NULL,
            code_expires_at = NULL, code_attempts = 0, verified_at = excluded.verified_at,
            updated_at = excluded.updated_at",
    )
    .bind(member_id.to_string())
    .bind(pubkey)
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

/// 틀린 코드 입력을 센다.
pub async fn count_wrong_code(
    conn: &mut Conn,
    member_id: MemberId,
    now: Timestamp,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE buzz_accounts SET code_attempts = code_attempts + 1, updated_at = ?
         WHERE member_id = ? AND status = 'pending'",
    )
    .bind(now.as_millis())
    .bind(member_id.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// 확인을 마친다(코드를 지운다).
pub async fn mark_verified(conn: &mut Conn, member_id: MemberId, now: Timestamp) -> DbResult<()> {
    sqlx::query(
        "UPDATE buzz_accounts SET status = 'verified', verified_at = ?1, code_hash = NULL,
             code_expires_at = NULL, code_attempts = 0, updated_at = ?1
         WHERE member_id = ?2 AND status = 'pending'",
    )
    .bind(now.as_millis())
    .bind(member_id.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// 연결한 공개키가 그대로일 때만 DM 채널 ID를 저장한다(그사이 다른 계정으로 바꿨을 수 있다).
pub async fn cache_dm_channel(
    conn: &mut Conn,
    member_id: MemberId,
    pubkey: &str,
    channel_id: &str,
) -> DbResult<()> {
    sqlx::query("UPDATE buzz_accounts SET dm_channel_id = ? WHERE member_id = ? AND pubkey = ?")
        .bind(channel_id)
        .bind(member_id.to_string())
        .bind(pubkey)
        .execute(conn)
        .await?;
    Ok(())
}

/// 연결을 지운다. 지웠으면 그 계정.
pub async fn remove(conn: &mut Conn, member_id: MemberId) -> DbResult<Option<BuzzAccount>> {
    let found = find(conn, member_id).await?;
    if found.is_some() {
        sqlx::query("DELETE FROM buzz_accounts WHERE member_id = ?")
            .bind(member_id.to_string())
            .execute(conn)
            .await?;
    }
    Ok(found)
}

pub async fn count_verified(conn: &mut Conn) -> DbResult<i64> {
    Ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM buzz_accounts WHERE status = 'verified'")
            .fetch_one(conn)
            .await?,
    )
}

/// 같은 공개키를 다른 구성원이 이미 확인했으면(유일 색인 위반) `true`.
pub fn is_duplicate_pubkey(error: &crate::DbError) -> bool {
    error.is_unique_violation("buzz_accounts.pubkey")
}
