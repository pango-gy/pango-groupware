//! 저장소 계층 통합 테스트 (계정·세션·조직·감사 로그·멱등성).

use std::collections::BTreeSet;

use chrono::NaiveDate;
use pango_core::authz::Role;
use pango_core::calendar::{DateRange, WorkWeek};
use pango_core::id::{AuditLogId, DepartmentId, MemberId, PositionId, SessionId};
use pango_core::member::MemberStatus;
use pango_core::time::{MinuteOfDay, Timestamp};
use pango_db::audit::{self, AuditEntry, AuditFilter};
use pango_db::idempotency::{self, StoredResponse};
use pango_db::members::{self, Member, MemberFilter};
use pango_db::org::{self, Department, Position};
use pango_db::sessions::{self, Session, SessionVia, ThrottleMode};
use pango_db::settings::{self, CompanySettings, Holiday};
use pango_db::testing::TestDb;
use pango_db::{Db, DbOptions};

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis).expect("timestamp")
}

fn date(value: &str) -> NaiveDate {
    value.parse().expect("date")
}

fn member(login_id: &str) -> Member {
    Member {
        id: MemberId::new(),
        login_id: login_id.to_owned(),
        name: format!("이름-{login_id}"),
        email: None,
        phone: None,
        department_id: None,
        position_id: None,
        role: Role::Member,
        status: MemberStatus::Active,
        hired_on: date("2024-03-02"),
        retired_on: None,
        must_change_password: false,
        created_at: ts(1),
        version: 1,
    }
}

async fn insert_member(db: &Db, login_id: &str) -> Member {
    let member = member(login_id);
    let mut tx = db.begin_write().await.expect("begin");
    members::insert(&mut tx, &member, "hash", ts(1))
        .await
        .expect("insert member");
    tx.commit().await.expect("commit");
    member
}

fn session(member_id: MemberId, now: i64) -> Session {
    Session {
        id: SessionId::new(),
        member_id,
        via: SessionVia::Cookie,
        user_agent: Some("test-agent".into()),
        ip: Some("127.0.0.1".into()),
        created_at: ts(now),
        last_seen_at: ts(now),
        idle_expires_at: ts(now + 1_000),
        absolute_expires_at: ts(now + 10_000),
        revoked_at: None,
    }
}

fn settings() -> CompanySettings {
    CompanySettings {
        company_name: "팡고".into(),
        timezone: "Asia/Seoul".into(),
        day_cutoff: MinuteOfDay::parse("05:00").expect("t"),
        work_start: MinuteOfDay::parse("09:00").expect("t"),
        work_end: MinuteOfDay::parse("18:00").expect("t"),
        late_grace_minutes: 0,
        half_day_split: MinuteOfDay::parse("14:00").expect("t"),
        quarter_minutes: 120,
        break_tiers: vec![(240, 30), (480, 60)],
        workdays: WorkWeek::MON_TO_FRI,
        version: 1,
        updated_at: ts(1),
    }
}

#[tokio::test]
async fn reopening_runs_migrations_idempotently_and_keeps_data() {
    let test = TestDb::new().await;
    let created = insert_member(&test, "gildong").await;
    test.close().await;

    let reopened = Db::open(&test.path(), DbOptions::default())
        .await
        .expect("reopen");
    let mut conn = reopened.reader().acquire().await.expect("conn");
    assert_eq!(
        members::find(&mut conn, created.id).await.expect("find"),
        Some(created)
    );
}

#[tokio::test]
async fn reader_pool_cannot_write() {
    let test = TestDb::new().await;
    let mut conn = test.reader().acquire().await.expect("conn");
    let result = members::insert(&mut conn, &member("reader"), "hash", ts(1)).await;
    assert!(result.is_err(), "읽기 풀로는 쓸 수 없어야 한다");
}

