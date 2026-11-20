//! 공지사항 통합 테스트.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::Duration;
use http::StatusCode;
use pango_core::authz::Role;
use pango_core::id::{DepartmentId, MemberId, NoticeId};
use pango_core::member::MemberStatus;
use pango_db::notices::{self, NoticeFilter, NoticeListItem};
use pango_db::notifications::{self, Notification};
use pango_db::{members, org};
use pango_server::testing::{TestApp, TestMember};
use serde_json::{json, Value};

// ── 준비 ────────────────────────────────────────────────────────────────

fn to_all(title: &str) -> Value {
    json!({ "title": title, "body": "본문입니다.", "audience": "all" })
}

fn to_department(title: &str, department_id: DepartmentId) -> Value {
    json!({
        "title": title, "body": "부서 공지 본문", "audience": "department",
        "department_id": department_id,
    })
}

async fn create(app: &TestApp, token: &str, body: &Value) -> Value {
    app.post("/admin/notices")
        .bearer(token)
        .json(body)
        .send()
        .await
        .expect(StatusCode::CREATED)
}

fn id_of(body: &Value) -> NoticeId {
    body["id"].as_str().expect("id").parse().expect("notice id")
}

/// 공지 목록의 ID들(응답 순서 그대로).
fn ids(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["id"].as_str().expect("id").to_owned())
        .collect()
}

async fn visible_ids(app: &TestApp, token: &str) -> BTreeSet<String> {
    let page = app
        .get("/notices?limit=200")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK);
    ids(&page).into_iter().collect()
}

/// DB에 저장된 공지 전부(보관 포함).
async fn stored_notices(app: &TestApp) -> Vec<NoticeListItem> {
    let mut conn = app.state.db.read().await.expect("conn");
    let filter = NoticeFilter {
        reader: app.owner_id,
        visible_departments: None,
        include_archived: true,
    };
    notices::list(&mut conn, &filter, None, 200)
        .await
        .expect("list")
        .0
}

async fn notifications_of(app: &TestApp, member_id: MemberId) -> Vec<Notification> {
    let mut conn = app.state.db.read().await.expect("conn");
    notifications::list(&mut conn, member_id, false, None, 200)
        .await
        .expect("list")
        .0
}

async fn read_rows(app: &TestApp, notice_id: NoticeId) -> Vec<notices::NoticeRead> {
    let mut conn = app.state.db.read().await.expect("conn");
    notices::list_reads(&mut conn, notice_id)
        .await
        .expect("reads")
}

