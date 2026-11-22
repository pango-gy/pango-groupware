//! 첨부파일 규칙.

use crate::error::{DomainError, DomainResult, ValidationReason};
use crate::text::TextRule;

/// 공지·결재 문서 하나에 붙일 수 있는 첨부파일 수.
pub const MAX_PER_OWNER: usize = 10;

/// 첨부파일 이름. 내려받을 때 그대로 쓰는 이름이라 경로처럼 보이는 값은 받지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileName(String);

impl FileName {
    pub const RULE: TextRule = TextRule {
        field: "filename",
        min_chars: 1,
        max_chars: 200,
        multiline: false,
    };

    pub fn parse(raw: &str) -> DomainResult<Self> {
        let name = Self::RULE.normalize(raw)?;
        // 경로 구분자가 있으면 내려받는 쪽에서 다른 폴더를 가리킬 수 있다.
        if name.contains(['/', '\\']) || name.chars().all(|c| c == '.') {
            return Err(DomainError::validation(
                Self::RULE.field,
                ValidationReason::InvalidCharacter,
            ));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names_and_normalizes_them() {
        assert_eq!(
            FileName::parse("  2026 연말 일정.pdf ").map(FileName::into_string),
            Ok("2026 연말 일정.pdf".to_owned())
        );
        assert!(FileName::parse(".env").is_ok());
        assert!(FileName::parse(&"가".repeat(FileName::RULE.max_chars)).is_ok());
    }

    #[test]
    fn rejects_paths_dots_controls_and_empty() {
        for bad in [
            "",
            "   ",
            ".",
            "..",
            "../secret.txt",
            "a/b.txt",
            "C:\\boot.ini",
            "line\nbreak.txt",
            "evil\u{202E}fdp.exe",
        ] {
            assert!(FileName::parse(bad).is_err(), "{bad:?}");
        }
        assert!(FileName::parse(&"a".repeat(FileName::RULE.max_chars + 1)).is_err());
    }
}
