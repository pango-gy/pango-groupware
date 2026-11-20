//! 인증과 쓰기 사이에 권한·소속이 바뀌는 경우.
//!
//! HTTP 요청은 매번 다시 인증하므로 "인증은 통과했는데 트랜잭션에 들어가기 전에 관리자에서
//! 해제된" 요청을 HTTP로는 만들 수 없다. 그래서 인증된 맥락을 받아 둔 뒤 DB를 바꾸고 서비스
//! 함수를 직접 부른다. 서비스는 낡은 맥락(`ctx.actor`는 여전히 관리자)을 들고 들어오지만,
//! 쓰기 트랜잭션 안에서 다시 읽은 권한으로 판정해야 한다.

use http::StatusCode;
use pango_api::admin::{CompanySettingsInput, HolidayImportRequest, HolidayInput};
use pango_api::approval::{EditFormRequest, NewFormRequest, RejectRequest};
use pango_api::attendance::{
    AdminCreateRecordRequest, AdminEditRecordRequest, CreateCorrectionRequest,
};
use pango_api::leave::{
    AdjustLeaveRequest, CreateLeaveRequest, CreateLeaveTypeRequest, UpdateLeaveTypeRequest,
};
use pango_api::notice::NoticeInput;
use pango_api::Versioned;
use pango_core::authz::Role;
use pango_core::id::DepartmentId;
use pango_core::member::MemberStatus;
use pango_server::service::{approval, attendance, leave, notices, settings, AuthContext};
use pango_server::testing::{denial, ts, TestApp, TestMember};
use pango_server::ServiceResult;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

fn parse<T: DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).expect("request body")
}

/// 표마다 (이름, 줄 수, `version` 합계). 거부된 쓰기가 아무것도 바꾸지 않았는지 비교한다.
/// 세션 표는 인증이 마지막 사용 시각을 갱신하므로 뺀다.
async fn fingerprint(app: &TestApp) -> Vec<(String, i64, i64)> {
    let mut conn = app.state.db.read().await.expect("read");
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_sqlx_%'
           AND name <> 'sessions'
         ORDER BY name",
    )
    .fetch_all(&mut *conn)
    .await
    .expect("tables");
    let mut fingerprint = Vec::new();
    for table in tables {
        let versioned: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = 'version'"
        ))
        .fetch_one(&mut *conn)
        .await
        .expect("columns");
        let versions = if versioned > 0 {
            "COALESCE(SUM(version), 0)"
        } else {
            "0"
        };
        let (rows, versions): (i64, i64) =
            sqlx::query_as(&format!("SELECT COUNT(*), {versions} FROM {table}"))
                .fetch_one(&mut *conn)
                .await
                .expect("count");
        fingerprint.push((table, rows, versions));
    }
    fingerprint
}

/// 관리자 한 명과, 그가 인증을 통과한 순간의 맥락.
struct Stale {
    app: TestApp,
    owner: String,
    admin: TestMember,
    ctx: AuthContext,
}

async fn stale_admin() -> Stale {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let ctx = app.auth_context(&admin.token).await;
    assert!(ctx.actor.is_admin());
    Stale {
        app,
        owner,
        admin,
        ctx,
    }
}

impl Stale {
    /// 인증 뒤, 쓰기 전에 관리자에서 해제된다.
    async fn demote(&self) {
        self.app
            .alter_member(self.admin.id, |m| m.role = Role::Member)
            .await;
    }

    async fn suspend(&self) {
        self.app
            .alter_member(self.admin.id, |m| m.status = MemberStatus::Suspended)
            .await;
    }
}

/// 낡은 맥락의 쓰기가 `expected`로 거부되고 아무것도 바꾸지 않았는지 확인한다.
macro_rules! assert_denied {
    ($stale:expr, $expected:expr, $($name:literal => $call:expr),+ $(,)?) => {{
        let before = fingerprint(&$stale.app).await;
        $(
            let result = $call.await;
            assert_eq!(denial(&result), $expected, "{}", $name);
        )+
        assert_eq!(fingerprint(&$stale.app).await, before, "거부된 쓰기는 흔적이 없다");
    }};
}

