// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The mergeable on-disk dataset.
//!
//! A single JSON file, load-mutate-save (the pkg-health report
//! pattern; JSON rather than TOML because a month of Fedora
//! builds is ~10^5 task records). Records are keyed
//! `"<instance>:<task_id>"` — task IDs are only unique per Koji
//! instance — in BTreeMaps so output is deterministic and merges
//! are order-independent. Independently collected files pool via
//! [`Dataset::merge`], and [`DatasetMeta::windows`] records what
//! time ranges each file actually covers so reports can call out
//! coverage gaps instead of silently under-counting.
//!
//! All record timestamps are the hub's UTC unix `f64` seconds
//! (`*_ts` fields) — the string `*_time` forms are never stored.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

/// Bump when the on-disk shape changes incompatibly. Loading a
/// file with a NEWER version errors (old tool, new file); older
/// versions are migrated or rejected explicitly.
pub const SCHEMA_VERSION: u32 = 1;

/// A pooled collection of build/task records from one or more
/// Koji instances.
#[derive(Debug, Clone)]
pub struct Dataset {
    pub meta: DatasetMeta,
    /// Parent `build` tasks, keyed `"<instance>:<task_id>"`.
    pub builds: BTreeMap<String, BuildRecord>,
    /// Child tasks of the builds, keyed `"<instance>:<task_id>"` — the
    /// per-arch `buildArch` builds and the `rebuildSRPM` that precedes
    /// them. See `TaskRecord::method`.
    pub tasks: BTreeMap<String, TaskRecord>,
    /// `"<instance>:<host_id>"` → builder hostname.
    pub hosts: BTreeMap<String, String>,
    /// `"<instance>:<channel_id>"` → channel name.
    pub channels: BTreeMap<String, String>,
    /// `"<instance>:<host_id>"` → the arches that host serves, as the
    /// hub reports them (`x86_64 i386`, `s390x`).
    ///
    /// What answers "where did this actually run" for a `noarch` task,
    /// whose own arch says only that it could have gone anywhere. The
    /// hub's host names do not carry it reliably — Fedora has builders
    /// called `xenbuilder3` and `ppc1` — so it comes from `listHosts`.
    pub host_arches: BTreeMap<String, String>,
}

/// One architecture's build. These are what race each other, so they are
/// the only tasks a bottleneck can be attributed to.
pub const BUILD_ARCH: &str = "buildArch";

/// The source rebuild for a scratch build submitted as an SRPM: the hub
/// rebuilds what it was handed. Precedes the per-arch builds rather than
/// racing them, on whichever arch the hub picked.
pub const REBUILD_SRPM: &str = "rebuildSRPM";

/// The same stage for a build from dist-git, where the source is checked
/// out and packaged rather than uploaded.
///
/// Both names are in use — the flow decides which — so a tool that knows
/// only one reports on part of the population without being able to say
/// which part. Scratch builds dominate by volume, so knowing only
/// `rebuildSRPM` would have looked like near-complete coverage while
/// missing every dist-git build.
pub const BUILD_SRPM_FROM_SCM: &str = "buildSRPMFromSCM";

/// Whether a method is the source-rebuild stage under either name.
pub fn is_srpm_step(method: &str) -> bool {
    method == REBUILD_SRPM || method == BUILD_SRPM_FROM_SCM
}

#[derive(Debug, Clone)]
pub struct DatasetMeta {
    pub schema_version: u32,
    /// When this file was last written.
    pub generated: DateTime<Utc>,
    /// Completion-time windows the records were swept from.
    pub windows: Vec<FetchWindow>,
}

/// One fetch's coverage: tasks completing in the half-open
/// window `[from, to)` on `instance` — adjacent daily windows
/// share a boundary instant without double-counting it.
///
/// Serialisable, unlike the rest of this module: a report states the
/// coverage it was computed over, so these travel in report JSON.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct FetchWindow {
    pub instance: String,
    /// UTC unix seconds, inclusive lower bound.
    pub from: f64,
    /// UTC unix seconds, exclusive upper bound.
    pub to: f64,
    pub fetched: DateTime<Utc>,
    /// True when the fetch was scoped (--owner/--package/
    /// --inventory) — such a window is NOT full coverage, and
    /// merge/report warn when mixing it with unfiltered ones.
    pub filtered: bool,
}

/// A parent `build` task.
#[derive(Debug, Clone)]
pub struct BuildRecord {
    pub instance: String,
    pub task_id: i64,
    /// Source package name, when extractable from the request.
    pub package: Option<String>,
    pub nvr: Option<String>,
    pub target: Option<String>,
    pub owner: Option<String>,
    pub scratch: bool,
    pub state: i64,
    pub create_ts: f64,
    pub start_ts: Option<f64>,
    pub completion_ts: Option<f64>,
    pub priority: Option<i64>,
    /// Builder the parent task itself was scheduled on.
    ///
    /// The `build` task coordinates rather than compiles, but it holds a
    /// slot on a real machine while it waits for its children — one was
    /// seen occupying an s390x builder for five minutes to produce a
    /// noarch package whose actual work ran on x86.
    pub host_id: Option<i64>,
}

