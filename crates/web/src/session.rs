//! 로그인 상태와 전역 알림(토스트).

use dioxus::prelude::*;
use pango_api::auth::Me;
use pango_api::Empty;

use crate::api::{self, ApiError, ApiResult};
use crate::route::Route;

/// 로그인 상태. 첫 `/me` 조회 결과로 정해지고, 이후 다시 읽을 때마다 [`next_state`]로 바뀐다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// 첫 `/me` 조회 중.
    Loading,
    /// 서버가 로그인 세션이 없다고 답했다(401).
    SignedOut,
    SignedIn(Box<Me>),
    /// 서버에 닿지 못했거나 서버가 실패했다(오프라인, 서버 재시작 등). 쿠키 세션은 살아 있을 수
    /// 있으므로 로그인 화면으로 보내지 않는다. 로그인을 다시 하면 서버 세션이 하나 더 생긴다.
    Unreachable(String),
}

/// `/me` 조회 결과로 다음 상태를 정한다.
///
/// 로그아웃은 서버가 401로 답했을 때만이다. 이미 로그인된 상태에서 다시 읽다가 실패하면
/// (네트워크 오류·5xx) 지금 상태를 그대로 둔다.
pub fn next_state(current: &SessionState, result: ApiResult<Me>) -> SessionState {
    match result {
        Ok(me) => SessionState::SignedIn(Box::new(me)),
        Err(error) if error.is_unauthenticated() => SessionState::SignedOut,
        Err(_) if matches!(current, SessionState::SignedIn(_)) => current.clone(),
        Err(error) => SessionState::Unreachable(error.message()),
    }
}

/// API 오류가 로그인 상태에 주는 영향.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorEffect {
    /// 세션이 끝났다. 로그인 화면으로 보낸다.
    SignOut,
    /// 권한이 바뀌었을 수 있다(조직장 임명·관리자 해제). 내 정보를 다시 읽는다.
    Recheck,
    None,
}

pub fn error_effect(error: &ApiError) -> ErrorEffect {
    if error.is_unauthenticated() {
        ErrorEffect::SignOut
    } else if error.status == 403 && error.code == "forbidden" {
        ErrorEffect::Recheck
    } else {
        ErrorEffect::None
    }
}

/// 403을 받고 내 정보를 다시 읽는 최소 간격. 403이 이어져도 이 간격에 한 번만 읽는다.
pub const RECHECK_INTERVAL_MS: i64 = 30_000;
/// 다른 앱에 갔다가 돌아왔을 때 내 정보를 다시 읽는 기준(마지막으로 읽은 뒤 지난 시간).
pub const VISIBLE_REFRESH_MS: i64 = 5 * 60_000;

/// 마지막으로 읽기 시작한 시각(`last`, ms)에서 `interval`이 지났는지. 읽은 적이 없으면 `true`.
/// 시계가 뒤로 갔으면(`now < last`) 간격을 알 수 없으므로 지난 것으로 본다.
pub fn refresh_due(last: Option<i64>, now: i64, interval: i64) -> bool {
    match last {
        None => true,
        Some(last) => now < last || now.saturating_sub(last) >= interval,
    }
}

/// 현재 로그인 상태. 앱 전체에서 `use_session()`으로 읽는다.
#[derive(Clone, Copy)]
pub struct Session {
    state: Signal<SessionState>,
    /// 마지막으로 `/me`를 읽기 시작한 시각(브라우저 시계, ms).
    last_refresh: Signal<Option<i64>>,
}

impl Session {
    pub fn state(&self) -> SessionState {
        self.state.read().clone()
    }

    pub fn me(&self) -> Option<Me> {
        match &*self.state.read() {
            SessionState::SignedIn(me) => Some((**me).clone()),
            _ => None,
        }
    }

    /// 회사 시간대. 로그인 전에는 화면이 그려지지 않으므로 UTC는 실제로 쓰이지 않는다.
    pub fn zone(&self) -> chrono_tz::Tz {
        crate::format::member_zone(self.me().as_ref())
    }

    /// 로그인·로그아웃 직후. 서버가 이미 답했으므로 그대로 반영한다.
    pub fn set(&mut self, me: Option<Me>) {
        self.state.set(match me {
            Some(me) => SessionState::SignedIn(Box::new(me)),
            None => SessionState::SignedOut,
        });
    }

    /// 서버에서 내 정보를 다시 읽는다.
    pub async fn refresh(&mut self) {
        self.last_refresh.set(Some(now_ms()));
        let result = api::get::<Me>("/me").await;
        let current = self.state.peek().clone();
        let next = next_state(&current, result);
        if current != next {
            self.state.set(next);
        }
    }

    /// 서버에 닿지 못했을 때 "다시 시도". 읽는 동안 불러오는 중으로 보인다.
    pub fn retry(mut self) {
        self.state.set(SessionState::Loading);
        spawn(async move { self.refresh().await });
    }

