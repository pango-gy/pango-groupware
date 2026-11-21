//! 표시용 변환(시각, 날짜, 시간 길이). 순수 함수만 둔다.

use chrono::{Datelike, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use pango_core::time::Timestamp;

/// 회사 시간대. 이름이 잘못됐으면 UTC로 표시한다(서버가 검증한 값이라 실제로는 일어나지 않는다).
pub fn zone(name: &str) -> Tz {
    name.parse().unwrap_or(Tz::UTC)
}

/// 로그인한 구성원의 회사 시간대. 아직 모르면(로그인 전) UTC.
pub fn member_zone(me: Option<&pango_api::auth::Me>) -> Tz {
    me.map_or(Tz::UTC, |me| zone(&me.timezone))
}

/// 폼 입력값: 앞뒤 공백을 지우고, 비었으면 `None`(선택 입력을 "보내지 않음"으로).
pub fn non_empty(raw: &str) -> Option<&str> {
    Some(raw.trim()).filter(|value| !value.is_empty())
}

/// 선택 날짜 입력(`YYYY-MM-DD`). 비었으면 `None`, 형식이 틀리면 `field`의 형식 오류다.
pub fn optional_date(
    field: &'static str,
    raw: &str,
) -> pango_core::DomainResult<Option<NaiveDate>> {
    non_empty(raw)
        .map(|value| pango_core::calendar::parse_date(field, value))
        .transpose()
}

/// `09:05`
pub fn time_hm(at: Timestamp, tz: Tz) -> String {
    let local = at.to_utc().with_timezone(&tz);
    format!("{:02}:{:02}", local.hour(), local.minute())
}

/// `3월 2일 (월)`
pub fn date_md(date: NaiveDate) -> String {
    format!(
        "{}월 {}일 ({})",
        date.month(),
        date.day(),
        weekday_short(date.weekday())
    )
}

/// `2026. 3. 2.`
pub fn date_ymd(date: NaiveDate) -> String {
    format!("{}. {}. {}.", date.year(), date.month(), date.day())
}

/// `2026. 3. 2. 09:05`
pub fn datetime(at: Timestamp, tz: Tz) -> String {
    let local = at.to_utc().with_timezone(&tz);
    format!("{} {}", date_ymd(local.date_naive()), time_hm(at, tz))
}

/// 목록에 쓰는 짧은 시각: 오늘이면 `09:05`, 올해면 `3월 2일`, 그 외 `2025. 3. 2.`
pub fn short_when(at: Timestamp, now: Timestamp, tz: Tz) -> String {
    let date = at.to_utc().with_timezone(&tz).date_naive();
    let today = now.to_utc().with_timezone(&tz).date_naive();
    if date == today {
        time_hm(at, tz)
    } else if date.year() == today.year() {
        format!("{}월 {}일", date.month(), date.day())
    } else {
        date_ymd(date)
    }
}

pub fn weekday_short(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "월",
        Weekday::Tue => "화",
        Weekday::Wed => "수",
        Weekday::Thu => "목",
        Weekday::Fri => "금",
        Weekday::Sat => "토",
        Weekday::Sun => "일",
    }
}

/// 분을 `8시간 30분`, `45분`, `0분`으로.
pub fn duration_minutes(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}분"),
        (h, 0) => format!("{h}시간"),
        (h, m) => format!("{h}시간 {m}분"),
    }
}

/// 휴가 수량(1/4일 단위)을 `1.5일`처럼.
pub fn leave_days(quarters: i32) -> String {
    format!(
        "{}일",
        pango_core::leave::LeaveAmount::from_quarters(quarters)
    )
}

/// 회사 시간대의 달력 날짜.
pub fn local_date(at: Timestamp, tz: Tz) -> NaiveDate {
    at.to_utc().with_timezone(&tz).date_naive()
}

/// `since`부터 `now`까지 지난 분. 시계가 어긋나 음수가 되면 0.
pub fn elapsed_minutes(since: Timestamp, now: Timestamp) -> u32 {
    u32::try_from(now.since(since).num_minutes().max(0)).unwrap_or(u32::MAX)
}

/// `<input type="datetime-local">` 값(`YYYY-MM-DDTHH:MM`, 회사 시간대 벽시계).
pub fn local_input(at: Timestamp, tz: Tz) -> String {
    at.to_utc()
        .with_timezone(&tz)
        .format("%Y-%m-%dT%H:%M")
        .to_string()
}

