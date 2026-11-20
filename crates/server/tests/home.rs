//! 홈·관리 대시보드 통합 테스트.
//!
//! 한 장면을 만들어 두고 각 숫자를 확인한다. 2026-03-02(월), 근무 09:00–18:00, 지각 유예 0분.
//!
//! | 구성원 | 소속 | 오늘 |
//! | --- | --- | --- |
//! | lead | 개발팀(팀장) | 출근 전 |
//! | kim | 개발팀 | 09:00 출근 |
//! | lee | 개발팀 | 09:30 출근(지각) |
//! | pat | 플랫폼파트(개발팀 하위) | 종일 연차 |
//! | shead | 영업팀(팀장) | 출근 전 |
//! | carol | 영업팀 | 09:00 출근, 02-26·02-27 퇴근 누락 |
//! | admin1 | 없음(관리자) | 출근 전 |
//! | owner | 없음(최고 관리자) | 출근 전 |
//! | gone | 개발팀, 정지됨 | 집계에서 빠진다 |

use http::StatusCode;
use pango_core::authz::Role;
use pango_core::id::{DepartmentId, MemberId};
use pango_core::member::MemberStatus;
use pango_server::testing::{kst, millis, TestApp, TestMember};
use serde_json::{json, Value};

struct Scene {
    app: TestApp,
    owner: String,
    dev: DepartmentId,
    sales: DepartmentId,
    lead: TestMember,
    kim: TestMember,
    lee: TestMember,
    pat: TestMember,
    shead: TestMember,
    carol: TestMember,
    admin: TestMember,
}

async fn get(app: &TestApp, token: &str, path: &str) -> Value {
    app.get(path)
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

async fn post(app: &TestApp, token: &str, path: &str, body: &Value, status: StatusCode) -> Value {
    app.post(path)
        .bearer(token)
        .json(body)
        .send()
        .await
        .expect(status)
}

async fn clock_in(app: &TestApp, who: &TestMember) {
    let body = json!({ "work_type": "office" });
    post(
        app,
        &who.token,
        "/attendance/clock-in",
        &body,
        StatusCode::OK,
    )
    .await;
}

async fn annual_type(app: &TestApp, token: &str) -> String {
    let types = get(app, token, "/leave/types").await;
    types
        .as_array()
        .expect("types")
        .iter()
        .find(|t| t["code"] == "annual")
        .expect("annual")["id"]
        .as_str()
        .expect("id")
        .to_owned()
}

/// 종일 연차를 신청해 문서 ID를 돌려준다.
async fn request_leave(app: &TestApp, who: &TestMember, start: &str, end: &str) -> String {
    let body = json!({
        "leave_type_id": annual_type(app, &who.token).await,
        "start_date": start, "end_date": end, "unit": "full_day", "reason": "개인 사정",
    });
    let created = post(
        app,
        &who.token,
        "/leave/requests",
        &body,
        StatusCode::CREATED,
    )
    .await;
    created["document_id"]
        .as_str()
        .expect("document")
        .to_owned()
}

async fn approve(app: &TestApp, who: &TestMember, document: &str) {
    let path = format!("/approval/documents/{document}/approve");
    post(app, &who.token, &path, &json!({}), StatusCode::OK).await;
}

async fn home(app: &TestApp, token: &str) -> Value {
    get(app, token, "/home").await
}

async fn dashboard(app: &TestApp, token: &str) -> Value {
    get(app, token, "/admin/dashboard").await
}

/// 모든 테이블의 행 수와, 조회가 몰래 바꿀 만한 "읽음" 표시의 수.
async fn snapshot(app: &TestApp) -> Vec<(String, i64)> {
    let mut conn = app.state.db.read().await.expect("conn");
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_sqlx%'
         ORDER BY name",
    )
    .fetch_all(&mut *conn)
    .await
    .expect("tables");
    drop(conn);
    assert!(tables.len() > 15, "tables: {tables:?}");
    let mut counts = Vec::new();
    for table in tables {
        let rows = app.count(&format!("SELECT COUNT(*) FROM {table}")).await;
        counts.push((table, rows));
    }
    for (name, sql) in [
        (
            "read notifications",
            "SELECT COUNT(*) FROM notifications WHERE read_at IS NOT NULL",
        ),
        (
            "read references",
            "SELECT COUNT(*) FROM approval_references WHERE read_at IS NOT NULL",
        ),
        (
            "record versions",
            "SELECT COALESCE(SUM(version), 0) FROM attendance_records",
        ),
    ] {
        counts.push((name.to_owned(), app.count(sql).await));
    }
    counts
}