#[tokio::test]
async fn uncommitted_transaction_rolls_back() {
    let test = TestDb::new().await;
    let m = member("ghost");
    {
        let mut tx = test.begin_write().await.expect("begin");
        members::insert(&mut tx, &m, "hash", ts(1))
            .await
            .expect("insert");
        // 커밋하지 않고 버린다.
    }
    let mut conn = test.reader().acquire().await.expect("conn");
    assert_eq!(members::find(&mut conn, m.id).await.expect("find"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn write_transactions_are_serialized() {
    let test = TestDb::new().await;
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let db = test.db.clone();
            tokio::spawn(async move {
                let mut tx = db.begin_write().await.expect("begin");
                let hit = sessions::throttle_hit(
                    &mut tx,
                    "acct:counter",
                    ts(1),
                    60_000,
                    ThrottleMode::Fixed,
                )
                .await
                .expect("hit");
                tx.commit().await.expect("commit");
                hit.attempts
            })
        })
        .collect();
    let mut seen = Vec::new();
    for task in tasks {
        seen.push(task.await.expect("join"));
    }
    seen.sort_unstable();
    assert_eq!(
        seen,
        (1..=20).collect::<Vec<_>>(),
        "동시 갱신에서 증가분이 유실되면 안 된다"
    );
}

#[tokio::test]
async fn login_id_is_unique() {
    let test = TestDb::new().await;
    insert_member(&test, "gildong").await;
    let mut tx = test.begin_write().await.expect("begin");
    let err = members::insert(&mut tx, &member("gildong"), "hash", ts(1))
        .await
        .unwrap_err();
    assert!(err.is_unique_violation("members.login_id"), "{err}");
    assert!(!err.is_unique_violation("members.email"));
}

#[tokio::test]
async fn member_update_requires_matching_version() {
    let test = TestDb::new().await;
    let mut m = insert_member(&test, "gildong").await;
    let mut tx = test.begin_write().await.expect("begin");

    m.name = "새 이름".into();
    assert!(members::update(&mut tx, &m, ts(2)).await.expect("update"));
    // 같은(이제는 낡은) version으로 다시 쓰면 반영되지 않는다.
    m.name = "덮어쓰기 시도".into();
    assert!(!members::update(&mut tx, &m, ts(3))
        .await
        .expect("stale update"));

    let stored = members::find(&mut tx, m.id)
        .await
        .expect("find")
        .expect("some");
    assert_eq!((stored.name.as_str(), stored.version), ("새 이름", 2));
}

#[tokio::test]
async fn foreign_keys_are_enforced() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    let mut m = member("orphan");
    m.department_id = Some(DepartmentId::new());
    let err = members::insert(&mut tx, &m, "hash", ts(1))
        .await
        .unwrap_err();
    assert!(err.is_foreign_key_violation(), "{err}");
}

