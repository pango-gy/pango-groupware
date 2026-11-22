//! 전자결재: 양식, 문서, 결재함.

use pango_core::approval::{
    DocumentKind, DocumentStatus, EventKind, FormCategory, FormOrigin, Mailbox, StepKind,
    StepStatus,
};
use pango_core::form_schema::FormSchema;
use pango_core::id::{DocumentId, FormId, MemberId};
use pango_core::string_enum;
use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

use crate::common::PageQuery;

string_enum! {
    /// 이 양식의 문서를 어느 화면에서 만드는지. 주소가 아니라 화면의 이름이다.
    pub enum FormRoute("route") {
        /// 일반 기안 화면(`POST /approval/documents`).
        ApprovalNew => "approval_new",
        /// 휴가 신청 화면. 휴가 신청서는 휴가 기능이 만든다.
        LeaveNew => "leave_new",
        /// 근태 화면의 정정 요청. 근태 정정 신청서는 근태 기능이 만든다.
        Attendance => "attendance",
    }
}

impl FormRoute {
    pub const fn of(kind: DocumentKind) -> Self {
        match kind {
            DocumentKind::General => Self::ApprovalNew,
            DocumentKind::Leave => Self::LeaveNew,
            DocumentKind::AttendanceCorrection => Self::Attendance,
        }
    }
}

/// 양식 전체(관리 화면).
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
    pub category: FormCategory,
    pub category_label: String,
    pub schema: FormSchema,
    /// 필드 정의를 고칠 때마다 1씩 오른다.
    pub schema_version: i64,
    /// 자유 입력 본문을 쓰는지(쓰면 필수).
    pub use_body: bool,
    /// 상신할 때 자동으로 붙는 참조자.
    pub default_reference_ids: Vec<MemberId>,
    pub origin: FormOrigin,
    /// 시스템 양식(휴가·근태 정정)인지. 일반 기안으로는 만들 수 없다.
    pub system: bool,
    pub route: FormRoute,
}

/// 기안 화면의 양식 하나. 사용 중인 양식만 내려간다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormListItem {
    pub id: FormId,
    pub code: String,
    pub name: String,
    pub description: String,
    pub kind: DocumentKind,
    pub body_template: String,
    pub use_body: bool,
    pub schema: FormSchema,
    pub schema_version: i64,
    /// `true`면 `route`의 화면에서만 만들 수 있다.
    pub system: bool,
    pub route: FormRoute,
}

/// `GET /approval/forms`의 한 묶음. 분류 순서대로, 양식이 없는 분류는 빠진다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormGroup {
    pub category: FormCategory,
    pub category_label: String,
    pub forms: Vec<FormListItem>,
}

/// 관리자가 만드는 양식(`POST /admin/approval/forms`). 종류는 항상 일반(`general`)이다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewFormRequest {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub body_template: String,
    #[serde(default)]
    pub sort_order: i64,
    /// 생략하면 `general`.
    #[serde(default)]
    pub category: Option<FormCategory>,
    /// 생략하면 필드 없음.
    #[serde(default)]
    pub schema: Option<FormSchema>,
    /// 생략하면 `true`.
    #[serde(default)]
    pub use_body: Option<bool>,
    #[serde(default)]
    pub default_reference_ids: Vec<MemberId>,
}

/// 양식 수정(`PATCH /admin/approval/forms/{id}`). `code`와 종류는 바꿀 수 없다.
/// `category`·`schema`·`use_body`·`default_reference_ids`는 생략하면 그대로 둔다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditFormRequest {
    pub version: i64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub body_template: String,
    pub active: bool,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub category: Option<FormCategory>,
    #[serde(default)]
    pub schema: Option<FormSchema>,
    #[serde(default)]
    pub use_body: Option<bool>,
    #[serde(default)]
    pub default_reference_ids: Option<Vec<MemberId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepInput {
    pub approver_id: MemberId,
    pub kind: StepKind,
}

/// 기안(`POST /approval/documents`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewDocumentRequest {
    pub form_id: FormId,
    pub title: String,
    /// 양식이 본문을 쓰면(`use_body`) 필수, 아니면 비워도 된다.
    #[serde(default)]
    pub body: String,
    /// 양식 필드의 값(`{필드 key: 값}`). 필드가 없는 양식이면 생략한다.
    #[serde(default)]
    pub values: Option<serde_json::Value>,
    pub steps: Vec<StepInput>,
    #[serde(default)]
    pub references: Vec<MemberId>,
    /// `true`면 저장과 동시에 상신한다. 아니면 임시저장.
    #[serde(default)]
    pub submit: bool,
}

