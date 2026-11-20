//! 검증된 텍스트 값 타입.
//!
//! 사용자 입력 문자열은 이 모듈의 타입으로 바꾼 뒤에만 도메인에 들어온다.
//! 길이 제한, 제어문자·방향 제어 문자 거부, 유니코드 정규화(NFC)를 한 곳에서 처리한다.

use unicode_normalization::UnicodeNormalization;

use crate::error::{DomainError, DomainResult, ValidationReason};

/// 눈에 보이지 않거나 표시 방향을 바꿔 다른 글자로 위장하는 데 쓰이는 문자.
fn is_deceptive(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'   // zero-width, LRM/RLM
            | '\u{202A}'..='\u{202E}' // bidi embedding/override
            | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
            | '\u{2066}'..='\u{2069}' // bidi isolates
            | '\u{FEFF}'              // BOM / zero-width no-break space
            | '\u{FFF9}'..='\u{FFFB}' // interlinear annotation
            | '\u{E0000}'..='\u{E007F}' // tag characters
    )
}

#[derive(Debug, Clone, Copy)]
pub struct TextRule {
    pub field: &'static str,
    pub min_chars: usize,
    pub max_chars: usize,
    /// 줄바꿈과 탭을 허용할지.
    pub multiline: bool,
}

impl TextRule {
    /// 입력을 정규화하고 규칙을 확인한다.
    ///
    /// 정규화 전 길이를 먼저 제한해, 거대한 입력을 정규화하느라 시간을 쓰지 않는다.
    pub fn normalize(&self, raw: &str) -> DomainResult<String> {
        let err = |reason| DomainError::validation(self.field, reason);
        // 정규화로 길이가 줄 수 있으므로 넉넉한 사전 상한(문자당 최대 4바이트 × 여유 3배).
        if raw.len() > self.max_chars.saturating_mul(12).max(64) {
            return Err(err(ValidationReason::TooLong { max: self.max_chars }));
        }
        let unified = raw.replace("\r\n", "\n").replace('\r', "\n");
        let trimmed = unified.trim();
        for c in trimmed.chars() {
            let allowed_whitespace = self.multiline && (c == '\n' || c == '\t');
            if (c.is_control() && !allowed_whitespace) || is_deceptive(c) {
                return Err(err(ValidationReason::InvalidCharacter));
            }
        }
        let normalized: String = trimmed.nfc().collect();
        let chars = normalized.chars().count();
        if chars == 0 && self.min_chars > 0 {
            return Err(err(ValidationReason::Empty));
        }
        if chars < self.min_chars {
            return Err(err(ValidationReason::TooShort { min: self.min_chars }));
        }
        if chars > self.max_chars {
            return Err(err(ValidationReason::TooLong { max: self.max_chars }));
        }
        Ok(normalized)
    }
}

