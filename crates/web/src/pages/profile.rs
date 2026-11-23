//! 내 정보와 로그인 기기 관리.

use dioxus::prelude::*;
use pango_api::auth::SessionInfo;
use pango_api::integration::{
    BuzzLinkStart, BuzzLinkStatus, BuzzLinkVerify, MyBuzzDto, NotificationPrefsDto,
    NotificationPrefsInput,
};
use pango_api::Empty;

use crate::api;
use crate::format;
use crate::route::Route;
use crate::session::{self, use_session};
use crate::text;
use crate::ui::{
    report_error, report_form_error, report_success, resource_view, Avatar, Badge, Button,
    ButtonKind, Card, Checkbox, ConfirmModal, FormError, Meta, PageHeader, TextField, Tone,
};

#[component]
pub fn Profile() -> Element {
    let session = use_session();
    let nav = navigator();
    let mut sessions =
        use_resource(|| async { api::get::<Vec<SessionInfo>>("/me/sessions").await });
    // 로그아웃시키는 중인 기기. 끝날 때까지 다른 "로그아웃"도 막는다.
    let mut revoking = use_signal(|| None::<String>);
    let Some(me) = session.me() else {
        return rsx! {};
    };
    let tz = format::member_zone(Some(&me));

    let logout = move |_| session::logout(session, nav);
    let revoke = move |id: String| async move {
        if revoking().is_some() {
            return;
        }
        revoking.set(Some(id.clone()));
        match api::delete::<Empty>(&format!("/me/sessions/{id}")).await {
            Ok(_) => {
                report_success("기기에서 로그아웃했어요.");
                sessions.restart();
            }
            Err(error) => report_error(&error),
        }
        revoking.set(None);
    };

    rsx! {
        PageHeader { title: "내 정보" }
        Card {
            div { class: "profile",
                Avatar { name: me.name.clone(), seed: me.id.to_string(), large: true }
                div { class: "profile-text",
                    h2 { class: "profile-name",
                        "{me.name}"
                        Badge { tone: Tone::Info, {text::role_label(me.role)} }
                    }
                    Meta {
                        items: vec![
                            me.department_name.clone().unwrap_or_default(),
                            me.position_name.clone().unwrap_or_default(),
                        ],
                    }
                }
            }
            dl { class: "detail-list",
                dt { "이메일" }
                dd { "{me.login_id}" }
                dt { "전화번호" }
                dd { {me.phone.clone().unwrap_or_else(|| "-".into())} }
            }
            div { class: "panel-actions",
                Link { class: "btn btn-grey", to: Route::ChangePassword {}, "비밀번호 변경" }
                Button { kind: ButtonKind::Grey, onclick: logout, "로그아웃" }
            }
        }
        ExternalNotifyCard {}
        BuzzLinkCard {}
        TotpCard {}
        Card { title: "로그인된 기기",
            {resource_view(sessions, |list: Vec<SessionInfo>| rsx! {
                ul { class: "list",
                    for item in list {
                        li { key: "{item.id}", class: "list-row",
                            div { class: "list-row-main",
                                strong {
                                    {item.user_agent.clone().unwrap_or_else(|| "알 수 없는 기기".into())}
                                }
                                Meta {
                                    items: vec![
                                        format!("마지막 사용 {}", format::datetime(item.last_seen_at, tz)),
                                        item.ip.clone().unwrap_or_default(),
                                    ],
                                }
                            }
                            div { class: "list-row-side",
                                if item.current {
                                    Badge { tone: Tone::Success, "현재 기기" }
                                } else {
                                    Button {
                                        kind: ButtonKind::Grey,
                                        small: true,
                                        busy: revoking().as_deref() == Some(item.id.to_string().as_str()),
                                        disabled: revoking().is_some(),
                                        onclick: {
                                            let id = item.id.to_string();
                                            move |_| revoke(id.clone())
                                        },
                                        "로그아웃"
                                    }
                                }
                            }
                        }
                    }
                }
            })}
        }
    }
}

