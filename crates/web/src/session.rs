//! 로그인 상태와 전역 알림(토스트).

use dioxus::prelude::*;
use pango_api::auth::Me;

use crate::api::{self, ApiError};

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
