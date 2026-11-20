//! 근태 정정 요청 저장소 통합 테스트.

use chrono::NaiveDate;
use pango_core::approval::{DocumentKind, DocumentStatus};
use pango_core::attendance::{CorrectionStatus, WorkType};
use pango_core::authz::Role;
use pango_core::id::{AttendanceCorrectionId, DocumentId, FormId, MemberId};
use pango_core::member::MemberStatus;
use pango_core::time::Timestamp;
use pango_db::approval::{self, DocumentRecord, Form};
use pango_db::attendance_corrections::{self as corrections, AttendanceCorrection};
use pango_db::members::{self, Member};
use pango_db::testing::TestDb;
use pango_db::Conn;

const HOUR: i64 = 3_600_000;
/// 2026-03-02 00:00Z = 09:00 KST.
const MAR_2: i64 = 1_772_409_600_000;

fn ts(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis).expect("timestamp")
}

fn date(value: &str) -> NaiveDate {
    value.parse().expect("date")
}

struct Fixture {
    test: TestDb,
    kim: MemberId,
    lee: MemberId,
    form: FormId,
}

async fn fixture() -> Fixture {
    let test = TestDb::new().await;
    let mut tx = test.begin_write().await.expect("begin");
    let mut ids = Vec::new();
    for login_id in ["kim", "lee"] {
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
            created_at: ts(1),
            version: 1,
        };
        members::insert(&mut tx, &member, "hash", ts(1))
            .await
            .expect("member");
        ids.push(member.id);
    }
    let form = Form::plain(
        FormId::new(),
        "attendance_correction",
        "근태 정정 신청서",
        DocumentKind::AttendanceCorrection,
    );
    approval::insert_form(&mut tx, &form, ts(1))
        .await
        .expect("form");
    tx.commit().await.expect("commit");
    Fixture {
        test,
        kim: ids[0],
        lee: ids[1],
        form: form.id,
    }
}

impl Fixture {
    /// 요청은 결재 문서를 참조하므로 문서부터 만든다.
    async fn document(&self, conn: &mut Conn, drafter: MemberId) -> DocumentId {
        let record = DocumentRecord {
            id: DocumentId::new(),
            doc_no: None,
            form_id: self.form,
            kind: DocumentKind::AttendanceCorrection,
            drafter_id: drafter,
            drafter_department_id: None,
            title: "근태 정정 요청".into(),
            body: "사유".into(),
            payload: None,
            status: DocumentStatus::Draft,
            submitted_at: None,
            completed_at: None,
            created_at: ts(10),
            version: 1,
        };
        approval::insert_document(conn, &record)
            .await
            .expect("document");
        record.id
    }

    async fn correction(
        &self,
        conn: &mut Conn,
        member: MemberId,
        work_date: &str,
    ) -> AttendanceCorrection {
        AttendanceCorrection {
            id: AttendanceCorrectionId::new(),
            member_id: member,
            work_date: date(work_date),
            clock_in_at: ts(MAR_2),
            clock_out_at: Some(ts(MAR_2 + 9 * HOUR)),
            work_type: WorkType::Remote,
            reason: "출근 기록 누락".into(),
            base_record_version: Some(3),
            status: CorrectionStatus::Pending,
            document_id: self.document(conn, member).await,
            created_at: ts(MAR_2 + 24 * HOUR),
            decided_at: None,
            version: 1,
        }
    }
}

#[tokio::test]
async fn insert_and_find_round_trip() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    let stored = f.correction(&mut tx, f.kim, "2026-03-02").await;
    corrections::insert(&mut tx, &stored).await.expect("insert");
    let open = AttendanceCorrection {
        clock_out_at: None,
        base_record_version: None,
        ..f.correction(&mut tx, f.kim, "2026-03-03").await
    };
    corrections::insert(&mut tx, &open).await.expect("insert");
    tx.commit().await.expect("commit");

    let mut conn = f.test.read().await.expect("read");
    assert_eq!(
        corrections::find(&mut conn, stored.id).await.expect("find"),
        Some(stored.clone())
    );
    assert_eq!(
        corrections::find(&mut conn, open.id).await.expect("find"),
        Some(open.clone())
    );
    assert_eq!(
        corrections::find_by_document(&mut conn, stored.document_id)
            .await
            .expect("find"),
        Some(stored.clone())
    );
    assert_eq!(
        corrections::find_pending(&mut conn, f.kim, date("2026-03-02"))
            .await
            .expect("find"),
        Some(stored)
    );
    assert_eq!(
        corrections::find(&mut conn, AttendanceCorrectionId::new())
            .await
            .expect("find"),
        None
    );
    assert_eq!(
        corrections::find_by_document(&mut conn, DocumentId::new())
            .await
            .expect("find"),
        None
    );
    assert_eq!(
        corrections::find_pending(&mut conn, f.lee, date("2026-03-02"))
            .await
            .expect("find"),
        None
    );
}

