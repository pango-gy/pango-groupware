//! 휴가 저장소 통합 테스트.

use chrono::NaiveDate;
use pango_core::approval::{ApprovalLine, Document, DocumentKind, LineStep, StepKind};
use pango_core::authz::Role;
use pango_core::calendar::DateRange;
use pango_core::id::{
    DocumentId, FormId, LeaveLedgerEntryId, LeaveRequestId, LeaveTypeId, MemberId,
};
use pango_core::leave::{
    Balance, LeaveAmount, LeaveRequestStatus, LeaveUnit, LedgerEntry, LedgerEntryKind,
};
use pango_core::member::MemberStatus;
use pango_core::time::Timestamp;
use pango_db::approval::{self, DocumentRecord, Form};
use pango_db::leave::{self, LeaveRequest, LeaveType, LedgerRecord, RequestFilter};
use pango_db::members::{self, Member};
use pango_db::testing::TestDb;
use pango_db::{Conn, Db};

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis).expect("timestamp")
}

fn date(value: &str) -> NaiveDate {
    value.parse().expect("date")
}

fn range(start: &str, end: &str) -> DateRange {
    DateRange::new(date(start), date(end)).expect("range")
}

/// 구성원 둘(신청자, 결재자), 휴가 종류, 휴가 양식.
struct Fixture {
    test: TestDb,
    member: MemberId,
    approver: MemberId,
    leave_type: LeaveType,
    form: Form,
}

async fn add_member(db: &Db, login_id: &str) -> MemberId {
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
        hired_on: date("2024-03-02"),
        retired_on: None,
        must_change_password: false,
        version: 1,
    };
    let mut tx = db.begin_write().await.expect("begin");
    members::insert(&mut tx, &member, "hash", ts(1))
        .await
        .expect("member");
    tx.commit().await.expect("commit");
    member.id
}

fn leave_type(code: &str, deducts_balance: bool) -> LeaveType {
    LeaveType {
        id: LeaveTypeId::new(),
        code: code.to_owned(),
        name: format!("종류-{code}"),
        deducts_balance,
        allow_partial: deducts_balance,
        active: true,
        sort_order: 0,
        version: 1,
    }
}

async fn fixture() -> Fixture {
    let test = TestDb::new().await;
    let member = add_member(&test, "kim").await;
    let approver = add_member(&test, "lead").await;
    let leave_type = leave_type("annual", true);
    let form = Form {
        id: FormId::new(),
        code: "leave".into(),
        name: "휴가 신청서".into(),
        description: String::new(),
        kind: DocumentKind::Leave,
        body_template: String::new(),
        active: true,
        sort_order: 0,
        version: 1,
    };
    let mut tx = test.begin_write().await.expect("begin");
    leave::insert_type(&mut tx, &leave_type, ts(1))
        .await
        .expect("type");
    approval::insert_form(&mut tx, &form, ts(1))
        .await
        .expect("form");
    tx.commit().await.expect("commit");
    Fixture {
        test,
        member,
        approver,
        leave_type,
        form,
    }
}

impl Fixture {
    /// 신청은 결재 문서를 참조하므로 문서부터 만든다.
    async fn add_request(
        &self,
        conn: &mut Conn,
        member: MemberId,
        range: DateRange,
        status: LeaveRequestStatus,
    ) -> LeaveRequest {
        let line = ApprovalLine::new(
            member,
            vec![LineStep {
                approver: if member == self.approver {
                    self.member
                } else {
                    self.approver
                },
                kind: StepKind::Approve,
            }],
            Vec::new(),
        )
        .expect("line");
        let document = Document::draft(DocumentKind::Leave, line);
        let record = DocumentRecord {
            id: DocumentId::new(),
            doc_no: None,
            form_id: self.form.id,
            kind: DocumentKind::Leave,
            drafter_id: member,
            drafter_department_id: None,
            title: "휴가 신청".into(),
            body: "사유".into(),
            payload: None,
            status: document.status(),
            submitted_at: None,
            completed_at: None,
            created_at: ts(10),
            version: 1,
        };
        approval::insert_document(conn, &record)
            .await
            .expect("document");
        let request = LeaveRequest {
            id: LeaveRequestId::new(),
            member_id: member,
            leave_type_id: self.leave_type.id,
            range,
            unit: LeaveUnit::FullDay,
            amount: LeaveAmount::from_quarters(4),
            reason: "개인 사정".into(),
            status,
            document_id: record.id,
            created_at: ts(10),
            decided_at: None,
            version: 1,
        };
        leave::insert_request(conn, &request)
            .await
            .expect("request");
        request
    }
}

