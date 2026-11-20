//! 결재 양식(분류·필드 정의·값) 통합 테스트.
//!
//! 필드 종류별 규칙의 전수 검사는 `pango_core::form_schema`의 단위 테스트가 한다. 여기서는
//! HTTP에서 그 규칙이 실제로 적용되는지, 저장·상신·열람·양식 관리가 맞물리는지를 본다.

use std::sync::Arc;

use http::StatusCode;
use pango_core::authz::Role;
use pango_core::id::MemberId;
use pango_core::member::MemberStatus;
use pango_server::service::form_catalog::ensure_form_catalog;
use pango_server::testing::{id_of, TestApp, TestMember, TestResponse};
use serde_json::{json, Value};

struct World {
    app: Arc<TestApp>,
    owner: String,
    drafter: TestMember,
    a1: TestMember,
    a2: TestMember,
    finance: TestMember,
    outsider: TestMember,
}

async fn world() -> World {
    let app = Arc::new(TestApp::new().await);
    let owner = app.login_owner().await;
    let add = |login: &'static str| app.add_member(login, Role::Member, None);
    let (drafter, a1, a2) = (
        add("drafter").await,
        add("first").await,
        add("second").await,
    );
    let (finance, outsider) = (add("finance").await, add("outsider").await);
    World {
        app,
        owner,
        drafter,
        a1,
        a2,
        finance,
        outsider,
    }
}

/// 모든 필드 종류가 하나씩 있는 양식 정의.
fn all_kinds() -> Value {
    json!({ "fields": [
        { "key": "subject", "label": "건명", "kind": "text", "required": true, "max_len": 20 },
        { "key": "detail", "label": "내용", "kind": "textarea" },
        { "key": "count", "label": "수량", "kind": "number", "min": 1, "max": 10 },
        { "key": "amount", "label": "금액", "kind": "money" },
        { "key": "day", "label": "일자", "kind": "date" },
        { "key": "period", "label": "기간", "kind": "date_range" },
        { "key": "hours", "label": "시간", "kind": "time_range" },
        { "key": "method", "label": "구분", "kind": "select", "options": ["법인카드", "개인경비"] },
        { "key": "agree", "label": "확인", "kind": "checkbox" },
        { "key": "handover", "label": "인수인계자", "kind": "member" },
        { "key": "items", "label": "내역", "kind": "table", "columns": [
            { "key": "on", "label": "일자", "kind": "date", "required": true },
            { "key": "place", "label": "사용처", "kind": "text" },
            { "key": "price", "label": "금액", "kind": "money", "required": true }
        ]}
    ]})
}

async fn post_form(app: &TestApp, token: &str, body: &Value) -> TestResponse {
    app.post("/admin/approval/forms")
        .bearer(token)
        .json(body)
        .send()
        .await
}

/// 본문 없이 필드만 쓰는 양식을 만든다.
async fn make_form(w: &World, code: &str, schema: Value) -> Value {
    let body = json!({
        "code": code, "name": format!("양식 {code}"), "category": "accounting",
        "schema": schema, "use_body": false,
    });
    post_form(&w.app, &w.owner, &body)
        .await
        .expect(StatusCode::CREATED)
}

async fn patch_form(w: &World, form: &Value, change: Value) -> TestResponse {
    let mut body = json!({
        "version": form["version"], "name": form["name"], "active": form["active"],
        "description": form["description"], "body_template": form["body_template"],
        "sort_order": form["sort_order"],
    });
    for (key, value) in change.as_object().expect("object") {
        body[key] = value.clone();
    }
    w.app
        .patch(&format!("/admin/approval/forms/{}", id_of(form)))
        .bearer(&w.owner)
        .json(&body)
        .send()
        .await
}

fn doc(w: &World, form: &Value, values: Value, submit: bool) -> Value {
    json!({
        "form_id": form["id"], "title": "지출 정산", "values": values,
        "steps": [{ "approver_id": w.a1.id, "kind": "approve" }],
        "submit": submit,
    })
}

async fn send_doc(w: &World, body: &Value) -> TestResponse {
    w.app
        .post("/approval/documents")
        .bearer(&w.drafter.token)
        .json(body)
        .send()
        .await
}

async fn create(w: &World, form: &Value, values: Value, submit: bool) -> Value {
    send_doc(w, &doc(w, form, values, submit))
        .await
        .expect(StatusCode::CREATED)
}

async fn submit(w: &World, id: &str) -> TestResponse {
    w.app
        .post(&format!("/approval/documents/{id}/submit"))
        .bearer(&w.drafter.token)
        .send()
        .await
}

async fn documents(w: &World) -> i64 {
    w.app.count("SELECT COUNT(*) FROM approval_documents").await
}

/// 오류 응답의 `fields`를 `(key, row, column, reason)`으로.
fn field_errors(response: TestResponse) -> Vec<(String, Option<u64>, Option<String>, String)> {
    assert_eq!(
        (response.status, response.error_code()),
        (StatusCode::BAD_REQUEST, "validation_failed"),
        "body: {}",
        response.body
    );
    let error = &response.body["error"];
    let fields: Vec<_> = error["fields"]
        .as_array()
        .unwrap_or_else(|| panic!("no fields: {}", response.body))
        .iter()
        .map(|field| {
            (
                field["key"].as_str().expect("key").to_owned(),
                field["row"].as_u64(),
                field["column"].as_str().map(str::to_owned),
                field["reason"].as_str().expect("reason").to_owned(),
            )
        })
        .collect();
    // 예전 클라이언트가 읽는 단일 필드에는 첫 오류가 들어 있다.
    assert_eq!(error["field"], json!(fields[0].0));
    assert_eq!(error["reason"], json!(fields[0].3));
    fields
}

fn plain(key: &str, reason: &str) -> (String, Option<u64>, Option<String>, String) {
    (key.to_owned(), None, None, reason.to_owned())
}

fn cell(
    key: &str,
    row: u64,
    column: &str,
    reason: &str,
) -> (String, Option<u64>, Option<String>, String) {
    (
        key.to_owned(),
        Some(row),
        Some(column.to_owned()),
        reason.to_owned(),
    )
}

// ── 기본 양식(카탈로그) ───────────────────────────────────────────────────

