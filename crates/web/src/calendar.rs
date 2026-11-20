//! 달력 계산(순수 함수). 근태·휴가 달력 화면이 함께 쓴다.

use chrono::{Datelike, NaiveDate};

/// 연·월. 화면의 "이번 달" 상태로 쓴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct YearMonth {
    pub year: i32,
    pub month: u32,
}

impl YearMonth {
    /// 달력에서 이동할 수 있는 범위. 서버가 받는 범위 안에서 넉넉하게 잡는다.
    const MIN_YEAR: i32 = 2000;
    const MAX_YEAR: i32 = 2100;

    pub fn of(date: NaiveDate) -> Self {
        Self {
            year: date.year(),
            month: date.month(),
        }
    }

    pub fn first_day(self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year, self.month, 1).unwrap_or(NaiveDate::MIN)
    }

    pub fn last_day(self) -> NaiveDate {
        self.next().first_day().pred_opt().unwrap_or(NaiveDate::MAX)
    }

    pub fn prev(self) -> Self {
        if self.month == 1 {
            Self {
                year: self.year - 1,
                month: 12,
            }
        } else {
            Self {
                year: self.year,
                month: self.month - 1,
            }
        }
    }

    pub fn next(self) -> Self {
        if self.month == 12 {
            Self {
                year: self.year + 1,
                month: 1,
            }
        } else {
            Self {
                year: self.year,
                month: self.month + 1,
            }
        }
    }

    pub fn can_go_prev(self) -> bool {
        self.prev().year >= Self::MIN_YEAR
    }

    pub fn can_go_next(self) -> bool {
        self.next().year <= Self::MAX_YEAR
    }

    /// API 쿼리용 `YYYY-MM`.
    pub fn query(self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }

    /// 화면용 `2026년 3월`.
    pub fn label(self) -> String {
        format!("{}년 {}월", self.year, self.month)
    }

    /// 일요일부터 시작하는 주 단위 격자. 그 달에 속하지 않는 칸은 `None`.
    pub fn weeks(self) -> Vec<[Option<NaiveDate>; 7]> {
        let first = self.first_day();
        let days = self.last_day().day();
        let offset = first.weekday().num_days_from_sunday() as usize;
        let mut weeks = Vec::new();
        let mut week = [None; 7];
        for day in 1..=days {
            let index = (offset + day as usize - 1) % 7;
            week[index] = first.with_day(day);
            if index == 6 || day == days {
                weeks.push(week);
                week = [None; 7];
            }
        }
        weeks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ym(year: i32, month: u32) -> YearMonth {
        YearMonth { year, month }
    }

    fn d(value: &str) -> NaiveDate {
        value.parse().expect("date")
    }

    #[test]
    fn navigation_wraps_years() {
        assert_eq!(ym(2026, 1).prev(), ym(2025, 12));
        assert_eq!(ym(2026, 12).next(), ym(2027, 1));
        assert_eq!(ym(2026, 6).prev().next(), ym(2026, 6));
        assert!(!ym(2000, 1).can_go_prev() && ym(2000, 2).can_go_prev());
        assert!(!ym(2100, 12).can_go_next() && ym(2100, 11).can_go_next());
    }

    #[test]
    fn month_bounds_and_labels() {
        assert_eq!(ym(2028, 2).last_day(), d("2028-02-29"));
        assert_eq!(ym(2026, 2).last_day(), d("2026-02-28"));
        assert_eq!(ym(2026, 12).last_day(), d("2026-12-31"));
        assert_eq!(ym(2026, 3).query(), "2026-03");
        assert_eq!(ym(2026, 3).label(), "2026년 3월");
        assert_eq!(YearMonth::of(d("2026-03-02")), ym(2026, 3));
    }

    #[test]
    fn weeks_cover_every_day_exactly_once_in_order() {
        for (year, month) in [
            (2026, 2),
            (2026, 3),
            (2028, 2),
            (2026, 8),
            (2027, 1),
            (2026, 11),
        ] {
            let month = ym(year, month);
            let days: Vec<_> = month.weeks().into_iter().flatten().flatten().collect();
            let expected: Vec<_> = month
                .first_day()
                .iter_days()
                .take_while(|day| *day <= month.last_day())
                .collect();
            assert_eq!(days, expected, "{month:?}");
        }
    }

    #[test]
    fn weeks_align_weekdays_with_sunday_first() {
        // 2026-03-01은 일요일, 2026-02-01도 일요일(4주로 딱 맞는 달).
        let march = ym(2026, 3).weeks();
        assert_eq!(march[0][0], Some(d("2026-03-01")));
        assert_eq!(march.len(), 5);
        assert_eq!(ym(2026, 2).weeks().len(), 4);
        // 2026-08-01은 토요일 → 6주.
        let august = ym(2026, 8).weeks();
        assert_eq!(
            august[0],
            [None, None, None, None, None, None, Some(d("2026-08-01"))]
        );
        assert_eq!(august.len(), 6);
        for week in &march {
            for (index, day) in week.iter().enumerate() {
                if let Some(day) = day {
                    assert_eq!(day.weekday().num_days_from_sunday() as usize, index);
                }
            }
        }
    }
}