fn ledger_row(member: MemberId, kind: LedgerEntryKind, quarters: i32) -> LedgerRecord {
    LedgerRecord {
        id: LeaveLedgerEntryId::new(),
        member_id: member,
        entry: LedgerEntry {
            kind,
            amount: LeaveAmount::from_quarters(quarters),
        },
        grant_key: None,
        memo: None,
        actor_id: None,
        created_at: ts(20),
    }
}

fn annual_grant(member: MemberId, key: &str) -> LedgerRecord {
    LedgerRecord {
        grant_key: Some(key.to_owned()),
        ..ledger_row(
            member,
            LedgerEntryKind::Grant {
                valid_from: date("2026-03-02"),
                expires_on: Some(date("2027-03-01")),
            },
            60,
        )
    }
}

// ── 휴가 종류 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn leave_type_round_trips_and_keeps_code_and_deduction_fixed() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    assert_eq!(
        leave::find_type(&mut tx, f.leave_type.id)
            .await
            .expect("find"),
        Some(f.leave_type.clone())
    );
    assert_eq!(
        leave::find_type_by_code(&mut tx, "annual")
            .await
            .expect("find"),
        Some(f.leave_type.clone())
    );
    let err = leave::insert_type(&mut tx, &leave_type("annual", false), ts(2))
        .await
        .expect_err("duplicate code");
    assert!(err.is_unique_violation("leave_types.code"), "{err}");
    drop(tx);

    let mut tx = f.test.begin_write().await.expect("begin");
    let edited = LeaveType {
        name: "연차휴가".into(),
        allow_partial: false,
        active: false,
        sort_order: 4,
        code: "ignored".into(),
        deducts_balance: false,
        ..f.leave_type.clone()
    };
    assert!(leave::update_type(&mut tx, &edited, ts(3))
        .await
        .expect("update"));
    assert!(!leave::update_type(&mut tx, &edited, ts(4))
        .await
        .expect("update"));
    let stored = leave::find_type(&mut tx, f.leave_type.id)
        .await
        .expect("find")
        .expect("type");
    assert_eq!(stored.name, "연차휴가");
    assert_eq!(stored.code, "annual");
    assert!(stored.deducts_balance, "차감 여부는 바뀌지 않는다");
    assert!(!stored.allow_partial);
    assert_eq!(stored.version, 2);
    assert!(leave::list_types(&mut tx, true)
        .await
        .expect("list")
        .is_empty());
    assert_eq!(
        leave::list_types(&mut tx, false).await.expect("list"),
        [stored]
    );
}

