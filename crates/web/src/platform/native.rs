//! 앱(iOS·Android) 구현. 화면은 시스템 WebView에 그려지고 이 코드는 네이티브로 돈다.
//!
//! - DOM: `dioxus::document::eval`로 짧은 스크립트를 보낸다. 값은 [`super::js_string`]으로만 넣는다.
//! - HTTP: reqwest가 설정된 서버 주소로 보내고 `Authorization: Bearer`를 붙인다.
//! - 저장: 앱 전용 폴더의 JSON 파일. 로그인 토큰은 iOS에서는 키체인, Android에서는 앱 전용
//!   폴더(다른 앱이 읽을 수 없는 샌드박스)에 둔다.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

use super::{js_string, NetworkError, RawResponse, Request};

/// 앱은 토큰을 응답 본문으로 받아 `Authorization: Bearer`로 보낸다.
pub const BEARER: bool = true;

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

pub async fn sleep_ms(ms: u32) {
    tokio::time::sleep(Duration::from_millis(ms.into())).await;
}

/// 스크립트를 보내고 결과는 기다리지 않는다(초점·스크롤처럼 실패해도 기능만 빠지는 일).
fn run(script: String) {
    let _ = dioxus::document::eval(&script);
}

/// 스크립트가 `dioxus.send`로 보내는 값 하나를 기다린다. 스크립트를 못 돌리면 끝나지 않는다
/// (부르는 쪽이 반복문이라 바로 실패하면 헛돌기 때문이다).
async fn receive<T: serde::de::DeserializeOwned>(script: &str) -> T {
    let mut eval = dioxus::document::eval(script);
    match eval.recv::<T>().await {
        Ok(value) => value,
        Err(_) => std::future::pending().await,
    }
}

pub async fn visibility_change() -> bool {
    receive(
        "document.addEventListener('visibilitychange', \
         () => dioxus.send(document.visibilityState === 'visible'), { once: true });",
    )
    .await
}

/// 마지막으로 잰 WebView 폭(`f64` 비트). 재기 전에는 0이라 휴대폰 배치로 본다.
static WIDTH: AtomicU64 = AtomicU64::new(0);

pub fn inner_width() -> Option<f64> {
    Some(f64::from_bits(WIDTH.load(Ordering::Relaxed)))
}

/// 창 크기가 바뀔 때까지 기다린다. 페이지에서 처음 부르면 지금 폭을 바로 잰다.
pub async fn resized() {
    let width: f64 = receive(
        "if (!window.__pangoWidth) { window.__pangoWidth = true; dioxus.send(window.innerWidth); } \
         else { window.addEventListener('resize', () => dioxus.send(window.innerWidth), { once: true }); }",
    )
    .await;
    WIDTH.store(width.to_bits(), Ordering::Relaxed);
}

pub fn scroll_into_view(id: &str, top: bool) {
    run(format!(
        "document.getElementById({})?.scrollIntoView({top});",
        js_string(id)
    ));
}

pub fn set_scroll_top(id: &str, top: f64) {
    run(format!(
        "{{ const el = document.getElementById({}); if (el) el.scrollTop = {}; }}",
        js_string(id),
        top.max(0.0).round()
    ));
}

pub fn focus_element(id: &str) {
    run(format!(
        "{{ const el = document.getElementById({}); if (el) {{ el.scrollIntoView(); \
           const t = ['INPUT', 'TEXTAREA', 'SELECT'].includes(el.tagName) ? el \
             : el.querySelector('input, textarea, select, button'); \
           t?.focus(); }} }}",
        js_string(id)
    ));
}

/// 대화 상자의 초점 처리. 앱에서는 Tab 키를 거의 쓰지 않으므로 여는 순간의 초점만 옮긴다.
pub mod dialog {
    /// 앱은 닫을 때 초점을 돌려주지 않으므로 담는 값이 없다.
    #[derive(Clone, Copy)]
    pub struct Opener;

    pub fn opener() -> Opener {
        Opener
    }

    pub fn focus(id: &str) {
        super::run(format!(
            "document.getElementById({})?.focus();",
            super::js_string(id)
        ));
    }

    pub fn restore(_opener: &Opener) {}

    pub fn trap_tab(_id: &str, _backward: bool) -> bool {
        false
    }
}

// ── 저장소 ─────────────────────────────────────────────

/// 앱 전용 폴더.
fn data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "ios")]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Application Support/pango"))
    }
    #[cfg(target_os = "android")]
    {
        // 앱 프로세스 이름이 패키지 이름이다(`com.example.app` 또는 `com.example.app:service`).
        let cmdline = std::fs::read("/proc/self/cmdline").ok()?;
        let name = cmdline.split(|b| *b == 0).next()?;
        let name = std::str::from_utf8(name).ok()?;
        let package = name.split(':').next()?;
        if package.is_empty()
            || !package
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
        {
            return None;
        }
        Some(PathBuf::from(format!("/data/data/{package}/files")))
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        // 데스크톱에서 앱 화면을 띄워 볼 때(개발용).
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".pango-app-dev"))
    }
}

const STORE_FILE: &str = "settings.json";

/// 메모리에 올린 저장소. 처음 쓸 때 파일에서 읽는다.
fn store() -> &'static Mutex<BTreeMap<String, String>> {
    static STORE: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    STORE.get_or_init(|| {
        let map = data_dir()
            .and_then(|dir| std::fs::read(dir.join(STORE_FILE)).ok())
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        Mutex::new(map)
    })
}

