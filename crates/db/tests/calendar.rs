//! 캘린더 저장소 통합 테스트.

use chrono::NaiveDate;
use pango_core::authz::Role;
use pango_core::calendar_access::{Calendar, COMPANY_CALENDAR_ID};
use pango_core::calendar_event::{AttendeeResponse, EventTime};
use pango_core::id::{CalendarEventId, MemberId};
use pango_core::member::MemberStatus;
use pango_core::time::Timestamp;
use pango_db::calendar::{self, CalendarEvent, EventWindow};
use pango_db::members::{self, Member};
use pango_db::testing::TestDb;
use pango_db::Db;

const DAY: i64 = 86_400_000;

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis).expect("timestamp")
}

fn d(value: &str) -> NaiveDate {
    value.parse().expect("date")
}

async fn insert_member(db: &Db, login_id: &str) -> MemberId {
    let member = Member {
        id: MemberId::new(),
        login_id: login_id.to_owned(),
        name: format!("이름-{login_id}"),
        email: None,
        phone: None,
        department_id: None,
        position_id: None,
        role: Role::Member,
        status: MemberStatus::Active,
        hired_on: d("2024-03-02"),
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
    member.id
}

fn event(owner: MemberId, calendar: Calendar, time: EventTime) -> CalendarEvent {
    CalendarEvent {
        id: CalendarEventId::new(),
        calendar,
        owner_id: owner,
        title: "회의".into(),
        time,
        location: Some("3층".into()),
        description: None,
        created_at: ts(1),
        updated_at: ts(1),
        version: 1,
    }
}

fn all_day(start: &str, end: &str) -> EventTime {
    EventTime::AllDay {
        start: d(start),
        end: d(end),
    }
}

fn timed(start: i64, end: i64) -> EventTime {
    EventTime::Timed {
        start: ts(start),
        end: ts(end),
    }
}

async fn save(db: &Db, event: &CalendarEvent) {
    let mut tx = db.begin_write().await.expect("begin");
    calendar::ensure_calendar(&mut tx, &event.calendar, ts(1))
        .await
        .expect("calendar");
    calendar::insert_event(&mut tx, event)
        .await
        .expect("insert");
    tx.commit().await.expect("commit");
}

/// 2026-03 한 달(UTC 기준 경계).
fn march() -> EventWindow {
    EventWindow {
        first_day: d("2026-03-01"),
        last_day: d("2026-03-31"),
        starts: ts(1_772_323_200_000),          // 2026-03-01T00:00Z
        ends: ts(1_772_323_200_000 + 31 * DAY), // 2026-04-01T00:00Z
    }
}

#[tokio::test]
async fn migration_creates_the_company_calendar_once() {
    let db = TestDb::new().await;
    let mut tx = db.begin_write().await.expect("begin");
    assert!(calendar::calendar_exists(&mut tx, COMPANY_CALENDAR_ID)
        .await
        .expect("exists"));
    calendar::ensure_calendar(&mut tx, &Calendar::Company, ts(5))
        .await
        .expect("ensure");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM calendars WHERE kind = 'company'")
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    assert_eq!(count, 1);
    // 다른 ID로 두 번째 전사 캘린더를 만들 수 없다.
    let second = sqlx::query(
        "INSERT INTO calendars (id, kind, created_at)
         VALUES ('00000000-0000-7000-8000-000000000002', 'company', 1)",
    )
    .execute(&mut *tx)
    .await;
    assert!(second.is_err());
}

#[tokio::test]
async fn personal_calendar_rows_are_created_lazily_and_idempotently() {
    let db = TestDb::new().await;
    let owner = insert_member(&db, "owner").await;
    let personal = Calendar::Personal(owner);
    let mut tx = db.begin_write().await.expect("begin");
    assert!(!calendar::calendar_exists(&mut tx, personal.id())
        .await
        .expect("exists"));
    for _ in 0..2 {
        calendar::ensure_calendar(&mut tx, &personal, ts(1))
            .await
            .expect("ensure");
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM calendars WHERE owner_id = ?")
        .bind(owner.to_string())
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    assert_eq!(count, 1);
    // 캘린더 ID가 주인과 어긋난 행은 제약이 막는다.
    let mismatched = sqlx::query(
        "INSERT INTO calendars (id, kind, owner_id, created_at) VALUES (?, 'personal', ?, 1)",
    )
    .bind(MemberId::new().to_string())
    .bind(owner.to_string())
    .execute(&mut *tx)
    .await;
    assert!(mismatched.is_err());
}

#[tokio::test]
async fn schema_rejects_mixed_all_day_and_timed_columns() {
    let db = TestDb::new().await;
    let owner = insert_member(&db, "owner").await;
    save(
        &db,
        &event(
            owner,
            Calendar::Personal(owner),
            all_day("2026-03-02", "2026-03-02"),
        ),
    )
    .await;
    let mut tx = db.begin_write().await.expect("begin");
    let insert = |all_day: i64,
                  start_date: Option<&'static str>,
                  end_date: Option<&'static str>,
                  start_at: Option<i64>,
                  end_at: Option<i64>| {
        sqlx::query(
            "INSERT INTO calendar_events
                (id, calendar_id, owner_id, title, all_day, start_date, end_date, start_at,
                 end_at, created_at, updated_at)
             VALUES (?, ?, ?, 't', ?, ?, ?, ?, ?, 1, 1)",
        )
        .bind(CalendarEventId::new().to_string())
        .bind(owner.to_string())
        .bind(owner.to_string())
        .bind(all_day)
        .bind(start_date)
        .bind(end_date)
        .bind(start_at)
        .bind(end_at)
    };
    let bad = [
        insert(1, Some("2026-03-02"), Some("2026-03-02"), Some(1), None),
        insert(1, Some("2026-03-03"), Some("2026-03-02"), None, None),
        insert(1, Some("2026-3-2"), Some("2026-03-02"), None, None),
        insert(0, None, None, Some(5), Some(5)),
        insert(0, Some("2026-03-02"), None, Some(1), Some(2)),
        insert(0, None, None, Some(1), None),
    ];
    for query in bad {
        assert!(query.execute(&mut *tx).await.is_err());
    }
    assert!(insert(0, None, None, Some(1), Some(2))
        .execute(&mut *tx)
        .await
        .is_ok());
}

#[tokio::test]
async fn update_and_delete_require_the_current_version() {
    let db = TestDb::new().await;
    let owner = insert_member(&db, "owner").await;
    let guest = insert_member(&db, "guest").await;
    let original = event(owner, Calendar::Personal(owner), timed(0, 3_600_000));
    save(&db, &original).await;
    let mut tx = db.begin_write().await.expect("begin");
    assert!(calendar::add_attendee(&mut tx, original.id, guest, ts(2))
        .await
        .expect("add"));
    assert!(!calendar::add_attendee(&mut tx, original.id, guest, ts(3))
        .await
        .expect("add again"));

    let changed = CalendarEvent {
        title: "바뀐 회의".into(),
        time: all_day("2026-03-02", "2026-03-04"),
        ..original.clone()
    };
    assert!(calendar::update_event(&mut tx, &changed, ts(9))
        .await
        .expect("update"));
    assert!(!calendar::update_event(&mut tx, &changed, ts(10))
        .await
        .expect("stale update"));
    let stored = calendar::find_event(&mut tx, original.id)
        .await
        .expect("find")
        .expect("event");
    assert_eq!(stored.version, 2);
    assert_eq!(stored.title, "바뀐 회의");
    assert_eq!(stored.time, all_day("2026-03-02", "2026-03-04"));
    assert_eq!(stored.updated_at, ts(9));

    assert!(!calendar::delete_event(&mut tx, original.id, 1)
        .await
        .expect("stale delete"));
    assert!(calendar::delete_event(&mut tx, original.id, 2)
        .await
        .expect("delete"));
    assert!(calendar::find_event(&mut tx, original.id)
        .await
        .expect("find")
        .is_none());
    let attendees: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM calendar_attendees")
        .fetch_one(&mut *tx)
        .await
        .expect("count");
    assert_eq!(attendees, 0, "attendees are removed with the event");
}

#[tokio::test]
async fn list_events_finds_overlaps_once_and_invited_events() {
    let db = TestDb::new().await;
    let owner = insert_member(&db, "owner").await;
    let guest = insert_member(&db, "guest").await;
    let mine = Calendar::Personal(owner);
    let march_start = march().starts.as_millis();
    let crossing = event(owner, mine, all_day("2026-02-27", "2026-03-02"));
    let inside = event(
        owner,
        mine,
        timed(march_start + DAY, march_start + DAY + 3_600_000),
    );
    let ends_at_boundary = event(owner, mine, timed(march_start - 3_600_000, march_start));
    let after = event(owner, mine, all_day("2026-04-01", "2026-04-01"));
    let company = event(
        owner,
        Calendar::Company,
        all_day("2026-03-31", "2026-04-02"),
    );
    for e in [&crossing, &inside, &ends_at_boundary, &after, &company] {
        save(&db, e).await;
    }
    let mut tx = db.begin_write().await.expect("begin");
    let found = calendar::list_events(&mut tx, &[mine.id()], None, &march())
        .await
        .expect("list");
    let mut ids: Vec<_> = found.iter().map(|e| e.id).collect();
    let mut expected = vec![crossing.id, inside.id];
    ids.sort();
    expected.sort();
    assert_eq!(ids, expected);

    // 초대받은 일정은 캘린더를 고르지 않아도 나온다.
    calendar::add_attendee(&mut tx, inside.id, guest, ts(2))
        .await
        .expect("add");
    let invited = calendar::list_events(&mut tx, &[], Some(guest), &march())
        .await
        .expect("list");
    assert_eq!(
        invited.iter().map(|e| e.id).collect::<Vec<_>>(),
        [inside.id]
    );
    let both = calendar::list_events(&mut tx, &[COMPANY_CALENDAR_ID], Some(guest), &march())
        .await
        .expect("list");
    assert_eq!(both.len(), 2);
    assert_eq!(
        both.iter().find(|e| e.id == company.id).map(|e| e.calendar),
        Some(Calendar::Company)
    );
}

#[tokio::test]
async fn busy_events_cover_own_and_accepted_invitations_but_not_declined() {
    let db = TestDb::new().await;
    let host = insert_member(&db, "host").await;
    let guest = insert_member(&db, "guest").await;
    let start = march().starts.as_millis();
    let own = event(
        guest,
        Calendar::Personal(guest),
        timed(start, start + 3_600_000),
    );
    let invited = event(
        host,
        Calendar::Personal(host),
        timed(start + DAY, start + DAY + 60_000),
    );
    let declined = event(
        host,
        Calendar::Personal(host),
        all_day("2026-03-05", "2026-03-05"),
    );
    let company = event(host, Calendar::Company, all_day("2026-03-06", "2026-03-06"));
    for e in [&own, &invited, &declined, &company] {
        save(&db, e).await;
    }
    let mut tx = db.begin_write().await.expect("begin");
    for e in [&invited, &declined] {
        calendar::add_attendee(&mut tx, e.id, guest, ts(2))
            .await
            .expect("add");
    }
    assert!(calendar::set_response(
        &mut tx,
        declined.id,
        guest,
        AttendeeResponse::Declined,
        ts(3)
    )
    .await
    .expect("respond"));
    assert!(
        !calendar::set_response(&mut tx, own.id, host, AttendeeResponse::Accepted, ts(3))
            .await
            .expect("not an attendee")
    );

    let busy = calendar::busy_events(&mut tx, &[guest], &march())
        .await
        .expect("busy");
    let mut ids: Vec<_> = busy.iter().map(|(member, e)| (*member, e.id)).collect();
    ids.sort();
    let mut expected = vec![(guest, own.id), (guest, invited.id)];
    expected.sort();
    assert_eq!(ids, expected);
    // 주인에게는 자기 일정 셋이 모두 바쁜 시간이다(전사 일정은 아니다).
    let host_busy = calendar::busy_events(&mut tx, &[host], &march())
        .await
        .expect("busy");
    assert_eq!(host_busy.len(), 2);

    let attendees = calendar::list_attendees(&mut tx, declined.id)
        .await
        .expect("attendees");
    assert_eq!(attendees.len(), 1);
    assert_eq!(attendees[0].response, AttendeeResponse::Declined);
    assert_eq!(attendees[0].responded_at, Some(ts(3)));
    assert_eq!(attendees[0].name, "이름-guest");
    let attendance = calendar::attendance_of(&mut tx, &[invited.id, declined.id, own.id])
        .await
        .expect("attendance");
    assert_eq!(attendance.len(), 2);
}
