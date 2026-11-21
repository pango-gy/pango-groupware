//! 외부 알림 연동(범용 웹훅)과 구성원의 수신 설정.

use pango_core::time::Timestamp;
use serde::{Deserialize, Serialize};

/// 웹훅 연동 수정. [`crate::Versioned`]로 감싸 보낸다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookSettingsInput {
    pub enabled: bool,
    /// 받는 쪽 주소. 비우면(`null`·빈 문자열) 주소를 지운다(끈 상태에서만).
    #[serde(default)]
    pub url: Option<String>,
}

/// 최근 발송 현황.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryStatsDto {
    /// 보내는 중이거나 다시 보낼 메시지.
    pub pending: u32,
    /// 최근 24시간에 보낸 메시지.
    pub sent_24h: u32,
    /// 최근 24시간에 실패로 끝난 메시지.
    pub failed_24h: u32,
    /// 가장 최근 실패의 짧은 사유(`http_404`, `timeout` 등). 응답 본문은 담지 않는다.
    pub last_error: Option<String>,
    pub last_error_at: Option<Timestamp>,
}

/// 웹훅 연동 설정. 서명 키는 돌려주지 않는다(`has_secret`만).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookSettingsDto {
    pub enabled: bool,
    pub url: Option<String>,
    pub has_secret: bool,
    pub version: i64,
    pub updated_at: Timestamp,
    pub stats: DeliveryStatsDto,
}

/// 새로 만든 서명 키. **이 응답에서만** 원문을 볼 수 있다.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookSecretDto {
    pub secret: String,
    pub settings: WebhookSettingsDto,
}

impl std::fmt::Debug for WebhookSecretDto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookSecretDto")
            .field("secret", &"***")
            .field("settings", &self.settings)
            .finish()
    }
}

/// 테스트 발송 결과.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookTestResult {
    /// 받는 쪽이 2xx로 답했는지.
    pub delivered: bool,
    /// 받는 쪽의 응답 상태 코드. 연결하지 못했으면 `None`.
    pub status: Option<u16>,
    /// 실패했으면 짧은 사유(`http_404`, `timeout`, `connect_failed` 등).
    pub error: Option<String>,
}

/// 내 알림 수신 설정.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPrefsDto {
    /// 회사가 외부 알림 연동을 켰는지. 꺼져 있으면 설정을 바꿔도 아무것도 보내지 않는다.
    pub external_available: bool,
    /// 외부 메신저로도 알림을 받을지.
    pub external_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPrefsInput {
    pub external_enabled: bool,
}

/// Buzz 채널 상태(관리자). 봇 비밀키는 실행 설정에만 있고 여기에 없다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuzzSettingsDto {
    /// 서버 실행 설정(릴레이 주소·봇 키)이 있는지. 없으면 켤 수 없다.
    pub configured: bool,
    pub enabled: bool,
    pub relay_url: Option<String>,
    /// 봇 계정의 공개키(`npub1…`). 릴레이 구성원 등록에 쓴다.
    pub bot_npub: Option<String>,
    /// Buzz 계정 확인을 마친 구성원 수.
    pub linked_members: u32,
    pub version: i64,
    pub updated_at: Timestamp,
    pub stats: DeliveryStatsDto,
}

/// Buzz 채널 켜기·끄기. [`crate::Versioned`]로 감싸 보낸다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuzzSettingsInput {
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuzzLinkStatus {
    /// 연결하지 않음.
    None,
    /// 확인 코드를 보냈고 입력을 기다린다.
    Pending,
    /// 확인을 마쳤다. 알림이 Buzz DM으로도 간다.
    Verified,
}

/// 내 Buzz 연결.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MyBuzzDto {
    /// 회사가 Buzz 채널을 켰는지(서버 설정 포함). 꺼져 있으면 연결할 수 없다.
    pub available: bool,
    pub status: BuzzLinkStatus,
    /// 연결(중)인 Buzz ID(`npub1…`).
    pub npub: Option<String>,
    /// 확인 코드의 만료 시각(`pending`일 때).
    pub code_expires_at: Option<Timestamp>,
    /// 남은 입력 기회(`pending`일 때).
    pub attempts_left: Option<u32>,
    /// 코드를 다시 보낼 수 있는 시각.
    pub resend_available_at: Option<Timestamp>,
    /// 코드를 보내는 봇의 Buzz ID. Buzz에서 이 계정의 DM을 확인하라고 안내한다.
    pub bot_npub: Option<String>,
}

/// 연결 시작: Buzz ID(`npub1…` 또는 64자 16진수).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuzzLinkStart {
    pub pubkey: String,
}

/// 받은 확인 코드(숫자 6자리).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuzzLinkVerify {
    pub code: String,
}

impl std::fmt::Debug for BuzzLinkVerify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BuzzLinkVerify(***)")
    }
}

/// 관리자가 보는 구성원의 Buzz 연결(해제만 할 수 있다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberBuzzDto {
    pub status: BuzzLinkStatus,
    /// 줄인 Buzz ID(`npub1abcd…wxyz`).
    pub npub_short: Option<String>,
}
