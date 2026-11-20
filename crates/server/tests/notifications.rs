//! 앱 내 알림 통합 테스트.

use std::sync::Arc;

use chrono::Duration;
use http::StatusCode;
use pango_core::authz::Role;
use pango_core::id::{MemberId, NotificationId};
use pango_db::notifications::{self, Notification};
use pango_server::testing::{TestApp, TestMember};
use serde_json::Value;

/// 알림을 DB에 직접 만든다. 만든 뒤 시계를 1초 앞으로 보낸다.
async fn notify(app: &TestApp, member_id: MemberId, title: &str) -> NotificationId {
    let notification = Notification {
        id: NotificationId::new(),
        member_id,
        kind: "test.event".into(),
        title: title.to_owned(),
        body: format!("{title} 본문"),
        link: Some("/somewhere".into()),
        dedupe_key: None,
        read_at: None,
        created_at: app.state.now(),
    };
    let mut tx = app.state.db.begin_write().await.expect("begin");
    assert!(notifications::insert(&mut tx, &notification)
        .await
        .expect("insert"));
    tx.commit().await.expect("commit");
    app.advance(Duration::seconds(1));
    notification.id
}

fn titles(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["title"].as_str().expect("title").to_owned())
        .collect()
}

async fn unread_count(app: &TestApp, member: &TestMember) -> u64 {
    app.get("/notifications/unread-count")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK)["unread"]
        .as_u64()
        .expect("count")
}

async fn two_members(app: &TestApp) -> (TestMember, TestMember) {
    (
        app.add_member("alice", Role::Member, None).await,
        app.add_member("bob", Role::Member, None).await,
    )
}

