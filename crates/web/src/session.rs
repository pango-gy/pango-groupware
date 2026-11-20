//! 로그인 상태와 전역 알림(토스트).

use dioxus::prelude::*;
use pango_api::auth::Me;
use pango_api::Empty;

use crate::api::{self, ApiError, ApiResult};
use crate::route::Route;

/// 현재 로그인한 구성원. 앱 전체에서 `use_session()`으로 읽는다.
#[derive(Clone, Copy)]
pub struct Session {
    me: Signal<Option<Me>>,
    /// 첫 `/me` 조회가 끝났는지. 끝나기 전에는 로그인 화면으로 보내지 않는다.
    loaded: Signal<bool>,
}

impl Session {
    pub fn me(&self) -> Option<Me> {
        self.me.read().clone()
    }

    /// 회사 시간대. 로그인 전에는 화면이 그려지지 않으므로 UTC는 실제로 쓰이지 않는다.
    pub fn zone(&self) -> chrono_tz::Tz {
        crate::format::member_zone(self.me.read().as_ref())
    }

    pub fn is_loaded(&self) -> bool {
        *self.loaded.read()
    }

    pub fn set(&mut self, me: Option<Me>) {
        self.me.set(me);
        self.loaded.set(true);
    }

    /// 서버에서 내 정보를 다시 읽는다.
    pub async fn refresh(&mut self) {
        match api::get::<Me>("/me").await {
            Ok(me) => self.set(Some(me)),
            // 네트워크 오류로 로그아웃시키지 않는다. 인증 실패일 때만 비운다.
            Err(error) if error.is_unauthenticated() => self.set(None),
            Err(_) => self.loaded.set(true),
        }
    }

    /// API 오류를 처리한다. 세션이 만료됐으면 로그인 화면으로 가도록 상태를 비운다.
    /// 화면에 보여 줄 문구를 돌려준다.
    pub fn handle(&mut self, error: &ApiError) -> String {
        if error.is_unauthenticated() {
            self.set(None);
        }
        error.message()
    }
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
        me: Signal::new(None),
        loaded: Signal::new(false),
    });
    use_future(move || async move { session.refresh().await });
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
