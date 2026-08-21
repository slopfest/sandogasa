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
//! Comparing each population with *its own* earlier self cancels the mix,
//! and dividing one population's drift by the other's separates two causes
//! that a single number conflates:
//!
//! | control drift | rest drift | reading |
//! |---|---|---|
//! | 1.0 | 1.0 | nothing changed |
//! | 1.5 | 1.5 | the **platform** got slower — builders, storage, kernel |
//! | 1.0 | 1.5 | the **toolchain** got more expensive — compiler flags, hardening |
//! | 1.5 | 1.0 | look again; this ordering has no obvious cause |
//!
//! Fedora between F42 and F45 measured the third row on every
//! architecture and the second row only on s390x, which is why the capacity
//! ask and the compiler-flag conversation are separate conversations.

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
    /// Ratio of the non-control drift to the control drift. Above 1 the
    /// toolchain moved; at 1 with both drifts high, the platform did.
    pub divergence: Option<f64>,
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

/// Write a trend beside an events tree.
pub fn write(root: &Path, trend: &Trend) -> Result<Vec<std::path::PathBuf>, String> {
    if trend.arches.is_empty() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut written = Vec::new();
    for (name, body) in [
        ("trend.txt", render(trend)),
        (
            "trend.json",
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
        let ratio_of = |name: &str| drift.get(name).and_then(|d| d.ratio);
        let entry = ArchTrend {
            arch: a.arch.clone(),
            from: from_label.clone(),
            to: to_label.clone(),
            divergence: match (ratio_of("rest"), ratio_of("control")) {
                (Some(r), Some(c)) if c > 0.0 => Some(r / c),
                _ => None,
            },
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
    let mins = |s: Option<f64>| s.map_or("?".to_string(), |x| format!("{:.1}m", x / 60.0));
    for (name, d) in &t.drift {
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
    // Only worth saying once both populations are in hand, and only in the
    // direction that has a known cause.
    if let Some(div) = t.divergence
        && div >= drift_warn
    {
        out.push(format!(
            "{}: non-control build time rose {div:.2}x faster than the \
             rust control -- a toolchain cost rather than the platform",
            t.arch
        ));
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
        "  arch      population   median then    median now   ratio\n  \
         --------  -----------  ------------  ------------  ------\n",
    );
    let mins = |x: Option<f64>| x.map_or("-".to_string(), |v| format!("{:.1}m", v / 60.0));
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
        if let Some(div) = a.divergence {
            s.push_str(&format!(
                "  {:<8}  {:<11}  {:>12}  {:>12}  {:>6}\n",
                a.arch,
                "divergence",
                "",
                "",
                format!("{div:.2}x")
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
         - `control` -- packages named `{}*`, whose build cost is dominated \
         by rustc\n    rather than by the C and C++ compilers. They are the \
         population held\n    still, so that a change in what they cost is a \
         change in the machines\n    and not in a compiler flag.\n  \
         - `rest` -- every other package in the same window.\n  \
         - `ratio` -- median build time now over median build time then, for \
         one\n    population against *its own* earlier self. Never one \
         population against\n    the other: the gap between them is package \
         size, since Rust crates are\n    small, and reads as a compiler \
         penalty that is not there.\n  \
         - `divergence` -- the `rest` ratio over the `control` ratio, which \
         separates\n    two causes a single number conflates. Both ratios \
         high with divergence\n    near 1.0 is the platform getting slower; \
         divergence above 1.0 is the\n    toolchain getting more expensive, \
         whatever the ratios themselves did.\n  \
         - `utilisation` -- offered task weight over enabled builder weight, \
         at each\n    end of the range. Queueing is nonlinear in it.\n\n  \
         Below about {:.2}x a ratio is not distinguishable from what people \
         happened\n  to build in that period, which is why the periods \
         compared matter as much as\n  the numbers: two mass rebuilds build \
         nearly the same set of packages, two\n  calendar months need not.\n",
        crate::health::CONTROL_PREFIX,
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
    fn both_populations_slowing_equally_reads_as_the_platform() {
        let t = assess_default(&series(
            arch("s390x", 0.5, 100.0, 200.0),
            arch("s390x", 0.5, 150.0, 300.0),
        ));
        let a = &t.arches[0];
        assert_eq!(a.divergence.map(|d| (d * 100.0).round()), Some(100.0));
        // Both drifts warn; the toolchain line does not, since neither
        // population moved relative to the other.
        assert_eq!(a.drift["rest"].ratio, Some(1.5));
        assert!(
            !t.warnings.iter().any(|w| w.contains("toolchain")),
            "{:?}",
            t.warnings
        );
        assert_eq!(t.warnings.len(), 2, "{:?}", t.warnings);
    }

    #[test]
    fn only_the_non_control_slowing_reads_as_the_toolchain() {
        let t = assess_default(&series(
            arch("s390x", 0.5, 100.0, 200.0),
            arch("s390x", 0.5, 100.0, 300.0),
        ));
        assert_eq!(t.arches[0].divergence, Some(1.5));
        assert!(
            t.warnings.iter().any(|w| w.contains("toolchain")),
            "{:?}",
            t.warnings
        );
    }

    #[test]
    fn a_mix_shift_within_a_window_is_not_a_drift() {
        // The trap this module exists for: the two populations are far
        // apart in both periods and neither has changed. Nothing warns.
        let t = assess_default(&series(
            arch("s390x", 0.5, 228.0, 534.0),
            arch("s390x", 0.5, 228.0, 534.0),
        ));
        assert_eq!(t.arches[0].divergence, Some(1.0));
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