#[tokio::test]
async fn catalog_is_seeded_and_listed_by_category() {
    let w = world().await;
    let groups = w
        .app
        .get("/approval/forms")
        .bearer(&w.drafter.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let summary: Vec<(&str, &str, usize)> = groups
        .as_array()
        .expect("groups")
        .iter()
        .map(|group| {
            (
                group["category"].as_str().expect("category"),
                group["category_label"].as_str().expect("label"),
                group["forms"].as_array().expect("forms").len(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("general", "일반", 3),
            ("hr", "인사", 9),
            ("attendance", "근태", 3),
            ("trip", "출장", 2),
            ("accounting", "회계", 2),
            ("support", "지원", 5),
            ("welfare", "복지", 3),
        ]
    );
    let names: Vec<&str> = groups[0]["forms"]
        .as_array()
        .expect("forms")
        .iter()
        .map(|form| form["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["업무기안", "업무협조", "품의서"]);

    // 양식 하나의 모양: 화면이 이것만으로 입력란을 그린다.
    let expense = w
        .app
        .approval_form(&w.drafter.token, "expense_report")
        .await;
    assert_eq!(expense["name"], "지출결의서");
    assert_eq!(expense["kind"], "general");
    assert_eq!(expense["use_body"], false);
    assert_eq!(expense["schema_version"], 1);
    assert_eq!(expense["system"], false);
    assert_eq!(expense["route"], "approval_new");
    let fields = expense["schema"]["fields"].as_array().expect("fields");
    assert_eq!(
        fields[1],
        json!({
            "key": "pay_type", "label": "지출 구분", "kind": "select",
            "options": ["법인카드", "개인경비"], "required": true
        })
    );
    assert_eq!(fields[2]["kind"], "table");
    assert_eq!(
        fields[2]["columns"][3],
        json!({
            "key": "amount", "label": "금액", "kind": "money", "required": true
        })
    );
    assert!(fields[2]["help"].as_str().expect("help").contains("증빙"));
    // 관리용 값은 기안 화면에 내려가지 않는다.
    for hidden in [
        "active",
        "sort_order",
        "version",
        "default_reference_ids",
        "origin",
    ] {
        assert!(expense.get(hidden).is_none(), "{hidden}");
    }
}

#[tokio::test]
async fn system_forms_point_to_their_own_screens_and_cannot_be_drafted() {
    let w = world().await;
    for (code, category, route) in [
        ("leave", "hr", "leave_new"),
        ("attendance_correction", "attendance", "attendance"),
    ] {
        let form = w.app.approval_form(&w.drafter.token, code).await;
        assert_eq!(form["system"], true, "{code}");
        assert_eq!(form["route"], route);
        assert_eq!(form["schema"], json!({ "fields": [] }));
        send_doc(&w, &doc(&w, &form, json!({}), false))
            .await
            .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
        let admin = admin_form(&w, code).await;
        assert_eq!(admin["category"], category);
        assert_eq!(admin["origin"], "catalog");
    }
    assert_eq!(documents(&w).await, 0);
}

async fn admin_forms(w: &World) -> Vec<Value> {
    w.app
        .get("/admin/approval/forms")
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK)
        .as_array()
        .expect("forms")
        .clone()
}

async fn admin_form(w: &World, code: &str) -> Value {
    admin_forms(w)
        .await
        .into_iter()
        .find(|form| form["code"] == code)
        .unwrap_or_else(|| panic!("form {code}"))
}

#[tokio::test]
async fn ensuring_the_catalog_twice_changes_nothing_and_keeps_admin_edits() {
    let w = world().await;
    // 관리자가 기본 양식 하나를 고치고, 하나는 꺼 둔다.
    let trip = admin_form(&w, "domestic_trip").await;
    let edited = patch_form(
        &w,
        &trip,
        json!({ "name": "출장 신청(국내)", "schema": { "fields": [
            { "key": "where_to", "label": "행선지", "kind": "text", "required": true }
        ]}}),
    )
    .await
    .expect(StatusCode::OK);
    let resignation = admin_form(&w, "resignation").await;
    patch_form(&w, &resignation, json!({ "active": false }))
        .await
        .expect(StatusCode::OK);
    let before = admin_forms(&w).await;
    assert_eq!(before.len(), 27);

    for _ in 0..2 {
        let added = ensure_form_catalog(&w.app.state).await.expect("ensure");
        assert!(added.is_empty(), "{added:?}");
    }
    assert_eq!(admin_forms(&w).await, before);
    assert_eq!(admin_form(&w, "domestic_trip").await, edited);
    assert_eq!(admin_form(&w, "resignation").await["active"], false);
    assert!(w
        .app
        .approval_forms(&w.drafter.token)
        .await
        .iter()
        .all(|form| form["code"] != "resignation"));
    assert!(w
        .app
        .audit_entries("approval_form.catalog_seed")
        .await
        .is_empty());
}

#[tokio::test]
async fn an_older_database_gets_the_missing_catalog_forms_on_startup() {
    let w = world().await;
    // 이 기능 이전의 회사: 처음의 세 양식만 있고, 그중 품의서는 이름을 고쳐 쓰고 있다.
    w.app
        .execute(
            "DELETE FROM approval_forms
             WHERE code NOT IN ('general', 'leave', 'attendance_correction')",
        )
        .await;
    w.app
        .execute("UPDATE approval_forms SET name = '우리 품의서' WHERE code = 'general'")
        .await;
    let general_id = id_of(&admin_form(&w, "general").await);

    let added = ensure_form_catalog(&w.app.state).await.expect("ensure");
    assert_eq!(added.len(), 24);
    assert!(added.contains(&"expense_report".to_owned()));
    let forms = admin_forms(&w).await;
    assert_eq!(forms.len(), 27);
    let general = admin_form(&w, "general").await;
    assert_eq!(
        (id_of(&general), &general["name"]),
        (general_id, &json!("우리 품의서"))
    );
    let audits = w.app.audit_entries("approval_form.catalog_seed").await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor_id, None);
    assert_eq!(
        audits[0].detail.as_ref().expect("detail")["codes"],
        json!(added)
    );
    // 다시 시작해도 더 넣지 않는다.
    assert!(ensure_form_catalog(&w.app.state)
        .await
        .expect("ensure")
        .is_empty());
    assert_eq!(
        w.app
            .audit_entries("approval_form.catalog_seed")
            .await
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_catalog_runs_insert_each_form_once() {
    let w = world().await;
    w.app
        .execute(
            "DELETE FROM approval_forms
             WHERE code NOT IN ('general', 'leave', 'attendance_correction')",
        )
        .await;
    let runs: Vec<_> = (0..8)
        .map(|_| {
            let app = w.app.clone();
            tokio::spawn(async move { ensure_form_catalog(&app.state).await.expect("ensure") })
        })
        .collect();
    let mut added = 0;
    for run in runs {
        added += run.await.expect("join").len();
    }
    assert_eq!(added, 24, "각 양식은 한 실행에서만 들어간다");
    assert_eq!(w.app.count("SELECT COUNT(*) FROM approval_forms").await, 27);
    assert_eq!(
        w.app
            .count("SELECT COUNT(DISTINCT code) FROM approval_forms")
            .await,
        27
    );
}

// ── 값: 해피 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_field_kind_is_stored_normalized_and_shown_in_the_snapshot() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    let values = json!({
        "subject": "  \u{1112}\u{1161}\u{11AB}글 정산 ",
        "detail": "첫 줄\r\n둘째 줄",
        "count": 10,
        "amount": 1_000_000_000_000_i64,
        "day": "2026-03-02",
        "period": { "start": "2026-01-01", "end": "2027-01-01" },
        "hours": { "start": "09:00", "end": "18:30" },
        "method": "법인카드",
        "agree": true,
        "handover": w.a2.id,
        "items": [
            { "on": "2026-03-02", "place": "식당", "price": 30000 },
            { "on": "2026-03-03", "price": 12000 }
        ]
    });
    let created = create(&w, &form, values, true).await;
    assert_eq!(created["status"], "in_progress");
    assert_eq!(created["body"], "");
    assert_eq!(created["payload"], Value::Null);
    let snapshot = &created["form_snapshot"];
    assert_eq!(snapshot["category"], "accounting");
    assert_eq!(snapshot["category_label"], "회계");
    assert_eq!(snapshot["use_body"], false);
    assert_eq!(snapshot["schema_version"], 1);
    assert_eq!(snapshot["schema"], form["schema"]);
    assert_eq!(
        snapshot["values"],
        json!({
            "subject": "한글 정산",
            "detail": "첫 줄\n둘째 줄",
            "count": 10,
            "amount": 1_000_000_000_000_i64,
            "day": "2026-03-02",
            "period": { "start": "2026-01-01", "end": "2027-01-01" },
            "hours": { "start": "09:00", "end": "18:30" },
            "method": "법인카드",
            "agree": true,
            "handover": w.a2.id,
            "items": [
                { "on": "2026-03-02", "place": "식당", "price": 30000 },
                { "on": "2026-03-03", "price": 12000 }
            ]
        })
    );
    assert_eq!(
        snapshot["computed"],
        json!({ "period": { "days": 366 }, "items": { "totals": { "price": 42000 } } })
    );
    assert_eq!(
        snapshot["member_names"],
        json!({ w.a2.id.to_string(): "second 님" })
    );

    // 결재자가 보는 내용도 같다.
    let seen = w.app.document(&w.a1.token, &id_of(&created)).await;
    assert_eq!(seen["form_snapshot"], created["form_snapshot"]);
    // 문서를 볼 수 없는 사람에게는 값이 새지 않는다.
    w.app
        .get(&format!("/approval/documents/{}", id_of(&created)))
        .bearer(&w.outsider.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn only_required_values_are_needed() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    let created = create(
        &w,
        &form,
        json!({ "subject": "제목", "detail": " ", "day": null, "items": [] }),
        true,
    )
    .await;
    assert_eq!(
        created["form_snapshot"]["values"],
        json!({ "subject": "제목" })
    );
    assert_eq!(created["form_snapshot"]["computed"], json!({}));
    assert_eq!(created["form_snapshot"]["member_names"], json!({}));
}

#[tokio::test]
async fn catalog_expense_report_is_filed_with_totals() {
    let w = world().await;
    let form = w
        .app
        .approval_form(&w.drafter.token, "expense_report")
        .await;
    let created = create(
        &w,
        &form,
        json!({
            "spent_on": "2026-03-02", "pay_type": "법인카드",
            "items": [
                { "used_on": "2026-02-27", "vendor": "택시", "account": "교통비", "amount": 18000 },
                { "used_on": "2026-02-28", "vendor": "식당", "account": "식대", "amount": 42000, "note": "고객 미팅" }
            ]
        }),
        true,
    )
    .await;
    assert_eq!(created["form_name"], "지출결의서");
    assert_eq!(
        created["form_snapshot"]["computed"]["items"]["totals"]["amount"],
        60000
    );
    assert!(created["doc_no"].as_str().is_some());
}

// ── 값: 거부 ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn boundaries_are_enforced_over_http() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    let with = |extra: Value| {
        let mut values = json!({ "subject": "제목" });
        for (key, value) in extra.as_object().expect("object") {
            values[key] = value.clone();
        }
        doc(&w, &form, values, true)
    };
    let row = json!({ "on": "2026-03-02", "price": 1 });
    let cases = [
        (
            json!({ "subject": "가".repeat(21) }),
            plain("subject", "too_long"),
        ),
        (
            json!({ "detail": "가".repeat(5001) }),
            plain("detail", "too_long"),
        ),
        (json!({ "count": 0 }), plain("count", "out_of_range")),
        (json!({ "count": 11 }), plain("count", "out_of_range")),
        (json!({ "amount": -1 }), plain("amount", "out_of_range")),
        (
            json!({ "amount": 1_000_000_000_001_i64 }),
            plain("amount", "out_of_range"),
        ),
        (
            json!({ "period": { "start": "2026-01-01", "end": "2027-01-02" } }),
            plain("period", "out_of_range"),
        ),
        (
            json!({ "period": { "start": "2026-03-02", "end": "2026-03-01" } }),
            plain("period", "inconsistent"),
        ),
        (
            json!({ "hours": { "start": "18:00", "end": "18:00" } }),
            plain("hours", "inconsistent"),
        ),
        (
            json!({ "items": vec![row.clone(); 51] }),
            plain("items", "too_long"),
        ),
    ];
    for (extra, expected) in cases {
        assert_eq!(
            field_errors(send_doc(&w, &with(extra.clone())).await),
            vec![expected],
            "{extra}"
        );
    }
    assert_eq!(documents(&w).await, 0);
    // 경계 안쪽은 받는다.
    let ok = with(json!({
        "subject": "가".repeat(20), "detail": "가".repeat(5000), "count": 1, "amount": 0,
        "items": vec![row; 50],
    }));
    let created = send_doc(&w, &ok).await.expect(StatusCode::CREATED);
    assert_eq!(
        created["form_snapshot"]["computed"]["items"]["totals"]["price"],
        50
    );
}

#[tokio::test]
async fn missing_unknown_and_mistyped_values_are_rejected() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    let reject =
        async |values: Value| field_errors(send_doc(&w, &doc(&w, &form, values, false)).await);

    assert_eq!(reject(json!({})).await, vec![plain("subject", "empty")]);
    // `values`를 아예 보내지 않아도 필수 필드는 필수다.
    let mut without = doc(&w, &form, json!({}), false);
    without.as_object_mut().expect("object").remove("values");
    assert_eq!(
        field_errors(send_doc(&w, &without).await),
        vec![plain("subject", "empty")]
    );
    assert_eq!(
        reject(json!({ "subject": "제목", "memo": "x", "__proto__": {} })).await,
        vec![
            plain("__proto__", "unknown_field"),
            plain("memo", "unknown_field")
        ]
    );
    for values in [json!([]), json!("subject=제목"), json!(1), json!(true)] {
        assert_eq!(
            reject(values).await,
            vec![plain("values", "invalid_format")]
        );
    }
    let wrong_types = json!({
        "subject": 1, "detail": ["a"], "count": "3", "amount": 1.5, "day": 20260302,
        "period": "2026-03-02", "hours": { "start": "9:00", "end": "18:00" },
        "method": 0, "agree": "yes", "handover": "someone", "items": { "on": "2026-03-02" }
    });
    assert_eq!(
        reject(wrong_types).await,
        [
            "subject", "detail", "count", "amount", "day", "period", "hours", "method", "agree",
            "handover", "items"
        ]
        .map(|key| plain(key, "invalid_format"))
    );
    let mut nested = json!("leaf");
    for depth in 0..40 {
        nested = if depth % 2 == 0 {
            json!({ "start": nested, "end": "x" })
        } else {
            json!([nested])
        };
    }
    assert_eq!(
        reject(json!({ "subject": nested.clone(), "period": nested.clone(), "items": [nested] }))
            .await,
        vec![
            plain("subject", "invalid_format"),
            plain("period", "invalid_format"),
            (
                "items".to_owned(),
                Some(0),
                None,
                "invalid_format".to_owned()
            ),
        ]
    );
    assert_eq!(documents(&w).await, 0);
}

