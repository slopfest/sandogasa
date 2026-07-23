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
    /// UTC unix bounds on task completion; `None` = unbounded.
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
    /// Builds where this arch finished last.
    pub builds_gated: usize,
    /// Total seconds this arch finished after the runner-up,
    /// summed over the builds it gated.
    pub gating_total_delay: f64,
    /// Median marginal delay over the builds it gated.
    pub gating_median_delay: Option<f64>,
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
    pub gated_builds: usize,
}

/// Compute the report over a (merged) dataset.
pub fn run(dataset: &Dataset, opts: &ReportOpts) -> ReportOutput {
    let in_window = |task: &TaskRecord| -> bool {
        let ts = task.completion_ts.unwrap_or(task.create_ts);
        opts.since.is_none_or(|s| ts >= s) && opts.until.is_none_or(|u| ts <= u)
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
    let mut gated_builds = 0usize;
    let mut gating: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut paths: Vec<CriticalPath> = Vec::new();
    for children in by_parent.values() {
        if let Some(cp) = critical_path(children) {
            gated_builds += 1;
            paths.push(cp);
        }
    }
    for cp in &paths {
        gating
            .entry(&cp.gating_arch)
            .or_default()
            .push(cp.marginal_delay);
    }

    let arches = arch_stats(&selected, &gating, opts.include_failed);
    let (official, scratch) = if opts.scratch.is_some() {
        (None, None)
    } else {
        let split = |want: bool| -> Vec<ArchStats> {
            let subset: Vec<&TaskRecord> = selected
                .iter()
                .copied()
                .filter(|t| scratchness(t) == Some(want))
                .collect();
            // Gating attribution is not re-split: a build's
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
            let mut gating: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
            let mut paths: Vec<CriticalPath> = Vec::new();
            for children in by_parent.values() {
                if let Some(cp) = critical_path(children) {
                    paths.push(cp);
                }
            }
            for cp in &paths {
                gating
                    .entry(&cp.gating_arch)
                    .or_default()
                    .push(cp.marginal_delay);
            }
            arch_stats(&subset, &gating, opts.include_failed)
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
        gated_builds,
    }
}

/// Aggregate one task subset into per-arch rows, sorted by total
/// gating delay descending (the headline ordering: which arch
/// costs the most).
fn arch_stats(
    tasks: &[&TaskRecord],
    gating: &BTreeMap<&str, Vec<f64>>,
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
    all_arches.extend(gating.keys().copied());

    let mut rows: Vec<ArchStats> = all_arches
        .into_iter()
        .map(|arch| {
            let delays = gating.get(arch);
            let mut sorted_delays = delays.cloned().unwrap_or_default();
            sorted_delays.sort_by(|a, b| a.total_cmp(b));
            ArchStats {
                arch: arch.to_string(),
                queue_wait: queue.get(arch).cloned().and_then(|mut v| summarize(&mut v)),
                build_time: build.get(arch).cloned().and_then(|mut v| summarize(&mut v)),
                builds_gated: sorted_delays.len(),
                gating_total_delay: sorted_delays.iter().sum(),
                gating_median_delay: median(&sorted_delays),
            }
        })
        .collect();
    rows.sort_by(|a, b| b.gating_total_delay.total_cmp(&a.gating_total_delay));
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
        "Gated builds: {} (critical-path attribution); \
         unattributed tasks: {}",
        output.gated_builds, output.unattributed_tasks
    );

    let render_rows = |o: &mut String, title: &str, rows: &[ArchStats]| {
        if rows.is_empty() {
            return;
        }
        let _ = writeln!(o, "\n{title}:");
        let _ = writeln!(
            o,
            "  {:<10} {:>7} {:>9} {:>9} {:>7} {:>9} {:>9} {:>7} {:>10} {:>10}",
            "arch",
            "queued",
            "med-wait",
            "p90-wait",
            "built",
            "med-time",
            "p90-time",
            "gated",
            "med-delay",
            "tot-delay"
        );
        for row in rows {
            let thin = row
                .queue_wait
                .as_ref()
                .map(|s| s.count)
                .max(row.build_time.as_ref().map(|s| s.count))
                .unwrap_or(0)
                < min_samples;
            if thin {
                let n = row
                    .queue_wait
                    .as_ref()
                    .map(|s| s.count)
                    .max(row.build_time.as_ref().map(|s| s.count))
                    .unwrap_or(0);
                let _ = writeln!(o, "  {:<10} (n={n}, below --min-samples)", row.arch);
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
            let _ = writeln!(
                o,
                "  {:<10} {:>7} {:>9} {:>9} {:>7} {:>9} {:>9} {:>7} {:>10} {:>10}",
                row.arch,
                queued,
                med_wait,
                p90_wait,
                built,
                med_time,
                p90_time,
                row.builds_gated,
                row.gating_median_delay
                    .map(fmt_duration)
                    .unwrap_or_else(|| "-".into()),
                fmt_duration(row.gating_total_delay),
            );
        }
    };

    render_rows(&mut o, "All builds", &output.arches);
    if let Some(official) = &output.official {
        render_rows(&mut o, "Official builds", official);
    }
    if let Some(scratch) = &output.scratch {
        render_rows(&mut o, "Scratch builds", scratch);
    }
    // These tables get pasted into tickets and threads, so they
    // must explain themselves.
    let _ = writeln!(
        o,
        "\nColumns: queued/built = tasks counted in the wait/time \
         stats.\n\
         *-wait = task creation until a builder picked it up \
         (median, p90).\n\
         *-time = builder start until completion (median, p90).\n\
         gated = builds where this arch finished last, holding up \
         the build.\n\
         med-delay / tot-delay = how long after the second-slowest \
         arch the\n\
         gating arch finished — the extra wall-clock time it alone \
         cost those\n\
         builds (median per build / summed over the window)."
    );
    o
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
        assert_eq!(out.gated_builds, 2);
        assert_eq!(out.unattributed_tasks, 1);
        // s390x tops the ordering with 300s total gating delay.
        assert_eq!(out.arches[0].arch, "s390x");
        assert_eq!(out.arches[0].gating_total_delay, 300.0);
        assert_eq!(out.arches[0].builds_gated, 1);
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
                .any(|r| r.arch == "s390x" && r.builds_gated == 1)
        );
        assert!(official.iter().all(|r| r.arch != "ppc64le"));
        assert!(
            scratch
                .iter()
                .any(|r| r.arch == "ppc64le" && r.builds_gated == 1)
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
    fn render_is_stable_and_guards_thin_rows() {
        let ds = dataset();
        let out = run(&ds, &ReportOpts::default());
        let text = render(&out, 5);
        assert!(text.contains("below --min-samples"), "{text}");
        let text = render(&out, 1);
        assert!(text.contains("s390x"), "{text}");
        assert!(text.contains("tot-delay"), "{text}");
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
