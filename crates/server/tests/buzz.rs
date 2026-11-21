//! Buzz(Nostr 릴레이) DM 채널 통합 테스트.
//!
//! 가짜 릴레이(axum, 127.0.0.1)가 실제 Buzz처럼 NIP-98 머리글과 이벤트의 ID·서명(BIP340)을
//! `k256`으로 검증하고, kind 41010(DM 열기)·9(메시지)에 Buzz와 같은 모양으로 답한다.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode as AxumStatus};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine as _;
use chrono::Duration;
use http::StatusCode;
use k256::schnorr::{Signature, VerifyingKey};
use pango_core::authz::Role;
use pango_core::id::MemberId;
use pango_core::nostr::PublicKey;
use pango_core::time::{Clock, ManualClock};
use pango_db::Conn;
use pango_server::config::{BuzzConfig, Config};
use pango_server::nostr::{sha256_hex, BotKey, Event};
use pango_server::service::outbound;
use pango_server::service::support::{notify, Notify};
use pango_server::testing::TestApp;
use serde_json::{json, Value};

const BOT_SECRET: &str = "b7e151628aed2a6abf7158809cf4f3c762e7160f38b4da56a784d9045190cfef";
const ALICE: &str = "npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg";
const ALICE_HEX: &str = "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e";
/// BIP340 벡터 0번의 공개키(다른 구성원의 Buzz ID로 쓴다).
const BOB_HEX: &str = "f9308a019258c31049344f85f89d5229b531c845836f99b08601f113bce036f9";
/// 봇 공개키(BIP340 벡터 1번).
const BOT_HEX: &str = "dff1d77f2a671c5f36183726db2341be58feae1da2deced843240f7b502ba659";

// ── 가짜 릴레이 ──────────────────────────────────────────────────────────

#[derive(Default)]
struct RelayState {
    /// 받아 들인 이벤트(검증을 통과한 것만).
    events: Vec<Event>,
    /// 다음 요청들에 줄 상태(검증보다 먼저). 다 쓰면 정상 처리.
    forced: VecDeque<u16>,
    /// 받는 사람 공개키 → DM 채널.
    dms: BTreeMap<String, String>,
    clock: Option<Arc<ManualClock>>,
}

#[derive(Clone)]
struct Relay {
    base: String,
    bot: String,
    state: Arc<Mutex<RelayState>>,
}

impl Relay {
    fn events(&self, kind: u32) -> Vec<Event> {
        let state = self.state.lock().expect("lock");
        state
            .events
            .iter()
            .filter(|e| e.kind == kind)
            .cloned()
            .collect()
    }

    fn force(&self, statuses: &[u16]) {
        self.state.lock().expect("lock").forced.extend(statuses);
    }

    fn use_clock(&self, clock: Arc<ManualClock>) {
        self.state.lock().expect("lock").clock = Some(clock);
    }

    /// 받는 사람에게 보낸 마지막 확인 코드.
    fn last_code(&self) -> String {
        let message = self.events(9).last().expect("message").content.clone();
        message
            .split(|c: char| !c.is_ascii_digit())
            .find(|part| part.len() == 6)
            .expect("code")
            .to_owned()
    }
}

fn hex_bytes(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

/// NIP-01 ID와 BIP340 서명을 직접 검증한다.
fn verify(event: &Event) -> bool {
    // 서버의 `event_id`를 쓰지 않고 NIP-01 직렬화를 따로 만든다(같은 실수를 함께 하지 않도록).
    let canonical = serde_json::to_string(&json!([
        0,
        event.pubkey,
        event.created_at,
        event.kind,
        event.tags,
        event.content
    ]))
    .expect("json");
    let id: [u8; 32] = {
        use sha2::Digest as _;
        sha2::Sha256::digest(canonical.as_bytes()).into()
    };
    if pango_core::nostr::to_hex(&id) != event.id {
        return false;
    }
    let (Some(pubkey), Some(sig)) = (hex_bytes(&event.pubkey), hex_bytes(&event.sig)) else {
        return false;
    };
    let (Ok(key), Ok(sig)) = (
        VerifyingKey::from_bytes(&pubkey),
        Signature::try_from(sig.as_slice()),
    ) else {
        return false;
    };
    key.verify_raw(&id, &sig).is_ok()
}

fn tag<'a>(event: &'a Event, name: &str) -> Option<&'a str> {
    event
        .tags
        .iter()
        .find(|t| t.first().map(String::as_str) == Some(name))
        .and_then(|t| t.get(1))
        .map(String::as_str)
}

