// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Metrics that only mean anything across periods.
//!
//! A [`crate::report`] measures one window, so it can only state
//! within-window facts. Some of the most useful things about a build
//! system are not facts about a window at all — whether a build costs more
//! than it used to, and whether the fleet is filling up — and asking a
//! single report for them yields a number that looks like an answer and is
//! not. Package mix is the trap: on s390x in July 2026 the control
//! population averaged 3.8 minutes against 8.9 for everything else, which
//! reads as a 2.3x penalty and is only the observation that Rust crates
//! are small.
//!
//! Comparing each family with *its own* earlier self cancels the mix. Two
//! families are never divided by each other: what that would measure is
//! mostly how big their packages are.
//!
//! Reading the resulting rows is the analysis, and it is not hard. Fedora
//! between F42 and F45, per rebuild and noarch excluded:
//!
//! | arch | rust | other |
//! |---|---|---|
//! | s390x | 1.25x | 1.49x |
//! | ppc64le | 0.73x | 0.99x |
//! | x86_64 | 0.68x | 0.82x |
//!
//! Every architecture's C and C++ work fared worse than its Rust work, which
//! is a toolchain cost and cannot be a platform fault. Only s390x got slower
//! at all, which is a platform regression and cannot be a compiler flag. The
//! two conclusions come from two different comparisons in the table, neither
//! of which needs the families divided by one another.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::health::Health;
use crate::periods::Grain;

/// Drift beyond which a population's build time is worth a warning.
///
/// Set above the measured noise floor rather than at a round number.
/// Comparing each population with its own earlier self cancels the
/// *difference* between populations but not the drift in either one's
/// composition, and a month is not enough time for that to settle: over
/// March to July 2026, with no regression anybody knows of, the six
/// architectures' medians moved between 0.63x and 1.07x, so a threshold
/// anywhere near 1.25 reports what people happened to build that month.
///
/// The comparison that does not have this problem is one mass rebuild
/// against the next, since a rebuild builds nearly everything and its mix
/// is therefore roughly fixed. [`crate::events`] already identifies those
/// windows; a trend restricted to them could be tightened considerably
/// below this.
pub const DRIFT_WARN: f64 = 1.5;

/// Drift threshold for a comparison of like windows.
///
/// A mass rebuild builds nearly everything, so its mix is roughly fixed and
/// the noise a calendar month carries is largely gone: s390x's control
/// population steps 1.30x, 1.17x and 0.99x across F42 to F45, so a
/// rebuild-to-rebuild comparison is stable to about 15% against the 40% an
/// unrestricted month moves by. Which is why the comparison worth
/// automating is this one, and why [`crate::events`] rather than the monthly
/// tree supplies its periods -- it already knows when the rebuilds were, and
/// deriving that a second time would be deriving it differently.
pub const REBUILD_DRIFT_WARN: f64 = 1.25;

/// How far a population may change size before its ratio stops meaning
/// anything.
///
/// A ratio between two periods answers "did this get more expensive" only if
/// the thing measured is the same thing at both ends, and the population
/// count is where that shows. Twice this tool has reported a large drift that
/// was a population change: `buildSRPMFromSCM` tasks leaving s390x, and then
/// noarch builds leaving it. Both were visible in the counts printed beside
/// the ratio and neither was caught by looking at the ratio.
///
/// So the counts are now checked rather than merely printed. 1.5x either way
/// is loose on purpose — Fedora gains and loses packages, and an
/// architecture's share of a rebuild moves legitimately — but it catches the
/// cases that have actually misled: golang on s390x went from 1,419 builds
/// to 430 between F42 and F45, which is not a population that can be
/// compared with itself.
pub const POPULATION_SHIFT: f64 = 1.5;

/// Utilisation above which queueing stops being linear in load.
pub const UTIL_WARN: f64 = 0.80;

