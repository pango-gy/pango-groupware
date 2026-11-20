//! 회사 설정·공휴일 통합 테스트.

use std::collections::BTreeSet;
use std::sync::Arc;

use http::StatusCode;
use pango_core::authz::Role;
use pango_core::calendar::DateRange;
use pango_db::audit::{self, AuditFilter};
use pango_db::settings::{self, CompanySettings, Holiday};
use pango_server::testing::TestApp;
use serde_json::{json, Value};

async fn stored_settings(app: &TestApp) -> CompanySettings {
    let mut conn = app.state.db.read().await.expect("conn");
    settings::get(&mut conn)
        .await
        .expect("get")
        .expect("settings")
}

async fn stored_holidays(app: &TestApp) -> Vec<Holiday> {
    let mut conn = app.state.db.read().await.expect("conn");
    // 테스트는 2025~2027년만 쓰므로 그 범위를 연도별로 모은다(한 번에 최대 366일).
    let mut all = Vec::new();
    for year in 2025..=2027 {
        let range = DateRange::new(
            chrono::NaiveDate::from_ymd_opt(year, 1, 1).expect("date"),
            chrono::NaiveDate::from_ymd_opt(year, 12, 31).expect("date"),
        )
        .expect("range");
        all.extend(
            settings::list_holidays(&mut conn, range)
                .await
                .expect("list"),
        );
    }
    all
}

async fn audit_actions(app: &TestApp, prefix: &str) -> Vec<String> {
    let mut conn = app.state.db.read().await.expect("conn");
    let (entries, _) = audit::list(&mut conn, &AuditFilter::default(), 200)
        .await
        .expect("audit");
    entries
        .into_iter()
        .map(|e| e.action)
        .filter(|a| a.starts_with(prefix))
        .collect()
}

async fn admin_settings(app: &TestApp, token: &str) -> Value {
    app.get("/admin/settings")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

// ── 회사 설정 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn members_see_only_the_public_company_subset() {
    let app = TestApp::new().await;
    let member = app.add_member("member1", Role::Member, None).await;
    let body = app
        .get("/settings/company")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        body,
        json!({
            "company_name": "팡고", "timezone": "Asia/Seoul",
            "work_start": "09:00", "work_end": "18:00",
            "workdays": 31, "day_cutoff": "05:00",
        })
    );
    app.get("/settings/company")
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
}

#[tokio::test]
async fn admin_reads_full_settings_but_members_cannot() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let member = app.add_member("member1", Role::Member, None).await;
    let full = admin_settings(&app, &admin.token).await;
    assert_eq!(full, admin_settings(&app, &owner).await);
    assert_eq!(full["version"], 1);
    assert_eq!(
        full["break_tiers"],
        json!([
            { "min_span_minutes": 240, "break_minutes": 30 },
            { "min_span_minutes": 480, "break_minutes": 60 },
        ])
    );
    assert_eq!(full["half_day_split"], "14:00");
    app.get("/admin/settings")
        .bearer(&member.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    let mut put = full.clone();
    put["company_name"] = json!("탈취");
    app.put("/admin/settings")
        .bearer(&member.token)
        .json(&put)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    assert_eq!(stored_settings(&app).await.company_name, "팡고");
}

/// 현재 설정에 `changes`를 덮어쓴 PUT 본문.
fn edited(current: &Value, changes: Value) -> Value {
    let mut body = current.clone();
    if let (Some(body), Some(changes)) = (body.as_object_mut(), changes.as_object()) {
        for (key, value) in changes {
            body.insert(key.clone(), value.clone());
        }
    }
    body.as_object_mut().map(|b| b.remove("updated_at"));
    body
}

#[tokio::test]
async fn valid_update_bumps_version_and_audits_changed_keys() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let current = admin_settings(&app, &owner).await;
    let body = edited(
        &current,
        json!({ "company_name": "  팡고 주식회사 ", "work_start": "08:30", "late_grace_minutes": 10 }),
    );
    let updated = app
        .put("/admin/settings")
        .bearer(&owner)
        .json(&body)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(updated["version"], 2);
    assert_eq!(
        updated["company_name"], "팡고 주식회사",
        "이름은 정리해 저장한다"
    );
    assert_eq!(updated["work_start"], "08:30");
    let stored = stored_settings(&app).await;
    assert_eq!((stored.version, stored.late_grace_minutes), (2, 10));

    let mut conn = app.state.db.read().await.expect("conn");
    let filter = AuditFilter {
        action: Some("settings.update".into()),
        ..Default::default()
    };
    let (entries, _) = audit::list(&mut conn, &filter, 10).await.expect("audit");
    assert_eq!(entries.len(), 1);
    let detail = entries[0].detail.clone().expect("detail");
    let changed: BTreeSet<&str> = detail["changed"]
        .as_object()
        .expect("changed")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        changed,
        BTreeSet::from(["company_name", "work_start", "late_grace_minutes"])
    );
    assert_eq!(
        detail["changed"]["work_start"],
        json!({ "before": "09:00", "after": "08:30" })
    );
}

