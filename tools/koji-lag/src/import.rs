// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reading JSON datasets — the format sweeps wrote before the store
//! existed — into the store.
//!
//! Worth having rather than re-sweeping: the datasets already collected
//! run to hundreds of megabytes and cost hours of hub time, and the rows
//! are the same rows. It also carries the store between machines, since
//! one JSON export imports anywhere.
//!
//! The delicate part is coverage. A dataset records the window it was
//! swept for, and that window is on **completion** time, while the store
//! records what has been **listed**, on creation time. The two are not
//! interchangeable, so this claims the narrower thing: see
//! [`listed_from_window`].

use std::path::Path;

use crate::dataset::{Dataset, is_srpm_step};
use crate::store::{CHILDREN_GEN, Span, Store, Written};

/// What an import put in, and what it refused to claim.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub written: Written,
    /// Builds whose children are current.
    pub children_current: usize,
    /// Builds whose children came from an older generation — a dataset
    /// swept before some method was collected — so a sweep will ask for
    /// them again.
    pub children_behind: usize,
}

/// The creation span a completion window entitles the store to claim.
///
/// A sweep for `[after, before)` on completion listed creations from
/// `after - grace` up to `before`, and nothing about the dataset proves
/// more than that. The claim made here is the *inner* span,
/// `[after, before)`, which is smaller than what was really listed: a
/// build created in the margin before `after` and completing inside the
/// window is in the dataset, but so might others created then have been,
/// completing *after* the window and so absent. Claiming the margin would
/// tell a later sweep those creations were enumerated when they were not.
///
/// The cost of the conservative claim is that a sweep of the window
/// before an imported one re-lists the margin. The cost of the generous
/// one is missing builds, which is the mistake this whole design is built
/// to avoid.
pub fn listed_from_window(after: f64, before: f64) -> Option<Span> {
    (before > after).then_some(Span {
        from: after,
        to: before,
    })
}

/// Fold `dataset` into `store`.
pub fn ingest(store: &mut Store, dataset: Dataset) -> Result<Imported, String> {
    let mut report = Imported::default();

    // Which children generation this dataset represents. One swept before
    // the SRPM stage was collected holds only `buildArch` children, so
    // recording it as current would tell the store it knows what the
    // source rebuild cost when it does not; recorded as generation 1, the
    // rows stay usable and a sweep asks for the rest.
    //
    // Read from the rows rather than from a version number, because a
    // dataset merged from several sweeps can hold a mixture and the rows
    // are the evidence.
    let children_generation = match dataset.tasks.values().any(|t| is_srpm_step(&t.method)) {
        true => CHILDREN_GEN,
        // Zero, not "current minus one": the store's default already
        // means never asked for, and a sweep will then fetch children it
        // genuinely lacks rather than trusting a claim nothing supports.
        false => 0,
    };

    let mut by_instance: std::collections::BTreeMap<String, Vec<_>> = Default::default();
    for build in dataset.builds.into_values() {
        by_instance
            .entry(build.instance.clone())
            .or_default()
            .push(build);
    }
    let mut tasks_by_instance: std::collections::BTreeMap<String, Vec<_>> = Default::default();
    for task in dataset.tasks.into_values() {
        tasks_by_instance
            .entry(task.instance.clone())
            .or_default()
            .push(task);
    }

    for (instance, builds) in &by_instance {
        report.written.builds += store.put_builds(instance, builds)?;
        let ids: Vec<i64> = builds.iter().map(|b| b.task_id).collect();
        store.mark_children_swept(instance, &ids, children_generation)?;
        if children_generation >= CHILDREN_GEN {
            report.children_current += ids.len();
        } else {
            report.children_behind += ids.len();
        }
    }
    for (instance, tasks) in &tasks_by_instance {
        report.written.tasks += store.put_tasks(instance, tasks)?;
    }

    // Hosts and channels are keyed "<instance>:<id>" in the JSON.
    let split = |key: &str| -> Option<(String, i64)> {
        let (instance, id) = key.rsplit_once(':')?;
        Some((instance.to_string(), id.parse().ok()?))
    };
    let mut hosts: std::collections::BTreeMap<String, Vec<(i64, String, String)>> =
        Default::default();
    for (key, name) in &dataset.hosts {
        if let Some((instance, id)) = split(key) {
            let arches = dataset.host_arches.get(key).cloned().unwrap_or_default();
            hosts
                .entry(instance)
                .or_default()
                .push((id, name.clone(), arches));
        }
    }
    for (instance, rows) in &hosts {
        store.put_hosts(instance, rows)?;
    }
    let mut channels: std::collections::BTreeMap<String, Vec<(i64, String)>> = Default::default();
    for (key, name) in &dataset.channels {
        if let Some((instance, id)) = split(key) {
            channels
                .entry(instance)
                .or_default()
                .push((id, name.clone()));
        }
    }
    for (instance, rows) in &channels {
        store.put_channels(instance, rows)?;
    }

    for window in &dataset.meta.windows {
        // A scoped sweep is not full coverage of anything, so it may
        // contribute rows but never a claim that a span was enumerated.
        if window.filtered {
            continue;
        }
        if let Some(span) = listed_from_window(window.from, window.to) {
            store.add_listed(&window.instance, span)?;
        }
    }
    Ok(report)
}