/// One architecture, first period against last.
#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct ArchTrend {
    pub arch: String,
    /// The periods compared, as the labels they were reported under.
    pub from: String,
    pub to: String,
    /// Median build time in the two periods, per population, and the
    /// ratio between them. Keyed by population name.
    pub drift: BTreeMap<String, Drift>,
    pub utilisation_from: Option<f64>,
    pub utilisation_to: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, schemars::JsonSchema)]
pub struct Drift {
    pub from_secs: Option<f64>,
    pub to_secs: Option<f64>,
    pub ratio: Option<f64>,
    pub tasks_from: usize,
    pub tasks_to: usize,
}

impl Drift {
    /// How much the population changed size, as a factor at least 1.0
    /// whichever direction it moved. `None` when either end is empty.
    pub fn population_shift(&self) -> Option<f64> {
        let (a, b) = (self.tasks_from as f64, self.tasks_to as f64);
        (a > 0.0 && b > 0.0).then(|| if a > b { a / b } else { b / a })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, schemars::JsonSchema)]
pub struct Trend {
    pub arches: Vec<ArchTrend>,
    pub warnings: Vec<String>,
    /// The threshold these warnings were raised against, so a reader of the
    /// file knows which comparison it is and does not have to guess from
    /// the labels.
    pub drift_warn: f64,
}

/// The monthly series a reports tree already holds, oldest first.
///
/// Read off disk rather than recomputed, for two reasons. A period whose
/// reports are already written is skipped by [`crate::pool::run`], so
/// recomputing would give a trend only over whatever happened to be
/// rebuilt this run; and the whole point of writing `report.json` is that
/// it is the answer, so asking the store again would be asking twice.
///
/// Monthly because the question is whether a build costs more than it did
/// a release ago. A daily series answers a different question loudly: the
/// first and last days of a fourteen-month range differ by which packages
/// happened to be built on them.
///
/// Periods with no health section are skipped, not an error — reports
/// written before health existed are still perfectly good reports.
pub fn from_reports_root(root: &Path) -> Result<Vec<(String, Health)>, String> {
    #[derive(serde::Deserialize)]
    struct Partial {
        health: Option<Health>,
    }
    let base = root.join(Grain::Monthly.dir());
    let mut out = Vec::new();
    // year/month, both zero-padded, so lexical order is chronological.
    let mut years: Vec<_> = read_dir(&base)?;
    years.sort();
    for year in years {
        let mut months = read_dir(&year)?;
        months.sort();
        for month in months {
            let file = month.join("report.json");
            let Ok(body) = std::fs::read_to_string(&file) else {
                continue;
            };
            let parsed: Partial =
                serde_json::from_str(&body).map_err(|e| format!("{}: {e}", file.display()))?;
            let Some(health) = parsed.health else {
                continue;
            };
            let label = format!(
                "{}-{}",
                year.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
                month.file_name().and_then(|s| s.to_str()).unwrap_or("?")
            );
            out.push((label, health));
        }
    }
    Ok(out)
}

