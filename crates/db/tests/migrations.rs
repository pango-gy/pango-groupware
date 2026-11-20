//! 데이터를 고치는 마이그레이션의 테스트.
//!
//! 스키마를 한 단계씩 직접 적용해(0001 → 0003), 옛 형식의 행을 넣은 뒤 대상 마이그레이션의
//! SQL을 그대로 실행한다. 마이그레이터와 같이 한 트랜잭션 안에서 실행한다.

use chrono::NaiveDate;
use pango_core::authz::Role;
use pango_core::id::{LeaveLedgerEntryId, MemberId};
use pango_core::leave::{accrual_grants, LeaveAmount, LedgerEntry, LedgerEntryKind};
use pango_core::member::MemberStatus;
use pango_core::time::Timestamp;
use pango_db::leave::{self, LedgerRecord};
use pango_db::members::{self, Member};
use pango_db::Conn;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};

const BEFORE_GRANT_KEYS: [&str; 3] = [
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_attendance_corrections.sql"),
    include_str!("../migrations/0003_member_date_checks.sql"),
];
const GRANT_KEYS: &str = include_str!("../migrations/0004_leave_grant_keys.sql");

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis).expect("timestamp")
}

fn date(value: &str) -> NaiveDate {
    value.parse().expect("date")
}

/// 0003까지 적용한 빈 데이터베이스.
async fn database_before_grant_keys(dir: &tempfile::TempDir) -> Conn {
    let mut conn = SqliteConnectOptions::new()
        .filename(dir.path().join("staged.db"))
        .create_if_missing(true)
        .foreign_keys(true)
        .connect()
        .await
        .expect("connect");
    for sql in BEFORE_GRANT_KEYS {
        sqlx::raw_sql(sql).execute(&mut conn).await.expect("schema");
    }
    conn
}

async fn migrate_grant_keys(conn: &mut Conn) {
    let mut tx = conn.begin().await.expect("begin");
    sqlx::raw_sql(GRANT_KEYS)
        .execute(&mut *tx)
        .await
        .expect("0004");
    tx.commit().await.expect("commit");
}

async fn add_member(conn: &mut Conn, login_id: &str, hired_on: &str) -> MemberId {
    let member = Member {
        id: MemberId::new(),
        login_id: login_id.to_owned(),
        name: login_id.to_owned(),
        email: None,
        phone: None,
        department_id: None,
        position_id: None,
        role: Role::Member,
        status: MemberStatus::Active,
        hired_on: date(hired_on),
        retired_on: None,
        must_change_password: false,
        created_at: ts(1),
        version: 1,
    };
    members::insert(conn, &member, "hash", ts(1))
        .await
        .expect("member");
    member.id
}

fn grant_row(member_id: MemberId, key: Option<String>, entry: LedgerEntry) -> LedgerRecord {
    LedgerRecord {
        id: LeaveLedgerEntryId::new(),
        member_id,
        entry,
        grant_key: key,
        memo: None,
        actor_id: None,
        created_at: ts(1),
    }
}

/// 0004 이전의 서버가 넣던 그대로: 그 입사일의 법정 부여를 날짜 키로 넣는다.
async fn insert_date_keyed_grants(conn: &mut Conn, member_id: MemberId, hired: &str, as_of: &str) {
    for grant in accrual_grants(date(hired), None, date(as_of)) {
        let old_key = format!("{}:{}", grant.kind.as_str(), grant.granted_on);
        let row = grant_row(member_id, Some(old_key), grant.to_ledger_entry());
        leave::insert_ledger(conn, &row).await.expect("old grant");
    }
}

/// (부여일, 키) 목록. 들어간 순서.
async fn keys(conn: &mut Conn, member_id: MemberId) -> Vec<(NaiveDate, Option<String>)> {
    leave::list_ledger(conn, member_id)
        .await
        .expect("ledger")
        .into_iter()
        .filter_map(|record| match record.entry.kind {
            LedgerEntryKind::Grant { valid_from, .. } => Some((valid_from, record.grant_key)),
            _ => None,
        })
        .collect()
}

/// 지금 서버가 그 입사일로 계산하는 (부여일, 키).
fn expected(hired: &str, as_of: &str) -> Vec<(NaiveDate, Option<String>)> {
    accrual_grants(date(hired), None, date(as_of))
        .into_iter()
        .map(|grant| (grant.granted_on, Some(grant.grant_key)))
        .collect()
}

