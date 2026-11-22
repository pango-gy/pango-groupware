//! 퇴근 누락 알림 통합 테스트. 시계는 2026-03-02(월) 09:00 KST, 근무 09:00–18:00.

use http::StatusCode;
use pango_core::authz::Role;
use pango_core::member::MemberStatus;
use pango_server::service::reminders::{remind_clock_outs, CLOCK_OUT_MISSING};
use pango_server::testing::{TestApp, TestMember};
use serde_json::json;

async fn clock_in(app: &TestApp, member: &TestMember) {
    app.post("/attendance/clock-in")
        .bearer(&member.token)
        .json(&json!({ "work_type": "office" }))
        .send()
        .await
        .expect(StatusCode::OK);
}

async fn reminders(app: &TestApp, member: &TestMember) -> Vec<String> {
    app.notifications_of(member.id)
        .await
        .into_iter()
        .filter(|n| n.kind == CLOCK_OUT_MISSING)
        .map(|n| n.title)
        .collect()
}

#[tokio::test]
async fn an_open_record_is_reminded_once_an_hour_after_work_ends() {
    let app = TestApp::new().await;
    app.set_time("2026-03-02T09:00:00+09:00");
    let forgot = app.add_member("kim", Role::Member, None).await;
    let left = app.add_member("lee", Role::Member, None).await;
    let absent = app.add_member("park", Role::Member, None).await;
    clock_in(&app, &forgot).await;
    clock_in(&app, &left).await;

    app.set_time("2026-03-02T18:30:00+09:00");
    app.post("/attendance/clock-out")
        .bearer(&left.token)
        .send()
        .await
        .expect(StatusCode::OK);
    app.set_time("2026-03-02T18:59:00+09:00");
    assert_eq!(remind_clock_outs(&app.state).await.expect("run"), 0);

    app.set_time("2026-03-02T19:00:00+09:00");
    assert_eq!(remind_clock_outs(&app.state).await.expect("run"), 1);
    assert_eq!(
        reminders(&app, &forgot).await,
        ["3월 2일 퇴근 기록이 없어요"]
    );
    assert!(reminders(&app, &left).await.is_empty(), "퇴근한 사람");
    assert!(
        reminders(&app, &absent).await.is_empty(),
        "출근하지 않은 사람"
    );

    // 다시 돌려도 한 번만.
    app.set_time("2026-03-03T08:00:00+09:00");
    assert_eq!(remind_clock_outs(&app.state).await.expect("run"), 0);
    assert_eq!(reminders(&app, &forgot).await.len(), 1);
}

#[tokio::test]
async fn late_starters_inactive_members_and_old_records_are_not_reminded() {
    let app = TestApp::new().await;
    let late = app.add_member("late", Role::Member, None).await;
    let retired = app.add_member("gone", Role::Member, None).await;
    let old = app.add_member("old", Role::Member, None).await;

    app.set_time("2026-03-02T09:00:00+09:00");
    clock_in(&app, &retired).await;
    clock_in(&app, &old).await;
    app.set_status(retired.id, MemberStatus::Retired).await;

    // 업무 종료 뒤 20:30에 출근 → 출근 1시간 뒤(21:30)부터.
    app.set_time("2026-03-02T20:30:00+09:00");
    clock_in(&app, &late).await;
    app.set_time("2026-03-02T21:29:00+09:00");
    remind_clock_outs(&app.state).await.expect("run");
    assert!(reminders(&app, &late).await.is_empty());
    assert_eq!(reminders(&app, &old).await.len(), 1);
    assert!(reminders(&app, &retired).await.is_empty(), "퇴사자");
    app.set_time("2026-03-02T21:30:00+09:00");
    remind_clock_outs(&app.state).await.expect("run");
    assert_eq!(reminders(&app, &late).await.len(), 1);

    // 서버를 오래 꺼 두었다 켜도 이틀보다 오래된 기록에는 알리지 않는다.
    let app = TestApp::new().await;
    let member = app.add_member("kim", Role::Member, None).await;
    app.set_time("2026-03-02T09:00:00+09:00");
    clock_in(&app, &member).await;
    app.set_time("2026-03-05T12:00:00+09:00");
    assert_eq!(remind_clock_outs(&app.state).await.expect("run"), 0);
}
