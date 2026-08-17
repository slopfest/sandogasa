// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Calendar periods: the grains a report can cover, and where each one
//! is filed.
//!
//! Weeks run Monday to Sunday but never across a month boundary, so a
//! month's figures are the sum of its weeks with no week counted against
//! two months. That is the one rule here worth stating twice, because it
//! is what makes a monthly report and its weeklies agree.

use std::path::PathBuf;

use chrono::{Datelike, Duration, NaiveDate};

/// How a period is stored: the directory under the root, and how a
/// period's first day names its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grain {
    Daily,
    Weekly,
    Monthly,
}

impl Grain {
    pub fn dir(self) -> &'static str {
        match self {
            Grain::Daily => "daily",
            Grain::Weekly => "weekly",
            Grain::Monthly => "monthly",
        }
    }

    /// Where a period beginning on `day` is stored, relative to a root.
    ///
    /// Daily and weekly are dated to the day — a week by its first day,
    /// which is its Monday, or the first of the month where a week is
    /// clipped by one. Monthly stops at the month, since the day would
    /// always be the first.
    pub fn path(self, day: NaiveDate) -> PathBuf {
        let base = PathBuf::from(self.dir()).join(format!("{:04}", day.year()));
        match self {
            Grain::Monthly => base.join(format!("{:02}", day.month())),
            _ => base
                .join(format!("{:02}", day.month()))
                .join(format!("{:02}", day.day())),
        }
    }
}

/// The days a chunk covers, and where it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub grain: Grain,
    pub start: NaiveDate,
    /// Inclusive last day.
    pub end: NaiveDate,
}

impl Chunk {
    pub fn days(&self) -> impl Iterator<Item = NaiveDate> + use<> {
        let (start, end) = (self.start, self.end);
        std::iter::successors(Some(start), move |d| {
            let next = *d + Duration::days(1);
            (next <= end).then_some(next)
        })
    }

    pub fn path(&self) -> PathBuf {
        self.grain.path(self.start)
    }
}

/// The week a day belongs to, clipped to its month.
///
/// Weeks run Monday to Sunday, but never across a month boundary: a
/// month's figures are then the sum of its weeks, with no week counted
/// against two months. A clipped week keeps its first day as its name, so
/// the stub at the start of a month is dated to the first — August 2026
/// opens with a Saturday, and `weekly/2026/08/01` covers just the 1st and
/// 2nd.
pub fn week_of(day: NaiveDate) -> Chunk {
    let back = day.weekday().num_days_from_monday() as i64;
    let monday = day - Duration::days(back);
    let start = if monday.month() == day.month() {
        monday
    } else {
        day.with_day(1).expect("first of month exists")
    };
    let sunday = start + Duration::days(6 - (start.weekday().num_days_from_monday() as i64));
    let end = last_of_month(day).min(sunday);
    Chunk {
        grain: Grain::Weekly,
        start,
        end,
    }
}

/// The month a day belongs to.
pub fn month_of(day: NaiveDate) -> Chunk {
    Chunk {
        grain: Grain::Monthly,
        start: day.with_day(1).expect("first of month exists"),
        end: last_of_month(day),
    }
}

fn last_of_month(day: NaiveDate) -> NaiveDate {
    let (year, month) = (day.year(), day.month());
    let first_next = match month {
        12 => NaiveDate::from_ymd_opt(year + 1, 1, 1),
        _ => NaiveDate::from_ymd_opt(year, month + 1, 1),
    };
    first_next.expect("a month follows every month") - Duration::days(1)
}

/// Every week of a month, in order, clipped at both ends.
pub fn weeks_of_month(day: NaiveDate) -> Vec<Chunk> {
    let month = month_of(day);
    let mut weeks: Vec<Chunk> = Vec::new();
    let mut cursor = month.start;
    while cursor <= month.end {
        let week = week_of(cursor);
        cursor = week.end + Duration::days(1);
        weeks.push(week);
    }
    weeks
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn weeks_run_monday_to_sunday_but_never_across_a_month() {
        // A whole week inside one month.
        let mid = week_of(day(2026, 8, 12));
        assert_eq!(mid.start, day(2026, 8, 10)); // Monday
        assert_eq!(mid.end, day(2026, 8, 16)); // Sunday

        // August 2026 opens on a Saturday, so the first "week" is two
        // days and is dated to the 1st.
        let opening = week_of(day(2026, 8, 1));
        assert_eq!(opening.start, day(2026, 8, 1));
        assert_eq!(opening.end, day(2026, 8, 2));
        assert_eq!(week_of(day(2026, 8, 2)), opening);

        // And the last week is clipped at the month's end.
        let closing = week_of(day(2026, 8, 31));
        assert_eq!(closing.start, day(2026, 8, 31));
        assert_eq!(closing.end, day(2026, 8, 31));
    }

    #[test]
    fn a_months_weeks_cover_it_exactly_once() {
        for (year, month) in [(2026, 8), (2026, 2), (2024, 2), (2026, 12)] {
            let weeks = weeks_of_month(day(year, month, 1));
            let covered: Vec<NaiveDate> = weeks.iter().flat_map(Chunk::days).collect();
            let mut unique: BTreeSet<NaiveDate> = BTreeSet::new();
            for d in &covered {
                assert!(unique.insert(*d), "{d} counted twice in {year}-{month}");
            }
            assert_eq!(*covered.first().unwrap(), day(year, month, 1));
            assert_eq!(*covered.last().unwrap(), last_of_month(day(year, month, 1)));
            assert_eq!(covered.len(), unique.len());
        }
    }

    #[test]
    fn paths_follow_the_grain() {
        assert_eq!(
            Grain::Daily.path(day(2026, 7, 3)),
            PathBuf::from("daily/2026/07/03")
        );
        // A week is named for its first day, so a stub sits at the 1st.
        assert_eq!(
            week_of(day(2026, 8, 2)).path(),
            PathBuf::from("weekly/2026/08/01")
        );
        // A month needs no day.
        assert_eq!(
            month_of(day(2026, 7, 3)).path(),
            PathBuf::from("monthly/2026/07")
        );
    }
}
