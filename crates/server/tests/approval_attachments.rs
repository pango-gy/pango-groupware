//! 결재 문서 첨부파일 통합 테스트.

use http::StatusCode;
use pango_core::authz::Role;
use pango_server::testing::{id_of, TestApp, TestMember, TestResponse};
use serde_json::{json, Value};

struct World {
    app: TestApp,
    owner: String,
    drafter: TestMember,
    approver: TestMember,
    outsider: TestMember,
    form: String,
}

async fn world() -> World {
    let app = TestApp::new().await;
    let owner = app.login_owner().await;
    let drafter = app.add_member("drafter", Role::Member, None).await;
    let approver = app.add_member("approver", Role::Member, None).await;
    let outsider = app.add_member("outsider", Role::Member, None).await;
    let form = id_of(&app.approval_form(&owner, "general").await);
    World {
        app,
        owner,
        drafter,
        approver,
        outsider,
        form,
    }
}

async fn draft(w: &World) -> String {
    let body = json!({
        "form_id": w.form, "title": "출장 품의", "body": "본문",
        "steps": [{ "approver_id": w.approver.id, "kind": "approve" }],
        "references": [], "submit": false,
    });
    id_of(
        &w.app
            .post("/approval/documents")
            .bearer(&w.drafter.token)
            .json(&body)
            .send()
            .await
            .expect(StatusCode::CREATED),
    )
}

async fn upload(w: &World, token: &str, doc: &str, name: &str) -> TestResponse {
    w.app
        .post(&format!("/approval/documents/{doc}/attachments"))
        .bearer(token)
        .header("x-file-name", name)
        .raw_body("application/octet-stream", b"receipt".to_vec())
        .send()
        .await
}

async fn detail(w: &World, token: &str, doc: &str) -> Value {
    w.app
        .get(&format!("/approval/documents/{doc}"))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)
}

async fn download(w: &World, token: &str, doc: &str, file: &str) -> TestResponse {
    w.app
        .get(&format!("/approval/documents/{doc}/attachments/{file}"))
        .bearer(token)
        .send()
        .await
}

#[tokio::test]
async fn drafters_attach_to_drafts_and_attachments_freeze_on_submit() {
    let w = world().await;
    let doc = draft(&w).await;
    let file = upload(&w, &w.drafter.token, &doc, "receipt.pdf")
        .await
        .expect(StatusCode::CREATED);
    let file_id = file["id"].as_str().expect("id").to_owned();
    assert_eq!(
        detail(&w, &w.drafter.token, &doc).await["attachments"],
        json!([file])
    );
    // 임시저장은 기안자만 본다.
    download(&w, &w.approver.token, &doc, &file_id)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");

    w.app
        .post(&format!("/approval/documents/{doc}/submit"))
        .bearer(&w.drafter.token)
        .json(&json!({}))
        .send()
        .await
        .expect(StatusCode::OK);

    let got = download(&w, &w.approver.token, &doc, &file_id).await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.body, "receipt");
    download(&w, &w.outsider.token, &doc, &file_id)
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");

    // 상신한 뒤에는 기안자도 바꿀 수 없다.
    upload(&w, &w.drafter.token, &doc, "more.pdf")
        .await
        .expect_error(StatusCode::CONFLICT, "document_not_editable");
    w.app
        .delete(&format!("/approval/documents/{doc}/attachments/{file_id}"))
        .bearer(&w.drafter.token)
        .send()
        .await
        .expect_error(StatusCode::CONFLICT, "document_not_editable");
    // 결재자는 볼 수는 있지만 붙일 수 없다.
    upload(&w, &w.approver.token, &doc, "x.pdf")
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");

    // 당사자가 아닌 관리자가 받으면 감사 로그가 남는다.
    assert!(w
        .app
        .audit_entries("approval.attachment.admin_download")
        .await
        .is_empty());
    download(&w, &w.owner, &doc, &file_id).await;
    assert_eq!(
        w.app
            .audit_entries("approval.attachment.admin_download")
            .await
            .len(),
        1
    );
    assert_eq!(
        w.app.audit_entries("approval.attachment.add").await.len(),
        1
    );
}

#[tokio::test]
async fn others_cannot_touch_a_draft_and_deleting_it_removes_its_files() {
    let w = world().await;
    let doc = draft(&w).await;
    upload(&w, &w.outsider.token, &doc, "x.pdf")
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    let file = upload(&w, &w.drafter.token, &doc, "a.pdf")
        .await
        .expect(StatusCode::CREATED);
    let file_id = file["id"].as_str().expect("id");

    // 다른 문서의 경로로는 지울 수 없다.
    let other = draft(&w).await;
    w.app
        .delete(&format!(
            "/approval/documents/{other}/attachments/{file_id}"
        ))
        .bearer(&w.drafter.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");

    w.app
        .delete(&format!("/approval/documents/{doc}/attachments/{file_id}"))
        .bearer(&w.drafter.token)
        .send()
        .await
        .expect(StatusCode::NO_CONTENT);
    assert_eq!(
        detail(&w, &w.drafter.token, &doc).await["attachments"],
        json!([])
    );

    upload(&w, &w.drafter.token, &doc, "b.pdf")
        .await
        .expect(StatusCode::CREATED);
    w.app
        .delete(&format!("/approval/documents/{doc}"))
        .bearer(&w.drafter.token)
        .send()
        .await
        .expect(StatusCode::OK);
    assert_eq!(w.app.count("SELECT COUNT(*) FROM attachments").await, 0);
    assert_eq!(
        w.app
            .count("SELECT COUNT(*) FROM attachment_contents")
            .await,
        0
    );
}
