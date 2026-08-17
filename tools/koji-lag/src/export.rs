// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Writing a store's rows out as CSV, for analysis elsewhere.
//!
//! Requested by the Fedora Data WG, and CSV rather than JSON on purpose. A
//! store travels perfectly well as itself — one SQLite file, portable
//! across platforms — so re-encoding it as JSON only to import it again
//! bought nothing and carried a hazard: a dataset's coverage windows are a
//! promise a later import acts on, so an export of half-swept days could
//! tell a sweep to skip creations nobody ever listed.
//!
//! CSV makes no such promise. It is a dump for someone else's tools, so the
//! rows are the store's own rows rather than an interpretation of them:
//! whoever asked can pivot, join and aggregate without arguing with a shape
//! we chose. What the export *does* say is how much of the range it holds
//! whole, since a spreadsheet cannot warn about that itself.

use std::io::Write;
use std::path::Path;

use crate::store::Store;

/// What an export wrote.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Exported {
    pub builds: usize,
    pub tasks: usize,
    pub hosts: usize,
    pub channels: usize,
    /// Whole days of the range the store holds completely.
    pub days_whole: usize,
    /// Days in range left out for being incomplete, named so they can be
    /// synced.
    pub days_skipped: Vec<String>,
    pub files: Vec<std::path::PathBuf>,
}