// ── 신청 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn request_round_trips_and_status_change_needs_the_read_version() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    let request = f
        .add_request(
            &mut tx,
            f.member,
            range("2026-03-10", "2026-03-11"),
            LeaveRequestStatus::Pending,
        )
        .await;
    assert_eq!(
        leave::find_request(&mut tx, request.id)
            .await
            .expect("find"),
        Some(request.clone())
    );
    assert_eq!(
        leave::find_request_by_document(&mut tx, request.document_id)
            .await
            .expect("find"),
        Some(request.clone())
    );
    assert_eq!(
        leave::find_request_by_document(&mut tx, DocumentId::new())
            .await
            .expect("find"),
        None
    );
    assert_eq!(
        leave::count_pending_for_type(&mut tx, f.leave_type.id)
            .await
            .expect("count"),
        1
    );

    let approved = LeaveRequestStatus::Approved;
    assert!(
        leave::set_request_status(&mut tx, request.id, 1, approved, ts(30))
            .await
            .expect("set")
    );
    assert!(
        !leave::set_request_status(&mut tx, request.id, 1, LeaveRequestStatus::Canceled, ts(40))
            .await
            .expect("set"),
        "예전 버전으로는 상태를 바꿀 수 없다"
    );
    let stored = leave::find_request(&mut tx, request.id)
        .await
        .expect("find")
        .expect("request");
    assert_eq!(stored.status, approved);
    assert_eq!(stored.decided_at, Some(ts(30)));
    assert_eq!(stored.version, 2);
    assert_eq!(
        leave::count_pending_for_type(&mut tx, f.leave_type.id)
            .await
            .expect("count"),
        0
    );

    // 문서 하나에 신청은 하나뿐이다.
    let duplicate = LeaveRequest {
        id: LeaveRequestId::new(),
        ..request
    };
    let err = leave::insert_request(&mut tx, &duplicate)
        .await
        .expect_err("same document");
    assert!(
        err.is_unique_violation("leave_requests.document_id"),
        "{err}"
    );
}

#[tokio::test]
async fn request_list_filters_by_member_overlap_and_status() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    let pending = LeaveRequestStatus::Pending;
    let early = f
        .add_request(
            &mut tx,
            f.member,
            range("2026-03-10", "2026-03-11"),
            pending,
        )
        .await;
    let late = f
        .add_request(
            &mut tx,
            f.member,
            range("2026-03-30", "2026-04-02"),
            pending,
        )
        .await;
    let rejected = f
        .add_request(
            &mut tx,
            f.member,
            range("2026-03-16", "2026-03-16"),
            LeaveRequestStatus::Rejected,
        )
        .await;
    let other = f
        .add_request(
            &mut tx,
            f.approver,
            range("2026-03-10", "2026-03-10"),
            pending,
        )
        .await;

    let mut ids = async |filter: RequestFilter, limit: Option<u32>| {
        let (items, has_more) = leave::list_requests(&mut tx, &filter, limit)
            .await
            .expect("list");
        (
            items.iter().map(|request| request.id).collect::<Vec<_>>(),
            has_more,
        )
    };
    let mine = || RequestFilter {
        member_id: Some(f.member),
        ..RequestFilter::default()
    };
    // 최신 접수순.
    assert_eq!(ids(mine(), None).await.0, [rejected.id, late.id, early.id]);
    assert_eq!(
        ids(RequestFilter::default(), None).await.0,
        [other.id, rejected.id, late.id, early.id]
    );
    // 기간은 양 끝을 포함해 겹치는지 본다.
    let overlapping = |start, end| RequestFilter {
        overlapping: Some(range(start, end)),
        ..mine()
    };
    assert_eq!(
        ids(overlapping("2026-03-11", "2026-03-15"), None).await.0,
        [early.id]
    );
    assert_eq!(
        ids(overlapping("2026-03-12", "2026-03-15"), None).await.0,
        []
    );
    assert_eq!(
        ids(overlapping("2026-04-01", "2026-04-30"), None).await.0,
        [late.id],
        "달을 넘는 신청도 잡힌다"
    );
    assert_eq!(
        ids(
            RequestFilter {
                statuses: vec![pending, LeaveRequestStatus::Approved],
                ..overlapping("2026-03-01", "2026-03-31")
            },
            None
        )
        .await
        .0,
        [late.id, early.id]
    );
    // 페이지.
    let (first, more) = ids(mine(), Some(2)).await;
    assert_eq!(
        (first.as_slice(), more),
        ([rejected.id, late.id].as_slice(), true)
    );
    let next = RequestFilter {
        before: first.last().copied(),
        ..mine()
    };
    assert_eq!(ids(next, Some(2)).await, (vec![early.id], false));
}