async fn leave_type(app: &TestApp, token: &str, code: &str) -> Value {
    let types = app
        .get("/admin/leave/types")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK);
    types
        .as_array()
        .expect("types")
        .iter()
        .find(|leave_type| leave_type["code"] == code)
        .expect("type")
        .clone()
}

// ── 휴가 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn leave_admin_writes_recheck_the_role_inside_the_transaction() {
    let s = stale_admin().await;
    let kim = s.app.add_member("kim", Role::Member, None).await;
    let annual = leave_type(&s.app, &s.owner, "annual").await;
    // kim의 승인 대기 신청 하나(관리자 취소 대상).
    let created = s
        .app
        .post("/leave/requests")
        .bearer(&kim.token)
        .json(&json!({
            "leave_type_id": annual["id"], "start_date": "2026-03-10", "end_date": "2026-03-10",
            "unit": "full_day", "reason": "개인 사정",
            "steps": [{ "approver_id": s.app.owner_id, "kind": "approve" }],
        }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    let request_id = parse(created["id"].clone());
    let adjust: AdjustLeaveRequest = parse(json!({
        "member_id": kim.id, "amount_q": 40, "effective_on": "2026-03-02", "memo": "포상",
    }));
    let new_type: CreateLeaveTypeRequest = parse(json!({
        "code": "bonus", "name": "포상 휴가", "deducts_balance": false, "allow_partial": false,
    }));
    let edit_type: UpdateLeaveTypeRequest = parse(json!({
        "version": annual["version"], "name": "연차(수정)", "allow_partial": true, "active": true,
    }));
    let type_id = parse(annual["id"].clone());

    s.demote().await;
    assert_denied!(s, "forbidden",
        "adjust" => leave::admin_adjust(&s.app.state, &s.ctx, &adjust, None),
        "create type" => leave::admin_create_type(&s.app.state, &s.ctx, &new_type, None),
        "update type" => leave::admin_update_type(&s.app.state, &s.ctx, type_id, &edit_type),
        "cancel" => leave::admin_cancel_request(&s.app.state, &s.ctx, request_id),
        "accrual" => leave::admin_run_accrual(&s.app.state, &s.ctx),
    );
    // 계정이 정지된 경우는 인증 자체가 무효다.
    s.suspend().await;
    assert_denied!(s, "unauthenticated",
        "adjust" => leave::admin_adjust(&s.app.state, &s.ctx, &adjust, None),
        "cancel" => leave::admin_cancel_request(&s.app.state, &s.ctx, request_id),
    );
}

// ── 근태 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn attendance_admin_writes_recheck_the_role_inside_the_transaction() {
    let s = stale_admin().await;
    let kim = s.app.add_member("kim", Role::Member, None).await;
    let record = s
        .app
        .post("/attendance/clock-in")
        .bearer(&kim.token)
        .json(&json!({ "work_type": "office" }))
        .send()
        .await
        .expect(StatusCode::OK);
    s.app.set_time("2026-03-04T03:00:00Z");
    let at = |rfc3339: &str| ts(rfc3339).as_millis();
    let create: AdminCreateRecordRequest = parse(json!({
        "member_id": kim.id, "work_date": "2026-03-03",
        "clock_in_at": at("2026-03-03T00:00:00Z"), "clock_out_at": at("2026-03-03T09:00:00Z"),
        "work_type": "office", "reason": "기록 누락",
    }));
    let edit: AdminEditRecordRequest = parse(json!({
        "version": record["version"],
        "clock_in_at": at("2026-03-01T23:00:00Z"), "clock_out_at": at("2026-03-02T09:00:00Z"),
        "work_type": "office", "reason": "지각 정정",
    }));
    let record_id = parse(record["id"].clone());

    s.demote().await;
    assert_denied!(s, "forbidden",
        "create" => attendance::admin_create(&s.app.state, &s.ctx, &create, None),
        "edit" => attendance::admin_edit(&s.app.state, &s.ctx, record_id, &edit),
    );
    s.suspend().await;
    assert_denied!(s, "unauthenticated",
        "create" => attendance::admin_create(&s.app.state, &s.ctx, &create, None),
        "edit" => attendance::admin_edit(&s.app.state, &s.ctx, record_id, &edit),
    );
}

