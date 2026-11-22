//! 외부 알림 연동: 범용 웹훅(켜기·주소, 서명 키, 테스트 발송, 발송 현황)과 사내 메신저 Buzz
//! (켜기·끄기, 서버 설정 상태, 연결한 구성원 수, 발송 현황, 구성원 Buzz ID 일괄 등록).

use dioxus::prelude::*;
use pango_api::integration::{
    BuzzBulkInput, BuzzBulkResult, BuzzSettingsDto, BuzzSettingsInput, DeliveryStatsDto,
    WebhookSecretDto, WebhookSettingsDto, WebhookSettingsInput, WebhookTestResult,
};
use pango_api::{Empty, Versioned};
use pango_core::outbound::MAX_WEBHOOK_URL_LEN;

use crate::api;
use crate::format;
use crate::session::use_session;
use crate::text;
use crate::ui::{
    report_error, report_success, resource_view, Badge, Button, ButtonKind, Card, Checkbox,
    ConfirmModal, FormError, Icon, IconName, PageHeader, SecretReveal, Stat, TextArea, TextField,
    Tone,
};

const PATH: &str = "/admin/integrations/webhook";
const BUZZ_PATH: &str = "/admin/integrations/buzz";
/// 운영자 가이드(봇 키 만들기, 릴레이 구성원 등록, 실행 설정).
const BUZZ_GUIDE: &str = "https://github.com/pango-gy/pango-groupware/blob/main/docs/BUZZ.md";

#[component]
pub fn AdminIntegrations() -> Element {
    let mut settings = use_resource(|| async { api::get::<WebhookSettingsDto>(PATH).await });
    let mut buzz = use_resource(|| async { api::get::<BuzzSettingsDto>(BUZZ_PATH).await });
    rsx! {
        PageHeader {
            title: "외부 알림 연동",
            description: "앱 알림을 사내 메신저 같은 외부 서비스로도 보내요.",
        }
        {resource_view(buzz, move |dto: BuzzSettingsDto| rsx! {
            BuzzPanel { key: "{dto.version}", dto, onreload: move |_| buzz.restart() }
        })}
        BuzzBulkLink { onlinked: move |_| buzz.restart() }
        {resource_view(settings, move |dto: WebhookSettingsDto| rsx! {
            WebhookEditor { key: "{dto.version}", dto, onreload: move |_| settings.restart() }
        })}
    }
}

/// 최근 24시간 발송 현황 타일.
#[component]
fn DeliveryStats(stats: DeliveryStatsDto) -> Element {
    let session = use_session();
    let tz = session.zone();
    rsx! {
        div { class: "stats",
            Stat { label: "보내는 중", value: format!("{}건", stats.pending) }
            Stat { label: "최근 24시간 보냄", value: format!("{}건", stats.sent_24h) }
            Stat {
                label: "최근 24시간 실패",
                value: format!("{}건", stats.failed_24h),
                tone: if stats.failed_24h > 0 { Tone::Danger } else { Tone::Neutral },
            }
        }
        if let (Some(reason), Some(at)) = (stats.last_error.clone(), stats.last_error_at) {
            p { class: "field-hint",
                "마지막 실패: {text::delivery_error_label(&reason)} ({format::datetime(at, tz)})"
            }
        }
    }
}

