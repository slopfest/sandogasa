// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Writing reports at every grain the store covers.
//!
//! A period's report is written once the store can answer for it in full,
//! and every grain is kept: a week's report is written *beside* its
//! dailies, not instead of them. Reports are kilobytes, and a daily
//! report answers questions a monthly one has already averaged away.
//!
//! Each grain is computed from the raw rows for that period, never
//! combined from the finer ones — see DEVELOPMENT.md on percentiles not
//! composing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate};

use crate::periods::{Chunk, Grain, month_of, week_of};
use crate::report;
use crate::store::{Span, Store};

/// What a pooling run wrote.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pooled {
    pub written: Vec<PathBuf>,
    /// Periods whose reports were already on disk.
    pub present: usize,
    /// Periods the store cannot answer for yet.
    pub incomplete: usize,
}

/// How to render, and what to do about reports already on disk.
pub struct PoolOpts {
    pub report: report::ReportOpts,
    pub min_samples: usize,
    /// Rewrite reports that are already there. Worth it after the store
    /// gains rows for a period — a day listed by one run and given its
    /// children by a later one has a report worth recomputing.
    pub force: bool,
    pub verbose: bool,
}

/// The UTC seconds a period spans, `[from, to)`.
pub fn bounds(chunk: &Chunk) -> (f64, f64) {
    let midnight = |d: NaiveDate| {
        d.and_hms_opt(0, 0, 0)
            .expect("midnight exists")
            .and_utc()
            .timestamp() as f64
    };
    (midnight(chunk.start), midnight(chunk.end) + 86_400.0)
}

/// Whether the store can answer for `chunk` in full.
///
/// Two conditions, and both are needed. The creation span the period
/// depends on must have been listed — that reaches `grace` before the
/// period, since a build created earlier can finish inside it — and every
/// build completing in the period must have had its children fetched. A
/// period passing only the first would report builds with no arch tasks
/// as though they had none, which reads as a build that took no time at
/// all rather than as data that has not arrived.
pub fn covered(store: &Store, instance: &str, chunk: &Chunk, grace: f64) -> Result<bool, String> {
    let (from, to) = bounds(chunk);
    // Whole means every day of it is whole, which is the same question the
    // store answers for a report or an export. Asking it here rather than
    // repeating the rule keeps the three from ever disagreeing.
    Ok(store.whole_days(instance, from, to, grace)? == [Span { from, to }])
}

/// Write every report the store can answer for over `days`.
///
/// The days are candidates, not a promise: each day, each week those days
/// touch and each month they touch is written only if [`covered`] says
/// the store holds it whole. So a sync of one more day can bring a week
/// and even a month into range, and pooling is just running this again.
pub fn run(
    store: &Store,
    instance: &str,
    reports_root: &Path,
    days: &[NaiveDate],
    grace: f64,
    opts: &PoolOpts,
) -> Result<Pooled, String> {
    let mut chunks: Vec<Chunk> = days
        .iter()
        .map(|d| Chunk {
            grain: Grain::Daily,
            start: *d,
            end: *d,
        })
        .collect();
    // Coarser grains, deduplicated: a month's worth of days asks about
    // the same month thirty times otherwise.
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for day in days {
        for chunk in [week_of(*day), month_of(*day)] {
            if seen.insert(chunk.path()) {
                chunks.push(chunk);
            }
        }
    }

    let mut pooled = Pooled::default();
    for chunk in &chunks {
        let dir = reports_root.join(chunk.path());
        if !opts.force && dir.join("report.txt").exists() && dir.join("report.json").exists() {
            pooled.present += 1;
            continue;
        }
        if !covered(store, instance, chunk, grace)? {
            pooled.incomplete += 1;
            if opts.verbose {
                eprintln!(
                    "[koji-lag] reports: {} not yet complete in the store",
                    chunk.path().display()
                );
            }
            continue;
        }
        let (from, to) = bounds(chunk);
        let dataset = store.dataset_for(instance, from, to, grace)?;
        let output = report::run(
            &dataset,
            &report::ReportOpts {
                period: Some((from, to)),
                ..opts.report.clone()
            },
        );
        pooled
            .written
            .extend(write(&dir, &output, opts.min_samples)?);
        if opts.verbose {
            eprintln!(
                "[koji-lag] reports: {} ({} build(s))",
                chunk.path().display(),
                dataset.builds.len()
            );
        }
    }
    Ok(pooled)
}

