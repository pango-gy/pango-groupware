//! 출퇴근 저장소 통합 테스트.

use chrono::NaiveDate;
use pango_core::attendance::{AttendanceEventKind, WorkType};
use pango_core::authz::Role;
use pango_core::calendar::DateRange;
use pango_core::id::{AttendanceEventId, AttendanceRecordId, DepartmentId, MemberId};
use pango_core::member::MemberStatus;
use pango_core::time::Timestamp;
use pango_db::attendance::{self, AttendanceEvent, AttendanceRecord, RecordCursor, RecordFilter};
use pango_db::members::{self, Member};
use pango_db::org::{self, Department};
use pango_db::testing::TestDb;
use pango_db::Db;

const HOUR: i64 = 3_600_000;
/// 2026-03-02 00:00Z = 09:00 KST.
const MAR_2: i64 = 1_772_409_600_000;

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis).expect("timestamp")
}

fn date(value: &str) -> NaiveDate {
    value.parse().expect("date")
}

fn range(start: &str, end: &str) -> DateRange {
    DateRange::new(date(start), date(end)).expect("range")
}

async fn insert_member(db: &Db, login_id: &str, name: &str, dept: Option<DepartmentId>) -> Member {
    let member = Member {
        id: MemberId::new(),
        login_id: login_id.to_owned(),
        name: name.to_owned(),
        email: None,
        phone: None,
        department_id: dept,
        position_id: None,
        role: Role::Member,
        status: MemberStatus::Active,
        hired_on: date("2024-03-02"),
        retired_on: None,
        must_change_password: false,
        created_at: ts(1),
        version: 1,
    };
    let mut tx = db.begin_write().await.expect("begin");
    members::insert(&mut tx, &member, "hash", ts(1))
        .await
        .expect("member");
    tx.commit().await.expect("commit");
    member
}

async fn insert_department(db: &Db, name: &str) -> DepartmentId {
    let department = Department {
        id: DepartmentId::new(),
        parent_id: None,
        name: name.to_owned(),
        head_member_id: None,
        sort_order: 0,
        archived_at: None,
        version: 1,
    };
    let mut tx = db.begin_write().await.expect("begin");
    org::insert_department(&mut tx, &department, ts(1))
        .await
        .expect("department");
    tx.commit().await.expect("commit");
    department.id
}

fn record(member_id: MemberId, work_date: &str, clock_in: i64) -> AttendanceRecord {
    AttendanceRecord {
        id: AttendanceRecordId::new(),
        member_id,
        work_date: date(work_date),
        clock_in_at: ts(clock_in),
        clock_out_at: None,
        work_type: WorkType::Office,
        note: None,
        version: 1,
    }
}

async fn insert_record(db: &Db, record: &AttendanceRecord) {
    let mut tx = db.begin_write().await.expect("begin");
    attendance::insert(&mut tx, record, ts(1))
        .await
        .expect("insert");
    tx.commit().await.expect("commit");
}

fn event(record: &AttendanceRecord, kind: AttendanceEventKind, at: i64) -> AttendanceEvent {
    AttendanceEvent {
        id: AttendanceEventId::new(),
        record_id: record.id,
        member_id: record.member_id,
        kind,
        before: None,
        after: serde_json::json!({ "clock_in_at": record.clock_in_at }),
        reason: None,
        actor_id: record.member_id,
        created_at: ts(at),
    }
}

#[tokio::test]
async fn record_roundtrip_and_lookups() {
    let test = TestDb::new().await;
    let member = insert_member(&test, "kim", "김", None).await;
    let mut first = record(member.id, "2026-03-02", MAR_2);
    first.note = Some("첫 출근".into());
    first.work_type = WorkType::Remote;
    let second = record(member.id, "2026-03-03", MAR_2 + 24 * HOUR);
    insert_record(&test, &first).await;
    insert_record(&test, &second).await;

    let mut conn = test.read().await.expect("conn");
    assert_eq!(
        attendance::find(&mut conn, first.id).await.expect("find"),
        Some(first.clone())
    );
    assert_eq!(
        attendance::find_by_member_date(&mut conn, member.id, date("2026-03-03"))
            .await
            .expect("by date"),
        Some(second.clone())
    );
    assert_eq!(
        attendance::find_by_member_date(&mut conn, member.id, date("2026-03-04"))
            .await
            .expect("by date"),
        None
    );
    assert_eq!(
        attendance::latest_for_member(&mut conn, member.id)
            .await
            .expect("latest"),
        Some(second.clone()),
        "가장 최근 출근 시각의 기록"
    );
    let other = insert_member(&test, "lee", "이", None).await;
    assert_eq!(
        attendance::latest_for_member(&mut conn, other.id)
            .await
            .expect("latest"),
        None
    );
    assert_eq!(
        attendance::find(&mut conn, AttendanceRecordId::new())
            .await
            .expect("find"),
        None
    );
}

