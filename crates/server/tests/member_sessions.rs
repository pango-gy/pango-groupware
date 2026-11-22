//! 관리자의 구성원 로그인 기기(세션) 관리 통합 테스트.

use http::StatusCode;
use pango_core::authz::Role;
use pango_server::testing::{TestApp, MEMBER_PASSWORD};
use serde_json::Value;

fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .expect("list")
        .iter()
        .map(|s| s["id"].as_str().expect("id").to_owned())
        .collect()
}

#[tokio::test]
async fn admins_list_and_sign_out_a_members_devices() {
    let app = TestApp::new().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let member = app.add_member("kim", Role::Member, None).await;
    let phone = app.login("kim", MEMBER_PASSWORD).await;
    let path = format!("/admin/members/{}/sessions", member.id);

    let list = app
        .get(&path)
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    let sessions = ids(&list);
    assert_eq!(sessions.len(), 2);
    assert!(list[0]["current"] == false && list[1]["current"] == false);

    // 하나만 끊으면 그 기기만 로그아웃된다.
    let phone_session = app.auth_context(&phone).await.session_id.to_string();
    app.delete(&format!("{path}/{phone_session}"))
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::NO_CONTENT);
    app.get("/me")
        .bearer(&phone)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    app.get("/me")
        .bearer(&member.token)
        .send()
        .await
        .expect(StatusCode::OK);
    // 다시 보내도 성공(이미 끊김).
    app.delete(&format!("{path}/{phone_session}"))
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::NO_CONTENT);
    assert_eq!(app.audit_entries("member.session_revoke").await.len(), 1);

    // 모두 끊기.
    let all = app
        .delete(&path)
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(all["revoked"], 1);
    app.get("/me")
        .bearer(&member.token)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");
    assert_eq!(
        app.audit_entries("member.sessions_revoke_all").await.len(),
        1
    );
    assert!(ids(&app
        .get(&path)
        .bearer(&admin.token)
        .send()
        .await
        .expect(StatusCode::OK))
    .is_empty());
}

#[tokio::test]
async fn device_management_follows_account_management_rights() {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let other_admin = app.add_member("admin2", Role::Admin, None).await;
    let member = app.add_member("kim", Role::Member, None).await;
    let other = app.add_member("lee", Role::Member, None).await;

    // 일반 구성원은 볼 수 없다.
    app.get(&format!("/admin/members/{}/sessions", other.id))
        .bearer(&member.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    // 관리자는 다른 관리자의 기기를 다룰 수 없고, 최고 관리자는 된다.
    app.get(&format!("/admin/members/{}/sessions", other_admin.id))
        .bearer(&admin.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    app.get(&format!("/admin/members/{}/sessions", other_admin.id))
        .bearer(&owner)
        .send()
        .await
        .expect(StatusCode::OK);
    // 관리자는 본인 계정도 관리 대상이 아니다(최고 관리자만 관리자 계정을 다룬다).
    app.delete(&format!("/admin/members/{}/sessions", admin.id))
        .bearer(&admin.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    // 최고 관리자가 자기 기기를 모두 끊으면 지금 쓰는 세션은 남는다.
    let owner_phone = app.login_owner().await;
    let done = app
        .delete(&format!("/admin/members/{}/sessions", app.owner_id))
        .bearer(&owner)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(done["revoked"], 1);
    app.get("/me")
        .bearer(&owner)
        .send()
        .await
        .expect(StatusCode::OK);
    app.get("/me")
        .bearer(&owner_phone)
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");

    // 다른 구성원의 세션 ID로는 끊을 수 없다.
    let others_session = app.auth_context(&other.token).await.session_id;
    app.delete(&format!(
        "/admin/members/{}/sessions/{others_session}",
        member.id
    ))
    .bearer(&admin.token)
    .send()
    .await
    .expect_error(StatusCode::NOT_FOUND, "not_found");
    app.get("/me")
        .bearer(&other.token)
        .send()
        .await
        .expect(StatusCode::OK);
}
