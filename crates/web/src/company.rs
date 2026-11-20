//! 회사의 근무 요일·근무 시간·근무일 전환 시각(`GET /settings/company`).
//!
//! 로그인하면 셸이 한 번 읽고, 근무 정책을 저장하면 다시 읽는다. 읽기 전이나 읽지 못했을 때는
//! 서버 기본값(월~금, 09:00~18:00, 자정 전환)으로 본다. 모두 표시·기본값 용도이며 최종 판정은
//! 서버가 한다.

use chrono::{Datelike, Duration, NaiveDate, NaiveTime};
use chrono_tz::Tz;
use dioxus::prelude::*;
use pango_api::admin::PublicCompanySettings;
use pango_core::calendar::WorkWeek;
use pango_core::time::{MinuteOfDay, Timestamp, WorkCalendarZone};

use crate::api;
use crate::format;

#[derive(Clone, Copy)]
pub struct Company {
    settings: Signal<Option<PublicCompanySettings>>,
}

impl Company {
    /// 근무 요일. 모르면 월~금.
    pub fn work_week(&self) -> WorkWeek {
        work_week(self.settings.read().as_ref())
    }

    /// 근무일 전환 시각을 반영한 오늘(브라우저 시계). 전환 시각 전이면 전날이다.
    pub fn work_today(&self, tz: Tz) -> NaiveDate {
        work_date(
            crate::ui::now(),
            tz,
            day_cutoff(self.settings.read().as_ref()),
        )
    }

    /// 기록이 없는 날의 정정 요청 기본값 `(출근, 퇴근)`(`datetime-local` 값). 모르면 빈 값.
    pub fn default_times(&self, date: NaiveDate) -> (String, String) {
        default_times(date, self.settings.read().as_ref())
    }