/// 지금의 자동 부여를 다시 반영했을 때 새로 들어가는 줄 수.
async fn reaccrue(conn: &mut Conn, member_id: MemberId, hired: &str, as_of: &str) -> usize {
    let mut inserted = 0;
    for grant in accrual_grants(date(hired), None, date(as_of)) {
        let row = grant_row(
            member_id,
            Some(grant.grant_key.clone()),
            grant.to_ledger_entry(),
        );
        if leave::insert_grant_if_absent(conn, &row)
            .await
            .expect("grant")
        {
            inserted += 1;
        }
    }
    inserted
}

#[tokio::test]
async fn date_keys_become_the_ordinals_the_server_now_computes() {
    let dir = tempfile::tempdir().expect("dir");
    let mut conn = database_before_grant_keys(&dir).await;
    // 평범한 입사일, 말일 입사(2월은 28·29일로 맞춰진다), 윤일 입사, 30년 근속.
    let cases = [
        ("plain", "2024-03-02", "2026-03-02"),
        ("month-end", "2024-01-31", "2026-11-20"),
        ("leap-day", "2024-02-29", "2027-03-01"),
        ("veteran", "1996-07-15", "2026-11-20"),
        ("first-year", "2026-02-10", "2026-11-20"),
    ];
    let mut ids = Vec::new();
    for (login_id, hired, as_of) in cases {
        let id = add_member(&mut conn, login_id, hired).await;
        insert_date_keyed_grants(&mut conn, id, hired, as_of).await;
        ids.push(id);
    }
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leave_ledger")
        .fetch_one(&mut conn)
        .await
        .expect("count");

    migrate_grant_keys(&mut conn).await;

    for (id, (login_id, hired, as_of)) in ids.iter().zip(cases) {
        assert_eq!(
            keys(&mut conn, *id).await,
            expected(hired, as_of),
            "{login_id}"
        );
        // 그래서 다음 자동 부여는 아무것도 더 넣지 않는다.
        assert_eq!(
            reaccrue(&mut conn, *id, hired, as_of).await,
            0,
            "{login_id}"
        );
    }
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM leave_ledger")
        .fetch_one(&mut conn)
        .await
        .expect("count");
    assert_eq!(after, before, "줄 수는 그대로");
    let left: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM leave_ledger WHERE grant_key GLOB '*:*-*'")
            .fetch_one(&mut conn)
            .await
            .expect("count");
    assert_eq!(left, 0, "날짜 키가 남지 않는다");
}

/// 입사일을 고쳐 이미 두 벌이 들어간 구성원: 차수마다 한 줄만 차수 키를 갖고, 나머지는 옛 키
/// 그대로 남는다. 수량과 날짜는 어느 줄도 바뀌지 않는다.
#[tokio::test]
async fn already_duplicated_grants_keep_one_row_per_ordinal() {
    let dir = tempfile::tempdir().expect("dir");
    let mut conn = database_before_grant_keys(&dir).await;
    let as_of = "2026-03-02";
    // 2024-03-02로 등록해 부여받은 뒤 2024-03-01로 고쳤고, 그 뒤 조회에서 한 벌이 더 들어갔다.
    let member = add_member(&mut conn, "corrected", "2024-03-01").await;
    insert_date_keyed_grants(&mut conn, member, "2024-03-02", as_of).await;
    insert_date_keyed_grants(&mut conn, member, "2024-03-01", as_of).await;
    let before = leave::list_ledger(&mut conn, member).await.expect("ledger");
    assert_eq!(before.len(), 26);

    migrate_grant_keys(&mut conn).await;

    let after = leave::list_ledger(&mut conn, member).await.expect("ledger");
    let strip = |records: &[LedgerRecord]| -> Vec<LedgerRecord> {
        records
            .iter()
            .cloned()
            .map(|record| LedgerRecord {
                grant_key: None,
                ..record
            })
            .collect()
    };
    assert_eq!(
        strip(&after),
        strip(&before),
        "키 말고는 아무것도 바뀌지 않는다"
    );
    // 현재 입사일(03-01)에 맞는 두 번째 벌이 차수 키를 갖는다.
    let rekeyed: Vec<_> = keys(&mut conn, member).await;
    assert_eq!(rekeyed[13..], expected("2024-03-01", as_of)[..]);
    // 첫 번째 벌은 옛 날짜 키 그대로다.
    for ((valid_from, key), record) in rekeyed[..13].iter().zip(&before[..13]) {
        assert_eq!(key, &record.grant_key, "{valid_from}");
        assert!(key.as_deref().is_some_and(|k| k.contains('-')));
    }
    // 다음 자동 부여가 세 번째 벌을 넣지 않는다.
    assert_eq!(reaccrue(&mut conn, member, "2024-03-01", as_of).await, 0);
}

