//! 공지사항과 읽음 확인.

use pango_core::authz::{Audience, AudienceKind};
use pango_core::id::{DepartmentId, MemberId, NoticeId};
use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

/// 저장되는 공지 한 건.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub id: NoticeId,
    pub title: String,
    pub body: String,
    pub author_id: MemberId,
    pub pinned: bool,
    pub must_read: bool,
    pub audience: Audience,
    pub published_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    pub version: i64,
}

/// 목록에 보여 줄 공지 정보(본문 제외)와 읽는 사람의 읽음 시각.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeListItem {
    pub id: NoticeId,
    pub title: String,
    pub author_id: MemberId,
    pub author_name: String,
    pub pinned: bool,
    pub must_read: bool,
    pub audience: Audience,
    pub department_name: Option<String>,
    pub published_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    pub version: i64,
    pub read_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeView {
    pub item: NoticeListItem,
    pub body: String,
}

/// 목록 정렬 키(고정 우선 → 게시 시각 최신 → ID). 다음 페이지 커서에 담는다.
///
/// 게시 시각이 같은 공지가 여러 개여도 ID까지 비교하므로 페이지 사이에 빠지거나
/// 겹치는 항목이 없다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoticeCursor {
    pub pinned: bool,
    pub published_at: Timestamp,
    pub id: NoticeId,
}

impl NoticeListItem {
    pub fn cursor(&self) -> NoticeCursor {
        NoticeCursor {
            pinned: self.pinned,
            published_at: self.published_at,
            id: self.id,
        }
    }
}

/// 목록·개수 조회 조건.
#[derive(Debug, Clone, Copy)]
pub struct NoticeFilter<'a> {
    /// 읽음 여부를 판단할 구성원.
    pub reader: MemberId,
    /// `None`이면 제한 없음. `Some`이면 전체 대상 공지와 이 부서들을 대상으로 한 공지만.
    /// 범위 계산은 `pango_core::authz::audience_scope`가 한다.
    pub visible_departments: Option<&'a [DepartmentId]>,
    pub include_archived: bool,
}

fn audience(kind: &str, department_id: Option<&str>) -> DbResult<Audience> {
    let kind = codec::parse_with("audience", kind, AudienceKind::parse)?;
    let department_id = codec::parse_opt("department_id", department_id)?;
    Audience::new(kind, department_id).map_err(codec::corrupt("audience"))
}

#[derive(sqlx::FromRow)]
struct NoticeRow {
    id: String,
    title: String,
    body: String,
    author_id: String,
    pinned: bool,
    must_read: bool,
    audience: String,
    department_id: Option<String>,
    published_at: i64,
    archived_at: Option<i64>,
    version: i64,
}

impl TryFrom<NoticeRow> for Notice {
    type Error = crate::DbError;

    fn try_from(row: NoticeRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            title: row.title,
            body: row.body,
            author_id: codec::parse("author_id", &row.author_id)?,
            pinned: row.pinned,
            must_read: row.must_read,
            audience: audience(&row.audience, row.department_id.as_deref())?,
            published_at: codec::timestamp("published_at", row.published_at)?,
            archived_at: codec::timestamp_opt("archived_at", row.archived_at)?,
            version: row.version,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ListRow {
    id: String,
    title: String,
    author_id: String,
    author_name: String,
    pinned: bool,
    must_read: bool,
    audience: String,
    department_id: Option<String>,
    department_name: Option<String>,
    published_at: i64,
    archived_at: Option<i64>,
    version: i64,
    read_at: Option<i64>,
}

impl TryFrom<ListRow> for NoticeListItem {
    type Error = crate::DbError;

    fn try_from(row: ListRow) -> DbResult<Self> {
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            title: row.title,
            author_id: codec::parse("author_id", &row.author_id)?,
            author_name: row.author_name,
            pinned: row.pinned,
            must_read: row.must_read,
            audience: audience(&row.audience, row.department_id.as_deref())?,
            department_name: row.department_name,
            published_at: codec::timestamp("published_at", row.published_at)?,
            archived_at: codec::timestamp_opt("archived_at", row.archived_at)?,
            version: row.version,
            read_at: codec::timestamp_opt("read_at", row.read_at)?,
        })
    }
}

