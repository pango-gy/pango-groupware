//! 사내 메신저 Buzz 채널의 순수 규칙: 릴레이 주소, 계정 연결 확인 코드의 제한, DM 문구.

use chrono::Duration;

use crate::error::{DomainError, DomainResult, ValidationReason};
use crate::outbound::WebhookUrl;
use crate::time::Timestamp;

/// 확인 코드의 유효 시간.
pub const CODE_TTL: Duration = Duration::minutes(10);
/// 한 코드로 틀릴 수 있는 횟수. 넘으면 그 코드는 쓸 수 없다(새로 받아야 한다).
pub const MAX_CODE_ATTEMPTS: u32 = 5;
/// 코드를 다시 보내기까지 기다리는 시간.
pub const RESEND_COOLDOWN: Duration = Duration::seconds(60);
/// 한 시간에 보낼 수 있는 코드 수.
pub const SENDS_PER_HOUR: u32 = 5;
const SEND_WINDOW: Duration = Duration::hours(1);
/// DM 본문의 최대 길이(바이트). 릴레이의 한도(64KiB)보다 넉넉히 작다.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024;

/// 릴레이 주소를 출처(`https://host[:port]`)로 맞춘다. `wss://`·`ws://`로 적어도 받는다(같은
/// 호스트의 HTTP API를 쓴다). 웹훅 주소와 같은 규칙(https, 개발용 루프백만 http)이고, 경로·쿼리는
/// 받지 않는다.
pub fn relay_origin(raw: &str) -> DomainResult<String> {
    const FIELD: &str = "buzz_relay_url";
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    let rewritten = if lower.starts_with("wss://") {
        format!("https://{}", &trimmed[6..])
    } else if lower.starts_with("ws://") {
        format!("http://{}", &trimmed[5..])
    } else {
        trimmed.to_owned()
    };
    let url = WebhookUrl::parse(&rewritten).map_err(|error| error.with_field(FIELD))?;
    let origin = url.origin();
    let rest = &url.as_str()[origin.len()..];
    if rest != "/" && !rest.is_empty() {
        return Err(DomainError::validation(
            FIELD,
            ValidationReason::InvalidFormat,
        ));
    }
    Ok(origin)
}

/// 코드 발송 기록(시간당 상한을 세는 창).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendWindow {
    pub start: Timestamp,
    pub count: u32,
}

/// 지금 코드를 보낼 수 있으면 발송 뒤의 창을, 아니면 기다려야 할 시간을 돌려준다.
pub fn next_send(
    last_sent: Option<Timestamp>,
    window: Option<SendWindow>,
    now: Timestamp,
) -> Result<SendWindow, Duration> {
    if let Some(last) = last_sent {
        let since = now.since(last);
        if since < RESEND_COOLDOWN && since >= Duration::zero() {
            return Err(RESEND_COOLDOWN - since);
        }
    }
    match window {
        Some(window) if now.since(window.start) < SEND_WINDOW => {
            if window.count >= SENDS_PER_HOUR {
                Err(SEND_WINDOW - now.since(window.start))
            } else {
                Ok(SendWindow {
                    count: window.count + 1,
                    ..window
                })
            }
        }
        _ => Ok(SendWindow {
            start: now,
            count: 1,
        }),
    }
}

/// 코드를 받을 수 없는 이유.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeProblem {
    /// 받은 코드가 없다(보낸 적 없거나 이미 확인함).
    Missing,
    /// 유효 시간이 지났다(만료 시각 **포함**).
    Expired,
    /// 틀린 횟수를 다 썼다.
    Exhausted,
}

/// 입력한 코드를 비교해도 되는지.
pub fn code_usable(
    expires_at: Option<Timestamp>,
    attempts: u32,
    now: Timestamp,
) -> Result<(), CodeProblem> {
    let expires_at = expires_at.ok_or(CodeProblem::Missing)?;
    if now >= expires_at {
        return Err(CodeProblem::Expired);
    }
    if attempts >= MAX_CODE_ATTEMPTS {
        return Err(CodeProblem::Exhausted);
    }
    Ok(())
}

/// 입력한 확인 코드를 정리한다: 공백·하이픈을 빼고 숫자 6자리만 받는다.
pub fn normalize_code(raw: &str) -> DomainResult<String> {
    let digits: String = raw
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    if digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_digit()) {
        Ok(digits)
    } else {
        Err(DomainError::validation(
            "buzz_code",
            ValidationReason::InvalidFormat,
        ))
    }
}

