//! 출퇴근·근태 통합 테스트.
//!
//! 시계는 2026-03-02(월) 09:00 KST에서 시작한다. 근무일 전환 05:00, 근무 09:00–18:00.

use std::sync::Arc;

use http::StatusCode;
use pango_core::authz::Role;
use pango_core::calendar::DateRange;
use pango_core::id::{DepartmentId, MemberId};
use pango_core::member::MemberStatus;
use pango_db::audit::{self, AuditFilter};
use pango_db::settings::{self, Holiday};
use pango_db::{attendance, members};
use pango_server::testing::{TestApp, TestMember, TestResponse};
use serde_json::{json, Value};

// ── 도우미 ──────────────────────────────────────────────────────────────

fn kst(local: &str) -> String {
    format!("{local}+09:00")
}

fn millis(local: &str) -> i64 {
    pango_server::testing::ts(&kst(local)).as_millis()
}

async fn clock_in(app: &TestApp, token: &str) -> TestResponse {
    app.post("/attendance/clock-in")
        .bearer(token)
        .json(&json!({ "work_type": "office" }))
        .send()
        .await
}

async fn clock_out(app: &TestApp, token: &str) -> TestResponse {
    app.post("/attendance/clock-out").bearer(token).send().await
}

async fn today(app: &TestApp, token: &str) -> Value {
    app.get("/attendance/today")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

async fn month(app: &TestApp, token: &str, month: &str) -> Value {
    app.get(&format!("/attendance/records?month={month}"))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

fn day<'a>(records: &'a Value, date: &str) -> &'a Value {
    records["days"]
        .as_array()
        .expect("days")
        .iter()
        .find(|d| d["date"] == date)
        .unwrap_or_else(|| panic!("no entry for {date}"))
}

async fn events(app: &TestApp, token: &str, record_id: &str) -> Vec<Value> {
    app.get(&format!("/attendance/records/{record_id}/events"))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
        .as_array()
        .expect("events")
        .clone()
}

fn id_of(body: &Value) -> String {
    body["id"].as_str().expect("id").to_owned()
}

/// 구성원의 2026년 3월 저장 기록 수(API를 거치지 않고 DB에서 직접).
async fn stored_records(app: &TestApp, member_id: MemberId, range: (&str, &str)) -> usize {
    let range =
        DateRange::new(range.0.parse().expect("d"), range.1.parse().expect("d")).expect("range");
    let mut conn = app.state.db.read().await.expect("conn");
    attendance::list_for_members(&mut conn, &[member_id], range)
        .await
        .expect("list")
        .len()
}

async fn audit_actions(app: &TestApp) -> Vec<String> {
    let mut conn = app.state.db.read().await.expect("conn");
    let (entries, _) = audit::list(&mut conn, &AuditFilter::default(), 200)
        .await
        .expect("audit");
    entries.into_iter().map(|e| e.action).collect()
}

async fn add_holiday(app: &TestApp, date: &str, name: &str) {
    let mut tx = app.state.db.begin_write().await.expect("begin");
    settings::upsert_holiday(
        &mut tx,
        &Holiday {
            date: date.parse().expect("date"),
            name: name.to_owned(),
        },
    )
    .await
    .expect("holiday");
    tx.commit().await.expect("commit");
}

async fn update_member(app: &TestApp, id: MemberId, change: impl FnOnce(&mut members::Member)) {
    let mut tx = app.state.db.begin_write().await.expect("begin");
    let mut member = members::find(&mut tx, id)
        .await
        .expect("find")
        .expect("member");
    change(&mut member);
    assert!(members::update(&mut tx, &member, app.state.now())
        .await
        .expect("update"));
    tx.commit().await.expect("commit");
}

/// 테스트 DB에 직접 SQL을 실행한다(장애 주입용 트리거 등).
async fn execute(app: &TestApp, sql: &str) {
    let mut tx = app.state.db.begin_write().await.expect("begin");
    sqlx::query(sql).execute(&mut *tx).await.expect("execute");
    tx.commit().await.expect("commit");
}

/// 개발팀(조직장 dev_head, 구성원 alice·bob), 영업팀(조직장 sales_head, 구성원 carol),
/// 관리자 admin(부서 없음), 최고 관리자 owner.
struct Org {
    dev: DepartmentId,
    alice: TestMember,
    bob: TestMember,
    dev_head: TestMember,
    sales_head: TestMember,
    carol: TestMember,
    admin: TestMember,
    owner: String,
}

async fn org(app: &TestApp) -> Org {
    let dev = app.add_department("개발", None, None).await;
    let sales = app.add_department("영업", None, None).await;
    let alice = app.add_member("alice", Role::Member, Some(dev)).await;
    let bob = app.add_member("bob", Role::Member, Some(dev)).await;
    let dev_head = app.add_member("devhead", Role::Member, Some(dev)).await;
    let sales_head = app.add_member("saleshead", Role::Member, Some(sales)).await;
    let carol = app.add_member("carol", Role::Member, Some(sales)).await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    app.set_department_head(dev, Some(dev_head.id)).await;
    app.set_department_head(sales, Some(sales_head.id)).await;
    let owner = app.login_owner().await;
    Org {
        dev,
        alice,
        bob,
        dev_head,
        sales_head,
        carol,
        admin,
        owner,
    }
}

// ── 해피 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn happy_day_clock_in_then_out() {
    let app = TestApp::new().await;
    let alice = app.add_member("alice", Role::Member, None).await;

    let before = today(&app, &alice.token).await;
    assert_eq!(before["work_date"], "2026-03-02");
    assert_eq!(before["is_working_day"], true);
    assert_eq!(before["record"], Value::Null);
    assert_eq!(before["evaluation"]["status"], "scheduled");
    assert_eq!(before["expected_start"], "09:00");
    assert_eq!(before["expected_end"], "18:00");

    let record = app
        .post("/attendance/clock-in")
        .bearer(&alice.token)
        .json(&json!({ "work_type": "remote", "note": "  재택 근무  " }))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(record["work_date"], "2026-03-02");
    assert_eq!(record["member_id"], alice.id.to_string());
    assert_eq!(record["clock_in_at"], millis("2026-03-02T09:00:00"));
    assert_eq!(record["clock_out_at"], Value::Null);
    assert_eq!(record["work_type"], "remote");
    assert_eq!(
        record["note"], "재택 근무",
        "메모는 앞뒤 공백을 정리해 저장한다"
    );

    let working = today(&app, &alice.token).await;
    assert_eq!(working["record"], record);
    assert_eq!(working["evaluation"]["status"], "working");

    app.set_time(&kst("2026-03-02T18:00:00"));
    let closed = clock_out(&app, &alice.token).await.expect(StatusCode::OK);
    assert_eq!(closed["id"], record["id"]);
    assert_eq!(closed["clock_out_at"], millis("2026-03-02T18:00:00"));

    let done = today(&app, &alice.token).await;
    assert_eq!(
        done["evaluation"],
        json!({
            "status": "normal", "worked_minutes": 480, "break_minutes": 60,
            "late_minutes": 0, "early_leave_minutes": 0, "overtime_minutes": 0
        })
    );

    let history = events(&app, &alice.token, &id_of(&record)).await;
    let kinds: Vec<_> = history.iter().map(|e| e["kind"].clone()).collect();
    assert_eq!(kinds, vec![json!("clock_in"), json!("clock_out")]);
    assert_eq!(history[0]["before"], Value::Null);
    assert_eq!(history[1]["before"]["clock_out_at"], Value::Null);
    assert_eq!(
        history[1]["after"]["clock_out_at"],
        millis("2026-03-02T18:00:00")
    );
    assert_eq!(history[0]["actor_id"], alice.id.to_string());
    assert_eq!(history[0]["actor_name"], "alice 님");
}

