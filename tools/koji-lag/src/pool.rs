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

/// An output form a report can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Format {
    /// Padded tables for a person, `report.txt`.
    Text,
    /// The whole report as one document, `report.json`.
    Json,
    /// One file per table, since a CSV holds one table.
    Csv,
}

impl Format {
    /// The default set when writing to a directory: what was written
    /// before a choice existed.
    pub fn written_by_default() -> Vec<Self> {
        vec![Self::Text, Self::Json]
    }

    /// Whether this form can go to stdout at all. CSV cannot: a report is
    /// several tables and a stream is one file.
    pub fn suits_stdout(self) -> bool {
        !matches!(self, Self::Csv)
    }

    /// The forms to write into a directory, given what was asked for.
    pub fn for_files(asked: &[Self]) -> Vec<Self> {
        if asked.is_empty() {
            return Self::written_by_default();
        }
        let mut formats = asked.to_vec();
        // Deduplicated and ordered, so a repeated flag is harmless rather
        // than writing a file twice, and the summary line reads the same
        // however the flags were given.
        formats.sort();
        formats.dedup();
        formats
    }

    /// The single form stdout may have, or why it cannot have one.
    ///
    /// `json` is the workspace-wide shorthand every tool here accepts, and
    /// means exactly `--format json`.
    pub fn for_stdout(asked: &[Self], json: bool) -> Result<Self, String> {
        if json {
            return Ok(Self::Json);
        }
        match asked {
            [] => Ok(Self::Text),
            [one] if one.suits_stdout() => Ok(*one),
            [Self::Csv] => {
                Err("--format csv writes one file per table; pass --out DIR".to_string())
            }
            _ => Err("--format takes one form when printing to stdout; \
                      pass --out DIR for several"
                .to_string()),
        }
    }
}

/// What a pooling run wrote.
// `Eq` is out because the trend carries f64s; nothing compares a Pooled
// for equality beyond field-by-field assertions in tests.
#[derive(Debug, Default, PartialEq)]
pub struct Pooled {
    /// Periods reported. Not derivable from the file count: a period is
    /// two files or nine depending on the forms asked for.
    pub periods: usize,
    pub written: Vec<PathBuf>,
    /// Periods whose reports were already on disk.
    pub present: usize,
    /// Periods the store cannot answer for yet.
    pub incomplete: usize,
    /// The cross-period comparison, over whatever monthly reports the tree
    /// now holds. Empty below two months.
    pub trend: crate::trend::Trend,
}

/// How to render, and what to do about reports already on disk.
pub struct PoolOpts {
    pub report: report::ReportOpts,
    pub min_samples: usize,
    /// The forms to write.
    pub formats: Vec<Format>,
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
        // Present in the forms asked for, not merely reported: asking for
        // CSV where only text and JSON exist is a reason to write, since
        // the period is "already reported" in a sense nobody asked about.
        let present = opts.formats.iter().all(|f| {
            dir.join(match f {
                Format::Text => "report.txt",
                Format::Json => "report.json",
                Format::Csv => "all-builds.csv",
            })
            .exists()
        });
        if !opts.force && present {
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
            .extend(write(&dir, &output, opts.min_samples, &opts.formats)?);
        pooled.periods += 1;
        if opts.verbose {
            eprintln!(
                "[koji-lag] reports: {} ({} build(s))",
                chunk.path().display(),
                dataset.builds.len()
            );
        }
    }
    // Last, and over the tree rather than over this run: the periods this
    // run skipped as already present are exactly the ones a trend most
    // wants, and they are on disk.
    let series = crate::trend::from_reports_root(reports_root)?;
    pooled.trend = crate::trend::assess(&series);
    if !pooled.trend.arches.is_empty() {
        let dir = reports_root.to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for (name, body) in [
            ("trend.txt", crate::trend::render(&pooled.trend)),
            (
                "trend.json",
                serde_json::to_string_pretty(&pooled.trend).map_err(|e| e.to_string())? + "\n",
            ),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
            pooled.written.push(path);
        }
    }
    Ok(pooled)
}