/// Buzz DM 본문: 제목, 짧은 본문, 링크를 줄마다. 빈 부분은 뺀다. 릴레이에 평문으로 남으므로
/// 앱 알림에 이미 있는 값만 넣는다. 너무 길면 글자 단위로 자른다.
pub fn message_text(title: &str, body: &str, link: Option<&str>) -> String {
    let parts = [title.trim(), body.trim(), link.unwrap_or_default().trim()];
    let mut text = parts
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    if text.len() > MAX_MESSAGE_BYTES {
        let mut end = MAX_MESSAGE_BYTES - '…'.len_utf8();
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_millis(1_767_225_600_000 + seconds * 1000).expect("ts")
    }

    #[test]
    fn relay_urls_become_origins() {
        assert_eq!(
            relay_origin("wss://buzz.example.com").as_deref(),
            Ok("https://buzz.example.com")
        );
        assert_eq!(
            relay_origin("HTTPS://Buzz.Example.com:8443/").as_deref(),
            Ok("https://buzz.example.com:8443")
        );
        assert_eq!(
            relay_origin("ws://localhost:3000").as_deref(),
            Ok("http://localhost:3000")
        );
        for bad in [
            "ws://buzz.example.com",
            "https://buzz.example.com/events",
            "https://buzz.example.com/?x=1",
            "https://user@buzz.example.com",
            "",
        ] {
            assert!(relay_origin(bad).is_err(), "{bad}");
        }
        assert!(matches!(
            relay_origin("ftp://x"),
            Err(DomainError::Validation {
                field: "buzz_relay_url",
                ..
            })
        ));
    }

    #[test]
    fn codes_can_be_resent_after_a_minute_up_to_five_an_hour() {
        let first = next_send(None, None, at(0)).expect("first");
        assert_eq!(
            first,
            SendWindow {
                start: at(0),
                count: 1
            }
        );
        // 경계: 59초는 기다리고 60초는 보낸다.
        assert_eq!(
            next_send(Some(at(0)), Some(first), at(59)),
            Err(Duration::seconds(1))
        );
        let mut window = first;
        for n in 1..SENDS_PER_HOUR {
            window = next_send(
                Some(at(i64::from(n - 1) * 60)),
                Some(window),
                at(i64::from(n) * 60),
            )
            .expect("within cap");
        }
        assert_eq!(window.count, SENDS_PER_HOUR);
        // 여섯 번째는 창이 끝날 때까지 기다린다.
        assert_eq!(
            next_send(Some(at(240)), Some(window), at(600)),
            Err(Duration::seconds(3000))
        );
        // 창이 끝나면 새로 센다.
        assert_eq!(
            next_send(Some(at(240)), Some(window), at(3600)),
            Ok(SendWindow {
                start: at(3600),
                count: 1
            })
        );
    }

    #[test]
    fn codes_expire_at_ten_minutes_and_after_five_wrong_tries() {
        let expires = at(600);
        assert_eq!(code_usable(Some(expires), 0, at(599)), Ok(()));
        assert_eq!(
            code_usable(Some(expires), 0, at(600)),
            Err(CodeProblem::Expired)
        );
        assert_eq!(code_usable(Some(expires), 4, at(0)), Ok(()));
        assert_eq!(
            code_usable(Some(expires), 5, at(0)),
            Err(CodeProblem::Exhausted)
        );
        assert_eq!(code_usable(None, 0, at(0)), Err(CodeProblem::Missing));
    }

    #[test]
    fn codes_are_six_digits() {
        assert_eq!(normalize_code(" 123 456 ").as_deref(), Ok("123456"));
        assert_eq!(normalize_code("123-456").as_deref(), Ok("123456"));
        for bad in ["12345", "1234567", "12a456", "", "١٢٣٤٥٦"] {
            assert!(normalize_code(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn messages_have_one_part_per_line_and_stay_short() {
        assert_eq!(
            message_text(
                "결재 요청: 출장비 정산",
                "김팡고 님이 결재를 요청했어요.",
                Some("https://gw.example.com/approval/documents/1")
            ),
            "결재 요청: 출장비 정산\n김팡고 님이 결재를 요청했어요.\nhttps://gw.example.com/approval/documents/1"
        );
        assert_eq!(message_text("제목", "  ", None), "제목");
        let long = message_text(&"가".repeat(10_000), "", None);
        assert!(long.len() <= MAX_MESSAGE_BYTES);
        assert!(long.ends_with('…'));
    }
}