/// 저장소 전체를 파일로 쓴다. 임시 파일에 쓴 뒤 바꿔 넣어 반쯤 쓴 파일이 남지 않게 한다.
fn persist(map: &BTreeMap<String, String>) {
    let Some(dir) = data_dir() else {
        return;
    };
    let Ok(raw) = serde_json::to_vec(map) else {
        return;
    };
    let temp = dir.join(format!("{STORE_FILE}.tmp"));
    let written = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::write(&temp, raw))
        .and_then(|()| std::fs::rename(&temp, dir.join(STORE_FILE)));
    if let Err(error) = written {
        // 저장에 실패해도 이번 실행에서는 메모리 값으로 계속 쓴다. 다음 실행에서 다시 입력한다.
        dioxus::logger::tracing::warn!("앱 설정을 저장하지 못했어요: {error}");
    }
}

pub fn local_get(key: &str) -> Option<String> {
    store().lock().ok()?.get(key).cloned()
}

pub fn local_set(key: &str, value: &str) {
    write_store(key, Some(value));
}

fn write_store(key: &str, value: Option<&str>) {
    let Ok(mut map) = store().lock() else {
        return;
    };
    let changed = match value {
        Some(value) => map.insert(key.to_owned(), value.to_owned()).as_deref() != Some(value),
        None => map.remove(key).is_some(),
    };
    if changed {
        persist(&map);
    }
}

// ── 서버 주소·로그인 토큰 ──────────────────────────────

const SERVER_KEY: &str = "pango.server";

/// 설정된 서버 주소(`https://host[:port]`). [`super::server_url::normalize`]를 거친 값만 저장한다.
pub fn server_origin() -> Option<String> {
    local_get(SERVER_KEY)
}

/// 서버 주소를 바꾼다. 다른 서버의 토큰을 보내지 않도록 토큰도 지운다.
pub fn set_server_origin(origin: Option<&str>) {
    if server_origin().as_deref() != origin {
        set_token(None);
    }
    write_store(SERVER_KEY, origin);
}

/// 로그인 토큰 보관소.
mod secret {
    #[cfg(target_os = "ios")]
    fn entry() -> Option<keyring::Entry> {
        keyring::Entry::new("pango-groupware", "session").ok()
    }

    #[cfg(target_os = "ios")]
    pub fn load() -> Option<String> {
        match entry()?.get_password() {
            Ok(token) => Some(token),
            Err(keyring::Error::NoEntry) => None,
            Err(error) => {
                dioxus::logger::tracing::warn!("키체인에서 로그인 정보를 읽지 못했어요: {error}");
                None
            }
        }
    }

    #[cfg(target_os = "ios")]
    pub fn save(token: Option<&str>) {
        let Some(entry) = entry() else {
            return;
        };
        let result = match token {
            Some(token) => entry.set_password(token),
            None => match entry.delete_credential() {
                Err(keyring::Error::NoEntry) => Ok(()),
                other => other,
            },
        };
        if let Err(error) = result {
            // 저장에 실패하면 다음 실행에서 다시 로그인하게 될 뿐이다. 토큰 값은 남기지 않는다.
            dioxus::logger::tracing::warn!("키체인에 로그인 정보를 저장하지 못했어요: {error}");
        }
    }

    #[cfg(not(target_os = "ios"))]
    const KEY: &str = "pango.session";

    #[cfg(not(target_os = "ios"))]
    pub fn load() -> Option<String> {
        super::local_get(KEY)
    }

    #[cfg(not(target_os = "ios"))]
    pub fn save(token: Option<&str>) {
        super::write_store(KEY, token);
    }
}

fn token_cell() -> &'static RwLock<Option<String>> {
    static TOKEN: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    TOKEN.get_or_init(|| RwLock::new(secret::load()))
}

fn token() -> Option<String> {
    token_cell().read().ok()?.clone()
}

/// 로그인하면 토큰을 넣고, 로그아웃·세션 만료 때 지운다.
pub fn set_token(token: Option<String>) {
    secret::save(token.as_deref());
    if let Ok(mut cell) = token_cell().write() {
        *cell = token;
    }
}

// ── HTTP ───────────────────────────────────────────────

fn client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                // 다른 곳으로 넘기는 응답을 따라가 토큰을 실어 보내지 않는다.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .ok()
        })
        .as_ref()
}

pub async fn send(request: Request<'_>) -> Result<RawResponse, NetworkError> {
    let origin = server_origin().ok_or(NetworkError)?;
    let client = client().ok_or(NetworkError)?;
    let method = reqwest::Method::from_bytes(request.method.as_str().as_bytes())
        .map_err(|_| NetworkError)?;
    let mut builder = client
        .request(method, format!("{origin}{}", request.path))
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(token) = token() {
        builder = builder.bearer_auth(token);
    }
    if let Some(key) = request.idempotency_key {
        builder = builder.header("Idempotency-Key", key);
    }
    if let Some(name) = request.file_name {
        builder = builder.header("X-File-Name", name);
    }
    if let Some(body) = request.body {
        builder = builder
            .header(reqwest::header::CONTENT_TYPE, body.content_type)
            .body(body.bytes);
    }
    let response = builder.send().await.map_err(|_| NetworkError)?;
    let status = response.status().as_u16();
    let body = response.bytes().await.map_err(|_| NetworkError)?.to_vec();
    Ok(RawResponse { status, body })
}