/// 규칙이 붙은 문자열 newtype을 만든다.
macro_rules! text_type {
    ($(#[$meta:meta])* $name:ident, $rule:expr) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub const RULE: TextRule = $rule;

            pub fn parse(raw: &str) -> DomainResult<Self> {
                Self::RULE.normalize(raw).map(Self)
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }
    };
}

const fn line(field: &'static str, min_chars: usize, max_chars: usize) -> TextRule {
    TextRule { field, min_chars, max_chars, multiline: false }
}

const fn block(field: &'static str, min_chars: usize, max_chars: usize) -> TextRule {
    TextRule { field, min_chars, max_chars, multiline: true }
}

text_type!(
    /// 구성원 이름.
    PersonName, line("name", 1, 50)
);
text_type!(
    /// 부서·직책·양식·휴가 종류 등 관리 대상의 이름.
    LabelName, line("name", 1, 50)
);
text_type!(
    /// 문서·공지 제목.
    Title, line("title", 1, 200)
);
text_type!(
    /// 문서·공지 본문.
    Body, block("body", 1, 20_000)
);
text_type!(
    /// 결재 의견, 수정 사유 등 짧은 설명. 비어 있을 수 있다.
    Comment, block("comment", 0, 1_000)
);
text_type!(
    /// 반드시 적어야 하는 사유.
    Reason, block("reason", 1, 1_000)
);

/// 로그인 아이디. 소문자 영문·숫자와 `.`, `_`, `-`만 쓰며 영문·숫자로 시작한다.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LoginId(String);

impl LoginId {
    pub const MIN_LEN: usize = 3;
    pub const MAX_LEN: usize = 32;

    /// 대문자는 소문자로 바꿔 받는다. 그 외의 정규화는 하지 않는다.
    pub fn parse(raw: &str) -> DomainResult<Self> {
        let err = |reason| DomainError::validation("login_id", reason);
        if raw.len() > Self::MAX_LEN {
            return Err(err(ValidationReason::TooLong { max: Self::MAX_LEN }));
        }
        if raw.is_empty() {
            return Err(err(ValidationReason::Empty));
        }
        if raw.len() < Self::MIN_LEN {
            return Err(err(ValidationReason::TooShort { min: Self::MIN_LEN }));
        }
        let lowered = raw.to_ascii_lowercase();
        let valid_char = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b);
        if !lowered.bytes().all(valid_char) {
            return Err(err(ValidationReason::InvalidCharacter));
        }
        if !lowered.as_bytes()[0].is_ascii_alphanumeric() {
            return Err(err(ValidationReason::InvalidFormat));
        }
        Ok(Self(lowered))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for LoginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 이메일 주소. 발송 가능 여부가 아니라 형태만 확인한다.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Email(String);

impl Email {
    pub const MAX_LEN: usize = 254;

    pub fn parse(raw: &str) -> DomainResult<Self> {
        let err = |reason| DomainError::validation("email", reason);
        let value = raw.trim();
        if value.is_empty() {
            return Err(err(ValidationReason::Empty));
        }
        if value.len() > Self::MAX_LEN {
            return Err(err(ValidationReason::TooLong { max: Self::MAX_LEN }));
        }
        let Some((local, domain)) = value.rsplit_once('@') else {
            return Err(err(ValidationReason::InvalidFormat));
        };
        let local_ok = !local.is_empty()
            && local.len() <= 64
            && !local.starts_with('.')
            && !local.ends_with('.')
            && !local.contains("..")
            && local.bytes().all(|b| b.is_ascii_alphanumeric() || b"._%+-".contains(&b));
        let label_ok = |label: &str| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        };
        let domain_ok = domain.contains('.') && domain.split('.').all(label_ok);
        if !(local_ok && domain_ok) {
            return Err(err(ValidationReason::InvalidFormat));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 비어 있으면 `None`, 아니면 파서를 적용한다. 선택 입력 필드에 쓴다.
pub fn optional<T>(
    raw: Option<&str>,
    parse: impl FnOnce(&str) -> DomainResult<T>,
) -> DomainResult<Option<T>> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(value) => parse(value).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn reason<T: std::fmt::Debug>(result: DomainResult<T>) -> ValidationReason {
        match result {
            Err(DomainError::Validation { reason, .. }) => reason,
            other => panic!("expected validation error, got {other:?}"),
        }
    }

    #[test]
    fn trims_and_keeps_inner_spaces() {
        assert_eq!(PersonName::parse("  홍 길동\t").expect("valid").as_str(), "홍 길동");
    }

    #[test]
    fn empty_and_whitespace_only_are_empty() {
        for raw in ["", "   ", "\n\t ", "\u{3000}"] {
            assert_eq!(reason(Title::parse(raw)), ValidationReason::Empty, "{raw:?}");
        }
        assert_eq!(Comment::parse("   ").expect("comment may be empty").as_str(), "");
    }

    #[test]
    fn length_is_counted_in_characters_not_bytes() {
        let exactly_max = "가".repeat(50);
        assert!(PersonName::parse(&exactly_max).is_ok());
        let over = "가".repeat(51);
        assert_eq!(reason(PersonName::parse(&over)), ValidationReason::TooLong { max: 50 });
    }

    #[test]
    fn decomposed_hangul_is_normalized_to_nfc() {
        let nfd = "\u{1112}\u{1169}\u{11BC}"; // 홍 (자모 분리)
        let parsed = PersonName::parse(nfd).expect("valid");
        assert_eq!(parsed.as_str(), "홍");
        assert_eq!(parsed.as_str().chars().count(), 1);
    }

    #[test]
    fn decomposed_input_at_limit_is_accepted_after_normalization() {
        let nfd = "\u{1112}\u{1169}\u{11BC}".repeat(50);
        assert_eq!(PersonName::parse(&nfd).expect("valid").as_str().chars().count(), 50);
    }

    #[test]
    fn control_characters_are_rejected() {
        for raw in ["a\0b", "a\u{7}b", "a\u{1b}[31mb", "a\u{7f}b", "a\u{85}b"] {
            assert_eq!(reason(Title::parse(raw)), ValidationReason::InvalidCharacter, "{raw:?}");
        }
    }

    #[test]
    fn single_line_rejects_newline_but_multiline_accepts() {
        assert_eq!(reason(Title::parse("a\nb")), ValidationReason::InvalidCharacter);
        assert_eq!(Body::parse("a\r\nb\rc\td").expect("valid").as_str(), "a\nb\nc\td");
    }

    #[test]
    fn deceptive_characters_are_rejected() {
        for raw in [
            "admin\u{202E}txt.exe",
            "a\u{200B}b",
            "a\u{FEFF}b",
            "a\u{2066}b\u{2069}",
            "a\u{E0041}b",
        ] {
            assert_eq!(reason(Title::parse(raw)), ValidationReason::InvalidCharacter, "{raw:?}");
        }
    }

    #[test]
    fn markup_and_sql_fragments_are_plain_text() {
        // 저장은 그대로 하고, 출력 단계에서 이스케이프한다.
        for raw in ["<script>alert(1)</script>", "'; DROP TABLE members;--", "{{7*7}}", "${jndi:x}"] {
            assert_eq!(Title::parse(raw).expect("valid").as_str(), raw);
        }
    }

    #[test]
    fn huge_input_is_rejected_before_normalization() {
        let huge = "a".repeat(5_000_000);
        assert_eq!(reason(PersonName::parse(&huge)), ValidationReason::TooLong { max: 50 });
    }

    #[test]
    fn emoji_sequences_count_per_scalar() {
        // 가족 이모지는 ZWJ(U+200D)를 포함하므로 위장 문자로 거부된다. 단일 이모지는 허용.
        assert!(Title::parse("회식 🎉").is_ok());
        assert_eq!(reason(Title::parse("👨\u{200D}👩")), ValidationReason::InvalidCharacter);
    }

    #[test]
    fn login_id_rules() {
        assert_eq!(LoginId::parse("Hong.GD-1").expect("valid").as_str(), "hong.gd-1");
        assert_eq!(reason(LoginId::parse("")), ValidationReason::Empty);
        assert_eq!(reason(LoginId::parse("ab")), ValidationReason::TooShort { min: 3 });
        assert_eq!(reason(LoginId::parse(&"a".repeat(33))), ValidationReason::TooLong { max: 32 });
        assert!(LoginId::parse(&"a".repeat(32)).is_ok());
        for bad in ["홍길동", "a b c", "a@b.c", "abc\n", "abc\0", "ａｂｃ", "a/b", "a%00"] {
            assert_eq!(reason(LoginId::parse(bad)), ValidationReason::InvalidCharacter, "{bad:?}");
        }
        for bad in [".abc", "-abc", "_abc"] {
            assert_eq!(reason(LoginId::parse(bad)), ValidationReason::InvalidFormat, "{bad:?}");
        }
    }

    #[test]
    fn email_rules() {
        assert_eq!(Email::parse(" Hong@Example.CO.kr ").expect("valid").as_str(), "hong@example.co.kr");
        assert!(Email::parse("a.b+tag@sub.example.com").is_ok());
        for bad in [
            "plain", "@example.com", "a@", "a@b", "a@.com", "a@b..com", "a@-b.com", "a@b-.com",
            ".a@b.com", "a.@b.com", "a..b@b.com", "a b@b.com", "a@b.com\n.evil", "a@b.com,c@d.com",
            "a<script>@b.com", "\"a\"@b.com",
        ] {
            assert_eq!(reason(Email::parse(bad)), ValidationReason::InvalidFormat, "{bad:?}");
        }
        let long_local = format!("{}@b.com", "a".repeat(65));
        assert_eq!(reason(Email::parse(&long_local)), ValidationReason::InvalidFormat);
        let too_long = format!("a@{}.com", "b".repeat(63).repeat(5));
        assert!(Email::parse(&too_long).is_err());
    }

    #[test]
    fn optional_treats_blank_as_none() {
        assert_eq!(optional(None, Email::parse), Ok(None));
        assert_eq!(optional(Some("  "), Email::parse), Ok(None));
        assert!(optional(Some("a@b.com"), Email::parse).expect("valid").is_some());
        assert!(optional(Some("nope"), Email::parse).is_err());
    }

    proptest! {
        #[test]
        fn normalize_never_panics_and_output_obeys_rule(raw in any::<String>()) {
            if let Ok(body) = Body::parse(&raw) {
                let chars = body.as_str().chars().count();
                prop_assert!((1..=20_000).contains(&chars));
                prop_assert!(!body.as_str().chars().any(|c| c.is_control() && c != '\n' && c != '\t'));
                // 이미 정규화된 값은 다시 넣어도 같다(멱등).
                let reparsed = Body::parse(body.as_str());
                prop_assert_eq!(reparsed.as_ref().map(Body::as_str), Ok(body.as_str()));
            }
        }

        #[test]
        fn login_id_parse_is_idempotent(raw in "[A-Za-z0-9._-]{0,40}") {
            if let Ok(id) = LoginId::parse(&raw) {
                prop_assert_eq!(LoginId::parse(id.as_str()), Ok(id));
            }
        }
    }
}
