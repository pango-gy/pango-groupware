//! 전자결재: 양식, 문서, 결재함.

use pango_core::approval::{
    DocumentKind, DocumentStatus, EventKind, Mailbox, StepKind, StepStatus,
};
use pango_core::id::{DocumentId, FormId, MemberId};
use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::common::PageQuery;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormDto {
    pub id: FormId,
    pub code: String,
    pub name: String,
    pub description: String,
    pub kind: DocumentKind,
    pub body_template: String,
    pub active: bool,
    pub sort_order: i64,
    pub version: i64,
}

/// 관리자가 만드는 양식. 종류는 항상 일반(`general`)이다. 시스템 양식은 초기화 때만 만든다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateFormRequest {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub body_template: String,
    #[serde(default)]
    pub sort_order: i64,
}

/// `code`와 종류는 바꿀 수 없다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateFormRequest {
    pub version: i64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub body_template: String,
    pub active: bool,
    #[serde(default)]
    pub sort_order: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepInput {
    pub approver_id: MemberId,
    pub kind: StepKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateDocumentRequest {
    pub form_id: FormId,
    pub title: String,
    pub body: String,
    pub steps: Vec<StepInput>,
    #[serde(default)]
    pub references: Vec<MemberId>,
    /// `true`면 저장과 동시에 상신한다. 아니면 임시저장.
    #[serde(default)]
    pub submit: bool,
}

/// 임시저장 문서 수정. 제목·본문·결재선을 통째로 바꾼다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateDocumentRequest {
    pub version: i64,
    pub title: String,
    pub body: String,
    pub steps: Vec<StepInput>,
    #[serde(default)]
    pub references: Vec<MemberId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApproveRequest {
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectRequest {
    pub reason: String,
}

/// 결재함 목록의 한 줄.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSummary {
    pub id: DocumentId,
    /// 상신 전(임시저장)에는 없다.
    pub doc_no: Option<String>,
    pub title: String,
    pub form_name: String,
    pub kind: DocumentKind,
    pub status: DocumentStatus,
    pub drafter_id: MemberId,
    pub drafter_name: String,
    pub current_approver_id: Option<MemberId>,
    pub current_approver_name: Option<String>,
    pub submitted_at: Option<Timestamp>,
    /// 지금 내가 처리할 차례인지.
    pub my_turn: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepDto {
    pub order: u8,
    pub approver_id: MemberId,
    pub approver_name: String,
    pub kind: StepKind,
    pub status: StepStatus,
    pub acted_at: Option<Timestamp>,
    /// 승인 의견 또는 반려 사유.
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceDto {
    pub member_id: MemberId,
    pub name: String,
    pub read_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventDto {
    pub kind: EventKind,
    pub actor_id: MemberId,
    pub actor_name: String,
    pub step_order: Option<u8>,
    pub comment: Option<String>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentDetail {
    pub id: DocumentId,
    pub doc_no: Option<String>,
    pub form_id: FormId,
    pub form_name: String,
    pub kind: DocumentKind,
    pub status: DocumentStatus,
    pub drafter_id: MemberId,
    pub drafter_name: String,
    pub drafter_department_name: Option<String>,
    pub title: String,
    pub body: String,
    /// 시스템 양식(휴가 신청 등)의 구조화된 내용.
    pub payload: Option<serde_json::Value>,
    pub steps: Vec<StepDto>,
    pub references: Vec<ReferenceDto>,
    pub events: Vec<EventDto>,
    pub submitted_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub version: i64,
    /// 지금 내가 처리할 차례인지.
    pub my_turn: bool,
}

/// `GET /approval/documents` 쿼리. `box`는 필수다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentQuery {
    #[serde(rename = "box")]
    pub mailbox: Mailbox,
    #[serde(default)]
    pub status: Option<DocumentStatus>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl DocumentQuery {
    pub fn page(&self) -> PageQuery {
        PageQuery {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }
}

/// `GET /admin/approval/documents` 쿼리. 임시저장 문서는 관리자에게도 보이지 않는다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminDocumentQuery {
    #[serde(default)]
    pub status: Option<DocumentStatus>,
    #[serde(default)]
    pub kind: Option<DocumentKind>,
    #[serde(default)]
    pub drafter_id: Option<MemberId>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl AdminDocumentQuery {
    pub fn page(&self) -> PageQuery {
        PageQuery {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }
}

/// 메뉴 배지용 건수.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalCounts {
    /// 내가 결재할 차례인 문서.
    pub todo: u32,
    /// 내가 기안했고 아직 진행 중인 문서.
    pub in_progress_drafted: u32,
    /// 참조자로 받았지만 아직 열어 보지 않은 문서.
    pub unread_reference: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedStep {
    pub approver_id: MemberId,
    pub approver_name: String,
    pub kind: StepKind,
}

/// 기본 결재선 추천. 조직장이 없으면 비어 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineSuggestion {
    pub steps: Vec<SuggestedStep>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_query_uses_the_box_key_and_rejects_unknown_values() {
        let parsed: DocumentQuery = serde_json::from_str(r#"{"box":"todo"}"#).expect("parse");
        assert_eq!(parsed.mailbox, Mailbox::Todo);
        assert!(serde_json::from_str::<DocumentQuery>(r#"{"box":"everything"}"#).is_err());
        assert!(serde_json::from_str::<DocumentQuery>("{}").is_err());
    }

    #[test]
    fn create_request_defaults_to_draft_without_references() {
        let parsed: CreateDocumentRequest = serde_json::from_value(serde_json::json!({
            "form_id": FormId::new(), "title": "t", "body": "b", "steps": []
        }))
        .expect("parse");
        assert!(!parsed.submit);
        assert!(parsed.references.is_empty());
    }
}
