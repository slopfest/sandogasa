// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure statistics over task records — no I/O.
//!
//! Population rules (the report's fine print):
//! - **build time**: CLOSED buildArch tasks with both start and
//!   completion timestamps. FAILED tasks are excluded by default
//!   (failures often stop early or hang, biasing "how slow is
//!   this arch" both ways) and opted in with `--include-failed`.
//!   CANCELED and non-terminal tasks are never counted.
//! - **queue wait**: started tasks, CLOSED or FAILED — the wait
//!   is real even when the build later failed. Tasks canceled
//!   before starting have no start timestamp and drop out.
//! - **critical path**: builds whose ≥2 buildArch children are
//!   all CLOSED. The bottleneck arch is the last to complete;
//!   its marginal delay is how long after the runner-up it
//!   finished
//!   (an exact tie attributes zero delay). Any FAILED/CANCELED
//!   child disqualifies the build — its timing isn't lag.

use schemars::JsonSchema;
use serde::Serialize;

use crate::dataset::TaskRecord;

use sandogasa_kojihub::hub::{TASK_CLOSED, TASK_FAILED};

/// Nearest-rank percentile over an ascending-sorted slice:
/// `sorted[ceil(p * n) - 1]`.
pub fn percentile(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

pub fn median(sorted: &[f64]) -> Option<f64> {
    percentile(sorted, 0.5)
}

/// Distribution summary of one population.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct DistSummary {
    pub count: usize,
    pub median: f64,
    pub p90: f64,
    pub max: f64,
}

/// Sort and summarize; `None` when empty.
pub fn summarize(values: &mut [f64]) -> Option<DistSummary> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    Some(DistSummary {
        count: values.len(),
        median: median(values)?,
        p90: percentile(values, 0.9)?,
        max: *values.last()?,
    })
}

/// Whether a task belongs in the build-time population.
pub fn in_build_time_population(task: &TaskRecord, include_failed: bool) -> bool {
    let terminal_ok = task.state == TASK_CLOSED || (include_failed && task.state == TASK_FAILED);
    terminal_ok && task.start_ts.is_some() && task.completion_ts.is_some()
}

/// Whether a task belongs in the queue-wait population.
pub fn in_queue_wait_population(task: &TaskRecord) -> bool {
    (task.state == TASK_CLOSED || task.state == TASK_FAILED) && task.start_ts.is_some()
}

/// The critical-path verdict for one build.
#[derive(Debug, Clone, PartialEq)]
pub struct CriticalPath {
    /// Arch whose task completed last — the build's bottleneck.
    pub bottleneck_arch: String,
    /// Seconds it finished after the second-slowest arch.
    pub marginal_delay: f64,
}

/// Which arch a build was bottlenecked on, from its buildArch
/// children.
/// `None` when fewer than two children, any child not CLOSED, or
/// timestamps are missing.
pub fn critical_path(children: &[&TaskRecord]) -> Option<CriticalPath> {
    if children.len() < 2 {
        return None;
    }
    let mut completions: Vec<(f64, &str)> = Vec::with_capacity(children.len());
    for child in children {
        if child.state != TASK_CLOSED {
            return None;
        }
        completions.push((child.completion_ts?, &child.arch));
    }
    completions.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (last_ts, bottleneck_arch) = completions[completions.len() - 1];
    let (runner_up_ts, _) = completions[completions.len() - 2];
    Some(CriticalPath {
        bottleneck_arch: bottleneck_arch.to_string(),
        marginal_delay: last_ts - runner_up_ts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(arch: &str, state: i64, start: Option<f64>, completion: Option<f64>) -> TaskRecord {
        TaskRecord {
            instance: "fedora".to_string(),
            task_id: 1,
            parent: Some(1),
            arch: arch.to_string(),
            package: None,
            state,
            create_ts: 0.0,
            start_ts: start,
            completion_ts: completion,
            host_id: None,
            channel_id: None,
            weight: None,
        }
    }

    #[test]
    fn percentile_edge_cases() {
        assert_eq!(percentile(&[], 0.5), None);
        assert_eq!(percentile(&[7.0], 0.5), Some(7.0));
        assert_eq!(percentile(&[7.0], 0.9), Some(7.0));
        // Even count: nearest-rank median is the lower-middle.
        assert_eq!(median(&[1.0, 2.0, 3.0, 4.0]), Some(2.0));
        assert_eq!(median(&[1.0, 2.0, 3.0]), Some(2.0));
        // p90 of 10 elements is the 9th (rank ceil(0.9*10)=9).
        let ten: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(percentile(&ten, 0.9), Some(9.0));
        assert_eq!(percentile(&ten, 1.0), Some(10.0));
    }

    #[test]
    fn summarize_sorts_and_reports() {
        let mut values = vec![30.0, 10.0, 20.0];
        let s = summarize(&mut values).unwrap();
        assert_eq!(s.count, 3);
        assert_eq!(s.median, 20.0);
        assert_eq!(s.max, 30.0);
        assert_eq!(summarize(&mut Vec::new()), None);
    }

    #[test]
    fn population_rules() {
        let closed = task("s390x", TASK_CLOSED, Some(10.0), Some(20.0));
        let failed = task("s390x", TASK_FAILED, Some(10.0), Some(20.0));
        let canceled = task("s390x", 3, Some(10.0), Some(20.0));
        let running = task("s390x", 1, Some(10.0), None);
        let never_started = task("s390x", 3, None, None);

        assert!(in_build_time_population(&closed, false));
        assert!(!in_build_time_population(&failed, false));
        assert!(in_build_time_population(&failed, true));
        assert!(!in_build_time_population(&canceled, true));
        assert!(!in_build_time_population(&running, false));

        assert!(in_queue_wait_population(&closed));
        assert!(in_queue_wait_population(&failed));
        assert!(!in_queue_wait_population(&canceled));
        assert!(!in_queue_wait_population(&never_started));
    }

    #[test]
    fn critical_path_attributes_last_arch() {
        let x86 = task("x86_64", TASK_CLOSED, Some(0.0), Some(100.0));
        let arm = task("aarch64", TASK_CLOSED, Some(0.0), Some(90.0));
        let s390x = task("s390x", TASK_CLOSED, Some(0.0), Some(400.0));
        let cp = critical_path(&[&x86, &arm, &s390x]).unwrap();
        assert_eq!(cp.bottleneck_arch, "s390x");
        assert_eq!(cp.marginal_delay, 300.0);
    }

    #[test]
    fn critical_path_tie_attributes_zero() {
        let a = task("x86_64", TASK_CLOSED, Some(0.0), Some(100.0));
        let b = task("s390x", TASK_CLOSED, Some(0.0), Some(100.0));
        let cp = critical_path(&[&a, &b]).unwrap();
        assert_eq!(cp.marginal_delay, 0.0);
    }

    #[test]
    fn critical_path_requires_all_closed_and_two_children() {
        let ok = task("x86_64", TASK_CLOSED, Some(0.0), Some(100.0));
        let failed = task("s390x", TASK_FAILED, Some(0.0), Some(50.0));
        assert_eq!(critical_path(&[&ok, &failed]), None);
        assert_eq!(critical_path(&[&ok]), None);
        let missing_ts = task("s390x", TASK_CLOSED, Some(0.0), None);
        assert_eq!(critical_path(&[&ok, &missing_ts]), None);
    }
}