#[tokio::test]
async fn invalid_role_is_rejected_by_check_constraint() {
    let test = TestDb::new().await;
    let m = insert_member(&test, "gildong").await;
    let mut tx = test.begin_write().await.expect("begin");
    let result = sqlx::query("UPDATE members SET role = 'root' WHERE id = ?")
        .bind(m.id.to_string())
        .execute(&mut *tx)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn member_list_filters_and_escapes_search() {
    let test = TestDb::new().await;
    let a = insert_member(&test, "alice").await;
    let _b = insert_member(&test, "bob").await;
    let mut tx = test.begin_write().await.expect("begin");
    let mut retired = member("carol");
    retired.status = MemberStatus::Retired;
    retired.name = "100%_퇴사".into();
    members::insert(&mut tx, &retired, "hash", ts(1))
        .await
        .expect("insert");

    let all = members::list(&mut tx, &MemberFilter::default())
        .await
        .expect("list");
    assert_eq!(all.len(), 3);

    let active = MemberFilter {
        statuses: vec![MemberStatus::Active],
        ..Default::default()
    };
    assert_eq!(
        members::list(&mut tx, &active).await.expect("list").len(),
        2
    );

    let by_login = MemberFilter {
        search: Some("lic".into()),
        ..Default::default()
    };
    assert_eq!(
        members::list(&mut tx, &by_login).await.expect("list"),
        vec![a]
    );

    // `%`와 `_`는 글자 그대로 찾는다. 와일드카드로 동작하면 전원이 나온다.
    for (term, expected) in [("%", 1), ("_", 1), ("%_", 1), ("' OR 1=1 --", 0)] {
        let filter = MemberFilter {
            search: Some(term.into()),
            ..Default::default()
        };
        assert_eq!(
            members::list(&mut tx, &filter).await.expect("list").len(),
            expected,
            "{term}"
        );
    }

    let no_departments = MemberFilter {
        departments: Some(vec![]),
        ..Default::default()
    };
    assert!(members::list(&mut tx, &no_departments)
        .await
        .expect("list")
        .is_empty());
}

#[tokio::test]
async fn find_many_handles_empty_and_unknown_ids() {
    let test = TestDb::new().await;
    let a = insert_member(&test, "alice").await;
    let mut conn = test.reader().acquire().await.expect("conn");
    assert!(members::find_many(&mut conn, &[])
        .await
        .expect("empty")
        .is_empty());
    let found = members::find_many(&mut conn, &[a.id, MemberId::new()])
        .await
        .expect("find");
    assert_eq!(found, vec![a]);
}

#[tokio::test]
async fn set_password_updates_hash_and_flag() {
    let test = TestDb::new().await;
    let m = insert_member(&test, "gildong").await;
    let mut tx = test.begin_write().await.expect("begin");
    assert!(
        members::set_password(&mut tx, m.id, "new-hash", true, ts(5))
            .await
            .expect("set")
    );
    let after = members::find_credentials_by_id(&mut tx, m.id)
        .await
        .expect("find")
        .expect("some");
    assert_eq!(after.password_hash, "new-hash");
    assert!(after.member.must_change_password);
    assert_eq!(after.member.version, 2);
    let by_login = members::find_credentials(&mut tx, "gildong")
        .await
        .expect("find")
        .expect("some");
    assert_eq!(by_login.member.id, m.id);
    assert!(members::find_credentials(&mut tx, "GILDONG")
        .await
        .expect("find")
        .is_none());
    assert!(
        !members::set_password(&mut tx, MemberId::new(), "x", false, ts(5))
            .await
            .expect("none")
    );
}

#[tokio::test]
async fn owner_count_ignores_inactive_owners() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    for (login, status) in [
        ("o1", MemberStatus::Active),
        ("o2", MemberStatus::Suspended),
    ] {
        let mut m = member(login);
        m.role = Role::Owner;
        m.status = status;
        members::insert(&mut tx, &m, "hash", ts(1))
            .await
            .expect("insert");
    }
    assert_eq!(
        members::count_active_owners(&mut tx).await.expect("count"),
        1
    );
}

#[tokio::test]
async fn session_lookup_by_token_hash_and_validity() {
    let test = TestDb::new().await;
    let m = insert_member(&test, "gildong").await;
    let s = session(m.id, 100);
    let hash = [7u8; 32];
    let mut tx = test.begin_write().await.expect("begin");
    sessions::insert(&mut tx, &s, &hash).await.expect("insert");

    let found = sessions::find_by_token_hash(&mut tx, &hash)
        .await
        .expect("find")
        .expect("some");
    assert_eq!(found, s);
    assert!(sessions::find_by_token_hash(&mut tx, &[8u8; 32])
        .await
        .expect("find")
        .is_none());

    assert!(found.is_valid_at(ts(100)));
    assert!(found.is_valid_at(ts(1_099)));
    assert!(
        !found.is_valid_at(ts(1_100)),
        "유휴 만료 시각 정각에는 무효"
    );

    // 같은 토큰 해시는 두 번 저장할 수 없다.
    let dup = sessions::insert(&mut tx, &session(m.id, 100), &hash)
        .await
        .unwrap_err();
    assert!(dup.is_unique_violation("sessions.token_hash"));
}

#[tokio::test]
async fn absolute_expiry_wins_even_if_idle_is_extended() {
    let test = TestDb::new().await;
    let m = insert_member(&test, "gildong").await;
    let s = session(m.id, 0);
    let mut tx = test.begin_write().await.expect("begin");
    sessions::insert(&mut tx, &s, &[1u8; 32])
        .await
        .expect("insert");
    sessions::touch(&mut tx, s.id, ts(9_000), ts(50_000))
        .await
        .expect("touch");
    let found = sessions::find_by_token_hash(&mut tx, &[1u8; 32])
        .await
        .expect("find")
        .expect("some");
    assert!(found.is_valid_at(ts(9_999)));
    assert!(!found.is_valid_at(ts(10_000)));
}