/// One child task of a build: an architecture's build, or the source
/// rebuild that precedes them.
#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub instance: String,
    pub task_id: i64,
    /// Parent build task id (same instance); tasks whose parent
    /// wasn't captured are reported as unattributed.
    pub parent: Option<i64>,
    pub arch: String,
    /// Koji task method. `buildArch` is one architecture's build;
    /// `rebuildSRPM` is the source rebuild that precedes them all, on
    /// whichever arch the hub happened to pick. Defaults to `buildArch`
    /// for datasets written before the SRPM step was collected.
    pub method: String,
    pub package: Option<String>,
    pub state: i64,
    pub create_ts: f64,
    pub start_ts: Option<f64>,
    pub completion_ts: Option<f64>,
    pub host_id: Option<i64>,
    pub channel_id: Option<i64>,
    pub weight: Option<f64>,
}

impl TaskRecord {
    /// Seconds spent queued before a builder picked the task up.
    pub fn queue_wait(&self) -> Option<f64> {
        Some(self.start_ts? - self.create_ts)
    }

    /// Seconds spent building (started → completed).
    pub fn build_time(&self) -> Option<f64> {
        Some(self.completion_ts? - self.start_ts?)
    }

    /// The dataset key for this record.
    pub fn key(&self) -> String {
        format!("{}:{}", self.instance, self.task_id)
    }
}

impl BuildRecord {
    /// The dataset key for this record.
    pub fn key(&self) -> String {
        format!("{}:{}", self.instance, self.task_id)
    }
}

/// What a merge did, for reporting.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MergeStats {
    pub added: usize,
    pub replaced: usize,
    pub unchanged: usize,
}

impl Default for Dataset {
    fn default() -> Self {
        Self::new()
    }
}

impl Dataset {
    pub fn new() -> Self {
        Self {
            meta: DatasetMeta {
                schema_version: SCHEMA_VERSION,
                generated: Utc::now(),
                windows: Vec::new(),
            },
            builds: BTreeMap::new(),
            tasks: BTreeMap::new(),
            hosts: BTreeMap::new(),
            channels: BTreeMap::new(),
            host_arches: BTreeMap::new(),
        }
    }

    /// Union `other` into `self`. Records dedupe by key; on
    /// conflict the record with a completion timestamp (or the
    /// newer one) wins, so a re-sweep refreshes still-running
    /// tasks. Windows are appended then coalesced per instance.
    pub fn merge(&mut self, other: Dataset) -> MergeStats {
        let mut stats = MergeStats::default();
        fn newer(a: Option<f64>, b: Option<f64>) -> bool {
            match (a, b) {
                (Some(x), Some(y)) => x > y,
                (Some(_), None) => true,
                _ => false,
            }
        }
        for (key, record) in other.tasks {
            match self.tasks.get(&key) {
                None => {
                    self.tasks.insert(key, record);
                    stats.added += 1;
                }
                Some(existing) if newer(record.completion_ts, existing.completion_ts) => {
                    self.tasks.insert(key, record);
                    stats.replaced += 1;
                }
                Some(_) => stats.unchanged += 1,
            }
        }
        for (key, record) in other.builds {
            match self.builds.get(&key) {
                None => {
                    self.builds.insert(key, record);
                    stats.added += 1;
                }
                Some(existing) if newer(record.completion_ts, existing.completion_ts) => {
                    self.builds.insert(key, record);
                    stats.replaced += 1;
                }
                Some(_) => stats.unchanged += 1,
            }
        }
        self.hosts.extend(other.hosts);
        self.host_arches.extend(other.host_arches);
        self.channels.extend(other.channels);
        self.meta.windows.extend(other.meta.windows);
        coalesce_windows(&mut self.meta.windows);
        self.meta.generated = Utc::now();
        stats
    }

    /// Holes between this dataset's coverage windows, per
    /// instance: `(instance, gap_from, gap_to)`.
    pub fn coverage_gaps(&self) -> Vec<(String, f64, f64)> {
        let mut by_instance: BTreeMap<&str, Vec<&FetchWindow>> = BTreeMap::new();
        for w in &self.meta.windows {
            by_instance.entry(&w.instance).or_default().push(w);
        }
        let mut gaps = Vec::new();
        for (instance, mut windows) in by_instance {
            windows.sort_by(|a, b| a.from.total_cmp(&b.from));
            for pair in windows.windows(2) {
                if pair[1].from > pair[0].to {
                    gaps.push((instance.to_string(), pair[0].to, pair[1].from));
                }
            }
        }
        gaps
    }

    /// Whether the dataset mixes filtered (scoped) and unfiltered
    /// windows — a coverage honesty warning for reports.
    pub fn mixes_filtered_windows(&self) -> bool {
        let filtered = self.meta.windows.iter().filter(|w| w.filtered).count();
        filtered > 0 && filtered < self.meta.windows.len()
    }
}

