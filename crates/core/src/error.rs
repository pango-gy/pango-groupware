//! 도메인 오류. HTTP 상태나 문구는 여기서 정하지 않고, 안정적인 `code()`만 제공한다.

use std::fmt;

pub type DomainResult<T> = Result<T, DomainError>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    #[error("validation failed on `{field}`: {reason}")]
    Validation {
        field: &'static str,
        reason: ValidationReason,
    },
    #[error("authentication required")]
    Unauthenticated,
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(ConflictReason),
}

impl DomainError {
    pub const fn validation(field: &'static str, reason: ValidationReason) -> Self {
        Self::Validation { field, reason }
    }

    /// 입력 오류의 필드 이름을 요청 필드로 바꾼다. 값 타입의 검증(`Reason::parse` 등)은 자기
    /// 이름으로 오류를 내므로, 요청의 어느 필드였는지 알릴 때 쓴다. 입력 오류가 아니면 그대로다.
    #[must_use]
    pub fn with_field(self, field: &'static str) -> Self {
        match self {
            Self::Validation { reason, .. } => Self::Validation { field, reason },
            other => other,
        }
    }

    /// API와 UI가 공유하는 안정적인 오류 코드.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation { .. } => "validation_failed",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict(reason) => reason.code(),
        }
    }
}

impl From<ConflictReason> for DomainError {
    fn from(reason: ConflictReason) -> Self {
        Self::Conflict(reason)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationReason {
    Empty,
    TooShort {
        min: usize,
    },
    TooLong {
        max: usize,
    },
    InvalidFormat,
    InvalidCharacter,
    OutOfRange,
    /// 다른 필드와의 관계가 맞지 않음 (예: 종료일이 시작일보다 빠름).
    Inconsistent,
    Duplicate,
}

impl ValidationReason {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooShort { .. } => "too_short",
            Self::TooLong { .. } => "too_long",
            Self::InvalidFormat => "invalid_format",
            Self::InvalidCharacter => "invalid_character",
            Self::OutOfRange => "out_of_range",
            Self::Inconsistent => "inconsistent",
            Self::Duplicate => "duplicate",
        }
    }
}

impl fmt::Display for ValidationReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { min } => write!(f, "too_short(min={min})"),
            Self::TooLong { max } => write!(f, "too_long(max={max})"),
            other => f.write_str(other.code()),
        }
    }
}

/// 상태 충돌의 종류. 새 종류는 여기에만 추가한다.
macro_rules! conflict_reasons {
    ($($variant:ident => $code:literal),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum ConflictReason { $($variant),+ }

        impl ConflictReason {
            pub const ALL: &'static [ConflictReason] = &[$(Self::$variant),+];

            pub const fn code(&self) -> &'static str {
                match self { $(Self::$variant => $code),+ }
            }
        }
    };
}

conflict_reasons! {
    // 공통
    VersionConflict => "version_conflict",
    AlreadyExists => "already_exists",
    InUse => "in_use",
    // 계정
    LoginIdTaken => "login_id_taken",
    LastOwner => "last_owner",
    MemberInactive => "member_inactive",
    PasswordReused => "password_reused",
    // 조직
    DepartmentCycle => "department_cycle",
    DepartmentTooDeep => "department_too_deep",
    DepartmentNotEmpty => "department_not_empty",
    // 출퇴근
    NotClockedIn => "not_clocked_in",
    AlreadyClockedOut => "already_clocked_out",
    ClockOutBeforeClockIn => "clock_out_before_clock_in",
    NotWorkingMember => "not_working_member",
    // 휴가
    InsufficientLeaveBalance => "insufficient_leave_balance",
    LeaveOverlap => "leave_overlap",
    LeaveNoWorkingDays => "leave_no_working_days",
    LeaveTypeInactive => "leave_type_inactive",
    LeaveAlreadyStarted => "leave_already_started",
    // 결재
    DocumentNotEditable => "document_not_editable",
    DocumentAlreadyClosed => "document_already_closed",
    NotCurrentApprover => "not_current_approver",
    WithdrawTooLate => "withdraw_too_late",
    FormInactive => "form_inactive",
    // 공지
    NoticeArchived => "notice_archived",
    AttachmentLimit => "attachment_limit",
    // 외부 알림(Buzz)
    BuzzUnavailable => "buzz_unavailable",
    BuzzAccountTaken => "buzz_account_taken",
    BuzzCodeWrong => "buzz_code_wrong",
    BuzzCodeExpired => "buzz_code_expired",
    BuzzCodeExhausted => "buzz_code_exhausted",
    BuzzCodeMissing => "buzz_code_missing",
}

impl fmt::Display for ConflictReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn with_field_relabels_only_validation_errors() {
        let error = DomainError::validation("reason", ValidationReason::TooLong { max: 5 });
        assert_eq!(
            error.with_field("memo"),
            DomainError::validation("memo", ValidationReason::TooLong { max: 5 })
        );
        assert_eq!(
            DomainError::NotFound.with_field("memo"),
            DomainError::NotFound
        );
        assert_eq!(
            DomainError::Conflict(ConflictReason::VersionConflict).with_field("memo"),
            DomainError::Conflict(ConflictReason::VersionConflict)
        );
    }

    #[test]
    fn conflict_codes_are_unique_snake_case() {
        let mut seen = HashSet::new();
        for reason in ConflictReason::ALL {
            let code = reason.code();
            assert!(seen.insert(code), "duplicate code {code}");
            assert!(
                code.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "not snake_case: {code}"
            );
        }
    }

    #[test]
    fn conflict_codes_do_not_collide_with_top_level_codes() {
        let reserved = [
            "validation_failed",
            "unauthenticated",
            "forbidden",
            "not_found",
        ];
        for reason in ConflictReason::ALL {
            assert!(!reserved.contains(&reason.code()));
        }
    }

    #[test]
    fn domain_error_code_delegates_to_reason() {
        let err: DomainError = ConflictReason::LeaveOverlap.into();
        assert_eq!(err.code(), "leave_overlap");
        assert_eq!(
            DomainError::validation("name", ValidationReason::Empty).code(),
            "validation_failed"
        );
    }
}