#[component]
fn BuzzPanel(dto: BuzzSettingsDto, onreload: EventHandler<()>) -> Element {
    let mut saving = use_signal(|| false);
    let version = dto.version;
    let enabled = dto.enabled;
    let toggle = move |_| async move {
        if saving() {
            return;
        }
        saving.set(true);
        let request = Versioned {
            version,
            data: BuzzSettingsInput { enabled: !enabled },
        };
        match api::put::<_, BuzzSettingsDto>(BUZZ_PATH, &request).await {
            Ok(_) => {
                report_success(if enabled {
                    "Buzz로 보내기를 껐어요."
                } else {
                    "Buzz로 보내기를 켰어요."
                });
                onreload.call(());
            }
            Err(failure) => {
                saving.set(false);
                report_error(&failure);
                if failure.code == "version_conflict" {
                    onreload.call(());
                }
            }
        }
    };
    rsx! {
        Card {
            title: "Buzz(사내 메신저)",
            action: rsx! {
                if dto.enabled {
                    Badge { tone: Tone::Success, "보내는 중" }
                } else if dto.configured {
                    Badge { tone: Tone::Neutral, "꺼짐" }
                } else {
                    Badge { tone: Tone::Warning, "서버 설정 필요" }
                }
            },
            p { class: "field-hint",
                "알림을 구성원의 Buzz DM으로도 보내요. 구성원이 내 정보에서 Buzz ID를 직접 연결하거나, 관리자가 아래에서 한꺼번에 등록해요. Buzz에는 알림 제목·짧은 본문·링크만 가요."
            }
            dl { class: "detail-list",
                dt { "서버 설정" }
                dd {
                    if dto.configured { "설정됨" } else { "설정 안 됨" }
                }
                dt { "릴레이 주소" }
                dd { {dto.relay_url.clone().unwrap_or_else(|| "-".into())} }
                dt { "알림 봇 ID" }
                dd { class: "mono-wrap", {dto.bot_npub.clone().unwrap_or_else(|| "-".into())} }
                dt { "연결한 구성원" }
                dd { "{dto.linked_members}명" }
            }
            if !dto.configured {
                div { class: "callout callout-info", role: "note",
                    Icon { name: IconName::Alert }
                    div {
                        p { "Buzz를 켜려면 서버 운영자가 먼저 설정해야 해요." }
                        ol { class: "checklist",
                            li { "Buzz 관리 도구로 알림 봇 키를 만들어 비밀 저장소에 넣기" }
                            li { "그룹웨어 서버에 PANGO_BUZZ_RELAY_URL과 PANGO_BUZZ_BOT_KEY_FILE(또는 PANGO_BUZZ_BOT_KEY) 지정" }
                            li { "봇 ID를 Buzz 릴레이 구성원으로 등록(buzz-admin add-member)" }
                            li { "릴레이가 같은 사설망이면 --outbound-allow-private로 서버 실행" }
                        }
                    }
                }
            }
            p { class: "field-hint",
                a { href: BUZZ_GUIDE, target: "_blank", rel: "noopener noreferrer", "운영자 설정 가이드 보기" }
            }
            div { class: "panel-actions",
                Button {
                    kind: if dto.enabled { ButtonKind::Grey } else { ButtonKind::Primary },
                    busy: saving(),
                    disabled: !dto.configured && !dto.enabled,
                    onclick: toggle,
                    if dto.enabled { "Buzz로 보내기 끄기" } else { "Buzz로 보내기 켜기" }
                }
            }
            DeliveryStats { stats: dto.stats.clone() }
        }
    }
}