#[tokio::test]
async fn late_boundary_is_whole_minutes_through_the_api() {
    let app = TestApp::new().await;
    let on_time = app.add_member("ontime", Role::Member, None).await;
    let last_second = app.add_member("lastsecond", Role::Member, None).await;
    let late = app.add_member("late", Role::Member, None).await;

    clock_in(&app, &on_time.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2026-03-02T09:00:59"));
    clock_in(&app, &last_second.token)
        .await
        .expect(StatusCode::OK);
    app.set_time(&kst("2026-03-02T09:01:00"));
    clock_in(&app, &late.token).await.expect(StatusCode::OK);

    for (member, minutes) in [(&on_time, 0), (&last_second, 0), (&late, 1)] {
        let body = today(&app, &member.token).await;
        assert_eq!(
            body["evaluation"]["late_minutes"], minutes,
            "{}",
            member.login_id
        );
    }
}

#[tokio::test]
async fn early_leave_and_overtime_boundaries() {
    let app = TestApp::new().await;
    let early = app.add_member("early", Role::Member, None).await;
    let exact = app.add_member("exact", Role::Member, None).await;
    let overtime = app.add_member("overtime", Role::Member, None).await;
    for m in [&early, &exact, &overtime] {
        clock_in(&app, &m.token).await.expect(StatusCode::OK);
    }
    app.set_time(&kst("2026-03-02T17:59:59"));
    clock_out(&app, &early.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2026-03-02T18:00:00"));
    clock_out(&app, &exact.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2026-03-02T19:30:00"));
    clock_out(&app, &overtime.token)
        .await
        .expect(StatusCode::OK);

    let eval = |body: Value| body["evaluation"].clone();
    let e = eval(today(&app, &early.token).await);
    assert_eq!(
        (e["status"].clone(), e["early_leave_minutes"].clone()),
        (json!("early_leave"), json!(1))
    );
    let e = eval(today(&app, &exact.token).await);
    assert_eq!(
        (e["status"].clone(), e["early_leave_minutes"].clone()),
        (json!("normal"), json!(0))
    );
    let e = eval(today(&app, &overtime.token).await);
    assert_eq!(e["overtime_minutes"], 90);
    assert_eq!(e["worked_minutes"], 570);
}

// ── 순서 역전 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn clock_out_without_clock_in_is_conflict_and_changes_nothing() {
    let app = TestApp::new().await;
    let alice = app.add_member("alice", Role::Member, None).await;
    clock_out(&app, &alice.token)
        .await
        .expect_error(StatusCode::CONFLICT, "not_clocked_in");
    assert_eq!(
        stored_records(&app, alice.id, ("2026-03-01", "2026-03-31")).await,
        0
    );
}

#[tokio::test]
async fn clock_in_after_closed_day_is_conflict() {
    let app = TestApp::new().await;
    let alice = app.add_member("alice", Role::Member, None).await;
    let record = clock_in(&app, &alice.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2026-03-02T18:00:00"));
    clock_out(&app, &alice.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2026-03-02T19:00:00"));
    clock_in(&app, &alice.token)
        .await
        .expect_error(StatusCode::CONFLICT, "already_clocked_out");
    // 거부된 출근은 기록도 이력도 바꾸지 않았다.
    let history = events(&app, &alice.token, &id_of(&record)).await;
    assert_eq!(history.len(), 2);
    assert_eq!(
        today(&app, &alice.token).await["record"]["clock_out_at"],
        millis("2026-03-02T18:00:00")
    );
}

// ── 멱등성 ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn repeated_clock_in_and_out_are_idempotent() {
    let app = TestApp::new().await;
    let alice = app.add_member("alice", Role::Member, None).await;
    let first = clock_in(&app, &alice.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2026-03-02T09:30:00"));
    let again = app
        .post("/attendance/clock-in")
        .bearer(&alice.token)
        .json(&json!({ "work_type": "field", "note": "다른 값" }))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(again, first, "두 번째 출근은 기존 기록을 그대로 돌려준다");

    app.set_time(&kst("2026-03-02T18:00:00"));
    let out = clock_out(&app, &alice.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2026-03-02T18:10:00"));
    let out_again = clock_out(&app, &alice.token).await.expect(StatusCode::OK);
    assert_eq!(out_again, out, "퇴근 시각을 덮어쓰지 않는다");

    let history = events(&app, &alice.token, &id_of(&first)).await;
    let kinds: Vec<_> = history.iter().map(|e| e["kind"].clone()).collect();
    assert_eq!(kinds, vec![json!("clock_in"), json!("clock_out")]);
    assert_eq!(
        stored_records(&app, alice.id, ("2026-03-01", "2026-03-31")).await,
        1
    );
}

// ── 동시성 ──────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_clock_ins_create_exactly_one_record_and_event() {
    let app = Arc::new(TestApp::new().await);
    let alice = app.add_member("alice", Role::Member, None).await;
    let tasks: Vec<_> = (0..10)
        .map(|_| {
            let (app, token) = (app.clone(), alice.token.clone());
            tokio::spawn(async move {
                let response = clock_in(&app, &token).await;
                (response.status, response.body)
            })
        })
        .collect();
    let mut ids = std::collections::HashSet::new();
    for task in tasks {
        let (status, body) = task.await.expect("join");
        assert_eq!(status, StatusCode::OK, "{body}");
        ids.insert(id_of(&body));
    }
    assert_eq!(ids.len(), 1, "모두 같은 기록");
    assert_eq!(
        stored_records(&app, alice.id, ("2026-03-01", "2026-03-31")).await,
        1
    );
    let id = ids.into_iter().next().expect("id");
    assert_eq!(events(&app, &alice.token, &id).await.len(), 1);
}

// ── 엣지: 근무일 전환 ────────────────────────────────────────────────────

#[tokio::test]
async fn night_shift_crossing_cutoff_closes_previous_work_date() {
    let app = TestApp::new().await;
    let alice = app.add_member("alice", Role::Member, None).await;
    app.set_time(&kst("2026-03-02T22:00:00"));
    let record = clock_in(&app, &alice.token).await.expect(StatusCode::OK);
    assert_eq!(record["work_date"], "2026-03-02");

    app.set_time(&kst("2026-03-03T02:00:00"));
    let night = today(&app, &alice.token).await;
    assert_eq!(night["work_date"], "2026-03-02", "05:00 전은 전날 근무일");
    assert_eq!(night["record"]["id"], record["id"]);

    app.set_time(&kst("2026-03-03T06:00:00"));
    let morning = today(&app, &alice.token).await;
    assert_eq!(morning["work_date"], "2026-03-03");
    assert_eq!(
        morning["record"]["id"], record["id"],
        "아직 퇴근으로 닫을 수 있는 야간 기록을 보여 준다"
    );
    assert_eq!(morning["evaluation"]["status"], "working");

    let closed = clock_out(&app, &alice.token).await.expect(StatusCode::OK);
    assert_eq!(closed["id"], record["id"]);
    assert_eq!(closed["work_date"], "2026-03-02");
    assert_eq!(closed["clock_out_at"], millis("2026-03-03T06:00:00"));

    app.set_time(&kst("2026-03-03T09:00:00"));
    let next = clock_in(&app, &alice.token).await.expect(StatusCode::OK);
    assert_ne!(next["id"], record["id"]);
    assert_eq!(next["work_date"], "2026-03-03");
}

#[tokio::test]
async fn work_date_switches_exactly_at_cutoff() {
    let app = TestApp::new().await;
    let before = app.add_member("before", Role::Member, None).await;
    let after = app.add_member("after", Role::Member, None).await;
    app.set_time(&kst("2026-03-03T04:59:59"));
    let early = clock_in(&app, &before.token).await.expect(StatusCode::OK);
    assert_eq!(early["work_date"], "2026-03-02");
    app.set_time(&kst("2026-03-03T05:00:00"));
    let on_cutoff = clock_in(&app, &after.token).await.expect(StatusCode::OK);
    assert_eq!(on_cutoff["work_date"], "2026-03-03");
}

#[tokio::test]
async fn forgotten_clock_out_becomes_missing_and_next_day_still_works() {
    let app = TestApp::new().await;
    let alice = app.add_member("alice", Role::Member, None).await;
    let forgotten = clock_in(&app, &alice.token).await.expect(StatusCode::OK);

    app.set_time(&kst("2026-03-03T10:00:00"));
    // 25시간이 지난 기록은 더 이상 퇴근으로 닫을 수 없다.
    clock_out(&app, &alice.token)
        .await
        .expect_error(StatusCode::CONFLICT, "not_clocked_in");
    let records = month(&app, &alice.token, "2026-03").await;
    assert_eq!(
        day(&records, "2026-03-02")["evaluation"]["status"],
        "missing_clock_out"
    );
    assert_eq!(today(&app, &alice.token).await["record"], Value::Null);

    let next = clock_in(&app, &alice.token).await.expect(StatusCode::OK);
    assert_ne!(next["id"], forgotten["id"]);
    app.set_time(&kst("2026-03-03T19:00:00"));
    let closed = clock_out(&app, &alice.token).await.expect(StatusCode::OK);
    assert_eq!(closed["id"], next["id"]);

    let records = month(&app, &alice.token, "2026-03").await;
    assert_eq!(
        day(&records, "2026-03-02")["evaluation"]["status"],
        "missing_clock_out"
    );
    assert_eq!(day(&records, "2026-03-03")["evaluation"]["status"], "late");
}

#[tokio::test]
async fn weekends_holidays_absence_and_future_days_in_month_view() {
    let app = TestApp::new().await;
    let alice = app.add_member("alice", Role::Member, None).await;
    add_holiday(&app, "2026-03-04", "창립기념일").await;

    // 03-02 정상 근무, 03-03 결근, 03-04 공휴일 근무, 03-07(토) 근무 없음.
    clock_in(&app, &alice.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2026-03-02T18:00:00"));
    clock_out(&app, &alice.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2026-03-04T10:00:00"));
    clock_in(&app, &alice.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2026-03-04T14:00:00"));
    clock_out(&app, &alice.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2026-03-09T12:00:00"));

    let records = month(&app, &alice.token, "2026-03").await;
    assert_eq!(records["member_id"], alice.id.to_string());
    assert_eq!(records["month"], "2026-03");
    assert_eq!(
        day(&records, "2026-03-02")["evaluation"]["status"],
        "normal"
    );
    assert_eq!(
        day(&records, "2026-03-03")["evaluation"]["status"],
        "absent"
    );
    let holiday = day(&records, "2026-03-04");
    assert_eq!(holiday["is_working_day"], false);
    assert_eq!(holiday["holiday_name"], "창립기념일");
    assert_eq!(
        holiday["evaluation"]["status"], "normal",
        "휴일 근무는 지각·조퇴 없음"
    );
    assert_eq!(holiday["evaluation"]["worked_minutes"], 210);
    assert_eq!(holiday["evaluation"]["overtime_minutes"], 0);
    let saturday = day(&records, "2026-03-07");
    assert_eq!(saturday["is_working_day"], false);
    assert_eq!(saturday["holiday_name"], Value::Null);
    assert_eq!(saturday["evaluation"]["status"], "day_off");
    assert_eq!(
        day(&records, "2026-03-09")["evaluation"]["status"],
        "scheduled"
    );
    assert_eq!(
        day(&records, "2026-03-31")["evaluation"]["status"],
        "scheduled"
    );

    let summary = app
        .get("/attendance/summary?month=2026-03")
        .bearer(&alice.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(summary["member_id"], alice.id.to_string());
    assert_eq!(summary["month"], "2026-03");
    assert_eq!(summary["normal_days"], 2);
    // 03-03, 03-05, 03-06 결근.
    assert_eq!(summary["absent_days"], 3);
    assert_eq!(summary["total_worked_minutes"], 480 + 210);
    // 일요일 5일 + 토요일 4일 + 공휴일 0(근무해서 normal).
    assert_eq!(summary["day_off_days"], 9);
}

#[tokio::test]
async fn month_view_has_one_entry_per_calendar_day() {
    let app = TestApp::new().await;
    let alice = app.add_member("alice", Role::Member, None).await;
    for (m, days, last) in [
        ("2026-02", 28, "2026-02-28"),
        ("2028-02", 29, "2028-02-29"),
        ("2026-04", 30, "2026-04-30"),
        ("2026-03", 31, "2026-03-31"),
        ("2026-12", 31, "2026-12-31"),
    ] {
        let records = month(&app, &alice.token, m).await;
        let entries = records["days"].as_array().expect("days");
        assert_eq!(entries.len(), days, "{m}");
        assert_eq!(entries[0]["date"], format!("{m}-01"));
        assert_eq!(entries[days - 1]["date"], last);
    }
}

#[tokio::test]
async fn night_shift_on_new_years_eve_belongs_to_december() {
    let app = TestApp::new().await;
    app.set_time(&kst("2026-12-31T22:00:00"));
    let alice = app.add_member("alice", Role::Member, None).await;
    clock_in(&app, &alice.token).await.expect(StatusCode::OK);
    app.set_time(&kst("2027-01-01T06:00:00"));
    let closed = clock_out(&app, &alice.token).await.expect(StatusCode::OK);
    assert_eq!(closed["work_date"], "2026-12-31");

    let december = month(&app, &alice.token, "2026-12").await;
    assert_eq!(day(&december, "2026-12-31")["record"]["id"], closed["id"]);
    let january = month(&app, &alice.token, "2027-01").await;
    assert_eq!(day(&january, "2027-01-01")["record"], Value::Null);
    assert!(january["days"]
        .as_array()
        .expect("days")
        .iter()
        .all(|d| d["record"].is_null()));
}

// ── 권한 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn personal_records_permission_matrix() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    let record = clock_in(&app, &org.alice.token)
        .await
        .expect(StatusCode::OK);
    let alice = org.alice.id;

    let viewers = [
        ("self", org.alice.token.clone(), StatusCode::OK),
        ("colleague", org.bob.token.clone(), StatusCode::NOT_FOUND),
        ("dept head", org.dev_head.token.clone(), StatusCode::OK),
        (
            "other head",
            org.sales_head.token.clone(),
            StatusCode::NOT_FOUND,
        ),
        ("other dept", org.carol.token.clone(), StatusCode::NOT_FOUND),
        ("admin", org.admin.token.clone(), StatusCode::OK),
        ("owner", org.owner.clone(), StatusCode::OK),
    ];
    let paths = [
        format!("/attendance/records?month=2026-03&member_id={alice}"),
        format!("/attendance/summary?month=2026-03&member_id={alice}"),
        format!("/attendance/records/{}/events", id_of(&record)),
    ];
    for path in &paths {
        let unknown = app
            .get(
                &path
                    .replace(&alice.to_string(), &MemberId::new().to_string())
                    .replace(
                        &id_of(&record),
                        &pango_core::id::AttendanceRecordId::new().to_string(),
                    ),
            )
            .bearer(&org.admin.token)
            .send()
            .await;
        for (who, token, expected) in &viewers {
            let response = app.get(path).bearer(token).send().await;
            assert_eq!(
                response.status, *expected,
                "{who} {path}: {}",
                response.body
            );
            if *expected == StatusCode::NOT_FOUND {
                assert_eq!(
                    response.body, unknown.body,
                    "{who}: 볼 수 없는 기록은 없는 기록과 구별되지 않는다"
                );
            }
        }
    }
    let as_head = app
        .get(&paths[0])
        .bearer(&org.dev_head.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(as_head["member_id"], alice.to_string());
    assert_eq!(day(&as_head, "2026-03-02")["record"]["id"], record["id"]);
}

#[tokio::test]
async fn colleague_cannot_probe_whether_a_record_exists() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    let record = clock_in(&app, &org.alice.token)
        .await
        .expect(StatusCode::OK);
    let real = app
        .get(&format!("/attendance/records/{}/events", id_of(&record)))
        .bearer(&org.bob.token)
        .send()
        .await;
    let fake = app
        .get(&format!(
            "/attendance/records/{}/events",
            pango_core::id::AttendanceRecordId::new()
        ))
        .bearer(&org.bob.token)
        .send()
        .await;
    assert_eq!((real.status, &real.body), (fake.status, &fake.body));
    assert_eq!(real.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn team_attendance_scope() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    clock_in(&app, &org.alice.token)
        .await
        .expect(StatusCode::OK);
    clock_in(&app, &org.carol.token)
        .await
        .expect(StatusCode::OK);
    // 정지된 구성원은 팀 현황에 나오지 않는다.
    update_member(&app, org.bob.id, |m| m.status = MemberStatus::Suspended).await;

    let names = |body: &Value| -> Vec<String> {
        let mut names: Vec<String> = body["members"]
            .as_array()
            .expect("members")
            .iter()
            .map(|m| m["name"].as_str().expect("name").to_owned())
            .collect();
        names.sort();
        names
    };

    let dev = app
        .get("/team/attendance?date=2026-03-02")
        .bearer(&org.dev_head.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(dev["date"], "2026-03-02");
    assert_eq!(names(&dev), vec!["alice 님", "devhead 님"]);
    let alice_row = dev["members"]
        .as_array()
        .expect("members")
        .iter()
        .find(|m| m["member_id"] == org.alice.id.to_string())
        .expect("alice")
        .clone();
    assert_eq!(alice_row["evaluation"]["status"], "working");
    assert_eq!(alice_row["department_id"], org.dev.to_string());
    assert_eq!(alice_row["department_name"], "개발");
    assert!(
        !dev.to_string().contains("carol"),
        "다른 부서 정보가 섞이지 않는다"
    );

    let sales = app
        .get("/team/attendance?date=2026-03-02")
        .bearer(&org.sales_head.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(names(&sales), vec!["carol 님", "saleshead 님"]);

    let all = app
        .get("/team/attendance")
        .bearer(&org.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(all["date"], "2026-03-02", "생략하면 오늘 근무일");
    assert_eq!(
        names(&all),
        vec![
            "admin1 님",
            "alice 님",
            "carol 님",
            "devhead 님",
            "saleshead 님",
            "대표"
        ]
    );

    for token in [&org.alice.token, &org.carol.token] {
        app.get("/team/attendance?date=2026-03-02")
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
}

#[tokio::test]
async fn admin_routes_reject_non_admins_without_side_effects() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    let record = clock_in(&app, &org.alice.token)
        .await
        .expect(StatusCode::OK);
    let edit = json!({
        "version": record["version"], "clock_in_at": record["clock_in_at"],
        "clock_out_at": millis("2026-03-02T10:00:00"), "work_type": "office", "reason": "x"
    });
    app.set_time(&kst("2026-03-02T11:00:00"));
    for token in [&org.alice.token, &org.bob.token, &org.dev_head.token] {
        app.get("/admin/attendance?from=2026-03-01&to=2026-03-31")
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        app.get("/admin/attendance/export.csv?from=2026-03-01&to=2026-03-31")
            .bearer(token)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        app.patch(&format!("/admin/attendance/{}", id_of(&record)))
            .bearer(token)
            .json(&edit)
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
        app.post("/admin/attendance")
            .bearer(token)
            .json(&json!({
                "member_id": org.bob.id, "work_date": "2026-03-02",
                "clock_in_at": millis("2026-03-02T09:00:00"), "work_type": "office", "reason": "x"
            }))
            .send()
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    assert_eq!(
        events(&app, &org.alice.token, &id_of(&record)).await.len(),
        1
    );
    assert_eq!(
        stored_records(&app, org.bob.id, ("2026-03-01", "2026-03-31")).await,
        0
    );
    assert!(audit_actions(&app)
        .await
        .iter()
        .all(|a| !a.starts_with("attendance.")));

    // 관리자·최고 관리자는 쓸 수 있다.
    for token in [&org.admin.token, &org.owner] {
        app.get("/admin/attendance?from=2026-03-01&to=2026-03-31")
            .bearer(token)
            .send()
            .await
            .expect(StatusCode::OK);
    }
}

#[tokio::test]
async fn suspended_member_cannot_clock_in() {
    let app = TestApp::new().await;
    let alice = app.add_member("alice", Role::Member, None).await;
    update_member(&app, alice.id, |m| m.status = MemberStatus::Suspended).await;
    clock_in(&app, &alice.token)
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    assert_eq!(
        stored_records(&app, alice.id, ("2026-03-01", "2026-03-31")).await,
        0
    );
}

// ── 관리자 수정 ─────────────────────────────────────────────────────────

struct Edited {
    app: TestApp,
    org: Org,
    record: Value,
}

async fn edit_fixture() -> Edited {
    let app = TestApp::new().await;
    let org = org(&app).await;
    let record = clock_in(&app, &org.alice.token)
        .await
        .expect(StatusCode::OK);
    app.set_time(&kst("2026-03-03T12:00:00"));
    Edited { app, org, record }
}

impl Edited {
    async fn patch(&self, body: &Value) -> TestResponse {
        self.app
            .patch(&format!("/admin/attendance/{}", id_of(&self.record)))
            .bearer(&self.org.admin.token)
            .json(body)
            .send()
            .await
    }

    fn body(&self, clock_in: &str, clock_out: Option<&str>) -> Value {
        json!({
            "version": self.record["version"],
            "clock_in_at": millis(clock_in),
            "clock_out_at": clock_out.map(millis),
            "work_type": "office",
            "reason": "퇴근 누락",
        })
    }

    async fn history_len(&self) -> usize {
        events(&self.app, &self.org.admin.token, &id_of(&self.record))
            .await
            .len()
    }
}

#[tokio::test]
async fn admin_edit_records_history_with_before_and_after() {
    let fx = edit_fixture().await;
    let edited = fx
        .patch(&fx.body("2026-03-02T08:50:00", Some("2026-03-02T18:30:00")))
        .await
        .expect(StatusCode::OK);
    assert_eq!(edited["id"], fx.record["id"]);
    assert_eq!(edited["version"], 2);
    assert_eq!(edited["clock_in_at"], millis("2026-03-02T08:50:00"));
    assert_eq!(edited["clock_out_at"], millis("2026-03-02T18:30:00"));

    let history = events(&fx.app, &fx.org.alice.token, &id_of(&fx.record)).await;
    let last = history.last().expect("event");
    assert_eq!(last["kind"], "admin_edit");
    assert_eq!(last["reason"], "퇴근 누락");
    assert_eq!(last["actor_id"], fx.org.admin.id.to_string());
    assert_eq!(last["before"]["clock_out_at"], Value::Null);
    assert_eq!(last["before"]["clock_in_at"], millis("2026-03-02T09:00:00"));
    assert_eq!(last["after"]["clock_out_at"], millis("2026-03-02T18:30:00"));
    assert!(audit_actions(&fx.app)
        .await
        .contains(&"attendance.edit".to_owned()));

    let records = month(&fx.app, &fx.org.alice.token, "2026-03").await;
    let evaluation = &day(&records, "2026-03-02")["evaluation"];
    assert_eq!(evaluation["status"], "normal");
    assert_eq!(evaluation["overtime_minutes"], 30);
}

#[tokio::test]
async fn admin_edit_validation_failures_change_nothing() {
    let fx = edit_fixture().await;
    let base = fx.body("2026-03-02T09:00:00", Some("2026-03-02T18:00:00"));
    let with = |patch: Value| {
        let mut body = base.clone();
        for (k, v) in patch.as_object().expect("object") {
            body[k] = v.clone();
        }
        body
    };
    let cases = [
        (with(json!({ "reason": "   " })), "reason"),
        (with(json!({ "reason": "\u{0007}벨" })), "reason"),
        (
            with(json!({ "clock_out_at": millis("2026-03-02T08:00:00") })),
            "clock_out_at",
        ),
        (
            with(json!({ "clock_out_at": millis("2026-03-02T09:00:00") })),
            "clock_out_at",
        ),
        (
            with(json!({ "clock_out_at": millis("2026-03-03T09:00:01") })),
            "clock_out_at",
        ),
        (
            with(json!({
                "clock_in_at": millis("2026-03-03T12:10:00"),
                "clock_out_at": null
            })),
            "clock_in_at",
        ),
        (
            with(json!({
                "clock_in_at": millis("2026-03-03T09:00:00"),
                "clock_out_at": millis("2026-03-03T11:00:00")
            })),
            "work_date",
        ),
        (
            with(json!({ "clock_in_at": -1, "clock_out_at": null })),
            "work_date",
        ),
        (
            with(json!({ "clock_in_at": -3_000_000_000_000_i64, "clock_out_at": null })),
            "clock_in_at",
        ),
    ];
    for (body, field) in cases {
        let response = fx.patch(&body).await;
        assert_eq!(response.body["error"]["field"], field, "{body}");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    // 사유 필드 자체가 없거나 형식이 틀린 본문.
    let mut no_reason = base.clone();
    no_reason.as_object_mut().expect("object").remove("reason");
    fx.patch(&no_reason)
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    fx.patch(&with(
        json!({ "clock_in_at": 1_000_000_000_000_000_000_i64 }),
    ))
    .await
    .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    fx.patch(&with(json!({ "work_type": "beach" })))
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");

    assert_eq!(
        fx.history_len().await,
        1,
        "실패한 수정은 이력을 남기지 않는다"
    );
    let records = month(&fx.app, &fx.org.alice.token, "2026-03").await;
    assert_eq!(day(&records, "2026-03-02")["record"], fx.record);
    assert!(!audit_actions(&fx.app)
        .await
        .contains(&"attendance.edit".to_owned()));
}

#[tokio::test]
async fn admin_edit_span_up_to_24h_is_allowed() {
    let fx = edit_fixture().await;
    fx.patch(&fx.body("2026-03-02T09:00:00", Some("2026-03-03T09:00:00")))
        .await
        .expect(StatusCode::OK);
}

#[tokio::test]
async fn stale_version_loses_and_changes_nothing() {
    let fx = edit_fixture().await;
    let body = fx.body("2026-03-02T09:00:00", Some("2026-03-02T18:00:00"));
    fx.patch(&body).await.expect(StatusCode::OK);
    let mut loser = fx.body("2026-03-02T07:00:00", Some("2026-03-02T12:00:00"));
    loser["version"] = json!(1);
    fx.patch(&loser)
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");

    assert_eq!(fx.history_len().await, 2, "패자는 이력을 남기지 않는다");
    let records = month(&fx.app, &fx.org.alice.token, "2026-03").await;
    let stored = &day(&records, "2026-03-02")["record"];
    assert_eq!(stored["version"], 2);
    assert_eq!(stored["clock_out_at"], millis("2026-03-02T18:00:00"));
    let edits = audit_actions(&fx.app)
        .await
        .into_iter()
        .filter(|a| a == "attendance.edit")
        .count();
    assert_eq!(edits, 1);
}

#[tokio::test]
async fn admin_edit_unknown_or_malformed_ids_are_not_found() {
    let fx = edit_fixture().await;
    let body = fx.body("2026-03-02T09:00:00", Some("2026-03-02T18:00:00"));
    for id in [
        pango_core::id::AttendanceRecordId::new().to_string(),
        "not-a-uuid".to_owned(),
        id_of(&fx.record).to_uppercase(),
        "1%20OR%201=1".to_owned(),
    ] {
        fx.app
            .patch(&format!("/admin/attendance/{id}"))
            .bearer(&fx.org.admin.token)
            .json(&body)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
        fx.app
            .get(&format!("/attendance/records/{id}/events"))
            .bearer(&fx.org.admin.token)
            .send()
            .await
            .expect_error(StatusCode::NOT_FOUND, "not_found");
    }
}

// ── 관리자 생성 ─────────────────────────────────────────────────────────

fn create_body(member_id: MemberId, date: &str, clock_in: &str, clock_out: Option<&str>) -> Value {
    json!({
        "member_id": member_id,
        "work_date": date,
        "clock_in_at": millis(clock_in),
        "clock_out_at": clock_out.map(millis),
        "work_type": "field",
        "note": "외근",
        "reason": "출장 중 기록 누락",
    })
}

#[tokio::test]
async fn admin_create_and_duplicates() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    app.set_time(&kst("2026-03-04T12:00:00"));
    let body = create_body(
        org.bob.id,
        "2026-03-03",
        "2026-03-03T09:10:00",
        Some("2026-03-03T18:00:00"),
    );
    let created = app
        .post("/admin/attendance")
        .bearer(&org.admin.token)
        .json(&body)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(created["member_id"], org.bob.id.to_string());
    assert_eq!(created["work_date"], "2026-03-03");
    assert_eq!(created["work_type"], "field");
    assert_eq!(created["note"], "외근");

    let history = events(&app, &org.bob.token, &id_of(&created)).await;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["kind"], "admin_create");
    assert_eq!(history[0]["before"], Value::Null);
    assert_eq!(history[0]["reason"], "출장 중 기록 누락");
    assert!(audit_actions(&app)
        .await
        .contains(&"attendance.create".to_owned()));

    let records = month(&app, &org.bob.token, "2026-03").await;
    assert_eq!(
        day(&records, "2026-03-03")["evaluation"]["late_minutes"],
        10
    );

    // 같은 구성원·근무일은 한 건뿐.
    app.post("/admin/attendance")
        .bearer(&org.admin.token)
        .json(&create_body(
            org.bob.id,
            "2026-03-03",
            "2026-03-03T08:00:00",
            None,
        ))
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "already_exists");
    // 본인이 출근한 날도 마찬가지.
    clock_in(&app, &org.bob.token).await.expect(StatusCode::OK);
    app.post("/admin/attendance")
        .bearer(&org.admin.token)
        .json(&create_body(
            org.bob.id,
            "2026-03-04",
            "2026-03-04T08:00:00",
            None,
        ))
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "already_exists");
    assert_eq!(
        stored_records(&app, org.bob.id, ("2026-03-01", "2026-03-31")).await,
        2
    );
    let creates = audit_actions(&app)
        .await
        .into_iter()
        .filter(|a| a == "attendance.create")
        .count();
    assert_eq!(creates, 1);

    // 없는 구성원은 404, 검증 실패는 400.
    app.post("/admin/attendance")
        .bearer(&org.admin.token)
        .json(&create_body(
            MemberId::new(),
            "2026-03-03",
            "2026-03-03T09:00:00",
            None,
        ))
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    let mut blank = create_body(org.carol.id, "2026-03-03", "2026-03-03T09:00:00", None);
    blank["reason"] = json!("");
    app.post("/admin/attendance")
        .bearer(&org.admin.token)
        .json(&blank)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    for bad_date in ["2026-3-3", "2026-02-30", "03/03/2026", ""] {
        let mut body = create_body(org.carol.id, "2026-03-03", "2026-03-03T09:00:00", None);
        body["work_date"] = json!(bad_date);
        let response = app
            .post("/admin/attendance")
            .bearer(&org.admin.token)
            .json(&body)
            .send()
            .await;
        assert_eq!(response.body["error"]["field"], "work_date", "{bad_date}");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
}

#[tokio::test]
async fn admin_create_honours_idempotency_key() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    app.set_time(&kst("2026-03-04T12:00:00"));
    let body = create_body(org.bob.id, "2026-03-03", "2026-03-03T09:00:00", None);
    let send = || {
        app.post("/admin/attendance")
            .bearer(&org.admin.token)
            .header("idempotency-key", "create-bob-0303")
            .json(&body)
            .send()
    };
    let first = send().await.expect(StatusCode::OK);
    let second = send().await.expect(StatusCode::OK);
    assert_eq!(first, second);
    assert_eq!(events(&app, &org.bob.token, &id_of(&first)).await.len(), 1);
}

// ── 관리자 목록 ─────────────────────────────────────────────────────────

#[tokio::test]
async fn admin_list_paginates_and_filters() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    let child = app.add_department("개발-플랫폼", Some(org.dev), None).await;
    let dana = app.add_member("dana", Role::Member, Some(child)).await;
    for day in 2..=6 {
        app.set_time(&kst(&format!("2026-03-0{day}T09:00:00")));
        for m in [&org.alice, &org.carol, &dana] {
            clock_in(&app, &m.token).await.expect(StatusCode::OK);
        }
    }
    let token = &org.admin.token;
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut path = "/admin/attendance?from=2026-03-01&to=2026-03-31&limit=4".to_owned();
        if let Some(c) = &cursor {
            path.push_str(&format!("&cursor={c}"));
        }
        let page = app
            .get(&path)
            .bearer(token)
            .send()
            .await
            .expect(StatusCode::OK);
        let items = page["items"].as_array().expect("items");
        assert!(items.len() <= 4);
        seen.extend(items.iter().map(|i| i["record"]["id"].clone()));
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(seen.len(), 15);
    let mut unique = seen.clone();
    unique.sort_by_key(|v| v.to_string());
    unique.dedup();
    assert_eq!(unique.len(), 15);

    let first = app
        .get("/admin/attendance?from=2026-03-01&to=2026-03-31&limit=1")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK);
    let item = &first["items"][0];
    assert_eq!(item["record"]["work_date"], "2026-03-06", "최신 근무일부터");
    assert!(item["member_name"].is_string() && item["login_id"].is_string());
    assert_eq!(item["evaluation"]["status"], "working");

    let dev = app
        .get(&format!(
            "/admin/attendance?from=2026-03-01&to=2026-03-31&department_id={}",
            org.dev
        ))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK);
    let logins: std::collections::BTreeSet<_> = dev["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["login_id"].as_str().expect("login").to_owned())
        .collect();
    assert_eq!(
        logins.into_iter().collect::<Vec<_>>(),
        vec!["alice", "dana"],
        "하위 부서 포함"
    );

    let one = app
        .get(&format!(
            "/admin/attendance?from=2026-03-03&to=2026-03-04&member_id={}",
            org.carol.id
        ))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(one["items"].as_array().expect("items").len(), 2);
    assert_eq!(one["next_cursor"], Value::Null);
}

// ── CSV ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn csv_export_is_excel_safe() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    let evil = app.add_member("evil", Role::Member, None).await;
    let plus = app.add_member("plus", Role::Member, None).await;
    let tricky = app.add_member("tricky", Role::Member, None).await;
    update_member(&app, evil.id, |m| {
        m.name = r#"=HYPERLINK("http://evil","x")"#.into()
    })
    .await;
    update_member(&app, plus.id, |m| m.name = "+cmd".into()).await;
    update_member(&app, tricky.id, |m| m.name = "박,\"철\"\n수".into()).await;
    for m in [&org.alice, &evil, &plus, &tricky] {
        clock_in(&app, &m.token).await.expect(StatusCode::OK);
    }
    app.set_time(&kst("2026-03-02T18:00:00"));
    clock_out(&app, &org.alice.token)
        .await
        .expect(StatusCode::OK);

    let response = app
        .get("/admin/attendance/export.csv?from=2026-03-01&to=2026-03-31")
        .bearer(&org.admin.token)
        .send()
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.headers["content-type"], "text/csv; charset=utf-8");
    let disposition = response.headers["content-disposition"]
        .to_str()
        .expect("header");
    assert!(disposition.starts_with("attachment;"), "{disposition}");
    assert!(disposition.contains("attendance-2026-03-01-2026-03-31.csv"));
    let csv = response.body.as_str().expect("csv text");
    assert!(csv.starts_with('\u{feff}'), "BOM");
    let lines: Vec<&str> = csv.trim_start_matches('\u{feff}').split("\r\n").collect();
    assert_eq!(
        lines[0],
        "날짜,이름,아이디,부서,근무 형태,출근,퇴근,근무 분,상태"
    );
    assert!(csv.contains(
        "2026-03-02,alice 님,alice,개발,office,2026-03-02 09:00,2026-03-02 18:00,480,normal"
    ));
    assert!(
        csv.contains(r#","'=HYPERLINK(""http://evil"",""x"")",evil,"#),
        "{csv}"
    );
    assert!(csv.contains(",'+cmd,plus,"), "{csv}");
    assert!(csv.contains("\"박,\"\"철\"\"\n수\",tricky,"), "{csv}");
    // 퇴근 전 기록은 빈 칸.
    assert!(
        csv.contains(",'+cmd,plus,,office,2026-03-02 09:00,,0,working"),
        "{csv}"
    );
    assert!(
        !csv.contains(",=") && !csv.contains(",+"),
        "수식으로 시작하는 칸이 없다"
    );
    assert!(audit_actions(&app)
        .await
        .contains(&"attendance.export".to_owned()));
}

#[tokio::test]
async fn csv_export_range_rules() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    let empty = app
        .get("/admin/attendance/export.csv?from=2026-01-05&to=2026-01-05")
        .bearer(&org.admin.token)
        .send()
        .await;
    assert_eq!(empty.status, StatusCode::OK);
    let text = empty.body.as_str().expect("csv");
    assert_eq!(
        text,
        "\u{feff}날짜,이름,아이디,부서,근무 형태,출근,퇴근,근무 분,상태\r\n"
    );

    app.get("/admin/attendance/export.csv?from=2026-01-01&to=2027-01-01")
        .bearer(&org.admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    for (from, to, field) in [
        ("2026-01-01", "2027-01-02", "date_range"),
        ("2026-03-02", "2026-03-01", "date_range"),
        ("2026-3-1", "2026-03-31", "from"),
        ("2026-03-01", "tomorrow", "to"),
    ] {
        let response = app
            .get(&format!("/admin/attendance/export.csv?from={from}&to={to}"))
            .bearer(&org.admin.token)
            .send()
            .await;
        assert_eq!(response.body["error"]["field"], field, "{from}~{to}");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    app.get("/admin/attendance/export.csv")
        .bearer(&org.admin.token)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
}

// ── 기형 입력 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn malformed_month_and_date_queries_are_rejected() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    let token = &org.admin.token;
    for bad in [
        "2026-13",
        "2026-1",
        "0000-00",
        "99999-01",
        "2026-00",
        "abc",
        "",
        "2026-03%00",
    ] {
        for path in ["/attendance/records", "/attendance/summary"] {
            let response = app
                .get(&format!("{path}?month={bad}"))
                .bearer(token)
                .send()
                .await;
            assert_eq!(response.body["error"]["field"], "month", "{path} {bad:?}");
            response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
        }
    }
    app.get("/attendance/records")
        .bearer(token)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    app.get("/attendance/records?month=2026-03&member_id=not-a-uuid")
        .bearer(token)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    // 없는 구성원은 존재를 드러내지 않는다.
    app.get(&format!(
        "/attendance/records?month=2026-03&member_id={}",
        MemberId::new()
    ))
    .bearer(token)
    .send()
    .await
    .expect_error(StatusCode::NOT_FOUND, "not_found");

    for bad in ["2026-3-2", "2026-02-30", "yesterday", "2026-03-02'--"] {
        let response = app
            .get(&format!("/team/attendance?date={bad}"))
            .bearer(token)
            .send()
            .await;
        assert_eq!(response.body["error"]["field"], "date", "{bad}");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    for cursor in [
        "garbage",
        "2026-03-02~not-a-uuid",
        "~",
        "2026-3-2~0190",
        "%00",
    ] {
        let response = app
            .get(&format!(
                "/admin/attendance?from=2026-03-01&to=2026-03-31&cursor={cursor}"
            ))
            .bearer(token)
            .send()
            .await;
        assert_eq!(response.body["error"]["field"], "cursor", "{cursor}");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
}

#[tokio::test]
async fn malformed_clock_in_bodies_create_nothing() {
    let app = TestApp::new().await;
    let alice = app.add_member("alice", Role::Member, None).await;
    let post = |body: Value| {
        let (app, token) = (&app, alice.token.clone());
        async move {
            app.post("/attendance/clock-in")
                .bearer(&token)
                .json(&body)
                .send()
                .await
        }
    };
    for body in [
        json!({ "work_type": "beach" }),
        json!({ "work_type": "OFFICE" }),
        json!({}),
        json!({ "work_type": 1 }),
        json!({ "work_type": "office", "note": 5 }),
    ] {
        post(body.clone())
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    for note in [
        "가".repeat(10_000),
        "a\u{0000}b".into(),
        "a\u{202E}b".into(),
    ] {
        let response = post(json!({ "work_type": "office", "note": note })).await;
        assert_eq!(response.body["error"]["field"], "note");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    app.post("/attendance/clock-in")
        .bearer(&alice.token)
        .raw_body("text/plain", "work_type=office")
        .send()
        .await
        .expect_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type");
    assert_eq!(
        stored_records(&app, alice.id, ("2026-03-01", "2026-03-31")).await,
        0
    );

    // 빈 메모는 없는 것으로 저장한다.
    let record = post(json!({ "work_type": "office", "note": "   " }))
        .await
        .expect(StatusCode::OK);
    assert_eq!(record["note"], Value::Null);
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let app = TestApp::new().await;
    for (method, path) in [
        ("GET", "/attendance/today"),
        ("POST", "/attendance/clock-in"),
        ("POST", "/attendance/clock-out"),
        ("GET", "/attendance/records?month=2026-03"),
        ("GET", "/team/attendance"),
        ("GET", "/admin/attendance?from=2026-03-01&to=2026-03-02"),
    ] {
        let request = match method {
            "GET" => app.get(path),
            _ => app.post(path),
        };
        request
            .send()
            .await
            .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
}

// ── 부분 실패(롤백) ─────────────────────────────────────────────────────

#[tokio::test]
async fn failure_after_record_insert_rolls_back_clock_in() {
    let app = TestApp::new().await;
    let alice = app.add_member("alice", Role::Member, None).await;
    execute(
        &app,
        "CREATE TRIGGER fail_clock_in BEFORE INSERT ON attendance_events
         WHEN NEW.kind = 'clock_in' BEGIN SELECT RAISE(ABORT, 'injected'); END",
    )
    .await;
    clock_in(&app, &alice.token)
        .await
        .expect_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    assert_eq!(
        stored_records(&app, alice.id, ("2026-03-01", "2026-03-31")).await,
        0
    );

    execute(&app, "DROP TRIGGER fail_clock_in").await;
    clock_in(&app, &alice.token).await.expect(StatusCode::OK);
}

#[tokio::test]
async fn failure_at_audit_rolls_back_admin_create_entirely() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    app.set_time(&kst("2026-03-04T12:00:00"));
    execute(
        &app,
        "CREATE TRIGGER fail_audit BEFORE INSERT ON audit_log
         WHEN NEW.action = 'attendance.create' BEGIN SELECT RAISE(ABORT, 'injected'); END",
    )
    .await;
    app.post("/admin/attendance")
        .bearer(&org.admin.token)
        .json(&create_body(
            org.bob.id,
            "2026-03-03",
            "2026-03-03T09:00:00",
            None,
        ))
        .send()
        .await
        .expect_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");

    assert_eq!(
        stored_records(&app, org.bob.id, ("2026-03-01", "2026-03-31")).await,
        0
    );
    let mut conn = app.state.db.read().await.expect("conn");
    let (event_count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM attendance_events")
        .fetch_one(&mut *conn)
        .await
        .expect("count");
    assert_eq!(event_count, 0, "이력도 남지 않는다");
    drop(conn);
    assert!(!audit_actions(&app)
        .await
        .contains(&"attendance.create".to_owned()));
}

#[tokio::test]
async fn failure_at_audit_rolls_back_admin_edit() {
    let fx = edit_fixture().await;
    execute(
        &fx.app,
        "CREATE TRIGGER fail_audit BEFORE INSERT ON audit_log
         WHEN NEW.action = 'attendance.edit' BEGIN SELECT RAISE(ABORT, 'injected'); END",
    )
    .await;
    fx.patch(&fx.body("2026-03-02T09:00:00", Some("2026-03-02T18:00:00")))
        .await
        .expect_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    assert_eq!(fx.history_len().await, 1);
    let records = month(&fx.app, &fx.org.alice.token, "2026-03").await;
    assert_eq!(day(&records, "2026-03-02")["record"], fx.record);
}

// ── 근태 추적 기간: max(입사일, 계정 생성일) ~ 퇴사일 ─────────────────────

fn status_of(records: &Value, date: &str) -> String {
    day(records, date)["evaluation"]["status"]
        .as_str()
        .expect("status")
        .to_owned()
}

async fn summary_of(app: &TestApp, token: &str, query: &str) -> Value {
    app.get(&format!("/attendance/summary?{query}"))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

#[tokio::test]
async fn account_created_and_hired_today_has_no_absence_before_today() {
    let app = TestApp::new().await;
    // 03-16(월) 10:00에 계정을 만들고 같은 날 입사.
    app.set_time(&kst("2026-03-16T10:00:00"));
    let fresh = app
        .add_member_created_at("fresh", Role::Member, None, app.state.now())
        .await;
    update_member(&app, fresh.id, |m| {
        m.hired_on = "2026-03-16".parse().expect("d")
    })
    .await;

    let records = month(&app, &fresh.token, "2026-03").await;
    for d in 1..=15 {
        let date = format!("2026-03-{d:02}");
        assert_eq!(status_of(&records, &date), "day_off", "{date}");
    }
    assert_eq!(status_of(&records, "2026-03-16"), "scheduled");
    assert_eq!(status_of(&records, "2026-03-17"), "scheduled");
    let summary = summary_of(&app, &fresh.token, "month=2026-03").await;
    assert_eq!(summary["absent_days"], 0);
    assert_eq!(
        summary["day_off_days"],
        15 + 4,
        "추적 전 15일 + 이후 주말 4일"
    );

    assert_eq!(
        today(&app, &fresh.token).await["evaluation"]["status"],
        "scheduled"
    );
    clock_in(&app, &fresh.token).await.expect(StatusCode::OK);
    let now = today(&app, &fresh.token).await;
    assert_eq!(now["evaluation"]["status"], "working");
    assert_eq!(
        now["evaluation"]["late_minutes"], 60,
        "오늘은 추적 기간 안이다"
    );
}

#[tokio::test]
async fn days_before_account_creation_are_not_absent_even_if_hired_long_ago() {
    let app = TestApp::new().await;
    app.set_time(&kst("2026-03-20T12:00:00"));
    // 입사 2024-03-02, 계정은 03-10(화) 10:00 KST에 생성.
    let late = app
        .add_member_created_at(
            "late",
            Role::Member,
            None,
            pango_server::testing::ts(&kst("2026-03-10T10:00:00")),
        )
        .await;

    let records = month(&app, &late.token, "2026-03").await;
    for date in ["2026-03-02", "2026-03-06", "2026-03-09"] {
        assert_eq!(status_of(&records, date), "day_off", "{date}");
    }
    for date in ["2026-03-10", "2026-03-13", "2026-03-19"] {
        assert_eq!(status_of(&records, date), "absent", "{date}");
    }
    assert_eq!(status_of(&records, "2026-03-20"), "scheduled");
    let summary = summary_of(&app, &late.token, "month=2026-03").await;
    // 03-10~13, 03-16~19.
    assert_eq!(summary["absent_days"], 8);
}

#[tokio::test]
async fn account_creation_date_uses_the_company_calendar_date() {
    let app = TestApp::new().await;
    // 2026-03-09T16:00Z = 03-10 01:00 KST. 근무일 전환(05:00) 전이지만 달력 날짜로 03-10부터다.
    let member = app
        .add_member_created_at(
            "boundary",
            Role::Member,
            None,
            pango_server::testing::ts("2026-03-09T16:00:00Z"),
        )
        .await;
    app.set_time(&kst("2026-03-12T12:00:00"));
    let records = month(&app, &member.token, "2026-03").await;
    assert_eq!(status_of(&records, "2026-03-09"), "day_off");
    assert_eq!(status_of(&records, "2026-03-10"), "absent");
    assert_eq!(status_of(&records, "2026-03-11"), "absent");
}

#[tokio::test]
async fn record_before_tracking_starts_is_still_evaluated_and_shown() {
    let app = TestApp::new().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let late = app
        .add_member_created_at(
            "late",
            Role::Member,
            None,
            pango_server::testing::ts(&kst("2026-03-10T10:00:00")),
        )
        .await;
    app.set_time(&kst("2026-03-12T12:00:00"));
    app.post("/admin/attendance")
        .bearer(&admin.token)
        .json(&create_body(
            late.id,
            "2026-03-05",
            "2026-03-05T10:00:00",
            Some("2026-03-05T19:00:00"),
        ))
        .send()
        .await
        .expect(StatusCode::OK);

    let records = month(&app, &late.token, "2026-03").await;
    let entry = day(&records, "2026-03-05");
    assert_eq!(entry["record"]["work_date"], "2026-03-05");
    assert_eq!(entry["evaluation"]["status"], "normal");
    assert_eq!(entry["evaluation"]["worked_minutes"], 540 - 60);
    assert_eq!(entry["evaluation"]["late_minutes"], 0);
    assert_eq!(status_of(&records, "2026-03-04"), "day_off");

    let summary = summary_of(&app, &late.token, "month=2026-03").await;
    assert_eq!(summary["normal_days"], 1);
    assert_eq!(summary["absent_days"], 2, "03-10, 03-11");
    assert_eq!(summary["total_worked_minutes"], 480);

    let list = app
        .get("/admin/attendance?from=2026-03-01&to=2026-03-31")
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let items = list["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["evaluation"]["status"], "normal");
    assert_eq!(items[0]["evaluation"]["worked_minutes"], 480);
}

#[tokio::test]
async fn days_after_retirement_are_not_absent_in_admin_views() {
    let app = TestApp::new().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let alice = app.add_member("alice", Role::Member, None).await;
    update_member(&app, alice.id, |m| {
        m.status = MemberStatus::Retired;
        m.retired_on = Some("2026-03-05".parse().expect("d"));
    })
    .await;
    app.set_time(&kst("2026-03-12T12:00:00"));
    // 퇴사 후 날짜(03-09)의 기록: 10:00 출근이지만 근무일이 아니므로 지각이 아니다.
    for date in ["2026-03-04", "2026-03-09"] {
        app.post("/admin/attendance")
            .bearer(&admin.token)
            .json(&create_body(
                alice.id,
                date,
                &format!("{date}T10:00:00"),
                Some(&format!("{date}T19:00:00")),
            ))
            .send()
            .await
            .expect(StatusCode::OK);
    }

    let query = format!("month=2026-03&member_id={}", alice.id);
    let records = app
        .get(&format!("/attendance/records?{query}"))
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(status_of(&records, "2026-03-03"), "absent");
    assert_eq!(status_of(&records, "2026-03-04"), "late");
    assert_eq!(
        status_of(&records, "2026-03-05"),
        "absent",
        "퇴사일 당일은 추적한다"
    );
    for date in ["2026-03-06", "2026-03-10", "2026-03-11"] {
        assert_eq!(status_of(&records, date), "day_off", "{date}");
    }
    assert_eq!(status_of(&records, "2026-03-09"), "normal");
    let summary = summary_of(&app, &admin.token, &query).await;
    // 03-02, 03-03, 03-05.
    assert_eq!(summary["absent_days"], 3);
    assert_eq!(summary["late_days"], 1);
    assert_eq!(summary["normal_days"], 1);

    let list = app
        .get(&format!(
            "/admin/attendance?from=2026-03-01&to=2026-03-31&member_id={}",
            alice.id
        ))
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let statuses: Vec<_> = list["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| {
            (
                i["record"]["work_date"].as_str().expect("d").to_owned(),
                i["evaluation"]["status"].as_str().expect("s").to_owned(),
                i["evaluation"]["late_minutes"].as_u64().expect("late"),
            )
        })
        .collect();
    assert!(
        statuses.contains(&("2026-03-04".into(), "late".into(), 60)),
        "{statuses:?}"
    );
    assert!(
        statuses.contains(&("2026-03-09".into(), "normal".into(), 0)),
        "{statuses:?}"
    );

    let csv = app
        .get("/admin/attendance/export.csv?from=2026-03-01&to=2026-03-31")
        .bearer(&admin.token)
        .send()
        .await;
    assert_eq!(csv.status, StatusCode::OK);
    let body = csv.body.as_str().expect("csv").to_owned();
    let line = |date: &str| {
        body.lines()
            .find(|l| l.starts_with(date))
            .unwrap_or_else(|| panic!("no line for {date}: {body}"))
            .to_owned()
    };
    assert!(line("2026-03-04").ends_with(",late"), "{body}");
    assert!(line("2026-03-09").ends_with(",normal"), "{body}");
    assert!(!body.contains("absent"), "{body}");
}

#[tokio::test]
async fn home_and_dashboard_count_only_tracked_members_today() {
    let app = TestApp::new().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let bob = app.add_member("bob", Role::Member, None).await;
    let carol = app.add_member("carol", Role::Member, None).await;
    let dave = app.add_member("dave", Role::Member, None).await;
    // bob은 03-02 09:00에 출근하고 퇴근을 찍지 않는다.
    clock_in(&app, &bob.token).await.expect(StatusCode::OK);

    // 03-04(수) 10:00.
    app.set_time(&kst("2026-03-04T10:00:00"));
    let now = app.state.now();
    // 오늘 만들고 오늘 입사: 오늘은 추적한다.
    let fresh = app
        .add_member_created_at("fresh", Role::Member, None, now)
        .await;
    update_member(&app, fresh.id, |m| {
        m.hired_on = "2026-03-04".parse().expect("d")
    })
    .await;
    // 내일 입사 예정: 오늘은 추적하지 않는다.
    let early = app
        .add_member_created_at("early", Role::Member, None, now)
        .await;
    let idle = app
        .add_member_created_at("idle", Role::Member, None, now)
        .await;
    for m in [&early, &idle] {
        update_member(&app, m.id, |m| {
            m.hired_on = "2026-03-05".parse().expect("d")
        })
        .await;
    }
    clock_in(&app, &dave.token).await.expect(StatusCode::OK);
    clock_in(&app, &early.token).await.expect(StatusCode::OK);
    let _ = carol;

    let board = app
        .get("/admin/dashboard")
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    // 재직: owner, admin1, bob, carol, dave, fresh, early, idle.
    assert_eq!(board["today"]["active_members"], 8);
    assert_eq!(board["today"]["clocked_in"], 2, "dave, early");
    assert_eq!(
        board["today"]["late"], 1,
        "dave만. early는 추적 전이라 지각이 아니다"
    );
    // owner, admin1, bob, carol, fresh. idle은 추적 전이라 빠진다.
    assert_eq!(board["today"]["not_clocked_in"], 5);
    let missing = board["missing_clock_out"].as_array().expect("missing");
    assert_eq!(missing.len(), 1, "{missing:?}");
    assert_eq!(missing[0]["member_id"], bob.id.to_string());
    assert_eq!(missing[0]["dates"], json!(["2026-03-02"]));

    let home = app
        .get("/home")
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(home["team_today"]["total"], 8);
    assert_eq!(home["team_today"]["present"], 2);
    assert_eq!(home["team_today"]["not_yet"], 5);

    let fresh_home = app
        .get("/home")
        .bearer(&fresh.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(fresh_home["today"]["evaluation"]["status"], "scheduled");
    let early_home = app
        .get("/home")
        .bearer(&early.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(early_home["today"]["evaluation"]["status"], "working");
    assert_eq!(early_home["today"]["evaluation"]["late_minutes"], 0);
}

// ── 관리자의 본인 기록 ────────────────────────────────────────────────────

/// 관리자는 자기 출퇴근 기록을 직접 만들거나 고칠 수 없다(다른 관리자나 최고 관리자가 한다).
/// 최고 관리자는 최종 권한자이므로 본인 기록도 다루되 감사 로그에 남는다.
#[tokio::test]
async fn admin_cannot_create_or_edit_their_own_record_but_the_owner_can() {
    let app = TestApp::new().await;
    let org = org(&app).await;
    let own = clock_in(&app, &org.admin.token)
        .await
        .expect(StatusCode::OK);
    app.set_time(&kst("2026-03-04T12:00:00"));
    let admin_id = org.admin.id;
    let before = (
        stored_records(&app, admin_id, ("2026-03-01", "2026-03-31")).await,
        audit_actions(&app).await,
    );

    // 만들기: 본인 기록은 403.
    let body = create_body(admin_id, "2026-03-03", "2026-03-03T09:00:00", None);
    app.post("/admin/attendance")
        .bearer(&org.admin.token)
        .json(&body)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    // 고치기: 본인 기록은 403.
    let edit = json!({
        "version": own["version"],
        "clock_in_at": millis("2026-03-02T08:00:00"),
        "clock_out_at": millis("2026-03-02T18:00:00"),
        "work_type": "office",
        "reason": "내 지각 지우기",
    });
    app.patch(&format!("/admin/attendance/{}", id_of(&own)))
        .bearer(&org.admin.token)
        .json(&edit)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(
        (
            stored_records(&app, admin_id, ("2026-03-01", "2026-03-31")).await,
            audit_actions(&app).await,
        ),
        before
    );
    let records = month(&app, &org.admin.token, "2026-03").await;
    assert_eq!(day(&records, "2026-03-02")["record"], own);
    assert_eq!(events(&app, &org.admin.token, &id_of(&own)).await.len(), 1);

    // 다른 관리자·최고 관리자는 그 관리자의 기록을 다룰 수 있다.
    let other_admin = app.add_member("admin2", Role::Admin, None).await;
    app.post("/admin/attendance")
        .bearer(&other_admin.token)
        .json(&body)
        .send()
        .await
        .expect(StatusCode::OK);
    app.patch(&format!("/admin/attendance/{}", id_of(&own)))
        .bearer(&org.owner)
        .json(&edit)
        .send()
        .await
        .expect(StatusCode::OK);
    // 관리자는 여전히 다른 사람의 기록을 다룬다.
    let for_bob = create_body(org.bob.id, "2026-03-03", "2026-03-03T09:00:00", None);
    app.post("/admin/attendance")
        .bearer(&org.admin.token)
        .json(&for_bob)
        .send()
        .await
        .expect(StatusCode::OK);

    // 최고 관리자는 본인 기록도 만들고 고칠 수 있고, 감사 로그에 남는다.
    let owner_body = create_body(app.owner_id, "2026-03-03", "2026-03-03T09:00:00", None);
    let created = app
        .post("/admin/attendance")
        .bearer(&org.owner)
        .json(&owner_body)
        .send()
        .await
        .expect(StatusCode::OK);
    let owner_edit = json!({
        "version": created["version"],
        "clock_in_at": millis("2026-03-03T09:00:00"),
        "clock_out_at": millis("2026-03-03T18:00:00"),
        "work_type": "office",
        "reason": "퇴근 누락",
    });
    app.patch(&format!("/admin/attendance/{}", id_of(&created)))
        .bearer(&org.owner)
        .json(&owner_edit)
        .send()
        .await
        .expect(StatusCode::OK);
    let mut conn = app.state.db.read().await.expect("conn");
    let (entries, _) = audit::list(&mut conn, &AuditFilter::default(), 200)
        .await
        .expect("audit");
    let owner_self: Vec<&str> = entries
        .iter()
        .filter(|e| e.actor_id == Some(app.owner_id))
        .filter(|e| {
            e.detail
                .as_ref()
                .is_some_and(|d| d["member_id"] == json!(app.owner_id))
        })
        .map(|e| e.action.as_str())
        .collect();
    assert!(owner_self.contains(&"attendance.create"));
    assert!(owner_self.contains(&"attendance.edit"));
}