/// 구성원과 오늘의 출근·휴가까지 만든다. 시계는 09:30에 선다.
async fn scene() -> Scene {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let dev = app.add_department("개발팀", None, None).await;
    let platform = app.add_department("플랫폼파트", Some(dev), None).await;
    let sales = app.add_department("영업팀", None, None).await;
    let lead = app.add_member("lead", Role::Member, Some(dev)).await;
    let kim = app.add_member("kim", Role::Member, Some(dev)).await;
    let lee = app.add_member("lee", Role::Member, Some(dev)).await;
    let pat = app.add_member("pat", Role::Member, Some(platform)).await;
    let shead = app.add_member("shead", Role::Member, Some(sales)).await;
    let carol = app.add_member("carol", Role::Member, Some(sales)).await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let gone = app.add_member("gone", Role::Member, Some(dev)).await;
    app.set_department_head(dev, Some(lead.id)).await;
    app.set_department_head(sales, Some(shead.id)).await;
    app.set_status(gone.id, MemberStatus::Suspended).await;

    clock_in(&app, &kim).await;
    clock_in(&app, &carol).await;
    // pat: 오늘 종일 연차(상위 부서의 팀장이 승인).
    let document = request_leave(&app, &pat, "2026-03-02", "2026-03-02").await;
    approve(&app, &lead, &document).await;
    app.set_time(&kst("2026-03-02T09:30:00"));
    clock_in(&app, &lee).await;
    Scene {
        app,
        owner,
        dev,
        sales,
        lead,
        kim,
        lee,
        pat,
        shead,
        carol,
        admin,
    }
}

async fn create_notice(app: &TestApp, token: &str, body: Value) -> String {
    // 게시 시각이 같으면 순서가 ID로만 정해지므로 1분씩 띄운다.
    app.advance(chrono::Duration::minutes(1));
    let created = post(app, token, "/admin/notices", &body, StatusCode::CREATED).await;
    created["id"].as_str().expect("id").to_owned()
}

fn notice(title: &str) -> Value {
    json!({ "title": title, "body": "본문", "audience": "all" })
}

// ── 홈 ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn home_attendance_matches_the_today_endpoint() {
    let s = scene().await;
    for who in [&s.kim, &s.lee, &s.pat, &s.lead] {
        let view = home(&s.app, &who.token).await;
        let today = get(&s.app, &who.token, "/attendance/today").await;
        assert_eq!(view["today"], today, "{}", who.login_id);
    }
    let kim = home(&s.app, &s.kim.token).await;
    assert_eq!(kim["today"]["work_date"], "2026-03-02");
    assert_eq!(
        kim["today"]["record"]["clock_in_at"],
        millis("2026-03-02T09:00:00")
    );
    assert_eq!(kim["today"]["evaluation"]["status"], "working");
    let lee = home(&s.app, &s.lee.token).await;
    assert_eq!(lee["today"]["evaluation"]["late_minutes"], 30);
    let pat = home(&s.app, &s.pat.token).await;
    assert_eq!(pat["today"]["record"], Value::Null);
    assert_eq!(pat["today"]["evaluation"]["status"], "on_leave");
    let lead = home(&s.app, &s.lead.token).await;
    assert_eq!(lead["today"]["evaluation"]["status"], "scheduled");

    // 전날 시작한 야간 근무는 다음 날 새벽에도 "퇴근"할 기록으로 보인다(같은 판정 코드).
    s.app.set_time(&kst("2026-03-03T06:00:00"));
    let kim = home(&s.app, &s.kim.token).await;
    assert_eq!(kim["today"]["work_date"], "2026-03-03");
    assert_eq!(kim["today"]["record"]["work_date"], "2026-03-02");
    assert_eq!(
        kim["today"],
        get(&s.app, &s.kim.token, "/attendance/today").await
    );
}

