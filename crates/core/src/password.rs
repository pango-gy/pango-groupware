//! 비밀번호 정책. 해시는 서버가 담당하고, 여기서는 형태만 판정한다.

use crate::error::{DomainError, DomainResult, ValidationReason};
use crate::text::LoginId;

pub const MIN_CHARS: usize = 10;
/// 해시 연산 비용이 입력 길이에 비례하므로 상한을 둔다.
pub const MAX_CHARS: usize = 128;

const FIELD: &str = "password";

/// 정책을 통과한 평문 비밀번호. 로그에 찍히지 않도록 `Debug`가 값을 가린다.
#[derive(Clone, PartialEq, Eq)]
pub struct PlainPassword(String);

impl PlainPassword {
    /// 새 비밀번호를 정책에 맞춰 검증한다.
    pub fn parse_new(raw: &str, login_id: &LoginId) -> DomainResult<Self> {
        let err = |reason| DomainError::validation(FIELD, reason);
        // 바이트 길이를 먼저 확인해 거대한 입력을 순회하지 않는다.
        if raw.len() > MAX_CHARS * 4 {
            return Err(err(ValidationReason::TooLong { max: MAX_CHARS }));
        }
        let chars = raw.chars().count();
        if chars == 0 {
            return Err(err(ValidationReason::Empty));
        }
        if chars < MIN_CHARS {
            return Err(err(ValidationReason::TooShort { min: MIN_CHARS }));
        }
        if chars > MAX_CHARS {
            return Err(err(ValidationReason::TooLong { max: MAX_CHARS }));
        }
        if raw.chars().any(char::is_control) {
            return Err(err(ValidationReason::InvalidCharacter));
        }
        let lowered = raw.to_lowercase();
        let all_same = raw.chars().all(|c| Some(c) == raw.chars().next());
        if all_same || lowered.contains(login_id.as_str()) {
            return Err(err(ValidationReason::InvalidFormat));
        }
        Ok(Self(raw.to_owned()))
    }

    /// 로그인 시 입력된 비밀번호. 정책은 적용하지 않고 길이 상한만 확인한다.
    /// (정책이 강화되기 전에 만든 비밀번호로도 로그인할 수 있어야 한다.)
    pub fn for_verification(raw: &str) -> DomainResult<Self> {
        if raw.is_empty() {
            return Err(DomainError::validation(FIELD, ValidationReason::Empty));
        }
        if raw.len() > MAX_CHARS * 4 {
            return Err(DomainError::validation(FIELD, ValidationReason::TooLong { max: MAX_CHARS }));
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PlainPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PlainPassword(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login() -> LoginId {
        LoginId::parse("gildong").expect("login id")
    }

    fn reason(raw: &str) -> ValidationReason {
        match PlainPassword::parse_new(raw, &login()) {
            Err(DomainError::Validation { reason, .. }) => reason,
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn accepts_boundary_lengths() {
        assert!(PlainPassword::parse_new("abcdefgh10", &login()).is_ok());
        let max = "ab".repeat(64);
        assert!(PlainPassword::parse_new(&max, &login()).is_ok());
        assert!(PlainPassword::parse_new("비밀번호는길어야합니다", &login()).is_ok());
    }

    #[test]
    fn rejects_out_of_range_lengths() {
        assert_eq!(reason(""), ValidationReason::Empty);
        assert_eq!(reason("abcdefgh9"), ValidationReason::TooShort { min: MIN_CHARS });
        assert_eq!(reason(&format!("{}c", "ab".repeat(64))), ValidationReason::TooLong { max: MAX_CHARS });
        assert_eq!(reason(&"x".repeat(10_000_000)), ValidationReason::TooLong { max: MAX_CHARS });
    }

    #[test]
    fn rejects_passwords_derived_from_login_id() {
        assert_eq!(reason("GilDong2026!"), ValidationReason::InvalidFormat);
        assert_eq!(reason("xxgildongxx"), ValidationReason::InvalidFormat);
    }

    #[test]
    fn rejects_single_repeated_character_and_control_characters() {
        assert_eq!(reason("aaaaaaaaaaaa"), ValidationReason::InvalidFormat);
        assert_eq!(reason("abcdefghij\0"), ValidationReason::InvalidCharacter);
        assert_eq!(reason("abcde\nfghij"), ValidationReason::InvalidCharacter);
    }

    #[test]
    fn spaces_are_significant_and_not_trimmed() {
        let pw = PlainPassword::parse_new("  spaced password  ", &login()).expect("valid");
        assert_eq!(pw.expose(), "  spaced password  ");
    }

    #[test]
    fn verification_input_only_checks_size() {
        assert!(PlainPassword::for_verification("short").is_ok());
        assert!(PlainPassword::for_verification("").is_err());
        assert!(PlainPassword::for_verification(&"x".repeat(MAX_CHARS * 4 + 1)).is_err());
    }

    #[test]
    fn debug_never_reveals_the_value() {
        let pw = PlainPassword::for_verification("super-secret-value").expect("valid");
        assert!(!format!("{pw:?}").contains("secret"));
    }
}