/// Write a report into `dir`: `report.txt` and `report.json` always, plus
/// a CSV per table when asked.
///
/// Both of the first two always, because a reader wants the table and a
/// machine wants the fields, and computing the report twice to get them
/// separately reads the store twice for the same answer.
///
/// The CSVs are per table rather than one file, since a CSV holds one
/// table — see [`report::csv_tables`]. They are opt-in because they are
/// seven more files per period, which is noise for anyone who does not
/// want them.
pub fn write(
    dir: &Path,
    output: &report::ReportOutput,
    min_samples: usize,
    formats: &[Format],
) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut written = Vec::new();
    let mut put = |path: PathBuf, body: String| -> Result<(), String> {
        std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path);
        Ok(())
    };
    for format in formats {
        match format {
            Format::Text => put(dir.join("report.txt"), report::render(output, min_samples))?,
            Format::Json => {
                let body = serde_json::to_string_pretty(output).map_err(|e| e.to_string())?;
                put(dir.join("report.json"), format!("{body}\n"))?;
            }
            Format::Csv => {
                for (name, body) in report::csv_tables(output) {
                    put(dir.join(name), body)?;
                }
            }
        }
    }
    Ok(written)
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
            formats: Format::written_by_default(),
            force: false,
            verbose: false,
        }
    }

    #[test]
    fn a_directory_gets_what_was_asked_for_or_the_old_pair() {
        // Nothing asked: what the tool wrote before a choice existed, so
        // an existing invocation keeps producing existing files.
        assert_eq!(Format::for_files(&[]), vec![Format::Text, Format::Json]);
        assert_eq!(Format::for_files(&[Format::Json]), vec![Format::Json]);
        // A repeated form writes one file, not two.
        assert_eq!(
            Format::for_files(&[Format::Csv, Format::Text, Format::Csv]),
            vec![Format::Text, Format::Csv]
        );
    }

    #[test]
    fn stdout_takes_one_form_and_says_so_when_it_cannot() {
        assert_eq!(Format::for_stdout(&[], false), Ok(Format::Text));
        // --json is the conventional shorthand and keeps working.
        assert_eq!(Format::for_stdout(&[], true), Ok(Format::Json));
        assert_eq!(Format::for_stdout(&[Format::Json], false), Ok(Format::Json));
        // A stream is one file, so CSV needs somewhere to put its tables,
        // and two forms have no order to be printed in.
        let csv = Format::for_stdout(&[Format::Csv], false).unwrap_err();
        assert!(csv.contains("--out"), "{csv}");
        let both = Format::for_stdout(&[Format::Text, Format::Json], false).unwrap_err();
        assert!(both.contains("one form"), "{both}");
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
        assert_eq!(pooled.periods, 1);
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
    fn csv_is_written_per_table_and_carries_the_period() {
        let d = day(2026, 8, 12);
        let store = store_with(&[d]);
        let root = tempfile::tempdir().unwrap();
        let opts = PoolOpts {
            formats: vec![Format::Text, Format::Json, Format::Csv],
            ..opts()
        };
        run(&store, "fedora", root.path(), &[d], GRACE, &opts).unwrap();

        let dir = root.path().join("daily/2026/08/12");
        for name in [
            "all-builds.csv",
            "srpm-rebuild.csv",
            "multi-arch.csv",
            "single-arch.csv",
            "noarch-by-host.csv",
        ] {
            assert!(dir.join(name).exists(), "{name} missing");
        }
        // The text and JSON forms are still written: CSV adds, it does not
        // replace.
        assert!(dir.join("report.txt").exists());
        assert!(dir.join("report.json").exists());

        let all = std::fs::read_to_string(dir.join("all-builds.csv")).unwrap();
        let mut lines = all.lines();
        assert!(
            lines
                .next()
                .unwrap()
                .starts_with("instance,period_start,period_end,arch,"),
            "{all}"
        );
        // Every row names its period, so a year of dailies concatenates
        // into something that still knows which day each row is from.
        let first = lines.next().expect("a row for the build's arch");
        assert!(
            first.starts_with("fedora,2026-08-12,2026-08-13,"),
            "{first}"
        );
        // Seconds, not "2.6m": a column mixing units cannot be summed.
        assert!(!first.contains('m'), "{first}");
    }

    #[test]
    fn asking_for_csv_where_only_text_exists_writes_it() {
        // Otherwise a tree reported before --csv existed could never gain
        // the CSVs without --force, and "already present" would be true in
        // a sense nobody asked about.
        let d = day(2026, 8, 12);
        let store = store_with(&[d]);
        let root = tempfile::tempdir().unwrap();
        run(&store, "fedora", root.path(), &[d], GRACE, &opts()).unwrap();
        assert!(!root.path().join("daily/2026/08/12/all-builds.csv").exists());

        let with_csv = PoolOpts {
            formats: vec![Format::Text, Format::Json, Format::Csv],
            ..opts()
        };
        let again = run(&store, "fedora", root.path(), &[d], GRACE, &with_csv).unwrap();
        assert!(!again.written.is_empty(), "should have written the CSVs");
        assert!(root.path().join("daily/2026/08/12/all-builds.csv").exists());
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
