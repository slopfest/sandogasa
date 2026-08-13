// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `backfill` subcommand: sweep a long window a day at a time,
//! collating finished days into weeks and weeks into months.
//!
//! Why a day at a time, when one wide window sweeps in a single pass?
//! Because the per-parent children queries dominate a sweep — they scale
//! with the number of builds, not with the number of windows — so days
//! cost the same in total, and paying per day means a run interrupted
//! after five hours has lost the day in flight rather than the lot.
//! Cheap seeking is what makes it affordable: each day starts below the
//! oldest build of the day already fetched, so no day walks history the
//! previous one already crossed.
//!
//! Raw data is collated and the finer files removed, because a month of
//! Fedora is hundreds of megabytes and the weekly file holds everything
//! its dailies did. Reports are collated and the finer files *kept*:
//! they are kilobytes, and a daily report answers questions a monthly
//! one cannot.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{Datelike, Duration, NaiveDate};

use crate::dataset::Dataset;

/// What to do when a chunk's file is already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Existing {
    /// Sweep again and fold the result into what is there.
    Merge,
    /// Sweep again and write over it.
    Replace,
    /// Ask, and merge when nobody can be asked.
    Ask,
}

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

/// Whether every day of `chunk` has a dataset under `root`.
pub fn complete(root: &Path, chunk: &Chunk, file: &str, grain: Grain) -> bool {
    let present: BTreeSet<NaiveDate> = chunk
        .days()
        .filter(|day| root.join(grain.path(*day)).join(file).exists())
        .collect();
    present.len() == chunk.days().count()
}

/// Fold the parts of `chunk` into one dataset file, then remove them.
///
/// The merge is written before anything is deleted: an interruption then
/// leaves both, which merges to the same thing next time, rather than
/// leaving neither.
pub fn collate(
    root: &Path,
    chunk: &Chunk,
    from: Grain,
    parts: &[PathBuf],
    file: &str,
) -> Result<usize, String> {
    let mut merged = Dataset::new();
    let mut found = 0usize;
    for part in parts {
        let path = root.join(part).join(file);
        if !path.exists() {
            continue;
        }
        merged.merge(Dataset::load(&path)?);
        found += 1;
    }
    if found == 0 {
        return Ok(0);
    }
    let target = root.join(chunk.path());
    std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    let out = target.join(file);
    merged.save(&out)?;

    for part in parts {
        let path = root.join(part).join(file);
        if path.exists() && path != out {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            // The dated directories exist only to hold these files, so an
            // empty one left behind reads as coverage that is not there.
            let mut dir = path.parent().map(Path::to_path_buf);
            while let Some(candidate) = dir {
                if candidate == root || std::fs::remove_dir(&candidate).is_err() {
                    break;
                }
                dir = candidate.parent().map(Path::to_path_buf);
            }
        }
    }
    let _ = from;
    Ok(found)
}

#[cfg(test)]
mod tests {

    /// A dataset holding one build, so merges can be told apart.
    fn one_build(id: i64) -> Dataset {
        let mut ds = Dataset::new();
        ds.builds.insert(
            format!("fedora:{id}"),
            crate::dataset::BuildRecord {
                instance: "fedora".to_string(),
                task_id: id,
                package: None,
                nvr: None,
                target: None,
                owner: None,
                scratch: false,
                state: 2,
                create_ts: 0.0,
                start_ts: None,
                completion_ts: None,
                priority: None,
                host_id: None,
            },
        );
        ds
    }

    #[test]
    fn collating_keeps_everything_and_removes_the_parts() {
        let root = tempfile::tempdir().unwrap();
        let file = "fedora.json";
        // Two days of a week, one build each.
        let week = week_of(day(2026, 8, 12));
        let days: Vec<NaiveDate> = week.days().take(2).collect();
        for (n, d) in days.iter().enumerate() {
            let dir = root.path().join(Grain::Daily.path(*d));
            std::fs::create_dir_all(&dir).unwrap();
            one_build(n as i64 + 1).save(&dir.join(file)).unwrap();
        }
        // Not every day of the week is there yet.
        assert!(!complete(root.path(), &week, file, Grain::Daily));

        let parts: Vec<PathBuf> = days.iter().map(|d| Grain::Daily.path(*d)).collect();
        let folded = collate(root.path(), &week, Grain::Daily, &parts, file).unwrap();
        assert_eq!(folded, 2);

        // The week holds both builds...
        let merged = Dataset::load(&root.path().join(week.path()).join(file)).unwrap();
        assert_eq!(merged.builds.len(), 2);
        // ...and the dailies are gone, along with the directories that
        // existed only to hold them.
        for d in &days {
            let dir = root.path().join(Grain::Daily.path(*d));
            assert!(!dir.join(file).exists(), "{d} still has a dataset");
            assert!(!dir.exists(), "{d} left an empty directory behind");
        }
    }

    #[test]
    fn collating_nothing_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let week = week_of(day(2026, 8, 12));
        let parts: Vec<PathBuf> = week.days().map(|d| Grain::Daily.path(d)).collect();
        assert_eq!(
            collate(root.path(), &week, Grain::Daily, &parts, "fedora.json").unwrap(),
            0
        );
        assert!(!root.path().join(week.path()).exists());
    }
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
