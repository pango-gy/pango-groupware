//! 인증·내 계정.

use pango_core::authz::{Capabilities, Role};
use pango_core::id::{DepartmentId, MemberId, PositionId, SessionId};
use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct LoginRequest {
    pub login_id: String,
    pub password: String,
    /// `true`면 응답 본문으로 토큰을 받는다(모바일 앱). 아니면 쿠키로 받는다(웹).
    #[serde(default)]
    pub token_in_body: bool,
}

// 비밀번호가 로그에 찍히지 않게 한다.
impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRequest")
            .field("login_id", &self.login_id)
            .field("password", &"***")
            .field("token_in_body", &self.token_in_body)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginResponse {
    /// `token_in_body`로 요청했을 때만 들어 있다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub me: Me,
}

/// 로그인한 구성원 본인의 정보.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub id: MemberId,
    pub login_id: String,
    pub name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: Role,
    pub department_id: Option<DepartmentId>,
    pub department_name: Option<String>,
    pub position_id: Option<PositionId>,
    pub position_name: Option<String>,
    /// 화면 노출용. 서버는 매 요청마다 권한을 다시 판정한다.
    pub capabilities: Capabilities,
    /// `true`면 비밀번호를 바꾸기 전까지 다른 기능을 쓸 수 없다.
    pub must_change_password: bool,
    pub company_name: String,
    /// 회사 시간대(IANA 이름). 화면의 시각 표시에 쓴다.
    pub timezone: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

impl std::fmt::Debug for ChangePasswordRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChangePasswordRequest(***)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    /// 지금 요청을 보낸 세션인지.
    pub current: bool,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: Timestamp,
    pub last_seen_at: Timestamp,
}

/// 내려받기 링크 요청(`POST /download-links`). `path`는 API 접두사를 뺀 경로와 쿼리
/// (`/notices/{id}/attachments/{attachment_id}` 또는 `/admin/attendance/export.csv?from=…&to=…`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadLinkRequest {
    pub path: String,
}

/// 몇 분 동안만 유효한 내려받기 링크(API 접두사를 포함한 경로). 앱은 서버 주소를 붙여 시스템
/// 브라우저로 연다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadLink {
    pub url: String,
    pub expires_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_hides_passwords() {
        let login = LoginRequest {
            login_id: "gildong".into(),
            password: "hunter2-secret".into(),
            token_in_body: false,
        };
        assert!(!format!("{login:?}").contains("hunter2"));
        let change = ChangePasswordRequest {
            current_password: "old-secret".into(),
            new_password: "new-secret".into(),
        };
        assert!(!format!("{change:?}").contains("secret"));
    }

    #[test]
    fn login_request_defaults_to_cookie_mode() {
        let parsed: LoginRequest =
            serde_json::from_str(r#"{"login_id":"a","password":"b"}"#).expect("parse");
        assert!(!parsed.token_in_body);
    }
}
