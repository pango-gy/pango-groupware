//! 공지 첨부파일 통합 테스트.

use http::StatusCode;
use pango_core::attachment::MAX_PER_NOTICE;
use pango_core::authz::Role;
use pango_server::config::Config;
use pango_server::testing::{TestApp, TestMember};
use serde_json::{json, Value};

// ── 준비 ────────────────────────────────────────────────────────────────

struct Setup {
    app: TestApp,
    owner: String,
    admin: TestMember,
    member: TestMember,
    outsider: TestMember,
    /// 개발팀 대상 공지.
    notice: String,
}

async fn setup_with(config: Config) -> Setup {
    let app = TestApp::with_config(config).await;
    let team = app.add_department("개발팀", None, None).await;
    let sales = app.add_department("영업팀", None, None).await;
    let owner = app.login_owner().await;
    let admin = app.add_member("admin1", Role::Admin, None).await;
    let member = app.add_member("team1", Role::Member, Some(team)).await;
    let outsider = app.add_member("sales1", Role::Member, Some(sales)).await;
    let notice = app
        .post("/admin/notices")
        .bearer(&owner)
        .json(&json!({
            "title": "개발팀 공지", "body": "본문", "audience": "department",
            "department_id": team,
        }))
        .send()
        .await
        .expect(StatusCode::CREATED)["id"]
        .as_str()
        .expect("id")
        .to_owned();
    Setup {
        app,
        owner,
        admin,
        member,
        outsider,
        notice,
    }
}

async fn setup() -> Setup {
    setup_with(Config::for_tests()).await
}

/// 퍼센트 인코딩(파일 이름 헤더용).
fn encode(name: &str) -> String {
    name.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

async fn upload(
    s: &Setup,
    token: &str,
    notice: &str,
    name: &str,
    data: &[u8],
) -> pango_server::testing::TestResponse {
    s.app
        .post(&format!("/admin/notices/{notice}/attachments"))
        .bearer(token)
        .header("x-file-name", &encode(name))
        .raw_body("application/octet-stream", data.to_vec())
        .send()
        .await
}

async fn attachments(s: &Setup, token: &str) -> Vec<Value> {
    s.app
        .get(&format!("/notices/{}", s.notice))
        .bearer(token)
        .send()
        .await
        .expect(StatusCode::OK)["attachments"]
        .as_array()
        .expect("attachments")
        .clone()
}

fn download_path(s: &Setup, attachment: &str) -> String {
    format!("/notices/{}/attachments/{attachment}", s.notice)
}

// ── 허용 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn admin_uploads_and_audience_member_downloads_it_as_a_forced_download() {
    let s = setup().await;
    let created = upload(&s, &s.admin.token, &s.notice, "회의록 v2.txt", b"hello")
        .await
        .expect(StatusCode::CREATED);
    assert_eq!(created["filename"], "회의록 v2.txt");
    assert_eq!(created["size"], 5);
    let id = created["id"].as_str().expect("id");

    let listed = attachments(&s, &s.member.token).await;
    assert_eq!(listed, vec![created.clone()]);

    let response = s
        .app
        .get(&download_path(&s, id))
        .bearer(&s.member.token)
        .send()
        .await;
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.body, "hello");
    let header = |name: &str| {
        response.headers[name]
            .to_str()
            .expect("ascii header")
            .to_owned()
    };
    assert_eq!(header("content-type"), "application/octet-stream");
    assert_eq!(
        header("content-disposition"),
        "attachment; filename=\"___ v2.txt\"; \
         filename*=UTF-8''%ED%9A%8C%EC%9D%98%EB%A1%9D%20v2.txt"
    );
    assert_eq!(
        header("content-security-policy"),
        "default-src 'none'; sandbox"
    );
    assert_eq!(header("x-content-type-options"), "nosniff");
    assert_eq!(header("cache-control"), "no-store");

    let audit = s.app.audit_entries("notice.attachment.add").await;
    assert_eq!(audit.len(), 1);
    let detail = audit[0].detail.as_ref().expect("detail");
    assert_eq!(detail["filename"], "회의록 v2.txt");
    assert_eq!(
        detail["sha256"],
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
}

#[tokio::test]
async fn same_idempotency_key_uploads_once() {
    let s = setup().await;
    let send = |data: &'static [u8]| {
        s.app
            .post(&format!("/admin/notices/{}/attachments", s.notice))
            .bearer(&s.owner)
            .header("x-file-name", "a.txt")
            .header("idempotency-key", "upload-1")
            .raw_body("application/octet-stream", data.to_vec())
            .send()
    };
    let first = send(b"same").await.expect(StatusCode::CREATED);
    let again = send(b"same").await.expect(StatusCode::CREATED);
    assert_eq!(first, again);
    assert_eq!(attachments(&s, &s.owner).await.len(), 1);

    send(b"different")
        .await
        .expect_error(StatusCode::UNPROCESSABLE_ENTITY, "idempotency_key_reused");
}