#[tokio::test]
async fn only_one_pending_request_per_member_and_work_date() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    let first = f.correction(&mut tx, f.kim, "2026-03-02").await;
    corrections::insert(&mut tx, &first).await.expect("insert");

    let duplicate = f.correction(&mut tx, f.kim, "2026-03-02").await;
    let error = corrections::insert(&mut tx, &duplicate)
        .await
        .expect_err("duplicate pending");
    assert!(corrections::is_duplicate_pending(&error), "{error}");

    // 다른 날, 다른 구성원은 상관없다.
    let other_day = f.correction(&mut tx, f.kim, "2026-03-03").await;
    corrections::insert(&mut tx, &other_day)
        .await
        .expect("other day");
    let other_member = f.correction(&mut tx, f.lee, "2026-03-02").await;
    corrections::insert(&mut tx, &other_member)
        .await
        .expect("other member");

    // 끝난 요청은 유일 인덱스에서 빠지므로 다시 넣을 수 있다.
    assert!(
        corrections::set_status(&mut tx, first.id, 1, CorrectionStatus::Rejected, ts(MAR_2))
            .await
            .expect("status")
    );
    corrections::insert(&mut tx, &duplicate)
        .await
        .expect("after rejection");
    assert_eq!(corrections::count_pending(&mut tx).await.expect("count"), 3);
    assert_eq!(
        corrections::find_pending(&mut tx, f.kim, date("2026-03-02"))
            .await
            .expect("find")
            .map(|c| c.id),
        Some(duplicate.id)
    );

    // 문서 하나에 요청 하나. 이 위반은 "결재 중 중복"으로 보지 않는다.
    let same_document = AttendanceCorrection {
        id: AttendanceCorrectionId::new(),
        work_date: date("2026-03-04"),
        ..other_day.clone()
    };
    let error = corrections::insert(&mut tx, &same_document)
        .await
        .expect_err("same document");
    assert!(error.is_any_unique_violation());
    assert!(!corrections::is_duplicate_pending(&error));
}

#[tokio::test]
async fn set_status_requires_the_current_version() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    let stored = f.correction(&mut tx, f.kim, "2026-03-02").await;
    corrections::insert(&mut tx, &stored).await.expect("insert");

    let decided = ts(MAR_2 + 30 * HOUR);
    assert!(
        !corrections::set_status(&mut tx, stored.id, 2, CorrectionStatus::Approved, decided)
            .await
            .expect("stale"),
        "버전이 다르면 바꾸지 않는다"
    );
    assert!(
        corrections::set_status(&mut tx, stored.id, 1, CorrectionStatus::Approved, decided)
            .await
            .expect("status")
    );
    assert!(
        !corrections::set_status(&mut tx, stored.id, 1, CorrectionStatus::Canceled, decided)
            .await
            .expect("stale"),
        "같은 버전으로 두 번 바꿀 수 없다"
    );
    let found = corrections::find(&mut tx, stored.id)
        .await
        .expect("find")
        .expect("correction");
    assert_eq!(found.status, CorrectionStatus::Approved);
    assert_eq!(found.decided_at, Some(decided));
    assert_eq!(found.version, 2);
    assert_eq!(corrections::count_pending(&mut tx).await.expect("count"), 0);
}

#[tokio::test]
async fn list_for_member_is_newest_first_and_paginated() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    let mut ids = Vec::new();
    for day in ["2026-02-25", "2026-02-26", "2026-02-27"] {
        let correction = f.correction(&mut tx, f.kim, day).await;
        corrections::insert(&mut tx, &correction)
            .await
            .expect("insert");
        ids.push(correction.id);
    }
    let other = f.correction(&mut tx, f.lee, "2026-02-27").await;
    corrections::insert(&mut tx, &other).await.expect("insert");
    tx.commit().await.expect("commit");

    let mut conn = f.test.read().await.expect("read");
    let (first, more) = corrections::list_for_member(&mut conn, f.kim, None, 2)
        .await
        .expect("list");
    assert_eq!(
        first.iter().map(|c| c.id).collect::<Vec<_>>(),
        [ids[2], ids[1]]
    );
    assert!(more);
    let (second, more) = corrections::list_for_member(&mut conn, f.kim, Some(ids[1]), 2)
        .await
        .expect("list");
    assert_eq!(second.iter().map(|c| c.id).collect::<Vec<_>>(), [ids[0]]);
    assert!(!more);
    let (theirs, _) = corrections::list_for_member(&mut conn, f.lee, None, 50)
        .await
        .expect("list");
    assert_eq!(theirs, [other]);
    let (nobody, more) = corrections::list_for_member(&mut conn, MemberId::new(), None, 50)
        .await
        .expect("list");
    assert!(nobody.is_empty() && !more);
}

#[tokio::test]
async fn schema_rejects_inconsistent_rows() {
    let f = fixture().await;
    let mut tx = f.test.begin_write().await.expect("begin");
    let valid = f.correction(&mut tx, f.kim, "2026-03-02").await;

    // 퇴근이 출근보다 늦지 않다.
    let backwards = AttendanceCorrection {
        clock_out_at: Some(valid.clock_in_at),
        ..valid.clone()
    };
    assert!(corrections::insert(&mut tx, &backwards).await.is_err());
    // 결재 중인데 결정 시각이 있다 / 끝났는데 결정 시각이 없다.
    let decided_but_pending = AttendanceCorrection {
        decided_at: Some(ts(MAR_2)),
        ..valid.clone()
    };
    assert!(corrections::insert(&mut tx, &decided_but_pending)
        .await
        .is_err());
    let closed_without_time = AttendanceCorrection {
        status: CorrectionStatus::Approved,
        ..valid.clone()
    };
    assert!(corrections::insert(&mut tx, &closed_without_time)
        .await
        .is_err());
    // 없는 구성원·문서는 참조할 수 없다.
    let unknown_member = AttendanceCorrection {
        member_id: MemberId::new(),
        ..valid.clone()
    };
    let error = corrections::insert(&mut tx, &unknown_member)
        .await
        .expect_err("fk");
    assert!(error.is_foreign_key_violation());
    let unknown_document = AttendanceCorrection {
        document_id: DocumentId::new(),
        ..valid.clone()
    };
    let error = corrections::insert(&mut tx, &unknown_document)
        .await
        .expect_err("fk");
    assert!(error.is_foreign_key_violation());

    corrections::insert(&mut tx, &valid).await.expect("valid");
    assert_eq!(corrections::count_pending(&mut tx).await.expect("count"), 1);
}
