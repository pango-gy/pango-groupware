//! 기능 사이의 연결을 확인하는 테스트 (휴가 ↔ 결재 ↔ 근태).

use http::StatusCode;
use pango_core::authz::Role;
use pango_server::testing::{TestApp, TestMember};
use serde_json::{json, Value};

/// 팀장(head)과 팀원(member)이 있는 부서 하나.
async fn team(app: &TestApp) -> (TestMember, TestMember) {
    let department = app.add_department("개발팀", None, None).await;
    let head = app.add_member("head", Role::Member, Some(department)).await;
    let member = app
        .add_member("member", Role::Member, Some(department))
        .await;
    app.set_department_head(department, Some(head.id)).await;
    (head, member)
}

async fn annual_leave_type(app: &TestApp, token: &str) -> String {
    let types = app
        .get("/leave/types")
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK);
    let annual = types
        .as_array()
        .expect("array")
        .iter()
        .find(|t| t["code"] == "annual")
        .expect("annual type");
    annual["id"].as_str().expect("id").to_owned()
}

/// 휴가를 신청하고 팀장이 승인한다.
async fn approved_leave(
    app: &TestApp,
    head: &TestMember,
    member: &TestMember,
    date: &str,
    unit: &str,
) {
    let leave_type_id = annual_leave_type(app, &member.token).await;
    let request = app
        .post("/leave/requests")
        .bearer(&member.token)
        .json(&json!({
            "leave_type_id": leave_type_id, "start_date": date, "end_date": date,
            "unit": unit, "reason": "개인 사정",
        }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    let document_id = request["document_id"].as_str().expect("document id");
    app.post(&format!("/approval/documents/{document_id}/approve"))
        .bearer(&head.token)
        .json(&json!({}))
        .send()
        .await
        .expect(StatusCode::OK);
}

async fn clock(app: &TestApp, token: &str, at: &str, action: &str) {
    app.set_time(at);
    let body = if action == "clock-in" {
        json!({ "work_type": "office" })
    } else {
        json!({})
    };
    app.post(&format!("/attendance/{action}"))
        .bearer(token)
        .json(&body)
        .send()
        .await
        .expect(StatusCode::OK);
}

#[tokio::test]
async fn approved_full_day_leave_shows_as_on_leave_not_absent() {
    let app = TestApp::new().await;
    let (head, member) = team(&app).await;
    approved_leave(&app, &head, &member, "2026-03-03", "full_day").await;

    // 휴가일이 지난 뒤에 본다.
    app.set_time("2026-03-05T01:00:00Z");
    assert_eq!(
        app.attendance_day(&member.token, "2026-03-03").await["evaluation"]["status"],
        "on_leave"
    );
    // 휴가가 없던 2026-03-04는 결근.
    assert_eq!(
        app.attendance_day(&member.token, "2026-03-04").await["evaluation"]["status"],
        "absent"
    );
}

#[tokio::test]
async fn pending_leave_does_not_excuse_absence() {
    let app = TestApp::new().await;
    let (_head, member) = team(&app).await;
    let leave_type_id = annual_leave_type(&app, &member.token).await;
    app.post("/leave/requests")
        .bearer(&member.token)
        .json(&json!({
            "leave_type_id": leave_type_id, "start_date": "2026-03-03", "end_date": "2026-03-03",
            "unit": "full_day", "reason": "개인 사정",
        }))
        .send()
        .await
        .expect(StatusCode::CREATED);

    app.set_time("2026-03-05T01:00:00Z");
    assert_eq!(
        app.attendance_day(&member.token, "2026-03-03").await["evaluation"]["status"],
        "absent"
    );
}

#[tokio::test]
async fn morning_half_day_leave_moves_the_expected_start() {
    let app = TestApp::new().await;
    let (head, member) = team(&app).await;
    approved_leave(&app, &head, &member, "2026-03-03", "am_half").await;

    // 오전 반차: 14:00(KST) 출근이면 지각이 아니다.
    clock(&app, &member.token, "2026-03-03T05:00:00Z", "clock-in").await;
    clock(&app, &member.token, "2026-03-03T09:00:00Z", "clock-out").await;
    let evaluation = app.attendance_day(&member.token, "2026-03-03").await["evaluation"].clone();
    assert_eq!(evaluation["status"], "normal");
    assert_eq!(evaluation["late_minutes"], 0);
}

#[tokio::test]
async fn without_leave_the_same_clock_in_is_late() {
    let app = TestApp::new().await;
    let (_head, member) = team(&app).await;
    clock(&app, &member.token, "2026-03-03T05:00:00Z", "clock-in").await;
    clock(&app, &member.token, "2026-03-03T09:00:00Z", "clock-out").await;
    let evaluation = app.attendance_day(&member.token, "2026-03-03").await["evaluation"].clone();
    assert_eq!(evaluation["status"], "late");
    assert_eq!(evaluation["late_minutes"], 300);
}

#[tokio::test]
async fn canceled_leave_no_longer_affects_attendance() {
    let app = TestApp::new().await;
    let (head, member) = team(&app).await;
    approved_leave(&app, &head, &member, "2026-03-10", "full_day").await;
    let requests = app
        .get("/leave/requests?year=2026")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let request_id = requests[0]["id"].as_str().expect("id").to_owned();
    app.post(&format!("/leave/requests/{request_id}/cancel"))
        .bearer(&member.token)
        .json(&json!({}))
        .send()
        .await
        .expect(StatusCode::OK);

    app.set_time("2026-03-12T01:00:00Z");
    assert_eq!(
        app.attendance_day(&member.token, "2026-03-10").await["evaluation"]["status"],
        "absent"
    );
}

// ── 개인 신청의 결재자 권한 ──────────────────────────────────────────────

async fn leave_body(app: &TestApp, token: &str, approver: &TestMember) -> Value {
    json!({
        "leave_type_id": annual_leave_type(app, token).await,
        "start_date": "2026-03-10", "end_date": "2026-03-10", "unit": "full_day",
        "reason": "개인 사정",
        "steps": [{ "approver_id": approver.id, "kind": "approve" }],
    })
}

#[tokio::test]
async fn colleagues_cannot_be_chosen_as_final_approver_of_personal_requests() {
    let app = TestApp::new().await;
    let (head, member) = team(&app).await;
    let department = app.add_department("다른팀", None, None).await;
    let friend = app
        .add_member("friend", Role::Member, Some(department))
        .await;
    let other_head = app
        .add_member("otherhead", Role::Member, Some(department))
        .await;
    app.set_department_head(department, Some(other_head.id))
        .await;

    // 휴가: 동료나 다른 부서의 조직장을 최종 결재자로 지정할 수 없다.
    for approver in [&friend, &other_head] {
        let response = app
            .post("/leave/requests")
            .bearer(&member.token)
            .json(&leave_body(&app, &member.token, approver).await)
            .send()
            .await;
        assert_eq!(response.body["error"]["field"], "steps");
        response.expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    // 근태 정정도 같다.
    let correction = json!({
        "work_date": "2026-03-02", "clock_in_at": 1772409600000i64, "clock_out_at": 1772442000000i64,
        "work_type": "office", "reason": "출근 기록 누락",
        "steps": [{ "approver_id": friend.id, "kind": "approve" }],
    });
    app.set_time("2026-03-03T01:00:00Z");
    app.post("/attendance/corrections")
        .bearer(&member.token)
        .json(&correction)
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    // 거부된 신청은 아무것도 남기지 않았다.
    let requests = app
        .get("/leave/requests?year=2026")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(requests.as_array().expect("array").len(), 0);

    // 관할 조직장과 관리자는 지정할 수 있다. 동료를 앞 단계(합의)로 넣는 것도 된다.
    let mut body = leave_body(&app, &member.token, &head).await;
    body["steps"] = json!([
        { "approver_id": friend.id, "kind": "agree" },
        { "approver_id": head.id, "kind": "approve" },
    ]);
    app.post("/leave/requests")
        .bearer(&member.token)
        .json(&body)
        .send()
        .await
        .expect(StatusCode::CREATED);
}

#[tokio::test]
async fn approver_who_lost_authority_cannot_complete_a_personal_request() {
    let app = TestApp::new().await;
    let (head, member) = team(&app).await;
    let request = app
        .post("/leave/requests")
        .bearer(&member.token)
        .json(&leave_body(&app, &member.token, &head).await)
        .send()
        .await
        .expect(StatusCode::CREATED);
    let document_id = request["document_id"].as_str().expect("id").to_owned();

    // 신청 뒤에 조직장에서 해제됐다.
    let department =
        pango_db::members::find(&mut app.state.db.read().await.expect("conn"), head.id)
            .await
            .expect("find")
            .expect("member")
            .department_id
            .expect("department");
    app.set_department_head(department, None).await;

    let approve = format!("/approval/documents/{document_id}/approve");
    app.post(&approve)
        .bearer(&head.token)
        .json(&json!({}))
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    // 승인은 전부 롤백됐다: 신청은 여전히 대기 중이고 잔여도 그대로 예약 상태다.
    let requests = app
        .get("/leave/requests?year=2026")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(requests[0]["status"], "pending");
    // 반려는 여전히 할 수 있어 문서가 갇히지 않는다.
    app.post(&format!("/approval/documents/{document_id}/reject"))
        .bearer(&head.token)
        .json(&json!({ "reason": "권한 변경으로 반려" }))
        .send()
        .await
        .expect(StatusCode::OK);
}
