//! 앱 내 알림.

use pango_core::id::{MemberId, NotificationId};
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub id: NotificationId,
    pub member_id: MemberId,
    pub kind: String,
    pub title: String,
    pub body: String,
    /// 앱 안에서 이동할 경로 (예: `/approval/documents/{id}`).
    pub link: Option<String>,
    pub dedupe_key: Option<String>,
    pub read_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

#[derive(sqlx::FromRow)]
struct NotificationRow {
    id: String,
    member_id: String,
    kind: String,
    title: String,
    body: String,
    link: Option<String>,
    dedupe_key: Option<String>,
    read_at: Option<i64>,
    created_at: i64,
}

impl TryFrom<NotificationRow> for Notification {
    type Error = crate::DbError;

    fn try_from(row: NotificationRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            member_id: codec::parse("member_id", &row.member_id)?,
            kind: row.kind,
            title: row.title,
            body: row.body,
            link: row.link,
            dedupe_key: row.dedupe_key,
            read_at: codec::timestamp_opt("read_at", row.read_at)?,
            created_at: codec::timestamp("created_at", row.created_at)?,
        })
    }
}

/// 알림을 만든다. 같은 구성원에게 같은 `dedupe_key`의 알림이 이미 있으면 아무 일도 하지 않는다.
/// 새로 만들었으면 `true`.
pub async fn insert(conn: &mut Conn, notification: &Notification) -> DbResult<bool> {
    let result = sqlx::query(
        "INSERT INTO notifications
            (id, member_id, kind, title, body, link, dedupe_key, read_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(notification.id.to_string())
    .bind(notification.member_id.to_string())
    .bind(&notification.kind)
    .bind(&notification.title)
    .bind(&notification.body)
    .bind(&notification.link)
    .bind(&notification.dedupe_key)
    .bind(notification.read_at.map(Timestamp::as_millis))
    .bind(notification.created_at.as_millis())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 한 번에 묶어 처리하는 구성원 수. SQLite의 바인드 변수 한도보다 충분히 작게 둔다.
const MEMBER_CHUNK: usize = 500;

/// 한 사건(`kind` + `dedupe_key`)의 알림 중 **아직 읽지 않은** 것을 받은 구성원.
pub async fn unread_recipients(
    conn: &mut Conn,
    kind: &str,
    dedupe_key: &str,
) -> DbResult<Vec<MemberId>> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT member_id FROM notifications
         WHERE kind = ? AND dedupe_key = ? AND read_at IS NULL ORDER BY member_id",
    )
    .bind(kind)
    .bind(dedupe_key)
    .fetch_all(conn)
    .await?;
    rows.iter()
        .map(|(member_id,)| codec::parse("member_id", member_id))
        .collect()
}

/// 한 사건의 **읽지 않은** 알림을 지우고 그 수를 돌려준다. `member_ids`가 있으면 그 구성원들의
/// 것만 지운다. 읽은 알림은 받은 기록이므로 지우지 않는다.
///
/// 알림이 가리키던 대상을 받는 사람이 더는 볼 수 없게 됐을 때 쓴다(보관된 공지 등). 알림에는
/// 대상의 제목이 들어 있기 때문이다.
pub async fn delete_unread(
    conn: &mut Conn,
    kind: &str,
    dedupe_key: &str,
    member_ids: Option<&[MemberId]>,
) -> DbResult<u64> {
    const BASE: &str =
        "DELETE FROM notifications WHERE kind = ? AND dedupe_key = ? AND read_at IS NULL";
    let Some(member_ids) = member_ids else {
        let result = sqlx::query(BASE)
            .bind(kind)
            .bind(dedupe_key)
            .execute(conn)
            .await?;
        return Ok(result.rows_affected());
    };
    let mut deleted = 0;
    for chunk in member_ids.chunks(MEMBER_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!("{BASE} AND member_id IN ({placeholders})");
        let mut query = sqlx::query(&sql).bind(kind).bind(dedupe_key);
        for id in chunk {
            query = query.bind(id.to_string());
        }
        deleted += query.execute(&mut *conn).await?.rows_affected();
    }
    Ok(deleted)
}

/// 최신순 목록. `(항목, 다음 페이지 존재 여부)`.
pub async fn list(
    conn: &mut Conn,
    member_id: MemberId,
    unread_only: bool,
    before: Option<NotificationId>,
    limit: u32,
) -> DbResult<(Vec<Notification>, bool)> {
    let rows = sqlx::query_as::<_, NotificationRow>(
        "SELECT id, member_id, kind, title, body, link, dedupe_key, read_at, created_at
         FROM notifications
         WHERE member_id = ?1 AND (?2 = 0 OR read_at IS NULL) AND (?3 IS NULL OR id < ?3)
         ORDER BY id DESC LIMIT ?4",
    )
    .bind(member_id.to_string())
    .bind(unread_only)
    .bind(before.map(|id| id.to_string()))
    .bind(codec::limit_plus_one(limit))
    .fetch_all(conn)
    .await?;
    let items = rows
        .into_iter()
        .map(Notification::try_from)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(codec::split_page(items, limit))
}

pub async fn count_unread(conn: &mut Conn, member_id: MemberId) -> DbResult<i64> {
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM notifications WHERE member_id = ? AND read_at IS NULL",
    )
    .bind(member_id.to_string())
    .fetch_one(conn)
    .await?;
    Ok(count)
}

/// 본인의 알림 하나를 읽음 처리한다. 대상이 본인 것이면(이미 읽었어도) `true`.
pub async fn mark_read(
    conn: &mut Conn,
    member_id: MemberId,
    id: NotificationId,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE notifications SET read_at = COALESCE(read_at, ?)
         WHERE id = ? AND member_id = ?",
    )
    .bind(now.as_millis())
    .bind(id.to_string())
    .bind(member_id.to_string())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 읽지 않은 알림을 모두 읽음 처리하고 그 수를 돌려준다.
pub async fn mark_all_read(conn: &mut Conn, member_id: MemberId, now: Timestamp) -> DbResult<u64> {
    let result =
        sqlx::query("UPDATE notifications SET read_at = ? WHERE member_id = ? AND read_at IS NULL")
            .bind(now.as_millis())
            .bind(member_id.to_string())
            .execute(conn)
            .await?;
    Ok(result.rows_affected())
}