#[tokio::test]
async fn revoke_is_scoped_to_owner_and_idempotent() {
    let test = TestDb::new().await;
    let alice = insert_member(&test, "alice").await;
    let bob = insert_member(&test, "bob").await;
    let s = session(alice.id, 0);
    let mut tx = test.begin_write().await.expect("begin");
    sessions::insert(&mut tx, &s, &[1u8; 32])
        .await
        .expect("insert");

    assert!(
        !sessions::revoke(&mut tx, bob.id, s.id, ts(5))
            .await
            .expect("other"),
        "남의 세션"
    );
    assert!(sessions::revoke(&mut tx, alice.id, s.id, ts(5))
        .await
        .expect("own"));
    assert!(!sessions::revoke(&mut tx, alice.id, s.id, ts(6))
        .await
        .expect("again"));

    let found = sessions::find_by_token_hash(&mut tx, &[1u8; 32])
        .await
        .expect("find")
        .expect("some");
    assert_eq!(found.revoked_at, Some(ts(5)), "처음 폐기한 시각이 유지된다");
    assert!(!found.is_valid_at(ts(5)));
}

#[tokio::test]
async fn revoke_all_can_keep_current_session() {
    let test = TestDb::new().await;
    let alice = insert_member(&test, "alice").await;
    let bob = insert_member(&test, "bob").await;
    let mut tx = test.begin_write().await.expect("begin");
    let mine: Vec<_> = (0..3u8).map(|_| session(alice.id, 0)).collect();
    for (i, s) in mine.iter().enumerate() {
        sessions::insert(&mut tx, s, &[i as u8 + 1; 32])
            .await
            .expect("insert");
    }
    let theirs = session(bob.id, 0);
    sessions::insert(&mut tx, &theirs, &[9u8; 32])
        .await
        .expect("insert");

    let revoked = sessions::revoke_all(&mut tx, alice.id, Some(mine[0].id), ts(5)).await;
    assert_eq!(revoked.expect("revoke"), 2);
    let left: BTreeSet<_> = sessions::list_active(&mut tx, alice.id, ts(5))
        .await
        .expect("list")
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(left, BTreeSet::from([mine[0].id]));
    assert_eq!(
        sessions::list_active(&mut tx, bob.id, ts(5))
            .await
            .expect("list")
            .len(),
        1
    );

    assert_eq!(
        sessions::revoke_all(&mut tx, alice.id, None, ts(6))
            .await
            .expect("revoke"),
        1
    );
    assert_eq!(
        sessions::revoke_all(&mut tx, alice.id, None, ts(7))
            .await
            .expect("revoke"),
        0
    );
}

#[tokio::test]
async fn fixed_throttle_counts_within_window_and_resets_after() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    const WINDOW: i64 = 60_000;
    let mut hit = async |bucket: &str, at: i64| {
        sessions::throttle_hit(&mut tx, bucket, ts(at), WINDOW, ThrottleMode::Fixed)
            .await
            .expect("hit")
            .attempts
    };
    for expected in 1..=3 {
        assert_eq!(hit("ip:1.1.1.1", 1_000).await, expected);
    }
    assert_eq!(
        hit("ip:1.1.1.1", 60_999).await,
        4,
        "창의 마지막 순간까지는 누적"
    );
    assert_eq!(hit("ip:1.1.1.1", 61_000).await, 1, "창이 지나면 1부터");
    assert_eq!(hit("ip:2.2.2.2", 61_000).await, 1, "다른 키는 독립");
    // 시계가 뒤로 가도(창 시작보다 과거) 음수 창에 갇히지 않고 새로 시작한다.
    assert_eq!(hit("ip:1.1.1.1", 10).await, 1);

    sessions::throttle_clear(&mut tx, "ip:2.2.2.2")
        .await
        .expect("clear");
    assert_eq!(
        sessions::throttle_purge(&mut tx, ts(1_000))
            .await
            .expect("purge"),
        1
    );
}