#[tokio::test]
async fn invalid_combinations_are_rejected_and_change_nothing() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let current = admin_settings(&app, &owner).await;
    let tiers = |pairs: &[(u16, u16)]| {
        json!(pairs
            .iter()
            .map(|(span, rest)| json!({ "min_span_minutes": span, "break_minutes": rest }))
            .collect::<Vec<_>>())
    };
    let cases = [
        (json!({ "work_end": "09:00" }), "work_end"),
        (json!({ "work_end": "08:00" }), "work_end"),
        (json!({ "half_day_split": "09:00" }), "half_day_split"),
        (json!({ "half_day_split": "18:00" }), "half_day_split"),
        (json!({ "half_day_split": "20:00" }), "half_day_split"),
        (json!({ "day_cutoff": "12:01" }), "day_cutoff"),
        (json!({ "timezone": "Mars/Olympus" }), "timezone"),
        (json!({ "timezone": "" }), "timezone"),
        (json!({ "workdays": 0 }), "workdays"),
        (json!({ "workdays": 128 }), "workdays"),
        (json!({ "late_grace_minutes": 121 }), "late_grace_minutes"),
        (json!({ "quarter_minutes": 0 }), "quarter_minutes"),
        (
            json!({ "break_tiers": tiers(&[(480, 60), (240, 30)]) }),
            "break_tiers",
        ),
        (
            json!({ "break_tiers": tiers(&[(240, 30), (240, 60)]) }),
            "break_tiers",
        ),
        (json!({ "break_tiers": tiers(&[(60, 60)]) }), "break_tiers"),
        (
            json!({ "break_tiers": tiers(&[(60, 1), (120, 2), (180, 3), (240, 4), (300, 5), (360, 6), (420, 7)]) }),
            "break_tiers",
        ),
        (json!({ "company_name": "   " }), "company_name"),
        (json!({ "company_name": "가".repeat(51) }), "company_name"),
        (json!({ "company_name": "팡고\u{202E}" }), "company_name"),
    ];
    for (changes, field) in cases {
        let body = edited(&current, changes.clone());
        let response = app
            .put("/admin/settings")
            .bearer(&owner)
            .json(&body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{changes}");
        assert_eq!(response.error_code(), "validation_failed", "{changes}");
        assert_eq!(response.body["error"]["field"], field, "{changes}");
    }
    let malformed = [
        json!({ "work_start": "25:00" }),
        json!({ "work_start": "9:00" }),
        json!({ "workdays": 256 }),
        json!({ "workdays": -1 }),
        json!({ "late_grace_minutes": "10" }),
        json!({ "version": "1" }),
    ];
    for changes in malformed {
        let body = edited(&current, changes.clone());
        app.put("/admin/settings")
            .bearer(&owner)
            .json(&body)
            .send()
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    assert_eq!(
        stored_settings(&app).await.version,
        1,
        "거부된 요청은 아무것도 바꾸지 않는다"
    );
    assert!(audit_actions(&app, "settings.").await.is_empty());

    // 경계값(전환 12:00, 유예 120분, 모든 요일, 반반차 = 근무 구간 - 1분)은 받는다.
    let boundary = edited(
        &current,
        json!({
            "day_cutoff": "12:00", "late_grace_minutes": 120, "workdays": 127,
            "quarter_minutes": 539,
        }),
    );
    app.put("/admin/settings")
        .bearer(&owner)
        .json(&boundary)
        .send()
        .await
        .expect(StatusCode::OK);
}

#[tokio::test]
async fn stale_version_is_a_conflict() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let current = admin_settings(&app, &owner).await;
    let body = edited(&current, json!({ "company_name": "첫 번째" }));
    app.put("/admin/settings")
        .bearer(&owner)
        .json(&body)
        .send()
        .await
        .expect(StatusCode::OK);
    let late = edited(&current, json!({ "company_name": "두 번째" }));
    app.put("/admin/settings")
        .bearer(&owner)
        .json(&late)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    assert_eq!(stored_settings(&app).await.company_name, "첫 번째");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_updates_with_same_version_have_exactly_one_winner() {
    let app = Arc::new(TestApp::new().await);
    let owner = app.login_owner().await;
    let current = admin_settings(&app, &owner).await;
    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let (app, token) = (app.clone(), owner.clone());
            let body = edited(&current, json!({ "company_name": format!("회사 {i}") }));
            tokio::spawn(async move {
                app.put("/admin/settings")
                    .bearer(&token)
                    .json(&body)
                    .send()
                    .await
                    .status
            })
        })
        .collect();
    let mut statuses = Vec::new();
    for task in tasks {
        statuses.push(task.await.expect("join"));
    }
    assert_eq!(statuses.iter().filter(|s| **s == StatusCode::OK).count(), 1);
    assert!(statuses
        .iter()
        .all(|s| *s == StatusCode::OK || *s == StatusCode::CONFLICT));
    assert_eq!(stored_settings(&app).await.version, 2);
    assert_eq!(audit_actions(&app, "settings.update").await.len(), 1);
}

