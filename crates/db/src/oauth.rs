//! 원격 MCP용 OAuth 2.1: 클라이언트, 인가 요청, 인가 코드, 토큰. 비밀 값은 해시만 다룬다.

use pango_core::id::SessionId;
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Client {
    pub client_id: String,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
}

/// 같은 ID면 그대로 둔다(ID가 이름·주소로 정해지므로 내용도 같다).
pub async fn upsert_client(conn: &mut Conn, client: &Client, now: Timestamp) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO oauth_clients (client_id, client_name, redirect_uris, created_at)
         VALUES (?, ?, ?, ?) ON CONFLICT (client_id) DO NOTHING",
    )
    .bind(&client.client_id)
    .bind(&client.client_name)
    .bind(codec::json_text("redirect_uris", &client.redirect_uris)?)
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn find_client(conn: &mut Conn, client_id: &str) -> DbResult<Option<Client>> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT client_id, client_name, redirect_uris FROM oauth_clients WHERE client_id = ?",
    )
    .bind(client_id)
    .fetch_optional(conn)
    .await?;
    row.map(|(client_id, client_name, uris)| {
        Ok(Client {
            client_id,
            client_name,
            redirect_uris: codec::json("redirect_uris", &uris)?,
        })
    })
    .transpose()
}

/// 동의 화면을 여는 동안의 인가 요청.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRequest {
    pub id: String,
    pub nonce_hash: [u8; 32],
    pub client_id: String,
    pub redirect_uri: String,
    pub scope: String,
    pub state: Option<String>,
    pub code_challenge: String,
    pub expires_at: Timestamp,
}

pub async fn insert_request(conn: &mut Conn, request: &AuthRequest) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO oauth_requests
            (id, nonce_hash, client_id, redirect_uri, scope, state, code_challenge, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&request.id)
    .bind(request.nonce_hash.as_slice())
    .bind(&request.client_id)
    .bind(&request.redirect_uri)
    .bind(&request.scope)
    .bind(&request.state)
    .bind(&request.code_challenge)
    .bind(request.expires_at.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

type RequestRow = (
    String,
    Vec<u8>,
    String,
    String,
    String,
    Option<String>,
    String,
    i64,
);

/// 아직 유효한 인가 요청.
pub async fn find_request(
    conn: &mut Conn,
    id: &str,
    now: Timestamp,
) -> DbResult<Option<AuthRequest>> {
    let row: Option<RequestRow> = sqlx::query_as(
        "SELECT id, nonce_hash, client_id, redirect_uri, scope, state, code_challenge, expires_at
         FROM oauth_requests WHERE id = ? AND expires_at > ?",
    )
    .bind(id)
    .bind(now.as_millis())
    .fetch_optional(conn)
    .await?;
    row.map(
        |(id, nonce, client_id, redirect_uri, scope, state, code_challenge, expires_at)| {
            Ok(AuthRequest {
                id,
                nonce_hash: nonce.try_into().map_err(|_| {
                    codec::corrupt("nonce_hash")(pango_core::DomainError::validation(
                        "nonce_hash",
                        pango_core::ValidationReason::InvalidFormat,
                    ))
                })?,
                client_id,
                redirect_uri,
                scope,
                state,
                code_challenge,
                expires_at: codec::timestamp("expires_at", expires_at)?,
            })
        },
    )
    .transpose()
}

/// 끝난(승인·거부한) 인가 요청을 지운다. 지웠으면 `true`(두 번 쓰지 못하게).
pub async fn delete_request(conn: &mut Conn, id: &str) -> DbResult<bool> {
    let result = sqlx::query("DELETE FROM oauth_requests WHERE id = ?")
        .bind(id)
        .execute(conn)
        .await?;
    Ok(result.rows_affected() == 1)
}

/// 인가 코드.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub code_hash: [u8; 32],
    pub client_id: String,
    pub session_id: SessionId,
    pub redirect_uri: String,
    pub scope: String,
    pub code_challenge: String,
    pub expires_at: Timestamp,
}

