//! 로그인 세션과 로그인 시도 제한.

use pango_core::id::{MemberId, SessionId};
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

pub const TOKEN_HASH_LEN: usize = 32;
pub type TokenHash = [u8; TOKEN_HASH_LEN];

pango_core::string_enum! {
    /// 세션 토큰이 전달되는 방식.
    pub enum SessionVia("via") {
        /// 웹 브라우저 쿠키.
        Cookie => "cookie",
        /// 모바일 앱의 `Authorization: Bearer`.
        Bearer => "bearer",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: SessionId,
    pub member_id: MemberId,
    pub via: SessionVia,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: Timestamp,
    pub last_seen_at: Timestamp,
    pub idle_expires_at: Timestamp,
    pub absolute_expires_at: Timestamp,
    pub revoked_at: Option<Timestamp>,
}

impl Session {
    /// 폐기되지 않았고 두 만료 시각이 모두 지나지 않았는지.
    pub fn is_valid_at(&self, now: Timestamp) -> bool {
        self.revoked_at.is_none() && now < self.idle_expires_at && now < self.absolute_expires_at
    }
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: String,
    member_id: String,
    via: String,
    user_agent: Option<String>,
    ip: Option<String>,
    created_at: i64,
    last_seen_at: i64,
    idle_expires_at: i64,
    absolute_expires_at: i64,
    revoked_at: Option<i64>,
}

impl TryFrom<SessionRow> for Session {
    type Error = crate::DbError;

    fn try_from(row: SessionRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            member_id: codec::parse("member_id", &row.member_id)?,
            via: codec::parse_with("via", &row.via, SessionVia::parse)?,
            user_agent: row.user_agent,
            ip: row.ip,
            created_at: codec::timestamp("created_at", row.created_at)?,
            last_seen_at: codec::timestamp("last_seen_at", row.last_seen_at)?,
            idle_expires_at: codec::timestamp("idle_expires_at", row.idle_expires_at)?,
            absolute_expires_at: codec::timestamp("absolute_expires_at", row.absolute_expires_at)?,
            revoked_at: codec::timestamp_opt("revoked_at", row.revoked_at)?,
        })
    }
}

const SESSION_COLUMNS: &str = "id, member_id, via, user_agent, ip, created_at, last_seen_at, \
     idle_expires_at, absolute_expires_at, revoked_at";