#[tokio::test]
async fn removing_deletes_the_content_and_is_audited() {
    let s = setup().await;
    let created = upload(&s, &s.owner, &s.notice, "a.txt", b"bye")
        .await
        .expect(StatusCode::CREATED);
    let id = created["id"].as_str().expect("id");
    let path = format!("/admin/notices/{}/attachments/{id}", s.notice);

    s.app
        .delete(&path)
        .bearer(&s.member.token)
        .send()
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    s.app
        .delete(&path)
        .bearer(&s.admin.token)
        .send()
        .await
        .expect(StatusCode::NO_CONTENT);

    assert!(attachments(&s, &s.owner).await.is_empty());
    assert_eq!(
        s.app
            .count("SELECT COUNT(*) FROM notice_attachment_contents")
            .await,
        0
    );
    s.app
        .get(&download_path(&s, id))
        .bearer(&s.member.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    s.app
        .delete(&path)
        .bearer(&s.admin.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    let audit = s.app.audit_entries("notice.attachment.remove").await;
    assert_eq!(audit.len(), 1);
    assert_eq!(
        audit[0].detail.as_ref().expect("detail")["filename"],
        "a.txt"
    );
}

// ── 거부 ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn only_admins_upload_and_only_the_audience_downloads() {
    let s = setup().await;
    upload(&s, &s.member.token, &s.notice, "a.txt", b"x")
        .await
        .expect_error(StatusCode::FORBIDDEN, "forbidden");
    let created = upload(&s, &s.owner, &s.notice, "a.txt", b"x")
        .await
        .expect(StatusCode::CREATED);
    let id = created["id"].as_str().expect("id");

    // 대상 밖 구성원에게는 공지와 첨부가 없는 것과 같다.
    s.app
        .get(&download_path(&s, id))
        .bearer(&s.outsider.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    s.app
        .get(&download_path(&s, id))
        .send()
        .await
        .expect_error(StatusCode::UNAUTHORIZED, "unauthenticated");

    // 다른 공지의 경로로는 이 첨부를 받을 수 없다.
    let other = s
        .app
        .post("/admin/notices")
        .bearer(&s.owner)
        .json(&json!({ "title": "전체 공지", "body": "본문", "audience": "all" }))
        .send()
        .await
        .expect(StatusCode::CREATED);
    let other = other["id"].as_str().expect("id");
    s.app
        .get(&format!("/notices/{other}/attachments/{id}"))
        .bearer(&s.outsider.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn malformed_uploads_are_rejected_without_storing_anything() {
    let s = setup().await;
    let path = format!("/admin/notices/{}/attachments", s.notice);

    s.app
        .post(&path)
        .bearer(&s.owner)
        .header("x-file-name", "a.txt")
        .raw_body("text/plain", b"x".to_vec())
        .send()
        .await
        .expect_error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type");
    s.app
        .post(&path)
        .bearer(&s.owner)
        .raw_body("application/octet-stream", b"x".to_vec())
        .send()
        .await
        .expect_error(StatusCode::BAD_REQUEST, "malformed_request");
    for bad in ["../etc/passwd", "a\\b.txt", "..", "   "] {
        upload(&s, &s.owner, &s.notice, bad, b"x")
            .await
            .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    }
    upload(&s, &s.owner, &s.notice, "empty.txt", b"")
        .await
        .expect_error(StatusCode::BAD_REQUEST, "validation_failed");
    upload(&s, &s.owner, "not-an-id", "a.txt", b"x")
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");

    assert_eq!(
        s.app.count("SELECT COUNT(*) FROM notice_attachments").await,
        0
    );
}

#[tokio::test]
async fn size_limit_applies_to_uploads_only() {
    let s = setup_with(Config {
        max_attachment_bytes: 16,
        ..Config::for_tests()
    })
    .await;
    upload(&s, &s.owner, &s.notice, "ok.bin", &[7; 16])
        .await
        .expect(StatusCode::CREATED);
    upload(&s, &s.owner, &s.notice, "big.bin", &[7; 17])
        .await
        .expect_error(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large");
    // 다른 경로의 본문 상한은 그대로다(첨부 상한이 작아도 공지 작성은 된다).
    s.app
        .post("/admin/notices")
        .bearer(&s.owner)
        .json(&json!({ "title": "제목", "body": "a".repeat(100), "audience": "all" }))
        .send()
        .await
        .expect(StatusCode::CREATED);
}

#[tokio::test]
async fn attachment_count_is_limited_per_notice() {
    let s = setup().await;
    for n in 0..MAX_PER_NOTICE {
        upload(&s, &s.owner, &s.notice, &format!("{n}.txt"), b"x")
            .await
            .expect(StatusCode::CREATED);
    }
    upload(&s, &s.owner, &s.notice, "one-more.txt", b"x")
        .await
        .expect_error(StatusCode::CONFLICT, "attachment_limit");
    assert_eq!(attachments(&s, &s.owner).await.len(), MAX_PER_NOTICE);
}

#[tokio::test]
async fn archived_notice_takes_no_uploads_and_hides_files_but_allows_removal() {
    let s = setup().await;
    let created = upload(&s, &s.owner, &s.notice, "a.txt", b"x")
        .await
        .expect(StatusCode::CREATED);
    let id = created["id"].as_str().expect("id");
    let version = s
        .app
        .get(&format!("/notices/{}", s.notice))
        .bearer(&s.owner)
        .send()
        .await
        .expect(StatusCode::OK)["version"]
        .as_i64()
        .expect("version");
    s.app
        .delete(&format!("/admin/notices/{}?version={version}", s.notice))
        .bearer(&s.owner)
        .send()
        .await
        .expect(StatusCode::OK);

    upload(&s, &s.owner, &s.notice, "b.txt", b"x")
        .await
        .expect_error(StatusCode::CONFLICT, "notice_archived");
    s.app
        .get(&download_path(&s, id))
        .bearer(&s.member.token)
        .send()
        .await
        .expect_error(StatusCode::NOT_FOUND, "not_found");
    s.app
        .delete(&format!("/admin/notices/{}/attachments/{id}", s.notice))
        .bearer(&s.owner)
        .send()
        .await
        .expect(StatusCode::NO_CONTENT);
}