/// Fold every JSON dataset under `path` (a file, or a tree of them).
pub fn ingest_path(store: &mut Store, path: &Path) -> Result<Imported, String> {
    let mut total = Imported::default();
    for file in datasets_under(path)? {
        let dataset = Dataset::load(&file)?;
        let one = ingest(store, dataset)?;
        eprintln!(
            "  {}: {} build(s), {} task(s)",
            file.display(),
            one.written.builds,
            one.written.tasks
        );
        total.written.builds += one.written.builds;
        total.written.tasks += one.written.tasks;
        total.children_current += one.children_current;
        total.children_behind += one.children_behind;
    }
    Ok(total)
}

fn datasets_under(path: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut found = Vec::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?.path();
            if entry.is_dir() {
                stack.push(entry);
            } else if entry.extension().is_some_and(|e| e == "json") {
                found.push(entry);
            }
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::{BuildRecord, FetchWindow, TaskRecord};

    fn build(id: i64, completion: f64) -> BuildRecord {
        BuildRecord {
            instance: "fedora".into(),
            task_id: id,
            package: Some("foo".into()),
            nvr: None,
            target: None,
            owner: None,
            scratch: false,
            state: 2,
            create_ts: completion - 100.0,
            start_ts: Some(completion - 90.0),
            completion_ts: Some(completion),
            priority: None,
            host_id: None,
        }
    }

    fn task(id: i64, parent: i64, method: &str) -> TaskRecord {
        TaskRecord {
            instance: "fedora".into(),
            task_id: id,
            parent: Some(parent),
            method: method.into(),
            arch: "x86_64".into(),
            package: None,
            state: 2,
            create_ts: 0.0,
            start_ts: Some(1.0),
            completion_ts: Some(2.0),
            host_id: None,
            channel_id: None,
            weight: None,
        }
    }

    fn dataset(tasks: Vec<TaskRecord>, from: f64, to: f64, filtered: bool) -> Dataset {
        let mut ds = Dataset::new();
        let b = build(1, from + 10.0);
        ds.builds.insert(b.key(), b);
        for t in tasks {
            ds.tasks.insert(t.key(), t);
        }
        ds.meta.windows.push(FetchWindow {
            instance: "fedora".into(),
            from,
            to,
            fetched: chrono::Utc::now(),
            filtered,
        });
        ds
    }

    #[test]
    fn an_import_claims_only_the_window_not_its_margin() {
        let mut store = Store::in_memory().unwrap();
        ingest(
            &mut store,
            dataset(vec![task(11, 1, "buildArch")], 1000.0, 2000.0, false),
        )
        .unwrap();
        // Exactly the window: the three days of creations before it were
        // read but not exhaustively, since builds created then and
        // finishing later are absent.
        assert_eq!(
            store.listed("fedora").unwrap(),
            vec![Span {
                from: 1000.0,
                to: 2000.0
            }]
        );
        assert!(
            store
                .gaps(
                    "fedora",
                    Span {
                        from: 0.0,
                        to: 2000.0
                    }
                )
                .unwrap()
                .contains(&Span {
                    from: 0.0,
                    to: 1000.0
                }),
            "the margin must still be swept"
        );
    }

    #[test]
    fn a_scoped_sweep_contributes_rows_but_claims_no_coverage() {
        let mut store = Store::in_memory().unwrap();
        let report = ingest(
            &mut store,
            dataset(vec![task(11, 1, "buildArch")], 1000.0, 2000.0, true),
        )
        .unwrap();
        assert_eq!(report.written.builds, 1);
        // Nothing may be skipped on the strength of a filtered sweep: it
        // held only some of what it saw.
        assert!(store.listed("fedora").unwrap().is_empty());
    }

    #[test]
    fn children_count_as_fetched_only_when_the_srpm_stage_is_there() {
        // A dataset from before the SRPM stage was collected: its builds
        // stay pending, so a later sweep fills in what it never had.
        let mut store = Store::in_memory().unwrap();
        let report = ingest(
            &mut store,
            dataset(vec![task(11, 1, "buildArch")], 1000.0, 2000.0, false),
        )
        .unwrap();
        assert_eq!(report.children_behind, 1);
        assert_eq!(report.children_current, 0);
        assert_eq!(
            store
                .builds_needing_children("fedora", 0.0, 9999.0)
                .unwrap(),
            vec![1]
        );

        // One that has it is taken at its word.
        let mut store = Store::in_memory().unwrap();
        let report = ingest(
            &mut store,
            dataset(
                vec![task(11, 1, "buildArch"), task(12, 1, "rebuildSRPM")],
                1000.0,
                2000.0,
                false,
            ),
        )
        .unwrap();
        assert_eq!(report.children_current, 1);
        assert!(
            store
                .builds_needing_children("fedora", 0.0, 9999.0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn importing_the_same_dataset_twice_changes_nothing() {
        let mut store = Store::in_memory().unwrap();
        let ds = || {
            dataset(
                vec![task(11, 1, "buildArch"), task(12, 1, "rebuildSRPM")],
                1000.0,
                2000.0,
                false,
            )
        };
        ingest(&mut store, ds()).unwrap();
        ingest(&mut store, ds()).unwrap();
        let counts = store.counts().unwrap();
        assert_eq!(counts["fedora"].builds, 1);
        assert_eq!(counts["fedora"].tasks, 2);
        assert_eq!(store.listed("fedora").unwrap().len(), 1);
    }
}