fn error(status: u16, message: &str) -> Response {
    let status = AxumStatus::from_u16(status).expect("status");
    (status, axum::Json(json!({ "error": message }))).into_response()
}

async fn submit(State(relay): State<Relay>, headers: HeaderMap, body: Bytes) -> Response {
    let mut state = relay.state.lock().expect("lock");
    if let Some(status) = state.forced.pop_front() {
        return error(status, "forced");
    }
    // NIP-98: Authorization: Nostr <base64(kind 27235)>
    let Some(auth) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Nostr "))
    else {
        return error(401, "missing auth");
    };
    let Ok(json) = base64::engine::general_purpose::STANDARD.decode(auth) else {
        return error(401, "bad base64");
    };
    let Ok(auth_event) = serde_json::from_slice::<Event>(&json) else {
        return error(401, "bad auth json");
    };
    let now = state
        .clock
        .as_ref()
        .map(|clock| clock.now().as_millis() / 1000)
        .unwrap_or_default();
    let auth_ok = auth_event.kind == 27235
        && verify(&auth_event)
        && (auth_event.created_at - now).abs() <= 120
        && tag(&auth_event, "u") == Some(format!("{}/events", relay.base).as_str())
        && tag(&auth_event, "method") == Some("POST")
        && tag(&auth_event, "payload") == Some(sha256_hex(&body).as_str());
    if !auth_ok {
        return error(401, "invalid NIP-98");
    }
    if auth_event.pubkey != relay.bot {
        return error(403, "not a relay member");
    }
    let Ok(event) = serde_json::from_slice::<Event>(&body) else {
        return error(400, "invalid event JSON");
    };
    if !verify(&event) || event.pubkey != auth_event.pubkey {
        return error(400, "invalid: bad signature or author");
    }
    let response = match event.kind {
        41010 => {
            let Some(other) = tag(&event, "p")
                .filter(|p| *p != event.pubkey)
                .map(str::to_owned)
            else {
                return error(
                    400,
                    "invalid: pubkeys must contain at least 1 other participant",
                );
            };
            let created = !state.dms.contains_key(&other);
            let next = format!("00000000-0000-4000-8000-{:012}", state.dms.len() + 1);
            let channel = state.dms.entry(other).or_insert(next).clone();
            json!({
                "event_id": event.id, "accepted": true,
                "message": format!("response:{}", json!({ "channel_id": channel, "created": created })),
            })
        }
        9 => {
            let channel = tag(&event, "h").unwrap_or_default().to_owned();
            if !state.dms.values().any(|known| *known == channel) {
                return error(400, "invalid: unknown channel");
            }
            let ms_ok = tag(&event, "ms")
                .and_then(|ms| ms.parse::<u32>().ok())
                .is_some_and(|ms| ms < 1000);
            if !ms_ok {
                return error(400, "invalid: ms tag");
            }
            json!({ "event_id": event.id, "accepted": true, "message": "" })
        }
        _ => return error(400, "unsupported kind"),
    };
    state.events.push(event);
    axum::Json(response).into_response()
}

