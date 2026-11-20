use pango_core::{DomainError, DomainResult, ValidationReason};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// 오류 응답 본문: `{"error": {...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// 안정적인 오류 코드(snake_case). UI 문구는 이 값으로 고른다.
    pub code: String,
    /// 개발자용 설명. 사용자에게 그대로 보여 주지 않는다.
    pub message: String,
    /// 입력 오류일 때 문제가 된 필드.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// 입력 오류의 세부 사유(`too_long` 등).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// 목록 응답. `next_cursor`가 있으면 다음 페이지가 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

/// 목록 조회 공통 쿼리.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageQuery {
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl PageQuery {
    pub const DEFAULT_LIMIT: u32 = 50;
    pub const MAX_LIMIT: u32 = 200;

    /// 범위를 벗어난 값은 잘라서 쓴다(0 → 1, 너무 큰 값 → 최대).
    pub fn limit(&self) -> u32 {
        self.limit
            .unwrap_or(Self::DEFAULT_LIMIT)
            .clamp(1, Self::MAX_LIMIT)
    }
}

impl PageQuery {
    /// 커서를 정렬 키로 되돌린다. 커서가 없으면 첫 페이지(`None`).
    pub fn after<K: DeserializeOwned>(&self) -> DomainResult<Option<K>> {
        self.cursor.as_deref().map(decode_cursor).transpose()
    }
}

impl<T> Page<T> {
    /// 한 페이지를 만든다. 다음 페이지가 있으면 마지막 항목의 정렬 키를 커서로 싣는다.
    pub fn next<K: Serialize>(
        items: Vec<T>,
        has_more: bool,
        key: impl FnOnce(&T) -> K,
    ) -> Result<Self, serde_json::Error> {
        let next_cursor = match items.last() {
            Some(last) if has_more => Some(encode_cursor(&key(last))?),
            _ => None,
        };
        Ok(Self { items, next_cursor })
    }
}

/// 커서의 최대 길이. 커서는 서버가 만든 값이므로 이보다 길면 손상되었거나 위조된 것이다.
pub const MAX_CURSOR_LEN: usize = 512;

/// 정렬 키를 불투명한 커서 문자열(JSON의 소문자 16진수)로 만든다.
///
/// 클라이언트는 커서를 해석하지 않고 그대로 되돌려 보내기만 한다. 정렬 키가 여러 열이어도
/// (예: 고정 여부 + 게시 시각 + ID) 한 값으로 다룰 수 있게, 형식은 이 모듈에서만 정한다.
pub fn encode_cursor<K: Serialize>(key: &K) -> Result<String, serde_json::Error> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = serde_json::to_vec(key)?;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    Ok(out)
}

/// [`encode_cursor`]로 만든 커서만 받는다. 표기·길이·내용 중 하나라도 어긋나면
/// `cursor` 필드의 입력 오류다. 다른 목록의 커서를 넣어도 키 모양이 달라 거부된다.
pub fn decode_cursor<K: DeserializeOwned>(raw: &str) -> DomainResult<K> {
    const INVALID: DomainError = DomainError::validation("cursor", ValidationReason::InvalidFormat);
    if raw.is_empty() || raw.len() > MAX_CURSOR_LEN || raw.len() % 2 != 0 {
        return Err(INVALID);
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(INVALID),
    };
    let bytes = raw
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Ok(nibble(pair[0])? << 4 | nibble(pair[1])?))
        .collect::<DomainResult<Vec<u8>>>()?;
    serde_json::from_slice(&bytes).map_err(|_| INVALID)
}

/// 삭제·보관처럼 본문 없는 수정 요청의 버전(`?version=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionQuery {
    pub version: i64,
}

/// 수정 요청에 함께 보내는 낙관적 잠금 버전.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versioned<T> {
    pub version: i64,
    #[serde(flatten)]
    pub data: T,
}

/// 본문 없는 성공 응답 대신 쓰는 빈 객체.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Empty {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_is_clamped() {
        let q = |limit| PageQuery {
            limit,
            cursor: None,
        };
        assert_eq!(q(None).limit(), 50);
        assert_eq!(q(Some(0)).limit(), 1);
        assert_eq!(q(Some(200)).limit(), 200);
        assert_eq!(q(Some(u32::MAX)).limit(), 200);
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Key {
        pinned: bool,
        at: i64,
        id: String,
    }

    fn key() -> Key {
        Key {
            pinned: true,
            at: -5,
            id: "가-\"'<>".into(),
        }
    }

    fn invalid() -> DomainError {
        DomainError::validation("cursor", ValidationReason::InvalidFormat)
    }

    #[test]
    fn cursor_round_trips_and_is_url_safe() {
        let encoded = encode_cursor(&key()).expect("encode");
        assert!(encoded
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_eq!(decode_cursor::<Key>(&encoded), Ok(key()));
    }

    #[test]
    fn cursor_rejects_garbage() {
        let encoded = encode_cursor(&key()).expect("encode");
        let cases = [
            String::new(),
            "zz".into(),
            "abc".into(),
            encoded.to_uppercase(),
            format!("{encoded}0"),
            format!("{encoded}00"),
            encoded[2..].to_owned(),
            " ".repeat(4),
            "1' OR '1'='1".into(),
            "00".repeat(MAX_CURSOR_LEN / 2 + 1),
            encode_cursor(&42).expect("encode"),
            encode_cursor(&serde_json::json!({ "pinned": true, "at": 1, "id": "x", "extra": 1 }))
                .expect("encode"),
        ];
        for case in cases {
            assert_eq!(decode_cursor::<Key>(&case), Err(invalid()), "{case:?}");
        }
    }

    #[test]
    fn page_carries_cursor_only_when_more_exist() {
        let page = Page::next(vec![1, 2], true, |last| *last).expect("page");
        assert_eq!(
            decode_cursor::<i32>(page.next_cursor.as_deref().expect("cursor")),
            Ok(2)
        );
        assert_eq!(
            Page::next(vec![1, 2], false, |last| *last)
                .expect("page")
                .next_cursor,
            None
        );
        assert_eq!(
            Page::next(Vec::<i32>::new(), true, |last| *last)
                .expect("page")
                .next_cursor,
            None
        );
    }

    #[test]
    fn page_query_decodes_optional_cursor() {
        let none = PageQuery::default();
        assert_eq!(none.after::<i32>(), Ok(None));
        let some = PageQuery {
            limit: None,
            cursor: Some(encode_cursor(&7).expect("encode")),
        };
        assert_eq!(some.after::<i32>(), Ok(Some(7)));
        let bad = PageQuery {
            limit: None,
            cursor: Some("not-a-cursor".into()),
        };
        assert_eq!(bad.after::<i32>(), Err(invalid()));
    }
}