#[derive(sqlx::FromRow)]
struct ViewRow {
    #[sqlx(flatten)]
    item: ListRow,
    body: String,
}

/// 목록·상세가 같이 쓰는 SELECT. `?1`은 읽는 사람. `extra`는 덧붙일 열(상세의 본문 등).
fn view_select(extra: &str) -> String {
    format!(
        "SELECT n.id, n.title, n.author_id, a.name AS author_name, n.pinned, n.must_read,
            n.audience, n.department_id, d.name AS department_name, n.published_at,
            n.archived_at, n.version, r.read_at{extra}
         FROM notices n
         JOIN members a ON a.id = n.author_id
         LEFT JOIN departments d ON d.id = n.department_id
         LEFT JOIN notice_reads r ON r.notice_id = n.id AND r.member_id = ?1"
    )
}

/// 볼 수 있는 범위 조건. `?2`: 보관 포함 여부, `?3`: 부서 ID의 JSON 배열(NULL이면 제한 없음).
const VISIBLE_WHERE: &str = "(?2 = 1 OR n.archived_at IS NULL)
       AND (?3 IS NULL OR n.audience = 'all'
            OR n.department_id IN (SELECT value FROM json_each(?3)))";

fn departments_json(filter: &NoticeFilter<'_>) -> DbResult<Option<String>> {
    filter
        .visible_departments
        .map(|ids| {
            let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
            codec::json_text("department_id", &ids)
        })
        .transpose()
}