pub async fn insert_code(conn: &mut Conn, code: &Code) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO oauth_codes
            (code_hash, client_id, session_id, redirect_uri, scope, code_challenge, expires_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(code.code_hash.as_slice())
    .bind(&code.client_id)
    .bind(code.session_id.to_string())
    .bind(&code.redirect_uri)
    .bind(&code.scope)
    .bind(&code.code_challenge)
    .bind(code.expires_at.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

/// `(client_id, session_id, redirect_uri, scope, code_challenge, expires_at, used_at)`.
type CodeRow = (String, String, String, String, String, i64, Option<i64>);

/// 코드를 한 번 쓴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeUse {
    /// 처음 쓴다(이제 쓴 것으로 표시됐다).
    Fresh(Code),
    /// 이미 쓴 코드다. 그 세션을 끊어야 한다(코드 탈취 가능성).
    Reused(SessionId),
    /// 없거나 만료됐다.
    Invalid,
}

pub async fn use_code(conn: &mut Conn, hash: &[u8; 32], now: Timestamp) -> DbResult<CodeUse> {
    let row: Option<CodeRow> = sqlx::query_as(
        "SELECT client_id, session_id, redirect_uri, scope, code_challenge, expires_at, used_at
         FROM oauth_codes WHERE code_hash = ?",
    )
    .bind(hash.as_slice())
    .fetch_optional(&mut *conn)
    .await?;
    let Some((client_id, session_id, redirect_uri, scope, code_challenge, expires_at, used_at)) =
        row
    else {
        return Ok(CodeUse::Invalid);
    };
    let session_id: SessionId = codec::parse("session_id", &session_id)?;
    if used_at.is_some() {
        return Ok(CodeUse::Reused(session_id));
    }
    if expires_at <= now.as_millis() {
        return Ok(CodeUse::Invalid);
    }
    let marked =
        sqlx::query("UPDATE oauth_codes SET used_at = ? WHERE code_hash = ? AND used_at IS NULL")
            .bind(now.as_millis())
            .bind(hash.as_slice())
            .execute(conn)
            .await?;
    if marked.rows_affected() != 1 {
        return Ok(CodeUse::Reused(session_id));
    }
    Ok(CodeUse::Fresh(Code {
        code_hash: *hash,
        client_id,
        session_id,
        redirect_uri,
        scope,
        code_challenge,
        expires_at: codec::timestamp("expires_at", expires_at)?,
    }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Access,
    Refresh,
}

impl TokenKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Refresh => "refresh",
        }
    }
}

/// 토큰 한 개(해시로 저장).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub token_hash: [u8; 32],
    pub kind: TokenKind,
    pub session_id: SessionId,
    pub client_id: String,
    pub scope: String,
    pub expires_at: Timestamp,
}