/// Write `[from, to)` of `instance` into `dir` as CSV files.
///
/// Four of them, mirroring the store: `builds.csv`, `tasks.csv`,
/// `hosts.csv` and `channels.csv`. The last two are small and easy to
/// overlook, but without them a `host_id` is a number and the arch a
/// `noarch` build actually ran on cannot be recovered at all.
pub fn run(
    store: &Store,
    instance: &str,
    from: f64,
    to: f64,
    grace: f64,
    dir: &Path,
) -> Result<Exported, String> {
    // Whole days only. A partial day is not a quiet day — its builds are
    // there and their arch tasks are not — so exporting one hands someone
    // a file whose figures are wrong in a way nothing in the file reveals.
    // Anyone analysing this cannot be expected to know which days to drop,
    // so they are dropped here.
    let selection = store.analysable(instance, from, to, grace)?;
    if selection.whole.is_empty() {
        return Err(refuse(&selection));
    }
    let dataset = &selection.dataset;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let mut exported = Exported::default();
    let mut write = |name: &str, rows: Vec<Vec<String>>| -> Result<usize, String> {
        let path = dir.join(name);
        let mut out = std::io::BufWriter::new(
            std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?,
        );
        for row in &rows {
            writeln!(out, "{}", crate::csv::row(row))
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        out.flush()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        exported.files.push(path);
        // Less the header, which is a row to the writer and not to a
        // reader counting what they got.
        Ok(rows.len().saturating_sub(1))
    };

    let num = |v: Option<f64>| v.map(|n| format!("{n}")).unwrap_or_default();
    let int = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_default();
    let text = |v: &Option<String>| v.clone().unwrap_or_default();

    let mut builds = vec![
        [
            "instance",
            "task_id",
            "package",
            "nvr",
            "target",
            "owner",
            "scratch",
            "state",
            "create_ts",
            "start_ts",
            "completion_ts",
            "priority",
            "host_id",
        ]
        .map(String::from)
        .to_vec(),
    ];
    for b in dataset.builds.values() {
        builds.push(vec![
            b.instance.clone(),
            b.task_id.to_string(),
            text(&b.package),
            text(&b.nvr),
            text(&b.target),
            text(&b.owner),
            u8::from(b.scratch).to_string(),
            b.state.to_string(),
            format!("{}", b.create_ts),
            num(b.start_ts),
            num(b.completion_ts),
            int(b.priority),
            int(b.host_id),
        ]);
    }
    exported.builds = write("builds.csv", builds)?;

    let mut tasks = vec![
        [
            "instance",
            "task_id",
            "parent",
            "method",
            "arch",
            "package",
            "state",
            "create_ts",
            "start_ts",
            "completion_ts",
            "host_id",
            "channel_id",
            "weight",
        ]
        .map(String::from)
        .to_vec(),
    ];
    for t in dataset.tasks.values() {
        tasks.push(vec![
            t.instance.clone(),
            t.task_id.to_string(),
            int(t.parent),
            t.method.clone(),
            t.arch.clone(),
            text(&t.package),
            t.state.to_string(),
            format!("{}", t.create_ts),
            num(t.start_ts),
            num(t.completion_ts),
            int(t.host_id),
            int(t.channel_id),
            num(t.weight),
        ]);
    }
    exported.tasks = write("tasks.csv", tasks)?;

    // The id→name maps are keyed "<instance>:<id>" in memory; split them
    // back out so a spreadsheet can join on the id.
    let split = |key: &str| -> Option<(String, String)> {
        let (instance, id) = key.rsplit_once(':')?;
        Some((instance.to_string(), id.to_string()))
    };
    let mut hosts = vec![
        ["instance", "host_id", "name", "arches"]
            .map(String::from)
            .to_vec(),
    ];
    for (key, name) in &dataset.hosts {
        if let Some((instance, id)) = split(key) {
            hosts.push(vec![
                instance,
                id,
                name.clone(),
                dataset.host_arches.get(key).cloned().unwrap_or_default(),
            ]);
        }
    }
    exported.hosts = write("hosts.csv", hosts)?;

    let mut channels = vec![
        ["instance", "channel_id", "name"]
            .map(String::from)
            .to_vec(),
    ];
    for (key, name) in &dataset.channels {
        if let Some((instance, id)) = split(key) {
            channels.push(vec![instance, id, name.clone()]);
        }
    }
    exported.channels = write("channels.csv", channels)?;

    exported.days_whole = selection.days();
    // Named, not just counted: someone who asked for a month and got
    // twenty-eight days needs to know which two to sync.
    exported.days_skipped = selection.skipped_dates();
    Ok(exported)
}

/// Why there is nothing to export, and what to do about it.
///
/// An error rather than an empty file: a CSV with only headers reads as
/// "no builds that week", which is a different claim entirely.
pub fn refuse(selection: &crate::store::Selection) -> String {
    format!(
        "nothing complete to analyse in that range: {} day(s) have rows but \
         are incomplete{}. Sync them first, or narrow the range.",
        selection.skipped.len(),
        match selection.skipped_dates().split_first() {
            Some((first, [])) => format!(" ({first})"),
            Some((first, rest)) => format!(" ({first} … {})", rest.last().unwrap()),
            None => String::new(),
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::{BuildRecord, TaskRecord};
    use crate::store::{CHILDREN_GEN, Span};
    use chrono::NaiveDate;

    const GRACE: f64 = 3.0 * 86_400.0;

    fn midnight(y: i32, m: u32, d: u32) -> f64 {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp() as f64
    }

    fn build(id: i64, completion: f64, package: &str) -> BuildRecord {
        BuildRecord {
            instance: "fedora".into(),
            task_id: id,
            package: Some(package.to_string()),
            nvr: None,
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
            package: None,
            state: 2,
            create_ts: completion - 500.0,
            start_ts: Some(completion - 400.0),
            completion_ts: Some(completion),
            host_id: Some(1),
            channel_id: Some(1),
            weight: None,
        }
    }

    /// One whole day (2026-08-12) and one listed but never finished
    /// (08-13), which is the state a sync leaves behind when interrupted.
    fn part_finished_store() -> Store {
        let mut store = Store::in_memory().unwrap();
        let whole = midnight(2026, 8, 12) + 43_200.0;
        let partial = midnight(2026, 8, 13) + 43_200.0;
        store
            .put_builds(
                "fedora",
                &[build(1, whole, "foo"), build(2, partial, "bar, baz")],
            )
            .unwrap();
        store.put_tasks("fedora", &[child(11, 1, whole)]).unwrap();
        store
            .mark_children_swept("fedora", &[1], CHILDREN_GEN)
            .unwrap();
        store
            .put_hosts("fedora", &[(1, "buildvm-01".into(), "x86_64 i686".into())])
            .unwrap();
        store
            .put_channels("fedora", &[(1, "default".into())])
            .unwrap();
        store
            .add_listed(
                "fedora",
                Span {
                    from: midnight(2026, 8, 12) - GRACE,
                    to: midnight(2026, 8, 14),
                },
            )
            .unwrap();
        store
    }

    #[test]
    fn an_export_writes_every_table_it_takes_to_interpret_the_rows() {
        let store = part_finished_store();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("csv");
        let exported = run(
            &store,
            "fedora",
            midnight(2026, 8, 12),
            midnight(2026, 8, 14),
            GRACE,
            &out,
        )
        .unwrap();

        // One of the two days is whole, and only that one is exported.
        assert_eq!(exported.builds, 1);
        assert_eq!(exported.tasks, 1);
        // Not an afterthought: without hosts, a host_id is a number and
        // the arch a noarch build ran on is unrecoverable.
        assert_eq!(exported.hosts, 1);
        assert_eq!(exported.channels, 1);
        for name in ["builds.csv", "tasks.csv", "hosts.csv", "channels.csv"] {
            assert!(out.join(name).exists(), "{name} missing");
        }
        let hosts = std::fs::read_to_string(out.join("hosts.csv")).unwrap();
        assert!(hosts.contains("fedora,1,buildvm-01,x86_64 i686"), "{hosts}");
    }

    #[test]
    fn an_incomplete_day_is_left_out_and_named() {
        // The 13th was listed but never had its children fetched, so its
        // builds are present with no arch tasks. Exporting them would give
        // an analyst a day that looks quiet rather than one that is
        // half-fetched, and nothing in a CSV could tell them apart.
        let store = part_finished_store();
        let dir = tempfile::tempdir().unwrap();
        let exported = run(
            &store,
            "fedora",
            midnight(2026, 8, 12),
            midnight(2026, 8, 14),
            GRACE,
            dir.path(),
        )
        .unwrap();
        assert_eq!(exported.days_whole, 1);
        assert_eq!(exported.days_skipped, vec!["2026-08-13".to_string()]);
        // Only the whole day's build, not the half-fetched day's.
        assert_eq!(exported.builds, 1);
        let builds = std::fs::read_to_string(dir.path().join("builds.csv")).unwrap();
        assert!(builds.contains("foo"), "{builds}");
        assert!(!builds.contains("bar"), "the partial day leaked: {builds}");
    }

    #[test]
    fn a_range_with_nothing_complete_is_refused() {
        // Better than writing an empty file, which reads as "no builds
        // that week" rather than "nothing fetched yet".
        let store = part_finished_store();
        let dir = tempfile::tempdir().unwrap();
        let err = run(
            &store,
            "fedora",
            midnight(2026, 8, 13),
            midnight(2026, 8, 14),
            GRACE,
            dir.path(),
        )
        .unwrap_err();
        assert!(err.contains("nothing complete to analyse"), "{err}");
        assert!(
            err.contains("2026-08-13"),
            "must name the day to sync: {err}"
        );
        assert!(!dir.path().join("builds.csv").exists(), "wrote anyway");
    }

    #[test]
    fn a_field_that_would_break_a_row_is_quoted_in_the_file() {
        // A whole day whose package name holds a comma, so the row that
        // gets written has to quote it.
        let mut store = part_finished_store();
        let noon = midnight(2026, 8, 12) + 43_200.0;
        store
            .put_builds("fedora", &[build(3, noon, "bar, baz")])
            .unwrap();
        store.put_tasks("fedora", &[child(31, 3, noon)]).unwrap();
        store
            .mark_children_swept("fedora", &[3], CHILDREN_GEN)
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        run(
            &store,
            "fedora",
            midnight(2026, 8, 12),
            midnight(2026, 8, 14),
            GRACE,
            dir.path(),
        )
        .unwrap();
        let builds = std::fs::read_to_string(dir.path().join("builds.csv")).unwrap();
        assert!(builds.contains("\"bar, baz\""), "{builds}");
        // And the header is there for a spreadsheet to name its columns.
        assert!(builds.starts_with("instance,task_id,package,"), "{builds}");
    }
}
