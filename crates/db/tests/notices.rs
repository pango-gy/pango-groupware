//! 공지 저장소 통합 테스트.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use pango_core::authz::{Audience, Role};
use pango_core::id::{DepartmentId, MemberId, NoticeId};
use pango_core::member::MemberStatus;
use pango_core::time::Timestamp;
use pango_db::members::{self, Member};
use pango_db::notices::{self, Notice, NoticeCursor, NoticeFilter};
use pango_db::org::{self, Department};
use pango_db::testing::TestDb;
use pango_db::Db;

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis).expect("timestamp")
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
        role: Role::Admin,
        status: MemberStatus::Active,
        hired_on: NaiveDate::from_ymd_opt(2024, 3, 2).expect("date"),
        retired_on: None,
        must_change_password: false,
        created_at: ts(1),
        deleted_at: None,
        version: 1,
    };
    let mut tx = db.begin_write().await.expect("begin");
    members::insert(&mut tx, &member, "hash", ts(1))
        .await
        .expect("member");
    tx.commit().await.expect("commit");
    member.id
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

fn notice(author_id: MemberId, audience: Audience, pinned: bool, published_at: i64) -> Notice {
    Notice {
        id: NoticeId::new(),
        title: format!("공지 {published_at}"),
        body: "본문".into(),
        author_id,
        pinned,
        must_read: false,
        audience,
        published_at: ts(published_at),
        archived_at: None,
        version: 1,
    }
}

async fn insert(db: &Db, notice: &Notice) {
    let mut tx = db.begin_write().await.expect("begin");
    notices::insert(&mut tx, notice, notice.published_at)
        .await
        .expect("insert");
    tx.commit().await.expect("commit");
}

fn everything(reader: MemberId) -> NoticeFilter<'static> {
    NoticeFilter {
        reader,
        visible_departments: None,
        include_archived: false,
    }
}

#[tokio::test]
async fn insert_find_and_versioned_update_round_trip() {
    let test = TestDb::new().await;
    let author = insert_member(&test, "author").await;
    let dept = insert_department(&test, "개발팀").await;
    let mut n = notice(author, Audience::Department(dept), true, 10);
    insert(&test, &n).await;

    let mut tx = test.begin_write().await.expect("begin");
    assert_eq!(
        notices::find(&mut tx, n.id).await.expect("find"),
        Some(n.clone())
    );
    n.title = "바뀐 제목".into();
    n.audience = Audience::All;
    assert!(notices::update(&mut tx, &n, ts(20)).await.expect("update"));
    n.title = "낡은 버전으로 덮어쓰기".into();
    assert!(!notices::update(&mut tx, &n, ts(30)).await.expect("stale"));
    let stored = notices::find(&mut tx, n.id)
        .await
        .expect("find")
        .expect("some");
    assert_eq!(
        (stored.title.as_str(), stored.audience, stored.version),
        ("바뀐 제목", Audience::All, 2)
    );
    assert_eq!(
        notices::find(&mut tx, NoticeId::new()).await.expect("find"),
        None
    );
}

#[tokio::test]
async fn department_audience_must_reference_existing_department() {
    let test = TestDb::new().await;
    let author = insert_member(&test, "author").await;
    let mut tx = test.begin_write().await.expect("begin");
    let ghost = notice(author, Audience::Department(DepartmentId::new()), false, 1);
    let err = notices::insert(&mut tx, &ghost, ts(1)).await.unwrap_err();
    assert!(err.is_foreign_key_violation(), "{err}");
}