#[tokio::test]
async fn one_record_per_member_and_work_date() {
    let test = TestDb::new().await;
    let member = insert_member(&test, "kim", "김", None).await;
    let other = insert_member(&test, "lee", "이", None).await;
    insert_record(&test, &record(member.id, "2026-03-02", MAR_2)).await;

    let mut tx = test.begin_write().await.expect("begin");
    let error = attendance::insert(
        &mut tx,
        &record(member.id, "2026-03-02", MAR_2 + HOUR),
        ts(1),
    )
    .await
    .expect_err("duplicate");
    assert!(attendance::is_duplicate_work_date(&error), "{error}");
    // 위반 뒤에도 같은 트랜잭션에서 기존 기록을 읽을 수 있다(서비스가 이 기록을 돌려준다).
    let existing = attendance::find_by_member_date(&mut tx, member.id, date("2026-03-02"))
        .await
        .expect("find");
    assert_eq!(existing.map(|r| r.clock_in_at), Some(ts(MAR_2)));

    // 다른 날짜, 다른 구성원은 문제없다.
    attendance::insert(
        &mut tx,
        &record(member.id, "2026-03-03", MAR_2 + 24 * HOUR),
        ts(1),
    )
    .await
    .expect("other date");
    attendance::insert(&mut tx, &record(other.id, "2026-03-02", MAR_2), ts(1))
        .await
        .expect("other member");
    tx.commit().await.expect("commit");
}