pub async fn insert(conn: &mut Conn, notice: &Notice, now: Timestamp) -> DbResult<()> {
    sqlx::query(
        "INSERT INTO notices
            (id, title, body, author_id, pinned, must_read, audience, department_id,
             published_at, archived_at, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(notice.id.to_string())
    .bind(&notice.title)
    .bind(&notice.body)
    .bind(notice.author_id.to_string())
    .bind(notice.pinned)
    .bind(notice.must_read)
    .bind(notice.audience.kind().as_str())
    .bind(notice.audience.department_id().map(|id| id.to_string()))
    .bind(notice.published_at.as_millis())
    .bind(notice.archived_at.map(Timestamp::as_millis))
    .bind(now.as_millis())
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(())
}

pub async fn find(conn: &mut Conn, id: NoticeId) -> DbResult<Option<Notice>> {
    sqlx::query_as::<_, NoticeRow>(
        "SELECT id, title, body, author_id, pinned, must_read, audience, department_id,
                published_at, archived_at, version
         FROM notices WHERE id = ?",
    )
    .bind(id.to_string())
    .fetch_optional(conn)
    .await?
    .map(Notice::try_from)
    .transpose()
}

/// 내용·대상·보관 상태를 갱신한다. `notice.version`이 현재 값과 같을 때만. 갱신했으면 `true`.
pub async fn update(conn: &mut Conn, notice: &Notice, now: Timestamp) -> DbResult<bool> {
    let result = sqlx::query(
        "UPDATE notices SET
            title = ?, body = ?, pinned = ?, must_read = ?, audience = ?, department_id = ?,
            archived_at = ?, updated_at = ?, version = version + 1
         WHERE id = ? AND version = ?",
    )
    .bind(&notice.title)
    .bind(&notice.body)
    .bind(notice.pinned)
    .bind(notice.must_read)
    .bind(notice.audience.kind().as_str())
    .bind(notice.audience.department_id().map(|id| id.to_string()))
    .bind(notice.archived_at.map(Timestamp::as_millis))
    .bind(now.as_millis())
    .bind(notice.id.to_string())
    .bind(notice.version)
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// 상세 조회(작성자·부서 이름, 읽는 사람의 읽음 시각 포함). 보관 여부와 무관하게 찾는다.
pub async fn find_view(
    conn: &mut Conn,
    id: NoticeId,
    reader: MemberId,
) -> DbResult<Option<NoticeView>> {
    let row = sqlx::query_as::<_, ViewRow>(&format!("{} WHERE n.id = ?2", view_select(", n.body")))
        .bind(reader.to_string())
        .bind(id.to_string())
        .fetch_optional(conn)
        .await?;
    row.map(|row| {
        Ok(NoticeView {
            item: row.item.try_into()?,
            body: row.body,
        })
    })
    .transpose()
}

/// 고정 공지 먼저, 그다음 최신순. `(항목, 다음 페이지 존재 여부)`.
pub async fn list(
    conn: &mut Conn,
    filter: &NoticeFilter<'_>,
    after: Option<NoticeCursor>,
    limit: u32,
) -> DbResult<(Vec<NoticeListItem>, bool)> {
    // 정렬이 모두 내림차순이므로 행 값 비교 한 번으로 "커서 다음"을 표현한다.
    let rows = sqlx::query_as::<_, ListRow>(&format!(
        "{select}
         WHERE {VISIBLE_WHERE}
           AND (?4 IS NULL OR (n.pinned, n.published_at, n.id) < (?4, ?5, ?6))
         ORDER BY n.pinned DESC, n.published_at DESC, n.id DESC
         LIMIT ?7",
        select = view_select("")
    ))
    .bind(filter.reader.to_string())
    .bind(filter.include_archived)
    .bind(departments_json(filter)?)
    .bind(after.map(|c| c.pinned))
    .bind(after.map(|c| c.published_at.as_millis()))
    .bind(after.map(|c| c.id.to_string()))
    .bind(codec::limit_plus_one(limit))
    .fetch_all(conn)
    .await?;
    let items = rows
        .into_iter()
        .map(NoticeListItem::try_from)
        .collect::<DbResult<Vec<_>>>()?;
    Ok(codec::split_page(items, limit))
}

/// 읽지 않은 공지 수와 그중 필독 공지 수.
pub async fn count_unread(conn: &mut Conn, filter: &NoticeFilter<'_>) -> DbResult<(i64, i64)> {
    let counts: (i64, i64) = sqlx::query_as(&format!(
        "SELECT COUNT(*), COALESCE(SUM(n.must_read), 0) FROM notices n
         WHERE {VISIBLE_WHERE}
           AND NOT EXISTS (SELECT 1 FROM notice_reads r
                           WHERE r.notice_id = n.id AND r.member_id = ?1)"
    ))
    .bind(filter.reader.to_string())
    .bind(filter.include_archived)
    .bind(departments_json(filter)?)
    .fetch_one(conn)
    .await?;
    Ok(counts)
}

/// 읽음 확인을 남긴다. 이미 있으면 첫 시각을 그대로 둔다. 새로 남겼으면 `true`.
pub async fn mark_read(
    conn: &mut Conn,
    notice_id: NoticeId,
    member_id: MemberId,
    now: Timestamp,
) -> DbResult<bool> {
    let result = sqlx::query(
        "INSERT INTO notice_reads (notice_id, member_id, read_at) VALUES (?, ?, ?)
         ON CONFLICT DO NOTHING",
    )
    .bind(notice_id.to_string())
    .bind(member_id.to_string())
    .bind(now.as_millis())
    .execute(conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeRead {
    pub member_id: MemberId,
    pub name: String,
    pub read_at: Timestamp,
}

/// 읽음 확인 목록(먼저 읽은 순).
pub async fn list_reads(conn: &mut Conn, notice_id: NoticeId) -> DbResult<Vec<NoticeRead>> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT r.member_id, m.name, r.read_at FROM notice_reads r
         JOIN members m ON m.id = r.member_id
         WHERE r.notice_id = ? ORDER BY r.read_at, m.name, r.member_id",
    )
    .bind(notice_id.to_string())
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|(member_id, name, read_at)| {
            Ok(NoticeRead {
                member_id: codec::parse("member_id", &member_id)?,
                name,
                read_at: codec::timestamp("read_at", read_at)?,
            })
        })
        .collect()
}