#[tokio::test]
async fn control_and_bidi_characters_are_rejected_and_text_is_nfc() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    for bad in [
        "a\u{0000}b",
        "a\nb",
        "결재\u{202E}승인",
        "a\u{200B}b",
        "\u{001B}[2J",
    ] {
        assert_eq!(
            field_errors(send_doc(&w, &doc(&w, &form, json!({ "subject": bad }), false)).await),
            vec![plain("subject", "invalid_character")],
            "{bad:?}"
        );
    }
    assert_eq!(
        field_errors(
            send_doc(
                &w,
                &doc(
                    &w,
                    &form,
                    json!({ "subject": "제목", "detail": "a\u{202E}b", "items": [
                        { "on": "2026-03-02", "price": 1, "place": "x\u{0007}" }
                    ]}),
                    false
                )
            )
            .await
        ),
        vec![
            plain("detail", "invalid_character"),
            cell("items", 0, "place", "invalid_character")
        ]
    );
    assert_eq!(documents(&w).await, 0);
    // 스크립트·SQL 조각은 글자 그대로 저장된다.
    let script = "<script>alert(1)</script>'; --";
    let created = create(
        &w,
        &form,
        json!({ "subject": "x' OR '1'='1", "detail": script, "method": "\u{1100}\u{1162}\u{110B}\u{1175}\u{11AB}\u{1100}\u{1167}\u{11BC}\u{1107}\u{1175}" }),
        false,
    )
    .await;
    let values = &created["form_snapshot"]["values"];
    assert_eq!(values["subject"], "x' OR '1'='1");
    assert_eq!(values["detail"], script);
    assert_eq!(values["method"], "개인경비", "분해형 한글도 같은 선택지다");
}

#[tokio::test]
async fn select_value_must_be_one_of_the_options() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    for bad in ["현금", "법인", "법인카드 개인경비", ""] {
        let response = send_doc(
            &w,
            &doc(
                &w,
                &form,
                json!({ "subject": "제목", "method": bad }),
                false,
            ),
        )
        .await;
        if bad.is_empty() {
            // 빈 값은 입력하지 않은 것이고, 이 필드는 필수가 아니다.
            response.expect(StatusCode::CREATED);
        } else {
            assert_eq!(
                field_errors(response),
                vec![plain("method", "not_an_option")]
            );
        }
    }
}

#[tokio::test]
async fn member_field_requires_an_existing_active_member() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    let with = |member: Value| {
        doc(
            &w,
            &form,
            json!({ "subject": "제목", "handover": member }),
            false,
        )
    };
    assert_eq!(
        field_errors(send_doc(&w, &with(json!(MemberId::new()))).await),
        vec![plain("handover", "not_found")]
    );
    w.app.set_status(w.a2.id, MemberStatus::Suspended).await;
    assert_eq!(
        field_errors(send_doc(&w, &with(json!(w.a2.id))).await),
        vec![plain("handover", "member_inactive")]
    );
    assert_eq!(documents(&w).await, 0);
    // 본인을 지정하는 것은 막지 않는다(양식의 뜻은 양식이 정한다).
    let created = send_doc(&w, &with(json!(w.drafter.id)))
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(
        created["form_snapshot"]["member_names"],
        json!({ w.drafter.id.to_string(): "drafter 님" })
    );
}

