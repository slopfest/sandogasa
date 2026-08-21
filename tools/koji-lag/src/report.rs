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

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::Serialize;

use crate::class::{self, Class};
use crate::dataset::{BUILD_ARCH, BuildRecord, Dataset, FetchWindow, TaskRecord};
use crate::stats::{
    CriticalPath, DistSummary, critical_path, in_build_time_population, in_queue_wait_population,
    median, summarize,
};

/// Report filters, resolved by the CLI layer.
#[derive(Debug, Default, Clone)]
pub struct ReportOpts {
    /// Half-open `[since, until)` UTC unix bounds on task
    /// completion; `None` = unbounded.
    pub since: Option<f64>,
    pub until: Option<f64>,
    /// Restrict to these arches (empty = all).
    pub arches: Vec<String>,
    /// Restrict to builds submitted by these accounts (empty = all).
    ///
    /// Matched exactly against the build's owner, which for services is a
    /// long name — `koschei/koschei-backend01.rdu3.fedoraproject.org`, not
    /// `koschei`. Narrowing by class is [`crate::class`]'s job; this is for
    /// "how did *my* builds fare", which wants one account.
    pub owners: Vec<String>,
    /// Restrict to these source packages (empty = all).
    pub packages: Vec<String>,
    /// Restrict to these classes of build (empty = all).
    ///
    /// The filter that makes populations comparable across periods. An
    /// unrestricted window is mostly koschei, whose mix moves with whatever
    /// it happened to retry, so the same window's median build time reads
    /// 56s, 38s, 3m and 3m across the four rebuilds and means nothing. A
    /// mass rebuild builds nearly everything, so restricting to it holds the
    /// mix roughly fixed and a drift is a drift.
    pub classes: Vec<Class>,
    /// Include FAILED tasks in build-time stats.
    pub include_failed: bool,
    /// `Some(true)` = scratch only, `Some(false)` = official only.
    pub scratch: Option<bool>,
    /// Human output withholds stats below this sample count.
    pub min_samples: usize,
    /// The period the report is *about*, when that differs from the row
    /// filter above — a store query has already selected the period, so
    /// filtering again would split builds across it.
    ///
    /// Coverage is judged against this. Without it, only holes *between*
    /// coverage windows can be found, so a period uncovered at its edges,
    /// or uncovered entirely, reads as complete: the report warned about
    /// nothing while saying 33,790 of 51,587 builds had no arch tasks.
    pub period: Option<(f64, f64)>,
}

/// How many arches a build was built for, which decides what can be
/// asked of its tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildShape {
    /// Two or more arches raced, so one of them finished last.
    Multi,
    /// One arch, with nothing to compare it against.
    Single,
    /// A `noarch` package: one build, on a machine of the hub's choosing.
    NoArch,
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
    /// Signals that say an architecture is in trouble, with the thresholds
    /// they crossed — see [`crate::health`]. Reported per class and per
    /// cohort of submitter, never summed across either.
    pub health: crate::health::Health,
    /// The source rebuild that starts every build, by the arch of the
    /// host that ran it. Koji picks that host independently of what the
    /// build targets, so a package can wait on a machine it does not
    /// even build for.
    pub srpm: Vec<ArchStats>,
    /// Per-arch builds of packages built for two or more arches — the
    /// only ones a bottleneck can be attributed to.
    pub multi_arch: Vec<ArchStats>,
    /// Per-arch builds of packages built for exactly one arch. Nothing
    /// to be slower than, so wait and run time stand alone.
    pub single_arch: Vec<ArchStats>,
    /// Builds of `noarch` packages, by the arch of the host that took
    /// them: the payload runs anywhere, but the machine that builds it
    /// is a real one and its speed is what the package waits on.
    pub noarch_by_host: Vec<ArchStats>,
    /// Builds counted for critical-path attribution.
    pub bottlenecked_builds: usize,
    /// Builds that completed in the window, whatever came of them.
    ///
    /// The denominator the attributed count needs: "5082 bottlenecked"
    /// says nothing about severity without knowing whether the day held
    /// five thousand builds or fifty thousand.
    pub builds_in_window: usize,
    /// Builds in the window with at least one per-arch task selected —
    /// the ones attribution could even be attempted on. Lower than
    /// `builds_in_window` when a build's children fall outside an arch
    /// filter, were never swept, or the build had none.
    pub builds_with_tasks: usize,
}