fn read_dir(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    match std::fs::read_dir(dir) {
        Ok(entries) => Ok(entries.flatten().map(|e| e.path()).collect()),
        // A tree with no monthly reports yet is a series of none.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

/// Label one event's window for a trend row.
///
/// The release when a schedule named one, since "F42 -> F45" is what the
/// comparison is actually about; otherwise the date the window opened.
pub fn label_of(event: &crate::events::Event) -> String {
    match event.release {
        Some(r) => format!("F{r}"),
        None => crate::events::day_name(event.from),
    }
}

/// Write a trend beside a tree, as `<stem>.txt` and `<stem>.json`.
///
/// The stem is the caller's because the two trends are not the same
/// measurement and must not share a filename. `events` compares one mass
/// rebuild with the next, where the package mix is roughly fixed and the
/// threshold can be tight; `reports` compares two calendar months, where it
/// cannot. Both are worth having and both are legitimately produced into
/// the same repository -- which is exactly why one of them being called
/// `trend.txt` and the other also being called `trend.txt` was a mistake.
pub fn write(root: &Path, stem: &str, trend: &Trend) -> Result<Vec<std::path::PathBuf>, String> {
    if trend.arches.is_empty() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut written = Vec::new();
    for (name, body) in [
        (format!("{stem}.txt"), render(trend)),
        (
            format!("{stem}.json"),
            serde_json::to_string_pretty(trend).map_err(|e| e.to_string())? + "\n",
        ),
    ] {
        let path = root.join(name);
        std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

/// Compare the first and last period in a series.
///
/// The series is `(label, health)` in the order the periods run. Fewer than
/// two periods yields an empty trend rather than an error: a series of one
/// is a legitimate thing to have, and there is simply nothing to compare.
///
/// First-against-last, not a fitted slope, because the question is "does
/// this cost more than it did" and the answer should be arithmetic a reader
/// can check against two reports they already have.
pub fn assess(series: &[(String, Health)], drift_warn: f64) -> Trend {
    let mut trend = Trend {
        drift_warn,
        ..Default::default()
    };
    let (Some((from_label, first)), Some((to_label, last))) = (series.first(), series.last())
    else {
        return trend;
    };
    if series.len() < 2 {
        return trend;
    }
    for a in &last.arches {
        let Some(b) = first.arches.iter().find(|x| x.arch == a.arch) else {
            continue;
        };
        let mut drift = BTreeMap::new();
        for pop in &a.service {
            let was = b.service.iter().find(|x| x.name == pop.name);
            let from_secs = was.and_then(|w| w.p50);
            drift.insert(
                pop.name.clone(),
                Drift {
                    from_secs,
                    to_secs: pop.p50,
                    ratio: match (from_secs, pop.p50) {
                        (Some(f), Some(t)) if f > 0.0 => Some(t / f),
                        _ => None,
                    },
                    tasks_from: was.map_or(0, |w| w.tasks),
                    tasks_to: pop.tasks,
                },
            );
        }
        let entry = ArchTrend {
            arch: a.arch.clone(),
            from: from_label.clone(),
            to: to_label.clone(),
            drift,
            utilisation_from: b.utilisation,
            utilisation_to: a.utilisation,
        };
        trend.warnings.extend(warn(&entry, drift_warn));
        trend.arches.push(entry);
    }
    trend
}

/// What crossed a threshold, phrased so it can be pasted into a ticket.
fn warn(t: &ArchTrend, drift_warn: f64) -> Vec<String> {
    let mut out = Vec::new();
    let mins = |s: Option<f64>| s.map_or("?".to_string(), |x| format!("{:.2}m", x / 60.0));
    for (name, d) in &t.drift {
        // Said instead of the drift, not beside it: a ratio over a
        // population that changed size is not a smaller finding, it is a
        // different measurement, and reporting both invites the reader to
        // believe the one they recognise.
        if let Some(shift) = d.population_shift()
            && shift >= POPULATION_SHIFT
        {
            out.push(format!(
                "{}: {} population changed {:.2}x between {} and {} ({} -> {} \
                 builds), so its {:.2}x build-time ratio compares two \
                 different populations and is not a drift",
                t.arch,
                name,
                shift,
                t.from,
                t.to,
                d.tasks_from,
                d.tasks_to,
                d.ratio.unwrap_or_default(),
            ));
            continue;
        }
        if d.ratio.is_some_and(|r| r >= drift_warn) {
            out.push(format!(
                "{}: {} build time {:.2}x since {} ({} -> {}); \
                 {} and {} builds compared",
                t.arch,
                name,
                d.ratio.unwrap_or_default(),
                t.from,
                mins(d.from_secs),
                mins(d.to_secs),
                d.tasks_from,
                d.tasks_to,
            ));
        }
    }
    if let (Some(f), Some(to)) = (t.utilisation_from, t.utilisation_to)
        && to >= UTIL_WARN
        && to > f
    {
        out.push(format!(
            "{}: utilisation rose {f:.2} -> {to:.2}, past {UTIL_WARN:.2} \
             where queueing stops being linear in load",
            t.arch
        ));
    }
    out
}

/// Render for a reader.
pub fn render(t: &Trend) -> String {
    if t.arches.is_empty() {
        return String::new();
    }
    let mut s = String::from("\nTrend across periods\n");
    let (from, to) = (&t.arches[0].from, &t.arches[0].to);
    s.push_str(&format!("  {from} -> {to}\n\n"));
    s.push_str(
        "  arch      toolchain    median then    median now   ratio\n  \
         --------  -----------  ------------  ------------  ------\n",
    );
    let mins = |x: Option<f64>| x.map_or("-".to_string(), |v| format!("{:.2}m", v / 60.0));
    for a in &t.arches {
        for (name, d) in &a.drift {
            s.push_str(&format!(
                "  {:<8}  {:<11}  {:>12}  {:>12}  {:>6}\n",
                a.arch,
                name,
                mins(d.from_secs),
                mins(d.to_secs),
                d.ratio.map_or("-".to_string(), |r| format!("{r:.2}x"))
            ));
        }
        if let (Some(f), Some(t2)) = (a.utilisation_from, a.utilisation_to) {
            s.push_str(&format!(
                "  {:<8}  utilisation  {:>12}  {:>12}\n",
                a.arch,
                format!("{f:.2}"),
                format!("{t2:.2}")
            ));
        }
    }
    s.push_str(&format!(
        "\n  Legend\n  \
         - each row is one build toolchain, guessed from the package name. \
         `other` is\n    what the prefixes do not name, mostly C and C++.\n  \
         - `ratio` -- median build time now over median build time then, for \
         that\n    family against *its own* earlier self. Families are \
         never compared with\n    each other: the gap between two of them \
         in one window is mostly how big\n    their packages are, not what \
         they cost.\n  \
         - `utilisation` -- offered task weight over enabled builder weight, \
         at each\n    end of the range. Queueing is nonlinear in it.\n\n  \
         Noarch builds are excluded throughout. Koji records their single \
         task\n  against whichever host it picked, so they are no \
         architecture's compile\n  cost -- s390x hosted 2,291 of them in \
         F42's rebuild and 5 in F45's.\n\n  \
         Below about {:.2}x a ratio is not distinguishable from what people \
         happened\n  to build in that period, which is why the periods \
         compared matter as much as\n  the numbers: two mass rebuilds build \
         nearly the same set of packages, two\n  calendar months need not.\n",
        t.drift_warn
    ));
    if !t.warnings.is_empty() {
        s.push_str("\n  Warnings\n");
        for w in &t.warnings {
            s.push_str(&format!("  - {w}\n"));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::{ArchHealth, Population};

    fn arch(name: &str, util: f64, control: f64, rest: f64) -> ArchHealth {
        ArchHealth {
            arch: name.to_string(),
            capacity: Some(100.0),
            offered_weight: Some(util * 100.0),
            utilisation: Some(util),
            tasks: 1000,
            builder_hours: 0.0,
            wasted_pct: 0.0,
            tail_pct: 0.0,
            tail_tasks: 0,
            pool: Some(name.to_string()),
            pool_capacity: Some(100.0),
            pool_offered: Some(util * 100.0),
            submitters: 0,
            submitters_slow: 0,
            submitters_slow_task_pct: 0.0,
            service: vec![
                Population {
                    name: "rest".to_string(),
                    tasks: 500,
                    p50: Some(rest),
                    p90: Some(rest * 3.0),
                },
                Population {
                    name: "control".to_string(),
                    tasks: 500,
                    p50: Some(control),
                    p90: Some(control * 3.0),
                },
            ],
        }
    }

    fn assess_default(series: &[(String, Health)]) -> Trend {
        assess(series, DRIFT_WARN)
    }

    fn series(a: ArchHealth, b: ArchHealth) -> Vec<(String, Health)> {
        vec![
            (
                "2025-01".to_string(),
                Health {
                    arches: vec![a],
                    ..Default::default()
                },
            ),
            (
                "2026-01".to_string(),
                Health {
                    arches: vec![b],
                    ..Default::default()
                },
            ),
        ]
    }

    #[test]
    fn the_monthly_series_comes_off_disk_in_chronological_order() {
        let root = tempfile::tempdir().unwrap();
        let put = |year: &str, month: &str, body: String| {
            let dir = root.path().join("monthly").join(year).join(month);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("report.json"), body).unwrap();
        };
        let with_health = |control: f64| {
            serde_json::to_string(&serde_json::json!({
                "health": &Health {
                    arches: vec![arch("s390x", 0.5, control, control)],
                    ..Default::default()
                }
            }))
            .unwrap()
        };
        // Written out of order, and spanning a year boundary.
        put("2026", "01", with_health(200.0));
        put("2025", "03", with_health(100.0));
        put("2025", "11", with_health(150.0));
        // A report from before health existed is skipped, not an error.
        put("2025", "12", "{}".to_string());
        // So is a directory with no report at all.
        std::fs::create_dir_all(root.path().join("monthly/2025/06")).unwrap();

        let series = from_reports_root(root.path()).unwrap();
        let labels: Vec<&str> = series.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, ["2025-03", "2025-11", "2026-01"]);

        // Which makes the comparison first-against-last across the range,
        // not first-against-whatever-was-written-last.
        let t = assess(&series, DRIFT_WARN);
        assert_eq!(t.arches[0].drift["control"].ratio, Some(2.0));
        assert_eq!(
            (&*t.arches[0].from, &*t.arches[0].to),
            ("2025-03", "2026-01")
        );
    }

    #[test]
    fn a_tree_with_no_monthly_reports_is_a_series_of_none() {
        let root = tempfile::tempdir().unwrap();
        assert!(from_reports_root(root.path()).unwrap().is_empty());
        // And nothing is written for an empty trend.
        assert!(
            write(root.path(), "rebuild-trend", &Trend::default())
                .unwrap()
                .is_empty()
        );
        assert!(!root.path().join("rebuild-trend.txt").exists());
    }

    #[test]
    fn a_population_that_changed_size_is_reported_as_that_and_not_as_a_drift() {
        // golang on s390x, F42 to F45: 1,419 builds against 430, with a
        // build time that looks 1.46x worse. The ratio is real arithmetic
        // over two different populations.
        let mut a = arch("s390x", 0.5, 100.0, 100.0);
        let mut b = arch("s390x", 0.5, 100.0, 200.0);
        a.service[0].tasks = 1419;
        b.service[0].tasks = 430;
        let t = assess_default(&series(a, b));
        assert_eq!(t.warnings.len(), 1, "{:?}", t.warnings);
        let w = &t.warnings[0];
        assert!(w.contains("population changed"), "{w}");
        assert!(w.contains("not a drift"), "{w}");
        // And the ratio is still in the data for a reader who wants it.
        assert_eq!(t.arches[0].drift["rest"].ratio, Some(2.0));
    }

    #[test]
    fn a_stable_population_of_a_different_size_is_still_comparable() {
        // Populations need to be the same size as *each other*, not any
        // particular size: 601 against 601 is fine, and so is 9,430
        // against 9,327.
        let mut a = arch("s390x", 0.5, 100.0, 100.0);
        let mut b = arch("s390x", 0.5, 100.0, 200.0);
        a.service[0].tasks = 9430;
        b.service[0].tasks = 9327;
        let t = assess_default(&series(a, b));
        assert_eq!(t.warnings.len(), 1, "{:?}", t.warnings);
        assert!(t.warnings[0].contains("build time"), "{:?}", t.warnings);
    }

    #[test]
    fn a_like_for_like_comparison_warns_where_a_calendar_one_would_not() {
        // 1.30x: the step s390x's control population actually took between
        // two rebuilds. Over unrestricted months that is inside the noise
        // and must stay quiet; over two rebuilds it is the finding.
        let s = series(
            arch("s390x", 0.5, 100.0, 100.0),
            arch("s390x", 0.5, 130.0, 130.0),
        );
        assert!(assess(&s, DRIFT_WARN).warnings.is_empty());
        let tight = assess(&s, REBUILD_DRIFT_WARN);
        assert_eq!(tight.warnings.len(), 2, "{:?}", tight.warnings);
        // The file records which comparison it was, so a reader does not
        // have to infer it from the labels.
        assert_eq!(tight.drift_warn, REBUILD_DRIFT_WARN);
        assert!(render(&tight).contains("1.25x"));
    }

    #[test]
    fn an_events_period_is_named_by_its_release_when_one_is_known() {
        let mut e = crate::events::Event {
            kind: crate::events::Kind::MassRebuild,
            arch: None,
            from: 1_736_985_600.0,
            to: 1_737_331_200.0,
            days: 4,
            release: Some(42),
            announced: None,
            facts: Vec::new(),
            causes: Vec::new(),
        };
        assert_eq!(label_of(&e), "F42");
        // No schedule supplied, so fall back to the day it opened rather
        // than leaving the row unlabelled.
        e.release = None;
        assert_eq!(label_of(&e), "2025-01-16");
    }

    #[test]
    fn one_period_has_nothing_to_compare() {
        let s = vec![(
            "2025-01".to_string(),
            Health {
                arches: vec![arch("s390x", 0.5, 100.0, 200.0)],
                ..Default::default()
            },
        )];
        assert!(assess(&s, DRIFT_WARN).arches.is_empty());
    }

    #[test]
    fn every_family_reports_its_own_drift() {
        let t = assess_default(&series(
            arch("s390x", 0.5, 100.0, 200.0),
            arch("s390x", 0.5, 150.0, 300.0),
        ));
        let a = &t.arches[0];
        // Every family's own drift is reported and warned about; nothing
        // divides one family by another.
        assert_eq!(a.drift["rest"].ratio, Some(1.5));
        assert_eq!(a.drift["control"].ratio, Some(1.5));
        assert_eq!(t.warnings.len(), 2, "{:?}", t.warnings);
    }

    #[test]
    fn one_family_moving_and_another_not_is_two_rows_not_one_number() {
        let t = assess_default(&series(
            arch("s390x", 0.5, 100.0, 200.0),
            arch("s390x", 0.5, 100.0, 300.0),
        ));
        let d = &t.arches[0].drift;
        assert_eq!(
            (d["control"].ratio, d["rest"].ratio),
            (Some(1.0), Some(1.5))
        );
        // Only the family that moved is reported as having moved.
        assert_eq!(t.warnings.len(), 1, "{:?}", t.warnings);
        assert!(t.warnings[0].contains("rest"), "{:?}", t.warnings);
    }

    #[test]
    fn a_mix_shift_within_a_window_is_not_a_drift() {
        // The trap this module exists for: the two populations are far
        // apart in both periods and neither has changed. Nothing warns.
        let t = assess_default(&series(
            arch("s390x", 0.5, 228.0, 534.0),
            arch("s390x", 0.5, 228.0, 534.0),
        ));
        assert_eq!(t.arches[0].drift["rest"].ratio, Some(1.0));
        assert_eq!(t.arches[0].drift["control"].ratio, Some(1.0));
        assert!(t.warnings.is_empty(), "{:?}", t.warnings);
    }

    #[test]
    fn utilisation_warns_only_when_rising_into_the_nonlinear_range() {
        let falling = assess_default(&series(
            arch("s390x", 0.95, 100.0, 200.0),
            arch("s390x", 0.85, 100.0, 200.0),
        ));
        assert!(falling.warnings.is_empty(), "{:?}", falling.warnings);
        let rising = assess_default(&series(
            arch("s390x", 0.60, 100.0, 200.0),
            arch("s390x", 0.85, 100.0, 200.0),
        ));
        assert_eq!(rising.warnings.len(), 1, "{:?}", rising.warnings);
        assert!(rising.warnings[0].contains("utilisation"));
    }

    #[test]
    fn an_architecture_absent_from_the_first_period_is_skipped() {
        let t = assess_default(&series(
            arch("s390x", 0.5, 100.0, 200.0),
            arch("ppc64le", 0.5, 100.0, 300.0),
        ));
        assert!(t.arches.is_empty());
    }
}