/// `datetime-local` 값을 해석하지 못한 이유.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalInputError {
    Empty,
    Invalid,
    /// 서머타임 전환으로 회사 시간대에 존재하지 않는 시각.
    Nonexistent,
}

/// [`local_input`]의 역. 회사 시간대의 벽시계 시각으로 해석한다.
///
/// 서머타임으로 두 번 오는 시각은 먼저 오는 쪽(서버 `instant_at`과 같은 규칙)을 고른다.
/// 존재하지 않는 시각은 서버처럼 뒤로 밀지 않고 거부한다. 근태 기록은 감사 대상이라
/// 입력한 값과 다른 시각이 조용히 저장되면 안 된다.
pub fn parse_local_input(raw: &str, tz: Tz) -> Result<Timestamp, LocalInputError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(LocalInputError::Empty);
    }
    let bytes = raw.as_bytes();
    let shape_ok = bytes.len() == 16
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 => *byte == b':',
            _ => byte.is_ascii_digit(),
        });
    if !shape_ok {
        return Err(LocalInputError::Invalid);
    }
    let naive = NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M")
        .map_err(|_| LocalInputError::Invalid)?;
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(at) | LocalResult::Ambiguous(at, _) => {
            Ok(Timestamp::from_utc(at.with_timezone(&Utc)))
        }
        LocalResult::None => Err(LocalInputError::Nonexistent),
    }
}

