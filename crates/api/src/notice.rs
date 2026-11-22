//! 공지사항.

use pango_core::authz::AudienceKind;
use pango_core::id::{DepartmentId, MemberId, NoticeId};
use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

/// 공지 작성·수정 본문. 수정은 [`crate::Versioned`]로 감싸 전체를 다시 보낸다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeInput {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub must_read: bool,
    pub audience: AudienceKind,
    /// `audience`가 `department`일 때만 보낸다.
    #[serde(default)]
    pub department_id: Option<DepartmentId>,
}

/// 공지 목록의 한 줄. 본문은 상세 조회에서만 준다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeSummary {
    pub id: NoticeId,
    pub title: String,
    pub author_id: MemberId,
    pub author_name: String,
    pub pinned: bool,
    pub must_read: bool,
    pub audience: AudienceKind,
    pub department_id: Option<DepartmentId>,
    pub department_name: Option<String>,
    pub published_at: Timestamp,
    /// 요청한 구성원이 읽음 확인을 했는지.
    pub read: bool,
    /// 보관된 공지(관리자 목록에서만 보인다).
    pub archived_at: Option<Timestamp>,
    pub version: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeDetail {
    #[serde(flatten)]
    pub summary: NoticeSummary,
    pub body: String,
    /// 요청한 구성원의 첫 읽음 확인 시각.
    pub read_at: Option<Timestamp>,
    /// 첨부파일(올린 순). 내용은 `GET /notices/{id}/attachments/{attachment_id}`로 받는다.
    pub attachments: Vec<crate::attachment::Attachment>,
}

/// 배지 표시용 읽지 않은 공지 수.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeUnreadCount {
    pub unread: u32,
    /// 그중 필독 공지 수.
    pub must_read_unread: u32,
}

/// 읽음 확인 결과. 여러 번 보내도 첫 확인 시각이 유지된다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeReadReceipt {
    pub notice_id: NoticeId,
    pub read_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeReader {
    pub member_id: MemberId,
    pub name: String,
    /// 읽지 않은 사람이면 `None`.
    pub read_at: Option<Timestamp>,
}

/// 관리자용 읽음 현황.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeReadReport {
    /// 읽음 확인을 한 사람 전부(확인 순).
    pub read: Vec<NoticeReader>,
    /// 지금 대상 범위의 재직 구성원 중 아직 읽지 않은 사람(이름순).
    pub unread: Vec<NoticeReader>,
}