async fn set_status(app: &TestApp, member_id: MemberId, status: MemberStatus) {
    let mut tx = app.state.db.begin_write().await.expect("begin");
    let mut member = members::find(&mut tx, member_id)
        .await
        .expect("find")
        .expect("member");
    member.status = status;
    assert!(members::update(&mut tx, &member, app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");
}

async fn archive_department(app: &TestApp, department_id: DepartmentId) {
    let mut tx = app.state.db.begin_write().await.expect("begin");
    let mut department = org::find_department(&mut tx, department_id)
        .await
        .expect("find")
        .expect("department");
    department.archived_at = Some(app.state.now());
    assert!(
        org::update_department(&mut tx, &department, app.state.now())
            .await
            .expect("update")
    );
    tx.commit().await.expect("commit");
}

/// 본부 ─ 팀 ─ 파트, 형제 부서, 부서 없는 구성원, 관리자.
struct Org {
    owner: String,
    admin: TestMember,
    hq: DepartmentId,
    team: DepartmentId,
    part: DepartmentId,
    other: DepartmentId,
    hq_member: TestMember,
    team_member: TestMember,
    part_member: TestMember,
    other_member: TestMember,
    homeless: TestMember,
}

async fn org(app: &TestApp) -> Org {
    let hq = app.add_department("본부", None, None).await;
    let team = app.add_department("개발팀", Some(hq), None).await;
    let part = app.add_department("백엔드파트", Some(team), None).await;
    let other = app.add_department("영업팀", Some(hq), None).await;
    Org {
        owner: app.login_owner().await,
        admin: app.add_member("admin1", Role::Admin, None).await,
        hq,
        team,
        part,
        other,
        hq_member: app.add_member("hq1", Role::Member, Some(hq)).await,
        team_member: app.add_member("team1", Role::Member, Some(team)).await,
        part_member: app.add_member("part1", Role::Member, Some(part)).await,
        other_member: app.add_member("other1", Role::Member, Some(other)).await,
        homeless: app.add_member("nodept", Role::Member, None).await,
    }
}

// ── 해피 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn admin_creates_notice_and_member_reads_it() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let mut body = to_all("주간 회의 안내");
    body["pinned"] = json!(true);
    body["must_read"] = json!(true);
    let created = create(&app, &o.admin.token, &body).await;
    assert_eq!(created["title"], "주간 회의 안내");
    assert_eq!(created["body"], "본문입니다.");
    assert_eq!(created["author_name"], "admin1 님");
    assert_eq!(created["audience"], "all");
    assert_eq!(created["department_id"], Value::Null);
    assert_eq!(created["pinned"], true);
    assert_eq!(created["must_read"], true);
    assert_eq!(created["version"], 1);
    assert_eq!(created["published_at"], app.state.now().as_millis());

    let id = id_of(&created);
    let page = app
        .get("/notices")
        .bearer(&o.team_member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(ids(&page), vec![id.to_string()]);
    let item = &page["items"][0];
    assert_eq!(item["read"], false);
    assert!(item.get("body").is_none(), "목록에는 본문이 없다");
    assert_eq!(page["next_cursor"], Value::Null);

    let detail = app
        .get(&format!("/notices/{id}"))
        .bearer(&o.team_member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(detail["body"], "본문입니다.");
    assert_eq!(detail["read"], false, "열람만으로는 읽음 처리되지 않는다");
    assert!(read_rows(&app, id).await.is_empty());
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let app = TestApp::new().await;
    for path in ["/notices", "/notices/unread-count"] {
        app.get(path)
            .send()
            .await
            .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    app.post("/admin/notices")
        .json(&to_all("x"))
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    assert!(stored_notices(&app).await.is_empty());
}

// ── 대상 범위 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn visibility_follows_department_tree() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let all = id_of(&create(&app, &o.owner, &to_all("전체")).await).to_string();
    let team = id_of(&create(&app, &o.owner, &to_department("팀", o.team)).await).to_string();
    let hq = id_of(&create(&app, &o.owner, &to_department("본부", o.hq)).await).to_string();
    let other = id_of(&create(&app, &o.owner, &to_department("영업", o.other)).await).to_string();
    let part = id_of(&create(&app, &o.owner, &to_department("파트", o.part)).await).to_string();

    let set = |ids: &[&String]| ids.iter().map(|s| (*s).clone()).collect::<BTreeSet<_>>();
    let cases = [
        (
            "본부원: 하위 부서 공지는 안 보임",
            &o.hq_member,
            set(&[&all, &hq]),
        ),
        (
            "팀원: 상위 본부 공지는 보임",
            &o.team_member,
            set(&[&all, &hq, &team]),
        ),
        (
            "파트원: 팀·본부 공지까지",
            &o.part_member,
            set(&[&all, &hq, &team, &part]),
        ),
        ("형제 부서원", &o.other_member, set(&[&all, &hq, &other])),
        ("부서 없는 구성원", &o.homeless, set(&[&all])),
        (
            "관리자는 전부",
            &o.admin,
            set(&[&all, &hq, &team, &other, &part]),
        ),
    ];
    for (label, member, expected) in cases {
        assert_eq!(visible_ids(&app, &member.token).await, expected, "{label}");
    }

    // 범위 밖 공지는 상세·읽음 확인 모두 404이고 아무것도 남기지 않는다.
    for path in [format!("/notices/{team}"), format!("/notices/{part}")] {
        app.get(&path)
            .bearer(&o.other_member.token)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    app.post(&format!("/notices/{team}/read"))
        .bearer(&o.hq_member.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    assert!(read_rows(&app, team.parse().expect("id")).await.is_empty());
}

#[tokio::test]
async fn archived_notices_vanish_for_members_but_remain_for_admins() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let kept = id_of(&create(&app, &o.owner, &to_all("유지")).await);
    let gone = id_of(&create(&app, &o.owner, &to_all("보관")).await);
    app.delete(&format!("/admin/notices/{gone}?version=1"))
        .bearer(&o.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);

    for token in [&o.team_member.token, &o.admin.token] {
        assert_eq!(
            visible_ids(&app, token).await,
            BTreeSet::from([kept.to_string()])
        );
        app.get(&format!("/notices/{gone}"))
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
        app.post(&format!("/notices/{gone}/read"))
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    let admin_list = app
        .get("/admin/notices")
        .bearer(&o.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let items = admin_list["items"].as_array().expect("items");
    assert_eq!(items.len(), 2);
    let archived = items
        .iter()
        .find(|i| i["id"] == gone.to_string())
        .expect("archived in admin list");
    assert_eq!(archived["archived_at"], app.state.now().as_millis());
}

// ── 정렬과 페이지 ───────────────────────────────────────────────────────

#[tokio::test]
async fn list_is_pinned_first_and_pages_cleanly_across_ties() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    // (고정, 시각 오프셋 분). 같은 시각과 고정/일반 경계를 일부러 섞는다.
    let plan = [
        (false, 0),
        (true, 0),
        (false, 1),
        (false, 1),
        (true, 2),
        (false, 1),
        (true, 0),
    ];
    let start = app.state.now();
    let mut created = Vec::new();
    for (i, (pinned, minutes)) in plan.into_iter().enumerate() {
        app.set_time(&(start.to_utc() + Duration::minutes(minutes)).to_rfc3339());
        let mut body = to_all(&format!("공지 {i}"));
        body["pinned"] = json!(pinned);
        let notice = create(&app, &o.owner, &body).await;
        created.push((pinned, minutes, id_of(&notice)));
    }
    created.sort_by(|a, b| (b.0, b.1, b.2).cmp(&(a.0, a.1, a.2)));
    let expected: Vec<String> = created.iter().map(|c| c.2.to_string()).collect();

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let path = match &cursor {
            Some(c) => format!("/notices?limit=3&cursor={c}"),
            None => "/notices?limit=3".to_owned(),
        };
        let page = app
            .get(&path)
            .bearer(&o.team_member.token)
            .send()
            .await
            .expect(StatusCode::OK);
        let page_ids = ids(&page);
        assert!(page_ids.len() <= 3);
        seen.extend(page_ids);
        pages += 1;
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, expected, "고정 먼저, 최신순, 빠짐·중복 없음");
}

#[tokio::test]
async fn garbage_cursor_and_limit_bounds() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    for i in 0..3 {
        create(&app, &o.owner, &to_all(&format!("공지 {i}"))).await;
    }
    for cursor in ["abc", "zz", "00", "%27%20OR%201%3D1%20--", "7b7d", "%00"] {
        let response = app
            .get(&format!("/notices?cursor={cursor}"))
            .bearer(&o.team_member.token)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{cursor}");
        assert_eq!(response.body["error"]["field"], "cursor", "{cursor}");
    }
    let page = app
        .get("/notices?limit=0")
        .bearer(&o.team_member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(ids(&page).len(), 1, "limit 0은 1로 올린다");
    app.get("/notices?limit=-1")
        .bearer(&o.team_member.token)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
}

// ── 읽음 확인 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn read_receipt_is_idempotent_and_keeps_first_time() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let id = id_of(&create(&app, &o.owner, &to_all("읽기")).await);
    let first_time = app.state.now().as_millis();
    let first = app
        .post(&format!("/notices/{id}/read"))
        .bearer(&o.team_member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(first["read_at"], first_time);
    app.advance(Duration::hours(1));
    let second = app
        .post(&format!("/notices/{id}/read"))
        .bearer(&o.team_member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(second["read_at"], first_time, "첫 확인 시각 유지");

    let rows = read_rows(&app, id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].member_id, o.team_member.id);
    let detail = app
        .get(&format!("/notices/{id}"))
        .bearer(&o.team_member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        (detail["read"].clone(), detail["read_at"].clone()),
        (json!(true), json!(first_time))
    );
    // 다른 사람의 읽음 여부에는 영향이 없다.
    let page = app
        .get("/notices")
        .bearer(&o.part_member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(page["items"][0]["read"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reads_by_same_member_leave_one_receipt() {
    let app = Arc::new(TestApp::new().await);
    let owner = app.login_owner().await;
    let member = app.add_member("reader", Role::Member, None).await;
    let id = id_of(&create(&app, &owner, &to_all("동시 읽기")).await);
    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let (app, token) = (app.clone(), member.token.clone());
            tokio::spawn(async move {
                app.post(&format!("/notices/{id}/read"))
                    .bearer(&token)
                    .send()
                    .await
                    .status
            })
        })
        .collect();
    for task in tasks {
        assert_eq!(task.await.expect("join"), StatusCode::OK);
    }
    assert_eq!(read_rows(&app, id).await.len(), 1);
}

#[tokio::test]
async fn unread_counts_follow_reads_must_read_and_visibility() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let count = |token: String| {
        let app = &app;
        async move {
            let body = app
                .get("/notices/unread-count")
                .bearer(&token)
                .send()
                .await
                .expect(StatusCode::OK);
            (body["unread"].as_u64(), body["must_read_unread"].as_u64())
        }
    };
    assert_eq!(count(o.team_member.token.clone()).await, (Some(0), Some(0)));

    let mut must = to_all("필독");
    must["must_read"] = json!(true);
    let must_id = id_of(&create(&app, &o.owner, &must).await);
    create(&app, &o.owner, &to_all("일반")).await;
    let mut other_must = to_department("영업 필독", o.other);
    other_must["must_read"] = json!(true);
    create(&app, &o.owner, &other_must).await;

    assert_eq!(count(o.team_member.token.clone()).await, (Some(2), Some(1)));
    assert_eq!(
        count(o.other_member.token.clone()).await,
        (Some(3), Some(2))
    );

    app.post(&format!("/notices/{must_id}/read"))
        .bearer(&o.team_member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(count(o.team_member.token.clone()).await, (Some(1), Some(0)));
    assert_eq!(
        count(o.other_member.token.clone()).await,
        (Some(3), Some(2)),
        "다른 사람의 읽음은 영향 없음"
    );

    // 보관하면 읽지 않은 수에서도 빠진다.
    let to_archive = id_of(&create(&app, &o.owner, &to_all("곧 보관")).await);
    assert_eq!(count(o.team_member.token.clone()).await, (Some(2), Some(0)));
    app.delete(&format!("/admin/notices/{to_archive}?version=1"))
        .bearer(&o.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(count(o.team_member.token.clone()).await, (Some(1), Some(0)));
}

#[tokio::test]
async fn read_report_lists_readers_and_active_non_readers() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let id = id_of(&create(&app, &o.owner, &to_department("팀 공지", o.team)).await);
    app.post(&format!("/notices/{id}/read"))
        .bearer(&o.part_member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let extra = app.add_member("team2", Role::Member, Some(o.team)).await;
    let retired = app.add_member("team3", Role::Member, Some(o.team)).await;
    set_status(&app, retired.id, MemberStatus::Retired).await;

    let report = app
        .get(&format!("/admin/notices/{id}/reads"))
        .bearer(&o.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let members_of = |key: &str| -> Vec<String> {
        report[key]
            .as_array()
            .expect("list")
            .iter()
            .map(|r| r["member_id"].as_str().expect("id").to_owned())
            .collect()
    };
    assert_eq!(members_of("read"), vec![o.part_member.id.to_string()]);
    assert!(report["read"][0]["read_at"].is_i64());
    let unread: BTreeSet<_> = members_of("unread").into_iter().collect();
    assert_eq!(
        unread,
        BTreeSet::from([o.team_member.id.to_string(), extra.id.to_string()]),
        "대상 범위(팀+하위)의 재직자만, 퇴사자 제외"
    );

    app.get(&format!("/admin/notices/{id}/reads"))
        .bearer(&o.team_member.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    app.get(&format!("/admin/notices/{}/reads", NoticeId::new()))
        .bearer(&o.admin.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

// ── 알림 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn creating_notifies_each_audience_member_exactly_once() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let suspended = app.add_member("team9", Role::Member, Some(o.team)).await;
    set_status(&app, suspended.id, MemberStatus::Suspended).await;
    let id = id_of(&create(&app, &o.admin.token, &to_department("팀 공지", o.team)).await);

    for member in [&o.team_member, &o.part_member] {
        let list = notifications_of(&app, member.id).await;
        assert_eq!(list.len(), 1, "{}", member.login_id);
        assert_eq!(list[0].kind, "notice.new");
        assert_eq!(
            list[0].link.as_deref(),
            Some(format!("/notices/{id}").as_str())
        );
    }
    for member in [
        &o.hq_member,
        &o.other_member,
        &o.homeless,
        &suspended,
        &o.admin,
    ] {
        assert!(
            notifications_of(&app, member.id).await.is_empty(),
            "{}: 대상 밖·비활성·작성자는 알림 없음",
            member.login_id
        );
    }

    // 수정은 다시 알리지 않는다.
    let mut edit = to_department("팀 공지(수정)", o.team);
    edit["version"] = json!(1);
    app.patch(&format!("/admin/notices/{id}"))
        .bearer(&o.admin.token)
        .json(&edit)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(notifications_of(&app, o.team_member.id).await.len(), 1);

    let to_everyone = create(&app, &o.admin.token, &to_all("전체 공지")).await;
    assert_eq!(notifications_of(&app, o.homeless.id).await.len(), 1);
    assert_eq!(notifications_of(&app, app.owner_id).await.len(), 1);
    assert!(notifications_of(&app, o.admin.id).await.is_empty());
    assert_eq!(notifications_of(&app, o.team_member.id).await.len(), 2);
    let _ = to_everyone;
}

#[tokio::test]
async fn idempotency_key_replay_creates_one_notice_and_one_notification_each() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let send = |body: Value| {
        let app = &app;
        let token = o.admin.token.clone();
        async move {
            app.post("/admin/notices")
                .bearer(&token)
                .header("idempotency-key", "notice-key-1")
                .json(&body)
                .send()
                .await
        }
    };
    let first = send(to_all("한 번만")).await.expect(StatusCode::CREATED);
    let again = send(to_all("한 번만")).await.expect(StatusCode::CREATED);
    assert_eq!(first, again, "같은 응답을 그대로 돌려준다");
    send(to_all("다른 본문"))
        .await
        .expect_error(StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused");

    assert_eq!(stored_notices(&app).await.len(), 1);
    assert_eq!(notifications_of(&app, o.team_member.id).await.len(), 1);
}

#[tokio::test]
async fn failure_during_fan_out_rolls_back_the_whole_notice() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    // 이름순으로 중간에 오는 구성원에게 알림을 만들 때 DB가 실패하게 한다.
    let mut tx = app.state.db.begin_write().await.expect("begin");
    sqlx::query(&format!(
        "CREATE TRIGGER fail_notify BEFORE INSERT ON notifications
         WHEN NEW.member_id = '{}' BEGIN SELECT RAISE(ABORT, 'boom'); END",
        o.part_member.id
    ))
    .execute(&mut *tx)
    .await
    .expect("trigger");
    tx.commit().await.expect("commit");

    app.post("/admin/notices")
        .bearer(&o.owner)
        .json(&to_all("실패할 공지"))
        .send()
        .await
        .expect_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    assert!(stored_notices(&app).await.is_empty());
    for member in [&o.admin, &o.hq_member, &o.team_member, &o.homeless] {
        assert!(
            notifications_of(&app, member.id).await.is_empty(),
            "{}: 먼저 만든 알림도 롤백",
            member.login_id
        );
    }
    let mut conn = app.state.db.read().await.expect("conn");
    let filter = pango_db::audit::AuditFilter {
        target_type: Some("notice".into()),
        ..Default::default()
    };
    let (entries, _) = pango_db::audit::list(&mut conn, &filter, 10)
        .await
        .expect("audit");
    assert!(entries.is_empty());
}

// ── 수정·보관 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn update_changes_content_and_stale_version_changes_nothing() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let id = id_of(&create(&app, &o.owner, &to_all("원래 제목")).await);
    let published = stored_notices(&app).await[0].published_at;
    app.advance(Duration::minutes(5));

    let mut first = to_department("첫 수정", o.team);
    first["version"] = json!(1);
    first["pinned"] = json!(true);
    let updated = app
        .patch(&format!("/admin/notices/{id}"))
        .bearer(&o.admin.token)
        .json(&first)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(updated["title"], "첫 수정");
    assert_eq!(updated["audience"], "department");
    assert_eq!(updated["department_name"], "개발팀");
    assert_eq!(updated["version"], 2);
    assert_eq!(
        updated["published_at"],
        published.as_millis(),
        "수정은 게시 시각을 바꾸지 않는다"
    );

    // 같은 버전(1)으로 보낸 두 번째 수정은 충돌이고 아무것도 바꾸지 않는다.
    let mut loser = to_all("늦은 수정");
    loser["version"] = json!(1);
    app.patch(&format!("/admin/notices/{id}"))
        .bearer(&o.owner)
        .json(&loser)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    let stored = &stored_notices(&app).await[0];
    assert_eq!(
        (stored.title.as_str(), stored.version, stored.pinned),
        ("첫 수정", 2, true)
    );

    app.patch(&format!("/admin/notices/{}", NoticeId::new()))
        .bearer(&o.owner)
        .json(&loser)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn edit_after_archive_conflicts_and_archiving_twice_is_ok() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let id = id_of(&create(&app, &o.owner, &to_all("보관 예정")).await);

    // 낡은 버전으로는 보관할 수 없다.
    app.delete(&format!("/admin/notices/{id}?version=7"))
        .bearer(&o.owner)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    assert_eq!(stored_notices(&app).await[0].archived_at, None);

    let archived = app
        .delete(&format!("/admin/notices/{id}?version=1"))
        .bearer(&o.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(archived["version"], 2);
    let archived_at = archived["archived_at"].clone();
    assert!(archived_at.is_i64());

    app.advance(Duration::minutes(1));
    let again = app
        .delete(&format!("/admin/notices/{id}?version=1"))
        .bearer(&o.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        (again["archived_at"].clone(), again["version"].clone()),
        (archived_at, json!(2)),
        "두 번째 보관은 아무것도 바꾸지 않는다"
    );

    for version in [1, 2] {
        let mut edit = to_all("보관 후 수정");
        edit["version"] = json!(version);
        app.patch(&format!("/admin/notices/{id}"))
            .bearer(&o.owner)
            .json(&edit)
            .send()
            .await
            .expect_error(StatusCode::CONFLICT, "notice_archived");
    }
    assert_eq!(stored_notices(&app).await[0].title, "보관 예정");

    app.delete(&format!("/admin/notices/{id}"))
        .bearer(&o.owner)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    app.delete(&format!("/admin/notices/{}?version=1", NoticeId::new()))
        .bearer(&o.owner)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn audit_log_records_notice_lifecycle() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let id = id_of(&create(&app, &o.admin.token, &to_all("감사")).await);
    let mut edit = to_all("감사(수정)");
    edit["version"] = json!(1);
    app.patch(&format!("/admin/notices/{id}"))
        .bearer(&o.admin.token)
        .json(&edit)
        .send()
        .await
        .expect(StatusCode::OK);
    for _ in 0..2 {
        app.delete(&format!("/admin/notices/{id}?version=2"))
            .bearer(&o.admin.token)
            .send()
            .await
            .expect(StatusCode::OK);
    }
    let mut conn = app.state.db.read().await.expect("conn");
    let filter = pango_db::audit::AuditFilter {
        target_type: Some("notice".into()),
        ..Default::default()
    };
    let (entries, _) = pango_db::audit::list(&mut conn, &filter, 50)
        .await
        .expect("audit");
    let actions: Vec<_> = entries.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(
        actions,
        vec!["notice.archive", "notice.update", "notice.create"],
        "두 번째 보관은 기록하지 않는다"
    );
    assert!(entries
        .iter()
        .all(|e| e.actor_id == Some(o.admin.id) && e.target_id == Some(id.to_string())));
}

// ── 권한 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn non_admins_cannot_manage_notices_and_nothing_changes() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let manager = app.add_member("lead", Role::Member, Some(o.team)).await;
    app.set_department_head(o.team, Some(manager.id)).await;
    let id = id_of(&create(&app, &o.owner, &to_all("원본")).await);
    let mut edit = to_all("변조");
    edit["version"] = json!(1);

    for member in [&o.team_member, &manager] {
        let token = &member.token;
        app.post("/admin/notices")
            .bearer(token)
            .json(&to_all("몰래"))
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        app.patch(&format!("/admin/notices/{id}"))
            .bearer(token)
            .json(&edit)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        app.delete(&format!("/admin/notices/{id}?version=1"))
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        app.get("/admin/notices")
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    let stored = stored_notices(&app).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(
        (
            stored[0].title.as_str(),
            stored[0].version,
            stored[0].archived_at
        ),
        ("원본", 1, None)
    );
}

// ── 기형·악의적 입력 ─────────────────────────────────────────────────────

#[tokio::test]
async fn malformed_notice_input_is_rejected_without_side_effects() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let archived = app.add_department("해체된 팀", None, None).await;
    archive_department(&app, archived).await;
    let long_title = "가".repeat(201);
    let long_body = "나".repeat(20_001);

    let with = |key: &str, value: Value| {
        let mut body = to_all("정상 제목");
        body[key] = value;
        body
    };
    let validation_cases = [
        (with("title", json!("")), "title"),
        (with("title", json!("   ")), "title"),
        (with("title", json!(long_title)), "title"),
        (with("title", json!("제목\u{0007}벨")), "title"),
        (with("title", json!("제목\n줄바꿈")), "title"),
        (with("title", json!("관리자\u{202E}txt.exe")), "title"),
        (with("title", json!("보이지\u{200B}않음")), "title"),
        (with("body", json!("")), "body"),
        (with("body", json!(long_body)), "body"),
        (with("body", json!("본문\u{0000}널")), "body"),
        (with("department_id", json!(o.team)), "department_id"),
        (with("audience", json!("department")), "department_id"),
    ];
    for (body, field) in validation_cases {
        let response = app
            .post("/admin/notices")
            .bearer(&o.owner)
            .json(&body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(response.error_code(), "validation_failed", "{body}");
        assert_eq!(response.body["error"]["field"], field, "{body}");
    }

    let not_found_cases = [
        to_department("없는 부서", DepartmentId::new()),
        to_department("보관된 부서", archived),
    ];
    for body in not_found_cases {
        app.post("/admin/notices")
            .bearer(&o.owner)
            .json(&body)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }

    let malformed_cases = [
        with("audience", json!("everyone")),
        with("audience", json!(null)),
        with("pinned", json!("yes")),
        with("title", json!(123)),
        with("department_id", json!("not-a-uuid")),
        with("department_id", json!(o.team.to_string().to_uppercase())),
        json!({ "title": "본문 없음", "audience": "all" }),
        json!([]),
    ];
    for body in malformed_cases {
        app.post("/admin/notices")
            .bearer(&o.owner)
            .json(&body)
            .send()
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    app.post("/admin/notices")
        .bearer(&o.owner)
        .raw_body("application/json", "{\"title\": \"잘린")
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    app.post("/admin/notices")
        .bearer(&o.owner)
        .raw_body("application/json", vec![b'{', 0xff, 0xfe, b'}'])
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");

    assert!(stored_notices(&app).await.is_empty());
    assert!(notifications_of(&app, o.team_member.id).await.is_empty());
}

#[tokio::test]
async fn limits_are_inclusive() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let body = json!({
        "title": "가".repeat(200), "body": "나".repeat(20_000), "audience": "all",
    });
    let created = create(&app, &owner, &body).await;
    assert_eq!(
        created["title"].as_str().map(|t| t.chars().count()),
        Some(200)
    );
    assert_eq!(
        created["body"].as_str().map(|t| t.chars().count()),
        Some(20_000)
    );
}

#[tokio::test]
async fn script_tags_are_stored_and_returned_verbatim() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let member = app.add_member("viewer", Role::Member, None).await;
    let script = "<script>alert('x')</script> & \"quote\"";
    let body =
        json!({ "title": script, "body": format!("{script}\n두 번째 줄"), "audience": "all" });
    let id = id_of(&create(&app, &owner, &body).await);
    let response = app
        .get(&format!("/notices/{id}"))
        .bearer(&member.token)
        .send()
        .await;
    assert_eq!(
        response.headers["content-type"], "application/json",
        "HTML로 해석되지 않게 JSON으로만 내보낸다"
    );
    let detail = response.expect(StatusCode::OK);
    assert_eq!(detail["title"], script);
    assert_eq!(detail["body"], format!("{script}\n두 번째 줄"));
}

#[tokio::test]
async fn malformed_or_unknown_ids_are_not_found() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let unknown = NoticeId::new();
    let upper = unknown.to_string().to_uppercase();
    for path in [
        "/notices/not-a-uuid".to_owned(),
        format!("/notices/{upper}"),
        format!("/notices/{unknown}"),
        "/notices/1%27%20OR%201=1".to_owned(),
    ] {
        app.get(&path)
            .bearer(&owner)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
    app.post("/notices/not-a-uuid/read")
        .bearer(&owner)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    app.post(&format!("/notices/{unknown}/read"))
        .bearer(&owner)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

// ── 공지가 사라지면 알림도 사라진다 ──────────────────────────────────────

/// 그 구성원이 가진 `notice.new` 알림의 (제목, 읽음 여부).
async fn notice_alerts(app: &TestApp, member: &TestMember) -> Vec<(String, bool)> {
    let mut alerts: Vec<(String, bool)> = notifications_of(app, member.id)
        .await
        .into_iter()
        .filter(|n| n.kind == "notice.new")
        .map(|n| (n.title, n.read_at.is_some()))
        .collect();
    alerts.sort();
    alerts
}

async fn read_all_notifications(app: &TestApp, member: &TestMember) {
    app.post("/notifications/read-all")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK);
}

async fn unread_notifications(app: &TestApp, member: &TestMember) -> i64 {
    app.get("/notifications/unread-count")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK)["unread"]
        .as_i64()
        .expect("count")
}

/// 알림에는 공지 제목이 들어 있다. 공지를 보관하면 더는 볼 수 없는 공지의 제목이 읽지 않은
/// 알림으로 남아 있으면 안 된다. 이미 읽은 알림은 받은 기록이므로 그대로 둔다.
#[tokio::test]
async fn archiving_removes_unread_notifications_but_keeps_read_ones() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let kept = id_of(&create(&app, &o.owner, &to_all("계속 게시")).await);
    let gone = id_of(&create(&app, &o.owner, &to_all("인사 발령 (비공개 예정)")).await);
    // team_member는 두 알림을 모두 읽었고, 나머지는 읽지 않았다.
    read_all_notifications(&app, &o.team_member).await;
    let both_unread = vec![
        ("계속 게시".to_owned(), false),
        ("인사 발령 (비공개 예정)".to_owned(), false),
    ];
    assert_eq!(notice_alerts(&app, &o.part_member).await, both_unread);
    assert_eq!(unread_notifications(&app, &o.part_member).await, 2);

    app.delete(&format!("/admin/notices/{gone}?version=1"))
        .bearer(&o.owner)
        .send()
        .await
        .expect(StatusCode::OK);

    // 읽지 않은 사람: 보관된 공지의 알림만 사라진다.
    let only_kept = vec![("계속 게시".to_owned(), false)];
    for member in [
        &o.part_member,
        &o.hq_member,
        &o.other_member,
        &o.homeless,
        &o.admin,
    ] {
        assert_eq!(
            notice_alerts(&app, member).await,
            only_kept,
            "{}",
            member.login_id
        );
    }
    assert_eq!(unread_notifications(&app, &o.part_member).await, 1);
    // 읽은 사람: 그대로 남는다.
    assert_eq!(
        notice_alerts(&app, &o.team_member).await,
        vec![
            ("계속 게시".to_owned(), true),
            ("인사 발령 (비공개 예정)".to_owned(), true),
        ]
    );
    // 다른 종류의 알림과 다른 공지는 건드리지 않는다.
    assert!(visible_ids(&app, &o.part_member.token)
        .await
        .contains(&kept.to_string()));

    // 다시 보관해도(자연 멱등) 더 지워지는 것이 없다.
    app.delete(&format!("/admin/notices/{gone}?version=1"))
        .bearer(&o.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(notice_alerts(&app, &o.part_member).await, only_kept);
    assert_eq!(notice_alerts(&app, &o.team_member).await.len(), 2);
}

/// 대상을 좁히면, 더는 그 공지를 볼 수 없는 구성원의 읽지 않은 알림만 사라진다.
#[tokio::test]
async fn narrowing_the_audience_removes_unread_notifications_outside_it() {
    let app = TestApp::new().await;
    let o = org(&app).await;
    let id = id_of(&create(&app, &o.owner, &to_all("조직 개편 안내")).await);
    let other_notice = id_of(&create(&app, &o.owner, &to_all("다른 공지")).await);
    // 대상 밖이 될 사람 중 other_member는 이미 읽었다.
    read_all_notifications(&app, &o.other_member).await;
    let titles = |alerts: Vec<(String, bool)>| -> Vec<String> {
        alerts.into_iter().map(|(title, _)| title).collect()
    };

    // 전체 → 개발팀(하위 백엔드파트 포함)으로 좁힌다.
    let mut narrowed = to_department("조직 개편 안내", o.team);
    narrowed["version"] = json!(1);
    app.patch(&format!("/admin/notices/{id}"))
        .bearer(&o.owner)
        .json(&narrowed)
        .send()
        .await
        .expect(StatusCode::OK);

    let both = vec!["다른 공지".to_owned(), "조직 개편 안내".to_owned()];
    let only_other = vec!["다른 공지".to_owned()];
    // 대상 안(개발팀, 그 하위 파트): 그대로.
    for member in [&o.team_member, &o.part_member] {
        assert_eq!(
            titles(notice_alerts(&app, member).await),
            both,
            "{}",
            member.login_id
        );
        assert_eq!(unread_notifications(&app, member).await, 2);
        assert!(visible_ids(&app, &member.token)
            .await
            .contains(&id.to_string()));
    }
    // 관리자는 대상과 무관하게 볼 수 있으므로 그대로.
    assert_eq!(titles(notice_alerts(&app, &o.admin).await), both);
    // 대상 밖(상위 본부, 형제 부서, 부서 없음): 읽지 않은 것만 사라진다.
    for member in [&o.hq_member, &o.homeless] {
        assert_eq!(
            titles(notice_alerts(&app, member).await),
            only_other,
            "{}",
            member.login_id
        );
        assert_eq!(unread_notifications(&app, member).await, 1);
        assert!(!visible_ids(&app, &member.token)
            .await
            .contains(&id.to_string()));
    }
    assert_eq!(
        notice_alerts(&app, &o.other_member).await,
        vec![
            ("다른 공지".to_owned(), true),
            ("조직 개편 안내".to_owned(), true),
        ],
        "읽은 알림은 남는다"
    );
    assert!(visible_ids(&app, &o.hq_member.token)
        .await
        .contains(&other_notice.to_string()));

    // 대상을 바꾸지 않는 수정(제목만)은 알림을 건드리지 않는다.
    let mut retitled = to_department("조직 개편 안내 (수정)", o.team);
    retitled["version"] = json!(2);
    app.patch(&format!("/admin/notices/{id}"))
        .bearer(&o.owner)
        .json(&retitled)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(titles(notice_alerts(&app, &o.team_member).await), both);
    assert_eq!(titles(notice_alerts(&app, &o.hq_member).await), only_other);

    // 낡은 버전의 수정은 거부되고 알림도 그대로다.
    let mut stale = to_department("조직 개편 안내", o.part);
    stale["version"] = json!(1);
    app.patch(&format!("/admin/notices/{id}"))
        .bearer(&o.owner)
        .json(&stale)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    assert_eq!(titles(notice_alerts(&app, &o.team_member).await), both);

    // 더 좁히면(개발팀 → 백엔드파트) 팀원의 읽지 않은 알림도 사라진다.
    let mut narrower = to_department("조직 개편 안내 (수정)", o.part);
    narrower["version"] = json!(3);
    app.patch(&format!("/admin/notices/{id}"))
        .bearer(&o.owner)
        .json(&narrower)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        titles(notice_alerts(&app, &o.team_member).await),
        only_other
    );
    assert_eq!(titles(notice_alerts(&app, &o.part_member).await), both);
}