pub async fn insert_token(conn: &mut Conn, token: &Token) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO oauth_tokens (token_hash, kind, session_id, client_id, scope, expires_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(token.token_hash.as_slice())
    .bind(token.kind.as_str())
    .bind(token.session_id.to_string())
    .bind(&token.client_id)
    .bind(&token.scope)
    .bind(token.expires_at.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

/// 유효한 접근 토큰. `(세션, 클라이언트, scope)`.
pub async fn find_access(
    conn: &mut Conn,
    hash: &[u8; 32],
    now: Timestamp,
) -> DbResult<Option<(SessionId, String, String)>> {
    let row: Option<(String, String, String)> = sqlx::query_as(
        "SELECT session_id, client_id, scope FROM oauth_tokens
         WHERE token_hash = ? AND kind = 'access' AND expires_at > ?",
    )
    .bind(hash.as_slice())
    .bind(now.as_millis())
    .fetch_optional(conn)
    .await?;
    row.map(|(session, client, scope)| Ok((codec::parse("session_id", &session)?, client, scope)))
        .transpose()
}

/// 갱신 토큰을 한 번 쓴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshUse {
    Fresh(Token),
    /// 이미 쓴 갱신 토큰이 다시 왔다. `used_at`은 처음 쓴 시각(병렬 갱신·재시도와 탈취를
    /// 가르는 데 쓴다).
    Reused {
        session_id: SessionId,
        used_at: Timestamp,
    },
    Invalid,
}

pub async fn use_refresh(conn: &mut Conn, hash: &[u8; 32], now: Timestamp) -> DbResult<RefreshUse> {
    let row: Option<(String, String, String, i64, Option<i64>)> = sqlx::query_as(
        "SELECT session_id, client_id, scope, expires_at, used_at FROM oauth_tokens
         WHERE token_hash = ? AND kind = 'refresh'",
    )
    .bind(hash.as_slice())
    .fetch_optional(&mut *conn)
    .await?;
    let Some((session_id, client_id, scope, expires_at, used_at)) = row else {
        return Ok(RefreshUse::Invalid);
    };
    let session_id: SessionId = codec::parse("session_id", &session_id)?;
    if let Some(used_at) = used_at {
        return Ok(RefreshUse::Reused {
            session_id,
            used_at: codec::timestamp("used_at", used_at)?,
        });
    }
    if expires_at <= now.as_millis() {
        return Ok(RefreshUse::Invalid);
    }
    let marked =
        sqlx::query("UPDATE oauth_tokens SET used_at = ? WHERE token_hash = ? AND used_at IS NULL")
            .bind(now.as_millis())
            .bind(hash.as_slice())
            .execute(conn)
            .await?;
    if marked.rows_affected() != 1 {
        return Ok(RefreshUse::Reused {
            session_id,
            used_at: now,
        });
    }
    Ok(RefreshUse::Fresh(Token {
        token_hash: *hash,
        kind: TokenKind::Refresh,
        session_id,
        client_id,
        scope,
        expires_at: codec::timestamp("expires_at", expires_at)?,
    }))
}

/// 만료된 인가 요청·코드와 오래 지난 토큰, 쓰이지 않는 클라이언트를 지운다. 지운 줄 수.
pub async fn purge(conn: &mut Conn, now: Timestamp, older_than: Timestamp) -> DbResult<u64> {
    let mut removed = 0;
    removed += sqlx::query("DELETE FROM oauth_requests WHERE expires_at <= ?")
        .bind(now.as_millis())
        .execute(&mut *conn)
        .await?
        .rows_affected();
    removed += sqlx::query("DELETE FROM oauth_codes WHERE expires_at <= ?")
        .bind(older_than.as_millis())
        .execute(&mut *conn)
        .await?
        .rows_affected();
    removed += sqlx::query("DELETE FROM oauth_tokens WHERE expires_at <= ?")
        .bind(older_than.as_millis())
        .execute(&mut *conn)
        .await?
        .rows_affected();
    // 코드·토큰·요청이 하나도 남지 않은 클라이언트 중, 승인을 받은 적 없이 하루가 지났거나
    // 마지막 승인에서 1년이 지난 것. 승인을 받은 클라이언트는 claude.ai가 client_id를 다시 쓴다.
    let stale = older_than
        .checked_add(-chrono::Duration::days(365))
        .unwrap_or(older_than);
    removed += sqlx::query(
        "DELETE FROM oauth_clients
         WHERE ((last_used_at IS NULL AND created_at <= ?1) OR last_used_at <= ?2)
           AND NOT EXISTS (SELECT 1 FROM oauth_tokens t WHERE t.client_id = oauth_clients.client_id)
           AND NOT EXISTS (SELECT 1 FROM oauth_codes c WHERE c.client_id = oauth_clients.client_id)
           AND NOT EXISTS (SELECT 1 FROM oauth_requests r WHERE r.client_id = oauth_clients.client_id)",
    )
    .bind(older_than.as_millis())
    .bind(stale.as_millis())
    .execute(conn)
    .await?
    .rows_affected();
    Ok(removed)
}

/// 클라이언트가 승인을 받았다고 표시한다(정리 작업이 오래 남겨 둔다).
pub async fn mark_client_used(conn: &mut Conn, client_id: &str, now: Timestamp) -> DbResult<()> {
    sqlx::query("UPDATE oauth_clients SET last_used_at = ? WHERE client_id = ?")
        .bind(now.as_millis())
        .bind(client_id)
        .execute(conn)
        .await?;
    Ok(())
}