async fn relay() -> Relay {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let relay = Relay {
        base: format!("http://127.0.0.1:{port}"),
        bot: BotKey::parse(BOT_SECRET)
            .expect("key")
            .public_key()
            .to_hex(),
        state: Arc::default(),
    };
    let app = axum::Router::new()
        .route("/events", post(submit))
        .with_state(relay.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    relay
}

fn buzz_config(relay: &Relay) -> Config {
    Config {
        buzz: BuzzConfig::from_settings(Some(&relay.base), Some(BOT_SECRET), None).expect("config"),
        // 가짜 릴레이가 이 컴퓨터(루프백)에 있으므로 연다.
        outbound_allow_private: true,
        ..Config::for_tests()
    }
}

/// Buzz 채널이 켜진 서버와 가짜 릴레이.
async fn world() -> (TestApp, Relay) {
    let relay = relay().await;
    let app = TestApp::with_config(buzz_config(&relay)).await;
    relay.use_clock(app.clock.clone());
    app.enable_buzz().await;
    (app, relay)
}

async fn start(app: &TestApp, token: &str, pubkey: &str) -> pango_server::testing::TestResponse {
    app.post("/me/buzz")
        .bearer(token)
        .json(&json!({ "pubkey": pubkey }))
        .send()
        .await
}

async fn verify_code(
    app: &TestApp,
    token: &str,
    code: &str,
) -> pango_server::testing::TestResponse {
    app.post("/me/buzz/verify")
        .bearer(token)
        .json(&json!({ "code": code }))
        .send()
        .await
}

async fn my_buzz(app: &TestApp, token: &str) -> Value {
    app.get("/me/buzz")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

/// 연결을 끝까지 마친다.
async fn link(app: &TestApp, relay: &Relay, token: &str, pubkey: &str) {
    start(app, token, pubkey).await.expect(StatusCode::OK);
    verify_code(app, token, &relay.last_code())
        .await
        .expect(StatusCode::OK);
}

async fn send_notification(app: &TestApp, to: MemberId) {
    let now = app.state.now();
    app.state
        .write(async |conn: &mut Conn| {
            notify(
                conn,
                now,
                Notify {
                    to,
                    kind: "approval.todo",
                    title: "결재 요청: 출장비 정산".into(),
                    body: "김팡고 님이 결재를 요청했어요.".into(),
                    link: Some("/approval/documents/abc".into()),
                    dedupe_key: None,
                },
            )
            .await
        })
        .await
        .expect("notify");
}

async fn dm_channel_of(app: &TestApp, member: MemberId) -> Option<String> {
    let mut conn = app.state.read().await.expect("read");
    sqlx::query_scalar("SELECT dm_channel_id FROM buzz_accounts WHERE member_id = ?")
        .bind(member.to_string())
        .fetch_one(&mut *conn)
        .await
        .expect("row")
}

// ── 연결 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_member_links_their_buzz_id_with_a_code_sent_by_dm() {
    let (app, relay) = world().await;
    let member = app.add_member("kim", Role::Member, None).await;

    let pending = start(&app, &member.token, ALICE)
        .await
        .expect(StatusCode::OK);
    assert_eq!(pending["status"], "pending");
    assert_eq!(pending["npub"], ALICE);
    assert_eq!(pending["attempts_left"], 5);
    assert_eq!(
        pending["code_expires_at"],
        app.state.now().as_millis() + 600_000
    );
    assert_eq!(
        pending["bot_npub"],
        BotKey::parse(BOT_SECRET)
            .expect("key")
            .public_key()
            .to_npub()
    );

    // 봇이 DM을 열고(받는 사람 = 입력한 ID) 코드를 보냈다.
    let opens = relay.events(41010);
    assert_eq!(opens.len(), 1);
    assert_eq!(tag(&opens[0], "p"), Some(ALICE_HEX));
    assert_eq!(opens[0].content, "");
    let messages = relay.events(9);
    assert_eq!(messages.len(), 1);
    assert_eq!(
        tag(&messages[0], "h"),
        Some("00000000-0000-4000-8000-000000000001")
    );
    let code = relay.last_code();
    assert!(messages[0].content.contains("확인 코드"));

    let verified = verify_code(
        &app,
        &member.token,
        &format!("{} {}", &code[..3], &code[3..]),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(verified["status"], "verified");
    assert_eq!(verified["attempts_left"], Value::Null);
    assert_eq!(
        dm_channel_of(&app, member.id).await.as_deref(),
        Some("00000000-0000-4000-8000-000000000001"),
        "DM 채널을 저장해 둔다"
    );

    // 감사 로그에는 줄인 ID만 남는다.
    let entries = app.audit_entries("buzz.link").await;
    assert_eq!(entries.len(), 1);
    let detail = entries[0].detail.clone().expect("detail").to_string();
    assert!(detail.contains("npub10elfc…jptg"), "{detail}");
    assert!(
        !detail.contains(ALICE_HEX) && !detail.contains(ALICE),
        "{detail}"
    );
}

#[tokio::test]
async fn wrong_codes_are_counted_and_five_misses_end_the_code() {
    let (app, relay) = world().await;
    let member = app.add_member("kim", Role::Member, None).await;
    start(&app, &member.token, ALICE)
        .await
        .expect(StatusCode::OK);
    let code = relay.last_code();
    let wrong = if code == "000000" { "111111" } else { "000000" };

    for left in (1..5).rev() {
        verify_code(&app, &member.token, wrong)
            .await
            .expect_error(StatusCode::CONFLICT, "buzz_code_wrong");
        assert_eq!(my_buzz(&app, &member.token).await["attempts_left"], left);
    }
    verify_code(&app, &member.token, wrong)
        .await
        .expect_error(StatusCode::CONFLICT, "buzz_code_wrong");
    // 다섯 번 틀리면 맞는 코드도 받지 않는다.
    verify_code(&app, &member.token, &code)
        .await
        .expect_error(StatusCode::CONFLICT, "buzz_code_exhausted");
    assert_eq!(my_buzz(&app, &member.token).await["status"], "pending");
    // 형식이 틀린 입력은 횟수를 쓰지 않는다.
    verify_code(&app, &member.token, "12ab56")
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
}

#[tokio::test]
async fn codes_expire_after_ten_minutes() {
    let (app, relay) = world().await;
    let early = app.add_member("kim", Role::Member, None).await;
    let late = app.add_member("lee", Role::Member, None).await;
    start(&app, &early.token, ALICE)
        .await
        .expect(StatusCode::OK);
    let early_code = relay.last_code();
    start(&app, &late.token, BOB_HEX)
        .await
        .expect(StatusCode::OK);
    let late_code = relay.last_code();

    // 경계: 만료 1초 전은 받고, 만료 시각 정각은 받지 않는다.
    app.advance(Duration::seconds(599));
    verify_code(&app, &early.token, &early_code)
        .await
        .expect(StatusCode::OK);
    app.advance(Duration::seconds(1));
    verify_code(&app, &late.token, &late_code)
        .await
        .expect_error(StatusCode::CONFLICT, "buzz_code_expired");
}

#[tokio::test]
async fn codes_can_be_resent_only_after_a_minute_and_five_times_an_hour() {
    let (app, relay) = world().await;
    let member = app.add_member("kim", Role::Member, None).await;
    start(&app, &member.token, ALICE)
        .await
        .expect(StatusCode::OK);
    let first = relay.last_code();

    let limited = start(&app, &member.token, ALICE).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.headers["retry-after"], "60");
    app.advance(Duration::seconds(59));
    assert_eq!(
        start(&app, &member.token, ALICE).await.status,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(relay.events(9).len(), 1, "막힌 요청은 보내지 않는다");

    app.advance(Duration::seconds(1));
    start(&app, &member.token, ALICE)
        .await
        .expect(StatusCode::OK);
    let second = relay.last_code();
    assert_eq!(
        relay.events(41010).len(),
        1,
        "같은 계정이면 저장한 DM을 다시 쓴다"
    );
    if first != second {
        verify_code(&app, &member.token, &first)
            .await
            .expect_error(StatusCode::CONFLICT, "buzz_code_wrong");
    }

    for _ in 0..3 {
        app.advance(Duration::seconds(60));
        start(&app, &member.token, ALICE)
            .await
            .expect(StatusCode::OK);
    }
    app.advance(Duration::seconds(60));
    let capped = start(&app, &member.token, ALICE).await;
    assert_eq!(
        capped.status,
        StatusCode::TOO_MANY_REQUESTS,
        "시간당 5번까지"
    );
    assert_eq!(relay.events(9).len(), 5);
}

#[tokio::test]
async fn one_buzz_account_belongs_to_one_member() {
    let (app, relay) = world().await;
    let kim = app.add_member("kim", Role::Member, None).await;
    let lee = app.add_member("lee", Role::Member, None).await;
    let park = app.add_member("park", Role::Member, None).await;
    // 둘이 같은 ID로 코드를 받아 두고, 김이 먼저 확인한다.
    start(&app, &lee.token, ALICE).await.expect(StatusCode::OK);
    let lee_code = relay.last_code();
    link(&app, &relay, &kim.token, ALICE).await;

    verify_code(&app, &lee.token, &lee_code)
        .await
        .expect_error(StatusCode::CONFLICT, "buzz_account_taken");
    start(&app, &park.token, ALICE_HEX)
        .await
        .expect_error(StatusCode::CONFLICT, "buzz_account_taken");
    assert_eq!(my_buzz(&app, &kim.token).await["status"], "verified");
}

#[tokio::test]
async fn a_code_only_works_for_the_member_who_asked_for_it() {
    let (app, relay) = world().await;
    let kim = app.add_member("kim", Role::Member, None).await;
    let lee = app.add_member("lee", Role::Member, None).await;
    start(&app, &kim.token, ALICE).await.expect(StatusCode::OK);
    let kims_code = relay.last_code();

    verify_code(&app, &lee.token, &kims_code)
        .await
        .expect_error(StatusCode::CONFLICT, "buzz_code_missing");
    start(&app, &lee.token, BOB_HEX)
        .await
        .expect(StatusCode::OK);
    let lees_code = relay.last_code();
    if lees_code != kims_code {
        verify_code(&app, &lee.token, &kims_code)
            .await
            .expect_error(StatusCode::CONFLICT, "buzz_code_wrong");
    }
    assert_eq!(my_buzz(&app, &kim.token).await["status"], "pending");
}

#[tokio::test]
async fn malformed_ids_and_an_unavailable_channel_are_rejected() {
    let (app, relay) = world().await;
    let member = app.add_member("kim", Role::Member, None).await;
    for bad in [
        "",
        "nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5",
        "npub1",
        "<script>",
    ] {
        start(&app, &member.token, bad)
            .await
            .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    app.execute("UPDATE buzz_settings SET enabled = 0").await;
    start(&app, &member.token, ALICE)
        .await
        .expect_error(StatusCode::CONFLICT, "buzz_unavailable");
    assert_eq!(my_buzz(&app, &member.token).await["available"], false);
    assert!(relay.events(41010).is_empty());
}

#[tokio::test]
async fn delivery_problems_while_linking_are_explained() {
    let (app, relay) = world().await;
    let member = app.add_member("kim", Role::Member, None).await;
    // 봇이 릴레이 구성원이 아니거나 서명이 틀림.
    relay.force(&[403]);
    start(&app, &member.token, ALICE)
        .await
        .expect_error(StatusCode::BAD_GATEWAY, "buzz_bot_rejected");
    app.advance(Duration::seconds(60));
    relay.force(&[503]);
    start(&app, &member.token, ALICE)
        .await
        .expect_error(StatusCode::BAD_GATEWAY, "buzz_unreachable");

    // 릴레이가 꺼져 있음.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let down = TestApp::with_config(Config {
        buzz: BuzzConfig::from_settings(
            Some(&format!("http://127.0.0.1:{closed}")),
            Some(BOT_SECRET),
            None,
        )
        .expect("config"),
        outbound_allow_private: true,
        ..Config::for_tests()
    })
    .await;
    down.enable_buzz().await;
    let someone = down.add_member("lee", Role::Member, None).await;
    start(&down, &someone.token, ALICE)
        .await
        .expect_error(StatusCode::BAD_GATEWAY, "buzz_unreachable");
}

#[tokio::test]
async fn a_loopback_relay_needs_the_private_address_option() {
    let relay = relay().await;
    let app = TestApp::with_config(Config {
        outbound_allow_private: false,
        ..buzz_config(&relay)
    })
    .await;
    relay.use_clock(app.clock.clone());
    app.enable_buzz().await;
    let member = app.add_member("kim", Role::Member, None).await;

    start(&app, &member.token, ALICE)
        .await
        .expect_error(StatusCode::BAD_GATEWAY, "buzz_unreachable");
    assert!(relay.events(41010).is_empty(), "연결하지 않았다");
}

// ── 발송 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn notifications_reach_linked_members_through_the_saved_dm() {
    let (app, relay) = world().await;
    let member = app.add_member("kim", Role::Member, None).await;
    let unlinked = app.add_member("lee", Role::Member, None).await;
    link(&app, &relay, &member.token, ALICE).await;

    send_notification(&app, member.id).await;
    send_notification(&app, unlinked.id).await;
    assert_eq!(
        app.outbox_by_channel().await,
        [("buzz".to_owned(), "pending".to_owned(), None)],
        "연결한 사람에게만, 웹훅은 꺼져 있다"
    );
    let report = outbound::dispatch_once(&app.state).await.expect("dispatch");
    assert_eq!(report.sent, 1);

    let messages = relay.events(9);
    let delivered = messages.last().expect("message");
    assert_eq!(
        delivered.content,
        "결재 요청: 출장비 정산\n김팡고 님이 결재를 요청했어요.\n/approval/documents/abc"
    );
    assert_eq!(
        tag(delivered, "h"),
        Some("00000000-0000-4000-8000-000000000001")
    );
    assert_eq!(relay.events(41010).len(), 1, "연결할 때 연 DM을 다시 쓴다");
    assert_eq!(
        app.outbox_by_channel().await,
        [("buzz".to_owned(), "sent".to_owned(), None)]
    );
}