/// Compute the report over a (merged) dataset.
/// The parts of the reported period no coverage window vouches for.
///
/// With a period given, this is the period minus the windows, so a range
/// that is uncovered at either end — or not covered at all — is reported.
/// Without one, the best that can be said is where the windows fail to
/// meet, which is what a dataset alone can answer.
fn uncovered(dataset: &Dataset, opts: &ReportOpts) -> Vec<(String, f64, f64)> {
    let Some((from, to)) = opts.period else {
        return dataset.coverage_gaps();
    };
    let mut holes = Vec::new();
    let mut instances: Vec<&str> = dataset.meta.windows.iter().map(|w| &*w.instance).collect();
    instances.sort_unstable();
    instances.dedup();
    // A period with no windows at all belongs to whichever instance the
    // rows are from, and if there are none either, to nobody: an empty
    // report needs no warning about coverage it never had.
    if instances.is_empty() {
        instances = dataset.builds.values().map(|b| &*b.instance).collect();
        instances.sort_unstable();
        instances.dedup();
    }
    for instance in instances {
        let mut spans: Vec<(f64, f64)> = dataset
            .meta
            .windows
            .iter()
            .filter(|w| w.instance == instance)
            .map(|w| (w.from.max(from), w.to.min(to)))
            .filter(|(a, b)| b > a)
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut at = from;
        for (start, end) in spans {
            if start > at {
                holes.push((instance.to_string(), at, start));
            }
            at = at.max(end);
        }
        if at < to {
            holes.push((instance.to_string(), at, to));
        }
    }
    holes
}

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

    // Owner and package come from the parent build, since a child task
    // records neither. A task whose parent is absent cannot be shown to
    // match, so it drops out of a narrowed report rather than being
    // counted into it — the same rule the scratch filter follows.
    // Whether a build passes the narrowing flags. Shared by the task
    // filter and by the build count, so a narrowed report's own
    // denominator is narrowed too: before this, `--owner NAME` reported
    // the whole fleet's "Builds completed" above that person's rows.
    let build_ok = |build: &BuildRecord| -> bool {
        let class_ok = opts.classes.is_empty() || opts.classes.contains(&class::of_build(build));
        let owner_ok = opts.owners.is_empty()
            || build
                .owner
                .as_deref()
                .is_some_and(|o| opts.owners.iter().any(|want| want == o));
        // A build's own package name is the one to trust; it is filled in
        // from its children when the build task itself did not name one.
        let package_ok = opts.packages.is_empty()
            || build
                .package
                .as_deref()
                .is_some_and(|p| opts.packages.iter().any(|want| want == p));
        owner_ok && package_ok && class_ok
    };

    let narrowed = !opts.owners.is_empty() || !opts.packages.is_empty() || !opts.classes.is_empty();
    let attributed_ok = |task: &TaskRecord| -> bool {
        if !narrowed {
            return true;
        }
        // A task whose parent was never swept cannot be shown to match,
        // so a narrowed report leaves it out rather than guessing.
        task.parent
            .and_then(|p| dataset.builds.get(&format!("{}:{p}", task.instance)))
            .is_some_and(build_ok)
    };

    let mut selected: Vec<&TaskRecord> = Vec::new();
    let mut unattributed = 0usize;
    for task in dataset.tasks.values() {
        if !in_window(task) || !arch_ok(task) || !attributed_ok(task) {
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
    // Every build that finished in the window, which is what makes the
    // attributed count legible as a proportion.
    let counted_builds: BTreeSet<&String> = dataset
        .builds
        .iter()
        .filter(|(_, b)| {
            let ts = b.completion_ts.unwrap_or(b.create_ts);
            opts.since.is_none_or(|s| ts >= s) && opts.until.is_none_or(|u| ts < u)
        })
        .filter(|(_, b)| match opts.scratch {
            Some(want) => want == b.scratch,
            None => true,
        })
        .filter(|(_, b)| build_ok(b))
        .map(|(key, _)| key)
        .collect();
    let builds_in_window = counted_builds.len();
    // Counted over the builds themselves, not over the parent ids the
    // tasks mention: a task whose parent was never swept points at no
    // build here, and is reported as an unattributed task instead.
    let builds_with_tasks = by_parent
        .keys()
        .filter(|key| counted_builds.contains(key))
        .count();
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

    // The existing tables describe per-arch builds, so the source
    // rebuild stays out of them: it is one task per build on an arch of
    // the hub's choosing, and folding it in would move every arch's
    // numbers for reasons that have nothing to do with that arch.
    let per_arch: Vec<&TaskRecord> = selected
        .iter()
        .copied()
        .filter(|t| t.method == BUILD_ARCH)
        .collect();
    let srpm_tasks: Vec<&TaskRecord> = selected
        .iter()
        .copied()
        .filter(|t| crate::dataset::is_srpm_step(&t.method))
        .collect();

    // How many arches each build was built for decides which questions
    // can be asked of it.
    let mut arches_per_build: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for task in &per_arch {
        if let Some(parent) = task.parent {
            arches_per_build
                .entry(format!("{}:{parent}", task.instance))
                .or_default()
                .insert(&task.arch);
        }
    }
    let class_of = |task: &TaskRecord| -> Option<BuildShape> {
        let parent = task.parent?;
        let arches = arches_per_build.get(&format!("{}:{parent}", task.instance))?;
        Some(match (arches.len(), arches.iter().next()) {
            (1, Some(&"noarch")) => BuildShape::NoArch,
            (1, _) => BuildShape::Single,
            _ => BuildShape::Multi,
        })
    };
    let of_shape = |shape: BuildShape| -> Vec<&TaskRecord> {
        per_arch
            .iter()
            .copied()
            .filter(|t| class_of(t) == Some(shape))
            .collect()
    };

    let no_delays: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let host_arch = |task: &TaskRecord| -> String {
        // Falling back to the task's own arch rather than inventing one:
        // a dataset swept before host arches were recorded has nothing to
        // say about where a task ran, and should not pretend otherwise.
        task.host_id
            .and_then(|id| dataset.host_arches.get(&format!("{}:{id}", task.instance)))
            .cloned()
            .unwrap_or_else(|| format!("{} (host unknown)", task.arch))
    };
    let srpm = arch_stats_by(&srpm_tasks, &no_delays, opts.include_failed, host_arch);
    let multi_arch = arch_stats(
        &of_shape(BuildShape::Multi),
        &bottleneck_delays,
        opts.include_failed,
    );
    let single_arch = arch_stats(
        &of_shape(BuildShape::Single),
        &no_delays,
        opts.include_failed,
    );
    let noarch_by_host = arch_stats_by(
        &of_shape(BuildShape::NoArch),
        &no_delays,
        opts.include_failed,
        host_arch,
    );

    let arches = arch_stats(&per_arch, &bottleneck_delays, opts.include_failed);
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
        // The period the report is about, which for a store query is
        // `period` — `since`/`until` are cleared there so a build's children
        // are not filtered twice. Utilisation needs a window length, so
        // without one it is simply not computed.
        health: crate::health::assess(
            dataset,
            &selected,
            opts.period.or(match (opts.since, opts.until) {
                (Some(a), Some(b)) => Some((a, b)),
                _ => None,
            }),
            opts.owners.is_empty(),
        ),
        // From the windows where there are any, and otherwise from the
        // rows: a period the store holds only in part has no window to
        // name it, and "Instances:" followed by nothing is no answer.
        instances: dataset
            .meta
            .windows
            .iter()
            .map(|w| w.instance.clone())
            .chain(dataset.builds.values().map(|b| b.instance.clone()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        // From `period` where the caller gave one: a store query has
        // already applied it, so `since`/`until` are unset there and the
        // report would otherwise say it covered no particular period.
        since: opts.since.or(opts.period.map(|p| p.0)),
        until: opts.until.or(opts.period.map(|p| p.1)),
        coverage: dataset.meta.windows.clone(),
        gaps: uncovered(dataset, opts),
        mixed_filtered_coverage: dataset.mixes_filtered_windows(),
        arches,
        official,
        scratch,
        srpm,
        multi_arch,
        single_arch,
        noarch_by_host,
        unattributed_tasks: unattributed,
        bottlenecked_builds,
        builds_in_window,
        builds_with_tasks,
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
    arch_stats_by(tasks, bottleneck_delays, include_failed, |t| t.arch.clone())
}

/// [`arch_stats`] with the row label chosen by the caller.
///
/// A `buildArch` task's own arch is the useful label — it is what was
/// compiled. A `noarch` task's is not: it says the payload runs anywhere,
/// while the question is which machine took it, so those rows are keyed
/// by the host's arch instead.
fn arch_stats_by(
    tasks: &[&TaskRecord],
    bottleneck_delays: &BTreeMap<&str, Vec<f64>>,
    include_failed: bool,
    label: impl Fn(&TaskRecord) -> String,
) -> Vec<ArchStats> {
    let mut queue: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut build: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for task in tasks {
        if in_queue_wait_population(task)
            && let Some(wait) = task.queue_wait()
        {
            queue.entry(label(task)).or_default().push(wait);
        }
        if in_build_time_population(task, include_failed)
            && let Some(time) = task.build_time()
        {
            build.entry(label(task)).or_default().push(time);
        }
    }
    let mut all_arches: std::collections::BTreeSet<String> = queue.keys().cloned().collect();
    all_arches.extend(build.keys().cloned());
    all_arches.extend(bottleneck_delays.keys().map(|a| a.to_string()));

    let mut rows: Vec<ArchStats> = all_arches
        .into_iter()
        .map(|arch| {
            let arch = arch.as_str();
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
/// The report's tables as CSV, one file per table.
///
/// One file each because a CSV holds one table, which is the whole reason
/// this is not simply "the report, in CSV": the text and JSON forms carry
/// every table for a period together, and CSV cannot.
///
/// Two differences from the human tables, both because a machine is
/// reading. Durations are plain seconds rather than "2.6m", since a column
/// mixing minutes and hours cannot be summed. And nothing is withheld for
/// having few samples — `--min-samples` protects a reader from reading too
/// much into three tasks, while a consumer wants the three and their count.
///
/// Every row carries the instance and the period it belongs to, so a year
/// of daily files can be concatenated without losing which day each row
/// came from.
pub fn csv_tables(output: &ReportOutput) -> Vec<(&'static str, String)> {
    const HEADER: &[&str] = &[
        "instance",
        "period_start",
        "period_end",
        "arch",
        "queued",
        "wait_median_s",
        "wait_p90_s",
        "wait_max_s",
        "built",
        "time_median_s",
        "time_p90_s",
        "time_max_s",
        "builds_bottlenecked",
        "bottleneck_median_delay_s",
        "bottleneck_total_delay_s",
    ];
    let instance = output.instances.join(" ");
    let start = crate::csv::date(output.since);
    let end = crate::csv::date(output.until);
    let rows = |stats: &[ArchStats]| -> Vec<Vec<String>> {
        stats
            .iter()
            .map(|s| {
                let (wait, time) = (s.queue_wait.as_ref(), s.build_time.as_ref());
                vec![
                    instance.clone(),
                    start.clone(),
                    end.clone(),
                    s.arch.clone(),
                    wait.map(|d| d.count.to_string()).unwrap_or_default(),
                    crate::csv::secs(wait.map(|d| d.median)),
                    crate::csv::secs(wait.map(|d| d.p90)),
                    crate::csv::secs(wait.map(|d| d.max)),
                    time.map(|d| d.count.to_string()).unwrap_or_default(),
                    crate::csv::secs(time.map(|d| d.median)),
                    crate::csv::secs(time.map(|d| d.p90)),
                    crate::csv::secs(time.map(|d| d.max)),
                    s.builds_bottlenecked.to_string(),
                    crate::csv::secs(s.bottleneck_median_delay),
                    crate::csv::secs(Some(s.bottleneck_total_delay)),
                ]
            })
            .collect()
    };

    // Named as the sections are titled, so a file can be matched to what a
    // reader saw in report.txt.
    let mut tables: Vec<(&'static str, &[ArchStats])> = vec![
        ("all-builds.csv", &output.arches),
        ("srpm-rebuild.csv", &output.srpm),
        ("multi-arch.csv", &output.multi_arch),
        ("single-arch.csv", &output.single_arch),
        ("noarch-by-host.csv", &output.noarch_by_host),
    ];
    if let Some(official) = &output.official {
        tables.push(("official.csv", official));
    }
    if let Some(scratch) = &output.scratch {
        tables.push(("scratch.csv", scratch));
    }
    // An empty table is still written: "no noarch builds this week" is a
    // finding, and a missing file is indistinguishable from a run that
    // failed halfway.
    tables
        .into_iter()
        .map(|(name, stats)| (name, crate::csv::table(HEADER, &rows(stats))))
        .collect()
}

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
    // Warnings first, and unconditionally: a threshold crossed at the foot
    // of a long report is a threshold nobody read.
    for w in &output.health.warnings {
        let _ = writeln!(o, "warning: {}", w.text);
    }
    if output.mixed_filtered_coverage {
        let _ = writeln!(
            o,
            "warning: dataset mixes scoped and full fetches — counts \
             under-represent the full instance"
        );
    }
    for (instance, from, to) in &output.gaps {
        // Dates, not unix seconds: this fires whenever a report covers a
        // period the store holds only in part, which is a normal thing to
        // ask for, and "no data between unix 1783036800 and 1783123200"
        // makes a reader do arithmetic to learn which days are missing.
        let day = |ts: f64| {
            chrono::DateTime::from_timestamp(ts as i64, 0)
                .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| format!("unix {ts:.0}"))
        };
        let _ = writeln!(
            o,
            "warning: {instance} is not held in full between {} and {} — \
             those days are excluded from the figures below",
            day(*from),
            day(*to)
        );
    }
    // A bottleneck count alone says nothing about how much of the day it
    // covers, so the day's builds are given with it — and the difference
    // is accounted for rather than left as a puzzle. The three build
    // figures sum to the total.
    let share = match output.builds_in_window {
        0 => String::new(),
        total => format!(
            " ({:.0}%)",
            output.bottlenecked_builds as f64 / total as f64 * 100.0
        ),
    };
    let unattributable = output
        .builds_with_tasks
        .saturating_sub(output.bottlenecked_builds);
    let no_tasks = output
        .builds_in_window
        .saturating_sub(output.builds_with_tasks);
    let _ = writeln!(
        o,
        "Builds completed: {}; with an arch on the critical path: {}{share}; \
         single-arch, failed or untimed: {}; no per-arch tasks swept: {}; \
         unattributed tasks: {}",
        output.builds_in_window,
        output.bottlenecked_builds,
        unattributable,
        no_tasks,
        output.unattributed_tasks
    );

    render_rows(&mut o, "All builds", &output.arches, min_samples);
    // Every stage of a build gets its wait and run time reported, whether
    // or not a bottleneck can be pinned on it: attribution needs two
    // arches racing, but a slow queue or a slow machine costs time
    // regardless, and those cases were previously visible only as part of
    // the combined rows.
    render_table(
        &mut o,
        "SRPM rebuild (by host arch)",
        &output.srpm,
        min_samples,
        "host arch",
        false,
    );
    render_rows(
        &mut o,
        "Multi-arch builds (attribution applies)",
        &output.multi_arch,
        min_samples,
    );
    render_table(
        &mut o,
        "Single-arch builds",
        &output.single_arch,
        min_samples,
        "arch",
        false,
    );
    render_table(
        &mut o,
        "noarch builds (by host arch)",
        &output.noarch_by_host,
        min_samples,
        "host arch",
        false,
    );
    if let Some(official) = &output.official {
        render_rows(&mut o, "Official builds", official, min_samples);
    }
    if let Some(scratch) = &output.scratch {
        render_rows(&mut o, "Scratch builds", scratch, min_samples);
    }
    render_health(&mut o, &output.health, min_samples);
    // These tables get pasted into tickets and threads, so they
    // must explain themselves. Backticked bullets stay readable in
    // a terminal and render as a list in Markdown (a bare leading
    // `*` would italicize).
    let _ = writeln!(
        o,
        "\nHeader legend:\n\
         - `Builds completed` — parent build tasks that finished in the \
         window; the\n  three figures after it sum to this.\n\
         - `with an arch on the critical path` — builds where one arch \
         finished last,\n  so an arch can be held responsible for the \
         wall-clock time.\n\
         - `single-arch, failed or untimed` — builds attribution cannot \
         apply to: one\n  arch alone has nothing to be slower than, and \
         a failed or untimed task\n  gives no completion to compare.\n\
         - `no per-arch tasks swept` — builds whose children are not in \
         the dataset,\n  usually an arch filter or a sweep that did not \
         reach them.\n\
         - `unattributed tasks` — per-arch tasks whose parent build was \
         not swept, so\n  their scratch-ness is unknown; they are \
         counted in the combined stats only.\n\
         \nSection legend:\n\
         - `All builds` — every per-arch build in the window, whatever \
         shape of build it\n  came from.\n\
         - `SRPM rebuild` — the source rebuild each build starts with, \
         keyed by the arch\n  of the host that ran it: the hub picks \
         that host regardless of what the\n  build targets, so a \
         package can queue behind a machine it does not build\n  for.\n\
         - `Multi-arch builds` — builds made for two or more arches. \
         Only these can\n  have a bottleneck, since attribution needs a \
         runner-up to measure against.\n\
         - `Single-arch builds` — built for one arch, so wait and run \
         time stand alone.\n\
         - `noarch builds` — packages that build once for every arch, \
         keyed by the host\n  that took them: the payload is portable, \
         the machine is not.\n\
         \nColumn legend:\n\
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
/// The health tables: per class, per cohort, and how builder time went.
///
/// Per class because summing them produced a headline that described releng
/// waiting for releng; per cohort because a population median hid, for
/// eighteen months, that ten people were absorbing most of the delay.
fn render_health(o: &mut String, health: &crate::health::Health, min_samples: usize) {
    use std::fmt::Write as _;
    let dur = |d: f64| {
        if d >= 3600.0 {
            format!("{:.1}h", d / 3600.0)
        } else if d >= 60.0 {
            format!("{:.0}m", d / 60.0)
        } else {
            format!("{d:.0}s")
        }
    };
    let pops: Vec<_> = health
        .arches
        .iter()
        // A median over a handful of builds is noise, and --min-samples
        // is the reader's statement of how much noise they will look at.
        .filter(|a| a.service.iter().any(|p| p.tasks >= min_samples))
        .collect();
    if !pops.is_empty() {
        let _ = writeln!(o, "\nBuild time by population\n");
        // A finer formatter than `dur` here: these medians sit between one
        // and three minutes, where whole minutes render nine different
        // families as "2m" and the table says nothing.
        let fine = |d: f64| {
            if d >= 3600.0 {
                format!("{:.1}h", d / 3600.0)
            } else {
                format!("{:.1}m", d / 60.0)
            }
        };
        let _ = writeln!(o, "| arch | toolchain | builds | median | p90 |");
        let _ = writeln!(o, "|---|---|---:|---:|---:|");
        for a in &pops {
            for p in a.service.iter().filter(|p| p.tasks >= min_samples) {
                let _ = writeln!(
                    o,
                    "| {} | {} | {} | {} | {} |",
                    a.arch,
                    p.name,
                    p.tasks,
                    p.p50.map(fine).unwrap_or_else(|| "-".into()),
                    p.p90.map(fine).unwrap_or_else(|| "-".into())
                );
            }
        }
        let _ = writeln!(
            o,
            "\nOne row per build toolchain, guessed from the package name. \
             Read a column\ndown across periods, never a row across within \
             one: the gap between two\nfamilies in the same window is mostly \
             how big their packages are. `other`\nis everything the prefixes \
             do not name, mostly C and C++.\n\nNoarch builds are left out. \
             Koji records their one task against whichever\nhost it picked, \
             so they are no architecture's compile cost — s390x hosted\n2,291 \
             of them in F42's rebuild and 5 in F45's."
        );
    }
    // One row per pool, not per architecture: where hosts are shared the
    // per-architecture figures each describe a slice of the same machines.
    let mut pools: Vec<(&str, f64, f64, f64)> = Vec::new();
    for a in &health.arches {
        if let (Some(p), Some(cap), Some(off), Some(u)) =
            (&a.pool, a.pool_capacity, a.pool_offered, a.utilisation)
            && !pools.iter().any(|(name, ..)| *name == p.as_str())
        {
            pools.push((p, cap, off, u));
        }
    }
    if !pools.is_empty() {
        let _ = writeln!(o, "\nBuilder pools\n");
        let _ = writeln!(o, "| architectures | capacity | offered | utilisation |");
        let _ = writeln!(o, "|---|---:|---:|---:|");
        for (name, cap, off, u) in &pools {
            let _ = writeln!(o, "| {name} | {cap:.1} | {off:.1} | {u:.2} |");
        }
        let _ = writeln!(
            o,
            "\nA pool is the set of architectures served by the same \
             builders, with each\nhost's weight counted once. Utilisation \
             belongs here rather than to an\narchitecture: Fedora's i386 \
             builders are its x86_64 builders, so counting\nthem under each \
             separately showed i386 at 0.19 and x86_64 at 0.35 while \
             the\nmachines they share were at 0.52 — and made 136 weight \
             units of headroom\nappear that could not be redeployed, being \
             x86_64's own counted twice."
        );
    }
    let streams: Vec<_> = health
        .streams
        .iter()
        .filter(|s| s.tasks >= min_samples)
        .collect();
    if !streams.is_empty() {
        let _ = writeln!(o, "\nWhere builder time went, by distribution\n");
        let _ = writeln!(
            o,
            "| arch | distribution | builds | builder hours | share |"
        );
        let _ = writeln!(o, "|---|---|---:|---:|---:|");
        for s in &streams {
            let _ = writeln!(
                o,
                "| {} | {} | {} | {:.0} | {:.1}% |",
                s.arch, s.stream, s.tasks, s.builder_hours, s.pct
            );
        }
        let _ = writeln!(
            o,
            "\nWhat narrowing an architecture's scope would free, against \
             what adding\nbuilders would. The two are alternative answers to \
             the same queue and are\nnot comparable until this table \
             has numbers in it. See the builder pools above for what \
             each\narchitecture's capacity is actually shared with."
        );
    }
    let stragglers: Vec<_> = health
        .stragglers
        .iter()
        .filter(|s| s.builds >= min_samples)
        .collect();
    if !stragglers.is_empty() {
        let _ = writeln!(
            o,
            "\nWhich architecture finished last (multi-arch builds only)\n"
        );
        let _ = writeln!(
            o,
            "| arch | builds finished last | share | median spread | p90 | max |"
        );
        let _ = writeln!(o, "|---|---:|---:|---:|---:|---:|");
        for s in &stragglers {
            let Some(d) = &s.spread else { continue };
            let _ = writeln!(
                o,
                "| {} | {} | {:.1}% | {} | {} | {} |",
                s.arch,
                s.builds,
                s.pct,
                dur(d.median),
                dur(d.p90),
                dur(d.max)
            );
        }
        let _ = writeln!(
            o,
            "\nSpread is the gap between the first architecture finishing a \
             build and the\nlast. A build is not output until every one of \
             them has finished. Distributed\nrather than averaged: the tail \
             is long, and a mean sits well above the\nordinary build."
        );
    }
    if !health.classes.is_empty() {
        let _ = writeln!(o, "\nQueue wait by class of build (never summed)\n");
        let _ = writeln!(o, "| arch | class | tasks | median | p90 | max | >1h |");
        let _ = writeln!(o, "|---|---|---:|---:|---:|---:|---:|");
        for c in &health.classes {
            let Some(w) = &c.queue_wait.as_ref().filter(|w| w.count >= min_samples) else {
                continue;
            };
            let _ = writeln!(
                o,
                "| {} | {} | {} | {} | {} | {} | {:.1}% |",
                c.arch,
                c.class,
                w.count,
                dur(w.median),
                dur(w.p90),
                dur(w.max),
                c.over_hour_pct
            );
        }
    }
    if !health.cohorts.is_empty() {
        let _ = writeln!(
            o,
            "\nQueue wait by submitter volume, official human builds\n\n             Bands are by how much each person submits; nobody is named. Use\n             `report --owner NAME` to see one account.\n"
        );
        let _ = writeln!(
            o,
            "| arch | cohort | people | tasks | share | median | p90 | >1h |"
        );
        let _ = writeln!(o, "|---|---|---:|---:|---:|---:|---:|---:|");
        for c in &health.cohorts {
            let Some(w) = &c.queue_wait.as_ref().filter(|w| w.count >= min_samples) else {
                continue;
            };
            let _ = writeln!(
                o,
                "| {} | {} | {} | {} | {:.0}% | {} | {} | {:.1}% |",
                c.arch,
                c.cohort,
                c.people,
                w.count,
                c.share_of_tasks,
                dur(w.median),
                dur(w.p90),
                c.over_hour_pct
            );
        }
    }
    if !health.arches.is_empty() {
        let _ = writeln!(o, "\nWhere builder time went\n");
        let _ = writeln!(
            o,
            "| arch | builder hours | utilisation | wasted | tail (>6h) | tail tasks |"
        );
        let _ = writeln!(o, "|---|---:|---:|---:|---:|---:|");
        for a in &health.arches {
            if a.tasks < min_samples {
                continue;
            }
            let _ = writeln!(
                o,
                "| {} | {:.0} | {} | {:.1}% | {:.1}% | {} |",
                a.arch,
                a.builder_hours,
                a.utilisation
                    .map(|u| format!("{u:.2}"))
                    .unwrap_or_else(|| "—".into()),
                a.wasted_pct,
                a.tail_pct,
                a.tail_tasks
            );
        }
        let _ = writeln!(
            o,
            "\n- `utilisation` — weight in use over enabled builder weight. \n\
             \u{20}\u{20}Queueing is nonlinear in it: below about 0.6 waits are minutes, \n\
             \u{20}\u{20}above about 0.7 they are hours.\n\
             - `wasted` — builder time in failed or cancelled tasks: capacity \n\
             \u{20}\u{20}spent producing nothing, and the cheapest kind to recover.\n\
             - `tail` — builder time in tasks over six hours. A handful of hung \n\
             \u{20}\u{20}builds can hold a fifth of an architecture."
        );
    }
}

fn render_rows(o: &mut String, title: &str, rows: &[ArchStats], min_samples: usize) {
    render_table(o, title, rows, min_samples, "arch", true)
}

/// [`render_rows`] with the first column named, and the attribution
/// columns omitted where they would be meaningless — a single-arch build
/// has nothing to be slower than, and the source rebuild races nothing.
fn render_table(
    o: &mut String,
    title: &str,
    rows: &[ArchStats],
    min_samples: usize,
    first_column: &str,
    with_attribution: bool,
) {
    use std::fmt::Write as _;
    if rows.is_empty() {
        return;
    }
    let columns = if with_attribution { 10 } else { 7 };
    let headers: [String; 10] = [
        first_column.to_string(),
        "queued".to_string(),
        "med-wait".to_string(),
        "p90-wait".to_string(),
        "built".to_string(),
        "med-time".to_string(),
        "p90-time".to_string(),
        "bottleneck".to_string(),
        "med-delay".to_string(),
        "tot-delay".to_string(),
    ];
    let headers = &headers[..columns];

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
        let widths: Vec<usize> = (0..columns)
            .map(|col| {
                cells
                    .iter()
                    .map(|row| row[col].chars().count())
                    .chain([headers[col].len()])
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
        line(o, headers);
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
            line(o, &row[..columns]);
        }
    }
    if !thin.is_empty() {
        let _ = writeln!(o, "\nBelow --min-samples: {}.", thin.join(", "));
    }
}

#[cfg(test)]
mod tests {
    use crate::dataset::{BUILD_SRPM_FROM_SCM, REBUILD_SRPM};

    #[test]
    fn both_names_for_the_source_rebuild_are_counted() {
        // rebuildSRPM is a scratch build submitted as an SRPM;
        // buildSRPMFromSCM is a build from dist-git. Knowing only one
        // reports on part of the population without saying which part.
        let mut ds = shaped_dataset();
        let mut from_scm = task(41, 1, "noarch", 8.0, 60.0);
        from_scm.method = BUILD_SRPM_FROM_SCM.to_string();
        from_scm.host_id = Some(2);
        ds.tasks.insert(from_scm.key(), from_scm);

        let out = run(&ds, &ReportOpts::default());
        let rows: Vec<&str> = out.srpm.iter().map(|r| r.arch.as_str()).collect();
        assert!(rows.contains(&"ppc64le"), "{rows:?}");
        assert!(rows.contains(&"s390x"), "{rows:?}");
        // And neither name is mistaken for a racing arch.
        assert!(!out.multi_arch.iter().any(|r| r.arch == "noarch"));
    }

    /// A dataset holding one build of each shape, plus the source
    /// rebuilds, with hosts of known arches.
    fn shaped_dataset() -> Dataset {
        let mut ds = Dataset::new();
        let on_host = |mut t: TaskRecord, host: i64, method: &str| {
            t.host_id = Some(host);
            t.method = method.to_string();
            t
        };
        // Two arches raced: attribution applies.
        ds.builds.insert("fedora:1".into(), build(1, false));
        for t in [
            on_host(task(11, 1, "x86_64", 10.0, 100.0), 1, BUILD_ARCH),
            on_host(task(12, 1, "s390x", 10.0, 400.0), 3, BUILD_ARCH),
            on_host(task(13, 1, "noarch", 5.0, 40.0), 3, REBUILD_SRPM),
        ] {
            ds.tasks.insert(t.key(), t);
        }
        // One arch only.
        ds.builds.insert("fedora:2".into(), build(2, false));
        {
            let t = on_host(task(21, 2, "ppc64le", 10.0, 200.0), 2, BUILD_ARCH);
            ds.tasks.insert(t.key(), t);
        }
        // A noarch package, built on an s390x machine.
        ds.builds.insert("fedora:3".into(), build(3, false));
        {
            let t = on_host(task(31, 3, "noarch", 10.0, 70.0), 3, BUILD_ARCH);
            ds.tasks.insert(t.key(), t);
        }
        ds.host_arches
            .insert("fedora:1".into(), "x86_64 i386".into());
        ds.host_arches.insert("fedora:2".into(), "ppc64le".into());
        ds.host_arches.insert("fedora:3".into(), "s390x".into());
        ds
    }

    #[test]
    fn builds_are_reported_by_the_shape_they_have() {
        let out = run(&shaped_dataset(), &ReportOpts::default());
        let arches =
            |rows: &[ArchStats]| -> Vec<String> { rows.iter().map(|r| r.arch.clone()).collect() };

        // Only the build with two arches can have a bottleneck.
        assert_eq!(arches(&out.multi_arch), vec!["s390x", "x86_64"]);
        let s390x = &out.multi_arch[0];
        assert_eq!(s390x.builds_bottlenecked, 1);
        // One arch has nothing to be slower than, so it is reported
        // apart, with its raw wait and run time.
        assert_eq!(arches(&out.single_arch), vec!["ppc64le"]);
        assert_eq!(
            out.single_arch[0].build_time.as_ref().unwrap().median,
            190.0
        );
        assert_eq!(out.single_arch[0].builds_bottlenecked, 0);

        // A noarch package says nothing about where it ran, so it is
        // keyed by the machine that took it.
        assert_eq!(arches(&out.noarch_by_host), vec!["s390x"]);
        assert_eq!(
            out.noarch_by_host[0].build_time.as_ref().unwrap().median,
            60.0
        );

        // The source rebuild is its own stage, on a host of the hub's
        // choosing — here an s390x one, for a build that also targets
        // x86_64.
        assert_eq!(arches(&out.srpm), vec!["s390x"]);
        assert_eq!(out.srpm[0].queue_wait.as_ref().unwrap().median, 5.0);
    }

    #[test]
    fn the_srpm_step_is_not_treated_as_a_racing_arch() {
        // The rebuild finishes before both arches. Counted as one, it
        // would be the earliest "arch" and would inflate the marginal
        // delay attributed to the real bottleneck.
        let out = run(&shaped_dataset(), &ReportOpts::default());
        let s390x = out
            .multi_arch
            .iter()
            .find(|r| r.arch == "s390x")
            .expect("s390x row");
        // 400 - 100, the gap to the runner-up arch, not to the rebuild.
        assert_eq!(s390x.bottleneck_total_delay, 300.0);
        // And the rebuild is absent from the per-arch tables.
        assert!(!out.multi_arch.iter().any(|r| r.arch == "noarch"));
    }

    #[test]
    fn a_dataset_without_host_arches_says_so() {
        // Datasets swept before host arches were recorded can still be
        // reported on; the row admits what it does not know rather than
        // guessing an arch or dropping the tasks.
        let mut ds = shaped_dataset();
        ds.host_arches.clear();
        let out = run(&ds, &ReportOpts::default());
        assert_eq!(
            out.noarch_by_host
                .iter()
                .map(|r| r.arch.clone())
                .collect::<Vec<_>>(),
            vec!["noarch (host unknown)"]
        );
    }
    use super::*;
    use crate::dataset::{BuildRecord, Dataset};

    fn task(id: i64, parent: i64, arch: &str, start: f64, completion: f64) -> TaskRecord {
        TaskRecord {
            instance: "fedora".to_string(),
            task_id: id,
            parent: Some(parent),
            method: "buildArch".to_string(),
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
            host_id: None,
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
        // The denominator a bottleneck count needs, and the figures
        // that account for the difference.
        assert_eq!(out.builds_in_window, 2);
        assert_eq!(out.builds_with_tasks, 2);
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
    fn an_owner_filter_narrows_to_that_account() {
        // The question a maintainer asks of a published store: how did my
        // own builds fare. Build 1 is alice's; nothing else is.
        let mut ds = dataset();
        let mut mine = build(1, false);
        mine.owner = Some("alice".into());
        ds.builds.insert("fedora:1".into(), mine);
        let mut theirs = build(2, true);
        theirs.owner = Some("bob".into());
        ds.builds.insert("fedora:2".into(), theirs);

        let out = run(
            &ds,
            &ReportOpts {
                owners: vec!["alice".into()],
                ..Default::default()
            },
        );
        let arches: Vec<&str> = out.arches.iter().map(|r| r.arch.as_str()).collect();
        assert!(arches.contains(&"aarch64"), "{arches:?}"); // alice's build
        assert!(!arches.contains(&"ppc64le"), "{arches:?}"); // bob's

        // An account nobody used reports nothing rather than everything,
        // which is the failure mode of an ignored filter.
        let none = run(
            &ds,
            &ReportOpts {
                owners: vec!["nobody".into()],
                ..Default::default()
            },
        );
        assert!(none.arches.is_empty(), "{:?}", none.arches);
    }

    #[test]
    fn a_package_filter_narrows_and_an_unattributed_task_drops_out() {
        let mut ds = dataset();
        let mut b = build(1, false);
        b.package = Some("foo".into());
        ds.builds.insert("fedora:1".into(), b);
        let mut other = build(2, true);
        other.package = Some("bar".into());
        ds.builds.insert("fedora:2".into(), other);

        let out = run(
            &ds,
            &ReportOpts {
                packages: vec!["foo".into()],
                ..Default::default()
            },
        );
        let arches: Vec<&str> = out.arches.iter().map(|r| r.arch.as_str()).collect();
        assert!(arches.contains(&"aarch64"), "{arches:?}");
        assert!(!arches.contains(&"ppc64le"), "{arches:?}");
        // The dataset's unattributed s390x task cannot be shown to belong
        // to `foo`, so a narrowed report must not count it in.
        let s390x = out.arches.iter().find(|r| r.arch == "s390x");
        assert_eq!(
            s390x.and_then(|r| r.queue_wait.as_ref()).map(|w| w.count),
            Some(1),
            "{:?}",
            out.arches
        );
    }

    #[test]
    fn owner_and_package_together_are_an_intersection() {
        let mut ds = dataset();
        let mut b = build(1, false);
        b.owner = Some("alice".into());
        b.package = Some("foo".into());
        ds.builds.insert("fedora:1".into(), b);

        // Right owner, wrong package: nothing.
        let out = run(
            &ds,
            &ReportOpts {
                owners: vec!["alice".into()],
                packages: vec!["bar".into()],
                ..Default::default()
            },
        );
        assert!(out.arches.is_empty(), "{:?}", out.arches);
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
    fn the_class_filter_selects_by_what_submitted_the_build() {
        let mut ds = dataset();
        // Build 3 is a mass rebuild; the fixture's others are official.
        let mut b = build(3, false);
        b.target = Some("f45-rebuild".to_string());
        b.owner = Some("releng".to_string());
        ds.builds.insert("fedora:3".into(), b);
        for t in [
            task(41, 3, "s390x", 10.0, 500.0),
            task(42, 3, "x86_64", 10.0, 120.0),
        ] {
            ds.tasks.insert(t.key(), t);
        }

        let rebuild_only = run(
            &ds,
            &ReportOpts {
                classes: vec![Class::MassRebuild],
                ..Default::default()
            },
        );
        assert_eq!(rebuild_only.bottlenecked_builds, 1);

        let official_only = run(
            &ds,
            &ReportOpts {
                classes: vec![Class::Official],
                ..Default::default()
            },
        );
        assert_eq!(official_only.bottlenecked_builds, 1);

        // Both together is not the same as no filter: the unattributed
        // task has no build and so has no class either.
        let both = run(
            &ds,
            &ReportOpts {
                classes: vec![Class::MassRebuild, Class::Official],
                ..Default::default()
            },
        );
        assert_eq!(both.unattributed_tasks, 0);
        assert_eq!(run(&ds, &ReportOpts::default()).unattributed_tasks, 1);
    }

    #[test]
    fn a_narrowed_report_narrows_its_own_denominator() {
        // "Builds completed" is the denominator every share above it is
        // read against, so a report narrowed to one owner that counted the
        // whole fleet was quoting somebody else's total.
        let mut ds = dataset();
        let mut b = build(3, false);
        b.owner = Some("bob".to_string());
        b.package = Some("bar".to_string());
        ds.builds.insert("fedora:3".into(), b);
        ds.tasks
            .insert("fedora:43".into(), task(43, 3, "s390x", 10.0, 60.0));

        assert_eq!(run(&ds, &ReportOpts::default()).builds_in_window, 3);
        for (opts, want) in [
            (
                ReportOpts {
                    owners: vec!["bob".to_string()],
                    ..Default::default()
                },
                1,
            ),
            (
                ReportOpts {
                    packages: vec!["foo".to_string()],
                    ..Default::default()
                },
                2,
            ),
            (
                ReportOpts {
                    classes: vec![Class::Official],
                    ..Default::default()
                },
                // Two of the three: the fixture's build 2 is scratch, so
                // its class is hand-scratch rather than official.
                2,
            ),
        ] {
            assert_eq!(run(&ds, &opts).builds_in_window, want);
        }
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