#[tokio::test]
async fn list_orders_pinned_first_and_pages_without_gaps_or_duplicates() {
    let test = TestDb::new().await;
    let author = insert_member(&test, "author").await;
    // 같은 게시 시각이 섞인 고정 3건 + 일반 4건.
    let mut all = Vec::new();
    for (pinned, at) in [
        (true, 5),
        (false, 7),
        (true, 5),
        (false, 7),
        (false, 3),
        (true, 9),
        (false, 7),
    ] {
        let n = notice(author, Audience::All, pinned, at);
        insert(&test, &n).await;
        all.push(n);
    }
    all.sort_by(|a, b| (b.pinned, b.published_at, b.id).cmp(&(a.pinned, a.published_at, a.id)));
    let expected: Vec<_> = all.iter().map(|n| n.id).collect();

    let mut conn = test.reader().acquire().await.expect("conn");
    let mut seen = Vec::new();
    let mut after: Option<NoticeCursor> = None;
    loop {
        let (page, more) = notices::list(&mut conn, &everything(author), after, 3)
            .await
            .expect("list");
        assert!(page.len() <= 3);
        seen.extend(page.iter().map(|n| n.id));
        if !more {
            break;
        }
        after = page.last().map(|n| n.cursor());
    }
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn visibility_filter_includes_all_audience_and_listed_departments_only() {
    let test = TestDb::new().await;
    let author = insert_member(&test, "author").await;
    let (a, b) = (
        insert_department(&test, "A").await,
        insert_department(&test, "B").await,
    );
    let to_all = notice(author, Audience::All, false, 1);
    let to_a = notice(author, Audience::Department(a), false, 2);
    let to_b = notice(author, Audience::Department(b), false, 3);
    let mut archived = notice(author, Audience::All, false, 4);
    archived.archived_at = Some(ts(5));
    for n in [&to_all, &to_a, &to_b, &archived] {
        insert(&test, n).await;
    }

    let mut conn = test.reader().acquire().await.expect("conn");
    let ids = |items: Vec<notices::NoticeListItem>| -> BTreeSet<NoticeId> {
        items.into_iter().map(|n| n.id).collect()
    };
    let only_a = [a];
    let filter = NoticeFilter {
        reader: author,
        visible_departments: Some(&only_a),
        include_archived: false,
    };
    let (items, _) = notices::list(&mut conn, &filter, None, 50)
        .await
        .expect("list");
    assert_eq!(ids(items), BTreeSet::from([to_all.id, to_a.id]));

    let none: [DepartmentId; 0] = [];
    let filter = NoticeFilter {
        visible_departments: Some(&none),
        ..filter
    };
    let (items, _) = notices::list(&mut conn, &filter, None, 50)
        .await
        .expect("list");
    assert_eq!(ids(items), BTreeSet::from([to_all.id]));

    let admin = NoticeFilter {
        include_archived: true,
        ..everything(author)
    };
    let (items, _) = notices::list(&mut conn, &admin, None, 50)
        .await
        .expect("list");
    assert_eq!(
        ids(items),
        BTreeSet::from([to_all.id, to_a.id, to_b.id, archived.id])
    );
}

#[tokio::test]
async fn read_receipts_are_unique_and_keep_first_time() {
    let test = TestDb::new().await;
    let author = insert_member(&test, "author").await;
    let reader = insert_member(&test, "reader").await;
    let mut must = notice(author, Audience::All, false, 1);
    must.must_read = true;
    let plain = notice(author, Audience::All, false, 2);
    insert(&test, &must).await;
    insert(&test, &plain).await;

    let mut tx = test.begin_write().await.expect("begin");
    assert_eq!(
        notices::count_unread(&mut tx, &everything(reader))
            .await
            .expect("count"),
        (2, 1)
    );
    assert!(notices::mark_read(&mut tx, must.id, reader, ts(100))
        .await
        .expect("read"));
    assert!(!notices::mark_read(&mut tx, must.id, reader, ts(200))
        .await
        .expect("again"));
    assert_eq!(
        notices::count_unread(&mut tx, &everything(reader))
            .await
            .expect("count"),
        (1, 0)
    );
    let reads = notices::list_reads(&mut tx, must.id).await.expect("reads");
    assert_eq!(reads.len(), 1);
    assert_eq!((reads[0].member_id, reads[0].read_at), (reader, ts(100)));

    let view = notices::find_view(&mut tx, must.id, reader)
        .await
        .expect("view")
        .expect("some");
    assert_eq!(view.item.read_at, Some(ts(100)));
    assert_eq!(view.body, "본문");
    assert_eq!(view.item.author_name, "이름-author");
    let unread_view = notices::find_view(&mut tx, plain.id, reader)
        .await
        .expect("view")
        .expect("some");
    assert_eq!(unread_view.item.read_at, None);
}

// ── 알림 회수 ───────────────────────────────────────────────────────────

mod withdrawn_notifications {
    use pango_core::id::NotificationId;
    use pango_db::notifications::{self, Notification};

    use super::*;

    fn alert(member_id: MemberId, kind: &str, key: &str, read: bool) -> Notification {
        Notification {
            id: NotificationId::new(),
            member_id,
            kind: kind.to_owned(),
            title: "제목".to_owned(),
            body: String::new(),
            link: None,
            dedupe_key: Some(key.to_owned()),
            read_at: read.then(|| ts(50)),
            created_at: ts(10),
        }
    }

    async fn remaining(db: &Db, member_id: MemberId) -> Vec<(String, Option<String>, bool)> {
        let mut conn = db.read().await.expect("read");
        let mut items: Vec<_> = notifications::list(&mut conn, member_id, false, None, 200)
            .await
            .expect("list")
            .0
            .into_iter()
            .map(|n| (n.kind, n.dedupe_key, n.read_at.is_some()))
            .collect();
        items.sort();
        items
    }

    #[tokio::test]
    async fn delete_unread_removes_only_unread_rows_of_that_event() {
        let test = TestDb::new().await;
        let (a, b, c) = (
            insert_member(&test, "a").await,
            insert_member(&test, "b").await,
            insert_member(&test, "c").await,
        );
        let mut tx = test.begin_write().await.expect("begin");
        for row in [
            alert(a, "notice.new", "notice.new:1", false),
            alert(b, "notice.new", "notice.new:1", true),
            alert(c, "notice.new", "notice.new:1", false),
            // 다른 공지, 같은 키를 쓰는 다른 종류.
            alert(a, "notice.new", "notice.new:2", false),
            alert(c, "other.kind", "notice.new:1x", false),
        ] {
            assert!(notifications::insert(&mut tx, &row).await.expect("insert"));
        }
        tx.commit().await.expect("commit");

        let mut tx = test.begin_write().await.expect("begin");
        assert_eq!(
            notifications::unread_recipients(&mut tx, "notice.new", "notice.new:1")
                .await
                .expect("recipients")
                .into_iter()
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([a, c])
        );
        // 고른 구성원의 것만. 읽은 사람(b)과 목록에 없는 사람(c)은 그대로다.
        let deleted =
            notifications::delete_unread(&mut tx, "notice.new", "notice.new:1", Some(&[a, b]))
                .await
                .expect("delete");
        assert_eq!(deleted, 1);
        // 빈 목록은 아무것도 지우지 않는다(전부 지우는 `None`과 다르다).
        let none = notifications::delete_unread(&mut tx, "notice.new", "notice.new:1", Some(&[]))
            .await
            .expect("delete");
        assert_eq!(none, 0);
        tx.commit().await.expect("commit");
        assert_eq!(
            remaining(&test, a).await,
            [(
                "notice.new".to_owned(),
                Some("notice.new:2".to_owned()),
                false
            )]
        );
        assert_eq!(remaining(&test, c).await.len(), 2);

        // 전부: 남은 읽지 않은 것(c)만 지워지고 읽은 것(b)은 남는다.
        let mut tx = test.begin_write().await.expect("begin");
        let deleted = notifications::delete_unread(&mut tx, "notice.new", "notice.new:1", None)
            .await
            .expect("delete");
        assert_eq!(deleted, 1);
        assert!(
            notifications::unread_recipients(&mut tx, "notice.new", "notice.new:1")
                .await
                .expect("recipients")
                .is_empty()
        );
        tx.commit().await.expect("commit");
        assert_eq!(
            remaining(&test, b).await,
            [(
                "notice.new".to_owned(),
                Some("notice.new:1".to_owned()),
                true
            )]
        );
        assert_eq!(
            remaining(&test, c).await,
            [(
                "other.kind".to_owned(),
                Some("notice.new:1x".to_owned()),
                false
            )]
        );
        assert_eq!(remaining(&test, a).await.len(), 1);
    }
}