// ── 원장 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ledger_round_trips_every_kind_in_insertion_order() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    let request = f
        .add_request(
            &mut tx,
            f.member,
            range("2026-03-10", "2026-03-10"),
            LeaveRequestStatus::Pending,
        )
        .await;
    let rows = [
        annual_grant(f.member, "annual:2026-03-02"),
        LedgerRecord {
            actor_id: Some(f.member),
            ..ledger_row(
                f.member,
                LedgerEntryKind::Use {
                    effective_on: date("2026-03-10"),
                    request: request.id,
                },
                4,
            )
        },
        ledger_row(
            f.member,
            LedgerEntryKind::Restore {
                request: request.id,
            },
            4,
        ),
        LedgerRecord {
            memo: Some("출근율 미달".into()),
            actor_id: Some(f.approver),
            ..ledger_row(
                f.member,
                LedgerEntryKind::Adjust {
                    effective_on: date("2026-03-02"),
                },
                -8,
            )
        },
        ledger_row(
            f.member,
            LedgerEntryKind::Grant {
                valid_from: date("2026-03-02"),
                expires_on: None,
            },
            2,
        ),
    ];
    for row in &rows {
        leave::insert_ledger(&mut tx, row).await.expect("ledger");
    }
    let stored = leave::list_ledger(&mut tx, f.member).await.expect("list");
    assert_eq!(stored, rows);
    assert!(leave::list_ledger(&mut tx, f.approver)
        .await
        .expect("list")
        .is_empty());
    assert_eq!(
        leave::find_use(&mut tx, request.id).await.expect("use"),
        Some(rows[1].clone())
    );
    assert_eq!(
        leave::find_use(&mut tx, LeaveRequestId::new())
            .await
            .expect("use"),
        None
    );

    // 저장된 원장으로 잔여가 계산된다: 60 + 2 − 8, 사용은 복원으로 무효.
    let entries: Vec<LedgerEntry> = stored.iter().map(|row| row.entry).collect();
    let balance = Balance::compute(&entries, date("2026-03-10")).expect("balance");
    assert_eq!(balance.remaining.quarters(), 54);
}