#[tokio::test]
async fn home_leave_summary_reports_the_stored_ledger_without_writing() {
    let s = scene().await;
    // kim의 원장에는 아직 아무것도 없다(자동 부여는 잔여 조회·신청·정리 작업이 반영한다).
    let kim_ledger = format!(
        "SELECT COUNT(*) FROM leave_ledger WHERE member_id = '{}'",
        s.kim.id
    );
    assert_eq!(s.app.count(&kim_ledger).await, 0);
    let before = snapshot(&s.app).await;
    let view = home(&s.app, &s.kim.token).await;
    assert_eq!(view["leave"]["member_id"], s.kim.id.to_string());
    assert_eq!(view["leave"]["as_of"], "2026-03-02");
    assert_eq!(view["leave"]["balance"]["remaining"], 0);
    assert_eq!(view["leave"]["expiring_soon"], 0);
    assert_eq!(view["upcoming_leaves"], json!([]));
    assert_eq!(
        s.app.count(&kim_ledger).await,
        0,
        "홈은 부여를 반영하지 않는다"
    );
    assert_eq!(snapshot(&s.app).await, before);

    // 잔여 조회가 부여를 반영한 뒤에는 홈도 같은 값을 보여 준다(입사 2년차: 15일 = 60).
    let balance = get(&s.app, &s.kim.token, "/leave/balance").await;
    assert_eq!(balance["balance"]["remaining"], 60);
    assert_eq!(home(&s.app, &s.kim.token).await["leave"], balance);

    // 다가오는 휴가: 30일 안(04-01까지)에 걸친 결재 중·승인 건만, 시작일순.
    let approved = request_leave(&s.app, &s.kim, "2026-03-20", "2026-03-20").await;
    approve(&s.app, &s.lead, &approved).await;
    request_leave(&s.app, &s.kim, "2026-03-10", "2026-03-11").await;
    request_leave(&s.app, &s.kim, "2026-04-01", "2026-04-02").await;
    request_leave(&s.app, &s.kim, "2026-04-03", "2026-04-03").await;
    let rejected = request_leave(&s.app, &s.kim, "2026-03-16", "2026-03-16").await;
    let path = format!("/approval/documents/{rejected}/reject");
    let reason = json!({ "reason": "일정 조정" });
    post(&s.app, &s.lead.token, &path, &reason, StatusCode::OK).await;

    let view = home(&s.app, &s.kim.token).await;
    let upcoming: Vec<(&str, &str, &str)> = view["upcoming_leaves"]
        .as_array()
        .expect("leaves")
        .iter()
        .map(|l| {
            (
                l["start_date"].as_str().expect("start"),
                l["end_date"].as_str().expect("end"),
                l["status"].as_str().expect("status"),
            )
        })
        .collect();
    assert_eq!(
        upcoming,
        [
            ("2026-03-10", "2026-03-11", "pending"),
            ("2026-03-20", "2026-03-20", "approved"),
            ("2026-04-01", "2026-04-02", "pending"),
        ]
    );
    assert_eq!(view["upcoming_leaves"][0]["leave_type_name"], "연차");
    assert_eq!(view["upcoming_leaves"][0]["amount"], 8);
    // 신청 6일(24)이 예약됐고 반려 1일(4)은 복원됐다.
    assert_eq!(view["leave"]["balance"]["remaining"], 60 - 24);
    assert_eq!(
        view["leave"],
        get(&s.app, &s.kim.token, "/leave/balance").await
    );
    // 남의 휴가는 섞이지 않는다.
    assert_eq!(
        home(&s.app, &s.lee.token).await["upcoming_leaves"],
        json!([])
    );
    let pat = home(&s.app, &s.pat.token).await;
    assert_eq!(pat["upcoming_leaves"].as_array().expect("leaves").len(), 1);
    assert_eq!(pat["leave"]["balance"]["remaining"], 56);
}

