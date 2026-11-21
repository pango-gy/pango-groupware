//! 외부 알림 채널(아웃박스)의 순수 규칙: 웹훅 주소 검증, 재시도 간격, 응답 분류.
//!
//! 앱 내 알림이 만들어지면 같은 트랜잭션에서 보낼 일(아웃박스 행)을 남기고, 서버의 발송
//! 작업이 따로 보낸다. 보내기에 실패해도 앱 내 알림과 업무 트랜잭션에는 영향이 없다.

use chrono::Duration;
use url::{Host, Url};

use crate::error::{DomainError, DomainResult, ValidationReason};

/// 웹훅 주소의 최대 길이(바이트).
pub const MAX_WEBHOOK_URL_LEN: usize = 2048;

/// 한 메시지를 보내 보는 최대 횟수. 이만큼 실패하면 `failed`로 끝난다.
pub const MAX_ATTEMPTS: u32 = 8;

/// 첫 재시도까지의 간격. 시도마다 두 배로 늘어난다.
pub const RETRY_BASE: Duration = Duration::seconds(30);

/// 재시도 간격의 상한.
pub const RETRY_CAP: Duration = Duration::hours(1);

/// 검증한 웹훅 주소.
///
/// - `https`만 받는다. 개발용으로 이 컴퓨터(`localhost`, `127.0.0.1`, `[::1]`)만 `http`를 허용한다.
/// - 사용자 정보(`user:pass@`)와 조각(`#…`)은 받지 않는다. 경로·쿼리는 받는다(받는 쪽이 주소에
///   토큰을 넣는 경우가 많다).
/// - 공백·제어 문자가 섞이면 거부한다(주소 해석기가 조용히 지우는 것을 막는다).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookUrl(String);

impl WebhookUrl {
    pub const FIELD: &'static str = "webhook_url";

    pub fn parse(raw: &str) -> DomainResult<Self> {
        let invalid = DomainError::validation(Self::FIELD, ValidationReason::InvalidFormat);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(DomainError::validation(
                Self::FIELD,
                ValidationReason::Empty,
            ));
        }
        if trimmed.len() > MAX_WEBHOOK_URL_LEN {
            return Err(DomainError::validation(
                Self::FIELD,
                ValidationReason::TooLong {
                    max: MAX_WEBHOOK_URL_LEN,
                },
            ));
        }
        if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(DomainError::validation(
                Self::FIELD,
                ValidationReason::InvalidCharacter,
            ));
        }
        // `https:example.com`처럼 `//`가 빠진 표기도 해석기는 받아 주므로 직접 확인한다.
        let lower = trimmed.to_ascii_lowercase();
        let secure = lower.starts_with("https://");
        if !secure && !lower.starts_with("http://") {
            return Err(invalid);
        }
        let url = Url::parse(trimmed).map_err(|_| invalid.clone())?;
        if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
            return Err(invalid);
        }
        let host = url.host().ok_or_else(|| invalid.clone())?;
        if !secure && !is_loopback(&host) {
            return Err(invalid);
        }
        Ok(Self(url.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    /// 감사 로그에 남길 출처(`https://host[:port]`). 경로·쿼리에는 받는 쪽의 토큰이 들어
    /// 있을 수 있어 남기지 않는다.
    pub fn origin(&self) -> String {
        Url::parse(&self.0)
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_default()
    }
}

fn is_loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => ip.is_loopback() && ip.octets() == [127, 0, 0, 1],
        Host::Ipv6(ip) => ip.is_loopback(),
    }
}

/// `attempts`번째 시도가 실패한 뒤 다음 시도까지의 간격: 30초, 1분, 2분, 4분 … 최대 1시간.
pub fn backoff(attempts: u32) -> Duration {
    let exponent = attempts.saturating_sub(1);
    // 2^7 × 30초 = 64분이 이미 상한을 넘으므로 더 큰 지수는 계산하지 않는다.
    if exponent >= 7 {
        return RETRY_CAP;
    }
    (RETRY_BASE * (1 << exponent)).min(RETRY_CAP)
}

/// `attempts`번 시도한 메시지를 다시 보낼 간격. 최대 횟수에 이르렀으면 `None`(포기).
pub fn next_retry(attempts: u32) -> Option<Duration> {
    (attempts < MAX_ATTEMPTS).then(|| backoff(attempts))
}

/// 받는 쪽의 HTTP 응답 상태에 따른 처리.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseClass {
    /// 2xx: 전달됨.
    Delivered,
    /// 408·429·5xx: 일시적인 문제. 나중에 다시 보낸다.
    Retry,
    /// 그 밖(3xx·4xx 등): 주소나 설정이 틀렸다. 다시 보내도 같으므로 바로 실패로 끝낸다.
    Reject,
}

