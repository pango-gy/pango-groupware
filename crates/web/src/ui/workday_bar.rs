//! 하루 막대(근무 타임라인)와 이번 주 누적 막대. 이 제품의 대표 요소다(docs/DESIGN.md §6).
//!
//! 위치 계산은 모두 순수 함수이고, 컴포넌트는 그 결과를 퍼센트로 그리기만 한다.

use chrono::{Datelike, Duration, NaiveDate, NaiveTime};
use chrono_tz::Tz;
use dioxus::prelude::*;
use pango_core::time::{MinuteOfDay, Timestamp};

use crate::calendar::YearMonth;
use crate::format;

const DAY_MINUTES: i64 = 24 * 60;
/// 주 기준 근무 시간(40시간)과 법정 상한(52시간), 막대의 오른쪽 끝(60시간).
pub const WEEK_TARGET_MINUTES: u32 = 40 * 60;
pub const WEEK_LIMIT_MINUTES: u32 = 52 * 60;
const WEEK_AXIS_MINUTES: u32 = 60 * 60;
/// 두 표지의 글자가 겹치지 않는 최소 간격(%).
const LABEL_GAP: f64 = 14.0;

/// 막대 위의 한 구간(왼쪽 위치와 폭, 0~100%).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub left: f64,
    pub width: f64,
}

impl Span {
    /// 축 안으로 자른 구간. 축 밖이거나 길이가 없으면 `None`.
    fn clamped(from: f64, to: f64) -> Option<Self> {
        let (from, to) = (from.clamp(0.0, 100.0), to.clamp(0.0, 100.0));
        (to > from).then_some(Self {
            left: from,
            width: to - from,
        })
    }

    fn style(self) -> String {
        format!("left: {:.2}%; width: {:.2}%", self.left, self.width)
    }
}

/// 하루 막대를 그리는 데 필요한 사실.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DayInput {
    /// 막대가 나타내는 근무일.
    pub work_date: NaiveDate,
    /// 근무일 전환 시각. 축은 이 시각에서 시작해 24시간이다.
    pub cutoff: MinuteOfDay,
    pub expected_start: MinuteOfDay,
    pub expected_end: MinuteOfDay,
    pub clock_in: Option<Timestamp>,
    pub clock_out: Option<Timestamp>,
    /// 지각으로 판정됐는지(유예 시간은 서버가 본다).
    pub late: bool,
    /// 퇴근 없이 끝난 기록(퇴근 누락). 근무 구간을 칠하지 않는다.
    pub missing_clock_out: bool,
    pub now: Timestamp,
    pub tz: Tz,
}

/// 하루 막대의 모양. 값은 모두 축의 퍼센트 위치다.
#[derive(Debug, Clone, PartialEq)]
pub struct DayBar {
    /// 회사 근무 시간대(옅은 띠).
    pub band: Option<Span>,
    /// 출근~퇴근(또는 현재).
    pub worked: Option<Span>,
    /// 근무 시작부터 출근까지 늦은 구간.
    pub late: Option<Span>,
    pub clock_in: Option<f64>,
    /// 지금이 이 근무일 안에 있을 때만.
    pub now: Option<f64>,
    /// 축 눈금 글자(6시간 간격, 다섯 개).
    pub ticks: [String; 5],
}

/// 축 시작(근무일의 전환 시각)에서 `at`까지의 분. 축 밖이면 음수이거나 1440 이상이다.
pub fn axis_minutes(at: Timestamp, work_date: NaiveDate, cutoff: MinuteOfDay, tz: Tz) -> i64 {
    let minutes = u32::from(cutoff.minutes());
    let start = work_date
        .and_time(NaiveTime::from_hms_opt(minutes / 60, minutes % 60, 0).unwrap_or(NaiveTime::MIN));
    let local = at.to_utc().with_timezone(&tz).naive_local();
    (local - start).num_minutes()
}

/// 축의 분을 퍼센트로(자르지 않는다).
fn percent(minutes: i64) -> f64 {
    minutes as f64 * 100.0 / DAY_MINUTES as f64
}

/// 축 위에 있을 때만 그 위치.
fn on_axis(minutes: i64) -> Option<f64> {
    (0..=DAY_MINUTES)
        .contains(&minutes)
        .then(|| percent(minutes))
}