/// Write both forms of a report into `dir`.
///
/// Both, always: a reader wants the table and a machine wants the fields,
/// and computing the report twice to get them separately reads the store
/// twice for the same answer.
pub fn write(
    dir: &Path,
    output: &report::ReportOutput,
    min_samples: usize,
) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let text = dir.join("report.txt");
    let json = dir.join("report.json");
    std::fs::write(&text, report::render(output, min_samples))
        .map_err(|e| format!("{}: {e}", text.display()))?;
    let body = serde_json::to_string_pretty(output).map_err(|e| e.to_string())?;
    std::fs::write(&json, format!("{body}\n")).map_err(|e| format!("{}: {e}", json.display()))?;
    Ok(vec![text, json])
}

/// Every whole UTC day the store has listed anything for, so a pooling
/// run with no dates given still knows what to consider.
///
/// Deliberately generous: these are candidates, and [`covered`] settles
/// each one. Being generous costs a coverage query per day and being
/// stingy costs a report nobody notices is missing.
pub fn days_in_store(store: &Store, instance: &str) -> Result<Vec<NaiveDate>, String> {
    let spans = store.listed(instance)?;
    let mut days: BTreeSet<NaiveDate> = BTreeSet::new();
    for span in spans {
        let mut ts = (span.from / 86_400.0).floor() * 86_400.0;
        while ts < span.to {
            if let Some(day) = DateTime::from_timestamp(ts as i64, 0) {
                days.insert(day.date_naive());
            }
            ts += 86_400.0;
        }
    }
    Ok(days.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::{BuildRecord, TaskRecord};
    use crate::store::CHILDREN_GEN;

    const GRACE: f64 = 3.0 * 86_400.0;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn midnight(d: NaiveDate) -> f64 {
        d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() as f64
    }

    fn build(id: i64, completion: f64) -> BuildRecord {
        BuildRecord {
            instance: "fedora".into(),
            task_id: id,
            package: Some("foo".into()),
            nvr: Some("foo-1-1.fc46".into()),
            target: None,
            owner: Some("alice".into()),
            scratch: false,
            state: 2,
            create_ts: completion - 600.0,
            start_ts: Some(completion - 590.0),
            completion_ts: Some(completion),
            priority: None,
            host_id: Some(1),
        }
    }

    fn child(id: i64, parent: i64, completion: f64) -> TaskRecord {
        TaskRecord {
            instance: "fedora".into(),
            task_id: id,
            parent: Some(parent),
            method: crate::dataset::BUILD_ARCH.into(),
            arch: "x86_64".into(),
            package: Some("foo".into()),
            state: 2,
            create_ts: completion - 500.0,
            start_ts: Some(completion - 400.0),
            completion_ts: Some(completion),
            host_id: Some(1),
            channel_id: Some(1),
            weight: None,
        }
    }

    /// A store holding one finished build on each of `days`, listed with
    /// its margin so those days are answerable.
    fn store_with(days: &[NaiveDate]) -> Store {
        let mut store = Store::in_memory().unwrap();
        for (n, d) in days.iter().enumerate() {
            let noon = midnight(*d) + 43_200.0;
            let id = 1000 + n as i64;
            store.put_builds("fedora", &[build(id, noon)]).unwrap();
            store
                .put_tasks("fedora", &[child(id * 10, id, noon)])
                .unwrap();
            store
                .mark_children_swept("fedora", &[id], CHILDREN_GEN)
                .unwrap();
            store
                .add_listed(
                    "fedora",
                    Span {
                        from: midnight(*d) - GRACE,
                        to: midnight(*d) + 86_400.0,
                    },
                )
                .unwrap();
        }
        store
    }

    fn opts() -> PoolOpts {
        PoolOpts {
            report: report::ReportOpts::default(),
            min_samples: 1,
            force: false,
            verbose: false,
        }
    }

    #[test]
    fn a_covered_day_is_reported_and_its_unfinished_week_is_not() {
        let d = day(2026, 8, 12); // A Wednesday.
        let store = store_with(&[d]);
        let root = tempfile::tempdir().unwrap();
        let pooled = run(&store, "fedora", root.path(), &[d], GRACE, &opts()).unwrap();

        assert!(
            root.path().join("daily/2026/08/12/report.txt").exists(),
            "the day the store holds must be reported"
        );
        // Its week and month are not there yet, and saying so is the
        // point: a weekly report from one day would be a lie about the
        // week rather than a report of what is known.
        assert!(!root.path().join("weekly/2026/08/10").exists());
        assert!(!root.path().join("monthly/2026/08").exists());
        assert_eq!(pooled.incomplete, 2);
        assert_eq!(pooled.written.len(), 2, "text and json");
    }

    #[test]
    fn a_week_completed_by_the_last_day_is_written_beside_its_dailies() {
        // The whole of the week 2026-08-10 to 08-16.
        let week: Vec<NaiveDate> = (10..=16).map(|d| day(2026, 8, d)).collect();
        let store = store_with(&week);
        let root = tempfile::tempdir().unwrap();
        run(&store, "fedora", root.path(), &week, GRACE, &opts()).unwrap();

        assert!(root.path().join("weekly/2026/08/10/report.txt").exists());
        // Every daily survives: the coarser report does not replace them.
        for d in 10..=16 {
            assert!(
                root.path()
                    .join(format!("daily/2026/08/{d:02}/report.txt"))
                    .exists(),
                "daily for the {d}th was lost"
            );
        }
        // August is still incomplete, so no monthly.
        assert!(!root.path().join("monthly/2026/08").exists());
    }

    #[test]
    fn a_day_whose_children_are_missing_is_not_reported() {
        let d = day(2026, 8, 12);
        let mut store = Store::in_memory().unwrap();
        let noon = midnight(d) + 43_200.0;
        store.put_builds("fedora", &[build(1, noon)]).unwrap();
        store
            .add_listed(
                "fedora",
                Span {
                    from: midnight(d) - GRACE,
                    to: midnight(d) + 86_400.0,
                },
            )
            .unwrap();
        // Listed, but nobody has asked for this build's arch tasks. A
        // report now would show a build with no work in it.
        let root = tempfile::tempdir().unwrap();
        let pooled = run(&store, "fedora", root.path(), &[d], GRACE, &opts()).unwrap();
        assert!(pooled.written.is_empty());
        assert!(!root.path().join("daily/2026/08/12").exists());
    }

    #[test]
    fn a_day_listed_without_its_margin_is_not_reported() {
        // Coverage that starts at the day itself: builds created the day
        // before and finishing in this one were never enumerated.
        let d = day(2026, 8, 12);
        let mut store = Store::in_memory().unwrap();
        let noon = midnight(d) + 43_200.0;
        store.put_builds("fedora", &[build(1, noon)]).unwrap();
        store.put_tasks("fedora", &[child(10, 1, noon)]).unwrap();
        store
            .mark_children_swept("fedora", &[1], CHILDREN_GEN)
            .unwrap();
        store
            .add_listed(
                "fedora",
                Span {
                    from: midnight(d),
                    to: midnight(d) + 86_400.0,
                },
            )
            .unwrap();
        let root = tempfile::tempdir().unwrap();
        let pooled = run(&store, "fedora", root.path(), &[d], GRACE, &opts()).unwrap();
        assert_eq!(pooled.incomplete, 3, "day, week and month all unanswerable");
        assert!(pooled.written.is_empty());
    }

    #[test]
    fn reports_already_there_are_left_alone_unless_forced() {
        let d = day(2026, 8, 12);
        let store = store_with(&[d]);
        let root = tempfile::tempdir().unwrap();
        run(&store, "fedora", root.path(), &[d], GRACE, &opts()).unwrap();

        let again = run(&store, "fedora", root.path(), &[d], GRACE, &opts()).unwrap();
        assert!(again.written.is_empty());
        assert_eq!(again.present, 1);

        let forced = PoolOpts {
            force: true,
            ..opts()
        };
        let rewritten = run(&store, "fedora", root.path(), &[d], GRACE, &forced).unwrap();
        assert_eq!(rewritten.written.len(), 2);
    }

    #[test]
    fn the_days_a_store_holds_come_from_what_it_listed() {
        let days: Vec<NaiveDate> = (10..=12).map(|d| day(2026, 8, d)).collect();
        let store = store_with(&days);
        let found = days_in_store(&store, "fedora").unwrap();
        // The listed spans reach back three days for the margin, so the
        // candidates run from the 7th: generous by design, and each one
        // still has to pass `covered`.
        assert_eq!(*found.first().unwrap(), day(2026, 8, 7));
        assert_eq!(*found.last().unwrap(), day(2026, 8, 12));
    }
}