#[tokio::test]
async fn webhook_and_buzz_are_queued_independently() {
    let (app, relay) = world().await;
    let member = app.add_member("kim", Role::Member, None).await;
    link(&app, &relay, &member.token, ALICE).await;
    app.enable_webhook("https://hooks.example.com/in").await;

    send_notification(&app, member.id).await;

    let mut channels: Vec<_> = app
        .outbox_by_channel()
        .await
        .into_iter()
        .map(|row| row.0)
        .collect();
    channels.sort();
    assert_eq!(channels, ["buzz", "webhook"]);

    // 외부 알림을 끄면 둘 다 남지 않는다.
    app.put("/me/notification-preferences")
        .bearer(&member.token)
        .json(&json!({ "external_enabled": false }))
        .send()
        .await
        .expect(StatusCode::OK);
    send_notification(&app, member.id).await;
    assert_eq!(app.outbox_by_channel().await.len(), 2);
}

#[tokio::test]
async fn unlinking_stops_messages_already_queued() {
    let (app, relay) = world().await;
    let member = app.add_member("kim", Role::Member, None).await;
    link(&app, &relay, &member.token, ALICE).await;
    send_notification(&app, member.id).await;

    let after = app
        .delete("/me/buzz")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(after["status"], "none");
    outbound::dispatch_once(&app.state).await.expect("dispatch");

    assert_eq!(relay.events(9).len(), 1, "확인 코드 말고는 보내지 않았다");
    assert_eq!(
        app.outbox_by_channel().await,
        [(
            "buzz".to_owned(),
            "failed".to_owned(),
            Some("recipient_unlinked".to_owned())
        )]
    );
    send_notification(&app, member.id).await;
    assert_eq!(
        app.outbox_by_channel().await.len(),
        1,
        "새 알림은 남지 않는다"
    );
    assert_eq!(app.audit_entries("buzz.unlink").await.len(), 1);
}