/// 회사 근무 시간대의 축 위치 `(시작 분, 끝 분)`. 종료가 시작보다 이르거나 같으면(야간 근무)
/// 다음 날 종료다. 전환 시각보다 이른 시작은 축의 뒤쪽(다음 날 새벽)에 놓인다.
pub fn band_minutes(start: MinuteOfDay, end: MinuteOfDay, cutoff: MinuteOfDay) -> (i64, i64) {
    let (start, end, cutoff) = (
        i64::from(start.minutes()),
        i64::from(end.minutes()),
        i64::from(cutoff.minutes()),
    );
    let from = (start - cutoff).rem_euclid(DAY_MINUTES);
    let length = match (end - start).rem_euclid(DAY_MINUTES) {
        0 => DAY_MINUTES,
        length => length,
    };
    (from, from + length)
}

/// 축 눈금: 전환 시각부터 6시간 간격의 시(`00`, `06`, …). 마지막은 다음 날 같은 시각이다.
pub fn axis_ticks(cutoff: MinuteOfDay) -> [String; 5] {
    let start = u32::from(cutoff.minutes()) / 60;
    std::array::from_fn(|index| format!("{:02}", (start + 6 * index as u32) % 24))
}

pub fn day_bar(input: &DayInput) -> DayBar {
    let offset = |at: Timestamp| axis_minutes(at, input.work_date, input.cutoff, input.tz);
    let (band_from, band_to) = band_minutes(input.expected_start, input.expected_end, input.cutoff);
    let now = offset(input.now);
    let clock_in = input.clock_in.map(offset);
    // 끝: 퇴근 시각, 아직 근무 중이면 지금. 퇴근 누락이면 끝을 모른다.
    let end = match (input.clock_out, input.missing_clock_out) {
        (Some(out), _) => Some(offset(out)),
        (None, false) => Some(now),
        (None, true) => None,
    };
    let worked = clock_in
        .zip(end)
        .and_then(|(from, to)| Span::clamped(percent(from), percent(to)));
    let late = clock_in
        .filter(|from| input.late && *from > band_from)
        .and_then(|from| Span::clamped(percent(band_from), percent(from)));
    DayBar {
        band: Span::clamped(percent(band_from), percent(band_to)),
        worked,
        late,
        clock_in: clock_in.map(|minutes| percent(minutes).clamp(0.0, 100.0)),
        now: on_axis(now),
        ticks: axis_ticks(input.cutoff),
    }
}

/// 표지 글자를 놓는 쪽. 막대 끝에서는 글자가 밖으로 나가지 않게 안쪽으로 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    Start,
    Middle,
    End,
}

pub fn label_anchor(position: f64) -> Anchor {
    if position < 10.0 {
        Anchor::Start
    } else if position > 90.0 {
        Anchor::End
    } else {
        Anchor::Middle
    }
}

impl Anchor {
    const fn class(self) -> &'static str {
        match self {
            Self::Start => "workday-flag workday-flag-start",
            Self::Middle => "workday-flag",
            Self::End => "workday-flag workday-flag-end",
        }
    }
}

/// "현재" 글자를 보일지. 출근 표지와 너무 가까우면 선만 남긴다(글자가 겹친다).
pub fn now_label_shown(clock_in: Option<f64>, now: f64) -> bool {
    clock_in.is_none_or(|at| (now - at).abs() >= LABEL_GAP)
}

/// 날짜가 속한 주(월요일~일요일).
pub fn week_of(date: NaiveDate) -> (NaiveDate, NaiveDate) {
    let back = i64::from(date.weekday().num_days_from_monday());
    let monday = date
        .checked_sub_signed(Duration::days(back))
        .unwrap_or(date);
    let sunday = monday
        .checked_add_signed(Duration::days(6))
        .unwrap_or(monday);
    (monday, sunday)
}

/// 그 주의 기록을 읽으려면 필요한 달. 주가 달을 넘으면 두 번째 달도 있다.
pub fn week_months(week: (NaiveDate, NaiveDate)) -> (YearMonth, Option<YearMonth>) {
    let (first, last) = (YearMonth::of(week.0), YearMonth::of(week.1));
    (first, (first != last).then_some(last))
}