#[tokio::test]
async fn home_counts_match_approval_notifications_and_notices() {
    let s = scene().await;
    // kim: 진행 중인 기안 2건(휴가), 참조 문서 1건(lee의 품의).
    request_leave(&s.app, &s.kim, "2026-03-10", "2026-03-10").await;
    request_leave(&s.app, &s.kim, "2026-03-12", "2026-03-12").await;
    let general = s.app.approval_form(&s.lee.token, "general").await["id"].clone();
    let draft = json!({
        "form_id": general, "title": "장비 구매", "body": "모니터 2대",
        "steps": [{ "approver_id": s.lead.id, "kind": "approve" }],
        "references": [s.kim.id], "submit": true,
    });
    let path = "/approval/documents";
    let document = post(&s.app, &s.lee.token, path, &draft, StatusCode::CREATED).await;

    let kim = home(&s.app, &s.kim.token).await;
    assert_eq!(
        kim["approval"],
        json!({ "todo": 0, "in_progress_drafted": 2, "unread_reference": 1 })
    );
    assert_eq!(
        kim["approval"],
        get(&s.app, &s.kim.token, "/approval/counts").await
    );
    // lead: 결재할 문서 3건(pat의 휴가는 이미 승인했다).
    let lead = home(&s.app, &s.lead.token).await;
    assert_eq!(
        lead["approval"],
        json!({ "todo": 3, "in_progress_drafted": 0, "unread_reference": 0 })
    );
    assert_eq!(
        home(&s.app, &s.lee.token).await["approval"],
        json!({ "todo": 0, "in_progress_drafted": 1, "unread_reference": 0 })
    );

    // 알림: kim은 참조 알림 1건, lead는 결재 요청 4건(pat 것 포함), pat은 승인 알림 1건.
    assert_eq!(kim["unread_notifications"], 1);
    assert_eq!(lead["unread_notifications"], 4);
    assert_eq!(home(&s.app, &s.pat.token).await["unread_notifications"], 1);
    assert_eq!(
        home(&s.app, &s.carol.token).await["unread_notifications"],
        0
    );
    for who in [&s.kim, &s.lead, &s.pat] {
        let count = get(&s.app, &who.token, "/notifications/unread-count").await;
        assert_eq!(
            home(&s.app, &who.token).await["unread_notifications"],
            count["unread"]
        );
    }
    // 읽으면 줄어든다. 문서를 열어 보면 참조 건수도 줄어든다.
    post(
        &s.app,
        &s.lead.token,
        "/notifications/read-all",
        &json!({}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(home(&s.app, &s.lead.token).await["unread_notifications"], 0);
    let id = document["id"].as_str().expect("id");
    get(&s.app, &s.kim.token, &format!("/approval/documents/{id}")).await;
    let kim = home(&s.app, &s.kim.token).await;
    assert_eq!(kim["approval"]["unread_reference"], 0);
    assert_eq!(
        kim["unread_notifications"], 1,
        "문서 열람은 알림을 읽음 처리하지 않는다"
    );
}

#[tokio::test]
async fn home_notices_follow_the_same_visibility_as_the_notice_list() {
    let s = scene().await;
    let dev_only = |title: &str, department: DepartmentId| {
        json!({
            "title": title, "body": "본문", "audience": "department",
            "department_id": department, "must_read": true,
        })
    };
    let mut pinned = notice("고정 공지");
    pinned["pinned"] = json!(true);
    let pinned = create_notice(&s.app, &s.admin.token, pinned).await;
    let mut must_read = notice("필독 공지");
    must_read["must_read"] = json!(true);
    let must_read = create_notice(&s.app, &s.admin.token, must_read).await;
    let sales_only = create_notice(&s.app, &s.admin.token, dev_only("영업팀 공지", s.sales)).await;
    let dev_notice = create_notice(&s.app, &s.admin.token, dev_only("개발팀 공지", s.dev)).await;
    let mut plain = Vec::new();
    for title in ["공지 1", "공지 2", "공지 3"] {
        plain.push(create_notice(&s.app, &s.admin.token, notice(title)).await);
    }
    let archived = create_notice(&s.app, &s.admin.token, notice("보관된 공지")).await;
    s.app
        .delete(&format!("/admin/notices/{archived}?version=1"))
        .bearer(&s.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    // kim은 필독 공지 하나를 읽었다.
    let read_path = format!("/notices/{must_read}/read");
    post(&s.app, &s.kim.token, &read_path, &json!({}), StatusCode::OK).await;

    // kim(개발팀): 전체 5건 + 개발팀 1건이 보이고, 그중 1건을 읽었다.
    let kim = home(&s.app, &s.kim.token).await;
    assert_eq!(kim["notices"]["unread"], 5);
    assert_eq!(kim["notices"]["must_read_unread"], 1);
    let latest = kim["notices"]["latest"].as_array().expect("latest");
    let ids: Vec<&str> = latest
        .iter()
        .map(|n| n["id"].as_str().expect("id"))
        .collect();
    assert_eq!(
        ids,
        [
            pinned.as_str(),
            plain[2].as_str(),
            plain[1].as_str(),
            plain[0].as_str(),
            dev_notice.as_str(),
        ],
        "고정 공지 먼저, 그다음 최신순 5건"
    );
    assert_eq!(latest[0]["title"], "고정 공지");
    assert_eq!(latest[0]["pinned"], true);
    assert_eq!(latest[0]["read"], false);
    assert_eq!(latest[4]["must_read"], true);
    assert!(latest[0]["published_at"].is_i64());
    let keys: Vec<&String> = latest[0].as_object().expect("notice").keys().collect();
    assert_eq!(
        keys,
        ["id", "must_read", "pinned", "published_at", "read", "title"],
        "본문·작성자는 싣지 않는다"
    );

    // 누구에게나 홈의 공지는 공지 목록·배지와 같다.
    for who in [&s.kim, &s.pat, &s.carol, &s.lead, &s.admin] {
        let view = home(&s.app, &who.token).await;
        let list = get(&s.app, &who.token, "/notices?limit=5").await;
        let expected: Vec<Value> = list["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|n| {
                json!({
                    "id": n["id"], "title": n["title"], "pinned": n["pinned"],
                    "must_read": n["must_read"], "read": n["read"],
                    "published_at": n["published_at"],
                })
            })
            .collect();
        assert_eq!(
            view["notices"]["latest"],
            json!(expected),
            "{}",
            who.login_id
        );
        let badge = get(&s.app, &who.token, "/notices/unread-count").await;
        assert_eq!(view["notices"]["unread"], badge["unread"]);
        assert_eq!(
            view["notices"]["must_read_unread"],
            badge["must_read_unread"]
        );
        let text = view["notices"].to_string();
        assert!(!text.contains(&archived), "보관된 공지가 보인다");
    }

    // 부서 대상 공지는 그 부서(와 하위 부서)에만 보인다.
    let carol = home(&s.app, &s.carol.token).await;
    assert_eq!(carol["notices"]["unread"], 6);
    assert_eq!(carol["notices"]["must_read_unread"], 2);
    let carol_all = get(&s.app, &s.carol.token, "/notices?limit=200")
        .await
        .to_string();
    assert!(carol_all.contains(&sales_only) && !carol_all.contains(&dev_notice));
    assert!(!carol["notices"].to_string().contains(&dev_notice));
    assert!(!kim["notices"].to_string().contains(&sales_only));
    // 하위 부서(플랫폼파트)의 pat에게는 개발팀 공지가 보인다.
    let pat = home(&s.app, &s.pat.token).await;
    assert_eq!(pat["notices"]["unread"], 6);
    assert_eq!(pat["notices"]["must_read_unread"], 2);
    // 관리자는 전부 본다(보관된 것 제외).
    assert_eq!(home(&s.app, &s.admin.token).await["notices"]["unread"], 7);
}

#[tokio::test]
async fn team_today_is_only_for_managers_and_covers_only_their_scope() {
    let s = scene().await;
    // 일반 구성원에게는 없다.
    for who in [&s.kim, &s.lee, &s.pat, &s.carol] {
        let view = home(&s.app, &who.token).await;
        assert_eq!(view["team_today"], Value::Null, "{}", who.login_id);
    }
    // 개발팀장: 개발팀 + 하위 플랫폼파트(lead, kim, lee, pat). 정지된 gone은 빠진다.
    let lead = home(&s.app, &s.lead.token).await;
    assert_eq!(
        lead["team_today"],
        json!({
            "date": "2026-03-02", "is_working_day": true,
            "total": 4, "present": 2, "not_yet": 1, "on_leave": 1,
        })
    );
    // 영업팀장: shead, carol.
    let shead = home(&s.app, &s.shead.token).await;
    assert_eq!(
        shead["team_today"],
        json!({
            "date": "2026-03-02", "is_working_day": true,
            "total": 2, "present": 1, "not_yet": 1, "on_leave": 0,
        })
    );
    // 관리자: 재직 구성원 8명 전체.
    for token in [&s.admin.token, &s.owner] {
        assert_eq!(
            home(&s.app, token).await["team_today"],
            json!({
                "date": "2026-03-02", "is_working_day": true,
                "total": 8, "present": 3, "not_yet": 4, "on_leave": 1,
            })
        );
    }
    // 팀 현황 화면과 같은 사람들을 센다.
    for who in [&s.lead, &s.shead, &s.admin] {
        let team = get(&s.app, &who.token, "/team/attendance").await;
        let members = team["members"].as_array().expect("members");
        let view = home(&s.app, &who.token).await;
        assert_eq!(view["team_today"]["total"], members.len());
        let present = members.iter().filter(|m| !m["record"].is_null()).count();
        assert_eq!(view["team_today"]["present"], present);
        let on_leave = members
            .iter()
            .filter(|m| m["evaluation"]["status"] == "on_leave")
            .count();
        assert_eq!(view["team_today"]["on_leave"], on_leave);
    }

    // 팀장이 출근하면 바로 반영된다.
    clock_in(&s.app, &s.lead).await;
    let lead = home(&s.app, &s.lead.token).await;
    assert_eq!(lead["team_today"]["present"], 3);
    assert_eq!(lead["team_today"]["not_yet"], 0);

    // 조직장에서 해임되면 사라진다(권한은 매 요청마다 다시 계산한다).
    s.app.set_department_head(s.dev, None).await;
    assert_eq!(home(&s.app, &s.lead.token).await["team_today"], Value::Null);
}

#[tokio::test]
async fn team_today_on_a_day_off_counts_nobody_as_missing() {
    let s = scene().await;
    // 03-07(토): 근무일이 아니다.
    s.app.set_time(&kst("2026-03-07T10:00:00"));
    clock_in(&s.app, &s.kim).await;
    let lead = home(&s.app, &s.lead.token).await;
    assert_eq!(
        lead["team_today"],
        json!({
            "date": "2026-03-07", "is_working_day": false,
            "total": 4, "present": 1, "not_yet": 0, "on_leave": 0,
        })
    );
    let board = dashboard(&s.app, &s.admin.token).await;
    assert_eq!(board["today"]["is_working_day"], false);
    assert_eq!(board["today"]["clocked_in"], 1);
    assert_eq!(board["today"]["not_clocked_in"], 0);
    assert_eq!(board["today"]["late"], 0, "휴일 근무에는 지각이 없다");
}

#[tokio::test]
async fn home_performs_no_writes() {
    let s = scene().await;
    request_leave(&s.app, &s.kim, "2026-03-10", "2026-03-10").await;
    create_notice(&s.app, &s.admin.token, notice("공지")).await;
    let before = snapshot(&s.app).await;
    let audit = s.app.count("SELECT COUNT(*) FROM audit_log").await;
    let ledger = s.app.count("SELECT COUNT(*) FROM leave_ledger").await;
    for _ in 0..3 {
        for token in [
            &s.kim.token,
            &s.lee.token,
            &s.lead.token,
            &s.shead.token,
            &s.admin.token,
            &s.owner,
        ] {
            home(&s.app, token).await;
        }
        dashboard(&s.app, &s.admin.token).await;
        dashboard(&s.app, &s.owner).await;
    }
    assert_eq!(snapshot(&s.app).await, before);
    assert_eq!(s.app.count("SELECT COUNT(*) FROM audit_log").await, audit);
    assert_eq!(
        s.app.count("SELECT COUNT(*) FROM leave_ledger").await,
        ledger
    );
}

#[tokio::test]
async fn home_and_dashboard_require_authentication() {
    let s = scene().await;
    for path in ["/home", "/admin/dashboard"] {
        s.app
            .get(path)
            .send()
            .await
            .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
        s.app
            .get(path)
            .bearer("not-a-token")
            .send()
            .await
            .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    // 읽기 전용 경로다.
    let response = s.app.post("/home").bearer(&s.kim.token).send().await;
    assert_eq!(response.status, StatusCode::METHOD_NOT_ALLOWED);
}

// ── 관리 대시보드 ───────────────────────────────────────────────────────

#[tokio::test]
async fn dashboard_is_forbidden_for_non_admins_and_leaks_nothing() {
    let s = scene().await;
    for who in [&s.kim, &s.lead, &s.shead, &s.pat] {
        let response = s
            .app
            .get("/admin/dashboard")
            .bearer(&who.token)
            .send()
            .await;
        let body = response.body.clone();
        response.expect_error(StatusCode::FORBIDDEN, "forbidden");
        let keys: Vec<&String> = body.as_object().expect("body").keys().collect();
        assert_eq!(keys, ["error"], "오류 말고는 아무것도 없다");
        let text = body.to_string();
        for leaked in ["active_members", "recent_audit", "carol", "pending"] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
    }
    for token in [&s.admin.token, &s.owner] {
        dashboard(&s.app, token).await;
    }
}

#[tokio::test]
async fn dashboard_today_and_pending_counts() {
    let s = scene().await;
    let board = dashboard(&s.app, &s.admin.token).await;
    assert_eq!(
        board["today"],
        json!({
            "date": "2026-03-02", "is_working_day": true,
            "active_members": 8, "clocked_in": 3, "not_clocked_in": 4,
            "on_leave": 1, "late": 1,
        })
    );
    assert_eq!(
        board["pending"],
        json!({ "leave_requests": 0, "correction_requests": 0, "approval_documents": 0 })
    );

    // 휴가 신청 2건(1건은 승인), 정정 요청 2건(1건은 반려), 품의 1건.
    request_leave(&s.app, &s.kim, "2026-03-10", "2026-03-10").await;
    let approved = request_leave(&s.app, &s.lee, "2026-03-11", "2026-03-11").await;
    approve(&s.app, &s.lead, &approved).await;
    let correction = |date: &str| {
        json!({
            "work_date": date,
            "clock_in_at": millis(&format!("{date}T09:00:00")),
            "clock_out_at": millis(&format!("{date}T18:00:00")),
            "work_type": "office", "reason": "기록 누락",
        })
    };
    let path = "/attendance/corrections";
    post(
        &s.app,
        &s.kim.token,
        path,
        &correction("2026-02-27"),
        StatusCode::CREATED,
    )
    .await;
    let rejected = post(
        &s.app,
        &s.lee.token,
        path,
        &correction("2026-02-27"),
        StatusCode::CREATED,
    )
    .await;
    let reject = format!(
        "/approval/documents/{}/reject",
        rejected["document_id"].as_str().expect("document")
    );
    let reason = json!({ "reason": "근거 부족" });
    post(&s.app, &s.lead.token, &reject, &reason, StatusCode::OK).await;
    let general = s.app.approval_form(&s.carol.token, "general").await["id"].clone();
    let mut draft = json!({
        "form_id": general, "title": "출장 신청", "body": "부산",
        "steps": [{ "approver_id": s.shead.id, "kind": "approve" }], "submit": true,
    });
    post(
        &s.app,
        &s.carol.token,
        "/approval/documents",
        &draft,
        StatusCode::CREATED,
    )
    .await;
    // 임시저장 문서는 세지 않는다.
    draft["submit"] = json!(false);
    post(
        &s.app,
        &s.carol.token,
        "/approval/documents",
        &draft,
        StatusCode::CREATED,
    )
    .await;

    let board = dashboard(&s.app, &s.owner).await;
    assert_eq!(
        board["pending"],
        json!({ "leave_requests": 1, "correction_requests": 1, "approval_documents": 3 })
    );
    // 지각자가 퇴근해도 지각으로 남고, 나머지가 출근하면 미출근이 줄어든다.
    s.app.set_time(&kst("2026-03-02T18:00:00"));
    post(
        &s.app,
        &s.lee.token,
        "/attendance/clock-out",
        &json!({}),
        StatusCode::OK,
    )
    .await;
    clock_in(&s.app, &s.lead).await;
    let board = dashboard(&s.app, &s.admin.token).await;
    assert_eq!(board["today"]["clocked_in"], 4);
    assert_eq!(board["today"]["not_clocked_in"], 3);
    assert_eq!(board["today"]["late"], 2, "lee(09:30)와 lead(18:00)");
    assert_eq!(board["today"]["on_leave"], 1);

    // 퇴직·정지된 구성원은 재직 인원에서 빠진다.
    s.app.set_status(s.shead.id, MemberStatus::Retired).await;
    let board = dashboard(&s.app, &s.admin.token).await;
    assert_eq!(board["today"]["active_members"], 7);
    assert_eq!(board["today"]["not_clocked_in"], 2);
}

#[tokio::test]
async fn dashboard_lists_missing_clock_outs_of_the_last_7_days() {
    let s = scene().await;
    let open_record = |member: MemberId, date: &str| {
        json!({
            "member_id": member, "work_date": date,
            "clock_in_at": millis(&format!("{date}T09:00:00")),
            "work_type": "office", "reason": "기록 보정",
        })
    };
    let path = "/admin/attendance";
    for (member, date) in [
        (s.carol.id, "2026-02-26"),
        (s.carol.id, "2026-02-27"),
        (s.lee.id, "2026-02-23"),
        // 8일 전은 범위 밖이다.
        (s.kim.id, "2026-02-22"),
        (s.shead.id, "2026-02-20"),
    ] {
        post(
            &s.app,
            &s.admin.token,
            path,
            &open_record(member, date),
            StatusCode::CREATED,
        )
        .await;
    }
    // 퇴근까지 찍힌 기록은 누락이 아니다.
    let mut closed = open_record(s.pat.id, "2026-02-27");
    closed["clock_out_at"] = json!(millis("2026-02-27T18:00:00"));
    post(&s.app, &s.admin.token, path, &closed, StatusCode::CREATED).await;

    let board = dashboard(&s.app, &s.admin.token).await;
    // 오늘 출근해 아직 근무 중인 kim·lee·carol의 오늘 기록은 누락이 아니다.
    assert_eq!(
        board["missing_clock_out"],
        json!([
            {
                "member_id": s.carol.id, "name": "carol 님", "department_name": "영업팀",
                "dates": ["2026-02-27", "2026-02-26"],
            },
            {
                "member_id": s.lee.id, "name": "lee 님", "department_name": "개발팀",
                "dates": ["2026-02-23"],
            },
        ])
    );

    // 24시간이 지나면 오늘 퇴근하지 않은 기록도 누락이 된다.
    s.app.set_time(&kst("2026-03-03T09:30:01"));
    let board = dashboard(&s.app, &s.admin.token).await;
    let names: Vec<&str> = board["missing_clock_out"]
        .as_array()
        .expect("missing")
        .iter()
        .map(|m| m["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        ["carol 님", "kim 님", "lee 님"],
        "최근 날짜 먼저, 같은 날은 이름순"
    );
    assert_eq!(
        board["missing_clock_out"][0]["dates"],
        json!(["2026-03-02", "2026-02-27", "2026-02-26"])
    );
    // 02-23은 이제 8일 전이라 빠진다.
    assert_eq!(
        board["missing_clock_out"][2]["dates"],
        json!(["2026-03-02"])
    );
}

#[tokio::test]
async fn dashboard_caps_missing_clock_outs_at_20_members() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    for index in 0..23 {
        let member = app
            .add_member(&format!("m{index:02}"), Role::Member, None)
            .await;
        clock_in(&app, &member).await;
    }
    app.set_time(&kst("2026-03-04T10:00:00"));
    let board = dashboard(&app, &owner).await;
    let missing = board["missing_clock_out"].as_array().expect("missing");
    assert_eq!(missing.len(), 20);
    assert_eq!(missing[0]["name"], "m00 님");
    assert_eq!(missing[19]["name"], "m19 님");
    assert_eq!(missing[0]["department_name"], Value::Null);
}

#[tokio::test]
async fn dashboard_shows_the_10_most_recent_audit_entries_with_actor_names() {
    let s = scene().await;
    for index in 0..12 {
        create_notice(&s.app, &s.admin.token, notice(&format!("공지 {index}"))).await;
    }
    let board = dashboard(&s.app, &s.admin.token).await;
    let recent = board["recent_audit"].as_array().expect("audit");
    assert_eq!(recent.len(), 10);
    assert_eq!(recent[0]["action"], "notice.create");
    assert_eq!(recent[0]["actor_id"], s.admin.id.to_string());
    assert_eq!(recent[0]["actor_name"], "admin1 님");
    let log = get(&s.app, &s.admin.token, "/admin/audit-log?limit=10").await;
    assert_eq!(
        board["recent_audit"], log["items"],
        "감사 로그 화면의 첫 10건과 같다"
    );
    let times: Vec<i64> = recent
        .iter()
        .map(|e| e["at"].as_i64().expect("at"))
        .collect();
    assert!(times.windows(2).all(|pair| pair[0] >= pair[1]), "최신순");
    // 비밀 값이 실리지 않는다.
    let text = board.to_string();
    assert!(!text.contains("password") && !text.contains("token_hash"));
}
