//! 원격 MCP 도구. 도구는 기존 유스케이스(`service::*`)를 요청자 권한 그대로 부른다 — 권한 확인,
//! 감사 로그, 알림이 웹 화면과 같다. 입력은 기존 요청 타입으로 바로 읽는다(같은 원본).
//!
//! scope: 조회(`groupware:read`) / 변경(`groupware:write`) / 관리(`groupware:admin`). 만드는 도구는
//! `request_id`를 주면 같은 값으로 다시 불러도 한 번만 만든다.

use pango_api::approval::{ApproveRequest, DocumentQuery, NewDocumentRequest, RejectRequest};
use pango_api::attendance::{ClockInRequest, MonthQuery};
use pango_api::leave::{CreateLeaveRequest, LeaveRequestQuery};
use pango_api::notice::NoticeInput;
use pango_api::org::MemberQuery;
use pango_api::PageQuery;
use pango_core::id::{DocumentId, LeaveRequestId, NoticeId};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::ServiceError;
use crate::security::fingerprint;
use crate::service::oauth::{SCOPE_ADMIN, SCOPE_READ, SCOPE_WRITE};
use crate::service::support::Idempotency;
use crate::service::{self, AuthContext};
use crate::state::AppState;

/// 도구 하나의 정의.
pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub scope: &'static str,
    pub schema: Value,
    /// 되돌리기 어려운 동작(승인·반려·회수·취소).
    pub destructive: bool,
    /// 사람에게 알림이 나가는 동작.
    pub notifies: bool,
}

impl Tool {
    pub fn describe(&self) -> Value {
        let read_only = self.scope == SCOPE_READ;
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "inputSchema": self.schema,
            "annotations": {
                "title": self.title,
                "readOnlyHint": read_only,
                "destructiveHint": self.destructive,
                "idempotentHint": read_only,
                "openWorldHint": self.notifies,
            },
        })
    }
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

const ID: &str = "UUID(소문자, 하이픈 포함)";

fn id_prop(what: &str) -> Value {
    json!({ "type": "string", "description": format!("{what} ID. {ID}") })
}

fn request_id_prop() -> Value {
    json!({ "type": "string", "maxLength": 100, "description": "같은 요청을 다시 보낼 때 같은 값을 주면 한 번만 만든다(재시도용, 생략 가능)." })
}

fn steps_prop() -> Value {
    json!({
        "type": "array",
        "description": "결재선(순서대로). 결재자 ID는 search_members로 찾는다. kind: approve(결재)·agree(합의).",
        "items": {
            "type": "object",
            "properties": {
                "approver_id": id_prop("결재자(구성원)"),
                "kind": { "type": "string", "enum": ["approve", "agree"] }
            },
            "required": ["approver_id", "kind"],
            "additionalProperties": false
        }
    })
}

