//! 파일 내려받기 응답(첨부파일·내보내기).

use axum::response::{IntoResponse, Response};
use http::{header, HeaderValue};

use crate::error::{ServiceError, ServiceResult};

/// 내려받기 응답. 올린 사람이 정한 형식을 믿지 않고 항상 내려받기(`attachment`)로만 보낸다.
/// HTML·SVG 같은 파일이 이 출처에서 열려 스크립트가 실행되지 않도록 CSP `sandbox`를 붙인다.
pub fn file_response(
    content_type: &'static str,
    filename: &str,
    body: Vec<u8>,
) -> ServiceResult<Response> {
    let disposition =
        HeaderValue::from_str(&content_disposition(filename)).map_err(ServiceError::internal)?;
    Ok((
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CONTENT_DISPOSITION, disposition),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("default-src 'none'; sandbox"),
            ),
        ],
        body,
    )
        .into_response())
}

/// 첨부파일 응답(형식은 항상 `application/octet-stream`).
pub fn attachment_response(
    file: crate::service::attachments::AttachmentFile,
) -> ServiceResult<Response> {
    file_response("application/octet-stream", &file.filename, file.data)
}

/// `attachment; filename="..."; filename*=UTF-8''...`(RFC 6266). `filename`은 오래된
/// 클라이언트용 ASCII 대체 이름이고, 실제 이름은 `filename*`에 퍼센트 인코딩해 담는다.
pub fn content_disposition(filename: &str) -> String {
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