/// 임시저장 문서 수정(`PATCH /approval/documents/{id}`). 제목·본문·결재선을 통째로 바꾼다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditDocumentRequest {
    pub version: i64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// 보내면 값을 통째로 바꾸고, 생략하면 저장된 값을 그대로 둔다.
    #[serde(default)]
    pub values: Option<serde_json::Value>,
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

/// 일반 양식 문서의 구조화된 내용. 문서를 낸 시점의 양식 정의가 함께 들어 있어, 그 뒤에
/// 양식이 바뀌어도 이 값만으로 읽기 전용 화면을 그릴 수 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormSnapshot {
    pub category: FormCategory,
    pub category_label: String,
    pub use_body: bool,
    /// 값을 검증한 양식 정의의 버전. 필드 정의가 없던 때의 문서는 0이다.
    pub schema_version: i64,
    pub schema: FormSchema,
    /// `{필드 key: 값}`. 입력하지 않은 필드는 없다.
    pub values: serde_json::Map<String, serde_json::Value>,
    /// 값에서 계산한 것: 금액 열이 있는 표는 `{"totals": {열 key: 합계}}`, 기간은 `{"days": 일수}`.
    pub computed: serde_json::Map<String, serde_json::Value>,
    /// `member` 필드에 지정된 구성원의 이름(`{구성원 ID: 이름}`).
    pub member_names: std::collections::BTreeMap<MemberId, String>,
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
    /// 시스템 양식(휴가 신청 등)의 구조화된 내용. 일반 양식 문서에서는 `null`이다.
    pub payload: Option<serde_json::Value>,
    /// 일반 양식 문서의 양식 정의와 값. 시스템 양식 문서에서는 `null`이다.
    pub form_snapshot: Option<FormSnapshot>,
    pub steps: Vec<StepDto>,
    pub references: Vec<ReferenceDto>,
    pub events: Vec<EventDto>,
    /// 첨부파일(올린 순). 기안자가 임시저장 상태에서만 붙이고 뺄 수 있고, 상신하면 고정된다.
    /// 내용은 `GET /approval/documents/{id}/attachments/{attachment_id}`로 받는다.
    #[serde(default)]
    pub attachments: Vec<crate::attachment::Attachment>,
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
    pub form_id: Option<FormId>,
    /// 양식의 지금 분류.
    #[serde(default)]
    pub category: Option<FormCategory>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl DocumentQuery {
    pub fn page(&self) -> PageQuery {
        PageQuery::of(self.limit, &self.cursor)
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
    pub form_id: Option<FormId>,
    #[serde(default)]
    pub category: Option<FormCategory>,
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl AdminDocumentQuery {
    pub fn page(&self) -> PageQuery {
        PageQuery::of(self.limit, &self.cursor)
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
        let parsed: NewDocumentRequest = serde_json::from_value(serde_json::json!({
            "form_id": FormId::new(), "title": "t", "body": "b", "steps": []
        }))
        .expect("parse");
        assert!(!parsed.submit);
        assert!(parsed.references.is_empty());
        assert_eq!(parsed.values, None);
    }

    /// 분류·필드 정의·값을 생략한 요청도 그대로 읽힌다(생략한 것은 서버가 그대로 둔다).
    #[test]
    fn optional_parts_may_be_omitted() {
        let form_id = FormId::new();
        let parsed: NewDocumentRequest = serde_json::from_value(serde_json::json!({
            "form_id": form_id, "title": "t", "body": "b", "steps": [], "submit": true
        }))
        .expect("parse");
        assert_eq!(
            (parsed.form_id, parsed.body.as_str(), parsed.submit),
            (form_id, "b", true)
        );

        let parsed: EditFormRequest = serde_json::from_value(serde_json::json!({
            "version": 3, "name": "n", "active": true
        }))
        .expect("parse");
        assert_eq!(parsed.version, 3);
        assert_eq!(
            (
                parsed.category,
                parsed.schema,
                parsed.use_body,
                parsed.default_reference_ids
            ),
            (None, None, None, None)
        );
    }

    #[test]
    fn form_route_follows_the_document_kind() {
        assert_eq!(
            FormRoute::of(DocumentKind::General).as_str(),
            "approval_new"
        );
        assert_eq!(FormRoute::of(DocumentKind::Leave).as_str(), "leave_new");
        assert_eq!(
            FormRoute::of(DocumentKind::AttendanceCorrection).as_str(),
            "attendance"
        );
    }

    #[test]
    fn document_query_takes_form_and_category_filters() {
        let parsed: DocumentQuery =
            serde_json::from_str(r#"{"box":"drafted","category":"accounting"}"#).expect("parse");
        assert_eq!(parsed.category, Some(FormCategory::Accounting));
        assert_eq!(parsed.form_id, None);
        assert!(serde_json::from_str::<DocumentQuery>(r#"{"box":"todo","category":"x"}"#).is_err());
    }
}