/// 구성원 Buzz ID 일괄 등록. 한 줄에 `아이디 Buzz ID`.
#[component]
fn BuzzBulkLink(onlinked: EventHandler<()>) -> Element {
    let mut raw = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut result = use_signal(|| None::<BuzzBulkResult>);

    let submit = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        // 보낸 내용을 따로 둔다. 응답을 기다리는 사이 입력을 고쳐도 결과를 보낸 줄에 맞춘다.
        let sent = raw.read().clone();
        let entries = match logic::parse_bulk(&sent) {
            Ok(entries) => entries,
            Err(message) => {
                error.set(message);
                return;
            }
        };
        busy.set(true);
        error.set(String::new());
        let input = BuzzBulkInput { entries };
        match api::post::<_, BuzzBulkResult>(&format!("{BUZZ_PATH}/accounts"), &input).await {
            Ok(done) => {
                if done.linked > 0 {
                    onlinked.call(());
                }
                report_success(format!(
                    "{}명을 새로 연결했어요. 이미 연결됨 {}명, 실패 {}명.",
                    done.linked, done.unchanged, done.failed
                ));
                // 실패한 줄만 남겨 고쳐서 다시 보낼 수 있게 한다.
                raw.set(logic::failed_lines(&sent, &done));
                result.set(Some(done));
            }
            Err(failure) => error.set(failure.message()),
        }
        busy.set(false);
    };

    rsx! {
        Card { title: "구성원 Buzz ID 일괄 등록",
            p { class: "field-hint",
                "한 줄에 구성원 아이디와 Buzz ID(npub1… 또는 64자 16진수)를 쉼표·탭·공백으로 나눠 적어요. "
                "본인 확인 없이 바로 연결되고, 구성원에게 \u{201c}Buzz 알림이 연결됐어요\u{201d} 알림이 가요. "
                "잘못 넣으면 다른 사람에게 알림이 가니 ID를 꼭 확인해 주세요."
            }
            form { class: "form", onsubmit: submit,
                FormError { message: error() }
                TextArea {
                    label: "아이디와 Buzz ID",
                    value: raw(),
                    rows: 8,
                    placeholder: "kim npub1…\nlee npub1…",
                    oninput: move |value| raw.set(value),
                }
                div { class: "panel-actions",
                    Button { submit: true, busy: busy(), disabled: raw.read().trim().is_empty(), "등록" }
                }
            }
            if let Some(done) = result() {
                if done.rows.iter().any(|row| !logic::is_success(&row.outcome)) {
                    ul { class: "list", "aria-label": "등록하지 못한 줄",
                        for (index, row) in done.rows.iter().enumerate().filter(|(_, row)| !logic::is_success(&row.outcome)) {
                            li { key: "{index}", class: "list-row",
                                span { class: "list-row-main", strong { "{row.login_id}" } }
                                span { class: "list-row-side", {logic::outcome_label(&row.outcome)} }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn WebhookEditor(dto: WebhookSettingsDto, onreload: EventHandler<()>) -> Element {
    let mut enabled = use_signal(|| dto.enabled);
    let mut url = use_signal(|| dto.url.clone().unwrap_or_default());
    let mut form_error = use_signal(String::new);
    let mut saving = use_signal(|| false);
    let mut confirm_rotate = use_signal(|| false);
    let mut rotating = use_signal(|| false);
    // 새로 만든 서명 키. 창을 닫으면 바로 버린다.
    let mut new_secret = use_signal(|| None::<String>);
    let mut testing = use_signal(|| false);
    let mut test_result = use_signal(|| None::<WebhookTestResult>);
    let version = dto.version;
    let saved_url = dto.url.clone();
    let has_secret = dto.has_secret;

    let save = move |event: FormEvent| async move {
        event.prevent_default();
        if saving() {
            return;
        }
        form_error.set(String::new());
        saving.set(true);
        let trimmed = url().trim().to_owned();
        let request = Versioned {
            version,
            data: WebhookSettingsInput {
                enabled: enabled(),
                url: (!trimmed.is_empty()).then_some(trimmed),
            },
        };
        match api::put::<_, WebhookSettingsDto>(PATH, &request).await {
            Ok(_) => {
                report_success("외부 알림 연동을 저장했어요.");
                onreload.call(());
            }
            Err(failure) if failure.code == "version_conflict" => {
                report_error(&failure);
                onreload.call(());
            }
            Err(failure) => {
                saving.set(false);
                if failure.code == "validation_failed" {
                    form_error.set(failure.message());
                } else {
                    report_error(&failure);
                }
            }
        }
    };

    let rotate = move |_| async move {
        if rotating() {
            return;
        }
        rotating.set(true);
        let request = Versioned {
            version,
            data: Empty {},
        };
        match api::post::<_, WebhookSecretDto>(&format!("{PATH}/secret"), &request).await {
            Ok(created) => {
                confirm_rotate.set(false);
                new_secret.set(Some(created.secret));
            }
            Err(failure) => {
                confirm_rotate.set(false);
                report_error(&failure);
                if failure.code == "version_conflict" {
                    onreload.call(());
                }
            }
        }
        rotating.set(false);
    };

    let send_test = move |_| async move {
        if testing() {
            return;
        }
        testing.set(true);
        test_result.set(None);
        match api::post::<_, WebhookTestResult>(&format!("{PATH}/test"), &Empty {}).await {
            Ok(result) => test_result.set(Some(result)),
            Err(failure) => report_error(&failure),
        }
        testing.set(false);
    };

    let can_test = saved_url.is_some() && has_secret;

    rsx! {
        Card {
            title: "웹훅",
            action: rsx! {
                if dto.enabled {
                    Badge { tone: Tone::Success, "보내는 중" }
                } else {
                    Badge { tone: Tone::Neutral, "꺼짐" }
                }
            },
            form { class: "form", onsubmit: save,
                FormError { message: form_error() }
                Checkbox {
                    label: "외부 알림 보내기",
                    hint: "켜면 새 알림이 생길 때마다 아래 주소로 서명한 JSON을 보내요. 외부 알림을 끈 구성원에게는 보내지 않아요.",
                    checked: enabled(),
                    onchange: move |value| enabled.set(value),
                }
                TextField {
                    label: "웹훅 주소",
                    kind: "url",
                    value: url(),
                    placeholder: "https://",
                    autocomplete: "off",
                    maxlength: MAX_WEBHOOK_URL_LEN as i64,
                    hint: "https 주소만 쓸 수 있어요.",
                    oninput: move |value| url.set(value),
                }
                div { class: "panel-actions",
                    Button { submit: true, busy: saving(), "저장" }
                }
            }
        }
        Card { title: "서명 키",
            p { class: "field-hint",
                if has_secret {
                    "받는 쪽은 이 키로 X-Pango-Signature 머리글을 확인해 진짜 알림인지 가려요. 키는 만들 때 한 번만 보여 줘요."
                } else {
                    "아직 서명 키가 없어요. 연동을 켜려면 먼저 만들어 받는 쪽에 넣어 주세요."
                }
            }
            div { class: "panel-actions",
                Button {
                    kind: ButtonKind::Weak,
                    onclick: move |_| confirm_rotate.set(true),
                    "서명 키 새로 만들기"
                }
            }
        }
        Card { title: "테스트 보내기",
            p { class: "field-hint", "저장한 주소로 테스트 메시지를 바로 보내요. 받는 사람은 나예요." }
            if let Some(result) = test_result() {
                if result.delivered {
                    p { class: "callout callout-success", role: "status",
                        Icon { name: IconName::Check }
                        span { "받는 쪽이 잘 받았어요." }
                    }
                } else {
                    p { class: "callout callout-warning", role: "status",
                        Icon { name: IconName::Alert }
                        span {
                            "보내지 못했어요. "
                            {text::delivery_error_label(result.error.as_deref().unwrap_or_default())}
                        }
                    }
                }
            }
            div { class: "panel-actions",
                Button {
                    kind: ButtonKind::Grey,
                    busy: testing(),
                    disabled: !can_test,
                    onclick: send_test,
                    "테스트 보내기"
                }
            }
            if !can_test {
                p { class: "field-hint", "주소를 저장하고 서명 키를 만들면 보낼 수 있어요." }
            }
        }
        Card { title: "웹훅 발송 현황",
            DeliveryStats { stats: dto.stats.clone() }
        }
        if confirm_rotate() {
            ConfirmModal {
                title: "서명 키를 새로 만들까요?",
                message: if has_secret {
                    "지금 키는 바로 쓸 수 없게 돼요. 받는 쪽에 새 키를 넣기 전까지 받는 쪽이 알림을 거절할 수 있어요."
                } else {
                    "새 키는 한 번만 보여 줘요. 바로 받는 쪽 설정에 넣어 주세요."
                },
                confirm_label: "새로 만들기",
                danger: has_secret,
                busy: rotating(),
                onconfirm: rotate,
                oncancel: move |_| confirm_rotate.set(false),
            }
        }
        if let Some(secret) = new_secret() {
            SecretReveal {
                title: "새 서명 키",
                label: "서명 키",
                secret,
                long: true,
                warning: "이 창을 닫으면 다시 볼 수 없어요. 지금 복사해 받는 쪽 설정에 넣어 주세요.",
                onclose: move |_| {
                    new_secret.set(None);
                    onreload.call(());
                },
            }
        }
    }
}

pub mod logic {
    use pango_api::integration::{BuzzBulkEntry, BuzzBulkInput, BuzzBulkResult};

    /// 일괄 등록 입력을 줄로 나눈다. 빈 줄과 `#`로 시작하는 줄은 건너뛴다.
    /// 한 줄은 쉼표·탭·공백으로 나눈 정확히 두 칸(아이디, Buzz ID)이어야 한다.
    pub fn parse_bulk(raw: &str) -> Result<Vec<BuzzBulkEntry>, String> {
        let mut entries = Vec::new();
        for (index, line) in raw.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let parts: Vec<&str> = line
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|part| !part.is_empty())
                .collect();
            let [login_id, pubkey] = parts.as_slice() else {
                return Err(format!(
                    "{}번째 줄은 \u{2018}아이디 Buzz ID\u{2019} 두 칸이어야 해요.",
                    index + 1
                ));
            };
            entries.push(BuzzBulkEntry {
                login_id: (*login_id).to_owned(),
                pubkey: (*pubkey).to_owned(),
            });
        }
        if entries.is_empty() {
            return Err("등록할 줄이 없어요.".to_owned());
        }
        if entries.len() > BuzzBulkInput::MAX {
            return Err(format!(
                "한 번에 {}줄까지 등록할 수 있어요.",
                BuzzBulkInput::MAX
            ));
        }
        Ok(entries)
    }

    pub fn is_success(outcome: &str) -> bool {
        matches!(outcome, "linked" | "unchanged")
    }

    pub fn outcome_label(outcome: &str) -> &'static str {
        match outcome {
            "linked" => "연결함",
            "unchanged" => "이미 연결됨",
            "not_found" => "없는 아이디",
            "forbidden" => "관리 권한 없음(관리자 계정은 최고 관리자만)",
            "member_inactive" => "재직 중이 아님",
            "invalid_pubkey" => "Buzz ID 형식이 아님",
            "buzz_account_taken" => "다른 구성원이 쓰는 Buzz ID",
            "duplicate" => "입력에 두 번 이상 나옴",
            _ => "등록하지 못함",
        }
    }

    /// 입력 중 실패한 줄만 남긴다(결과는 입력 순서와 같다).
    pub fn failed_lines(raw: &str, result: &BuzzBulkResult) -> String {
        let lines = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'));
        lines
            .zip(&result.rows)
            .filter(|(_, row)| !is_success(&row.outcome))
            .map(|(line, _)| line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use pango_api::integration::BuzzBulkRow;

        #[test]
        fn lines_accept_commas_tabs_and_spaces() {
            let entries = parse_bulk("# 아이디,ID\nkim, npub1aaa\n\nlee\tnpub1bbb\n  park   abc  ")
                .expect("valid");
            let pairs: Vec<(&str, &str)> = entries
                .iter()
                .map(|e| (e.login_id.as_str(), e.pubkey.as_str()))
                .collect();
            assert_eq!(
                pairs,
                [("kim", "npub1aaa"), ("lee", "npub1bbb"), ("park", "abc")]
            );
        }

        #[test]
        fn malformed_lines_are_pointed_out() {
            assert_eq!(
                parse_bulk("kim npub1aaa\nlee"),
                Err("2번째 줄은 \u{2018}아이디 Buzz ID\u{2019} 두 칸이어야 해요.".into())
            );
            assert!(parse_bulk("a b c").is_err());
            assert!(parse_bulk("  \n# only comment").is_err());
        }

        #[test]
        fn only_failed_lines_are_kept() {
            let result = BuzzBulkResult {
                linked: 1,
                unchanged: 0,
                failed: 1,
                rows: vec![
                    BuzzBulkRow {
                        login_id: "kim".into(),
                        outcome: "linked".into(),
                    },
                    BuzzBulkRow {
                        login_id: "lee".into(),
                        outcome: "not_found".into(),
                    },
                ],
            };
            assert_eq!(failed_lines("# head\nkim a\n\nlee b", &result), "lee b");
        }
    }
}
