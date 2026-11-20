//! 표시용 변환(시각, 날짜, 시간 길이). 순수 함수만 둔다.

use chrono::{Datelike, NaiveDate, Timelike, Weekday};
use chrono_tz::Tz;
use pango_core::time::Timestamp;

/// 회사 시간대. 이름이 잘못됐으면 UTC로 표시한다(서버가 검증한 값이라 실제로는 일어나지 않는다).
pub fn zone(name: &str) -> Tz {
    name.parse().unwrap_or(Tz::UTC)
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

    fn ts(rfc3339: &str) -> Timestamp {
        Timestamp::from_utc(
            chrono::DateTime::parse_from_rfc3339(rfc3339)
                .expect("rfc3339")
                .with_timezone(&chrono::Utc),
        )
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
    fn initial_handles_empty_and_multibyte() {
        assert_eq!(initial("홍길동"), "홍");
        assert_eq!(initial("  "), "?");
        assert_eq!(initial("😀 kim"), "😀");
    }
}
