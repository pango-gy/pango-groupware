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

impl<T> Page<T> {
    pub fn complete(items: Vec<T>) -> Self {
        Self {
            items,
            next_cursor: None,
        }
    }
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

/// 본문이 없는 변경 요청(`DELETE`)의 낙관적 잠금 버전: `?version=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionQuery {
    pub version: i64,
}

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
}