#[tokio::test]
async fn lockout_throttle_slides_until_locked_then_holds_for_one_window() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    const WINDOW: i64 = 900_000;
    let mode = ThrottleMode::Lockout { limit: 5 };
    let mut hit = async |at: i64| {
        let state = sessions::throttle_hit(&mut tx, "acct:a", ts(at), WINDOW, mode)
            .await
            .expect("hit");
        (state.attempts, state.window_start.as_millis())
    };
    // 창 끝 무렵에 몰아서 실패해도 횟수가 이어진다(고정 창이면 여기서 초기화됐을 것).
    assert_eq!(hit(0).await, (1, 0));
    for (n, at) in [(2, 890_000), (3, 895_000), (4, 899_000), (5, 899_999)] {
        assert_eq!(hit(at).await, (n, at));
    }
    // 한도를 넘긴 순간부터 한 창 동안 잠긴다.
    assert_eq!(hit(900_500).await, (6, 900_500));
    // 잠긴 동안의 시도는 횟수만 늘고 잠금을 늘리지 않는다.
    assert_eq!(hit(1_500_000).await, (7, 900_500));
    assert_eq!(hit(1_800_499).await, (8, 900_500));
    // 잠금 시각 + 창이 지나면 초기화.
    assert_eq!(hit(1_800_500).await, (1, 1_800_500));
}

#[tokio::test]
async fn lockout_throttle_resets_after_a_quiet_window() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    let mode = ThrottleMode::Lockout { limit: 5 };
    let mut hit = async |at: i64| {
        sessions::throttle_hit(&mut tx, "acct:b", ts(at), 1_000, mode)
            .await
            .expect("hit")
            .attempts
    };
    assert_eq!(hit(0).await, 1);
    assert_eq!(hit(999).await, 2);
    assert_eq!(hit(1_998).await, 3, "마지막 시도로부터 창 안이면 이어진다");
    assert_eq!(
        hit(2_998).await,
        1,
        "마지막 시도로부터 창만큼 조용했으면 초기화"
    );
}

#[tokio::test]
async fn settings_singleton_and_versioned_update() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    assert_eq!(settings::get(&mut tx).await.expect("get"), None);

    let mut s = settings();
    settings::insert(&mut tx, &s).await.expect("insert");
    assert_eq!(settings::get(&mut tx).await.expect("get"), Some(s.clone()));
    assert!(
        settings::insert(&mut tx, &s).await.is_err(),
        "설정 행은 하나뿐"
    );

    s.company_name = "새 이름".into();
    s.break_tiers = vec![];
    assert!(settings::update(&mut tx, &s).await.expect("update"));
    assert!(
        !settings::update(&mut tx, &s).await.expect("stale"),
        "낡은 version"
    );
    let stored = settings::get(&mut tx).await.expect("get").expect("some");
    assert_eq!(
        (stored.company_name.as_str(), stored.version),
        ("새 이름", 2)
    );
    assert!(stored.break_tiers.is_empty());
}

#[tokio::test]
async fn settings_check_constraints_reject_inverted_hours() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    let mut s = settings();
    s.work_start = MinuteOfDay::parse("18:00").expect("t");
    s.work_end = MinuteOfDay::parse("09:00").expect("t");
    assert!(settings::insert(&mut tx, &s).await.is_err());
}

#[tokio::test]
async fn holidays_upsert_list_delete() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    for (d, name) in [
        ("2026-03-01", "삼일절"),
        ("2026-05-05", "어린이날"),
        ("2027-01-01", "신정"),
    ] {
        let holiday = Holiday {
            date: date(d),
            name: name.into(),
        };
        settings::upsert_holiday(&mut tx, &holiday)
            .await
            .expect("upsert");
    }
    let renamed = Holiday {
        date: date("2026-03-01"),
        name: "3·1절".into(),
    };
    settings::upsert_holiday(&mut tx, &renamed)
        .await
        .expect("rename");

    let year = DateRange::new(date("2026-01-01"), date("2026-12-31")).expect("range");
    let listed = settings::list_holidays(&mut tx, year).await.expect("list");
    assert_eq!(
        listed
            .iter()
            .map(|h| (h.date, h.name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (date("2026-03-01"), "3·1절"),
            (date("2026-05-05"), "어린이날")
        ]
    );
    assert!(settings::delete_holiday(&mut tx, date("2026-05-05"))
        .await
        .expect("delete"));
    assert!(!settings::delete_holiday(&mut tx, date("2026-05-05"))
        .await
        .expect("again"));
}