#[tokio::test]
async fn all_field_errors_come_back_together_with_rows_and_columns() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    let response = send_doc(
        &w,
        &doc(
            &w,
            &form,
            json!({
                "count": 99,
                "day": "2026-02-30",
                "method": "수표",
                "items": [
                    { "on": "2026-03-02", "price": 1000 },
                    { "place": "식당", "price": "만원", "tip": 1 },
                    "행"
                ],
                "extra": true
            }),
            true,
        ),
    )
    .await;
    assert_eq!(
        response.body["error"]["message"],
        "validation failed on 9 field(s)"
    );
    assert_eq!(
        field_errors(response),
        vec![
            plain("subject", "empty"),
            plain("count", "out_of_range"),
            plain("day", "invalid_format"),
            plain("method", "not_an_option"),
            cell("items", 1, "on", "empty"),
            cell("items", 1, "price", "invalid_format"),
            cell("items", 1, "tip", "unknown_field"),
            (
                "items".to_owned(),
                Some(2),
                None,
                "invalid_format".to_owned()
            ),
            plain("extra", "unknown_field"),
        ]
    );
    assert_eq!(documents(&w).await, 0);
    assert_eq!(w.app.count("SELECT COUNT(*) FROM approval_steps").await, 0);
    assert_eq!(w.app.notification_count(w.a1.id, "approval.todo").await, 0);
}

#[tokio::test]
async fn other_errors_keep_the_single_field_shape() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    let mut body = doc(&w, &form, json!({ "subject": "제목" }), false);
    body["title"] = json!("");
    let response = send_doc(&w, &body).await;
    assert_eq!(response.status, StatusCode::BAD_REQUEST);
    assert_eq!(response.body["error"]["field"], "title");
    assert_eq!(response.body["error"]["reason"], "empty");
    assert!(response.body["error"].get("fields").is_none());
}

// ── 본문 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn body_is_required_only_when_the_form_uses_it() {
    let w = world().await;
    // 본문을 쓰는 양식: 필드와 본문 둘 다 받는다.
    let draft_form = w
        .app
        .approval_form(&w.drafter.token, "work_cooperation")
        .await;
    assert_eq!(draft_form["use_body"], true);
    let values = json!({ "target_dept": "재무팀" });
    let mut body = doc(&w, &draft_form, values.clone(), false);
    let response = send_doc(&w, &body).await;
    assert_eq!(response.body["error"]["field"], "body");
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    body["body"] = json!("  ");
    send_doc(&w, &body)
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    body["body"] = json!("협조 부탁드립니다.");
    let created = send_doc(&w, &body).await.expect(StatusCode::CREATED);
    assert_eq!(created["body"], "협조 부탁드립니다.");
    assert_eq!(created["form_snapshot"]["use_body"], true);
    assert_eq!(created["form_snapshot"]["values"], values);

    // 본문을 쓰지 않는 양식: 없어도 되고, 보내면 같은 규칙으로 검증해 남긴다.
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    let mut body = doc(&w, &form, json!({ "subject": "제목" }), false);
    assert_eq!(
        send_doc(&w, &body).await.expect(StatusCode::CREATED)["body"],
        ""
    );
    body["body"] = json!("덧붙임");
    assert_eq!(
        send_doc(&w, &body).await.expect(StatusCode::CREATED)["body"],
        "덧붙임"
    );
    body["body"] = json!("a\u{0000}b");
    send_doc(&w, &body)
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
}

#[tokio::test]
async fn forms_without_fields_still_work_with_the_old_request_shape() {
    let w = world().await;
    let general = w.app.approval_form(&w.drafter.token, "general").await;
    let body = json!({
        "form_id": general["id"], "title": "품의", "body": "본문",
        "steps": [{ "approver_id": w.a1.id, "kind": "approve" }], "submit": true,
    });
    let created = send_doc(&w, &body).await.expect(StatusCode::CREATED);
    assert_eq!(created["payload"], Value::Null);
    assert_eq!(
        created["form_snapshot"],
        json!({
            "category": "general", "category_label": "일반", "use_body": true,
            "schema_version": 1, "schema": { "fields": [] }, "values": {}, "computed": {},
            "member_names": {}
        })
    );
    // 필드가 없는 양식에 값을 보내면 모르는 키다.
    let mut with_values = body.clone();
    with_values["values"] = json!({ "amount": 1 });
    assert_eq!(
        field_errors(send_doc(&w, &with_values).await),
        vec![plain("amount", "unknown_field")]
    );
}

#[tokio::test]
async fn documents_filed_before_this_feature_show_an_empty_snapshot_and_can_be_submitted() {
    let w = world().await;
    let general = w.app.approval_form(&w.drafter.token, "general").await;
    let body = json!({
        "form_id": general["id"], "title": "예전 초안", "body": "본문",
        "steps": [{ "approver_id": w.a1.id, "kind": "approve" }],
    });
    let id = id_of(&send_doc(&w, &body).await.expect(StatusCode::CREATED));
    w.app
        .execute("UPDATE approval_documents SET payload = NULL")
        .await;
    let detail = w.app.document(&w.drafter.token, &id).await;
    assert_eq!(detail["form_snapshot"]["schema_version"], 0);
    assert_eq!(detail["form_snapshot"]["schema"], json!({ "fields": [] }));
    assert_eq!(detail["form_snapshot"]["values"], json!({}));
    let sent = submit(&w, &id).await.expect(StatusCode::OK);
    assert_eq!(sent["status"], "in_progress");
    assert_eq!(sent["form_snapshot"]["schema_version"], 1);
}

// ── 임시저장 → 수정 → 상신 ───────────────────────────────────────────────

async fn patch_doc(w: &World, id: &str, version: i64, change: Value) -> TestResponse {
    let mut body = json!({
        "version": version, "title": "지출 정산",
        "steps": [{ "approver_id": w.a1.id, "kind": "approve" }],
    });
    for (key, value) in change.as_object().expect("object") {
        body[key] = value.clone();
    }
    w.app
        .patch(&format!("/approval/documents/{id}"))
        .bearer(&w.drafter.token)
        .json(&body)
        .send()
        .await
}