pub async fn insert(conn: &mut Conn, session: &Session, token_hash: &TokenHash) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO sessions
            (id, member_id, token_hash, via, user_agent, ip, created_at, last_seen_at,
             idle_expires_at, absolute_expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(session.id.to_string())
    .bind(session.member_id.to_string())
    .bind(&token_hash[..])
    .bind(session.via.as_str())
    .bind(&session.user_agent)
    .bind(&session.ip)
    .bind(session.created_at.as_millis())
    .bind(session.last_seen_at.as_millis())
    .bind(session.idle_expires_at.as_millis())
    .bind(session.absolute_expires_at.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn find_by_token_hash(
    conn: &mut Conn,
    token_hash: &TokenHash,
) -> DbResult<Option<Session>> {
    sqlx::query_as::<_, SessionRow>(&format!(
        "SELECT {SESSION_COLUMNS} FROM sessions WHERE token_hash = ?"
    ))
    .bind(&token_hash[..])
    .fetch_optional(conn)
    .await?
    .map(Session::try_from)
    .transpose()
}

pub async fn find(conn: &mut Conn, id: SessionId) -> DbResult<Option<Session>> {
    sqlx::query_as::<_, SessionRow>(&format!(
        "SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?"
    ))
    .bind(id.to_string())
    .fetch_optional(conn)
    .await?
    .map(Session::try_from)
    .transpose()
}

/// 구성원의 유효한 세션 목록(최근 사용 순).
pub async fn list_active(
    conn: &mut Conn,
    member_id: MemberId,
    now: Timestamp,
) -> DbResult<Vec<Session>> {
    sqlx::query_as::<_, SessionRow>(&format!(
        "SELECT {SESSION_COLUMNS} FROM sessions
         WHERE member_id = ? AND revoked_at IS NULL
           AND idle_expires_at > ? AND absolute_expires_at > ?
         ORDER BY last_seen_at DESC, id DESC"
    ))
    .bind(member_id.to_string())
    .bind(now.as_millis())
    .bind(now.as_millis())
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Session::try_from)
    .collect()
}

/// 마지막 사용 시각과 유휴 만료를 늘린다.
pub async fn touch(
    conn: &mut Conn,
    id: SessionId,
    now: Timestamp,
    idle_expires_at: Timestamp,
) -> DbResult<()> {
    sqlx::query(
        "UPDATE sessions SET last_seen_at = ?, idle_expires_at = ?
         WHERE id = ? AND revoked_at IS NULL",
    )
    .bind(now.as_millis())
    .bind(idle_expires_at.as_millis())
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// 해당 구성원의 세션 하나를 폐기한다. 남의 세션은 건드리지 못한다. 폐기했으면 `true`.
pub async fn revoke(
    conn: &mut Conn,
    member_id: MemberId,
    id: SessionId,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE sessions SET revoked_at = ?
         WHERE id = ? AND member_id = ? AND revoked_at IS NULL",
    )
    .bind(now.as_millis())
    .bind(id.to_string())
    .bind(member_id.to_string())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 세션이 (폐기 여부와 무관하게) 해당 구성원의 것인지.
pub async fn belongs_to(conn: &mut Conn, member_id: MemberId, id: SessionId) -> DbResult<bool> {
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT 1 FROM sessions WHERE id = ? AND member_id = ?")
            .bind(id.to_string())
            .bind(member_id.to_string())
            .fetch_optional(conn)
            .await?;
    Ok(row.is_some())
}

/// 구성원의 모든 세션을 폐기한다. `except`가 있으면 그 세션은 남긴다. 폐기한 수를 돌려준다.
pub async fn revoke_all(
    conn: &mut Conn,
    member_id: MemberId,
    except: Option<SessionId>,
    now: Timestamp,
) -> DbResult<u64> {
    let result = sqlx::query(
        "UPDATE sessions SET revoked_at = ?
         WHERE member_id = ? AND revoked_at IS NULL AND (? IS NULL OR id <> ?)",
    )
    .bind(now.as_millis())
    .bind(member_id.to_string())
    .bind(except.map(|id| id.to_string()))
    .bind(except.map(|id| id.to_string()))
    .execute(conn)
    .await?;
    Ok(result.rows_affected())
}

/// 만료되었거나 폐기된 지 오래된 세션을 지운다.
pub async fn purge(conn: &mut Conn, older_than: Timestamp) -> DbResult<u64> {
    let result = sqlx::query(
        "DELETE FROM sessions
         WHERE absolute_expires_at < ? OR idle_expires_at < ?
            OR (revoked_at IS NOT NULL AND revoked_at < ?)",
    )
    .bind(older_than.as_millis())
    .bind(older_than.as_millis())
    .bind(older_than.as_millis())
    .execute(conn)
    .await?;
    Ok(result.rows_affected())
}

/// 시도 제한 카운터의 현재 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Throttle {
    /// 이번 시도를 포함한 창 안의 시도 횟수.
    pub attempts: i64,
    /// 현재 창이 시작된 시각. 창은 여기서 `window`만큼 이어진다.
    pub window_start: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleMode {
    /// 고정 창: 창이 시작된 뒤 일정 시간 동안의 횟수를 센다(IP당 요청 수 제한).
    Fixed,
    /// 잠금: 시도할 때마다 창이 다시 시작되어, 조용한 시간이 `window`만큼 지나야 초기화된다.
    /// 한도를 처음 넘긴 순간부터 `window` 동안 잠기며, 잠긴 동안의 시도는 잠금을 늘리지 않는다.
    Lockout { limit: i64 },
}

/// 시도를 1회 기록하고 현재 상태를 돌려준다.
///
/// 한 문장으로 처리하므로 동시 요청에서도 횟수가 빠지지 않는다.
pub async fn throttle_hit(
    conn: &mut Conn,
    bucket: &str,
    now: Timestamp,
    window_millis: i64,
    mode: ThrottleMode,
) -> DbResult<Throttle> {
    let lockout_limit = match mode {
        ThrottleMode::Fixed => None,
        ThrottleMode::Lockout { limit } => Some(limit),
    };
    let (attempts, window_start): (i64, i64) = sqlx::query_as(
        "INSERT INTO login_throttle (bucket, window_start, attempts) VALUES (?1, ?2, 1)
         ON CONFLICT (bucket) DO UPDATE SET
            attempts = CASE WHEN ?2 - window_start >= ?3 OR ?2 < window_start
                            THEN 1 ELSE attempts + 1 END,
            window_start = CASE
                WHEN ?2 - window_start >= ?3 OR ?2 < window_start THEN ?2
                WHEN ?4 IS NOT NULL AND attempts <= ?4 THEN ?2
                ELSE window_start END
         RETURNING attempts, window_start",
    )
    .bind(bucket)
    .bind(now.as_millis())
    .bind(window_millis)
    .bind(lockout_limit)
    .fetch_one(conn)
    .await?;
    Ok(Throttle {
        attempts,
        window_start: codec::timestamp("window_start", window_start)?,
    })
}

pub async fn throttle_clear(conn: &mut Conn, bucket: &str) -> DbResult<()> {
    sqlx::query("DELETE FROM login_throttle WHERE bucket = ?")
        .bind(bucket)
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn throttle_purge(conn: &mut Conn, older_than: Timestamp) -> DbResult<u64> {
    let result = sqlx::query("DELETE FROM login_throttle WHERE window_start < ?")
        .bind(older_than.as_millis())
        .execute(conn)
        .await?;
    Ok(result.rows_affected())
}