#[tokio::test]
async fn check_constraints_reject_inverted_times_and_unknown_work_type() {
    let test = TestDb::new().await;
    let member = insert_member(&test, "kim", "김", None).await;
    let mut inverted = record(member.id, "2026-03-02", MAR_2);
    inverted.clock_out_at = Some(ts(MAR_2 - 1));
    let mut tx = test.begin_write().await.expect("begin");
    assert!(attendance::insert(&mut tx, &inverted, ts(1)).await.is_err());

    let result = sqlx::query(
        "INSERT INTO attendance_records
            (id, member_id, work_date, clock_in_at, work_type, created_at, updated_at)
         VALUES (?, ?, '2026-03-04', 1, 'beach', 1, 1)",
    )
    .bind(AttendanceRecordId::new().to_string())
    .bind(member.id.to_string())
    .execute(&mut *tx)
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn close_only_closes_open_records() {
    let test = TestDb::new().await;
    let member = insert_member(&test, "kim", "김", None).await;
    let open = record(member.id, "2026-03-02", MAR_2);
    insert_record(&test, &open).await;

    let mut tx = test.begin_write().await.expect("begin");
    assert!(
        attendance::close(&mut tx, open.id, ts(MAR_2 + 9 * HOUR), ts(2))
            .await
            .expect("close")
    );
    assert!(
        !attendance::close(&mut tx, open.id, ts(MAR_2 + 10 * HOUR), ts(3))
            .await
            .expect("close again"),
        "이미 닫힌 기록은 덮어쓰지 않는다"
    );
    let closed = attendance::find(&mut tx, open.id)
        .await
        .expect("find")
        .expect("record");
    assert_eq!(closed.clock_out_at, Some(ts(MAR_2 + 9 * HOUR)));
    assert_eq!(closed.version, 2);
}

#[tokio::test]
async fn update_requires_matching_version() {
    let test = TestDb::new().await;
    let member = insert_member(&test, "kim", "김", None).await;
    let original = record(member.id, "2026-03-02", MAR_2);
    insert_record(&test, &original).await;

    let mut edited = original.clone();
    edited.clock_out_at = Some(ts(MAR_2 + 8 * HOUR));
    edited.work_type = WorkType::Field;
    let mut tx = test.begin_write().await.expect("begin");
    assert!(attendance::update(&mut tx, &edited, ts(2))
        .await
        .expect("update"));
    assert!(
        !attendance::update(&mut tx, &edited, ts(3))
            .await
            .expect("stale"),
        "같은 버전으로 두 번 고칠 수 없다"
    );
    let stored = attendance::find(&mut tx, original.id)
        .await
        .expect("find")
        .expect("record");
    assert_eq!(
        stored,
        AttendanceRecord {
            version: 2,
            ..edited
        }
    );
}

#[tokio::test]
async fn events_are_listed_in_order_and_append_only() {
    let test = TestDb::new().await;
    let member = insert_member(&test, "kim", "김", None).await;
    let rec = record(member.id, "2026-03-02", MAR_2);
    insert_record(&test, &rec).await;
    let first = event(&rec, AttendanceEventKind::ClockIn, 10);
    let mut second = event(&rec, AttendanceEventKind::AdminEdit, 20);
    second.before = Some(serde_json::json!({ "clock_out_at": null }));
    second.reason = Some("누락".into());

    let mut tx = test.begin_write().await.expect("begin");
    attendance::insert_event(&mut tx, &second)
        .await
        .expect("event");
    attendance::insert_event(&mut tx, &first)
        .await
        .expect("event");
    tx.commit().await.expect("commit");

    let mut conn = test.read().await.expect("conn");
    let events = attendance::list_events(&mut conn, rec.id)
        .await
        .expect("events");
    assert_eq!(events, vec![first.clone(), second]);
    assert!(
        attendance::list_events(&mut conn, AttendanceRecordId::new())
            .await
            .expect("none")
            .is_empty()
    );
    drop(conn);

    let mut tx = test.begin_write().await.expect("begin");
    let update = sqlx::query("UPDATE attendance_events SET reason = 'x' WHERE id = ?")
        .bind(first.id.to_string())
        .execute(&mut *tx)
        .await;
    assert!(update.is_err(), "이력은 고칠 수 없다");
    let delete = sqlx::query("DELETE FROM attendance_events")
        .execute(&mut *tx)
        .await;
    assert!(delete.is_err(), "이력은 지울 수 없다");
}

#[tokio::test]
async fn list_for_members_is_scoped_by_range_and_members() {
    let test = TestDb::new().await;
    let kim = insert_member(&test, "kim", "김", None).await;
    let lee = insert_member(&test, "lee", "이", None).await;
    let park = insert_member(&test, "park", "박", None).await;
    for (member, day, offset) in [
        (&kim, "2026-02-28", -2),
        (&kim, "2026-03-01", -1),
        (&kim, "2026-03-31", 29),
        (&kim, "2026-04-01", 30),
        (&lee, "2026-03-02", 0),
        (&park, "2026-03-02", 0),
    ] {
        insert_record(&test, &record(member.id, day, MAR_2 + offset * 24 * HOUR)).await;
    }
    let mut conn = test.read().await.expect("conn");
    let march = range("2026-03-01", "2026-03-31");
    let kims = attendance::list_for_members(&mut conn, &[kim.id], march)
        .await
        .expect("list");
    let dates: Vec<_> = kims.iter().map(|r| r.work_date).collect();
    assert_eq!(dates, vec![date("2026-03-01"), date("2026-03-31")]);

    let both = attendance::list_for_members(&mut conn, &[kim.id, lee.id], march)
        .await
        .expect("list");
    assert_eq!(both.len(), 3);
    assert!(both.iter().all(|r| r.member_id != park.id));
    assert!(attendance::list_for_members(&mut conn, &[], march)
        .await
        .expect("empty")
        .is_empty());
}

#[tokio::test]
async fn admin_page_filters_and_paginates_newest_first() {
    let test = TestDb::new().await;
    let sales = insert_department(&test, "영업").await;
    let dev = insert_department(&test, "개발").await;
    let kim = insert_member(&test, "kim", "김", Some(sales)).await;
    let lee = insert_member(&test, "lee", "이", Some(dev)).await;
    for day in 0..5 {
        let work_date = date("2026-03-02") + chrono::Duration::days(day);
        for member in [&kim, &lee] {
            let mut rec = record(member.id, "2026-03-02", MAR_2 + day * 24 * HOUR);
            rec.work_date = work_date;
            insert_record(&test, &rec).await;
        }
    }
    let mut conn = test.read().await.expect("conn");
    let all = RecordFilter {
        range: range("2026-03-01", "2026-03-31"),
        member_id: None,
        departments: None,
    };
    let (first, more) = attendance::list_page(&mut conn, &all, None, 4)
        .await
        .expect("page");
    assert!(more);
    assert_eq!(first.len(), 4);
    assert_eq!(first[0].record.work_date, date("2026-03-06"));
    assert_eq!(
        first[0]
            .department_name
            .as_deref()
            .map(|n| n == "영업" || n == "개발"),
        Some(true)
    );
    let last = &first[3].record;
    let cursor = RecordCursor {
        work_date: last.work_date,
        id: last.id,
    };
    let (rest, more) = attendance::list_page(&mut conn, &all, Some(cursor), 100)
        .await
        .expect("page");
    assert!(!more);
    assert_eq!(rest.len(), 6);
    let mut seen: Vec<_> = first.iter().chain(&rest).map(|r| r.record.id).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 10, "페이지가 겹치거나 빠지지 않는다");

    let only_sales = RecordFilter {
        departments: Some(vec![sales]),
        ..all.clone()
    };
    let (rows, _) = attendance::list_page(&mut conn, &only_sales, None, 100)
        .await
        .expect("page");
    assert_eq!(rows.len(), 5);
    assert!(rows
        .iter()
        .all(|r| r.record.member_id == kim.id && r.login_id == "kim"));

    let only_lee = RecordFilter {
        member_id: Some(lee.id),
        range: range("2026-03-03", "2026-03-04"),
        departments: None,
    };
    let (rows, _) = attendance::list_page(&mut conn, &only_lee, None, 100)
        .await
        .expect("page");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.member_name == "이"));

    let no_departments = RecordFilter {
        departments: Some(vec![]),
        ..all.clone()
    };
    let (rows, more) = attendance::list_page(&mut conn, &no_departments, None, 100)
        .await
        .expect("page");
    assert!(rows.is_empty() && !more);

    let export = attendance::list_for_export(&mut conn, &all)
        .await
        .expect("export");
    assert_eq!(export.len(), 10);
    assert_eq!(export[0].record.work_date, date("2026-03-02"));
    assert_eq!(export[0].member_name, "김", "같은 날은 이름순(가나다)");
}
