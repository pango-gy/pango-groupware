//! 서비스 오류와 HTTP 응답 변환. 상태 코드는 이 파일에서만 정한다.

use axum::response::{IntoResponse, Response};
use axum::Json;
use http::{header, HeaderValue, StatusCode};
use pango_api::{ErrorBody, ErrorDetail};
use pango_core::DomainError;
use pango_db::DbError;

pub type ServiceResult<T> = Result<T, ServiceError>;

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Domain(#[from] DomainError),
    /// 아이디가 없거나, 비밀번호가 틀렸거나, 로그인할 수 없는 계정. 구분해서 알려 주지 않는다.
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("too many attempts")]
    TooManyAttempts { retry_after_secs: u64 },
    /// 임시 비밀번호 상태. 비밀번호를 바꾸기 전에는 다른 기능을 쓸 수 없다.
    #[error("password change required")]
    PasswordChangeRequired,
    #[error("idempotency key reused with a different request")]
    IdempotencyKeyReused,
    #[error("cross-origin request rejected")]
    CrossOrigin,
    #[error("malformed request: {0}")]
    Malformed(&'static str),
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("unsupported media type")]
    UnsupportedMediaType,
    #[error("internal error: {0}")]
    Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl ServiceError {
    pub fn internal(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Internal(error.into())
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::Domain(DomainError::Validation { .. }) | Self::Malformed(_) => {
                StatusCode::BAD_REQUEST
            }
            Self::Domain(DomainError::Unauthenticated) | Self::InvalidCredentials => {
                StatusCode::UNAUTHORIZED
            }
            Self::Domain(DomainError::Forbidden)
            | Self::PasswordChangeRequired
            | Self::CrossOrigin => StatusCode::FORBIDDEN,
            Self::Domain(DomainError::NotFound) => StatusCode::NOT_FOUND,
            Self::Domain(DomainError::Conflict(_)) => StatusCode::CONFLICT,
            Self::IdempotencyKeyReused => StatusCode::UNPROCESSABLE_ENTITY,
            Self::TooManyAttempts { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Domain(error) => error.code(),
            Self::InvalidCredentials => "invalid_credentials",
            Self::TooManyAttempts { .. } => "too_many_attempts",
            Self::PasswordChangeRequired => "password_change_required",
            Self::IdempotencyKeyReused => "idempotency_key_reused",
            Self::CrossOrigin => "cross_origin_rejected",
            Self::Malformed(_) => "malformed_request",
            Self::PayloadTooLarge => "payload_too_large",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::Internal(_) => "internal_error",
        }
    }

    pub fn body(&self) -> ErrorBody {
        let (field, reason) = match self {
            Self::Domain(DomainError::Validation { field, reason }) => {
                (Some((*field).to_owned()), Some(reason.code().to_owned()))
            }
            _ => (None, None),
        };
        // 내부 오류의 원인은 응답에 싣지 않는다(로그에만 남긴다).
        let message = match self {
            Self::Internal(_) => "internal error".to_owned(),
            other => other.to_string(),
        };
        ErrorBody {
            error: ErrorDetail {
                code: self.code().to_owned(),
                message,
                field,
                reason,
            },
        }
    }
}

impl From<DbError> for ServiceError {
    fn from(error: DbError) -> Self {
        Self::internal(error)
    }
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        if let Self::Internal(source) = &self {
            tracing::error!(error = %source, "internal error");
        }
        let mut response = (self.status(), Json(self.body())).into_response();
        if let Self::TooManyAttempts { retry_after_secs } = self {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(retry_after_secs));
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pango_core::error::{ConflictReason, ValidationReason};

    #[test]
    fn status_mapping() {
        let cases: [(ServiceError, u16, &str); 9] = [
            (
                DomainError::validation("name", ValidationReason::Empty).into(),
                400,
                "validation_failed",
            ),
            (DomainError::Unauthenticated.into(), 401, "unauthenticated"),
            (ServiceError::InvalidCredentials, 401, "invalid_credentials"),
            (DomainError::Forbidden.into(), 403, "forbidden"),
            (DomainError::NotFound.into(), 404, "not_found"),
            (
                DomainError::Conflict(ConflictReason::VersionConflict).into(),
                409,
                "version_conflict",
            ),
            (
                ServiceError::IdempotencyKeyReused,
                422,
                "idempotency_key_reused",
            ),
            (
                ServiceError::TooManyAttempts {
                    retry_after_secs: 3,
                },
                429,
                "too_many_attempts",
            ),
            (ServiceError::internal("boom"), 500, "internal_error"),
        ];
        for (error, status, code) in cases {
            assert_eq!(error.status().as_u16(), status, "{error}");
            assert_eq!(error.code(), code);
        }
    }

    #[test]
    fn validation_body_carries_field_and_reason() {
        let error: ServiceError =
            DomainError::validation("title", ValidationReason::TooLong { max: 200 }).into();
        let body = error.body();
        assert_eq!(body.error.field.as_deref(), Some("title"));
        assert_eq!(body.error.reason.as_deref(), Some("too_long"));
    }

    #[test]
    fn internal_error_body_does_not_leak_cause() {
        let body = ServiceError::internal("password_hash column missing at /var/db").body();
        assert_eq!(body.error.message, "internal error");
    }
}