/// 외부 메신저로도 알림 받기. 회사가 연동을 켰을 때만 보인다.
#[component]
fn ExternalNotifyCard() -> Element {
    let mut prefs = use_resource(|| async {
        api::get::<NotificationPrefsDto>("/me/notification-preferences").await
    });
    let mut saving = use_signal(|| false);
    let current = match &*prefs.read() {
        Some(Ok(current)) if current.external_available => *current,
        // 불러오는 중이거나 실패했거나 연동이 꺼져 있으면 그리지 않는다(부가 설정이다).
        _ => return rsx! {},
    };
    let toggle = move |value: bool| async move {
        if saving() {
            return;
        }
        saving.set(true);
        let input = NotificationPrefsInput {
            external_enabled: value,
        };
        match api::put::<_, NotificationPrefsDto>("/me/notification-preferences", &input).await {
            Ok(_) => {
                report_success(if value {
                    "외부 메신저로도 알림을 보내 드릴게요."
                } else {
                    "외부 메신저로는 알림을 보내지 않을게요."
                });
                prefs.restart();
            }
            Err(error) => report_error(&error),
        }
        saving.set(false);
    };
    rsx! {
        Card { title: "알림",
            Checkbox {
                label: "외부 메신저로도 알림 받기",
                hint: "앱 알림을 회사 메신저로도 보내요. 꺼도 앱 알림은 그대로 와요.",
                checked: current.external_enabled,
                disabled: saving(),
                onchange: toggle,
            }
        }
    }
}

const BUZZ_PATH: &str = "/me/buzz";

/// 사내 메신저 Buzz 연결. 회사가 Buzz 채널을 켰을 때만 보인다.
#[component]
fn BuzzLinkCard() -> Element {
    let mut link = use_resource(|| async { api::get::<MyBuzzDto>(BUZZ_PATH).await });
    let mut pubkey = use_signal(String::new);
    let mut code = use_signal(String::new);
    let mut form_error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    // 코드를 기다리는 중에 다른 ID로 바꾸려고 입력 칸을 다시 연다.
    let mut editing = use_signal(|| false);
    let mut confirm_unlink = use_signal(|| false);
    let current = match &*link.read() {
        Some(Ok(current)) if current.available || current.status != BuzzLinkStatus::None => {
            current.clone()
        }
        _ => return rsx! {},
    };

    let send_code = move |value: String| async move {
        if busy() {
            return;
        }
        busy.set(true);
        form_error.set(String::new());
        match api::post::<_, MyBuzzDto>(BUZZ_PATH, &BuzzLinkStart { pubkey: value }).await {
            Ok(_) => {
                report_success("Buzz DM으로 확인 코드를 보냈어요.");
                editing.set(false);
                code.set(String::new());
                link.restart();
            }
            Err(error) => report_form_error(&error, form_error),
        }
        busy.set(false);
    };
    let submit_id = move |event: FormEvent| {
        event.prevent_default();
        send_code(pubkey())
    };
    let submit_code = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        busy.set(true);
        form_error.set(String::new());
        let request = BuzzLinkVerify { code: code() };
        match api::post::<_, MyBuzzDto>(&format!("{BUZZ_PATH}/verify"), &request).await {
            Ok(_) => {
                report_success("Buzz를 연결했어요. 이제 알림이 Buzz DM으로도 와요.");
                link.restart();
            }
            Err(error) => {
                report_form_error(&error, form_error);
                link.restart();
            }
        }
        busy.set(false);
    };
    let unlink = move |_| async move {
        if busy() {
            return;
        }
        busy.set(true);
        match api::delete::<MyBuzzDto>(BUZZ_PATH).await {
            Ok(_) => {
                report_success("Buzz 연결을 끊었어요.");
                link.restart();
            }
            Err(error) => report_error(&error),
        }
        confirm_unlink.set(false);
        busy.set(false);
    };

    let pending_npub = current.npub.clone().unwrap_or_default();
    let resend_wait = current
        .resend_available_at
        .map(|at| (at.as_millis() - crate::ui::now().as_millis()).max(0) / 1000)
        .unwrap_or(0);
    let show_id_form = current.status == BuzzLinkStatus::None || editing();

    rsx! {
        Card {
            title: "Buzz 연결",
            action: rsx! {
                match current.status {
                    BuzzLinkStatus::Verified => rsx! { Badge { tone: Tone::Success, "연결됨" } },
                    BuzzLinkStatus::Pending => rsx! { Badge { tone: Tone::Warning, "확인 기다림" } },
                    BuzzLinkStatus::None => rsx! {},
                }
            },
            if !current.available {
                p { class: "field-hint", "회사에서 Buzz 연동을 꺼 두어 지금은 Buzz로 알림이 가지 않아요." }
            }
            FormError { message: form_error() }
            if current.status == BuzzLinkStatus::Verified && !editing() {
                dl { class: "detail-list",
                    dt { "Buzz ID" }
                    dd { class: "mono-wrap", "{pending_npub}" }
                }
                div { class: "panel-actions",
                    Button {
                        kind: ButtonKind::Grey,
                        onclick: move |_| confirm_unlink.set(true),
                        "연결 해제"
                    }
                }
            } else if show_id_form {
                p { class: "field-hint",
                    "Buzz에서 복사한 내 Buzz ID를 붙여 넣으면 알림 봇이 Buzz DM으로 6자리 확인 코드를 보내요."
                }
                form { class: "form", onsubmit: submit_id,
                    TextField {
                        label: "내 Buzz ID",
                        value: pubkey(),
                        placeholder: "npub1…",
                        autocomplete: "off",
                        maxlength: 100,
                        oninput: move |value| pubkey.set(value),
                    }
                    div { class: "panel-actions",
                        Button { submit: true, busy: busy(), "확인 코드 받기" }
                        if editing() {
                            Button { kind: ButtonKind::Grey, onclick: move |_| editing.set(false), "취소" }
                        }
                    }
                }
            } else {
                p { class: "field-hint",
                    "Buzz에서 알림 봇이 보낸 DM의 6자리 코드를 입력해 주세요. 코드는 10분 동안 쓸 수 있어요."
                }
                dl { class: "detail-list",
                    dt { "Buzz ID" }
                    dd { class: "mono-wrap", "{pending_npub}" }
                    if let Some(left) = current.attempts_left {
                        dt { "남은 기회" }
                        dd { "{left}번" }
                    }
                }
                form { class: "form", onsubmit: submit_code,
                    TextField {
                        label: "확인 코드",
                        value: code(),
                        placeholder: "6자리 숫자",
                        autocomplete: "one-time-code",
                        maxlength: 7,
                        oninput: move |value| code.set(value),
                    }
                    div { class: "panel-actions",
                        Button { submit: true, busy: busy(), "확인" }
                        Button {
                            kind: ButtonKind::Grey,
                            disabled: busy() || resend_wait > 0,
                            onclick: {
                                let npub = pending_npub.clone();
                                move |_| send_code(npub.clone())
                            },
                            if resend_wait > 0 { "{resend_wait}초 뒤 다시 받기" } else { "코드 다시 받기" }
                        }
                        Button {
                            kind: ButtonKind::Grey,
                            onclick: move |_| {
                                pubkey.set(String::new());
                                editing.set(true);
                            },
                            "다른 ID로 바꾸기"
                        }
                    }
                }
            }
            BuzzGuide { bot_npub: current.bot_npub.clone() }
        }
        if confirm_unlink() {
            ConfirmModal {
                title: "Buzz 연결을 끊을까요?",
                message: "끊으면 아직 보내지 않은 알림도 Buzz로 가지 않아요. 앱 알림은 그대로 와요. 언제든 다시 연결할 수 있어요.",
                confirm_label: "연결 해제",
                busy: busy(),
                onconfirm: unlink,
                oncancel: move |_| confirm_unlink.set(false),
            }
        }
    }
}

