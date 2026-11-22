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
    /// 2단계 인증을 켠 계정의 인증 앱 6자리 코드 또는 복구 코드(`abcd-efgh`). 없이 보내면 비밀번호가
    /// 맞을 때 401 `mfa_required`로 답하고, 틀린 코드면 401 `mfa_invalid`다.
    #[serde(default)]
    pub totp_code: Option<String>,
}

// 비밀번호가 로그에 찍히지 않게 한다.
impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRequest")
            .field("login_id", &self.login_id)
            .field("password", &"***")
            .field("token_in_body", &self.token_in_body)
            .field("totp_code", &self.totp_code.as_ref().map(|_| "***"))
            .finish()
    }
}

/// 내 2단계 인증 상태(`GET /me/totp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotpStatus {
    pub enabled: bool,
    /// 남은 복구 코드 수(켜져 있을 때).
    pub recovery_codes_left: u32,
}

/// 관리자가 보는 구성원의 2단계 인증 여부(`GET /admin/members/{id}/totp`). 초기화는 `DELETE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberTotpDto {
    pub enabled: bool,
}

/// 켜기 시작(`POST /me/totp/setup`). 지금 비밀번호를 다시 확인한다.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotpSetupRequest {
    pub password: String,
}

impl std::fmt::Debug for TotpSetupRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TotpSetupRequest(***)")
    }
}

/// 인증 앱에 넣을 비밀키(base32)와 `otpauth://` 주소. 이 응답에서만 보여 준다.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotpSetup {
    pub secret: String,
    pub otpauth_uri: String,
}

impl std::fmt::Debug for TotpSetup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TotpSetup(***)")
    }
}

/// 첫 코드 확인(`POST /me/totp/confirm`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotpConfirmRequest {
    pub code: String,
}

/// 복구 코드. 켤 때 한 번만 보여 준다(서버에는 해시만 남는다).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryCodes {
    pub codes: Vec<String>,
}

impl std::fmt::Debug for RecoveryCodes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RecoveryCodes({} codes)", self.codes.len())
    }
}

/// 끄기(`POST /me/totp/disable`): 비밀번호와 지금 코드(또는 복구 코드).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TotpDisableRequest {
    pub password: String,
    pub code: String,
}

impl std::fmt::Debug for TotpDisableRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TotpDisableRequest(***)")
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

/// 한꺼번에 로그아웃시킨 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokedSessions {
    pub revoked: u64,
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
            totp_code: Some("123456".into()),
        };
        assert!(!format!("{login:?}").contains("hunter2"));
        assert!(!format!("{login:?}").contains("123456"));
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