// ── 공휴일 ──────────────────────────────────────────────────────────────

async fn holidays(app: &TestApp, token: &str, query: &str) -> Value {
    app.get(&format!("/holidays{query}"))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

#[tokio::test]
async fn holiday_upsert_is_idempotent_and_renames() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let member = app.add_member("member1", Role::Member, None).await;
    for _ in 0..2 {
        let body = app
            .put("/admin/holidays/2026-03-01")
            .bearer(&owner)
            .json(&json!({ "name": "삼일절" }))
            .send()
            .await
            .expect(StatusCode::OK);
        assert_eq!(body, json!({ "date": "2026-03-01", "name": "삼일절" }));
    }
    assert_eq!(
        holidays(&app, &member.token, "?year=2026").await,
        json!([{ "date": "2026-03-01", "name": "삼일절" }])
    );
    app.put("/admin/holidays/2026-03-01")
        .bearer(&owner)
        .json(&json!({ "name": "3·1절" }))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        holidays(&app, &member.token, "?year=2026").await,
        json!([{ "date": "2026-03-01", "name": "3·1절" }])
    );
    assert_eq!(
        audit_actions(&app, "holiday.").await,
        vec!["holiday.upsert", "holiday.upsert"],
        "바뀐 것이 없는 재요청은 기록하지 않는다"
    );
}

#[tokio::test]
async fn holiday_delete_is_idempotent() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    app.put("/admin/holidays/2026-05-05")
        .bearer(&owner)
        .json(&json!({ "name": "어린이날" }))
        .send()
        .await
        .expect(StatusCode::OK);
    for _ in 0..2 {
        app.delete("/admin/holidays/2026-05-05")
            .bearer(&owner)
            .send()
            .await
            .expect(StatusCode::OK);
    }
    app.delete("/admin/holidays/2031-01-01")
        .bearer(&owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert!(stored_holidays(&app).await.is_empty());
    assert_eq!(
        audit_actions(&app, "holiday.").await,
        vec!["holiday.delete", "holiday.upsert"]
    );
}

fn import_items(count: usize) -> Vec<Value> {
    (0..count)
        .map(|i| {
            let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("date")
                + chrono::Duration::days(i as i64);
            json!({ "date": date.format("%Y-%m-%d").to_string(), "name": format!("휴일 {i}") })
        })
        .collect()
}

#[tokio::test]
async fn holiday_import_is_all_or_nothing() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let invalid_items = [
        json!({ "date": "2026-02-30", "name": "없는 날" }),
        json!({ "date": "2026-2-3", "name": "짧은 표기" }),
        json!({ "date": "2026-12-31", "name": "" }),
        json!({ "date": "2026-12-31", "name": "이름\u{0007}" }),
        json!({ "date": "2026-01-01", "name": "중복 날짜" }),
    ];
    for bad in invalid_items {
        let mut items = import_items(99);
        items.push(bad.clone());
        let response = app
            .post("/admin/holidays/import")
            .bearer(&owner)
            .json(&json!({ "items": items }))
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(response.error_code(), "validation_failed", "{bad}");
        assert!(
            stored_holidays(&app).await.is_empty(),
            "{bad}: 하나도 들어가면 안 된다"
        );
    }

    // 검증은 통과했지만 저장 도중(50번째 항목)에 DB가 실패해도 전부 롤백된다.
    let mut tx = app.state.db.begin_write().await.expect("begin");
    sqlx::query(
        "CREATE TRIGGER fail_holiday BEFORE INSERT ON holidays
         WHEN NEW.date = '2026-02-19' BEGIN SELECT RAISE(ABORT, 'boom'); END",
    )
    .execute(&mut *tx)
    .await
    .expect("trigger");
    tx.commit().await.expect("commit");
    app.post("/admin/holidays/import")
        .bearer(&owner)
        .json(&json!({ "items": import_items(100) }))
        .send()
        .await
        .expect_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
    assert!(
        stored_holidays(&app).await.is_empty(),
        "부분 실패도 전부 롤백"
    );
    let mut tx = app.state.db.begin_write().await.expect("begin");
    sqlx::query("DROP TRIGGER fail_holiday")
        .execute(&mut *tx)
        .await
        .expect("drop");
    tx.commit().await.expect("commit");

    let ok = app
        .post("/admin/holidays/import")
        .bearer(&owner)
        .json(&json!({ "items": import_items(100) }))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(ok["imported"], 100);
    assert_eq!(stored_holidays(&app).await.len(), 100);
    assert_eq!(
        audit_actions(&app, "holiday.").await,
        vec!["holiday.import"]
    );
}