#[tokio::test]
async fn unauthorized_fails_at_once_and_rate_limits_are_retried() {
    let (app, relay) = world().await;
    let member = app.add_member("kim", Role::Member, None).await;
    link(&app, &relay, &member.token, ALICE).await;
    send_notification(&app, member.id).await;

    relay.force(&[429]);
    assert_eq!(
        outbound::dispatch_once(&app.state)
            .await
            .expect("dispatch")
            .retrying,
        1
    );
    assert_eq!(
        app.outbox_by_channel().await,
        [(
            "buzz".to_owned(),
            "pending".to_owned(),
            Some("http_429".to_owned())
        )]
    );

    app.advance(Duration::seconds(30));
    relay.force(&[401]);
    assert_eq!(
        outbound::dispatch_once(&app.state)
            .await
            .expect("dispatch")
            .failed,
        1
    );
    assert_eq!(
        app.outbox_by_channel().await,
        [(
            "buzz".to_owned(),
            "failed".to_owned(),
            Some("http_401".to_owned())
        )]
    );
    app.advance(Duration::hours(2));
    outbound::dispatch_once(&app.state).await.expect("dispatch");
    assert_eq!(relay.events(9).len(), 1, "다시 보내지 않는다");
}

#[tokio::test]
async fn a_stale_saved_dm_is_reopened_once() {
    let (app, relay) = world().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.link_buzz(member.id, ALICE_HEX).await;
    app.execute("UPDATE buzz_accounts SET dm_channel_id = '00000000-0000-4000-8000-0000000000ff'")
        .await;
    send_notification(&app, member.id).await;

    assert_eq!(
        outbound::dispatch_once(&app.state)
            .await
            .expect("dispatch")
            .sent,
        1
    );

    assert_eq!(relay.events(41010).len(), 1);
    assert_eq!(
        dm_channel_of(&app, member.id).await.as_deref(),
        Some("00000000-0000-4000-8000-000000000001"),
        "새로 연 DM으로 바꿔 저장한다"
    );
}

