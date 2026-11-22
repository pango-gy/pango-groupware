//! 원격 MCP 엔드포인트(`POST /mcp`, Streamable HTTP, 세션 없는 JSON 응답).
//!
//! - 토큰이 없거나 틀리면 401 + `WWW-Authenticate: Bearer resource_metadata="…"`(RFC 9728) —
//!   클라이언트가 이것을 보고 OAuth를 시작한다.
//! - 도구에 필요한 scope가 없으면 403 + `error="insufficient_scope"`(다시 승인받게 한다).
//! - `Origin`이 있으면 claude.ai·claude.com·이 서버만 받는다(없으면 서버 간 호출이라 통과).
//! - MCP가 꺼져 있으면 503.

use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use super::extract::ClientMeta;
use crate::mcp::{self, CallError};
use crate::service::oauth::{self, SCOPES, SCOPE_ADMIN};
use crate::service::AuthContext;

fn is_admin(ctx: &AuthContext) -> bool {
    pango_core::authz::require_admin(&ctx.actor).is_ok()
}
use crate::state::AppState;

/// 받는 MCP 프로토콜 판(최신 먼저). 클라이언트가 이 중 하나를 보내면 그대로 쓴다.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "사내 그룹웨어(근태·휴가·전자결재·공지)를 이 사용자의 권한으로 쓴다. \
날짜는 회사 시간대 기준 YYYY-MM-DD다. 휴가 신청은 list_leave_types로 종류 ID를, 결재 기안은 \
list_approval_forms로 양식을, 결재자·참조자는 search_members로 구성원 ID를 먼저 찾는다. \
승인·반려·회수·휴가 취소·공지 게시처럼 되돌리기 어렵거나 다른 사람에게 알림이 가는 일은 실행 전에 \
사용자에게 내용을 확인받는다. 같은 요청을 다시 보낼 수 있으면 request_id를 준다.";

pub fn routes() -> Router<AppState> {
    Router::new().route("/mcp", post(handle).get(not_allowed).delete(not_allowed))
}

async fn not_allowed() -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    response
}

fn challenge(state: &AppState, status: StatusCode, extra: &str) -> Response {
    let issuer = state
        .config
        .mcp
        .as_ref()
        .map(|m| m.issuer.as_str())
        .unwrap_or_default();
    let value = format!(
        "Bearer resource_metadata=\"{issuer}/.well-known/oauth-protected-resource\", {extra}"
    );
    let mut response =
        (status, Json(json!({ "error": status.canonical_reason() }))).into_response();
    if let Ok(value) = HeaderValue::from_str(&value) {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    response
}

fn origin_allowed(headers: &HeaderMap, issuer: &str) -> bool {
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(origin) => ["https://claude.ai", "https://claude.com", issuer]
            .iter()
            .any(|allowed| origin.eq_ignore_ascii_case(allowed)),
    }
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }))
        .into_response()
}

fn rpc_result(id: &Value, result: Value) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

fn tool_text(value: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_default();
    let structured = if value.is_object() {
        value.clone()
    } else {
        json!({ "result": value })
    };
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
        "isError": is_error,
    })
}

async fn handle(
    State(state): State<AppState>,
    ClientMeta(meta): ClientMeta,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(config) = state.config.mcp.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "mcp_not_configured" })),
        )
            .into_response();
    };
    if !origin_allowed(&headers, &config.issuer) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "origin_not_allowed" })),
        )
            .into_response();
    }
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            // 인증 방식 이름은 대소문자를 가리지 않는다(RFC 7235).
            let (scheme, token) = v.split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
        });
    let authenticated = match bearer {
        Some(token) => match oauth::authenticate(&state, token, meta).await {
            Ok(found) => found,
            Err(error) => {
                tracing::error!(%error, "mcp authentication failed");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "internal" })),
                )
                    .into_response();
            }
        },
        None => None,
    };
    let Some((ctx, scopes)) = authenticated else {
        // 클라이언트는 보통 여기 적힌 scope를 요청한다. 모두 적어 한 번의 승인으로 쓸 수 있게 하고,
        // 관리 scope는 동의할 때 관리자에게만 준다. scope 없이 온 요청은 조회만 받는다.
        let all = SCOPES.join(" ");
        let extra = if bearer.is_some() {
            format!("error=\"invalid_token\", scope=\"{all}\"")
        } else {
            format!("scope=\"{all}\"")
        };
        return challenge(&state, StatusCode::UNAUTHORIZED, &extra);
    };

    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(&Value::Null, -32700, "parse error");
    };
    if !message.is_object() {
        return rpc_error(&Value::Null, -32600, "batch requests are not supported");
    }
    let method = message["method"].as_str().unwrap_or_default();
    let Some(id) = message.get("id").cloned() else {
        // 알림·응답에는 답하지 않는다.
        return StatusCode::ACCEPTED.into_response();
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            let version = PROTOCOL_VERSIONS
                .iter()
                .find(|v| **v == asked)
                .copied()
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            rpc_result(
                &id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "pango-groupware", "title": "사내 그룹웨어", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => rpc_result(&id, json!({})),
        "tools/list" => {
            // 관리 도구는 관리자에게만 보인다(받을 수 없는 권한으로 다시 로그인하게 하지 않는다).
            let tools: Vec<Value> = mcp::tools()
                .iter()
                .filter(|tool| tool.scope != SCOPE_ADMIN || is_admin(&ctx))
                .map(mcp::Tool::describe)
                .collect();
            rpc_result(&id, json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let Some(tool) = mcp::tools().into_iter().find(|t| t.name == name) else {
                return rpc_error(&id, -32602, "unknown tool");
            };
            if tool.scope == SCOPE_ADMIN && !is_admin(&ctx) {
                return rpc_result(
                    &id,
                    tool_text(
                        &json!({ "error": { "code": "forbidden", "message": "관리자만 할 수 있는 작업입니다" } }),
                        true,
                    ),
                );
            }
            if !scopes.iter().any(|s| s == tool.scope) {
                let needed: Vec<String> = scopes
                    .iter()
                    .cloned()
                    .chain([tool.scope.to_owned()])
                    .collect();
                return challenge(
                    &state,
                    StatusCode::FORBIDDEN,
                    &format!(
                        "error=\"insufficient_scope\", scope=\"{}\"",
                        needed.join(" ")
                    ),
                );
            }
            let input = params.get("arguments").cloned().unwrap_or(Value::Null);
            match mcp::call(&state, &ctx, name, &input).await {
                Ok(value) => rpc_result(&id, tool_text(&value, false)),
                Err(CallError::Unknown) => rpc_error(&id, -32602, "unknown tool"),
                Err(CallError::BadArguments(detail)) => rpc_result(
                    &id,
                    tool_text(
                        &json!({ "error": { "code": "invalid_arguments", "detail": detail } }),
                        true,
                    ),
                ),
                Err(CallError::Service(error)) => {
                    if matches!(error, crate::ServiceError::Internal(_)) {
                        tracing::error!(%error, tool = name, "mcp tool failed");
                    }
                    rpc_result(
                        &id,
                        tool_text(
                            &serde_json::to_value(error.body()).unwrap_or_default(),
                            true,
                        ),
                    )
                }
            }
        }
        _ => rpc_error(&id, -32601, "method not found"),
    }
}
