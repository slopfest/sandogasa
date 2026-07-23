// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `report` subcommand: turn a dataset into per-arch lag
//! numbers.
//!
//! Tasks are selected by completion time within the requested
//! sub-window (matching how datasets are swept); a task not yet
//! completed uses its creation time so long-running stragglers
//! still show in queue-wait stats. All boundaries are UTC unix
//! seconds. The `--min-samples` guard is presentational: human
//! output withholds statistics for thin rows, JSON always carries
//! the numbers plus counts so pooled datasets can re-filter.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;

use crate::dataset::{Dataset, FetchWindow, TaskRecord};
use crate::stats::{
    CriticalPath, DistSummary, critical_path, in_build_time_population, in_queue_wait_population,
    median, summarize,
};

/// Report filters, resolved by the CLI layer.
#[derive(Debug, Default)]
pub struct ReportOpts {
    /// Half-open `[since, until)` UTC unix bounds on task
    /// completion; `None` = unbounded.
    pub since: Option<f64>,
    pub until: Option<f64>,
    /// Restrict to these arches (empty = all).
    pub arches: Vec<String>,
    /// Include FAILED tasks in build-time stats.
    pub include_failed: bool,
    /// `Some(true)` = scratch only, `Some(false)` = official only.
    pub scratch: Option<bool>,
    /// Human output withholds stats below this sample count.
    pub min_samples: usize,
}

/// Per-arch statistics for one population class.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ArchStats {
    pub arch: String,
    /// Seconds queued before a builder started the task.
    pub queue_wait: Option<DistSummary>,
    /// Seconds building.
    pub build_time: Option<DistSummary>,
    /// Builds where this arch finished last (their bottleneck).
    pub builds_bottlenecked: usize,
    /// Total seconds this arch finished after the runner-up,
    /// summed over the builds it bottlenecked.
    pub bottleneck_total_delay: f64,
    /// Median marginal delay over the builds it bottlenecked.
    pub bottleneck_median_delay: Option<f64>,
}

/// The whole report, serialized as-is for `--json`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ReportOutput {
    pub instances: Vec<String>,
    /// Effective completion-time window, UTC unix seconds.
    pub since: Option<f64>,
    pub until: Option<f64>,
    pub coverage: Vec<FetchWindow>,
    /// Coverage holes between fetch windows.
    pub gaps: Vec<(String, f64, f64)>,
    /// True when filtered (scoped) and full fetches are mixed —
    /// counts under-represent the full instance.
    pub mixed_filtered_coverage: bool,
    /// All selected tasks together.
    pub arches: Vec<ArchStats>,
    /// The same statistics split by scratch-ness (present unless
    /// a --scratch/--official filter already narrowed the set).
    pub official: Option<Vec<ArchStats>>,
    pub scratch: Option<Vec<ArchStats>>,
    /// Tasks with no captured parent build: counted, excluded
    /// from the scratch split, included in the combined stats.
    pub unattributed_tasks: usize,
    /// Builds counted for critical-path attribution.
    pub bottlenecked_builds: usize,
}