// ── 공지 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn notice_admin_writes_recheck_the_role_inside_the_transaction() {
    let s = stale_admin().await;
    s.app.add_member("kim", Role::Member, None).await;
    let existing = s
        .app
        .post("/admin/notices")
        .bearer(&s.owner)
        .json(&json!({ "title": "기존 공지", "body": "본문", "audience": "all" }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    let id = parse(existing["id"].clone());
    let version = existing["version"].as_i64().expect("version");
    let input: NoticeInput =
        parse(json!({ "title": "가짜 공지", "body": "본문", "audience": "all" }));
    let update = Versioned {
        version,
        data: input.clone(),
    };

    s.demote().await;
    assert_denied!(s, "forbidden",
        "create" => notices::create(&s.app.state, &s.ctx, &input, None),
        "update" => notices::update(&s.app.state, &s.ctx, id, &update),
        "archive" => notices::archive(&s.app.state, &s.ctx, id, version),
    );
    s.suspend().await;
    assert_denied!(s, "unauthenticated",
        "create" => notices::create(&s.app.state, &s.ctx, &input, None),
        "archive" => notices::archive(&s.app.state, &s.ctx, id, version),
    );
}

// ── 설정 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn settings_writes_recheck_the_role_inside_the_transaction() {
    let s = stale_admin().await;
    let current = s
        .app
        .get("/admin/settings")
        .bearer(&s.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    let mut changed = current.clone();
    changed["company_name"] = json!("가짜 회사");
    changed["late_grace_minutes"] = json!(120);
    let update: Versioned<CompanySettingsInput> = parse(changed);

    s.demote().await;
    assert_denied!(s, "forbidden",
        "update" => settings::update(&s.app.state, &s.ctx, &update),
    );
    s.suspend().await;
    assert_denied!(s, "unauthenticated",
        "update" => settings::update(&s.app.state, &s.ctx, &update),
    );
    let after = s
        .app
        .get("/admin/settings")
        .bearer(&s.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(after, current);
}

// ── 공휴일 ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn holiday_writes_recheck_the_role_inside_the_transaction() {
    let s = stale_admin().await;
    s.app
        .put("/admin/holidays/2026-05-05")
        .bearer(&s.owner)
        .json(&json!({ "name": "어린이날" }))
        .send()
        .await
        .expect(StatusCode::OK);
    let input: HolidayInput = parse(json!({ "name": "임의 휴일" }));
    let import: HolidayImportRequest = parse(json!({
        "items": [{ "date": "2026-06-03", "name": "임의 휴일" }],
    }));

    s.demote().await;
    assert_denied!(s, "forbidden",
        "upsert" => settings::upsert_holiday(&s.app.state, &s.ctx, "2026-03-03", &input),
        "rename" => settings::upsert_holiday(&s.app.state, &s.ctx, "2026-05-05", &input),
        "delete" => settings::delete_holiday(&s.app.state, &s.ctx, "2026-05-05"),
        "import" => settings::import_holidays(&s.app.state, &s.ctx, &import),
    );
    s.suspend().await;
    assert_denied!(s, "unauthenticated",
        "upsert" => settings::upsert_holiday(&s.app.state, &s.ctx, "2026-03-03", &input),
        "delete" => settings::delete_holiday(&s.app.state, &s.ctx, "2026-05-05"),
    );
    let holidays = s
        .app
        .get("/holidays?year=2026")
        .bearer(&s.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        holidays,
        json!([{ "date": "2026-05-05", "name": "어린이날" }])
    );
}

// ── 결재 ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn approval_admin_writes_recheck_the_role_inside_the_transaction() {
    let s = stale_admin().await;
    let drafter = s.app.add_member("drafter", Role::Member, None).await;
    let approver = s.app.add_member("approver", Role::Member, None).await;
    let forms = s
        .app
        .get("/admin/approval/forms")
        .bearer(&s.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    let general = forms
        .as_array()
        .expect("forms")
        .iter()
        .find(|form| form["code"] == "general")
        .expect("general")
        .clone();
    let document = s
        .app
        .post("/approval/documents")
        .bearer(&drafter.token)
        .json(&json!({
            "form_id": general["id"], "title": "지출 품의", "body": "본문",
            "steps": [{ "approver_id": approver.id, "kind": "approve" }],
            "references": [], "submit": true,
        }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    let document_id = parse(document["id"].clone());
    let new_form: NewFormRequest = parse(json!({ "code": "fake", "name": "가짜 양식" }));
    let edit_form: EditFormRequest = parse(json!({
        "version": general["version"], "name": "품의서(수정)", "active": false,
    }));
    let form_id = parse(general["id"].clone());
    let reject: RejectRequest = parse(json!({ "reason": "대리 반려" }));

    s.demote().await;
    assert_denied!(s, "forbidden",
        "create form" => approval::admin_create_form(&s.app.state, &s.ctx, &new_form, None),
        "update form" => approval::admin_update_form(&s.app.state, &s.ctx, form_id, &edit_form),
        "duplicate form" => approval::admin_duplicate_form(&s.app.state, &s.ctx, form_id, None),
        "admin reject" => approval::admin_reject(&s.app.state, &s.ctx, document_id, &reject),
    );
    s.suspend().await;
    assert_denied!(s, "unauthenticated",
        "create form" => approval::admin_create_form(&s.app.state, &s.ctx, &new_form, None),
        "duplicate form" => approval::admin_duplicate_form(&s.app.state, &s.ctx, form_id, None),
        "admin reject" => approval::admin_reject(&s.app.state, &s.ctx, document_id, &reject),
    );
    // 문서는 여전히 결재자의 차례다.
    let detail = s
        .app
        .get(&format!(
            "/approval/documents/{}",
            document["id"].as_str().expect("id")
        ))
        .bearer(&approver.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(detail["status"], "in_progress");
    assert_eq!(detail["my_turn"], true);
}

// ── 본인 신청: 소속·입사일은 트랜잭션 안에서 다시 읽는다 ─────────────────

struct Team {
    app: TestApp,
    owner: String,
    dev: DepartmentId,
    sales: DepartmentId,
    lead: TestMember,
    kim: TestMember,
}

/// 개발팀(팀장 lead, 팀원 kim)과 조직장 없는 영업팀.
async fn team() -> Team {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let dev = app.add_department("개발팀", None, None).await;
    let sales = app.add_department("영업팀", None, None).await;
    let lead = app.add_member("lead", Role::Member, Some(dev)).await;
    app.set_department_head(dev, Some(lead.id)).await;
    let kim = app.add_member("kim", Role::Member, Some(dev)).await;
    Team {
        app,
        owner,
        dev,
        sales,
        lead,
        kim,
    }
}

fn validation_field<T>(result: &ServiceResult<T>) -> Option<&'static str> {
    match result {
        Err(pango_server::ServiceError::Domain(pango_core::DomainError::Validation {
            field,
            ..
        })) => Some(field),
        _ => None,
    }
}

/// 인증 뒤에 부서가 바뀌고 입사일이 정정된 구성원의 휴가 신청은, 낡은 맥락의 소속(옛 팀장의
/// 관할)과 입사일(옛 연차)이 아니라 지금의 값으로 판정된다.
#[tokio::test]
async fn leave_request_uses_the_members_current_department_and_hire_date() {
    let t = team().await;
    let annual = leave_type(&t.app, &t.owner, "annual").await;
    let ctx = t.app.auth_context(&t.kim.token).await;
    assert_eq!(ctx.member.department_id, Some(t.dev));
    // 그사이 영업팀으로 옮겨지고 입사일이 2026-01-02로 정정됐다.
    t.app
        .alter_member(t.kim.id, |m| {
            m.department_id = Some(t.sales);
            m.hired_on = "2026-01-02".parse().expect("date");
        })
        .await;
    let request = |steps: Value| -> CreateLeaveRequest {
        parse(json!({
            "leave_type_id": annual["id"], "start_date": "2026-03-10", "end_date": "2026-03-10",
            "unit": "full_day", "reason": "개인 사정", "steps": steps,
        }))
    };
    let before = fingerprint(&t.app).await;
    // 추천 결재선: 영업팀에는 조직장이 없다(옛 소속이면 lead가 추천됐을 것이다).
    let suggested = leave::create_request(&t.app.state, &ctx, &request(Value::Null), None).await;
    assert_eq!(validation_field(&suggested), Some("steps"));
    // 옛 팀장을 최종 결재자로 직접 지정: 더는 kim의 관할 조직장이 아니다.
    let old_lead = json!([{ "approver_id": t.lead.id, "kind": "approve" }]);
    let by_old_lead = leave::create_request(&t.app.state, &ctx, &request(old_lead), None).await;
    assert_eq!(validation_field(&by_old_lead), Some("steps"));
    assert_eq!(fingerprint(&t.app).await, before);

    // 관리자를 결재자로 하면 접수된다. 부여는 정정된 입사일 기준이다(월 부여 2건뿐).
    let by_owner = json!([{ "approver_id": t.app.owner_id, "kind": "approve" }]);
    let created = leave::create_request(&t.app.state, &ctx, &request(by_owner), None)
        .await
        .expect("request");
    assert_eq!(t.app.grant_keys(t.kim.id).await, ["monthly:1", "monthly:2"]);
    let document = t
        .app
        .get(&format!("/approval/documents/{}", created.document_id))
        .bearer(&t.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(document["drafter_department_name"], "영업팀");
    // 잔여 2일 중 1일을 썼다. 옛 입사일이었다면 15일이 부여돼 3일 신청도 통과했을 것이다.
    let three_days: CreateLeaveRequest = parse(json!({
        "leave_type_id": annual["id"], "start_date": "2026-03-16", "end_date": "2026-03-18",
        "unit": "full_day", "reason": "개인 사정",
        "steps": [{ "approver_id": t.app.owner_id, "kind": "approve" }],
    }));
    let too_many = leave::create_request(&t.app.state, &ctx, &three_days, None).await;
    assert_eq!(
        too_many.err().map(|error| error.status()),
        Some(StatusCode::CONFLICT)
    );
}

#[tokio::test]
async fn correction_request_uses_the_members_current_department() {
    let t = team().await;
    t.app.set_time("2026-03-03T01:00:00Z");
    let ctx = t.app.auth_context(&t.kim.token).await;
    t.app
        .alter_member(t.kim.id, |m| m.department_id = Some(t.sales))
        .await;
    let at = |rfc3339: &str| ts(rfc3339).as_millis();
    let request = |steps: Value| -> CreateCorrectionRequest {
        parse(json!({
            "work_date": "2026-03-02",
            "clock_in_at": at("2026-03-02T00:00:00Z"), "clock_out_at": at("2026-03-02T09:00:00Z"),
            "work_type": "office", "reason": "출근 기록 누락", "steps": steps,
        }))
    };
    let before = fingerprint(&t.app).await;
    let suggested =
        attendance::create_correction(&t.app.state, &ctx, &request(Value::Null), None).await;
    assert_eq!(validation_field(&suggested), Some("steps"));
    let old_lead = json!([{ "approver_id": t.lead.id, "kind": "approve" }]);
    let by_old_lead =
        attendance::create_correction(&t.app.state, &ctx, &request(old_lead), None).await;
    assert_eq!(validation_field(&by_old_lead), Some("steps"));
    assert_eq!(fingerprint(&t.app).await, before);

    let by_owner = json!([{ "approver_id": t.app.owner_id, "kind": "approve" }]);
    let created = attendance::create_correction(&t.app.state, &ctx, &request(by_owner), None)
        .await
        .expect("correction");
    let document = t
        .app
        .get(&format!("/approval/documents/{}", created.document_id))
        .bearer(&t.owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(document["drafter_department_name"], "영업팀");

    // 그사이 정지된 계정의 요청은 인증이 무효다.
    t.app
        .alter_member(t.kim.id, |m| m.status = MemberStatus::Suspended)
        .await;
    let other_day: CreateCorrectionRequest = parse(json!({
        "work_date": "2026-02-27",
        "clock_in_at": at("2026-02-27T00:00:00Z"), "clock_out_at": at("2026-02-27T09:00:00Z"),
        "work_type": "office", "reason": "출근 기록 누락",
        "steps": [{ "approver_id": t.app.owner_id, "kind": "approve" }],
    }));
    let suspended = attendance::create_correction(&t.app.state, &ctx, &other_day, None).await;
    assert_eq!(denial(&suspended), "unauthenticated");
}