#[tokio::test]
async fn messages_fail_when_the_server_has_no_buzz_settings() {
    let app = TestApp::new().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.enable_buzz().await;
    app.link_buzz(member.id, ALICE_HEX).await;
    send_notification(&app, member.id).await;

    outbound::dispatch_once(&app.state).await.expect("dispatch");

    assert_eq!(
        app.outbox_by_channel().await,
        [(
            "buzz".to_owned(),
            "failed".to_owned(),
            Some("integration_disabled".to_owned())
        )]
    );
}

// ── 관리자 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn admins_see_the_status_and_switch_the_channel() {
    let (app, relay) = world().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let member = app.add_member("kim", Role::Member, None).await;
    link(&app, &relay, &member.token, ALICE).await;

    let status = app
        .get("/admin/integrations/buzz")
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(status["configured"], true);
    assert_eq!(status["enabled"], true);
    assert_eq!(status["relay_url"], relay.base);
    assert_eq!(status["linked_members"], 1);
    let bot = BotKey::parse(BOT_SECRET).expect("key").public_key();
    assert_eq!(status["bot_npub"], bot.to_npub());
    let version = status["version"].as_i64().expect("version");

    let off = app
        .put("/admin/integrations/buzz")
        .bearer(&admin.token)
        .json(&json!({ "version": version, "enabled": false }))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(off["enabled"], false);
    app.put("/admin/integrations/buzz")
        .bearer(&admin.token)
        .json(&json!({ "version": version, "enabled": true }))
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    assert_eq!(app.audit_entries("integration.buzz_update").await.len(), 1);

    // 연결 안 된 사람에게는 꺼진 뒤 아무것도 남지 않는다.
    send_notification(&app, member.id).await;
    assert!(app.outbox_by_channel().await.is_empty());
}