/// 주의 누적 근무 분. `days`는 `(날짜, 근무 분)`이고, 주 밖의 날은 세지 않는다.
/// 아직 퇴근하지 않은 근무의 경과 분(`live_minutes`)을 더한다.
pub fn week_minutes(
    days: impl IntoIterator<Item = (NaiveDate, u32)>,
    week: (NaiveDate, NaiveDate),
    live_minutes: u32,
) -> u32 {
    days.into_iter()
        .filter(|(date, _)| (week.0..=week.1).contains(date))
        .fold(live_minutes, |sum, (_, minutes)| {
            sum.saturating_add(minutes)
        })
}

/// 이번 주 막대의 모양.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeekBar {
    /// 40시간까지.
    pub regular: Option<Span>,
    /// 40시간을 넘긴 부분.
    pub extra: Option<Span>,
    /// 52시간 상한을 넘겼는지.
    pub over_limit: bool,
    pub target_mark: f64,
    pub limit_mark: f64,
}

pub fn week_bar(minutes: u32) -> WeekBar {
    let at = |minutes: u32| {
        f64::from(minutes.min(WEEK_AXIS_MINUTES)) * 100.0 / f64::from(WEEK_AXIS_MINUTES)
    };
    WeekBar {
        regular: Span::clamped(0.0, at(minutes.min(WEEK_TARGET_MINUTES))),
        extra: Span::clamped(at(WEEK_TARGET_MINUTES), at(minutes)),
        over_limit: minutes > WEEK_LIMIT_MINUTES,
        target_mark: at(WEEK_TARGET_MINUTES),
        limit_mark: at(WEEK_LIMIT_MINUTES),
    }
}

/// 하루 막대. `compact`면 높이 6px의 막대만 그린다(목록의 각 행용).
///
/// 막대는 그림이므로 같은 내용을 문장(`summary`)으로도 준다.
#[component]
pub fn WorkdayBar(
    bar: DayBar,
    /// 화면 낭독기가 읽는 한 줄 요약(예: "09:12 출근, 근무 중").
    #[props(into)]
    summary: String,
    /// 출근 표지에 붙는 글자(예: "출근 09:12").
    #[props(into, default)]
    clock_in_label: String,
    #[props(default)] compact: bool,
) -> Element {
    let track = rsx! {
        div { class: "workday-track",
            if let Some(band) = bar.band {
                span { class: "workday-band", style: band.style() }
            }
            if let Some(late) = bar.late {
                span { class: "workday-late", style: late.style() }
            }
            if let Some(worked) = bar.worked {
                span { class: "workday-worked", style: worked.style() }
            }
        }
    };
    if compact {
        return rsx! {
            div { class: "workday workday-compact", role: "img", "aria-label": "{summary}", {track} }
        };
    }
    rsx! {
        div { class: "workday", role: "img", "aria-label": "{summary}",
            div { class: "workday-flags", "aria-hidden": "true",
                if let Some(at) = bar.clock_in {
                    span { class: label_anchor(at).class(), style: "left: {at:.2}%",
                        "{clock_in_label}"
                    }
                }
                if let Some(at) = bar.now.filter(|at| now_label_shown(bar.clock_in, *at)) {
                    span { class: label_anchor(at).class(), style: "left: {at:.2}%", "현재" }
                }
            }
            div { class: "workday-rail",
                {track}
                if let Some(at) = bar.clock_in {
                    span { class: "workday-mark", style: "left: {at:.2}%" }
                }
                if let Some(at) = bar.now {
                    span { class: "workday-mark workday-mark-now", style: "left: {at:.2}%" }
                }
            }
            div { class: "workday-axis tnum", "aria-hidden": "true",
                for tick in bar.ticks.iter() {
                    span { "{tick}" }
                }
            }
        }
    }
}