#[tokio::test]
async fn departments_tree_roundtrip_and_head_lookup() {
    let test = TestDb::new().await;
    let head = insert_member(&test, "head").await;
    let mut tx = test.begin_write().await.expect("begin");
    let root = Department {
        id: DepartmentId::new(),
        parent_id: None,
        name: "본부".into(),
        head_member_id: Some(head.id),
        sort_order: 0,
        archived_at: None,
        version: 1,
    };
    let mut child = Department {
        id: DepartmentId::new(),
        parent_id: Some(root.id),
        name: "개발팀".into(),
        head_member_id: None,
        sort_order: 1,
        archived_at: None,
        version: 1,
    };
    org::insert_department(&mut tx, &root, ts(1))
        .await
        .expect("root");
    org::insert_department(&mut tx, &child, ts(1))
        .await
        .expect("child");

    let listed = org::list_departments(&mut tx).await.expect("list");
    assert_eq!(listed, vec![root.clone(), child.clone()]);
    assert_eq!(
        org::departments_headed_by(&mut tx, head.id)
            .await
            .expect("headed"),
        vec![root.id]
    );

    // 자기 자신을 부모로 지정하는 것은 DB 제약으로도 막힌다.
    let mut cyclic = child.clone();
    cyclic.parent_id = Some(child.id);
    assert!(org::update_department(&mut tx, &cyclic, ts(2))
        .await
        .is_err());

    child.archived_at = Some(ts(3));
    assert!(org::update_department(&mut tx, &child, ts(3))
        .await
        .expect("archive"));
    assert_eq!(org::list_departments(&mut tx).await.expect("list").len(), 1);
    assert!(org::find_department(&mut tx, child.id)
        .await
        .expect("find")
        .is_some());

    assert_eq!(
        org::clear_head(&mut tx, head.id, ts(4))
            .await
            .expect("clear"),
        1
    );
    assert!(org::departments_headed_by(&mut tx, head.id)
        .await
        .expect("headed")
        .is_empty());
}

#[tokio::test]
async fn position_names_are_unique_among_active_only() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    let mut lead = Position {
        id: PositionId::new(),
        name: "팀장".into(),
        level: 50,
        archived_at: None,
        version: 1,
    };
    org::insert_position(&mut tx, &lead, ts(1))
        .await
        .expect("insert");
    let dup = Position {
        id: PositionId::new(),
        name: "팀장".into(),
        level: 10,
        archived_at: None,
        version: 1,
    };
    let err = org::insert_position(&mut tx, &dup, ts(1))
        .await
        .unwrap_err();
    assert!(err.is_any_unique_violation());

    lead.archived_at = Some(ts(2));
    assert!(org::update_position(&mut tx, &lead, ts(2))
        .await
        .expect("archive"));
    org::insert_position(&mut tx, &dup, ts(3))
        .await
        .expect("보관 후에는 같은 이름 재사용 가능");
    assert_eq!(org::list_positions(&mut tx).await.expect("list"), vec![dup]);
}

fn audit_entry(actor: Option<MemberId>, action: &str, at: i64) -> AuditEntry {
    AuditEntry {
        id: AuditLogId::new(),
        at: ts(at),
        actor_id: actor,
        action: action.to_owned(),
        target_type: "member".into(),
        target_id: Some("t-1".into()),
        detail: Some(serde_json::json!({ "note": "<script>alert(1)</script>" })),
        ip: None,
    }
}