/// 모든 도구.
pub fn tools() -> Vec<Tool> {
    let read = |name, title, description, schema| Tool {
        name,
        title,
        description,
        scope: SCOPE_READ,
        schema,
        destructive: false,
        notifies: false,
    };
    let empty = || object(json!({}), &[]);
    vec![
        read("get_home_summary", "오늘 요약", "내 오늘 출퇴근, 남은 연차, 결재 대기 수, 최근 공지 등 홈 화면 요약.", empty()),
        read("get_today_attendance", "오늘 출퇴근", "오늘 출근·퇴근 기록과 할 수 있는 동작.", empty()),
        read(
            "get_monthly_attendance",
            "월별 근태",
            "한 달의 날짜별 근태(출퇴근 시각, 지각·누락, 휴가). member_id를 주면 조직장·관리자가 볼 수 있는 다른 구성원의 근태.",
            object(json!({ "month": { "type": "string", "pattern": "^\\d{4}-\\d{2}$", "description": "YYYY-MM" }, "member_id": id_prop("구성원") }), &["month"]),
        ),
        read("get_leave_balance", "남은 연차", "내 연차 잔여(부여·사용·소멸 예정).", empty()),
        read("list_leave_types", "휴가 종류", "신청할 수 있는 휴가 종류와 ID. request_leave 전에 부른다.", empty()),
        read(
            "list_my_leave_requests",
            "내 휴가 신청",
            "내 휴가 신청 목록(상태 포함). year를 생략하면 올해.",
            object(json!({ "year": { "type": "integer" } }), &[]),
        ),
        read(
            "list_approval_documents",
            "결재함",
            "결재함 문서 목록. box: todo(내가 결재할 차례), upcoming(아직 차례 아님), drafted(내가 기안), done(내가 처리), reference(참조).",
            object(json!({
                "box": { "type": "string", "enum": ["todo", "upcoming", "drafted", "done", "reference"] },
                "status": { "type": "string", "enum": ["draft", "in_progress", "approved", "rejected", "withdrawn"] },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                "cursor": { "type": "string", "description": "앞 응답의 next_cursor" }
            }), &["box"]),
        ),
        read("get_approval_document", "결재 문서", "결재 문서 상세(본문, 결재선, 이력, 첨부 목록).", object(json!({ "id": id_prop("문서") }), &["id"])),
        read("list_approval_forms", "결재 양식", "기안할 수 있는 결재 양식(ID, 필드 정의). create_approval_document 전에 부른다.", empty()),
        read("suggest_approval_line", "추천 결재선", "내 기본 결재선(상위 조직장 등)과 기본 참조자.", empty()),
        read(
            "search_members",
            "구성원 찾기",
            "이름·아이디로 구성원을 찾는다(결재자·참조자 ID를 알 때 쓴다).",
            object(json!({ "search": { "type": "string" }, "department_id": id_prop("부서") }), &[]),
        ),
        read(
            "list_notices",
            "공지 목록",
            "내가 볼 수 있는 공지(고정 먼저, 최신순)와 읽음 여부.",
            object(json!({ "limit": { "type": "integer", "minimum": 1, "maximum": 100 }, "cursor": { "type": "string" } }), &[]),
        ),
        read("get_notice", "공지 보기", "공지 본문과 첨부 목록. 읽음 처리는 하지 않는다.", object(json!({ "id": id_prop("공지") }), &["id"])),
        Tool {
            name: "request_leave",
            title: "휴가 신청",
            description: "휴가를 신청한다(결재 문서가 만들어져 바로 상신된다). leave_type_id는 list_leave_types에서. steps를 생략하면 추천 결재선을 쓴다. 날짜는 YYYY-MM-DD.",
            scope: SCOPE_WRITE,
            schema: object(json!({
                "leave_type_id": id_prop("휴가 종류"),
                "start_date": { "type": "string", "format": "date" },
                "end_date": { "type": "string", "format": "date" },
                "unit": { "type": "string", "enum": ["full_day", "am_half", "pm_half", "am_quarter", "pm_quarter"], "description": "하루 종일·오전 반차·오후 반차·오전 반반차·오후 반반차. 반차·반반차는 시작일과 종료일이 같아야 한다." },
                "reason": { "type": "string", "minLength": 1, "maxLength": 1000 },
                "steps": steps_prop(),
                "references": { "type": "array", "items": id_prop("참조자(구성원)") },
                "request_id": request_id_prop()
            }), &["leave_type_id", "start_date", "end_date", "unit", "reason"]),
            destructive: false,
            notifies: true,
        },
        Tool {
            name: "cancel_leave_request",
            title: "휴가 취소",
            description: "내 휴가 신청을 취소한다(시작 전까지).",
            scope: SCOPE_WRITE,
            schema: object(json!({ "id": id_prop("휴가 신청") }), &["id"]),
            destructive: true,
            notifies: true,
        },
        Tool {
            name: "create_approval_document",
            title: "결재 기안",
            description: "결재 문서를 만든다. submit=true면 바로 상신, 아니면 임시저장. form_id·필드는 list_approval_forms에서. values는 양식 필드 key별 값.",
            scope: SCOPE_WRITE,
            schema: object(json!({
                "form_id": id_prop("양식"),
                "title": { "type": "string", "minLength": 1, "maxLength": 200 },
                "body": { "type": "string", "maxLength": 20000 },
                "values": { "type": "object", "description": "양식 필드 값({key: 값})" },
                "steps": steps_prop(),
                "references": { "type": "array", "items": id_prop("참조자(구성원)") },
                "submit": { "type": "boolean" },
                "request_id": request_id_prop()
            }), &["form_id", "title", "steps"]),
            destructive: false,
            notifies: true,
        },
        Tool {
            name: "submit_approval_document",
            title: "임시저장 문서 상신",
            description: "내 임시저장 결재 문서를 상신한다.",
            scope: SCOPE_WRITE,
            schema: object(json!({ "id": id_prop("문서") }), &["id"]),
            destructive: false,
            notifies: true,
        },
        Tool {
            name: "approve_approval_document",
            title: "결재 승인",
            description: "내가 결재할 차례인 문서를 승인한다. 되돌릴 수 없다.",
            scope: SCOPE_WRITE,
            schema: object(json!({ "id": id_prop("문서"), "comment": { "type": "string", "maxLength": 1000 } }), &["id"]),
            destructive: true,
            notifies: true,
        },
        Tool {
            name: "reject_approval_document",
            title: "결재 반려",
            description: "내가 결재할 차례인 문서를 반려한다(사유 필수). 되돌릴 수 없다.",
            scope: SCOPE_WRITE,
            schema: object(json!({ "id": id_prop("문서"), "reason": { "type": "string", "minLength": 1, "maxLength": 1000 } }), &["id", "reason"]),
            destructive: true,
            notifies: true,
        },
        Tool {
            name: "withdraw_approval_document",
            title: "결재 회수",
            description: "내가 기안한 진행 중 문서를 회수한다(아직 아무도 처리하지 않았을 때만).",
            scope: SCOPE_WRITE,
            schema: object(json!({ "id": id_prop("문서") }), &["id"]),
            destructive: true,
            notifies: true,
        },
        Tool {
            name: "clock_in",
            title: "출근",
            description: "지금 시각으로 출근을 기록한다. work_type: office(사무실)·remote(재택)·field(외근).",
            scope: SCOPE_WRITE,
            schema: object(json!({ "work_type": { "type": "string", "enum": ["office", "remote", "field"] }, "note": { "type": "string", "maxLength": 1000 } }), &["work_type"]),
            destructive: false,
            notifies: false,
        },
        Tool {
            name: "clock_out",
            title: "퇴근",
            description: "지금 시각으로 퇴근을 기록한다.",
            scope: SCOPE_WRITE,
            schema: object(json!({}), &[]),
            destructive: false,
            notifies: false,
        },
        Tool {
            name: "mark_notice_read",
            title: "공지 읽음 확인",
            description: "공지를 읽었다고 확인한다(필독 공지의 확인).",
            scope: SCOPE_WRITE,
            schema: object(json!({ "id": id_prop("공지") }), &["id"]),
            destructive: false,
            notifies: false,
        },
        Tool {
            name: "create_notice",
            title: "공지 게시",
            description: "공지를 게시한다(관리자만). 대상 구성원 전원에게 알림이 간다. audience: all(전체) 또는 department(department_id 필요, 하위 부서 포함).",
            scope: SCOPE_ADMIN,
            schema: object(json!({
                "title": { "type": "string", "minLength": 1, "maxLength": 200 },
                "body": { "type": "string", "minLength": 1, "maxLength": 20000 },
                "audience": { "type": "string", "enum": ["all", "department"] },
                "department_id": id_prop("부서"),
                "pinned": { "type": "boolean" },
                "must_read": { "type": "boolean" },
                "request_id": request_id_prop()
            }), &["title", "body", "audience"]),
            destructive: false,
            notifies: true,
        },
    ]
}