#[tokio::test]
async fn holiday_import_rejects_more_than_100_items() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let response = app
        .post("/admin/holidays/import")
        .bearer(&owner)
        .json(&json!({ "items": import_items(101) }))
        .send()
        .await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert_eq!(response.body["error"]["field"], "items");
    assert_eq!(response.body["error"]["reason"], "too_long");
    assert!(stored_holidays(&app).await.is_empty());
    app.post("/admin/holidays/import")
        .bearer(&owner)
        .json(&json!({ "items": "2026-01-01" }))
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
}

#[tokio::test]
async fn holiday_paths_require_strict_dates_and_valid_names() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    for date in [
        "2026-02-30",
        "2026-2-3",
        "20260203",
        "2026-02-03T00:00",
        "today",
        "%27--",
    ] {
        let response = app
            .put(&format!("/admin/holidays/{date}"))
            .bearer(&owner)
            .json(&json!({ "name": "휴일" }))
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{date}");
        assert_eq!(response.body["error"]["field"], "date", "{date}");
        let response = app
            .delete(&format!("/admin/holidays/{date}"))
            .bearer(&owner)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{date}");
    }
    for name in [json!(""), json!("가".repeat(51)), json!("휴\u{200B}일")] {
        let response = app
            .put("/admin/holidays/2026-06-06")
            .bearer(&owner)
            .json(&json!({ "name": name }))
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{name}");
        assert_eq!(response.body["error"]["field"], "name");
    }
    app.put("/admin/holidays/2026-06-06")
        .bearer(&owner)
        .json(&json!({ "name": 6 }))
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    assert!(stored_holidays(&app).await.is_empty());
}

#[tokio::test]
async fn holiday_year_filter_boundaries() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    for date in ["2025-12-31", "2026-01-01", "2026-12-31", "2027-01-01"] {
        app.put(&format!("/admin/holidays/{date}"))
            .bearer(&owner)
            .json(&json!({ "name": format!("{date} 휴일") }))
            .send()
            .await
            .expect(StatusCode::OK);
    }
    let dates = |body: Value| -> Vec<String> {
        body.as_array()
            .expect("list")
            .iter()
            .map(|h| h["date"].as_str().expect("date").to_owned())
            .collect()
    };
    assert_eq!(
        dates(holidays(&app, &owner, "?year=2026").await),
        vec!["2026-01-01", "2026-12-31"]
    );
    assert_eq!(
        dates(holidays(&app, &owner, "?year=2025").await),
        vec!["2025-12-31"]
    );
    assert_eq!(
        dates(holidays(&app, &owner, "").await),
        vec!["2026-01-01", "2026-12-31"],
        "생략하면 올해(테스트 시계 2026년)"
    );
    for bad in ["abc", "2026.5", "", "99999999999"] {
        app.get(&format!("/holidays?year={bad}"))
            .bearer(&owner)
            .send()
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    for out_of_range in ["0", "-1", "999", "10000"] {
        let response = app
            .get(&format!("/holidays?year={out_of_range}"))
            .bearer(&owner)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "{out_of_range}");
        assert_eq!(response.body["error"]["field"], "year", "{out_of_range}");
    }
}

#[tokio::test]
async fn members_cannot_change_holidays() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let member = app.add_member("member1", Role::Member, None).await;
    app.put("/admin/holidays/2026-08-15")
        .bearer(&owner)
        .json(&json!({ "name": "광복절" }))
        .send()
        .await
        .expect(StatusCode::OK);
    app.put("/admin/holidays/2026-08-15")
        .bearer(&member.token)
        .json(&json!({ "name": "바꿈" }))
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    app.delete("/admin/holidays/2026-08-15")
        .bearer(&member.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    app.post("/admin/holidays/import")
        .bearer(&member.token)
        .json(&json!({ "items": import_items(3) }))
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    let stored = stored_holidays(&app).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].name, "광복절");
}