#[tokio::test]
async fn lists_only_own_notifications_newest_first() {
    let app = TestApp::new().await;
    let (alice, bob) = two_members(&app).await;
    notify(&app, alice.id, "a1").await;
    notify(&app, bob.id, "b1").await;
    notify(&app, alice.id, "a2").await;

    let page = app
        .get("/notifications")
        .bearer(&alice.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(titles(&page), vec!["a2", "a1"]);
    let first = &page["items"][0];
    assert_eq!(first["kind"], "test.event");
    assert_eq!(first["link"], "/somewhere");
    assert_eq!(first["read_at"], Value::Null);
    assert!(first.get("member_id").is_none());
    assert_eq!(page["next_cursor"], Value::Null);
    assert_eq!(unread_count(&app, &alice).await, 2);
    assert_eq!(unread_count(&app, &bob).await, 1);
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let app = TestApp::new().await;
    for path in ["/notifications", "/notifications/unread-count"] {
        app.get(path)
            .send()
            .await
            .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    app.post("/notifications/read-all")
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
}

#[tokio::test]
async fn marking_read_is_idempotent_and_keeps_first_time() {
    let app = TestApp::new().await;
    let (alice, _) = two_members(&app).await;
    let id = notify(&app, alice.id, "읽을 것").await;
    let first_time = app.state.now();
    for _ in 0..2 {
        app.post(&format!("/notifications/{id}/read"))
            .bearer(&alice.token)
            .send()
            .await
            .expect(StatusCode::OK);
        app.advance(Duration::minutes(5));
    }
    let list = app.notifications_of(alice.id).await;
    assert_eq!(list[0].read_at, Some(first_time));
    assert_eq!(unread_count(&app, &alice).await, 0);
}

#[tokio::test]
async fn cannot_read_or_mark_another_members_notification() {
    let app = TestApp::new().await;
    let (alice, bob) = two_members(&app).await;
    let bobs = notify(&app, bob.id, "밥의 알림").await;

    app.post(&format!("/notifications/{bobs}/read"))
        .bearer(&alice.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    for path in [
        format!("/notifications/{}/read", NotificationId::new()),
        "/notifications/not-a-uuid/read".to_owned(),
    ] {
        app.post(&path)
            .bearer(&alice.token)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    assert_eq!(
        app.notifications_of(bob.id).await[0].read_at,
        None,
        "남의 알림은 그대로"
    );
    let alice_page = app
        .get("/notifications")
        .bearer(&alice.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert!(titles(&alice_page).is_empty(), "남의 알림은 목록에도 없다");
}

#[tokio::test]
async fn read_all_affects_only_own_unread() {
    let app = TestApp::new().await;
    let (alice, bob) = two_members(&app).await;
    let already = notify(&app, alice.id, "a1").await;
    notify(&app, alice.id, "a2").await;
    notify(&app, alice.id, "a3").await;
    notify(&app, bob.id, "b1").await;
    app.post(&format!("/notifications/{already}/read"))
        .bearer(&alice.token)
        .send()
        .await
        .expect(StatusCode::OK);

    let result = app
        .post("/notifications/read-all")
        .bearer(&alice.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(result["updated"], 2, "이미 읽은 것은 세지 않는다");
    let again = app
        .post("/notifications/read-all")
        .bearer(&alice.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(again["updated"], 0);
    assert_eq!(unread_count(&app, &alice).await, 0);
    assert_eq!(unread_count(&app, &bob).await, 1);
    assert_eq!(app.notifications_of(bob.id).await[0].read_at, None);
}

#[tokio::test]
async fn unread_only_filter() {
    let app = TestApp::new().await;
    let (alice, _) = two_members(&app).await;
    let read = notify(&app, alice.id, "읽음").await;
    notify(&app, alice.id, "안 읽음").await;
    app.post(&format!("/notifications/{read}/read"))
        .bearer(&alice.token)
        .send()
        .await
        .expect(StatusCode::OK);

    let unread = app
        .get("/notifications?unread_only=true")
        .bearer(&alice.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(titles(&unread), vec!["안 읽음"]);
    let everything = app
        .get("/notifications?unread_only=false")
        .bearer(&alice.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(titles(&everything), vec!["안 읽음", "읽음"]);
    app.get("/notifications?unread_only=maybe")
        .bearer(&alice.token)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
}

#[tokio::test]
async fn paging_has_no_gaps_or_duplicates_and_rejects_garbage_cursor() {
    let app = TestApp::new().await;
    let (alice, bob) = two_members(&app).await;
    for i in 0..7 {
        notify(&app, alice.id, &format!("n{i}")).await;
        notify(&app, bob.id, &format!("bob{i}")).await;
    }
    let mut seen = Vec::new();
    let mut path = "/notifications?limit=3".to_owned();
    loop {
        let page = app
            .get(&path)
            .bearer(&alice.token)
            .send()
            .await
            .expect(StatusCode::OK);
        seen.extend(titles(&page));
        match page["next_cursor"].as_str() {
            Some(cursor) => path = format!("/notifications?limit=3&cursor={cursor}"),
            None => break,
        }
    }
    let expected: Vec<String> = (0..7).rev().map(|i| format!("n{i}")).collect();
    assert_eq!(seen, expected);

    for cursor in ["garbage", "00", "%22x%22"] {
        let response = app
            .get(&format!("/notifications?cursor={cursor}"))
            .bearer(&alice.token)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{cursor}");
        assert_eq!(response.body["error"]["field"], "cursor");
    }
}

#[tokio::test]
async fn cursor_from_another_member_does_not_reveal_their_items() {
    let app = TestApp::new().await;
    let (alice, bob) = two_members(&app).await;
    for i in 0..3 {
        notify(&app, bob.id, &format!("bob{i}")).await;
    }
    let bob_page = app
        .get("/notifications?limit=1")
        .bearer(&bob.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let cursor = bob_page["next_cursor"].as_str().expect("cursor");
    let alice_page = app
        .get(&format!("/notifications?cursor={cursor}"))
        .bearer(&alice.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert!(titles(&alice_page).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_read_all_counts_each_notification_once() {
    let app = Arc::new(TestApp::new().await);
    let alice = app.add_member("alice", Role::Member, None).await;
    for i in 0..5 {
        notify(&app, alice.id, &format!("n{i}")).await;
    }
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (app, token) = (app.clone(), alice.token.clone());
            tokio::spawn(async move {
                app.post("/notifications/read-all")
                    .bearer(&token)
                    .send()
                    .await
                    .expect(StatusCode::OK)["updated"]
                    .as_u64()
                    .expect("updated")
            })
        })
        .collect();
    let mut total = 0;
    for task in tasks {
        total += task.await.expect("join");
    }
    assert_eq!(total, 5, "동시에 보내도 합계는 실제 읽지 않은 수와 같다");
}
