//! 첨부파일(공지·결재 문서).
//!
//! 올리기는 소유자의 `…/attachments` 경로에 파일 내용을 본문 그대로
//! (`Content-Type: application/octet-stream`) 보내고, 파일 이름은 `X-File-Name` 헤더에 퍼센트
//! 인코딩한 UTF-8로 보낸다. 내려받기는 `…/attachments/{attachment_id}`.

use pango_core::id::AttachmentId;
use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: AttachmentId,
    pub filename: String,
    /// 바이트 수.
    pub size: u64,
    pub created_at: Timestamp,
}
