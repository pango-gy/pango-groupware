//! 첨부파일(공지·결재 문서)과 그 내용.

use pango_core::id::{AttachmentId, DocumentId, MemberId, NoticeId};
use pango_core::time::Timestamp;

use crate::codec;
use crate::error::DbResult;
use crate::pool::Conn;

/// 첨부파일이 붙은 곳.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Notice(NoticeId),
    Document(DocumentId),
}

impl Owner {
    /// 소유자 열 이름. 정해진 값만 돌려주므로 SQL 문에 그대로 넣어도 안전하다.
    fn column(self) -> &'static str {
        match self {
            Self::Notice(_) => "notice_id",
            Self::Document(_) => "document_id",
        }
    }

    fn id(self) -> String {
        match self {
            Self::Notice(id) => id.to_string(),
            Self::Document(id) => id.to_string(),
        }
    }
}

/// 첨부파일 정보(내용 제외).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub id: AttachmentId,
    pub owner: Owner,
    pub filename: String,
    pub size: i64,
    pub sha256: String,
    pub uploaded_by: MemberId,
    pub created_at: Timestamp,
}

#[derive(sqlx::FromRow)]
struct AttachmentRow {
    id: String,
    notice_id: Option<String>,
    document_id: Option<String>,
    filename: String,
    size: i64,
    sha256: String,
    uploaded_by: String,
    created_at: i64,
}

impl TryFrom<AttachmentRow> for Attachment {
    type Error = crate::DbError;

    fn try_from(row: AttachmentRow) -> DbResult<Self> {
        let owner = match (row.notice_id, row.document_id) {
            (Some(notice), None) => Owner::Notice(codec::parse("notice_id", &notice)?),
            (None, Some(document)) => Owner::Document(codec::parse("document_id", &document)?),
            _ => {
                return Err(codec::corrupt("notice_id")(
                    pango_core::DomainError::validation(
                        "notice_id",
                        pango_core::ValidationReason::Inconsistent,
                    ),
                ))
            }
        };
        Ok(Self {
            id: codec::parse("id", &row.id)?,
            owner,
            filename: row.filename,
            size: row.size,
            sha256: row.sha256,
            uploaded_by: codec::parse("uploaded_by", &row.uploaded_by)?,
            created_at: codec::timestamp("created_at", row.created_at)?,
        })
    }
}

const COLUMNS: &str =
    "id, notice_id, document_id, filename, size, sha256, uploaded_by, created_at FROM attachments";

/// 첨부파일과 내용을 함께 넣는다.
pub async fn insert(conn: &mut Conn, item: &Attachment, data: &[u8]) -> DbResult<()> {
    let (notice_id, document_id) = match item.owner {
        Owner::Notice(id) => (Some(id.to_string()), None),
        Owner::Document(id) => (None, Some(id.to_string())),
    };
    sqlx::query(
        "INSERT INTO attachments
            (id, notice_id, document_id, filename, size, sha256, uploaded_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(item.id.to_string())
    .bind(notice_id)
    .bind(document_id)
    .bind(&item.filename)
    .bind(item.size)
    .bind(&item.sha256)
    .bind(item.uploaded_by.to_string())
    .bind(item.created_at.as_millis())
    .execute(&mut *conn)
    .await?;
    sqlx::query("INSERT INTO attachment_contents (attachment_id, data) VALUES (?, ?)")
        .bind(item.id.to_string())
        .bind(data)
        .execute(conn)
        .await?;
    Ok(())
}

/// 소유자의 첨부파일(올린 순).
pub async fn list(conn: &mut Conn, owner: Owner) -> DbResult<Vec<Attachment>> {
    sqlx::query_as::<_, AttachmentRow>(&format!(
        "SELECT {COLUMNS} WHERE {} = ? ORDER BY created_at, id",
        owner.column()
    ))
    .bind(owner.id())
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(Attachment::try_from)
    .collect()
}

pub async fn count(conn: &mut Conn, owner: Owner) -> DbResult<i64> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM attachments WHERE {} = ?",
        owner.column()
    ))
    .bind(owner.id())
    .fetch_one(conn)
    .await?)
}

/// 소유자에 붙은 첨부파일 하나. 다른 소유자의 첨부 ID면 `None`.
pub async fn find(conn: &mut Conn, owner: Owner, id: AttachmentId) -> DbResult<Option<Attachment>> {
    sqlx::query_as::<_, AttachmentRow>(&format!(
        "SELECT {COLUMNS} WHERE id = ? AND {} = ?",
        owner.column()
    ))
    .bind(id.to_string())
    .bind(owner.id())
    .fetch_optional(conn)
    .await?
    .map(Attachment::try_from)
    .transpose()
}

/// 첨부파일의 내용.
pub async fn content(conn: &mut Conn, id: AttachmentId) -> DbResult<Option<Vec<u8>>> {
    Ok(
        sqlx::query_scalar("SELECT data FROM attachment_contents WHERE attachment_id = ?")
            .bind(id.to_string())
            .fetch_optional(conn)
            .await?,
    )
}

/// 첨부파일을 내용과 함께 지운다. 지웠으면 `true`.
pub async fn delete(conn: &mut Conn, id: AttachmentId) -> DbResult<bool> {
    let result = sqlx::query("DELETE FROM attachments WHERE id = ?")
        .bind(id.to_string())
        .execute(conn)
        .await?;
    Ok(result.rows_affected() == 1)
}