/// 도구 실행 실패.
pub enum CallError {
    /// 모르는 도구.
    Unknown,
    /// 인자 형식이 틀렸다(도구 결과의 오류로 알려 모델이 고치게 한다).
    BadArguments(String),
    /// 서비스가 거부했다(권한, 검증, 충돌 등).
    Service(ServiceError),
}

impl From<ServiceError> for CallError {
    fn from(error: ServiceError) -> Self {
        Self::Service(error)
    }
}

fn args<T: DeserializeOwned>(value: &Value) -> Result<T, CallError> {
    serde_json::from_value(value.clone()).map_err(|e| CallError::BadArguments(e.to_string()))
}

#[derive(Deserialize)]
struct IdArg<T> {
    id: T,
}

#[derive(Deserialize, Default)]
struct RequestIdArg {
    #[serde(default)]
    request_id: Option<String>,
}

/// `request_id`가 있으면 멱등성 키로 쓴다. 지문은 도구 이름과 인자 전체.
fn idempotency(tool: &str, value: &Value) -> Result<Option<Idempotency>, CallError> {
    let RequestIdArg { request_id } = args::<RequestIdArg>(value).unwrap_or_default();
    let Some(request_id) = request_id.filter(|r| !r.trim().is_empty()) else {
        return Ok(None);
    };
    if request_id.len() > 100 {
        return Err(CallError::BadArguments("request_id가 너무 깁니다".into()));
    }
    Ok(Some(Idempotency {
        key: format!("mcp:{tool}:{request_id}"),
        fingerprint: fingerprint(&[tool.as_bytes(), value.to_string().as_bytes()]),
    }))
}

fn to_value<T: Serialize>(value: T) -> Result<Value, CallError> {
    serde_json::to_value(value).map_err(|e| CallError::Service(ServiceError::internal(e)))
}

