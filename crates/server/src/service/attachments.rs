//! 첨부파일 공통 처리(공지·결재 문서). 권한·상태 확인과 감사 로그 이름은 각 기능 서비스가 정한다.

use pango_api::attachment::Attachment as AttachmentDto;
use pango_core::attachment::{FileName, MAX_PER_OWNER};
use pango_core::error::{ConflictReason, ValidationReason};
use pango_core::id::{AttachmentId, MemberId};
use pango_core::time::Timestamp;
use pango_core::DomainError;
use pango_db::attachments::{self, Attachment, Owner};
use pango_db::Conn;
use sha2::Digest;

use super::support::found;
use crate::error::{ServiceError, ServiceResult};
use crate::security::hex;

/// 검증을 마친 올릴 파일. 트랜잭션 밖에서 만든다(해시 계산).
pub struct NewFile<'a> {
    filename: String,
    size: i64,
    sha256: String,
    data: &'a [u8],
}

/// 이름을 검증하고 빈 파일을 거부하고 해시를 계산한다. 크기 상한은 HTTP 계층이 본문을 읽을 때 지킨다.
pub fn prepare<'a>(filename: &str, data: &'a [u8]) -> ServiceResult<NewFile<'a>> {
    let filename = FileName::parse(filename)?.into_string();
    if data.is_empty() {
        return Err(DomainError::validation("file", ValidationReason::Empty).into());
    }
    Ok(NewFile {
        filename,
        size: i64::try_from(data.len()).map_err(ServiceError::internal)?,
        sha256: hex(&sha2::Sha256::digest(data)),
        data,
    })
}

pub fn dto(item: Attachment) -> ServiceResult<AttachmentDto> {
    Ok(AttachmentDto {
        id: item.id,
        filename: item.filename,
        size: u64::try_from(item.size).map_err(ServiceError::internal)?,
        created_at: item.created_at,
    })
}

/// 감사 로그의 `detail`(이름·크기·해시).
pub fn audit_detail(item: &Attachment) -> serde_json::Value {
    serde_json::json!({
        "attachment_id": item.id,
        "filename": item.filename,
        "size": item.size,
        "sha256": item.sha256,
    })
}

/// 소유자에 붙인다. 소유자마다 [`MAX_PER_OWNER`]개까지(넘으면 409 `attachment_limit`).
pub async fn insert_tx(
    conn: &mut Conn,
    owner: Owner,
    file: &NewFile<'_>,
    uploader: MemberId,
    now: Timestamp,
) -> ServiceResult<Attachment> {
    let count = attachments::count(conn, owner).await?;
    if usize::try_from(count).map_err(ServiceError::internal)? >= MAX_PER_OWNER {
        return Err(DomainError::Conflict(ConflictReason::AttachmentLimit).into());
    }
    let item = Attachment {
        id: AttachmentId::new(),
        owner,
        filename: file.filename.clone(),
        size: file.size,
        sha256: file.sha256.clone(),
        uploaded_by: uploader,
        created_at: now,
    };
    attachments::insert(conn, &item, file.data).await?;
    Ok(item)
}

/// 소유자의 첨부 하나를 내용과 함께 지운다. 다른 소유자의 첨부 ID면 404.
pub async fn remove_tx(
    conn: &mut Conn,
    owner: Owner,
    id: AttachmentId,
) -> ServiceResult<Attachment> {
    let item = found(attachments::find(conn, owner, id).await?)?;
    attachments::delete(conn, id).await?;
    Ok(item)
}

/// 상세 응답에 싣는 첨부 목록.
pub async fn list_tx(conn: &mut Conn, owner: Owner) -> ServiceResult<Vec<AttachmentDto>> {
    attachments::list(conn, owner)
        .await?
        .into_iter()
        .map(dto)
        .collect()
}

/// 내려받을 첨부파일.
pub struct AttachmentFile {
    pub filename: String,
    pub data: Vec<u8>,
}

/// 소유자의 첨부 하나의 내용. 다른 소유자의 첨부 ID면 404. 볼 권한은 호출하는 쪽이 먼저 확인한다.
pub async fn file_tx(
    conn: &mut Conn,
    owner: Owner,
    id: AttachmentId,
) -> ServiceResult<AttachmentFile> {
    let item = found(attachments::find(conn, owner, id).await?)?;
    let data = found(attachments::content(conn, id).await?)?;
    Ok(AttachmentFile {
        filename: item.filename,
        data,
    })
}