pub fn classify_status(status: u16) -> ResponseClass {
    match status {
        200..=299 => ResponseClass::Delivered,
        408 | 429 | 500..=599 => ResponseClass::Retry,
        _ => ResponseClass::Reject,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(raw: &str) -> Option<ValidationReason> {
        match WebhookUrl::parse(raw) {
            Ok(_) => None,
            Err(DomainError::Validation { field, reason }) => {
                assert_eq!(field, "webhook_url");
                Some(reason)
            }
            Err(other) => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn https_urls_are_accepted_with_path_and_query() {
        let url = WebhookUrl::parse("  https://hooks.example.com/pango/abc?token=x  ").expect("ok");
        assert_eq!(url.as_str(), "https://hooks.example.com/pango/abc?token=x");
        assert_eq!(url.origin(), "https://hooks.example.com");
        assert_eq!(
            WebhookUrl::parse("https://example.com:8443")
                .expect("port")
                .as_str(),
            "https://example.com:8443/"
        );
    }

    #[test]
    fn uppercase_scheme_and_host_are_normalized() {
        let url = WebhookUrl::parse("HTTPS://Hooks.Example.COM/In").expect("ok");
        assert_eq!(url.as_str(), "https://hooks.example.com/In");
        assert!(WebhookUrl::parse("HTTP://LOCALHOST:9000/hook").is_ok());
    }

    #[test]
    fn plain_http_is_only_for_this_computer() {
        for ok in [
            "http://localhost:9000/hook",
            "http://127.0.0.1/hook",
            "http://[::1]:8080/hook",
        ] {
            assert_eq!(reason(ok), None, "{ok}");
        }
        for bad in [
            "http://example.com/hook",
            "http://10.0.0.5/hook",
            "http://127.0.0.2/hook",
            "http://localhost.example.com/hook",
            "http://[::2]/hook",
        ] {
            assert_eq!(reason(bad), Some(ValidationReason::InvalidFormat), "{bad}");
        }
    }

    #[test]
    fn userinfo_fragment_and_other_schemes_are_rejected() {
        for bad in [
            "https://user:pass@example.com/hook",
            "https://user@example.com/hook",
            "https://example.com/hook#frag",
            "ftp://example.com/hook",
            "javascript:alert(1)",
            "https:example.com",
            "https://",
            "example.com/hook",
            "not a url",
        ] {
            assert!(reason(bad).is_some(), "{bad}");
        }
        assert_eq!(
            reason("https://exa\nmple.com"),
            Some(ValidationReason::InvalidCharacter)
        );
        assert_eq!(
            reason("https://example.com/a b"),
            Some(ValidationReason::InvalidCharacter)
        );
    }

    #[test]
    fn empty_and_too_long_urls_are_rejected() {
        assert_eq!(reason(""), Some(ValidationReason::Empty));
        assert_eq!(reason("   "), Some(ValidationReason::Empty));
        let prefix = "https://example.com/";
        let exact = format!("{prefix}{}", "a".repeat(MAX_WEBHOOK_URL_LEN - prefix.len()));
        assert_eq!(reason(&exact), None);
        let over = format!("{exact}a");
        assert_eq!(
            reason(&over),
            Some(ValidationReason::TooLong {
                max: MAX_WEBHOOK_URL_LEN
            })
        );
    }

    #[test]
    fn backoff_doubles_from_thirty_seconds_and_is_capped() {
        let secs = |n: u32| backoff(n).num_seconds();
        assert_eq!(secs(0), 30);
        assert_eq!(secs(1), 30);
        assert_eq!(secs(2), 60);
        assert_eq!(secs(3), 120);
        assert_eq!(secs(4), 240);
        assert_eq!(secs(7), 30 * 64);
        assert_eq!(backoff(8), RETRY_CAP);
        assert_eq!(backoff(u32::MAX), RETRY_CAP);
    }

    #[test]
    fn retries_stop_at_the_maximum_attempts() {
        assert_eq!(next_retry(1), Some(Duration::seconds(30)));
        assert_eq!(
            next_retry(MAX_ATTEMPTS - 1),
            Some(backoff(MAX_ATTEMPTS - 1))
        );
        assert_eq!(next_retry(MAX_ATTEMPTS), None);
        assert_eq!(next_retry(MAX_ATTEMPTS + 1), None);
        // 모든 재시도를 합쳐도 두 시간을 넘기지 않는다.
        let total: i64 = (1..MAX_ATTEMPTS)
            .filter_map(next_retry)
            .map(|d| d.num_seconds())
            .sum();
        assert!(total < 2 * 3600, "{total}");
    }

    #[test]
    fn response_statuses_are_classified() {
        assert_eq!(classify_status(200), ResponseClass::Delivered);
        assert_eq!(classify_status(204), ResponseClass::Delivered);
        assert_eq!(classify_status(299), ResponseClass::Delivered);
        for retry in [408, 429, 500, 502, 503, 599] {
            assert_eq!(classify_status(retry), ResponseClass::Retry, "{retry}");
        }
        for reject in [100, 301, 302, 400, 401, 403, 404, 410, 422, 600] {
            assert_eq!(classify_status(reject), ResponseClass::Reject, "{reject}");
        }
    }
}