#[tokio::test]
async fn audit_log_is_append_only() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    let entry = audit_entry(None, "system.init", 1);
    audit::insert(&mut tx, &entry).await.expect("insert");

    let update = sqlx::query("UPDATE audit_log SET action = 'tampered'")
        .execute(&mut *tx)
        .await;
    assert!(update.unwrap_err().to_string().contains("append-only"));
    let delete = sqlx::query("DELETE FROM audit_log").execute(&mut *tx).await;
    assert!(delete.unwrap_err().to_string().contains("append-only"));

    let (stored, _) = audit::list(&mut tx, &AuditFilter::default(), 10)
        .await
        .expect("list");
    assert_eq!(stored, vec![entry]);
}

#[tokio::test]
async fn audit_list_paginates_newest_first_and_filters() {
    let test = TestDb::new().await;
    let actor = insert_member(&test, "admin").await;
    let mut tx = test.begin_write().await.expect("begin");
    let mut ids = Vec::new();
    for i in 0..5 {
        let action = if i % 2 == 0 {
            "member.update"
        } else {
            "member.create"
        };
        let entry = audit_entry(Some(actor.id), action, i);
        ids.push(entry.id);
        audit::insert(&mut tx, &entry).await.expect("insert");
    }

    let (page1, more1) = audit::list(&mut tx, &AuditFilter::default(), 2)
        .await
        .expect("p1");
    assert_eq!(
        page1.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![ids[4], ids[3]]
    );
    assert!(more1);

    let next = AuditFilter {
        before: Some(ids[3]),
        ..Default::default()
    };
    let (page2, more2) = audit::list(&mut tx, &next, 3).await.expect("p2");
    assert_eq!(
        page2.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![ids[2], ids[1], ids[0]]
    );
    assert!(!more2);

    let filter = AuditFilter {
        action: Some("member.create".into()),
        from: Some(ts(1)),
        until: Some(ts(3)),
        ..Default::default()
    };
    let (filtered, _) = audit::list(&mut tx, &filter, 10).await.expect("filtered");
    assert_eq!(
        filtered.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![ids[1]],
        "until은 미포함"
    );

    let nobody = AuditFilter {
        actor_id: Some(MemberId::new()),
        ..Default::default()
    };
    assert!(audit::list(&mut tx, &nobody, 10)
        .await
        .expect("empty")
        .0
        .is_empty());
}

#[tokio::test]
async fn idempotency_key_stores_first_response_only() {
    let test = TestDb::new().await;
    let alice = insert_member(&test, "alice").await;
    let bob = insert_member(&test, "bob").await;
    let first = StoredResponse {
        fingerprint: [1u8; 32],
        status: 201,
        body: "{\"id\":1}".into(),
    };
    let mut tx = test.begin_write().await.expect("begin");

    assert_eq!(
        idempotency::find(&mut tx, alice.id, "k")
            .await
            .expect("find"),
        None
    );
    idempotency::insert(&mut tx, alice.id, "k", &first, ts(10))
        .await
        .expect("insert");
    let second = StoredResponse {
        fingerprint: [2u8; 32],
        status: 500,
        body: "x".into(),
    };
    let err = idempotency::insert(&mut tx, alice.id, "k", &second, ts(11))
        .await
        .unwrap_err();
    assert!(err.is_any_unique_violation());
    assert_eq!(
        idempotency::find(&mut tx, alice.id, "k")
            .await
            .expect("find"),
        Some(first.clone())
    );

    // 키는 구성원별로 분리된다.
    assert_eq!(
        idempotency::find(&mut tx, bob.id, "k").await.expect("find"),
        None
    );
    idempotency::insert(&mut tx, bob.id, "k", &second, ts(12))
        .await
        .expect("other member");

    assert_eq!(idempotency::purge(&mut tx, ts(11)).await.expect("purge"), 1);
    assert_eq!(
        idempotency::find(&mut tx, alice.id, "k")
            .await
            .expect("find"),
        None
    );
}