#[tokio::test]
async fn automatic_grant_is_inserted_once_per_member_and_key() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    let key = "annual:2026-03-02";
    assert!(
        leave::insert_grant_if_absent(&mut tx, &annual_grant(f.member, key))
            .await
            .expect("grant")
    );
    assert!(
        !leave::insert_grant_if_absent(&mut tx, &annual_grant(f.member, key))
            .await
            .expect("grant"),
        "같은 키는 다시 들어가지 않는다"
    );
    assert!(
        leave::insert_grant_if_absent(&mut tx, &annual_grant(f.approver, key))
            .await
            .expect("grant"),
        "다른 구성원의 같은 키는 별개다"
    );
    assert!(
        leave::insert_grant_if_absent(&mut tx, &annual_grant(f.member, "annual:2027-03-02"))
            .await
            .expect("grant")
    );
    assert_eq!(
        leave::list_ledger(&mut tx, f.member)
            .await
            .expect("list")
            .len(),
        2
    );

    // 조건 없는 삽입은 같은 키를 유일 제약으로 막는다.
    let err = leave::insert_ledger(&mut tx, &annual_grant(f.member, key))
        .await
        .expect_err("duplicate grant");
    assert!(err.is_any_unique_violation(), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_grants_with_the_same_key_leave_one_row() {
    let f = fixture().await;
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let db = f.test.db.clone();
            let row = annual_grant(f.member, "annual:2026-03-02");
            tokio::spawn(async move {
                let mut tx = db.begin_write().await.expect("begin");
                let inserted = leave::insert_grant_if_absent(&mut tx, &row)
                    .await
                    .expect("grant");
                tx.commit().await.expect("commit");
                inserted
            })
        })
        .collect();
    let mut inserted = 0;
    for task in tasks {
        inserted += usize::from(task.await.expect("join"));
    }
    assert_eq!(inserted, 1);
    let mut conn = f.test.read().await.expect("read");
    assert_eq!(
        leave::list_ledger(&mut conn, f.member)
            .await
            .expect("list")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_request_has_at_most_one_use_and_one_restore() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    let request = f
        .add_request(
            &mut tx,
            f.member,
            range("2026-03-10", "2026-03-10"),
            LeaveRequestStatus::Pending,
        )
        .await;
    tx.commit().await.expect("commit");
    let used = || {
        ledger_row(
            f.member,
            LedgerEntryKind::Use {
                effective_on: date("2026-03-10"),
                request: request.id,
            },
            4,
        )
    };
    let restored = || {
        ledger_row(
            f.member,
            LedgerEntryKind::Restore {
                request: request.id,
            },
            4,
        )
    };

    let mut tx = f.test.begin_write().await.expect("begin");
    leave::insert_ledger(&mut tx, &used()).await.expect("use");
    leave::insert_ledger(&mut tx, &restored())
        .await
        .expect("restore");
    tx.commit().await.expect("commit");
    for duplicate in [used(), restored()] {
        let mut tx = f.test.begin_write().await.expect("begin");
        let err = leave::insert_ledger(&mut tx, &duplicate)
            .await
            .expect_err("duplicate");
        assert!(err.is_any_unique_violation(), "{err}");
    }
    let mut conn = f.test.read().await.expect("read");
    assert_eq!(
        leave::list_ledger(&mut conn, f.member)
            .await
            .expect("list")
            .len(),
        2
    );
}

#[tokio::test]
async fn ledger_is_append_only_and_checks_each_kind() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    leave::insert_ledger(&mut tx, &annual_grant(f.member, "annual:2026-03-02"))
        .await
        .expect("grant");
    tx.commit().await.expect("commit");

    let mut tx = f.test.begin_write().await.expect("begin");
    let update = sqlx::query("UPDATE leave_ledger SET amount_q = 4000")
        .execute(&mut *tx)
        .await;
    assert!(update.is_err(), "원장은 고칠 수 없다");
    let delete = sqlx::query("DELETE FROM leave_ledger")
        .execute(&mut *tx)
        .await;
    assert!(delete.is_err(), "원장은 지울 수 없다");
    drop(tx);

    let ghost = LeaveRequestId::new();
    let invalid = [
        // 수량이 0 이하인 부여·사용, 0인 조정.
        ledger_row(
            f.member,
            LedgerEntryKind::Grant {
                valid_from: date("2026-03-02"),
                expires_on: None,
            },
            0,
        ),
        ledger_row(
            f.member,
            LedgerEntryKind::Adjust {
                effective_on: date("2026-03-02"),
            },
            0,
        ),
        // 없는 신청을 가리키는 사용·복원(외래 키).
        ledger_row(
            f.member,
            LedgerEntryKind::Use {
                effective_on: date("2026-03-10"),
                request: ghost,
            },
            4,
        ),
        ledger_row(f.member, LedgerEntryKind::Restore { request: ghost }, 4),
        // 없는 구성원.
        annual_grant(MemberId::new(), "annual:2026-03-02"),
    ];
    for row in invalid {
        let mut tx = f.test.begin_write().await.expect("begin");
        assert!(
            leave::insert_ledger(&mut tx, &row).await.is_err(),
            "{:?}",
            row.entry
        );
    }
    let mut conn = f.test.read().await.expect("read");
    assert_eq!(
        leave::list_ledger(&mut conn, f.member)
            .await
            .expect("list")
            .len(),
        1
    );
}