#[tokio::test]
async fn draft_values_are_edited_then_submitted() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    let draft = create(
        &w,
        &form,
        json!({ "subject": "초안", "amount": 100 }),
        false,
    )
    .await;
    let id = id_of(&draft);
    assert_eq!(
        (draft["status"].as_str(), draft["version"].as_i64()),
        (Some("draft"), Some(1))
    );

    // 값을 통째로 바꾼다(보내지 않은 필드는 사라진다).
    let edited = patch_doc(
        &w,
        &id,
        1,
        json!({ "values": { "subject": "고친 건명", "items": [{ "on": "2026-03-02", "price": 700 }] } }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(edited["version"], 2);
    assert_eq!(
        edited["form_snapshot"]["values"],
        json!({ "subject": "고친 건명", "items": [{ "on": "2026-03-02", "price": 700 }] })
    );
    assert_eq!(
        edited["form_snapshot"]["computed"],
        json!({ "items": { "totals": { "price": 700 } } })
    );

    // 잘못된 값으로는 고쳐지지 않는다.
    assert_eq!(
        field_errors(patch_doc(&w, &id, 2, json!({ "values": { "amount": -5 } })).await),
        vec![plain("subject", "empty"), plain("amount", "out_of_range")]
    );
    // `values`를 생략하면 저장된 값을 그대로 둔다(제목만 고치는 예전 클라이언트).
    let retitled = patch_doc(&w, &id, 2, json!({ "title": "지출 정산(3월)" }))
        .await
        .expect(StatusCode::OK);
    assert_eq!(retitled["title"], "지출 정산(3월)");
    assert_eq!(
        retitled["form_snapshot"]["values"],
        edited["form_snapshot"]["values"]
    );
    // 낡은 버전으로는 고칠 수 없다.
    patch_doc(&w, &id, 2, json!({ "values": { "subject": "늦음" } }))
        .await
        .expect_error(StatusCode::CONFLICT, "version_conflict");
    // 다른 사람은 고칠 수 없다.
    w.app
        .patch(&format!("/approval/documents/{id}"))
        .bearer(&w.outsider.token)
        .json(&json!({
            "version": 3, "title": "가로채기", "values": { "subject": "x" },
            "steps": [{ "approver_id": w.a1.id, "kind": "approve" }],
        }))
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");

    let sent = submit(&w, &id).await.expect(StatusCode::OK);
    assert_eq!(sent["status"], "in_progress");
    assert_eq!(
        sent["form_snapshot"]["values"],
        edited["form_snapshot"]["values"]
    );
    // 상신한 뒤에는 값을 고칠 수 없다.
    patch_doc(
        &w,
        &id,
        sent["version"].as_i64().expect("version"),
        json!({ "values": { "subject": "x" } }),
    )
    .await
    .expect_error(StatusCode::CONFLICT, "document_not_editable");
    // 상신을 다시 눌러도 그대로다.
    let again = submit(&w, &id).await.expect(StatusCode::OK);
    assert_eq!(again["version"], sent["version"]);
    assert_eq!(w.app.notification_count(w.a1.id, "approval.todo").await, 1);
}

#[tokio::test]
async fn create_with_values_honours_the_idempotency_key() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    let send = |values: Value| {
        w.app
            .post("/approval/documents")
            .bearer(&w.drafter.token)
            .header("idempotency-key", "expense-1")
            .json(&doc(&w, &form, values, true))
            .send()
    };
    let first = send(json!({ "subject": "택시비", "amount": 18000 }))
        .await
        .expect(StatusCode::CREATED);
    let again = send(json!({ "subject": "택시비", "amount": 18000 }))
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(first, again);
    send(json!({ "subject": "택시비", "amount": 99000 }))
        .await
        .expect_error(StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused");
    assert_eq!(documents(&w).await, 1);
    assert_eq!(w.app.notification_count(w.a1.id, "approval.todo").await, 1);

    // 검증에 실패한 요청은 키를 쓰지 않으므로, 고쳐서 같은 키로 다시 보낼 수 있다.
    let retry = |values: Value| {
        w.app
            .post("/approval/documents")
            .bearer(&w.drafter.token)
            .header("idempotency-key", "expense-2")
            .json(&doc(&w, &form, values, true))
            .send()
    };
    field_errors(retry(json!({ "amount": 1 })).await);
    w.app
        .post("/approval/documents")
        .bearer(&w.drafter.token)
        .header("idempotency-key", "expense-3")
        .json(&doc(&w, &form, json!({ "subject": "두 번째" }), true))
        .send()
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(documents(&w).await, 2);
}

// ── 양식이 바뀐 뒤 ───────────────────────────────────────────────────────

fn two_fields() -> Value {
    json!({ "fields": [
        { "key": "vendor", "label": "거래처", "kind": "text", "required": true },
        { "key": "amount", "label": "금액", "kind": "money" }
    ]})
}

#[tokio::test]
async fn schema_edits_do_not_change_how_filed_documents_render() {
    let w = world().await;
    let form = make_form(&w, "purchase", two_fields()).await;
    let filed = create(
        &w,
        &form,
        json!({ "vendor": "팡고상사", "amount": 5000 }),
        true,
    )
    .await;
    let id = id_of(&filed);

    // 필드를 바꾸고(금액 삭제, 사유 추가) 분류와 이름도 바꾼다.
    let edited = patch_form(
        &w,
        &form,
        json!({ "name": "구매 요청", "category": "support", "schema": { "fields": [
            { "key": "vendor", "label": "공급사", "kind": "text", "required": true },
            { "key": "reason", "label": "사유", "kind": "textarea", "required": true }
        ]}}),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(edited["schema_version"], 2);
    assert_eq!(edited["version"], 2);

    let after = w.app.document(&w.drafter.token, &id).await;
    assert_eq!(after["form_snapshot"], filed["form_snapshot"]);
    assert_eq!(after["form_snapshot"]["schema_version"], 1);
    assert_eq!(after["form_snapshot"]["category"], "accounting");
    assert_eq!(
        after["form_snapshot"]["schema"]["fields"][0]["label"],
        "거래처"
    );
    assert_eq!(after["form_snapshot"]["values"]["amount"], 5000);
    // 이름은 문서가 아니라 양식의 것이라 따라 바뀐다.
    assert_eq!(after["form_name"], "구매 요청");
    // 결재도 그대로 진행된다.
    w.app
        .post(&format!("/approval/documents/{id}/approve"))
        .bearer(&w.a1.token)
        .json(&json!({}))
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        w.app.document(&w.drafter.token, &id).await["form_snapshot"],
        filed["form_snapshot"]
    );

    // 새 문서는 새 정의를 따른다.
    let current = w.app.approval_form(&w.drafter.token, "purchase").await;
    assert_eq!(
        field_errors(
            send_doc(
                &w,
                &doc(
                    &w,
                    &current,
                    json!({ "vendor": "팡고상사", "amount": 1 }),
                    true
                )
            )
            .await
        ),
        vec![plain("reason", "empty"), plain("amount", "unknown_field")]
    );
    let new = create(
        &w,
        &current,
        json!({ "vendor": "팡고상사", "reason": "교체" }),
        true,
    )
    .await;
    assert_eq!(new["form_snapshot"]["schema_version"], 2);
    assert_eq!(new["form_snapshot"]["category"], "support");
}

#[tokio::test]
async fn schema_version_only_moves_when_the_fields_change() {
    let w = world().await;
    let form = make_form(&w, "purchase", two_fields()).await;
    // 같은 정의를 다시 보내거나 다른 값만 고치면 그대로다.
    let same = patch_form(&w, &form, json!({ "schema": two_fields(), "name": "구매" }))
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        (same["schema_version"].as_i64(), same["version"].as_i64()),
        (Some(1), Some(2))
    );
    let omitted = patch_form(&w, &same, json!({ "sort_order": 3 }))
        .await
        .expect(StatusCode::OK);
    assert_eq!(omitted["schema_version"], 1);
    assert_eq!(omitted["schema"], form["schema"], "생략한 정의는 그대로다");
    assert_eq!(omitted["category"], "accounting");
    assert_eq!(omitted["use_body"], false);
    let changed = patch_form(
        &w,
        &omitted,
        json!({ "schema": { "fields": [
            { "key": "vendor", "label": "거래처", "kind": "text", "required": true, "max_len": 50 },
            { "key": "amount", "label": "금액", "kind": "money" }
        ]}}),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(changed["schema_version"], 2);
    let audits = w.app.audit_entries("approval_form.update").await;
    assert_eq!(audits.len(), 3);
    assert_eq!(
        audits[0].detail.as_ref().expect("detail")["schema_version"],
        2
    );
}

#[tokio::test]
async fn a_draft_is_checked_against_the_current_schema_when_submitted() {
    let w = world().await;
    let form = make_form(&w, "purchase", two_fields()).await;
    let draft = create(
        &w,
        &form,
        json!({ "vendor": "팡고상사", "amount": 5000 }),
        false,
    )
    .await;
    let id = id_of(&draft);
    patch_form(
        &w,
        &form,
        json!({ "schema": { "fields": [
            { "key": "vendor", "label": "거래처", "kind": "text", "required": true },
            { "key": "reason", "label": "사유", "kind": "textarea", "required": true }
        ]}}),
    )
    .await
    .expect(StatusCode::OK);

    // 예전 정의로 저장한 값은 그대로 보이지만, 그대로는 상신할 수 없다.
    assert_eq!(
        field_errors(submit(&w, &id).await),
        vec![plain("reason", "empty"), plain("amount", "unknown_field")]
    );
    let still = w.app.document(&w.drafter.token, &id).await;
    assert_eq!(still["status"], "draft");
    assert_eq!(still["form_snapshot"], draft["form_snapshot"]);
    assert_eq!(still["doc_no"], Value::Null);
    assert_eq!(w.app.notification_count(w.a1.id, "approval.todo").await, 0);

    // 지금 정의에 맞게 고치면 상신되고, 문서는 새 정의를 갖는다.
    patch_doc(
        &w,
        &id,
        1,
        json!({ "values": { "vendor": "팡고상사", "reason": "교체" } }),
    )
    .await
    .expect(StatusCode::OK);
    let sent = submit(&w, &id).await.expect(StatusCode::OK);
    assert_eq!(sent["status"], "in_progress");
    assert_eq!(sent["form_snapshot"]["schema_version"], 2);
    assert_eq!(
        sent["form_snapshot"]["values"],
        json!({ "vendor": "팡고상사", "reason": "교체" })
    );
}

#[tokio::test]
async fn a_draft_whose_values_still_fit_takes_the_new_schema_at_submit() {
    let w = world().await;
    let form = make_form(&w, "purchase", two_fields()).await;
    let draft = create(&w, &form, json!({ "vendor": "팡고상사" }), false).await;
    patch_form(
        &w,
        &form,
        json!({ "schema": { "fields": [
            { "key": "vendor", "label": "공급사", "kind": "text", "required": true }
        ]}}),
    )
    .await
    .expect(StatusCode::OK);
    let sent = submit(&w, &id_of(&draft)).await.expect(StatusCode::OK);
    assert_eq!(sent["form_snapshot"]["schema_version"], 2);
    assert_eq!(
        sent["form_snapshot"]["schema"]["fields"][0]["label"],
        "공급사"
    );
}

#[tokio::test]
async fn a_member_who_left_after_the_draft_blocks_submission() {
    let w = world().await;
    let form = make_form(&w, "all_kinds", all_kinds()).await;
    let draft = create(
        &w,
        &form,
        json!({ "subject": "인수인계", "handover": w.a2.id }),
        false,
    )
    .await;
    let id = id_of(&draft);
    w.app.set_status(w.a2.id, MemberStatus::Retired).await;
    assert_eq!(
        field_errors(submit(&w, &id).await),
        vec![plain("handover", "member_inactive")]
    );
    assert_eq!(
        w.app.document(&w.drafter.token, &id).await["status"],
        "draft"
    );
    // 이미 낸 문서는 그 사람이 퇴사해도 이름까지 그대로 보인다.
    w.app.set_status(w.a2.id, MemberStatus::Active).await;
    let sent = submit(&w, &id).await.expect(StatusCode::OK);
    w.app.set_status(w.a2.id, MemberStatus::Retired).await;
    assert_eq!(
        w.app.document(&w.a1.token, &id).await["form_snapshot"],
        sent["form_snapshot"]
    );
}

#[tokio::test]
async fn submission_rolls_back_completely_when_a_later_step_fails() {
    let w = world().await;
    let form = make_form(&w, "purchase", two_fields()).await;
    let with_reference = patch_form(
        &w,
        &form,
        json!({ "default_reference_ids": [w.finance.id] }),
    )
    .await
    .expect(StatusCode::OK);
    let draft = create(&w, &form, json!({ "vendor": "팡고상사" }), false).await;
    let id = id_of(&draft);
    // 상신 도중에 정의가 바뀐 것처럼 만들어, 내용 확정·참조자 추가 뒤의 저장을 실패시킨다.
    patch_form(
        &w,
        &with_reference,
        json!({ "schema": { "fields": [
            { "key": "vendor", "label": "공급사", "kind": "text", "required": true }
        ]}}),
    )
    .await
    .expect(StatusCode::OK);
    w.app
        .execute(
            "CREATE TRIGGER fail_events BEFORE INSERT ON approval_events
             BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .await;
    let response = submit(&w, &id).await;
    assert_eq!(response.status, StatusCode::INTERNAL_SERVER_ERROR);
    w.app.execute("DROP TRIGGER fail_events").await;

    let after = w.app.document(&w.drafter.token, &id).await;
    assert_eq!(after, draft, "아무것도 바뀌지 않았다");
    assert_eq!(
        w.app
            .notification_count(w.finance.id, "approval.reference")
            .await,
        0
    );
    assert_eq!(
        w.app
            .count("SELECT COALESCE(MAX(last_seq), 0) FROM doc_counters")
            .await,
        0
    );
    // 장애가 걷히면 그대로 상신된다.
    let sent = submit(&w, &id).await.expect(StatusCode::OK);
    assert_eq!(
        sent["doc_no"].as_str().map(|no| no.ends_with("000001")),
        Some(true)
    );
}

// ── 기본 참조자 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn default_references_are_added_at_submit_and_inactive_ones_are_dropped() {
    let w = world().await;
    let form = make_form(&w, "purchase", two_fields()).await;
    let form = patch_form(
        &w,
        &form,
        json!({ "default_reference_ids": [w.finance.id, w.a2.id, w.a1.id, w.drafter.id] }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(
        form["default_reference_ids"],
        json!([w.finance.id, w.a2.id, w.a1.id, w.drafter.id])
    );
    // 임시저장에는 아직 붙지 않는다.
    let draft = create(&w, &form, json!({ "vendor": "팡고상사" }), false).await;
    assert_eq!(draft["references"], json!([]));
    // 그사이 한 명이 정지된다.
    w.app.set_status(w.a2.id, MemberStatus::Suspended).await;

    let sent = submit(&w, &id_of(&draft)).await.expect(StatusCode::OK);
    let references: Vec<&str> = sent["references"]
        .as_array()
        .expect("references")
        .iter()
        .map(|reference| reference["member_id"].as_str().expect("id"))
        .collect();
    // 재직 중인 재무 담당자만 붙는다: 결재자(a1)·기안자는 이미 당사자이고 a2는 정지 상태다.
    assert_eq!(references, [w.finance.id.to_string()]);
    assert_eq!(
        w.app
            .notification_count(w.finance.id, "approval.reference")
            .await,
        1
    );
    assert_eq!(
        w.app
            .notification_count(w.a2.id, "approval.reference")
            .await,
        0
    );
    w.app.document(&w.finance.token, &id_of(&draft)).await;

    let audits = w
        .app
        .audit_entries("approval.default_reference_skipped")
        .await;
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].actor_id, Some(w.drafter.id));
    assert_eq!(audits[0].target_id.as_deref(), Some(id_of(&draft).as_str()));
    let detail = audits[0].detail.as_ref().expect("detail");
    assert_eq!(detail["inactive"], json!([w.a2.id]));
    assert_eq!(detail["over_limit"], json!([]));
    assert_eq!(detail["form_code"], "purchase");

    // 다시 상신을 눌러도 참조자·알림·감사 로그가 늘지 않는다.
    submit(&w, &id_of(&draft)).await.expect(StatusCode::OK);
    assert_eq!(
        w.app
            .notification_count(w.finance.id, "approval.reference")
            .await,
        1
    );
    assert_eq!(
        w.app
            .audit_entries("approval.default_reference_skipped")
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn default_references_merge_with_chosen_ones_on_create_and_submit() {
    let w = world().await;
    let form = make_form(&w, "purchase", two_fields()).await;
    let form = patch_form(
        &w,
        &form,
        json!({ "default_reference_ids": [w.finance.id] }),
    )
    .await
    .expect(StatusCode::OK);
    // 기안자가 고른 참조자와 합쳐지고, 이미 고른 사람은 두 번 들어가지 않는다.
    let mut body = doc(&w, &form, json!({ "vendor": "팡고상사" }), true);
    body["references"] = json!([w.outsider.id, w.finance.id]);
    let sent = send_doc(&w, &body).await.expect(StatusCode::CREATED);
    assert_eq!(sent["references"].as_array().expect("references").len(), 2);
    assert!(w
        .app
        .audit_entries("approval.default_reference_skipped")
        .await
        .is_empty());

    body["references"] = json!([w.outsider.id]);
    let sent = send_doc(&w, &body).await.expect(StatusCode::CREATED);
    let mut references: Vec<String> = sent["references"]
        .as_array()
        .expect("references")
        .iter()
        .map(|reference| reference["member_id"].as_str().expect("id").to_owned())
        .collect();
    references.sort();
    let mut expected = vec![w.outsider.id.to_string(), w.finance.id.to_string()];
    expected.sort();
    assert_eq!(references, expected);
    assert_eq!(
        w.app
            .notification_count(w.finance.id, "approval.reference")
            .await,
        2
    );
}

#[tokio::test]
async fn default_reference_ids_are_validated_when_set() {
    let w = world().await;
    let form = make_form(&w, "purchase", two_fields()).await;
    let response = patch_form(
        &w,
        &form,
        json!({ "default_reference_ids": [w.finance.id, w.finance.id] }),
    )
    .await;
    assert_eq!(response.body["error"]["field"], "default_reference_ids");
    assert_eq!(response.body["error"]["reason"], "duplicate");
    response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    patch_form(
        &w,
        &form,
        json!({ "default_reference_ids": [MemberId::new()] }),
    )
    .await
    .expect_error(StatusCode::NOT_FOUND, "not_found");
    let many: Vec<MemberId> = (0..31).map(|_| MemberId::new()).collect();
    patch_form(&w, &form, json!({ "default_reference_ids": many }))
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    w.app.set_status(w.a2.id, MemberStatus::Retired).await;
    patch_form(&w, &form, json!({ "default_reference_ids": [w.a2.id] }))
        .await
        .expect_error(StatusCode::CONFLICT, "member_inactive");
    assert_eq!(
        admin_form(&w, "purchase").await,
        form,
        "거부된 수정은 아무것도 바꾸지 않는다"
    );
    // 비우면 없어진다.
    let set = patch_form(
        &w,
        &form,
        json!({ "default_reference_ids": [w.finance.id] }),
    )
    .await
    .expect(StatusCode::OK);
    let cleared = patch_form(&w, &set, json!({ "default_reference_ids": [] }))
        .await
        .expect(StatusCode::OK);
    assert_eq!(cleared["default_reference_ids"], json!([]));
}

// ── 양식 관리 ────────────────────────────────────────────────────────────

fn schema_errors(response: TestResponse) -> Vec<(String, String)> {
    field_errors(response)
        .into_iter()
        .map(|(key, row, column, reason)| {
            assert_eq!((row, column), (None, None));
            (key, reason)
        })
        .collect()
}

fn at(key: &str, reason: &str) -> (String, String) {
    (key.to_owned(), reason.to_owned())
}

#[tokio::test]
async fn admin_schema_errors_are_reported_with_their_positions() {
    let w = world().await;
    let create = async |schema: Value| {
        let body = json!({ "code": "broken", "name": "잘못된 양식", "schema": schema });
        post_form(&w.app, &w.owner, &body).await
    };
    let text = |key: &str| json!({ "key": key, "label": "값", "kind": "text" });
    let column =
        |index: usize| json!({ "key": format!("c{index}"), "label": "열", "kind": "text" });

    assert_eq!(
        schema_errors(create(json!({ "fields": [text("amount"), text("amount")] })).await),
        vec![at("schema.fields[1].key", "duplicate")]
    );
    assert_eq!(
        schema_errors(
            create(
                json!({ "fields": [text("Amount"), text("1st"), text(""), text(&"k".repeat(41))] })
            )
            .await
        ),
        vec![
            at("schema.fields[0].key", "invalid_character"),
            at("schema.fields[1].key", "invalid_format"),
            at("schema.fields[2].key", "empty"),
            at("schema.fields[3].key", "too_long"),
        ]
    );
    let table = |n: usize| {
        json!({ "fields": [{ "key": "t", "label": "표", "kind": "table",
            "columns": (0..n).map(column).collect::<Vec<_>>() }] })
    };
    assert_eq!(
        schema_errors(create(table(9)).await),
        vec![at("schema.fields[0].columns", "too_long")]
    );
    assert_eq!(
        schema_errors(create(table(0)).await),
        vec![at("schema.fields[0].columns", "empty")]
    );
    assert_eq!(
        schema_errors(
            create(json!({ "fields": [
                { "key": "a", "label": "구분", "kind": "select" },
                { "key": "b", "label": "구분", "kind": "select", "options": [] },
                { "key": "c", "label": "구분", "kind": "select",
                  "options": (0..31).map(|i| format!("선택 {i}")).collect::<Vec<_>>() },
                { "key": "d", "label": "구분", "kind": "select", "options": ["가", "가", "나\u{202E}"] },
                { "key": "t", "label": "표", "kind": "table", "columns": [
                    { "key": "m", "label": "담당", "kind": "member" }
                ]},
                { "key": "e", "label": "", "kind": "number", "min": 5, "max": 1 }
            ]}))
            .await
        ),
        vec![
            at("schema.fields[0].options", "empty"),
            at("schema.fields[1].options", "empty"),
            at("schema.fields[2].options", "too_long"),
            at("schema.fields[3].options[1]", "duplicate"),
            at("schema.fields[3].options[2]", "invalid_character"),
            at("schema.fields[4].columns[0].kind", "invalid_format"),
            at("schema.fields[5].label", "empty"),
            at("schema.fields[5].min", "inconsistent"),
        ]
    );
    let fields =
        |n: usize| json!({ "fields": (0..n).map(|i| text(&format!("f{i}"))).collect::<Vec<_>>() });
    assert_eq!(
        schema_errors(create(fields(41)).await),
        vec![at("schema.fields", "too_long")]
    );
    // 모양 자체가 틀린 정의(모르는 종류 등)는 요청 형식 오류다.
    for malformed in [
        json!({ "fields": [{ "key": "a", "label": "값", "kind": "file" }] }),
        json!({ "fields": [{ "key": "a", "label": "값" }] }),
        json!({ "fields": "none" }),
        json!("fields"),
    ] {
        create(malformed)
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    assert_eq!(
        w.app
            .count("SELECT COUNT(*) FROM approval_forms WHERE code = 'broken'")
            .await,
        0
    );
    assert!(w.app.audit_entries("approval_form.create").await.is_empty());

    // 경계: 필드 40개, 열 8개, 선택지 30개는 받는다. 수정도 같은 검증을 거친다.
    let ok = post_form(
        &w.app,
        &w.owner,
        &json!({ "code": "broken", "name": "큰 양식", "schema": fields(40) }),
    )
    .await
    .expect(StatusCode::CREATED);
    assert_eq!(ok["schema"]["fields"].as_array().expect("fields").len(), 40);
    assert_eq!(
        ok["schema"]["fields"][0]["max_len"], 200,
        "기본값이 채워진다"
    );
    patch_form(&w, &ok, json!({ "schema": table(8) }))
        .await
        .expect(StatusCode::OK);
    let current = admin_form(&w, "broken").await;
    assert_eq!(
        schema_errors(patch_form(&w, &current, json!({ "schema": table(9) })).await),
        vec![at("schema.fields[0].columns", "too_long")]
    );
    assert_eq!(admin_form(&w, "broken").await, current);
}

#[tokio::test]
async fn a_form_needs_a_body_or_a_required_field() {
    let w = world().await;
    let optional = json!({ "fields": [{ "key": "memo", "label": "메모", "kind": "text" }] });
    for schema in [json!({ "fields": [] }), optional.clone()] {
        let response = post_form(
            &w.app,
            &w.owner,
            &json!({ "code": "hollow", "name": "빈 양식", "schema": schema, "use_body": false }),
        )
        .await;
        assert_eq!(response.body["error"]["field"], "use_body");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    let form = post_form(
        &w.app,
        &w.owner,
        &json!({ "code": "hollow", "name": "본문 양식", "schema": optional }),
    )
    .await
    .expect(StatusCode::CREATED);
    assert_eq!(form["use_body"], true);
    assert_eq!(form["category"], "general");
    patch_form(&w, &form, json!({ "use_body": false }))
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
}

#[tokio::test]
async fn system_forms_keep_their_category_and_structure() {
    let w = world().await;
    let leave = admin_form(&w, "leave").await;
    for change in [
        json!({ "category": "general" }),
        json!({ "schema": two_fields() }),
        json!({ "use_body": false }),
        json!({ "default_reference_ids": [w.finance.id] }),
        json!({ "active": false }),
    ] {
        patch_form(&w, &leave, change.clone())
            .await
            .expect_error(StatusCode::CONFLICT, "in_use");
    }
    assert_eq!(admin_form(&w, "leave").await, leave);
    // 이름·설명·순서는 고칠 수 있고, 같은 분류를 다시 보내는 것은 변경이 아니다.
    let renamed = patch_form(&w, &leave, json!({ "name": "휴가원", "category": "hr" }))
        .await
        .expect(StatusCode::OK);
    assert_eq!(renamed["name"], "휴가원");
    assert_eq!(renamed["schema_version"], 1);
    // 일반 양식의 분류는 바꿀 수 있고, 모르는 분류는 요청 형식 오류다.
    let general = admin_form(&w, "general").await;
    let moved = patch_form(&w, &general, json!({ "category": "etc" }))
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        (&moved["category"], &moved["category_label"]),
        (&json!("etc"), &json!("기타"))
    );
    patch_form(&w, &moved, json!({ "category": "finance" }))
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
}

#[tokio::test]
async fn duplicating_a_form_makes_an_inactive_copy_with_a_numbered_code() {
    let w = world().await;
    let source = admin_form(&w, "expense_report").await;
    let duplicate = async |id: &str, token: &str| {
        w.app
            .post(&format!("/admin/approval/forms/{id}/duplicate"))
            .bearer(token)
            .json(&json!({}))
            .send()
            .await
    };
    for token in [&w.drafter.token, &w.a1.token] {
        duplicate(&id_of(&source), token)
            .await
            .expect_error(StatusCode::FORBIDDEN, "forbidden");
    }
    let first = duplicate(&id_of(&source), &w.owner)
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(first["code"], "expense_report_copy_1");
    assert_eq!(first["name"], "지출결의서 사본");
    assert_eq!(first["active"], false);
    assert_eq!(first["origin"], "custom");
    assert_eq!(
        (first["version"].as_i64(), first["schema_version"].as_i64()),
        (Some(1), Some(1))
    );
    for same in [
        "schema",
        "category",
        "use_body",
        "description",
        "kind",
        "default_reference_ids",
    ] {
        assert_eq!(first[same], source[same], "{same}");
    }
    assert_ne!(first["id"], source["id"]);
    let second = duplicate(&id_of(&source), &w.owner)
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(second["code"], "expense_report_copy_2");
    // 꺼진 채로 만들어지므로 기안 화면에는 나오지 않고, 켜면 쓸 수 있다.
    assert!(w
        .app
        .approval_forms(&w.drafter.token)
        .await
        .iter()
        .all(|form| form["code"] != "expense_report_copy_1"));
    send_doc(&w, &doc(&w, &first, json!({}), false))
        .await
        .expect_error(StatusCode::CONFLICT, "form_inactive");
    patch_form(
        &w,
        &first,
        json!({ "active": true, "name": "지출결의서(해외)" }),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(
        w.app
            .approval_form(&w.drafter.token, "expense_report_copy_1")
            .await["name"],
        "지출결의서(해외)"
    );
    // 사본을 고쳐도 원본은 그대로다.
    assert_eq!(admin_form(&w, "expense_report").await, source);

    // 같은 멱등성 키는 사본을 하나만 만든다.
    let keyed = || {
        w.app
            .post(&format!(
                "/admin/approval/forms/{}/duplicate",
                id_of(&source)
            ))
            .bearer(&w.owner)
            .header("idempotency-key", "dup-1")
            .json(&json!({}))
            .send()
    };
    let third = keyed().await.expect(StatusCode::CREATED);
    assert_eq!(keyed().await.expect(StatusCode::CREATED), third);
    assert_eq!(third["code"], "expense_report_copy_3");
    assert_eq!(
        w.app
            .count("SELECT COUNT(*) FROM approval_forms WHERE code LIKE 'expense_report_copy_%'")
            .await,
        3
    );
    assert_eq!(
        w.app.audit_entries("approval_form.duplicate").await.len(),
        3
    );

    // 시스템 양식과 없는 양식.
    duplicate(&id_of(&admin_form(&w, "leave").await), &w.owner)
        .await
        .expect_error(StatusCode::CONFLICT, "in_use");
    duplicate(&MemberId::new().to_string(), &w.owner)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn duplicate_codes_and_names_stay_within_their_limits() {
    let w = world().await;
    let long = post_form(
        &w.app,
        &w.owner,
        &json!({ "code": "a".repeat(32), "name": "가".repeat(50) }),
    )
    .await
    .expect(StatusCode::CREATED);
    let copy = w
        .app
        .post(&format!("/admin/approval/forms/{}/duplicate", id_of(&long)))
        .bearer(&w.owner)
        .json(&json!({}))
        .send()
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(copy["code"], format!("{}_copy_1", "a".repeat(23)));
    assert!(copy["code"].as_str().expect("code").len() <= 32);
    assert_eq!(copy["name"], format!("{} 사본", "가".repeat(47)));
    assert_eq!(copy["name"].as_str().expect("name").chars().count(), 50);
}

// ── 문서함 필터 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn mailbox_filters_by_category_and_form() {
    let w = world().await;
    let general = w.app.approval_form(&w.drafter.token, "general").await;
    let expense = w
        .app
        .approval_form(&w.drafter.token, "expense_report")
        .await;
    let trip = w.app.approval_form(&w.drafter.token, "domestic_trip").await;
    let plain_doc = send_doc(
        &w,
        &json!({
            "form_id": general["id"], "title": "품의", "body": "본문",
            "steps": [{ "approver_id": w.a1.id, "kind": "approve" }], "submit": true,
        }),
    )
    .await
    .expect(StatusCode::CREATED);
    let expense_doc = create(
        &w,
        &expense,
        json!({ "spent_on": "2026-03-02", "pay_type": "개인경비", "items": [
            { "used_on": "2026-03-01", "vendor": "서점", "account": "교육훈련비", "amount": 25000 }
        ]}),
        true,
    )
    .await;
    let trip_doc = create(
        &w,
        &trip,
        json!({
            "period": { "start": "2026-03-09", "end": "2026-03-10" }, "destination": "부산",
            "purpose": "고객 미팅", "transport": "기차"
        }),
        true,
    )
    .await;
    let list = async |token: &str, query: &str| -> Vec<String> {
        let page = w
            .app
            .get(&format!("/approval/documents?{query}"))
            .bearer(token)
            .send()
            .await
            .expect(StatusCode::OK);
        page["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(id_of)
            .collect()
    };
    let (plain_id, expense_id, trip_id) =
        (id_of(&plain_doc), id_of(&expense_doc), id_of(&trip_doc));
    let drafter = &w.drafter.token;
    assert_eq!(
        list(drafter, "box=drafted").await,
        [trip_id.as_str(), expense_id.as_str(), plain_id.as_str()]
    );
    assert_eq!(
        list(drafter, "box=drafted&category=accounting").await,
        [expense_id.as_str()]
    );
    assert_eq!(
        list(drafter, "box=drafted&category=trip").await,
        [trip_id.as_str()]
    );
    assert_eq!(
        list(drafter, "box=drafted&category=welfare").await,
        Vec::<String>::new()
    );
    assert_eq!(
        list(drafter, &format!("box=drafted&form_id={}", id_of(&general))).await,
        [plain_id.as_str()]
    );
    assert_eq!(
        list(
            drafter,
            &format!("box=drafted&category=trip&form_id={}", id_of(&general))
        )
        .await,
        Vec::<String>::new()
    );
    // 결재자의 할 일 함에도 같은 필터가 적용된다.
    assert_eq!(
        list(&w.a1.token, "box=todo&category=accounting").await,
        [expense_id.as_str()]
    );
    // 필터는 내 문서함 안에서만 고른다.
    assert_eq!(
        list(&w.outsider.token, "box=drafted&category=accounting").await,
        Vec::<String>::new()
    );
    // 관리자 목록.
    let admin = w
        .app
        .get("/admin/approval/documents?category=trip")
        .bearer(&w.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(admin["items"].as_array().expect("items").len(), 1);
    assert_eq!(admin["items"][0]["id"], json!(trip_id));
    // 잘못된 필터 값.
    for bad in ["box=drafted&category=finance", "box=drafted&form_id=abc"] {
        w.app
            .get(&format!("/approval/documents?{bad}"))
            .bearer(drafter)
            .send()
            .await
            .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    }
    // 페이지를 넘겨도 필터가 유지된다.
    create(
        &w,
        &expense,
        json!({ "spent_on": "2026-03-03", "pay_type": "법인카드", "items": [
            { "used_on": "2026-03-03", "vendor": "카페", "account": "회의비", "amount": 9000 }
        ]}),
        true,
    )
    .await;
    let first = w
        .app
        .get("/approval/documents?box=drafted&category=accounting&limit=1")
        .bearer(drafter)
        .send()
        .await
        .expect(StatusCode::OK);
    let cursor = first["next_cursor"].as_str().expect("cursor");
    assert_eq!(
        list(
            drafter,
            &format!("box=drafted&category=accounting&limit=1&cursor={cursor}")
        )
        .await,
        [expense_id.as_str()]
    );
}