#[tokio::test]
async fn rows_that_do_not_map_to_an_ordinal_are_left_alone() {
    let dir = tempfile::tempdir().expect("dir");
    let mut conn = database_before_grant_keys(&dir).await;
    let grant = |valid_from: &str| LedgerEntry {
        kind: LedgerEntryKind::Grant {
            valid_from: date(valid_from),
            expires_on: Some(date("2030-01-01")),
        },
        amount: LeaveAmount::from_quarters(4),
    };
    let member = add_member(&mut conn, "odd", "2024-03-02").await;
    let rows = [
        // 입사 전·입사한 달의 월 부여(입사일을 뒤로 크게 고친 흔적): 차수 0 이하.
        ("monthly:2024-02-02", "2024-02-02", "monthly:2024-02-02"),
        ("monthly:2024-03-02", "2024-03-02", "monthly:2024-03-02"),
        // 12개월째의 "월" 부여, 연 단위로 떨어지지 않는 "연" 부여, 81년차.
        ("monthly:2025-03-02", "2025-03-02", "monthly:2025-03-02"),
        ("annual:2025-09-02", "2025-09-02", "annual:2025-09-02"),
        ("annual:2105-03-02", "2105-03-02", "annual:2105-03-02"),
        // 이미 차수 키인 줄과, 그 차수가 될 날짜 키: 날짜 키가 옛 키로 남는다.
        ("annual:1", "2025-03-02", "annual:1"),
        ("annual:2025-03-02", "2025-03-02", "annual:2025-03-02"),
        // 형식이 다른 키는 자동 부여가 넣은 것이 아니다.
        ("annual:2026", "2026-03-02", "annual:2026"),
        ("bonus:2026-03-02", "2026-03-02", "bonus:2026-03-02"),
        // 경계: 1개월째, 11개월째, 1년째(이미 있음)·2년째, 80년째.
        ("monthly:2024-04-02", "2024-04-02", "monthly:1"),
        ("monthly:2025-02-02", "2025-02-02", "monthly:11"),
        ("annual:2026-03-02", "2026-03-02", "annual:2"),
        ("annual:2104-03-02", "2104-03-02", "annual:80"),
    ];
    for (key, valid_from, _) in rows {
        let row = grant_row(member, Some(key.to_owned()), grant(valid_from));
        leave::insert_ledger(&mut conn, &row).await.expect("row");
    }
    // 키 없는 줄(수동 조정)과 다른 구성원의 줄.
    let adjust = LedgerEntry {
        kind: LedgerEntryKind::Adjust {
            effective_on: date("2024-04-02"),
        },
        amount: LeaveAmount::from_quarters(4),
    };
    leave::insert_ledger(&mut conn, &grant_row(member, None, adjust))
        .await
        .expect("adjust");
    let other = add_member(&mut conn, "other", "2025-04-02").await;
    let row = grant_row(
        other,
        Some("monthly:2025-05-02".into()),
        grant("2025-05-02"),
    );
    leave::insert_ledger(&mut conn, &row).await.expect("row");

    migrate_grant_keys(&mut conn).await;

    let stored: Vec<Option<String>> = leave::list_ledger(&mut conn, member)
        .await
        .expect("ledger")
        .into_iter()
        .map(|record| record.grant_key)
        .collect();
    let wanted: Vec<Option<String>> = rows
        .iter()
        .map(|(_, _, key)| Some((*key).to_owned()))
        .chain([None])
        .collect();
    assert_eq!(stored, wanted);
    assert_eq!(
        keys(&mut conn, other).await,
        [(date("2025-05-02"), Some("monthly:1".to_owned()))]
    );
}

/// 마이그레이션이 잠시 내린 추가 전용 트리거는 다시 걸려 있어야 한다.
#[tokio::test]
async fn ledger_is_append_only_again_after_the_rekey() {
    let dir = tempfile::tempdir().expect("dir");
    let mut conn = database_before_grant_keys(&dir).await;
    let member = add_member(&mut conn, "kim", "2024-03-02").await;
    insert_date_keyed_grants(&mut conn, member, "2024-03-02", "2026-03-02").await;
    migrate_grant_keys(&mut conn).await;

    for sql in [
        "UPDATE leave_ledger SET grant_key = 'annual:9'",
        "UPDATE leave_ledger SET amount_q = amount_q + 4",
        "DELETE FROM leave_ledger",
    ] {
        let error = sqlx::query(sql)
            .execute(&mut conn)
            .await
            .expect_err("append-only");
        assert!(error.to_string().contains("append-only"), "{sql}: {error}");
    }
    let temp: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_temp_master WHERE name = 'leave_grant_rekey'",
    )
    .fetch_one(&mut conn)
    .await
    .expect("temp");
    assert_eq!(temp, 0, "임시 표가 남지 않는다");
    // 빈 데이터베이스에도 그대로 적용된다(새 설치).
    let empty_dir = tempfile::tempdir().expect("dir");
    let mut empty = database_before_grant_keys(&empty_dir).await;
    migrate_grant_keys(&mut empty).await;
}