/// Buzz ID 찾는 법과 자주 묻는 것. Buzz 화면은 영어라 원문을 괄호로 함께 적는다.
#[component]
fn BuzzGuide(bot_npub: Option<String>) -> Element {
    rsx! {
        details { class: "guide",
            summary { "내 Buzz ID는 어디서 찾나요?" }
            div { class: "guide-body",
                div {
                    h3 { "데스크톱 앱" }
                    p {
                        "메뉴의 설정(Settings…, ⌘ + ,) → 신원(Identity) → 내 Buzz ID(Your Buzz ID) 카드에서 복사 버튼(Copy Buzz ID)을 눌러요."
                    }
                }
                div {
                    h3 { "모바일 앱" }
                    p {
                        "홈 메뉴의 설정(Settings) → 상태(Status) → 공개키 복사(Copy public key (npub))를 눌러요."
                    }
                }
                div {
                    h3 { "데스크톱과 휴대폰이 다른 계정이에요" }
                    p {
                        "Buzz는 기기에 저장된 키가 곧 계정이라, 휴대폰에서 QR로 커뮤니티에 들어가거나 새 신원 만들기(Create an identity and join)를 누르면 데스크톱과 다른 계정이 생겨요. 데스크톱과 같은 방법으로 로그인해야 같은 계정이 돼요."
                    }
                    ol {
                        li { "데스크톱 Buzz의 설정(Settings…) → 신원(Identity) → 커뮤니티 로그인(Community sign-ins)에서 어떤 방식으로 로그인했는지 봐요." }
                        li { "회사 이메일(Builderlab)로 로그인했다면: 휴대폰에서 설정(Settings) → 로그아웃(Sign out) 후, 같은 회사 이메일로 Builderlab 로그인을 해요. 데스크톱 설정의 모바일 앱(Mobile app) 화면에 쓸 이메일이 나와요." }
                        li { "키로만 쓰는 계정이라면: 데스크톱 설정 → 모바일 연결(Pair mobile)의 QR을 휴대폰 커뮤니티 추가 → QR 스캔(Scan a QR code)으로 찍어요." }
                        li { "두 기기의 Buzz ID(공개키)가 같은지 확인해요. 잘못 생긴 계정은 관리자에게 구성원에서 빼 달라고 해 주세요." }
                    }
                    p { "비밀키(nsec)를 메신저나 메모로 옮기지 마세요." }
                }
                div {
                    h3 { "코드가 오지 않아요" }
                    p {
                        "Buzz의 DM 목록에서 알림 봇이 보낸 메시지를 찾아 주세요. 1분 뒤 다시 받을 수 있고, 계속 오지 않으면 관리자에게 문의해 주세요(알림 봇이 아직 Buzz에 등록되지 않았을 수 있어요)."
                    }
                    if let Some(bot) = bot_npub {
                        p { "알림 봇 ID: " span { class: "mono-wrap", "{bot}" } }
                    }
                }
                div {
                    h3 { "Buzz로 무엇이 가나요?" }
                    p {
                        "앱 알림의 제목, 짧은 본문, 그룹웨어 링크만 가요. 휴가 사유나 결재 문서 내용은 보내지 않아요. 외부 메신저 알림을 끄거나 연결을 끊으면 더는 가지 않아요."
                    }
                }
            }
        }
    }
}