/// 이번 주 누적 근무시간 막대(40시간 기준, 52시간 상한 표시).
#[component]
pub fn WeekProgress(minutes: u32) -> Element {
    let bar = week_bar(minutes);
    let worked = format::duration_minutes(minutes);
    rsx! {
        div { class: "week",
            p { class: "week-head",
                span { class: "week-label", "이번 주" }
                strong { class: "week-value tnum", "{worked}" }
                span { class: "week-target tnum", "/ 40시간" }
                if bar.over_limit {
                    span { class: "week-over", "52시간을 넘었어요" }
                }
            }
            div { class: "week-rail", "aria-hidden": "true",
                div { class: "week-track",
                    if let Some(regular) = bar.regular {
                        span { class: "week-regular", style: regular.style() }
                    }
                    if let Some(extra) = bar.extra {
                        span {
                            class: if bar.over_limit { "week-extra week-extra-over" } else { "week-extra" },
                            style: extra.style(),
                        }
                    }
                }
                span { class: "week-mark", style: "left: {bar.target_mark:.2}%" }
                span { class: "week-mark week-mark-limit", style: "left: {bar.limit_mark:.2}%" }
            }
            div { class: "week-axis tnum", "aria-hidden": "true",
                span { style: "left: {bar.target_mark:.2}%", "40" }
                span { style: "left: {bar.limit_mark:.2}%", "52시간" }
            }
        }
    }
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

    fn date(value: &str) -> NaiveDate {
        value.parse().expect("date")
    }

    fn hm(value: &str) -> MinuteOfDay {
        MinuteOfDay::parse(value).expect("time")
    }

    fn seoul() -> Tz {
        format::zone("Asia/Seoul")
    }

    /// 2026-03-02(월), 09:00~18:00, 자정 전환, 한국 시간.
    fn input(clock_in: Option<&str>, clock_out: Option<&str>, now: &str) -> DayInput {
        DayInput {
            work_date: date("2026-03-02"),
            cutoff: MinuteOfDay::MIDNIGHT,
            expected_start: hm("09:00"),
            expected_end: hm("18:00"),
            clock_in: clock_in.map(ts),
            clock_out: clock_out.map(ts),
            late: false,
            missing_clock_out: false,
            now: ts(now),
            tz: seoul(),
        }
    }

    fn close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 0.01, "{actual} != {expected}");
    }

    fn span(actual: Option<Span>, left: f64, width: f64) {
        let actual = actual.expect("span");
        close(actual.left, left);
        close(actual.width, width);
    }

    #[test]
    fn no_record_shows_only_the_band_and_now() {
        // 03-02 08:00 KST: 근무 시작 전.
        let bar = day_bar(&input(None, None, "2026-03-01T23:00:00Z"));
        span(bar.band, 37.5, 37.5);
        assert_eq!(bar.worked, None);
        assert_eq!(bar.late, None);
        assert_eq!(bar.clock_in, None);
        close(bar.now.expect("now"), 100.0 * 8.0 / 24.0);
        assert_eq!(bar.ticks, ["00", "06", "12", "18", "00"]);
    }

    #[test]
    fn working_span_runs_from_clock_in_to_now() {
        // 08:30 출근(시작 전), 지금 12:00.
        let bar = day_bar(&input(
            Some("2026-03-01T23:30:00Z"),
            None,
            "2026-03-02T03:00:00Z",
        ));
        span(bar.worked, 100.0 * 8.5 / 24.0, 100.0 * 3.5 / 24.0);
        assert_eq!(bar.late, None, "일찍 왔으면 늦은 구간이 없다");
        close(bar.clock_in.expect("in"), 100.0 * 8.5 / 24.0);
        close(bar.now.expect("now"), 50.0);
    }

    #[test]
    fn late_span_covers_start_to_clock_in_only_when_judged_late() {
        // 09:30 출근, 18:10 퇴근.
        let mut late = input(
            Some("2026-03-02T00:30:00Z"),
            Some("2026-03-02T09:10:00Z"),
            "2026-03-02T10:00:00Z",
        );
        assert_eq!(day_bar(&late).late, None, "유예 안이면 지각이 아니다");
        late.late = true;
        let bar = day_bar(&late);
        span(bar.late, 37.5, 100.0 * 0.5 / 24.0);
        span(
            bar.worked,
            100.0 * 9.5 / 24.0,
            100.0 * (18.0 + 10.0 / 60.0 - 9.5) / 24.0,
        );
        // 지각 판정이어도 시작 전에 찍혔으면(정정 등) 칠할 구간이 없다.
        late.clock_in = Some(ts("2026-03-01T23:00:00Z"));
        assert_eq!(day_bar(&late).late, None);
    }

    #[test]
    fn spans_are_clamped_to_the_day() {
        // 퇴근이 다음 날 02:00(자정 전환): 막대는 24시에서 끝난다.
        let bar = day_bar(&input(
            Some("2026-03-02T09:00:00Z"),
            Some("2026-03-02T17:00:00Z"),
            "2026-03-02T18:00:00Z",
        ));
        span(bar.worked, 75.0, 25.0);
        assert_eq!(bar.now, None, "지금은 다음 근무일이다");
        // 전날 시작해 넘어온 기록을 오늘 막대에 그리면 0에서 시작한다.
        let mut carried = input(Some("2026-03-01T13:00:00Z"), None, "2026-03-01T21:00:00Z");
        carried.missing_clock_out = false;
        let bar = day_bar(&carried);
        span(bar.worked, 0.0, 25.0);
        close(bar.clock_in.expect("in"), 0.0);
        // 출근이 지금보다 뒤(시계 차이)면 길이가 없다.
        let ahead = input(Some("2026-03-02T05:00:00Z"), None, "2026-03-02T04:00:00Z");
        assert_eq!(day_bar(&ahead).worked, None);
    }

    #[test]
    fn missing_clock_out_draws_no_span() {
        let mut missing = input(Some("2026-03-02T00:00:00Z"), None, "2026-03-03T01:00:00Z");
        missing.missing_clock_out = true;
        let bar = day_bar(&missing);
        assert_eq!(bar.worked, None);
        close(bar.clock_in.expect("in"), 37.5);
        assert_eq!(bar.now, None);
    }

    #[test]
    fn night_shift_axis_starts_at_the_cutoff() {
        // 22:00~07:00 근무, 근무일 전환 12:00. 축은 12시부터 다음 날 12시.
        let night = DayInput {
            cutoff: hm("12:00"),
            expected_start: hm("22:00"),
            expected_end: hm("07:00"),
            // 03-02 22:05 출근, 03-03 06:50 퇴근(KST).
            clock_in: Some(ts("2026-03-02T13:05:00Z")),
            clock_out: Some(ts("2026-03-02T21:50:00Z")),
            late: true,
            ..input(None, None, "2026-03-02T22:00:00Z")
        };
        assert_eq!(
            band_minutes(hm("22:00"), hm("07:00"), hm("12:00")),
            (600, 1140)
        );
        let bar = day_bar(&night);
        span(bar.band, 100.0 * 10.0 / 24.0, 100.0 * 9.0 / 24.0);
        span(bar.late, 100.0 * 10.0 / 24.0, 100.0 * 5.0 / 1440.0);
        span(bar.worked, 100.0 * 605.0 / 1440.0, 100.0 * 525.0 / 1440.0);
        // 03-03 07:00 KST는 아직 03-02 근무일이다.
        close(bar.now.expect("now"), 100.0 * 19.0 / 24.0);
        assert_eq!(bar.ticks, ["12", "18", "00", "06", "12"]);
        // 전환 시각 전의 새벽 근무 시작(05:00, 전환 06:00)은 축 뒤쪽에 놓여 잘린다.
        assert_eq!(
            band_minutes(hm("05:00"), hm("14:00"), hm("06:00")),
            (1380, 1920)
        );
        // 시작과 종료가 같으면 24시간 근무로 본다.
        assert_eq!(
            band_minutes(hm("09:00"), hm("09:00"), hm("00:00")),
            (540, 1980)
        );
    }

    #[test]
    fn axis_minutes_follow_the_company_zone() {
        let cutoff = hm("04:00");
        let day = date("2026-03-02");
        // 03-02 04:00 KST = 03-01 19:00 UTC
        assert_eq!(
            axis_minutes(ts("2026-03-01T19:00:00Z"), day, cutoff, seoul()),
            0
        );
        assert_eq!(
            axis_minutes(ts("2026-03-01T18:59:00Z"), day, cutoff, seoul()),
            -1
        );
        assert_eq!(
            axis_minutes(ts("2026-03-02T19:00:00Z"), day, cutoff, seoul()),
            1440
        );
    }

    #[test]
    fn labels_stay_inside_and_do_not_overlap() {
        assert_eq!(label_anchor(0.0), Anchor::Start);
        assert_eq!(label_anchor(9.9), Anchor::Start);
        assert_eq!(label_anchor(50.0), Anchor::Middle);
        assert_eq!(label_anchor(90.1), Anchor::End);
        assert!(now_label_shown(None, 40.0));
        assert!(now_label_shown(Some(20.0), 40.0));
        assert!(!now_label_shown(Some(38.0), 40.0));
        assert!(!now_label_shown(Some(45.0), 40.0));
    }

    #[test]
    fn week_runs_monday_to_sunday_and_may_cross_months() {
        // 2026-03-04는 수요일.
        let week = week_of(date("2026-03-04"));
        assert_eq!(week, (date("2026-03-02"), date("2026-03-08")));
        assert_eq!(week_of(date("2026-03-02")), week, "월요일");
        assert_eq!(week_of(date("2026-03-08")), week, "일요일");
        assert_eq!(
            week_months(week),
            (
                YearMonth {
                    year: 2026,
                    month: 3
                },
                None
            )
        );
        // 2026-04-01(수)의 주는 3월 30일에 시작한다.
        let crossing = week_of(date("2026-04-01"));
        assert_eq!(crossing, (date("2026-03-30"), date("2026-04-05")));
        assert_eq!(
            week_months(crossing),
            (
                YearMonth {
                    year: 2026,
                    month: 3
                },
                Some(YearMonth {
                    year: 2026,
                    month: 4
                })
            )
        );
        // 해를 넘는 주.
        assert_eq!(
            week_months(week_of(date("2026-01-01"))),
            (
                YearMonth {
                    year: 2025,
                    month: 12
                },
                Some(YearMonth {
                    year: 2026,
                    month: 1
                })
            )
        );
    }

    #[test]
    fn week_minutes_count_only_days_in_the_week() {
        let week = week_of(date("2026-03-04"));
        let days = [
            (date("2026-03-01"), 480), // 지난주 일요일
            (date("2026-03-02"), 480),
            (date("2026-03-03"), 500),
            (date("2026-03-08"), 60),
            (date("2026-03-09"), 480), // 다음 주
        ];
        assert_eq!(week_minutes(days, week, 0), 1040);
        assert_eq!(
            week_minutes(days, week, 125),
            1165,
            "근무 중인 시간을 더한다"
        );
        assert_eq!(week_minutes([], week, 0), 0);
        assert_eq!(
            week_minutes([(date("2026-03-02"), u32::MAX)], week, 10),
            u32::MAX,
            "넘치지 않는다"
        );
    }

    #[test]
    fn week_bar_marks_target_and_limit() {
        let empty = week_bar(0);
        assert_eq!(empty.regular, None);
        assert_eq!(empty.extra, None);
        assert!(!empty.over_limit);
        close(empty.target_mark, 100.0 * 40.0 / 60.0);
        close(empty.limit_mark, 100.0 * 52.0 / 60.0);

        let half = week_bar(20 * 60);
        span(half.regular, 0.0, 100.0 * 20.0 / 60.0);
        assert_eq!(half.extra, None);

        let exact = week_bar(WEEK_TARGET_MINUTES);
        span(exact.regular, 0.0, exact.target_mark);
        assert_eq!(exact.extra, None);

        let overtime = week_bar(46 * 60);
        span(overtime.regular, 0.0, overtime.target_mark);
        span(overtime.extra, overtime.target_mark, 100.0 * 6.0 / 60.0);
        assert!(!overtime.over_limit);
        assert!(
            !week_bar(WEEK_LIMIT_MINUTES).over_limit,
            "52시간까지는 상한 안"
        );

        let over = week_bar(WEEK_LIMIT_MINUTES + 1);
        assert!(over.over_limit);
        // 축(60시간)을 넘어도 막대는 끝에서 멈춘다.
        let huge = week_bar(u32::MAX);
        span(huge.extra, huge.target_mark, 100.0 - huge.target_mark);
        assert!(huge.over_limit);
    }
}
