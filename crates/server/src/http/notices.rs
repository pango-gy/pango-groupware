//! 공지사항 경로(구성원용 + 관리자용).

use axum::extract::{DefaultBodyLimit, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Json, Router};
use http::{header, HeaderValue, StatusCode};
use pango_api::common::VersionQuery;
use pango_api::notice::{
    NoticeAttachment, NoticeDetail, NoticeInput, NoticeReadReceipt, NoticeReadReport,
    NoticeSummary, NoticeUnreadCount,
};
use pango_api::{Page, PageQuery, Versioned};
use pango_core::id::{NoticeAttachmentId, NoticeId};

use super::extract::{ApiJson, ApiPath, ApiQuery, Auth, FileUpload, IdempotentJson};
use crate::error::{ServiceError, ServiceResult};
use crate::service::notices;
use crate::state::AppState;

/// `max_attachment_bytes`: 첨부 올리기 경로의 본문 상한(다른 경로는 공통 상한을 쓴다).
pub fn routes(max_attachment_bytes: usize) -> Router<AppState> {
    Router::new()
        .route("/notices", get(list))
        .route("/notices/unread-count", get(unread_count))
        .route("/notices/{id}", get(detail))
        .route("/notices/{id}/read", post(mark_read))
        .route(
            "/notices/{id}/attachments/{attachment_id}",
            get(download_attachment),
        )
        .route("/admin/notices", get(admin_list).post(create))
        .route("/admin/notices/{id}", patch(update).delete(archive))
        .route("/admin/notices/{id}/reads", get(read_report))
        .route(
            "/admin/notices/{id}/attachments",
            post(add_attachment).layer(DefaultBodyLimit::max(max_attachment_bytes)),
        )
        .route(
            "/admin/notices/{id}/attachments/{attachment_id}",
            delete(remove_attachment),
        )
}

async fn list(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> ServiceResult<Json<Page<NoticeSummary>>> {
    notices::list(&state, &ctx, &query).await.map(Json)
}

async fn unread_count(
    State(state): State<AppState>,
    Auth(ctx): Auth,
) -> ServiceResult<Json<NoticeUnreadCount>> {
    notices::unread_count(&state, &ctx).await.map(Json)
}

async fn detail(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
) -> ServiceResult<Json<NoticeDetail>> {
    notices::get(&state, &ctx, id).await.map(Json)
}

async fn mark_read(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
) -> ServiceResult<Json<NoticeReadReceipt>> {
    notices::mark_read(&state, &ctx, id).await.map(Json)
}

async fn admin_list(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiQuery(query): ApiQuery<PageQuery>,
) -> ServiceResult<Json<Page<NoticeSummary>>> {
    notices::admin_list(&state, &ctx, &query).await.map(Json)
}

async fn create(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    request: IdempotentJson<NoticeInput>,
) -> ServiceResult<(StatusCode, Json<NoticeDetail>)> {
    let notice = notices::create(&state, &ctx, &request.body, request.idempotency.as_ref()).await?;
    Ok((StatusCode::CREATED, Json(notice)))
}

async fn update(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
    ApiJson(request): ApiJson<Versioned<NoticeInput>>,
) -> ServiceResult<Json<NoticeDetail>> {
    notices::update(&state, &ctx, id, &request).await.map(Json)
}

async fn archive(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
    ApiQuery(query): ApiQuery<VersionQuery>,
) -> ServiceResult<Json<NoticeDetail>> {
    notices::archive(&state, &ctx, id, query.version)
        .await
        .map(Json)
}

async fn read_report(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
) -> ServiceResult<Json<NoticeReadReport>> {
    notices::read_report(&state, &ctx, id).await.map(Json)
}

async fn add_attachment(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath(id): ApiPath<NoticeId>,
    upload: FileUpload,
) -> ServiceResult<(StatusCode, Json<NoticeAttachment>)> {
    let created = notices::add_attachment(
        &state,
        &ctx,
        id,
        &upload.filename,
        &upload.data,
        upload.idempotency.as_ref(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn remove_attachment(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath((id, attachment_id)): ApiPath<(NoticeId, NoticeAttachmentId)>,
) -> ServiceResult<StatusCode> {
    notices::remove_attachment(&state, &ctx, id, attachment_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 첨부파일 내려받기. 올린 사람이 정한 형식을 믿지 않고 항상 내려받기(`attachment`)로만
/// 보낸다. HTML·SVG 같은 파일이 이 출처에서 열려 스크립트가 실행되지 않도록, 형식은
/// `application/octet-stream`으로 고정하고 CSP `sandbox`를 붙인다.
async fn download_attachment(
    State(state): State<AppState>,
    Auth(ctx): Auth,
    ApiPath((id, attachment_id)): ApiPath<(NoticeId, NoticeAttachmentId)>,
) -> ServiceResult<Response> {
    let file = notices::download_attachment(&state, &ctx, id, attachment_id).await?;
    let disposition = HeaderValue::from_str(&content_disposition(&file.filename))
        .map_err(ServiceError::internal)?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("default-src 'none'; sandbox"),
            ),
        ],
        file.data,
    )
        .into_response())
}

/// `attachment; filename="..."; filename*=UTF-8''...`(RFC 6266). `filename`은 오래된
/// 클라이언트용 ASCII 대체 이름이고, 실제 이름은 `filename*`에 퍼센트 인코딩해 담는다.
fn content_disposition(filename: &str) -> String {
    let fallback: String = filename
        .chars()
        .map(|c| match c {
            ' '..='~' if c != '"' && c != '\\' => c,
            _ => '_',
        })
        .collect();
    let mut encoded = String::with_capacity(filename.len() * 3);
    for byte in filename.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

#[cfg(test)]
mod tests {
    use super::content_disposition;

    #[test]
    fn content_disposition_keeps_the_real_name_and_a_safe_fallback() {
        assert_eq!(
            content_disposition("공지 v2.pdf"),
            "attachment; filename=\"__ v2.pdf\"; filename*=UTF-8''%EA%B3%B5%EC%A7%80%20v2.pdf"
        );
        // 따옴표·역슬래시로 헤더 값을 끊거나 파라미터를 덧붙일 수 없다.
        let tricky = content_disposition("a\";b\\c.txt");
        assert_eq!(
            tricky,
            "attachment; filename=\"a_;b_c.txt\"; filename*=UTF-8''a%22%3Bb%5Cc.txt"
        );
        assert!(http::HeaderValue::from_str(&tricky).is_ok());
    }
}
