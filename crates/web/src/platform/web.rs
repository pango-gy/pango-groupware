//! 브라우저(WASM) 구현. DOM은 `web_sys`로 바로 다루고, 인증은 같은 출처 HttpOnly 쿠키다.

use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

use super::{NetworkError, RawResponse, Request};

/// 웹은 쿠키로 인증한다(스크립트가 토큰을 볼 수 없다).
pub const BEARER: bool = false;

/// 브라우저 시계(ms).
pub fn now_millis() -> i64 {
    js_sys::Date::now() as i64
}

pub async fn sleep_ms(ms: u32) {
    gloo_timers::future::TimeoutFuture::new(ms).await;
}

fn document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

fn element(id: &str) -> Option<HtmlElement> {
    document()?
        .get_element_by_id(id)?
        .dyn_into::<HtmlElement>()
        .ok()
}

/// 한 번 일어나는 이벤트를 기다린다. 등록에 실패하면 끝나지 않는다(그 기능만 빠진다).
async fn once(target: &web_sys::EventTarget, event: &str) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let options = web_sys::AddEventListenerOptions::new();
        options.set_once(true);
        let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
            event, &resolve, &options,
        );
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// 문서의 보임 상태가 바뀔 때까지 기다린다. 바뀐 뒤 보이는 상태면 `true`.
pub async fn visibility_change() -> bool {
    let Some(document) = document() else {
        return std::future::pending().await;
    };
    once(&document, "visibilitychange").await;
    document.visibility_state() == web_sys::VisibilityState::Visible
}

/// 창 안쪽 폭(px).
pub fn inner_width() -> Option<f64> {
    web_sys::window()?.inner_width().ok()?.as_f64()
}

/// 창 크기가 한 번 바뀔 때까지 기다린다.
pub async fn resized() {
    let Some(window) = web_sys::window() else {
        return std::future::pending().await;
    };
    once(&window, "resize").await;
}

/// `id` 요소가 보이게 굴린다. `top`이면 요소 위쪽을 맞춘다.
pub fn scroll_into_view(id: &str, top: bool) {
    if let Some(element) = element(id) {
        element.scroll_into_view_with_bool(top);
    }
}

/// 스크롤되는 영역 `id`를 `top`(px)으로 옮긴다.
pub fn set_scroll_top(id: &str, top: f64) {
    if let Some(element) = element(id) {
        element.set_scroll_top(top as i32);
    }
}

/// `id` 요소로 화면을 옮기고, 그 요소(또는 그 안의 첫 입력란)에 초점을 준다.
pub fn focus_element(id: &str) {
    let Some(element) = element(id) else {
        return;
    };
    element.scroll_into_view();
    let target = if matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT") {
        Some(element)
    } else {
        element
            .query_selector("input, textarea, select, button")
            .ok()
            .flatten()
            .and_then(|found| found.dyn_into::<HtmlElement>().ok())
    };
    if let Some(target) = target {
        // 초점을 줄 수 없는 요소면 화면만 옮겨진 채로 둔다.
        let _ = target.focus();
    }
}

/// 이 브라우저의 `localStorage`. 막아 둔 브라우저에서는 `None`(기능만 빠진다).
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

pub fn local_get(key: &str) -> Option<String> {
    storage()?.get_item(key).ok().flatten()
}

/// 용량 초과·비공개 창 등으로 못 써도 이번 화면에서는 그대로 쓸 수 있으므로 넘어간다.
pub fn local_set(key: &str, value: &str) {
    if let Some(storage) = storage() {
        let _ = storage.set_item(key, value);
    }
}

/// 대화 상자의 초점 처리. 계산은 [`crate::ui::tab_move`]가 한다.
pub mod dialog {
    use web_sys::HtmlElement;

    use crate::ui::{tab_move, TabMove};

    /// 초점을 받을 수 있는 칸.
    const FOCUSABLE: &str =
        "a[href], button:not([disabled]), input:not([disabled]):not([type=\"hidden\"]), \
        select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex=\"-1\"])";

    /// 대화 상자를 연 요소(닫힐 때 초점을 돌려줄 곳).
    pub type Opener = Option<HtmlElement>;

    pub fn opener() -> Opener {
        use wasm_bindgen::JsCast;
        super::document()?
            .active_element()?
            .dyn_into::<HtmlElement>()
            .ok()
    }

    /// 열릴 때: 대화 상자 자체에 초점(화면 낭독기가 제목을 읽고, Tab은 첫 칸부터).
    /// 입력 칸에 바로 두지 않는 것은 모바일에서 키보드가 바로 올라오지 않게 하려는 것이다.
    pub fn focus(id: &str) {
        if let Some(dialog) = super::element(id) {
            let _ = dialog.focus();
        }
    }

    /// 닫힐 때: 연 버튼이 아직 화면에 있으면 그리로 돌려준다.
    pub fn restore(opener: &Opener) {
        if let Some(opener) = opener.as_ref().filter(|el| el.is_connected()) {
            let _ = opener.focus();
        }
    }

    /// Tab을 대화 상자 안에 가둔다. 초점을 직접 옮겼으면(기본 동작을 막아야 하면) `true`.
    pub fn trap_tab(id: &str, backward: bool) -> bool {
        use wasm_bindgen::JsCast;
        let Some(dialog) = super::element(id) else {
            return false;
        };
        let Ok(nodes) = dialog.query_selector_all(FOCUSABLE) else {
            return false;
        };
        let items: Vec<HtmlElement> = (0..nodes.length())
            .filter_map(|index| nodes.item(index))
            .filter_map(|node| node.dyn_into::<HtmlElement>().ok())
            // 숨겨진 칸(`display: none` 등)은 건너뛴다.
            .filter(|el| el.get_client_rects().length() > 0)
            .collect();
        let active = opener();
        let current = active
            .as_ref()
            .and_then(|active| items.iter().position(|el| el == active));
        match tab_move(items.len(), current, backward) {
            TabMove::Browser => false,
            TabMove::Focus(index) => {
                if let Some(target) = items.get(index) {
                    let _ = target.focus();
                }
                true
            }
            TabMove::Stay => true,
        }
    }
}

/// 같은 출처로 보낸다. 세션은 HttpOnly 쿠키가 실어 간다.
pub async fn send(request: Request<'_>) -> Result<RawResponse, NetworkError> {
    use gloo_net::http::{Method, RequestBuilder};
    let method =
        Method::from_bytes(request.method.as_str().as_bytes()).map_err(|_| NetworkError)?;
    let mut builder = RequestBuilder::new(request.path)
        .method(method)
        .credentials(web_sys::RequestCredentials::SameOrigin);
    if let Some(key) = request.idempotency_key {
        builder = builder.header("Idempotency-Key", key);
    }
    let built = match request.json {
        Some(json) => builder
            .header("Content-Type", "application/json")
            .body(json),
        None => builder.build(),
    }
    .map_err(|_| NetworkError)?;
    let response = built.send().await.map_err(|_| NetworkError)?;
    let status = response.status();
    let body = response.binary().await.map_err(|_| NetworkError)?;
    Ok(RawResponse { status, body })
}

/// 웹은 자기 출처로만 보내므로 서버 주소를 따로 두지 않는다.
pub fn server_origin() -> Option<String> {
    None
}

pub fn set_server_origin(_origin: Option<&str>) {}

/// 웹의 세션은 HttpOnly 쿠키라 스크립트가 다루지 않는다.
pub fn set_token(_token: Option<String>) {}
