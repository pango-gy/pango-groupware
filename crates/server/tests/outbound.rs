//! 외부 알림 채널(아웃박스·발송 작업·웹훅 연동 설정) 통합 테스트.

use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use chrono::Duration;
use http::StatusCode;
use pango_core::authz::Role;
use pango_core::id::MemberId;
use pango_core::member::MemberStatus;
use pango_core::outbound::MAX_ATTEMPTS;
use pango_db::Conn;
use pango_server::security::webhook_signature;
use pango_server::service::maintenance;
use pango_server::service::outbound::webhook::WebhookSender;
use pango_server::service::outbound::{
    self, Delivery, DispatchReport, OutboundPayload, OutboundSender, Recipient, SendError,
    WebhookTarget,
};
use pango_server::service::support::{notify, Notify};
use pango_server::testing::{FakeSender, TestApp, TestMember};
use serde_json::{json, Value};

const HOOK: &str = "https://hooks.example.com/pango";

async fn app_with_fake() -> (TestApp, Arc<FakeSender>) {
    let fake = FakeSender::new();
    let app = TestApp::with_outbound_sender(fake.clone()).await;
    (app, fake)
}

/// 서비스의 알림 함수를 그대로 부른다(모든 알림이 지나는 길).
async fn send_notification(app: &TestApp, to: MemberId, dedupe_key: Option<&str>) {
    let now = app.state.now();
    app.state
        .write(async |conn: &mut Conn| {
            notify(
                conn,
                now,
                Notify {
                    to,
                    kind: "approval.requested",
                    title: "결재 요청: 출장비".into(),
                    body: "김팡고 님이 결재를 요청했어요.".into(),
                    link: Some("/approval/documents/abc".into()),
                    dedupe_key,
                },
            )
            .await
        })
        .await
        .expect("notify");
}

async fn dispatch(app: &TestApp) -> DispatchReport {
    outbound::dispatch_once(&app.state).await.expect("dispatch")
}

async fn next_attempt_at(app: &TestApp) -> i64 {
    app.count("SELECT next_attempt_at FROM outbound_messages")
        .await
}

fn statuses(rows: &[(String, i64, Option<String>)]) -> Vec<&str> {
    rows.iter().map(|(status, _, _)| status.as_str()).collect()
}

// ── 보낼 일 남기기 ──────────────────────────────────────────────────────

#[tokio::test]
async fn notification_is_queued_and_sent_with_the_documented_payload() {
    let (app, fake) = app_with_fake().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.alter_member(member.id, |m| m.email = Some("kim@example.com".into()))
        .await;
    app.enable_webhook(HOOK).await;

    send_notification(&app, member.id, Some("doc:1")).await;
    assert_eq!(statuses(&app.outbox().await), ["pending"]);
    let outbox_id: String = {
        let mut conn = app.state.read().await.expect("read");
        sqlx::query_scalar("SELECT id FROM outbound_messages")
            .fetch_one(&mut *conn)
            .await
            .expect("id")
    };

    let report = dispatch(&app).await;
    assert_eq!(
        report,
        DispatchReport {
            sent: 1,
            ..DispatchReport::default()
        }
    );
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    let delivery = &calls[0];
    assert_eq!(delivery.target.url, HOOK);
    assert_eq!(delivery.target.secret, "test-secret");
    let body = serde_json::to_value(&delivery.payload).expect("json");
    assert_eq!(
        body,
        json!({
            "event": "notification",
            "delivery_id": outbox_id,
            "kind": "approval.requested",
            "title": "결재 요청: 출장비",
            "body": "김팡고 님이 결재를 요청했어요.",
            // 공개 주소가 없으면 앱 안의 경로 그대로.
            "link": "/approval/documents/abc",
            "recipient": { "login_id": "kim", "name": "kim 님", "email": "kim@example.com" },
            "created_at": app.state.now().as_millis(),
        })
    );
    let rows = app.outbox().await;
    assert_eq!(rows, [("sent".to_owned(), 1, None)]);
    // 다시 돌려도 더 보내지 않는다.
    assert_eq!(dispatch(&app).await, DispatchReport::default());
    assert_eq!(fake.calls().len(), 1);
}

