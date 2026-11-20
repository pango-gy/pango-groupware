//! 앱 내 알림.

use pango_core::id::NotificationId;
use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::PageQuery;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationDto {
    pub id: NotificationId,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub link: Option<String>,
    pub read_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

/// 알림 목록 쿼리.
///
/// [`PageQuery`]를 `#[serde(flatten)]`으로 넣지 않는 이유: 쿼리 문자열 역직렬화에서
/// flatten을 쓰면 숫자 필드(`limit`)를 읽지 못한다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationQuery {
    #[serde(default)]
    pub unread_only: bool,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl NotificationQuery {
    pub fn page(&self) -> PageQuery {
        PageQuery {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadCount {
    pub unread: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAllResult {
    /// 이번 요청으로 읽음 처리된 알림 수.
    pub updated: u64,
}