/// Merge overlapping or touching windows of the same instance and
/// filteredness, so repeated overlapping sweeps don't accrete
/// window entries forever.
fn coalesce_windows(windows: &mut Vec<FetchWindow>) {
    windows.sort_by(|a, b| {
        (a.instance.as_str(), a.filtered)
            .cmp(&(b.instance.as_str(), b.filtered))
            .then(a.from.total_cmp(&b.from))
    });
    let mut out: Vec<FetchWindow> = Vec::with_capacity(windows.len());
    for w in windows.drain(..) {
        match out.last_mut() {
            Some(last)
                if last.instance == w.instance
                    && last.filtered == w.filtered
                    && w.from <= last.to =>
            {
                if w.to > last.to {
                    last.to = w.to;
                }
                if w.fetched > last.fetched {
                    last.fetched = w.fetched;
                }
            }
            _ => out.push(w),
        }
    }
    *windows = out;
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn task(instance: &str, id: i64, arch: &str, completion: Option<f64>) -> TaskRecord {
        TaskRecord {
            instance: instance.to_string(),
            task_id: id,
            parent: Some(1),
            method: "buildArch".to_string(),
            arch: arch.to_string(),
            package: Some("foo".to_string()),
            state: 2,
            create_ts: 1000.0,
            start_ts: Some(1060.0),
            completion_ts: completion,
            host_id: Some(643),
            channel_id: Some(1),
            weight: None,
        }
    }

    fn window(instance: &str, from: f64, to: f64, filtered: bool) -> FetchWindow {
        FetchWindow {
            instance: instance.to_string(),
            from,
            to,
            fetched: Utc::now(),
            filtered,
        }
    }

    #[test]
    fn queue_wait_and_build_time() {
        let t = task("fedora", 1, "s390x", Some(1500.0));
        assert_eq!(t.queue_wait(), Some(60.0));
        assert_eq!(t.build_time(), Some(440.0));
        let unstarted = TaskRecord {
            start_ts: None,
            ..t.clone()
        };
        assert_eq!(unstarted.queue_wait(), None);
        assert_eq!(unstarted.build_time(), None);
    }

    #[test]
    fn merge_dedupes_and_prefers_completed_and_newer() {
        let mut a = Dataset::new();
        a.tasks
            .insert("fedora:1".to_string(), task("fedora", 1, "s390x", None));
        a.tasks.insert(
            "fedora:2".to_string(),
            task("fedora", 2, "x86_64", Some(1500.0)),
        );

        let mut b = Dataset::new();
        // Completed now — must replace the running record.
        b.tasks.insert(
            "fedora:1".to_string(),
            task("fedora", 1, "s390x", Some(1800.0)),
        );
        // Older completion — must not replace.
        b.tasks.insert(
            "fedora:2".to_string(),
            task("fedora", 2, "x86_64", Some(1400.0)),
        );
        // New record.
        b.tasks.insert(
            "fedora:3".to_string(),
            task("fedora", 3, "aarch64", Some(1600.0)),
        );

        let stats = a.merge(b);
        assert_eq!(
            stats,
            MergeStats {
                added: 1,
                replaced: 1,
                unchanged: 1
            }
        );
        assert_eq!(a.tasks["fedora:1"].completion_ts, Some(1800.0));
        assert_eq!(a.tasks["fedora:2"].completion_ts, Some(1500.0));
    }

    #[test]
    fn merge_is_idempotent() {
        let mut a = Dataset::new();
        a.tasks.insert(
            "fedora:1".to_string(),
            task("fedora", 1, "s390x", Some(1800.0)),
        );
        let snapshot = a.tasks.clone();
        let mut b = Dataset::new();
        b.tasks = snapshot;
        let stats = a.merge(b);
        assert_eq!(stats.added, 0);
        assert_eq!(stats.replaced, 0);
        assert_eq!(stats.unchanged, 1);
    }

    #[test]
    fn windows_coalesce_and_gaps_are_reported() {
        let mut ds = Dataset::new();
        ds.meta.windows = vec![window("fedora", 0.0, 100.0, false)];
        let mut other = Dataset::new();
        other.meta.windows = vec![
            // Overlaps the first — coalesces.
            window("fedora", 50.0, 200.0, false),
            // Disjoint — leaves a gap [200, 300].
            window("fedora", 300.0, 400.0, false),
            // Different instance — independent.
            window("cbs", 0.0, 50.0, false),
        ];
        ds.merge(other);
        assert_eq!(ds.meta.windows.len(), 3);
        let gaps = ds.coverage_gaps();
        assert_eq!(gaps, vec![("fedora".to_string(), 200.0, 300.0)]);
    }

    #[test]
    fn filtered_windows_do_not_coalesce_with_full_ones() {
        let mut ds = Dataset::new();
        ds.meta.windows = vec![
            window("fedora", 0.0, 100.0, false),
            window("fedora", 50.0, 150.0, true),
        ];
        coalesce_windows(&mut ds.meta.windows);
        assert_eq!(ds.meta.windows.len(), 2);
        assert!(ds.mixes_filtered_windows());
    }
}