/// 2단계 인증 켜기 단계.
#[derive(Debug, Clone, PartialEq)]
enum TotpStep {
    /// 켜기 전: 비밀번호 입력.
    Password,
    /// 인증 앱에 비밀키를 넣고 첫 코드 입력.
    Confirm(pango_api::auth::TotpSetup),
    /// 켜짐: 복구 코드를 한 번만 보여 준다.
    Recovery(Vec<String>),
}

/// 2단계 인증(TOTP). 켜면 로그인할 때 인증 앱의 코드가 더 필요하다.
#[component]
fn TotpCard() -> Element {
    let mut status =
        use_resource(|| async { api::get::<pango_api::auth::TotpStatus>("/me/totp").await });
    let mut step = use_signal(|| None::<TotpStep>);
    let mut password = use_signal(String::new);
    let mut code = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);

    let mut reset = move || {
        password.set(String::new());
        code.set(String::new());
        error.set(String::new());
    };
    let start = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        busy.set(true);
        let request = pango_api::auth::TotpSetupRequest {
            password: password(),
        };
        match api::post::<_, pango_api::auth::TotpSetup>("/me/totp/setup", &request).await {
            Ok(setup) => {
                reset();
                step.set(Some(TotpStep::Confirm(setup)));
            }
            Err(failure) => error.set(failure.message()),
        }
        busy.set(false);
    };
    let confirm = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        busy.set(true);
        let request = pango_api::auth::TotpConfirmRequest {
            code: code.read().trim().to_owned(),
        };
        match api::post::<_, pango_api::auth::RecoveryCodes>("/me/totp/confirm", &request).await {
            Ok(done) => {
                reset();
                step.set(Some(TotpStep::Recovery(done.codes)));
                status.restart();
            }
            Err(failure) => error.set(failure.message()),
        }
        busy.set(false);
    };
    let disable = move |event: FormEvent| async move {
        event.prevent_default();
        if busy() {
            return;
        }
        busy.set(true);
        let request = pango_api::auth::TotpDisableRequest {
            password: password(),
            code: code.read().trim().to_owned(),
        };
        match api::post::<_, pango_api::auth::TotpStatus>("/me/totp/disable", &request).await {
            Ok(_) => {
                reset();
                report_success("2단계 인증을 껐어요.");
                status.restart();
            }
            Err(failure) => error.set(failure.message()),
        }
        busy.set(false);
    };

    let current = match &*status.read() {
        Some(Ok(current)) => *current,
        _ => return rsx! {},
    };
    rsx! {
        Card { title: "2단계 인증",
            action: rsx! {
                if current.enabled {
                    Badge { tone: Tone::Success, "켜짐" }
                } else {
                    Badge { tone: Tone::Neutral, "꺼짐" }
                }
            },
            p { class: "field-hint",
                "켜면 로그인할 때 비밀번호와 함께 인증 앱(Google Authenticator, 1Password 등)의 6자리 코드를 넣어요."
            }
            FormError { message: error() }
            match step() {
                Some(TotpStep::Recovery(codes)) => rsx! {
                    div { class: "callout callout-warning", role: "alert",
                        div {
                            p { strong { "복구 코드를 안전한 곳에 적어 두세요." } }
                            p { "휴대폰을 잃어버렸을 때 인증 코드 대신 한 번씩 쓸 수 있어요. 이 화면을 닫으면 다시 볼 수 없어요." }
                            ul { class: "recovery-codes",
                                for item in codes {
                                    li { key: "{item}", class: "mono-wrap", "{item}" }
                                }
                            }
                        }
                    }
                    div { class: "panel-actions",
                        Button { onclick: move |_| step.set(None), "적어 뒀어요" }
                    }
                },
                Some(TotpStep::Confirm(setup)) => rsx! {
                    ol { class: "checklist",
                        li { "인증 앱에서 계정을 추가하고 아래 키를 직접 입력해요(시간 기준, 6자리)." }
                        li { "앱에 나온 6자리 코드를 입력해 확인해요." }
                    }
                    p { class: "mono-wrap totp-secret", {logic::group_secret(&setup.secret)} }
                    p { class: "field-hint",
                        a { href: "{setup.otpauth_uri}", "이 기기의 인증 앱으로 열기" }
                    }
                    form { class: "form", onsubmit: confirm,
                        TextField {
                            label: "인증 코드",
                            value: code(),
                            autocomplete: "one-time-code",
                            maxlength: 10,
                            required: true,
                            oninput: move |value| code.set(value),
                        }
                        div { class: "panel-actions",
                            Button { kind: ButtonKind::Grey, onclick: move |_| { reset(); step.set(None) }, "취소" }
                            Button { submit: true, busy: busy(), "확인하고 켜기" }
                        }
                    }
                },
                Some(TotpStep::Password) => rsx! {
                    form { class: "form", onsubmit: start,
                        TextField {
                            label: "지금 비밀번호",
                            kind: "password",
                            value: password(),
                            autocomplete: "current-password",
                            required: true,
                            oninput: move |value| password.set(value),
                        }
                        div { class: "panel-actions",
                            Button { kind: ButtonKind::Grey, onclick: move |_| { reset(); step.set(None) }, "취소" }
                            Button { submit: true, busy: busy(), "다음" }
                        }
                    }
                },
                None if current.enabled => rsx! {
                    p { class: "field-hint", "남은 복구 코드 {current.recovery_codes_left}개" }
                    form { class: "form", onsubmit: disable,
                        TextField {
                            label: "지금 비밀번호",
                            kind: "password",
                            value: password(),
                            autocomplete: "current-password",
                            required: true,
                            oninput: move |value| password.set(value),
                        }
                        TextField {
                            label: "인증 코드 또는 복구 코드",
                            value: code(),
                            autocomplete: "one-time-code",
                            maxlength: 20,
                            required: true,
                            oninput: move |value| code.set(value),
                        }
                        div { class: "panel-actions",
                            Button { kind: ButtonKind::Grey, submit: true, busy: busy(), "2단계 인증 끄기" }
                        }
                    }
                },
                None => rsx! {
                    div { class: "panel-actions",
                        Button { onclick: move |_| step.set(Some(TotpStep::Password)), "2단계 인증 켜기" }
                    }
                },
            }
        }
    }
}

pub mod logic {
    /// 비밀키를 4글자씩 띄워 읽기 쉽게 한다.
    pub fn group_secret(secret: &str) -> String {
        secret
            .as_bytes()
            .chunks(4)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn secret_is_grouped_by_four() {
            assert_eq!(group_secret("ABCDEFGHIJ"), "ABCD EFGH IJ");
            assert_eq!(group_secret(""), "");
        }
    }
}