    pub async fn refresh(mut self) {
        // 읽지 못하면 기본값을 쓴다(화면 표시용이라 막지 않는다). 세션 만료는 `api`가 처리한다.
        if let Ok(settings) = api::get::<PublicCompanySettings>("/settings/company").await {
            self.settings.set(Some(settings));
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.settings.read().is_some()
    }
}

pub fn provide_company() -> Company {
    use_context_provider(|| Company {
        settings: Signal::new(None),
    })
}

pub fn use_company() -> Company {
    use_context::<Company>()
}

/// 회사 설정을 다시 읽는다. 훅이 아니므로 이벤트 핸들러 안에서 부른다.
pub fn refresh_company() {
    if let Some(company) = try_consume_context::<Company>() {
        dioxus::dioxus_core::spawn_forever(company.refresh());
    }
}

/// 근무 요일. 설정이 없거나 값이 잘못됐으면 월~금.
pub fn work_week(settings: Option<&PublicCompanySettings>) -> WorkWeek {
    settings
        .and_then(|s| WorkWeek::from_bits(s.workdays).ok())
        .unwrap_or(WorkWeek::MON_TO_FRI)
}

/// 근무 요일이 아닌 날(달력에서 흐리게).
pub fn is_day_off(week: WorkWeek, date: NaiveDate) -> bool {
    !week.contains(date.weekday())
}

fn day_cutoff(settings: Option<&PublicCompanySettings>) -> MinuteOfDay {
    settings.map_or(MinuteOfDay::MIDNIGHT, |s| s.day_cutoff)
}

/// `at`이 속한 근무일. 전환 시각이 범위 밖이면(서버가 막으므로 실제로는 없다) 달력 날짜다.
pub fn work_date(at: Timestamp, tz: Tz, cutoff: MinuteOfDay) -> NaiveDate {
    WorkCalendarZone::new(tz, cutoff)
        .map(|zone| zone.work_date(at))
        .unwrap_or_else(|_| format::local_date(at, tz))
}

fn wall_clock(date: NaiveDate, time: MinuteOfDay) -> String {
    let minutes = time.minutes();
    let at = NaiveTime::from_hms_opt(u32::from(minutes / 60), u32::from(minutes % 60), 0)
        .unwrap_or(NaiveTime::MIN);
    date.and_time(at).format("%Y-%m-%dT%H:%M").to_string()
}

/// 그날의 근무 시작·종료 시각. 종료가 시작보다 이르거나 같으면(야간 근무) 다음 날 종료다.
pub fn default_times(
    date: NaiveDate,
    settings: Option<&PublicCompanySettings>,
) -> (String, String) {
    let Some(settings) = settings else {
        return (String::new(), String::new());
    };
    let end_date = if settings.work_end <= settings.work_start {
        date.checked_add_signed(Duration::days(1)).unwrap_or(date)
    } else {
        date
    };
    (
        wall_clock(date, settings.work_start),
        wall_clock(end_date, settings.work_end),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(workdays: u8, start: &str, end: &str, cutoff: &str) -> PublicCompanySettings {
        PublicCompanySettings {
            company_name: "회사".into(),
            timezone: "Asia/Seoul".into(),
            work_start: MinuteOfDay::parse(start).expect("time"),
            work_end: MinuteOfDay::parse(end).expect("time"),
            workdays,
            day_cutoff: MinuteOfDay::parse(cutoff).expect("time"),
        }
    }

    fn d(value: &str) -> NaiveDate {
        value.parse().expect("date")
    }

    fn ts(rfc3339: &str) -> Timestamp {
        Timestamp::from_utc(
            chrono::DateTime::parse_from_rfc3339(rfc3339)
                .expect("rfc3339")
                .with_timezone(&chrono::Utc),
        )
    }

    #[test]
    fn work_week_falls_back_to_weekdays() {
        assert_eq!(work_week(None), WorkWeek::MON_TO_FRI);
        assert_eq!(
            work_week(Some(&settings(0, "09:00", "18:00", "00:00"))),
            WorkWeek::MON_TO_FRI,
            "근무 요일이 없는 값은 받지 않는다"
        );
        // 월~토(비트 0..5).
        let six = work_week(Some(&settings(0b011_1111, "09:00", "18:00", "00:00")));
        assert!(!is_day_off(six, d("2026-03-07")), "토요일 근무");
        assert!(is_day_off(six, d("2026-03-08")), "일요일 휴무");
        let weekdays = work_week(None);
        assert!(is_day_off(weekdays, d("2026-03-07")));
        assert!(!is_day_off(weekdays, d("2026-03-09")));
    }

    #[test]
    fn work_date_moves_back_before_the_cutoff() {
        let seoul = chrono_tz::Asia::Seoul;
        let cutoff = MinuteOfDay::parse("05:00").expect("time");
        // KST 2026-03-03 04:59 → 전날 근무일.
        assert_eq!(
            work_date(ts("2026-03-02T19:59:00Z"), seoul, cutoff),
            d("2026-03-02")
        );
        // KST 05:00 정각부터 그날.
        assert_eq!(
            work_date(ts("2026-03-02T20:00:00Z"), seoul, cutoff),
            d("2026-03-03")
        );
        assert_eq!(
            work_date(ts("2026-03-02T15:00:00Z"), seoul, MinuteOfDay::MIDNIGHT),
            d("2026-03-03"),
            "자정 전환이면 달력 날짜"
        );
    }

    #[test]
    fn default_times_follow_work_hours() {
        assert_eq!(
            default_times(d("2026-03-02"), None),
            (String::new(), String::new())
        );
        assert_eq!(
            default_times(
                d("2026-03-02"),
                Some(&settings(0b001_1111, "08:30", "17:30", "00:00"))
            ),
            ("2026-03-02T08:30".to_owned(), "2026-03-02T17:30".to_owned())
        );
        assert_eq!(
            default_times(
                d("2026-03-02"),
                Some(&settings(0b001_1111, "22:00", "06:00", "00:00"))
            ),
            ("2026-03-02T22:00".to_owned(), "2026-03-03T06:00".to_owned()),
            "야간 근무는 다음 날 퇴근"
        );
        assert_eq!(
            default_times(
                NaiveDate::MAX,
                Some(&settings(0b001_1111, "22:00", "06:00", "00:00"))
            )
            .1,
            format!("{}T06:00", NaiveDate::MAX),
            "날짜 끝에서 넘치지 않는다"
        );
    }
}