// ── 0006: 결재 양식의 분류와 필드 정의 ───────────────────────────────────

const BEFORE_FORM_SCHEMA: [&str; 5] = [
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_attendance_corrections.sql"),
    include_str!("../migrations/0003_member_date_checks.sql"),
    GRANT_KEYS,
    include_str!("../migrations/0005_calendar.sql"),
];
const FORM_SCHEMA: &str = include_str!("../migrations/0006_approval_form_schema.sql");

#[tokio::test]
async fn existing_forms_get_categories_and_keep_their_content() {
    use pango_core::approval::{FormCategory, FormOrigin};
    use pango_core::form_schema::FormSchema;
    use pango_db::approval;

    let dir = tempfile::tempdir().expect("tempdir");
    let mut conn = SqliteConnectOptions::new()
        .filename(dir.path().join("staged.db"))
        .create_if_missing(true)
        .foreign_keys(true)
        .connect()
        .await
        .expect("connect");
    for sql in BEFORE_FORM_SCHEMA {
        sqlx::raw_sql(sql).execute(&mut conn).await.expect("schema");
    }
    // 0006 이전의 서버가 만든 양식: 최초 설정의 세 개와 관리자가 만든 것 하나.
    let old_forms = [
        ("general", "품의서(우리 회사)", "general", 1),
        ("leave", "휴가 신청서", "leave", 1),
        (
            "attendance_correction",
            "근태 정정 신청서",
            "attendance_correction",
            1,
        ),
        ("expense", "지출 결의서", "general", 0),
        // 코드만 같고 종류가 다른 양식은 기본 양식으로 보지 않는다.
        ("leave_extra", "휴가 관련", "general", 1),
    ];
    for (index, (code, name, kind, active)) in old_forms.into_iter().enumerate() {
        sqlx::query(
            "INSERT INTO approval_forms
                (id, code, name, kind, body_template, active, sort_order, created_at, updated_at,
                 version)
             VALUES (?, ?, ?, ?, '1. 목적', ?, ?, 1, 1, 7)",
        )
        .bind(format!("00000000-0000-7000-8000-00000000000{index}"))
        .bind(code)
        .bind(name)
        .bind(kind)
        .bind(active)
        .bind(i64::try_from(index).expect("index"))
        .execute(&mut conn)
        .await
        .expect("form");
    }

    let mut tx = conn.begin().await.expect("begin");
    sqlx::raw_sql(FORM_SCHEMA)
        .execute(&mut *tx)
        .await
        .expect("0006");
    tx.commit().await.expect("commit");

    let forms = approval::list_forms(&mut conn, false).await.expect("list");
    let summary: Vec<_> = forms
        .iter()
        .map(|form| (form.code.as_str(), form.category, form.origin, form.active))
        .collect();
    assert_eq!(
        summary,
        [
            ("general", FormCategory::General, FormOrigin::Catalog, true),
            ("leave", FormCategory::Hr, FormOrigin::Catalog, true),
            (
                "attendance_correction",
                FormCategory::Attendance,
                FormOrigin::Catalog,
                true
            ),
            ("expense", FormCategory::General, FormOrigin::Custom, false),
            (
                "leave_extra",
                FormCategory::General,
                FormOrigin::Custom,
                true
            ),
        ]
    );
    for form in &forms {
        assert_eq!(form.schema, FormSchema::default());
        assert_eq!(
            (form.schema_version, form.use_body, form.version),
            (1, true, 7)
        );
        assert!(form.default_reference_ids.is_empty());
        assert_eq!(form.body_template, "1. 목적");
    }
    assert_eq!(forms[0].name, "품의서(우리 회사)", "고친 이름은 그대로다");

    // 분류는 정해진 값만 받는다.
    let bad = sqlx::query("UPDATE approval_forms SET category = 'finance' WHERE code = 'expense'")
        .execute(&mut conn)
        .await;
    assert!(bad.is_err());
}