#[tokio::test]
async fn a_real_business_action_reaches_the_outbox() {
    let (app, fake) = app_with_fake().await;
    let owner = app.login_owner().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.enable_webhook(HOOK).await;

    app.post("/admin/notices")
        .bearer(&owner)
        .json(&json!({ "title": "보안 점검 안내", "body": "본문", "audience": "all" }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    dispatch(&app).await;

    let to_member: Vec<_> = fake
        .calls()
        .into_iter()
        .filter(|d| d.payload.recipient.login_id == "kim")
        .collect();
    assert_eq!(to_member.len(), 1);
    assert_eq!(to_member[0].payload.kind, "notice.new");
    assert_eq!(
        to_member[0].payload.title,
        app.notifications_of(member.id).await[0].title
    );
}

#[tokio::test]
async fn nothing_is_queued_while_the_integration_is_off() {
    let (app, fake) = app_with_fake().await;
    let member = app.add_member("kim", Role::Member, None).await;

    send_notification(&app, member.id, None).await;

    assert_eq!(
        app.notifications_of(member.id).await.len(),
        1,
        "앱 알림은 그대로"
    );
    assert!(app.outbox().await.is_empty());
    dispatch(&app).await;
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn members_who_opted_out_get_only_the_in_app_notification() {
    let (app, fake) = app_with_fake().await;
    let member = app.add_member("kim", Role::Member, None).await;
    let other = app.add_member("lee", Role::Member, None).await;
    app.enable_webhook(HOOK).await;
    app.put("/me/notification-preferences")
        .bearer(&member.token)
        .json(&json!({ "external_enabled": false }))
        .send()
        .await
        .expect(StatusCode::OK);

    send_notification(&app, member.id, None).await;
    send_notification(&app, other.id, None).await;
    dispatch(&app).await;

    assert_eq!(app.notifications_of(member.id).await.len(), 1);
    let recipients: Vec<_> = fake
        .calls()
        .into_iter()
        .map(|d| d.payload.recipient.login_id)
        .collect();
    assert_eq!(recipients, ["lee"]);
}

#[tokio::test]
async fn inactive_members_get_nothing_external() {
    let (app, _fake) = app_with_fake().await;
    let suspended = app.add_member("kim", Role::Member, None).await;
    let retired = app.add_member("lee", Role::Member, None).await;
    app.set_status(suspended.id, MemberStatus::Suspended).await;
    app.set_status(retired.id, MemberStatus::Retired).await;
    app.enable_webhook(HOOK).await;

    send_notification(&app, suspended.id, None).await;
    send_notification(&app, retired.id, None).await;

    assert!(app.outbox().await.is_empty());
}

#[tokio::test]
async fn a_repeated_notification_queues_one_message() {
    let (app, fake) = app_with_fake().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.enable_webhook(HOOK).await;

    send_notification(&app, member.id, Some("doc:1:requested")).await;
    send_notification(&app, member.id, Some("doc:1:requested")).await;
    assert_eq!(app.notifications_of(member.id).await.len(), 1);
    assert_eq!(app.outbox().await.len(), 1);
    dispatch(&app).await;
    send_notification(&app, member.id, Some("doc:1:requested")).await;
    dispatch(&app).await;
    assert_eq!(fake.calls().len(), 1);
}

#[tokio::test]
async fn archiving_a_notice_before_sending_cancels_the_message() {
    let (app, fake) = app_with_fake().await;
    let owner = app.login_owner().await;
    app.add_member("kim", Role::Member, None).await;
    app.enable_webhook(HOOK).await;
    let notice = app
        .post("/admin/notices")
        .bearer(&owner)
        .json(&json!({ "title": "곧 보관할 공지", "body": "본문", "audience": "all" }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    assert!(!app.outbox().await.is_empty());

    let id = notice["id"].as_str().expect("id");
    app.delete(&format!("/admin/notices/{id}?version=1"))
        .bearer(&owner)
        .send()
        .await
        .expect(StatusCode::OK);

    assert!(
        app.outbox().await.is_empty(),
        "읽지 않은 알림과 함께 지워진다"
    );
    dispatch(&app).await;
    assert!(fake.calls().is_empty());
}

// ── 발송 실패와 재시도 ───────────────────────────────────────────────────

#[tokio::test]
async fn client_errors_fail_at_once_without_retrying() {
    let (app, fake) = app_with_fake().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.enable_webhook(HOOK).await;
    send_notification(&app, member.id, None).await;
    fake.respond(Err(SendError::http(404, false)));

    let report = dispatch(&app).await;

    assert_eq!(report.failed, 1);
    assert_eq!(
        app.outbox().await,
        [("failed".to_owned(), 1, Some("http_404".to_owned()))]
    );
    app.advance(Duration::hours(3));
    dispatch(&app).await;
    assert_eq!(fake.calls().len(), 1, "다시 보내지 않는다");
    // 외부 발송이 실패해도 앱 알림은 그대로다.
    assert_eq!(app.notifications_of(member.id).await.len(), 1);
}

#[tokio::test]
async fn server_errors_rate_limits_and_network_failures_are_retried_with_backoff() {
    let (app, fake) = app_with_fake().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.enable_webhook(HOOK).await;
    send_notification(&app, member.id, None).await;
    let start = app.state.now().as_millis();
    fake.respond(Err(SendError::http(503, true)));
    fake.respond(Err(SendError::http(429, true)));
    fake.respond(Err(SendError::retry("timeout")));

    assert_eq!(dispatch(&app).await.retrying, 1);
    assert_eq!(next_attempt_at(&app).await, start + 30_000);
    assert_eq!(
        app.outbox().await,
        [("pending".to_owned(), 1, Some("http_503".to_owned()))]
    );

    // 간격 전에는 다시 보내지 않는다(경계: 1밀리초 전).
    app.advance(Duration::milliseconds(29_999));
    dispatch(&app).await;
    assert_eq!(fake.calls().len(), 1);
    app.advance(Duration::milliseconds(1));
    dispatch(&app).await;
    assert_eq!(fake.calls().len(), 2);
    assert_eq!(
        next_attempt_at(&app).await,
        start + 30_000 + 60_000,
        "두 배"
    );

    app.advance(Duration::minutes(1));
    dispatch(&app).await;
    assert_eq!(
        app.outbox().await,
        [("pending".to_owned(), 3, Some("timeout".to_owned()))]
    );

    // 네 번째는 성공한다.
    app.advance(Duration::minutes(2));
    assert_eq!(dispatch(&app).await.sent, 1);
    assert_eq!(app.outbox().await, [("sent".to_owned(), 4, None)]);
}

#[tokio::test]
async fn retries_stop_after_the_maximum_attempts() {
    let (app, fake) = app_with_fake().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.enable_webhook(HOOK).await;
    send_notification(&app, member.id, None).await;
    for _ in 0..MAX_ATTEMPTS + 2 {
        fake.respond(Err(SendError::http(500, true)));
    }

    for _ in 0..MAX_ATTEMPTS + 2 {
        dispatch(&app).await;
        app.advance(Duration::hours(1));
    }

    assert_eq!(fake.calls().len(), MAX_ATTEMPTS as usize);
    assert_eq!(
        app.outbox().await,
        [(
            "failed".to_owned(),
            i64::from(MAX_ATTEMPTS),
            Some("http_500".to_owned())
        )]
    );
}

/// 보내는 도중 서버가 멈춘 경우: 집기만 하고 결과를 남기지 못했다.
#[tokio::test]
async fn a_claimed_message_is_not_picked_again_before_its_retry_time() {
    let (app, fake) = app_with_fake().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.enable_webhook(HOOK).await;
    send_notification(&app, member.id, None).await;
    let now = app.state.now();
    app.state
        .write(async |conn: &mut Conn| {
            let due = pango_db::outbound::due(conn, now, MAX_ATTEMPTS, 10).await?;
            let retry_at = now.checked_add(Duration::seconds(30)).expect("time");
            pango_db::outbound::claim(conn, due[0].id, now, retry_at).await?;
            Ok(())
        })
        .await
        .expect("claim");

    // 다시 띄운 서버가 바로 돌아도 보내지 않는다.
    dispatch(&app).await;
    assert!(fake.calls().is_empty());
    app.advance(Duration::seconds(30));
    dispatch(&app).await;
    assert_eq!(fake.calls().len(), 1, "한 번만 다시 보낸다");
    assert_eq!(app.outbox().await, [("sent".to_owned(), 2, None)]);
}

#[tokio::test]
async fn a_message_interrupted_on_its_last_attempt_ends_as_failed() {
    let (app, fake) = app_with_fake().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.enable_webhook(HOOK).await;
    send_notification(&app, member.id, None).await;
    let max = MAX_ATTEMPTS;
    app.execute(&format!("UPDATE outbound_messages SET attempts = {max}"))
        .await;

    assert_eq!(dispatch(&app).await.failed, 1);
    assert!(fake.calls().is_empty());
    assert_eq!(
        app.outbox().await,
        [(
            "failed".to_owned(),
            i64::from(MAX_ATTEMPTS),
            Some("interrupted".to_owned())
        )]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overlapping_dispatch_runs_never_send_a_message_twice() {
    let fake = FakeSender::with_delay(std::time::Duration::from_millis(50));
    let app = TestApp::with_outbound_sender(fake.clone()).await;
    let mut members = Vec::new();
    for index in 0..12 {
        members.push(
            app.add_member(&format!("member{index}"), Role::Member, None)
                .await,
        );
    }
    app.enable_webhook(HOOK).await;
    for member in &members {
        send_notification(&app, member.id, None).await;
    }

    let (a, b, c) = tokio::join!(
        outbound::dispatch_once(&app.state),
        outbound::dispatch_once(&app.state),
        outbound::dispatch_once(&app.state),
    );
    let sent = a.expect("a").sent + b.expect("b").sent + c.expect("c").sent;

    assert_eq!(sent, 12);
    let mut ids: Vec<_> = fake
        .calls()
        .into_iter()
        .map(|d| d.payload.delivery_id)
        .collect();
    assert_eq!(ids.len(), 12);
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 12, "같은 메시지를 두 번 보내지 않는다");
}

#[tokio::test]
async fn messages_are_dropped_when_things_change_before_sending() {
    let (app, fake) = app_with_fake().await;
    let quitter = app.add_member("kim", Role::Member, None).await;
    let opter = app.add_member("lee", Role::Member, None).await;
    app.enable_webhook(HOOK).await;
    send_notification(&app, quitter.id, None).await;
    send_notification(&app, opter.id, None).await;
    app.set_status(quitter.id, MemberStatus::Retired).await;
    app.put("/me/notification-preferences")
        .bearer(&opter.token)
        .json(&json!({ "external_enabled": false }))
        .send()
        .await
        .expect(StatusCode::OK);

    assert_eq!(dispatch(&app).await.failed, 2);
    assert!(fake.calls().is_empty());
    let mut reasons: Vec<_> = app.outbox().await.into_iter().map(|row| row.2).collect();
    reasons.sort();
    assert_eq!(
        reasons,
        [
            Some("recipient_inactive".to_owned()),
            Some("recipient_opted_out".to_owned())
        ]
    );
}

#[tokio::test]
async fn turning_the_integration_off_stops_queued_messages() {
    let (app, fake) = app_with_fake().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.enable_webhook(HOOK).await;
    send_notification(&app, member.id, None).await;
    app.execute("UPDATE outbound_settings SET webhook_enabled = 0")
        .await;

    dispatch(&app).await;

    assert!(fake.calls().is_empty());
    assert_eq!(
        app.outbox().await,
        [(
            "failed".to_owned(),
            0,
            Some("integration_disabled".to_owned())
        )]
    );
}

// ── 정리 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn finished_messages_are_cleaned_up_after_thirty_days() {
    let (app, fake) = app_with_fake().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.enable_webhook(HOOK).await;
    send_notification(&app, member.id, None).await;
    send_notification(&app, member.id, None).await;
    fake.respond(Ok(200));
    fake.respond(Err(SendError::http(400, false)));
    dispatch(&app).await;
    // 정확히 30일 된 기록은 남기고, 아직 보내지 않은 메시지는 지우지 않는다.
    app.advance(Duration::days(30));
    send_notification(&app, member.id, None).await;
    assert_eq!(
        maintenance::run_once(&app.state)
            .await
            .expect("cleanup")
            .outbound_messages,
        0,
        "보관 기간 안"
    );

    app.advance(Duration::days(1));
    assert_eq!(
        maintenance::run_once(&app.state)
            .await
            .expect("cleanup")
            .outbound_messages,
        2
    );
    assert_eq!(statuses(&app.outbox().await), ["pending"]);
}

// ── 관리자 API ──────────────────────────────────────────────────────────

async fn webhook_settings(app: &TestApp, token: &str) -> Value {
    app.get("/admin/integrations/webhook")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

async fn rotate(app: &TestApp, token: &str, version: i64) -> Value {
    app.post("/admin/integrations/webhook/secret")
        .bearer(token)
        .json(&json!({ "version": version }))
        .send()
        .await
        .expect(StatusCode::OK)
}

async fn put_webhook(app: &TestApp, token: &str, body: &Value) -> (StatusCode, Value) {
    let response = app
        .put("/admin/integrations/webhook")
        .bearer(token)
        .json(body)
        .send()
        .await;
    (response.status, response.body)
}

async fn admin_and_member(app: &TestApp) -> (TestMember, TestMember) {
    (
        app.add_member("admin1", Role::Admin, None).await,
        app.add_member("kim", Role::Member, None).await,
    )
}

#[tokio::test]
async fn only_admins_can_see_or_change_the_integration() {
    let (app, fake) = app_with_fake().await;
    let (admin, member) = admin_and_member(&app).await;
    let calls = [
        app.get("/admin/integrations/webhook").bearer(&member.token),
        app.put("/admin/integrations/webhook")
            .bearer(&member.token)
            .json(&json!({ "version": 1, "enabled": false, "url": HOOK })),
        app.post("/admin/integrations/webhook/secret")
            .bearer(&member.token)
            .json(&json!({ "version": 1 })),
        app.post("/admin/integrations/webhook/test")
            .bearer(&member.token),
    ];
    for request in calls {
        request
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    app.get("/admin/integrations/webhook")
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    // 거부된 요청은 아무것도 바꾸지 않았다.
    let body = webhook_settings(&app, &admin.token).await;
    assert_eq!(
        (
            body["enabled"].clone(),
            body["url"].clone(),
            body["has_secret"].clone()
        ),
        (json!(false), Value::Null, json!(false))
    );
    assert_eq!(body["version"], 1);
    assert!(fake.calls().is_empty());
    assert!(app.audit_actions("integration.").await.is_empty());
}

#[tokio::test]
async fn a_demoted_admin_cannot_change_the_integration() {
    let (app, _fake) = app_with_fake().await;
    let (admin, _) = admin_and_member(&app).await;
    let ctx = app.auth_context(&admin.token).await;
    app.alter_member(admin.id, |m| m.role = Role::Member).await;

    let result = pango_server::service::integrations::rotate_webhook_secret(
        &app.state,
        &ctx,
        &pango_api::Versioned {
            version: 1,
            data: pango_api::Empty {},
        },
    )
    .await;

    assert_eq!(pango_server::testing::denial(&result), "forbidden");
    assert_eq!(
        app.count("SELECT COUNT(*) FROM outbound_settings WHERE webhook_secret IS NOT NULL")
            .await,
        0
    );
}

#[tokio::test]
async fn the_secret_is_shown_once_and_never_again() {
    let (app, _fake) = app_with_fake().await;
    let (admin, _) = admin_and_member(&app).await;

    let rotated = rotate(&app, &admin.token, 1).await;
    let secret = rotated["secret"].as_str().expect("secret").to_owned();
    assert_eq!(secret.len(), 64);
    assert_eq!(rotated["settings"]["has_secret"], true);
    assert_eq!(rotated["settings"]["version"], 2);

    let body = webhook_settings(&app, &admin.token).await;
    assert_eq!(body["has_secret"], true);
    assert!(!body.to_string().contains(&secret), "조회 응답에 없다");
    assert!(body.get("secret").is_none());

    // 다시 만들면 바뀐다.
    let again = rotate(&app, &admin.token, 2).await;
    assert_ne!(again["secret"], json!(secret));

    // 감사 로그에는 남지만 키는 없다.
    let entries = app.audit_entries("integration.webhook_secret_rotate").await;
    assert_eq!(entries.len(), 2);
    for entry in &entries {
        let detail = entry.detail.clone().unwrap_or_default().to_string();
        assert!(!detail.contains(&secret), "{detail}");
        assert!(
            !detail.contains(again["secret"].as_str().expect("s")),
            "{detail}"
        );
    }
    assert_eq!(entries[0].detail, Some(json!({ "replaced": true })));
    assert_eq!(entries[1].detail, Some(json!({ "replaced": false })));
}

#[tokio::test]
async fn settings_are_saved_with_an_optimistic_version() {
    let (app, _fake) = app_with_fake().await;
    let (admin, _) = admin_and_member(&app).await;
    let url = "https://hooks.example.com/in/T0KEN?x=1";
    rotate(&app, &admin.token, 1).await;

    let (status, saved) = put_webhook(
        &app,
        &admin.token,
        &json!({ "version": 2, "enabled": true, "url": url }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        (
            saved["enabled"].clone(),
            saved["url"].clone(),
            saved["version"].clone()
        ),
        (json!(true), json!(url), json!(3))
    );

    // 낡은 버전은 409이고 아무것도 바꾸지 않는다.
    let (status, conflict) = put_webhook(
        &app,
        &admin.token,
        &json!({ "version": 2, "enabled": false, "url": url }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["error"]["code"], "version_conflict");
    let rotate_stale = app
        .post("/admin/integrations/webhook/secret")
        .bearer(&admin.token)
        .json(&json!({ "version": 2 }))
        .send()
        .await;
    rotate_stale.expect_error(StatusCode::CONFLICT, "version_conflict");
    assert_eq!(webhook_settings(&app, &admin.token).await["enabled"], true);

    // 감사 로그에는 주소의 출처만 남는다(경로·쿼리의 토큰은 남지 않는다).
    let entries = app.audit_entries("integration.webhook_update").await;
    assert_eq!(entries.len(), 1);
    let detail = entries[0].detail.clone().expect("detail");
    assert_eq!(
        detail,
        json!({
            "changed": {
                "enabled": { "from": false, "to": true },
                "url_origin": { "from": null, "to": "https://hooks.example.com" },
            },
            "url_changed": true,
        })
    );
    assert!(!detail.to_string().contains("T0KEN"));
}

#[tokio::test]
async fn enabling_needs_a_valid_url_and_a_secret() {
    let (app, _fake) = app_with_fake().await;
    let (admin, _) = admin_and_member(&app).await;
    let check = async |body: Value, field: &str, reason: &str| {
        let (status, error) = put_webhook(&app, &admin.token, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {error}");
        assert_eq!(
            (
                error["error"]["field"].clone(),
                error["error"]["reason"].clone()
            ),
            (json!(field), json!(reason)),
            "{body}"
        );
    };
    check(
        json!({ "version": 1, "enabled": true }),
        "webhook_url",
        "empty",
    )
    .await;
    check(
        json!({ "version": 1, "enabled": true, "url": "  " }),
        "webhook_url",
        "empty",
    )
    .await;
    // 서명 키를 만들기 전에는 켤 수 없다.
    check(
        json!({ "version": 1, "enabled": true, "url": HOOK }),
        "webhook_secret",
        "empty",
    )
    .await;
    for bad in [
        "http://hooks.example.com/in",
        "https://user:pass@hooks.example.com/in",
        "javascript:alert(1)",
        "garbage",
    ] {
        check(
            json!({ "version": 1, "enabled": false, "url": bad }),
            "webhook_url",
            "invalid_format",
        )
        .await;
    }
    let long = format!("https://hooks.example.com/{}", "a".repeat(2048));
    check(
        json!({ "version": 1, "enabled": false, "url": long }),
        "webhook_url",
        "too_long",
    )
    .await;

    // 끈 채로 저장하는 것은 된다(주소만 먼저 넣어 두기). 개발용 주소도 받는다.
    let (status, saved) = put_webhook(
        &app,
        &admin.token,
        &json!({ "version": 1, "enabled": false, "url": "HTTP://[::1]:9000/hook" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["url"], "http://[::1]:9000/hook");
    assert_eq!(
        app.audit_entries("integration.webhook_update").await.len(),
        1
    );
}

#[tokio::test]
async fn delivery_stats_show_recent_failures_without_response_bodies() {
    let (app, fake) = app_with_fake().await;
    let (admin, member) = admin_and_member(&app).await;
    app.enable_webhook(HOOK).await;
    send_notification(&app, member.id, None).await;
    send_notification(&app, member.id, None).await;
    send_notification(&app, member.id, None).await;
    fake.respond(Ok(200));
    fake.respond(Err(SendError::http(404, false)));
    fake.respond(Err(SendError::http(503, true)));
    dispatch(&app).await;

    let stats = webhook_settings(&app, &admin.token).await["stats"].clone();
    assert_eq!(
        (
            stats["pending"].clone(),
            stats["sent_24h"].clone(),
            stats["failed_24h"].clone()
        ),
        (json!(1), json!(1), json!(1))
    );
    assert!(["http_404", "http_503"].contains(&stats["last_error"].as_str().expect("error")));
    assert_eq!(stats["last_error_at"], app.state.now().as_millis());

    app.advance(Duration::hours(25));
    let stats = webhook_settings(&app, &admin.token).await["stats"].clone();
    assert_eq!(
        (stats["sent_24h"].clone(), stats["failed_24h"].clone()),
        (json!(0), json!(0))
    );
}

#[tokio::test]
async fn a_test_message_is_sent_to_the_admin_and_audited() {
    let (app, fake) = app_with_fake().await;
    let (admin, _) = admin_and_member(&app).await;
    // 주소·키가 없으면 보낼 수 없다.
    app.post("/admin/integrations/webhook/test")
        .bearer(&admin.token)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    rotate(&app, &admin.token, 1).await;
    put_webhook(
        &app,
        &admin.token,
        &json!({ "version": 2, "enabled": false, "url": HOOK }),
    )
    .await;

    // 꺼져 있어도 테스트는 보낼 수 있다.
    let result = app
        .post("/admin/integrations/webhook/test")
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        result,
        json!({ "delivered": true, "status": 200, "error": null })
    );
    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].payload.event, "test");
    assert_eq!(calls[0].payload.recipient.login_id, "admin1");
    assert!(calls[0].payload.delivery_id.starts_with("test-"));
    assert!(app.outbox().await.is_empty(), "아웃박스를 거치지 않는다");

    // 바로 다시 누르면 막는다.
    let limited = app
        .post("/admin/integrations/webhook/test")
        .bearer(&admin.token)
        .send()
        .await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.headers["retry-after"], "10");
    assert_eq!(fake.calls().len(), 1);

    app.advance(Duration::seconds(10));
    fake.respond(Err(SendError::http(401, false)));
    let failed = app
        .post("/admin/integrations/webhook/test")
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        failed,
        json!({ "delivered": false, "status": 401, "error": "http_401" })
    );

    let entries = app.audit_entries("integration.webhook_test").await;
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0].detail,
        Some(json!({ "delivered": false, "status": 401, "error": "http_401" }))
    );
}

// ── 내 수신 설정 ────────────────────────────────────────────────────────

#[tokio::test]
async fn members_manage_only_their_own_preference() {
    let (app, _fake) = app_with_fake().await;
    let (_admin, member) = admin_and_member(&app).await;
    let other = app.add_member("lee", Role::Member, None).await;
    let prefs = async |token: &str| {
        app.get("/me/notification-preferences")
            .bearer(token)
            .send()
            .await
            .expect(StatusCode::OK)
    };
    assert_eq!(
        prefs(&member.token).await,
        json!({ "external_available": false, "external_enabled": true })
    );

    let saved = app
        .put("/me/notification-preferences")
        .bearer(&member.token)
        .json(&json!({ "external_enabled": false }))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(saved["external_enabled"], false);
    app.enable_webhook(HOOK).await;
    assert_eq!(
        prefs(&member.token).await,
        json!({ "external_available": true, "external_enabled": false })
    );
    assert_eq!(
        prefs(&other.token).await["external_enabled"],
        true,
        "남의 설정은 그대로"
    );

    app.put("/me/notification-preferences")
        .bearer(&member.token)
        .json(&json!({ "external_enabled": "no" }))
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    app.get("/me/notification-preferences")
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
}

// ── 비밀 값 노출 ────────────────────────────────────────────────────────

#[test]
fn secrets_do_not_show_up_in_debug_output() {
    let target = WebhookTarget {
        url: "https://hooks.example.com/T0KEN".into(),
        secret: "s3cr3t".into(),
    };
    let delivery = Delivery {
        target,
        timestamp: pango_core::time::Timestamp::from_millis(0).expect("ts"),
        payload: OutboundPayload {
            event: "test",
            delivery_id: "d".into(),
            kind: "test".into(),
            title: "t".into(),
            body: "b".into(),
            link: None,
            recipient: Recipient {
                login_id: "kim".into(),
                name: "김".into(),
                email: None,
            },
            created_at: pango_core::time::Timestamp::from_millis(0).expect("ts"),
        },
    };
    let shown = format!("{delivery:?}");
    assert!(
        !shown.contains("s3cr3t") && !shown.contains("T0KEN"),
        "{shown}"
    );
    let dto = pango_api::integration::WebhookSecretDto {
        secret: "s3cr3t".into(),
        settings: pango_api::integration::WebhookSettingsDto {
            enabled: false,
            url: None,
            has_secret: true,
            version: 1,
            updated_at: pango_core::time::Timestamp::from_millis(0).expect("ts"),
            stats: Default::default(),
        },
    };
    assert!(!format!("{dto:?}").contains("s3cr3t"));
}

// ── 실제 HTTP로 보내는 웹훅 발송기 ───────────────────────────────────────

/// 받은 요청의 머리글과 본문.
type Request = (HeaderMap, Vec<u8>);

#[derive(Default, Clone)]
struct Received {
    requests: Arc<Mutex<Vec<Request>>>,
}

/// `127.0.0.1`의 임시 포트에 받는 쪽을 띄운다. 경로별로 정해진 상태로 답한다.
async fn receiver() -> (String, Received) {
    use axum::routing::post;
    let received = Received::default();
    let record = |status: StatusCode| {
        let received = received.clone();
        move |headers: HeaderMap, body: axum::body::Bytes| {
            let received = received.clone();
            async move {
                received
                    .requests
                    .lock()
                    .expect("lock")
                    .push((headers, body.to_vec()));
                (status, "response body that must not be read")
            }
        }
    };
    let redirect = |_: HeaderMap| async {
        (
            StatusCode::FOUND,
            [(http::header::LOCATION, "/ok")],
            "moved",
        )
    };
    let app = axum::Router::new()
        .route("/ok", post(record(StatusCode::NO_CONTENT)))
        .route("/gone", post(record(StatusCode::NOT_FOUND)))
        .route("/busy", post(record(StatusCode::SERVICE_UNAVAILABLE)))
        .route("/slow-down", post(record(StatusCode::TOO_MANY_REQUESTS)))
        .route("/moved", post(redirect));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (format!("http://127.0.0.1:{}", addr.port()), received)
}

fn delivery_to(url: String) -> Delivery {
    Delivery {
        target: WebhookTarget {
            url,
            secret: "receiver-secret".into(),
        },
        timestamp: pango_core::time::Timestamp::from_millis(1_767_225_600_123).expect("ts"),
        payload: OutboundPayload {
            event: "notification",
            delivery_id: "0190-delivery".into(),
            kind: "approval.requested".into(),
            title: "결재 요청".into(),
            body: "본문".into(),
            link: Some("/approval/documents/1".into()),
            recipient: Recipient {
                login_id: "kim".into(),
                name: "김팡고".into(),
                email: Some("kim@example.com".into()),
            },
            created_at: pango_core::time::Timestamp::from_millis(1_767_225_600_000).expect("ts"),
        },
    }
}

#[tokio::test]
async fn the_webhook_sender_posts_signed_json() {
    let (base, received) = receiver().await;
    let sender = WebhookSender::new().expect("sender");

    let status = sender
        .send(&delivery_to(format!("{base}/ok")))
        .await
        .expect("sent");

    assert_eq!(status, 204);
    let requests = received.requests.lock().expect("lock").clone();
    assert_eq!(requests.len(), 1);
    let (headers, body) = &requests[0];
    let header = |name: &str| headers[name].to_str().expect("header").to_owned();
    assert_eq!(header("content-type"), "application/json");
    assert_eq!(header("x-pango-event"), "notification");
    assert_eq!(header("x-pango-delivery"), "0190-delivery");
    assert_eq!(header("x-pango-timestamp"), "1767225600");
    // 받는 쪽은 머리글의 시각과 받은 본문 그대로로 서명을 다시 계산해 비교한다.
    assert_eq!(
        header("x-pango-signature"),
        webhook_signature("receiver-secret", 1_767_225_600, body)
    );
    let json: Value = serde_json::from_slice(body).expect("json");
    assert_eq!(json["recipient"]["email"], "kim@example.com");
    assert_eq!(json["delivery_id"], "0190-delivery");
}

#[tokio::test]
async fn the_webhook_sender_classifies_failures() {
    let (base, received) = receiver().await;
    let sender = WebhookSender::new().expect("sender");
    let send = async |path: &str| sender.send(&delivery_to(format!("{base}{path}"))).await;

    assert_eq!(send("/gone").await, Err(SendError::http(404, false)));
    assert_eq!(send("/busy").await, Err(SendError::http(503, true)));
    assert_eq!(send("/slow-down").await, Err(SendError::http(429, true)));
    // 리다이렉트는 따라가지 않는다.
    assert_eq!(send("/moved").await, Err(SendError::http(302, false)));
    assert_eq!(
        received.requests.lock().expect("lock").len(),
        3,
        "/ok로 가지 않았다"
    );

    // 연결할 수 없는 곳: 다시 보낼 오류이고, 사유에 주소가 들어가지 않는다.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let error = sender
        .send(&delivery_to(format!("http://127.0.0.1:{closed}/T0KEN")))
        .await
        .expect_err("refused");
    assert!(error.retry);
    assert_eq!(error.status, None);
    assert!(!error.reason.contains("T0KEN") && !error.reason.contains("127.0.0.1"));
}