#[tokio::test]
async fn active_member_counts_are_grouped_per_department() {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    let department = |name: &str| Department {
        id: DepartmentId::new(),
        parent_id: None,
        name: name.into(),
        head_member_id: None,
        sort_order: 0,
        archived_at: None,
        version: 1,
    };
    let (a, b, empty) = (department("A"), department("B"), department("빈 부서"));
    for d in [&a, &b, &empty] {
        org::insert_department(&mut tx, d, ts(1))
            .await
            .expect("dept");
    }
    let rows = [
        ("a1", Some(a.id), MemberStatus::Active),
        ("a2", Some(a.id), MemberStatus::Active),
        ("a3", Some(a.id), MemberStatus::Suspended),
        ("a4", Some(a.id), MemberStatus::Retired),
        ("b1", Some(b.id), MemberStatus::Active),
        ("none", None, MemberStatus::Active),
    ];
    for (login, department_id, status) in rows {
        let mut m = member(login);
        m.department_id = department_id;
        m.status = status;
        members::insert(&mut tx, &m, "hash", ts(1))
            .await
            .expect("insert");
    }
    let mut counts = members::count_active_by_department(&mut tx)
        .await
        .expect("counts");
    counts.sort();
    let mut expected = vec![(a.id, 2), (b.id, 1)];
    expected.sort();
    assert_eq!(
        counts, expected,
        "정지·퇴사·무소속은 세지 않고 빈 부서는 없다"
    );
}

// ── 구성원 날짜 트리거(0003) ─────────────────────────────────────────────

/// 저장 표기로 다시 읽을 수 없는 날짜는 어떤 경로로 들어와도 DB가 거부한다.
#[tokio::test]
async fn member_date_triggers_reject_unreadable_dates() {
    let db = TestDb::new().await;
    let good = insert_member(&db, "good").await;
    let ymd = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).expect("date");

    for bad in [ymd(202, 3, 2), ymd(10000, 1, 1), ymd(1899, 12, 31)] {
        // 넣기: 입사일.
        let mut tx = db.begin_write().await.expect("begin");
        let mut new = member("bad-hire");
        new.hired_on = bad;
        let error = members::insert(&mut tx, &new, "hash", ts(1))
            .await
            .expect_err("insert must fail");
        assert!(error.to_string().contains("hired_on/retired_on"), "{error}");
        drop(tx);
        // 넣기: 퇴사일.
        let mut tx = db.begin_write().await.expect("begin");
        let mut new = member("bad-retire");
        new.retired_on = Some(bad);
        assert!(members::insert(&mut tx, &new, "hash", ts(1)).await.is_err());
        drop(tx);
        // 고치기: 입사일, 퇴사일.
        for field in ["hired_on", "retired_on"] {
            let mut tx = db.begin_write().await.expect("begin");
            let mut changed = good.clone();
            if field == "hired_on" {
                changed.hired_on = bad;
            } else {
                changed.retired_on = Some(bad);
            }
            assert!(
                members::update(&mut tx, &changed, ts(2)).await.is_err(),
                "{field} {bad}"
            );
            drop(tx);
        }
    }
    // 표기는 맞지만 달력에 없는 날짜·다른 표기도 거부한다(코드를 거치지 않은 직접 쓰기).
    for raw in [
        "2026-02-30",
        "2026-3-2",
        "2026-03-02T00:00:00",
        "",
        "20260302",
    ] {
        let mut tx = db.begin_write().await.expect("begin");
        let result = sqlx::query("UPDATE members SET hired_on = ? WHERE id = ?")
            .bind(raw)
            .bind(good.id.to_string())
            .execute(&mut *tx)
            .await;
        assert!(result.is_err(), "accepted {raw:?}");
    }

    // 거부된 뒤에도 행은 그대로 읽힌다.
    let mut conn = db.read().await.expect("read");
    assert_eq!(
        members::find(&mut conn, good.id).await.expect("find"),
        Some(good.clone())
    );
    assert_eq!(
        members::list(&mut conn, &MemberFilter::default())
            .await
            .expect("list")
            .len(),
        1
    );
    drop(conn);

    // 경계값과 날짜가 아닌 열의 수정은 그대로 된다.
    let mut tx = db.begin_write().await.expect("begin");
    let mut edge = good.clone();
    edge.hired_on = ymd(1900, 1, 1);
    edge.retired_on = Some(ymd(9999, 12, 31));
    assert!(members::update(&mut tx, &edge, ts(3))
        .await
        .expect("update"));
    members::set_password(&mut tx, good.id, "hash2", false, ts(4))
        .await
        .expect("password");
    tx.commit().await.expect("commit");
}