    /// API 오류를 로그인 상태에 반영한다. [`crate::api`]가 모든 실패 응답마다 부른다
    /// (요청 future 안이므로 렌더 중이 아니다).
    pub fn observe(mut self, error: &ApiError) {
        match error_effect(error) {
            ErrorEffect::SignOut => {
                if !matches!(*self.state.peek(), SessionState::SignedOut) {
                    self.state.set(SessionState::SignedOut);
                }
            }
            ErrorEffect::Recheck => {
                if refresh_due(*self.last_refresh.peek(), now_ms(), RECHECK_INTERVAL_MS) {
                    // 오류를 받은 화면이 곧 사라질 수 있으므로 앱 수명의 작업으로 읽는다.
                    dioxus::dioxus_core::spawn_forever(async move { self.refresh().await });
                }
            }
            ErrorEffect::None => {}
        }
    }

    /// 화면이 다시 보일 때. 오래 읽지 않았으면 내 정보를 다시 읽는다.
    async fn refresh_if_stale(&mut self) {
        if refresh_due(*self.last_refresh.peek(), now_ms(), VISIBLE_REFRESH_MS) {
            self.refresh().await;
        }
    }
}

/// 브라우저 시계(ms).
fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

/// 문서의 보임 상태가 바뀔 때까지 기다린다. 바뀐 뒤 보이는 상태면 `true`.
/// 문서가 없는 환경(네이티브)에서는 끝나지 않는다.
async fn visibility_change() -> bool {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return std::future::pending().await;
    };
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let options = web_sys::AddEventListenerOptions::new();
        options.set_once(true);
        // 등록에 실패하면 이 기다림은 끝나지 않는다(다시 읽기만 빠진다).
        let _ = document.add_event_listener_with_callback_and_add_event_listener_options(
            "visibilitychange",
            &resolve,
            &options,
        );
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
    document.visibility_state() == web_sys::VisibilityState::Visible
}

/// 로그아웃 요청 결과를 화면에 어떻게 반영할지.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogoutOutcome {
    /// 서버 세션이 끝났다(이미 만료된 경우 포함). 화면도 로그아웃한다.
    SignedOut,
    /// 서버 세션이 남아 있을 수 있다. 화면은 로그인 상태를 유지하고 이 문구로 알린다.
    StillSignedIn(String),
}

/// 실패한 로그아웃을 "로그아웃됨"으로 보여 주면 안 된다. 세션 쿠키는 HttpOnly라 화면이 지울 수
/// 없으므로, 서버가 끊지 못했으면 공용 PC에서 다음 사람이 그대로 들어올 수 있다.
pub fn logout_outcome(result: &ApiResult<Empty>) -> LogoutOutcome {
    match result {
        Ok(_) => LogoutOutcome::SignedOut,
        Err(error) if error.is_unauthenticated() => LogoutOutcome::SignedOut,
        Err(error) => LogoutOutcome::StillSignedIn(format!(
            "로그아웃하지 못했습니다. 아직 로그인된 상태이니 다시 시도해 주세요. ({})",
            error.message()
        )),
    }
}

/// 로그아웃. 사이드바·내 정보·더보기가 함께 쓴다. 이벤트 핸들러 안에서 부른다.
pub async fn logout(mut session: Session, nav: dioxus::router::Navigator) {
    let result = api::post::<_, Empty>("/auth/logout", &Empty {}).await;
    match logout_outcome(&result) {
        LogoutOutcome::SignedOut => {
            session.set(None);
            nav.replace(Route::Login {});
        }
        LogoutOutcome::StillSignedIn(message) => consume_context::<Toasts>().error(message),
    }
}

pub fn provide_session() -> Session {
    let mut session = use_context_provider(|| Session {
        state: Signal::new(SessionState::Loading),
        last_refresh: Signal::new(None),
    });
    use_future(move || async move {
        session.refresh().await;
        // 권한(조직장·관리자)은 다른 사람이 바꾼다. 앱으로 돌아올 때 오래됐으면 다시 읽는다.
        loop {
            if visibility_change().await {
                session.refresh_if_stale().await;
            }
        }
    });
    session
}

pub fn use_session() -> Session {
    use_context::<Session>()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    pub id: u64,
    pub kind: ToastKind,
    pub message: String,
}

/// 화면 하단에 잠깐 떴다 사라지는 알림.
#[derive(Clone, Copy)]
pub struct Toasts {
    items: Signal<Vec<Toast>>,
    next_id: Signal<u64>,
}

impl Toasts {
    const VISIBLE_MS: u32 = 3_500;

    pub fn items(&self) -> Vec<Toast> {
        self.items.read().clone()
    }

    fn push(&mut self, kind: ToastKind, message: String) {
        let id = *self.next_id.read();
        self.next_id.set(id + 1);
        self.items.write().push(Toast { id, kind, message });
        let mut items = self.items;
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(Self::VISIBLE_MS).await;
            items.write().retain(|toast| toast.id != id);
        });
    }

    pub fn success(&mut self, message: impl Into<String>) {
        self.push(ToastKind::Success, message.into());
    }

    pub fn error(&mut self, message: impl Into<String>) {
        self.push(ToastKind::Error, message.into());
    }

    pub fn dismiss(&mut self, id: u64) {
        self.items.write().retain(|toast| toast.id != id);
    }
}