/// 도구를 실행한다. 결과는 기존 API 응답과 같은 모양의 JSON.
pub async fn call(
    state: &AppState,
    ctx: &AuthContext,
    name: &str,
    input: &Value,
) -> Result<Value, CallError> {
    use service::{approval, attendance, home, leave, members, notices};
    let input = if input.is_null() { &json!({}) } else { input };
    match name {
        "get_home_summary" => to_value(home::home(state, ctx).await?),
        "get_today_attendance" => to_value(attendance::today(state, ctx).await?),
        "get_monthly_attendance" => {
            to_value(attendance::month_records(state, ctx, &args::<MonthQuery>(input)?).await?)
        }
        "get_leave_balance" => to_value(leave::balance(state, ctx, None).await?),
        "list_leave_types" => to_value(leave::list_types(state).await?),
        "list_my_leave_requests" => {
            let query = LeaveRequestQuery {
                member_id: None,
                ..args::<LeaveRequestQuery>(input)?
            };
            to_value(leave::list_requests(state, ctx, &query).await?)
        }
        "list_approval_documents" => {
            to_value(approval::list_documents(state, ctx, &args::<DocumentQuery>(input)?).await?)
        }
        "get_approval_document" => {
            let IdArg { id } = args::<IdArg<DocumentId>>(input)?;
            to_value(approval::get_document(state, ctx, id).await?)
        }
        "list_approval_forms" => to_value(approval::list_forms(state).await?),
        "suggest_approval_line" => to_value(approval::line_suggestion(state, ctx).await?),
        "search_members" => {
            let query = MemberQuery {
                include_inactive: false,
                ..args::<MemberQuery>(input)?
            };
            to_value(members::list_directory(state, ctx, &query).await?)
        }
        "list_notices" => to_value(notices::list(state, ctx, &args::<PageQuery>(input)?).await?),
        "get_notice" => {
            let IdArg { id } = args::<IdArg<NoticeId>>(input)?;
            to_value(notices::get(state, ctx, id).await?)
        }
        "request_leave" => {
            let request = args::<CreateLeaveRequest>(input)?;
            let idem = idempotency(name, input)?;
            to_value(leave::create_request(state, ctx, &request, idem.as_ref()).await?)
        }
        "cancel_leave_request" => {
            let IdArg { id } = args::<IdArg<LeaveRequestId>>(input)?;
            to_value(leave::cancel_request(state, ctx, id).await?)
        }
        "create_approval_document" => {
            let request = args::<NewDocumentRequest>(input)?;
            let idem = idempotency(name, input)?;
            to_value(approval::create_document(state, ctx, &request, idem.as_ref()).await?)
        }
        "submit_approval_document" => {
            let IdArg { id } = args::<IdArg<DocumentId>>(input)?;
            to_value(approval::submit_document(state, ctx, id).await?)
        }
        "approve_approval_document" => {
            #[derive(Deserialize)]
            struct Approve {
                id: DocumentId,
                #[serde(default)]
                comment: Option<String>,
            }
            let Approve { id, comment } = args(input)?;
            to_value(approval::approve(state, ctx, id, &ApproveRequest { comment }).await?)
        }
        "reject_approval_document" => {
            #[derive(Deserialize)]
            struct Reject {
                id: DocumentId,
                reason: String,
            }
            let Reject { id, reason } = args(input)?;
            to_value(approval::reject(state, ctx, id, &RejectRequest { reason }).await?)
        }
        "withdraw_approval_document" => {
            let IdArg { id } = args::<IdArg<DocumentId>>(input)?;
            to_value(approval::withdraw(state, ctx, id).await?)
        }
        "clock_in" => {
            to_value(attendance::clock_in(state, ctx, &args::<ClockInRequest>(input)?).await?)
        }
        "clock_out" => to_value(attendance::clock_out(state, ctx).await?),
        "mark_notice_read" => {
            let IdArg { id } = args::<IdArg<NoticeId>>(input)?;
            to_value(notices::mark_read(state, ctx, id).await?)
        }
        "create_notice" => {
            let request = args::<NoticeInput>(input)?;
            let idem = idempotency(name, input)?;
            to_value(notices::create(state, ctx, &request, idem.as_ref()).await?)
        }
        _ => Err(CallError::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_unique_and_scoped() {
        let all = tools();
        let names: std::collections::BTreeSet<_> = all.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), all.len());
        for tool in &all {
            assert!(
                [SCOPE_READ, SCOPE_WRITE, SCOPE_ADMIN].contains(&tool.scope),
                "{}",
                tool.name
            );
            assert_eq!(tool.schema["type"], "object", "{}", tool.name);
            // 쓰는 도구는 읽기 전용이 아니라고 밝힌다(클라이언트가 확인 화면을 띄운다).
            let described = tool.describe();
            assert_eq!(
                described["annotations"]["readOnlyHint"],
                tool.scope == SCOPE_READ,
                "{}",
                tool.name
            );
        }
    }
}