/// 파일 크기(`512 B`, `1.5 KB`, `12.3 MB`). 1 KB = 1024 B.
pub fn file_size(bytes: u64) -> String {
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// 이름의 첫 글자(아바타용).
pub fn initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_size_picks_a_readable_unit() {
        assert_eq!(file_size(0), "0 B");
        assert_eq!(file_size(1023), "1023 B");
        assert_eq!(file_size(1536), "1.5 KB");
        assert_eq!(file_size(10 * 1024 * 1024), "10.0 MB");
        assert_eq!(file_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    fn ts(rfc3339: &str) -> Timestamp {
        Timestamp::from_utc(
            chrono::DateTime::parse_from_rfc3339(rfc3339)
                .expect("rfc3339")
                .with_timezone(&chrono::Utc),
        )
    }

    #[test]
    fn blank_inputs_become_none() {
        assert_eq!(non_empty(""), None);
        assert_eq!(non_empty("  \t"), None);
        assert_eq!(non_empty(" a b "), Some("a b"));
        assert_eq!(optional_date("hired_on", " "), Ok(None));
        assert_eq!(
            optional_date("hired_on", " 2026-03-02 "),
            Ok(Some(d("2026-03-02")))
        );
        assert_eq!(
            optional_date("hired_on", "2026-3-2"),
            Err(pango_core::DomainError::validation(
                "hired_on",
                pango_core::error::ValidationReason::InvalidFormat
            ))
        );
    }

    #[test]
    fn member_zone_falls_back_to_utc() {
        use pango_api::auth::Me;
        use pango_core::authz::{Capabilities, Role};
        assert_eq!(member_zone(None), Tz::UTC);
        let me = Me {
            id: pango_core::id::MemberId::new(),
            login_id: "me".into(),
            name: "나".into(),
            email: None,
            phone: None,
            role: Role::Member,
            department_id: None,
            department_name: None,
            position_id: None,
            position_name: None,
            capabilities: Capabilities {
                admin: false,
                manage_roles: false,
                team: false,
            },
            must_change_password: false,
            company_name: "회사".into(),
            timezone: "Asia/Seoul".into(),
        };
        assert_eq!(member_zone(Some(&me)), chrono_tz::Asia::Seoul);
        let broken = Me {
            timezone: "Mars/Olympus".into(),
            ..me
        };
        assert_eq!(member_zone(Some(&broken)), Tz::UTC);
    }

    #[test]
    fn times_are_shown_in_company_zone() {
        let seoul = zone("Asia/Seoul");
        assert_eq!(time_hm(ts("2026-03-02T00:05:00Z"), seoul), "09:05");
        assert_eq!(
            datetime(ts("2026-03-01T15:00:00Z"), seoul),
            "2026. 3. 2. 00:00"
        );
        assert_eq!(zone("Not/AZone"), Tz::UTC);
    }

    #[test]
    fn short_when_depends_on_distance() {
        let seoul = zone("Asia/Seoul");
        let now = ts("2026-03-02T03:00:00Z");
        assert_eq!(short_when(ts("2026-03-02T00:05:00Z"), now, seoul), "09:05");
        assert_eq!(
            short_when(ts("2026-03-01T14:59:00Z"), now, seoul),
            "3월 1일",
            "KST로 전날 23:59"
        );
        assert_eq!(
            short_when(ts("2025-12-31T14:59:00Z"), now, seoul),
            "2025. 12. 31."
        );
    }

    #[test]
    fn durations_and_dates() {
        assert_eq!(duration_minutes(0), "0분");
        assert_eq!(duration_minutes(45), "45분");
        assert_eq!(duration_minutes(480), "8시간");
        assert_eq!(duration_minutes(510), "8시간 30분");
        assert_eq!(date_md("2026-03-02".parse().expect("date")), "3월 2일 (월)");
        assert_eq!(leave_days(6), "1.5일");
        assert_eq!(leave_days(-1), "-0.25일");
    }

    #[test]
    fn local_input_round_trips_in_company_zone() {
        let seoul = zone("Asia/Seoul");
        let at = ts("2026-03-02T00:05:00Z");
        assert_eq!(local_input(at, seoul), "2026-03-02T09:05");
        assert_eq!(parse_local_input("2026-03-02T09:05", seoul), Ok(at));
        assert_eq!(parse_local_input(" 2026-03-02T09:05 ", seoul), Ok(at));
        // 초는 버린다(입력은 분 단위).
        assert_eq!(
            local_input(ts("2026-03-01T15:00:59Z"), seoul),
            "2026-03-02T00:00"
        );
        assert_eq!(
            local_date(ts("2026-03-01T15:00:00Z"), seoul),
            d("2026-03-02")
        );
    }

    fn d(value: &str) -> NaiveDate {
        value.parse().expect("date")
    }

    #[test]
    fn local_input_handles_dst_gap_and_overlap() {
        let new_york = zone("America/New_York");
        // 2026-03-08 02:00 → 03:00 (EST→EDT): 02:30은 없다.
        assert_eq!(
            parse_local_input("2026-03-08T02:30", new_york),
            Err(LocalInputError::Nonexistent)
        );
        assert_eq!(
            parse_local_input("2026-03-08T03:00", new_york),
            Ok(ts("2026-03-08T07:00:00Z"))
        );
        // 2026-11-01 01:30은 두 번 온다. 먼저 오는 EDT(UTC-4)를 고른다.
        assert_eq!(
            parse_local_input("2026-11-01T01:30", new_york),
            Ok(ts("2026-11-01T05:30:00Z"))
        );
        // 두 번째(EST) 01:30도 표시는 같은 벽시계 값이다.
        assert_eq!(
            local_input(ts("2026-11-01T06:30:00Z"), new_york),
            "2026-11-01T01:30"
        );
    }

    #[test]
    fn local_input_rejects_malformed_values() {
        let seoul = zone("Asia/Seoul");
        assert_eq!(parse_local_input("", seoul), Err(LocalInputError::Empty));
        assert_eq!(parse_local_input("   ", seoul), Err(LocalInputError::Empty));
        for raw in [
            "2026-3-2T09:05",
            "2026-03-02 09:05",
            "2026-03-02T9:05",
            "2026-03-02T09:05:00",
            "2026-03-02T09:05Z",
            "2026-02-30T09:00",
            "2026-13-01T09:00",
            "2026-03-02T24:00",
            "2026-03-02T23:60",
            "+2026-03-02T09:0",
            "２０２６-03-02T09:05",
            "abcd-ef-ghTij:kl",
        ] {
            assert_eq!(
                parse_local_input(raw, seoul),
                Err(LocalInputError::Invalid),
                "{raw}"
            );
        }
    }

    #[test]
    fn elapsed_minutes_never_goes_negative() {
        let start = ts("2026-03-02T00:00:00Z");
        assert_eq!(elapsed_minutes(start, ts("2026-03-02T00:00:59Z")), 0);
        assert_eq!(elapsed_minutes(start, ts("2026-03-02T03:12:00Z")), 192);
        assert_eq!(elapsed_minutes(start, ts("2026-03-01T23:00:00Z")), 0);
    }

    #[test]
    fn initial_handles_empty_and_multibyte() {
        assert_eq!(initial("홍길동"), "홍");
        assert_eq!(initial("  "), "?");
        assert_eq!(initial("😀 kim"), "😀");
    }
}