pub fn provide_toasts() -> Toasts {
    use_context_provider(|| Toasts {
        items: Signal::new(Vec::new()),
        next_id: Signal::new(0),
    })
}

pub fn use_toasts() -> Toasts {
    use_context::<Toasts>()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(status: u16, code: &str) -> ApiError {
        ApiError {
            status,
            code: code.into(),
            field: None,
            reason: None,
        }
    }

    fn me() -> Me {
        Me {
            id: pango_core::id::MemberId::new(),
            login_id: "me".into(),
            name: "나".into(),
            email: None,
            phone: None,
            role: pango_core::authz::Role::Member,
            department_id: None,
            department_name: None,
            position_id: None,
            position_name: None,
            capabilities: pango_core::authz::Capabilities {
                admin: false,
                manage_roles: false,
                team: false,
            },
            must_change_password: false,
            company_name: "회사".into(),
            timezone: "Asia/Seoul".into(),
        }
    }

    #[test]
    fn boot_signs_out_only_on_401() {
        let loading = SessionState::Loading;
        let signed_in = me();
        assert_eq!(
            next_state(&loading, Ok(signed_in.clone())),
            SessionState::SignedIn(Box::new(signed_in))
        );
        assert_eq!(
            next_state(&loading, Err(error(401, "unauthenticated"))),
            SessionState::SignedOut
        );
        for failure in [
            error(0, "network_error"),
            error(500, "internal"),
            error(502, "unexpected_response"),
            error(401, "invalid_credentials"),
        ] {
            assert_eq!(
                next_state(&loading, Err(failure.clone())),
                SessionState::Unreachable(failure.message()),
                "{failure:?}"
            );
        }
    }

    #[test]
    fn refresh_failure_keeps_a_signed_in_user() {
        let signed_in = SessionState::SignedIn(Box::new(me()));
        assert_eq!(
            next_state(&signed_in, Err(error(0, "network_error"))),
            signed_in
        );
        assert_eq!(
            next_state(&signed_in, Err(error(401, "unauthenticated"))),
            SessionState::SignedOut
        );
        let unreachable = SessionState::Unreachable("x".into());
        assert_eq!(
            next_state(&unreachable, Err(error(0, "network_error"))),
            SessionState::Unreachable(error(0, "network_error").message())
        );
        let mut changed = me();
        changed.capabilities.team = true;
        assert_eq!(
            next_state(&signed_in, Ok(changed.clone())),
            SessionState::SignedIn(Box::new(changed))
        );
    }

    #[test]
    fn only_401_and_403_forbidden_touch_the_session() {
        assert_eq!(
            error_effect(&error(401, "unauthenticated")),
            ErrorEffect::SignOut
        );
        assert_eq!(error_effect(&error(403, "forbidden")), ErrorEffect::Recheck);
        assert_eq!(
            error_effect(&error(403, "cross_origin_rejected")),
            ErrorEffect::None
        );
        assert_eq!(
            error_effect(&error(401, "invalid_credentials")),
            ErrorEffect::None
        );
        assert_eq!(error_effect(&error(404, "not_found")), ErrorEffect::None);
        assert_eq!(error_effect(&error(0, "network_error")), ErrorEffect::None);
    }

    #[test]
    fn refresh_is_debounced_by_interval() {
        let interval = RECHECK_INTERVAL_MS;
        assert!(refresh_due(None, 0, interval), "읽은 적 없음");
        assert!(!refresh_due(Some(1_000), 1_000, interval));
        assert!(!refresh_due(Some(1_000), 1_000 + interval - 1, interval));
        assert!(refresh_due(Some(1_000), 1_000 + interval, interval), "경계");
        assert!(refresh_due(Some(1_000), 999, interval), "시계가 뒤로 감");
        assert!(refresh_due(Some(i64::MIN), i64::MAX, interval), "넘침 없음");
        assert!(!refresh_due(
            Some(0),
            VISIBLE_REFRESH_MS - 1,
            VISIBLE_REFRESH_MS
        ));
        assert!(refresh_due(Some(0), VISIBLE_REFRESH_MS, VISIBLE_REFRESH_MS));
    }

    #[test]
    fn logout_signs_out_only_when_the_server_session_is_gone() {
        assert_eq!(logout_outcome(&Ok(Empty {})), LogoutOutcome::SignedOut);
        assert_eq!(
            logout_outcome(&Err(error(401, "unauthenticated"))),
            LogoutOutcome::SignedOut,
            "이미 만료된 세션"
        );
        for failure in [
            error(0, "network_error"),
            error(500, "internal"),
            error(403, "cross_origin_rejected"),
            error(401, "something_else"),
        ] {
            match logout_outcome(&Err(failure.clone())) {
                LogoutOutcome::StillSignedIn(message) => {
                    assert!(message.contains("아직 로그인된 상태"), "{message}");
                    assert!(message.contains(&failure.message()), "{message}");
                }
                LogoutOutcome::SignedOut => panic!("{failure:?}"),
            }
        }
    }
}