#[tokio::test]
async fn the_channel_cannot_be_enabled_without_server_settings() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let status = app
        .get("/admin/integrations/buzz")
        .bearer(&owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        (
            status["configured"].clone(),
            status["relay_url"].clone(),
            status["bot_npub"].clone()
        ),
        (json!(false), Value::Null, Value::Null)
    );
    app.put("/admin/integrations/buzz")
        .bearer(&owner)
        .json(&json!({ "version": 1, "enabled": true }))
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "buzz_unavailable");
}

#[tokio::test]
async fn members_cannot_use_the_admin_buzz_endpoints() {
    let (app, relay) = world().await;
    let member = app.add_member("kim", Role::Member, None).await;
    let other = app.add_member("lee", Role::Member, None).await;
    link(&app, &relay, &other.token, ALICE).await;
    let path = format!("/admin/members/{}/buzz", other.id);
    for request in [
        app.get("/admin/integrations/buzz").bearer(&member.token),
        app.put("/admin/integrations/buzz")
            .bearer(&member.token)
            .json(&json!({ "version": 1, "enabled": false })),
        app.get(&path).bearer(&member.token),
        app.delete(&path).bearer(&member.token),
    ] {
        request
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    assert_eq!(my_buzz(&app, &other.token).await["status"], "verified");
}

#[tokio::test]
async fn admins_can_unlink_but_not_link_for_someone() {
    let (app, relay) = world().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let member = app.add_member("kim", Role::Member, None).await;
    link(&app, &relay, &member.token, ALICE).await;
    let path = format!("/admin/members/{}/buzz", member.id);

    let seen = app
        .get(&path)
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        seen,
        json!({ "status": "verified", "npub_short": "npub10elfc…jptg" })
    );
    let unlinked = app
        .delete(&path)
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(unlinked["status"], "none");
    assert_eq!(my_buzz(&app, &member.token).await["status"], "none");
    let entries = app.audit_entries("buzz.admin_unlink").await;
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].target_id.as_deref(),
        Some(member.id.to_string().as_str())
    );
    // 관리자 경로에는 연결(POST)이 없다.
    app.post(&path)
        .bearer(&admin.token)
        .json(&json!({ "pubkey": ALICE }))
        .send()
        .await
        .expect_error(StatusCode::METHOD_NOT_ALLOWED, "");
    app.get(&format!("/admin/members/{}/buzz", MemberId::new()))
        .bearer(&admin.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn the_bot_secret_is_never_shown() {
    let (app, relay) = world().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let member = app.add_member("kim", Role::Member, None).await;
    link(&app, &relay, &member.token, ALICE).await;
    app.post("/me/buzz/verify")
        .bearer(&member.token)
        .json(&json!({ "code": "000000" }))
        .send()
        .await;

    let nsec_prefix = &BOT_SECRET[..16];
    let mut shown = app
        .get("/admin/integrations/buzz")
        .bearer(&admin.token)
        .send()
        .await
        .body
        .to_string();
    shown += &my_buzz(&app, &member.token).await.to_string();
    shown += &format!("{:?}", app.state.config);
    for action in ["buzz.link_start", "buzz.link"] {
        for entry in app.audit_entries(action).await {
            shown += &format!("{:?}", entry.detail);
        }
    }
    assert!(!shown.contains(nsec_prefix), "{shown}");
    assert!(!shown.contains("nsec1"), "{shown}");
}

/// 테스트의 봇 공개키는 BIP340 벡터 1번이다.
#[test]
fn the_test_bot_key_is_the_bip340_vector() {
    assert_eq!(
        BotKey::parse(BOT_SECRET).expect("key").public_key(),
        PublicKey::parse(BOT_HEX).expect("pubkey")
    );
}