/// Compute the report over a (merged) dataset.
pub fn run(dataset: &Dataset, opts: &ReportOpts) -> ReportOutput {
    let in_window = |task: &TaskRecord| -> bool {
        let ts = task.completion_ts.unwrap_or(task.create_ts);
        opts.since.is_none_or(|s| ts >= s) && opts.until.is_none_or(|u| ts < u)
    };
    let arch_ok = |task: &TaskRecord| -> bool {
        opts.arches.is_empty() || opts.arches.iter().any(|a| a == &task.arch)
    };

    // Scratch-ness per task, via its parent build. None =
    // unattributed.
    let scratchness = |task: &TaskRecord| -> Option<bool> {
        let parent = task.parent?;
        dataset
            .builds
            .get(&format!("{}:{parent}", task.instance))
            .map(|b| b.scratch)
    };

    let mut selected: Vec<&TaskRecord> = Vec::new();
    let mut unattributed = 0usize;
    for task in dataset.tasks.values() {
        if !in_window(task) || !arch_ok(task) {
            continue;
        }
        let class = scratchness(task);
        if class.is_none() {
            unattributed += 1;
        }
        match (opts.scratch, class) {
            // Explicit filter: unattributed tasks can't be proven
            // to match, so they drop out.
            (Some(want), Some(is)) if want == is => selected.push(task),
            (Some(_), _) => {}
            (None, _) => selected.push(task),
        }
    }

    // Critical path per build over its selected children.
    let mut by_parent: BTreeMap<String, Vec<&TaskRecord>> = BTreeMap::new();
    for task in &selected {
        if let Some(parent) = task.parent {
            by_parent
                .entry(format!("{}:{parent}", task.instance))
                .or_default()
                .push(task);
        }
    }
    let mut bottlenecked_builds = 0usize;
    let mut bottleneck_delays: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut paths: Vec<CriticalPath> = Vec::new();
    for children in by_parent.values() {
        if let Some(cp) = critical_path(children) {
            bottlenecked_builds += 1;
            paths.push(cp);
        }
    }
    for cp in &paths {
        bottleneck_delays
            .entry(&cp.bottleneck_arch)
            .or_default()
            .push(cp.marginal_delay);
    }

    let arches = arch_stats(&selected, &bottleneck_delays, opts.include_failed);
    let (official, scratch) = if opts.scratch.is_some() {
        (None, None)
    } else {
        let split = |want: bool| -> Vec<ArchStats> {
            let subset: Vec<&TaskRecord> = selected
                .iter()
                .copied()
                .filter(|t| scratchness(t) == Some(want))
                .collect();
            // Bottleneck attribution is not re-split: a build's
            // critical path is a whole-build property already
            // classified by its own scratch-ness below.
            let mut by_parent: BTreeMap<String, Vec<&TaskRecord>> = BTreeMap::new();
            for task in &subset {
                if let Some(parent) = task.parent {
                    by_parent
                        .entry(format!("{}:{parent}", task.instance))
                        .or_default()
                        .push(task);
                }
            }
            let mut bottleneck_delays: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
            let mut paths: Vec<CriticalPath> = Vec::new();
            for children in by_parent.values() {
                if let Some(cp) = critical_path(children) {
                    paths.push(cp);
                }
            }
            for cp in &paths {
                bottleneck_delays
                    .entry(&cp.bottleneck_arch)
                    .or_default()
                    .push(cp.marginal_delay);
            }
            arch_stats(&subset, &bottleneck_delays, opts.include_failed)
        };
        (Some(split(false)), Some(split(true)))
    };

    ReportOutput {
        instances: dataset
            .meta
            .windows
            .iter()
            .map(|w| w.instance.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        since: opts.since,
        until: opts.until,
        coverage: dataset.meta.windows.clone(),
        gaps: dataset.coverage_gaps(),
        mixed_filtered_coverage: dataset.mixes_filtered_windows(),
        arches,
        official,
        scratch,
        unattributed_tasks: unattributed,
        bottlenecked_builds,
    }
}

/// Aggregate one task subset into per-arch rows, sorted by total
/// bottleneck delay descending (the headline ordering: which arch
/// costs the most).
fn arch_stats(
    tasks: &[&TaskRecord],
    bottleneck_delays: &BTreeMap<&str, Vec<f64>>,
    include_failed: bool,
) -> Vec<ArchStats> {
    let mut queue: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut build: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    for task in tasks {
        if in_queue_wait_population(task)
            && let Some(wait) = task.queue_wait()
        {
            queue.entry(&task.arch).or_default().push(wait);
        }
        if in_build_time_population(task, include_failed)
            && let Some(time) = task.build_time()
        {
            build.entry(&task.arch).or_default().push(time);
        }
    }
    let mut all_arches: std::collections::BTreeSet<&str> = queue.keys().copied().collect();
    all_arches.extend(build.keys().copied());
    all_arches.extend(bottleneck_delays.keys().copied());

    let mut rows: Vec<ArchStats> = all_arches
        .into_iter()
        .map(|arch| {
            let delays = bottleneck_delays.get(arch);
            let mut sorted_delays = delays.cloned().unwrap_or_default();
            sorted_delays.sort_by(|a, b| a.total_cmp(b));
            ArchStats {
                arch: arch.to_string(),
                queue_wait: queue.get(arch).cloned().and_then(|mut v| summarize(&mut v)),
                build_time: build.get(arch).cloned().and_then(|mut v| summarize(&mut v)),
                builds_bottlenecked: sorted_delays.len(),
                bottleneck_total_delay: sorted_delays.iter().sum(),
                bottleneck_median_delay: median(&sorted_delays),
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        b.bottleneck_total_delay
            .total_cmp(&a.bottleneck_total_delay)
    });
    rows
}

/// Render seconds as a compact human duration.
pub fn fmt_duration(secs: f64) -> String {
    let secs = secs.max(0.0);
    if secs >= 3600.0 {
        format!("{:.1}h", secs / 3600.0)
    } else if secs >= 60.0 {
        format!("{:.1}m", secs / 60.0)
    } else {
        format!("{secs:.0}s")
    }
}

/// Human rendering of the report.
pub fn render(output: &ReportOutput, min_samples: usize) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(o, "Instances: {}", output.instances.join(", "));
    if output.mixed_filtered_coverage {
        let _ = writeln!(
            o,
            "warning: dataset mixes scoped and full fetches — counts \
             under-represent the full instance"
        );
    }
    for (instance, from, to) in &output.gaps {
        let _ = writeln!(
            o,
            "warning: coverage gap on {instance}: no data between \
             unix {from:.0} and {to:.0}"
        );
    }
    let _ = writeln!(
        o,
        "Bottlenecked builds: {} (critical-path attribution); \
         unattributed tasks: {}",
        output.bottlenecked_builds, output.unattributed_tasks
    );

    render_rows(&mut o, "All builds", &output.arches, min_samples);
    if let Some(official) = &output.official {
        render_rows(&mut o, "Official builds", official, min_samples);
    }
    if let Some(scratch) = &output.scratch {
        render_rows(&mut o, "Scratch builds", scratch, min_samples);
    }
    // These tables get pasted into tickets and threads, so they
    // must explain themselves. Backticked bullets stay readable in
    // a terminal and render as a list in Markdown (a bare leading
    // `*` would italicize).
    let _ = writeln!(
        o,
        "\nColumn legend:\n\
         - `queued` / `built` — tasks counted in the wait/time stats.\n\
         - `med-wait`, `p90-wait` — task creation until a builder \
         picked it up.\n\
         - `med-time`, `p90-time` — builder start until completion.\n\
         - `bottleneck` — builds where this arch finished last (the \
         build was bottlenecked on it).\n\
         - `med-delay` / `tot-delay` — how long after the \
         second-slowest arch the\n  \
         bottleneck arch finished; the extra wall-clock time it alone \
         cost those\n  \
         builds (median per build / summed over the window)."
    );
    o
}

/// Render one per-arch table as a padded Markdown pipe table —
/// aligned for terminal/plain-text reading, and pasteable into
/// anything that renders Markdown. Rows below the sample guard
/// are pulled out into a footnote (Markdown cells can't span).
fn render_rows(o: &mut String, title: &str, rows: &[ArchStats], min_samples: usize) {
    use std::fmt::Write as _;
    if rows.is_empty() {
        return;
    }
    const HEADERS: [&str; 10] = [
        "arch",
        "queued",
        "med-wait",
        "p90-wait",
        "built",
        "med-time",
        "p90-time",
        "bottleneck",
        "med-delay",
        "tot-delay",
    ];

    let mut cells: Vec<[String; 10]> = Vec::new();
    let mut thin: Vec<String> = Vec::new();
    for row in rows {
        let samples = row
            .queue_wait
            .as_ref()
            .map(|s| s.count)
            .max(row.build_time.as_ref().map(|s| s.count))
            .unwrap_or(0);
        if samples < min_samples {
            thin.push(format!("{} (n={samples})", row.arch));
            continue;
        }
        let dist = |d: &Option<DistSummary>| -> (String, String, String) {
            match d {
                Some(s) => (
                    s.count.to_string(),
                    fmt_duration(s.median),
                    fmt_duration(s.p90),
                ),
                None => ("0".into(), "-".into(), "-".into()),
            }
        };
        let (queued, med_wait, p90_wait) = dist(&row.queue_wait);
        let (built, med_time, p90_time) = dist(&row.build_time);
        cells.push([
            row.arch.clone(),
            queued,
            med_wait,
            p90_wait,
            built,
            med_time,
            p90_time,
            row.builds_bottlenecked.to_string(),
            row.bottleneck_median_delay
                .map(fmt_duration)
                .unwrap_or_else(|| "-".into()),
            fmt_duration(row.bottleneck_total_delay),
        ]);
    }

    let _ = writeln!(o, "\n{title}:\n");
    if !cells.is_empty() {
        let widths: Vec<usize> = (0..HEADERS.len())
            .map(|col| {
                cells
                    .iter()
                    .map(|row| row[col].chars().count())
                    .chain([HEADERS[col].len()])
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let line = |o: &mut String, row: &[String]| {
            let mut out = String::from("|");
            for (col, cell) in row.iter().enumerate() {
                if col == 0 {
                    // Arch names left-aligned, numbers right-aligned.
                    out.push_str(&format!(" {:<width$} |", cell, width = widths[col]));
                } else {
                    out.push_str(&format!(" {:>width$} |", cell, width = widths[col]));
                }
            }
            let _ = writeln!(o, "{out}");
        };
        line(o, &HEADERS.map(String::from));
        let mut sep = String::from("|");
        for (col, width) in widths.iter().enumerate() {
            if col == 0 {
                sep.push_str(&format!(":{:-<width$}-|", "", width = width));
            } else {
                sep.push_str(&format!("-{:->width$}:|", "", width = width));
            }
        }
        let _ = writeln!(o, "{sep}");
        for row in &cells {
            line(o, row);
        }
    }
    if !thin.is_empty() {
        let _ = writeln!(o, "\nBelow --min-samples: {}.", thin.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::{BuildRecord, Dataset};

    fn task(id: i64, parent: i64, arch: &str, start: f64, completion: f64) -> TaskRecord {
        TaskRecord {
            instance: "fedora".to_string(),
            task_id: id,
            parent: Some(parent),
            arch: arch.to_string(),
            package: Some("foo".to_string()),
            state: 2,
            create_ts: 0.0,
            start_ts: Some(start),
            completion_ts: Some(completion),
            host_id: None,
            channel_id: None,
            weight: None,
        }
    }

    fn build(id: i64, scratch: bool) -> BuildRecord {
        BuildRecord {
            instance: "fedora".to_string(),
            task_id: id,
            package: Some("foo".to_string()),
            nvr: None,
            target: None,
            owner: Some("alice".to_string()),
            scratch,
            state: 2,
            create_ts: 0.0,
            start_ts: Some(0.0),
            completion_ts: Some(1000.0),
            priority: None,
        }
    }

    fn dataset() -> Dataset {
        let mut ds = Dataset::new();
        // Official build 1: s390x gates by 300s.
        ds.builds.insert("fedora:1".into(), build(1, false));
        for t in [
            task(11, 1, "x86_64", 10.0, 100.0),
            task(12, 1, "aarch64", 10.0, 90.0),
            task(13, 1, "s390x", 10.0, 400.0),
        ] {
            ds.tasks.insert(t.key(), t);
        }
        // Scratch build 2: ppc64le gates by 50s.
        ds.builds.insert("fedora:2".into(), build(2, true));
        for t in [
            task(21, 2, "x86_64", 10.0, 100.0),
            task(22, 2, "ppc64le", 10.0, 150.0),
        ] {
            ds.tasks.insert(t.key(), t);
        }
        // Unattributed task (parent never captured).
        ds.tasks
            .insert("fedora:31".into(), task(31, 999, "s390x", 10.0, 50.0));
        ds
    }

    #[test]
    fn combined_report_attributes_and_counts() {
        let ds = dataset();
        let out = run(&ds, &ReportOpts::default());
        assert_eq!(out.bottlenecked_builds, 2);
        assert_eq!(out.unattributed_tasks, 1);
        // s390x tops the ordering with 300s total bottleneck delay.
        assert_eq!(out.arches[0].arch, "s390x");
        assert_eq!(out.arches[0].bottleneck_total_delay, 300.0);
        assert_eq!(out.arches[0].builds_bottlenecked, 1);
        // Its queue population includes the unattributed task.
        assert_eq!(out.arches[0].queue_wait.as_ref().unwrap().count, 2);
    }

    #[test]
    fn scratch_split_partitions_builds() {
        let ds = dataset();
        let out = run(&ds, &ReportOpts::default());
        let official = out.official.unwrap();
        let scratch = out.scratch.unwrap();
        assert!(
            official
                .iter()
                .any(|r| r.arch == "s390x" && r.builds_bottlenecked == 1)
        );
        assert!(official.iter().all(|r| r.arch != "ppc64le"));
        assert!(
            scratch
                .iter()
                .any(|r| r.arch == "ppc64le" && r.builds_bottlenecked == 1)
        );
    }

    #[test]
    fn scratch_filter_drops_unattributed_and_split() {
        let ds = dataset();
        let out = run(
            &ds,
            &ReportOpts {
                scratch: Some(true),
                ..Default::default()
            },
        );
        assert!(out.official.is_none());
        assert!(out.scratch.is_none());
        // Only the scratch build's arches appear.
        let arch_names: Vec<&str> = out.arches.iter().map(|r| r.arch.as_str()).collect();
        assert!(arch_names.contains(&"ppc64le"));
        assert!(!arch_names.contains(&"aarch64"));
    }

    #[test]
    fn window_and_arch_filters_apply() {
        let ds = dataset();
        let out = run(
            &ds,
            &ReportOpts {
                since: Some(120.0),
                ..Default::default()
            },
        );
        // Only completions >= 120: s390x@400, ppc64le@150.
        let arch_names: Vec<&str> = out.arches.iter().map(|r| r.arch.as_str()).collect();
        assert_eq!(arch_names.len(), 2);

        let out = run(
            &ds,
            &ReportOpts {
                arches: vec!["s390x".to_string()],
                ..Default::default()
            },
        );
        assert!(out.arches.iter().all(|r| r.arch == "s390x"));
    }

    #[test]
    fn window_bounds_are_half_open() {
        // A task completing exactly at `until` belongs to the NEXT
        // day's window — adjacent single-day reports must not both
        // count it. The lower bound stays inclusive.
        let ds = dataset();
        // Completions in the fixture: 90, 100, 150, 400 (+50 for
        // the unattributed task).
        let out = run(
            &ds,
            &ReportOpts {
                since: Some(100.0),
                until: Some(150.0),
                ..Default::default()
            },
        );
        let counted: usize = out
            .arches
            .iter()
            .map(|r| r.queue_wait.as_ref().map(|s| s.count).unwrap_or(0))
            .sum();
        // Only the two tasks completing at exactly 100 (inclusive
        // lower bound); 150 is excluded (exclusive upper bound).
        assert_eq!(counted, 2);
    }

    #[test]
    fn render_is_stable_and_guards_thin_rows() {
        let ds = dataset();
        let out = run(&ds, &ReportOpts::default());
        let text = render(&out, 5);
        assert!(text.contains("Below --min-samples"), "{text}");
        // Thin rows never appear inside the table.
        assert!(!text.contains("| s390x"), "{text}");
        let text = render(&out, 1);
        assert!(text.contains("| s390x"), "{text}");
        assert!(text.contains("| tot-delay |"), "{text}");
        // A Markdown pipe table: header, alignment row, data rows.
        assert!(text.contains("|:---"), "{text}");
        assert!(text.contains("---:|"), "{text}");
        // The legend ships with every report.
        assert!(text.contains("finished last"), "{text}");
        assert!(text.contains("second-slowest"), "{text}");
    }

    #[test]
    fn fmt_duration_scales() {
        assert_eq!(fmt_duration(42.0), "42s");
        assert_eq!(fmt_duration(90.0), "1.5m");
        assert_eq!(fmt_duration(5400.0), "1.5h");
        assert_eq!(fmt_duration(-3.0), "0s");
    }
}
