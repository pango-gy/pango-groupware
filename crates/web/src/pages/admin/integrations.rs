//! 외부 알림 연동(범용 웹훅): 켜기·주소, 서명 키, 테스트 발송, 발송 현황.

use dioxus::prelude::*;
use pango_api::integration::{
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
    ConfirmModal, FormError, Icon, IconName, PageHeader, SecretReveal, Stat, TextField, Tone,
};

const PATH: &str = "/admin/integrations/webhook";

#[component]
pub fn AdminIntegrations() -> Element {
    let mut settings = use_resource(|| async { api::get::<WebhookSettingsDto>(PATH).await });
    rsx! {
        PageHeader {
            title: "외부 알림 연동",
            description: "앱 알림을 사내 메신저 같은 외부 서비스로도 보내요.",
        }
        {resource_view(settings, move |dto: WebhookSettingsDto| rsx! {
            WebhookEditor { key: "{dto.version}", dto, onreload: move |_| settings.restart() }
        })}
    }
}

#[component]
fn WebhookEditor(dto: WebhookSettingsDto, onreload: EventHandler<()>) -> Element {
    let session = use_session();
    let tz = session.zone();
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

    let stats = dto.stats.clone();
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
        Card { title: "발송 현황",
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
