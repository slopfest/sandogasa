// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The SQLite store: every build and task the hub has told us about.
//!
//! Two ideas hold this together, both argued in DEVELOPMENT.md.
//!
//! *Keep everything.* A task is stored whether or not the window being
//! swept wants it, because a round trip to the hub costs minutes and the
//! row costs bytes. Nothing here filters rows out by window.
//!
//! *Record what was listed, not what was kept.* [`Store::listed`] holds
//! creation-time spans over which every build task has been enumerated,
//! and it is the only thing a sweep may skip work against. A record of
//! what was stored cannot serve: builds are stored by completion, so the
//! oldest one in a window sits inside the previous window's margin, and
//! bounding a sweep by it loses builds created near the boundary.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::dataset::{BuildRecord, Dataset, TaskRecord};

/// Bumped when the schema changes in a way an older binary cannot read.
pub const SCHEMA_VERSION: u32 = 1;

/// Bumped when a *new field* is taken from the build listing.
///
/// Rows recorded under an older generation are missing it, so their
/// creation spans need listing again — but only listing. Re-reading a
/// year of build rows costs about an hour; the per-parent child queries
/// behind them cost days. Keeping the two generations apart is what makes
/// adding a field a re-list rather than a rescan.
pub const LISTING_GEN: i64 = 1;

/// Bumped when a new field is taken from the child-task queries, or when
/// a new child *method* is collected.
///
/// This is the expensive one: every affected build's children must be
/// asked for again, which for a year is days rather than the hour a
/// re-list costs.
///
/// Generation 1 is the first the store ever held, and it includes the
/// source rebuild (`rebuildSRPM`, `buildSRPMFromSCM`) — every dataset
/// that exists was swept by a version collecting those. Anything that
/// turns out not to, imported from elsewhere or from before, is recorded
/// as generation 0, which the schema already means as "children never
/// asked for".
pub const CHILDREN_GEN: i64 = 1;

/// A half-open span of creation time, `[from, to)`, that has been listed
/// in full.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub from: f64,
    pub to: f64,
}

impl Span {
    fn touches(&self, other: &Span) -> bool {
        self.from <= other.to && other.from <= self.to
    }
}

/// What a sweep put in, for the summary line.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Written {
    pub builds: usize,
    pub tasks: usize,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if absent) the store at `path`.
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// An in-memory store, for tests.
    pub fn in_memory() -> Result<Self, String> {
        let conn = Connection::open_in_memory().map_err(|e| e.to_string())?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<(), String> {
        // WAL so a long sweep's writes commit without blocking a reader,
        // and a crash mid-sweep leaves a consistent file rather than a
        // truncated one.
        self.conn
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| e.to_string())?;
        self.conn
            .execute_batch(
                "BEGIN;
                 CREATE TABLE IF NOT EXISTS meta (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS builds (
                     instance TEXT NOT NULL,
                     task_id INTEGER NOT NULL,
                     package TEXT,
                     nvr TEXT,
                     target TEXT,
                     owner TEXT,
                     scratch INTEGER NOT NULL DEFAULT 0,
                     state INTEGER NOT NULL,
                     create_ts REAL NOT NULL,
                     start_ts REAL,
                     completion_ts REAL,
                     priority INTEGER,
                     host_id INTEGER,
                     -- The children generation this build was fetched
                     -- under; 0 means its children were never asked for.
                     -- Compared against CHILDREN_GEN so a store that
                     -- predates a newly collected method knows which
                     -- builds are behind, without re-listing anything.
                     children_gen INTEGER NOT NULL DEFAULT 0,
                     PRIMARY KEY (instance, task_id)
                 );
                 CREATE TABLE IF NOT EXISTS tasks (
                     instance TEXT NOT NULL,
                     task_id INTEGER NOT NULL,
                     parent INTEGER,
                     method TEXT NOT NULL,
                     arch TEXT NOT NULL,
                     package TEXT,
                     state INTEGER NOT NULL,
                     create_ts REAL NOT NULL,
                     start_ts REAL,
                     completion_ts REAL,
                     host_id INTEGER,
                     channel_id INTEGER,
                     weight REAL,
                     PRIMARY KEY (instance, task_id)
                 );
                 CREATE TABLE IF NOT EXISTS hosts (
                     instance TEXT NOT NULL,
                     host_id INTEGER NOT NULL,
                     name TEXT NOT NULL,
                     arches TEXT,
                     PRIMARY KEY (instance, host_id)
                 );
                 CREATE TABLE IF NOT EXISTS channels (
                     instance TEXT NOT NULL,
                     channel_id INTEGER NOT NULL,
                     name TEXT NOT NULL,
                     PRIMARY KEY (instance, channel_id)
                 );
                 -- Creation spans enumerated in full. The record a sweep
                 -- skips work against; see the module docs.
                 CREATE TABLE IF NOT EXISTS listed (
                     instance TEXT NOT NULL,
                     from_ts REAL NOT NULL,
                     to_ts REAL NOT NULL,
                     -- What the rows in this span were listed with. A
                     -- span below LISTING_GEN is treated as a gap, so a
                     -- new listing field refreshes itself.
                     listing_gen INTEGER NOT NULL DEFAULT 1
                 );
                 -- Reports select by completion; children lookups and
                 -- the sweep's own gap arithmetic select by parent and by
                 -- creation.
                 CREATE INDEX IF NOT EXISTS builds_completion
                     ON builds (instance, completion_ts);
                 CREATE INDEX IF NOT EXISTS builds_unswept
                     ON builds (instance, children_gen, completion_ts);
                 CREATE INDEX IF NOT EXISTS tasks_completion
                     ON tasks (instance, completion_ts);
                 CREATE INDEX IF NOT EXISTS tasks_parent
                     ON tasks (instance, parent);
                 CREATE INDEX IF NOT EXISTS listed_span
                     ON listed (instance, from_ts, to_ts);
                 COMMIT;",
            )
            .map_err(|e| format!("creating the schema: {e}"))?;

        let found: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match found.as_deref().map(str::parse::<u32>) {
            None => {
                self.conn
                    .execute(
                        "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                        params![SCHEMA_VERSION.to_string()],
                    )
                    .map_err(|e| e.to_string())?;
            }
            Some(Ok(v)) if v == SCHEMA_VERSION => {}
            // Refusing is the honest answer: a newer binary's rows may
            // mean something this one would misread, and a store is
            // rebuildable from the hub or an export.
            Some(Ok(v)) => {
                return Err(format!(
                    "store was written by schema version {v}, this build speaks {SCHEMA_VERSION}"
                ));
            }
            Some(Err(e)) => return Err(format!("unreadable schema version: {e}")),
        }
        Ok(())
    }

    /// Record builds, leaving `children_gen` alone for rows already there
    /// — a re-listed build must not lose credit for children already
    /// fetched, which is what makes re-listing cheap enough to do for a
    /// new field.
    pub fn put_builds(&mut self, instance: &str, builds: &[BuildRecord]) -> Result<usize, String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO builds (instance, task_id, package, nvr, target, owner,
                                         scratch, state, create_ts, start_ts, completion_ts,
                                         priority, host_id)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                     ON CONFLICT (instance, task_id) DO UPDATE SET
                         package=excluded.package, nvr=excluded.nvr, target=excluded.target,
                         owner=excluded.owner, scratch=excluded.scratch, state=excluded.state,
                         create_ts=excluded.create_ts, start_ts=excluded.start_ts,
                         completion_ts=excluded.completion_ts, priority=excluded.priority,
                         host_id=excluded.host_id",
                )
                .map_err(|e| e.to_string())?;
            for b in builds {
                stmt.execute(params![
                    instance,
                    b.task_id,
                    b.package,
                    b.nvr,
                    b.target,
                    b.owner,
                    b.scratch as i64,
                    b.state,
                    b.create_ts,
                    b.start_ts,
                    b.completion_ts,
                    b.priority,
                    b.host_id,
                ])
                .map_err(|e| format!("storing build {}: {e}", b.task_id))?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(builds.len())
    }

    /// Record child tasks.
    pub fn put_tasks(&mut self, instance: &str, tasks: &[TaskRecord]) -> Result<usize, String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO tasks (instance, task_id, parent, method, arch, package,
                                        state, create_ts, start_ts, completion_ts,
                                        host_id, channel_id, weight)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                     ON CONFLICT (instance, task_id) DO UPDATE SET
                         parent=excluded.parent, method=excluded.method, arch=excluded.arch,
                         package=excluded.package, state=excluded.state,
                         create_ts=excluded.create_ts, start_ts=excluded.start_ts,
                         completion_ts=excluded.completion_ts, host_id=excluded.host_id,
                         channel_id=excluded.channel_id, weight=excluded.weight",
                )
                .map_err(|e| e.to_string())?;
            for t in tasks {
                stmt.execute(params![
                    instance,
                    t.task_id,
                    t.parent,
                    t.method,
                    t.arch,
                    t.package,
                    t.state,
                    t.create_ts,
                    t.start_ts,
                    t.completion_ts,
                    t.host_id,
                    t.channel_id,
                    t.weight,
                ])
                .map_err(|e| format!("storing task {}: {e}", t.task_id))?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(tasks.len())
    }

    /// Note that these parents' children have been asked for, under
    /// `generation` — normally [`CHILDREN_GEN`], but an import may record
    /// an older one for rows it knows are behind.
    pub fn mark_children_swept(
        &mut self,
        instance: &str,
        parents: &[i64],
        generation: i64,
    ) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "UPDATE builds SET children_gen = max(children_gen, ?3)
                     WHERE instance = ?1 AND task_id = ?2",
                )
                .map_err(|e| e.to_string())?;
            for id in parents {
                stmt.execute(params![instance, id, generation])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn put_hosts(
        &mut self,
        instance: &str,
        hosts: &[(i64, String, String)],
    ) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO hosts (instance, host_id, name, arches) VALUES (?1,?2,?3,?4)
                     ON CONFLICT (instance, host_id) DO UPDATE SET
                         name=excluded.name, arches=excluded.arches",
                )
                .map_err(|e| e.to_string())?;
            for (id, name, arches) in hosts {
                let arches = (!arches.is_empty()).then_some(arches);
                stmt.execute(params![instance, id, name, arches])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn put_channels(
        &mut self,
        instance: &str,
        channels: &[(i64, String)],
    ) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO channels (instance, channel_id, name) VALUES (?1,?2,?3)
                     ON CONFLICT (instance, channel_id) DO UPDATE SET name=excluded.name",
                )
                .map_err(|e| e.to_string())?;
            for (id, name) in channels {
                stmt.execute(params![instance, id, name])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    /// Note that `span` has been listed in full, merging it into what is
    /// already recorded.
    ///
    /// Written per page as a sweep proceeds, so an interruption keeps
    /// credit for exactly the pages that landed.
    pub fn add_listed(&mut self, instance: &str, span: Span) -> Result<(), String> {
        let existing = self.listed(instance)?;
        let mut merged = span;
        let overlapping: Vec<Span> = existing
            .iter()
            .copied()
            .filter(|s| s.touches(&span))
            .collect();
        for s in &overlapping {
            merged.from = merged.from.min(s.from);
            merged.to = merged.to.max(s.to);
        }
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        for s in &overlapping {
            tx.execute(
                "DELETE FROM listed WHERE instance = ?1 AND from_ts = ?2 AND to_ts = ?3",
                params![instance, s.from, s.to],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute(
            "INSERT INTO listed (instance, from_ts, to_ts) VALUES (?1, ?2, ?3)",
            params![instance, merged.from, merged.to],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    }

    /// The creation spans listed in full, oldest first.
    pub fn listed(&self, instance: &str) -> Result<Vec<Span>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT from_ts, to_ts FROM listed WHERE instance = ?1 ORDER BY from_ts")
            .map_err(|e| e.to_string())?;
        let spans = stmt
            .query_map(params![instance], |r| {
                Ok(Span {
                    from: r.get(0)?,
                    to: r.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(spans)
    }

    /// The parts of `want` not yet listed, oldest first.
    ///
    /// What a sweep actually has to ask the hub for. An empty answer
    /// means the window is already covered and the hub need not be
    /// troubled at all.
    pub fn gaps(&self, instance: &str, want: Span) -> Result<Vec<Span>, String> {
        let mut gaps = Vec::new();
        let mut cursor = want.from;
        for span in self.listed(instance)? {
            if span.to <= cursor {
                continue;
            }
            if span.from >= want.to {
                break;
            }
            if span.from > cursor {
                gaps.push(Span {
                    from: cursor,
                    to: span.from.min(want.to),
                });
            }
            cursor = cursor.max(span.to);
            if cursor >= want.to {
                return Ok(gaps);
            }
        }
        if cursor < want.to {
            gaps.push(Span {
                from: cursor,
                to: want.to,
            });
        }
        Ok(gaps)
    }

    /// Builds completing in `[from, to)` whose children have not been
    /// asked for.
    pub fn builds_needing_children(
        &self,
        instance: &str,
        from: f64,
        to: f64,
    ) -> Result<Vec<i64>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT task_id FROM builds
                 WHERE instance = ?1 AND children_gen < ?4
                   AND completion_ts >= ?2 AND completion_ts < ?3
                 ORDER BY task_id DESC",
            )
            .map_err(|e| e.to_string())?;
        let ids = stmt
            .query_map(params![instance, from, to, CHILDREN_GEN], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<i64>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(ids)
    }

    /// Everything needed to report on `[from, to)`, as the in-memory
    /// shape the report code already speaks.
    ///
    /// Reports select by completion time, so this is a window query
    /// rather than a file to find: the period a report covers is a
    /// `WHERE` clause, which is why raw data no longer needs collating.
    pub fn dataset_for(&self, instance: &str, from: f64, to: f64) -> Result<Dataset, String> {
        let mut dataset = Dataset::new();
        let mut stmt = self
            .conn
            .prepare(
                "SELECT task_id, package, nvr, target, owner, scratch, state,
                        create_ts, start_ts, completion_ts, priority, host_id
                 FROM builds
                 WHERE instance = ?1 AND completion_ts >= ?2 AND completion_ts < ?3",
            )
            .map_err(|e| e.to_string())?;
        let builds = stmt
            .query_map(params![instance, from, to], |r| {
                Ok(BuildRecord {
                    instance: instance.to_string(),
                    task_id: r.get(0)?,
                    package: r.get(1)?,
                    nvr: r.get(2)?,
                    target: r.get(3)?,
                    owner: r.get(4)?,
                    scratch: r.get::<_, i64>(5)? != 0,
                    state: r.get(6)?,
                    create_ts: r.get(7)?,
                    start_ts: r.get(8)?,
                    completion_ts: r.get(9)?,
                    priority: r.get(10)?,
                    host_id: r.get(11)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        for b in builds {
            dataset.builds.insert(b.key(), b);
        }

        // Child tasks come by parent, not by their own completion: a
        // build's arches are its arches whenever they finished, and
        // selecting children by their own window would split a build
        // across periods.
        let mut stmt = self
            .conn
            .prepare(
                // A child's package falls back to its parent's: the
                // child's own request often carries no parseable srpm,
                // and the two halves may have been fetched by different
                // runs, so the join is the only place both are in hand.
                "SELECT t.task_id, t.parent, t.method, t.arch,
                        COALESCE(t.package, b.package), t.state,
                        t.create_ts, t.start_ts, t.completion_ts, t.host_id,
                        t.channel_id, t.weight
                 FROM tasks t JOIN builds b
                   ON b.instance = t.instance AND b.task_id = t.parent
                 WHERE t.instance = ?1 AND b.completion_ts >= ?2 AND b.completion_ts < ?3",
            )
            .map_err(|e| e.to_string())?;
        let tasks = stmt
            .query_map(params![instance, from, to], |r| {
                Ok(TaskRecord {
                    instance: instance.to_string(),
                    task_id: r.get(0)?,
                    parent: r.get(1)?,
                    method: r.get(2)?,
                    arch: r.get(3)?,
                    package: r.get(4)?,
                    state: r.get(5)?,
                    create_ts: r.get(6)?,
                    start_ts: r.get(7)?,
                    completion_ts: r.get(8)?,
                    host_id: r.get(9)?,
                    channel_id: r.get(10)?,
                    weight: r.get(11)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        for t in tasks {
            dataset.tasks.insert(t.key(), t);
        }

        let mut stmt = self
            .conn
            .prepare("SELECT host_id, name, arches FROM hosts WHERE instance = ?1")
            .map_err(|e| e.to_string())?;
        let hosts: Vec<(i64, String, Option<String>)> = stmt
            .query_map(params![instance], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        for (id, name, arches) in hosts {
            let key = format!("{instance}:{id}");
            dataset.hosts.insert(key.clone(), name);
            if let Some(arches) = arches {
                dataset.host_arches.insert(key, arches);
            }
        }

        let mut stmt = self
            .conn
            .prepare("SELECT channel_id, name FROM channels WHERE instance = ?1")
            .map_err(|e| e.to_string())?;
        let channels: Vec<(i64, String)> = stmt
            .query_map(params![instance], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        for (id, name) in channels {
            dataset.channels.insert(format!("{instance}:{id}"), name);
        }
        Ok(dataset)
    }

    /// Counts per instance, for a summary line.
    pub fn counts(&self) -> Result<BTreeMap<String, Written>, String> {
        let mut out: BTreeMap<String, Written> = BTreeMap::new();
        for (table, field) in [("builds", true), ("tasks", false)] {
            let mut stmt = self
                .conn
                .prepare(&format!(
                    "SELECT instance, COUNT(*) FROM {table} GROUP BY instance"
                ))
                .map_err(|e| e.to_string())?;
            let rows: Vec<(String, i64)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?;
            for (instance, n) in rows {
                let entry = out.entry(instance).or_default();
                let n = n.max(0) as usize;
                if field {
                    entry.builds = n;
                } else {
                    entry.tasks = n;
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(id: i64, completion: f64) -> BuildRecord {
        BuildRecord {
            instance: "fedora".into(),
            task_id: id,
            package: Some("foo".into()),
            nvr: None,
            target: None,
            owner: Some("alice".into()),
            scratch: false,
            state: 2,
            create_ts: completion - 100.0,
            start_ts: Some(completion - 90.0),
            completion_ts: Some(completion),
            priority: None,
            host_id: Some(1),
        }
    }

    fn task(id: i64, parent: i64) -> TaskRecord {
        TaskRecord {
            instance: "fedora".into(),
            task_id: id,
            parent: Some(parent),
            method: "buildArch".into(),
            arch: "x86_64".into(),
            package: Some("foo".into()),
            state: 2,
            create_ts: 0.0,
            start_ts: Some(10.0),
            completion_ts: Some(100.0),
            host_id: Some(1),
            channel_id: None,
            weight: None,
        }
    }

    #[test]
    fn listed_spans_merge_as_they_are_added() {
        let mut store = Store::in_memory().unwrap();
        store
            .add_listed(
                "fedora",
                Span {
                    from: 100.0,
                    to: 200.0,
                },
            )
            .unwrap();
        store
            .add_listed(
                "fedora",
                Span {
                    from: 300.0,
                    to: 400.0,
                },
            )
            .unwrap();
        assert_eq!(store.listed("fedora").unwrap().len(), 2);

        // A span bridging the two collapses all three into one, so the
        // record stays a small set of maximal spans however a sweep
        // arrived at them.
        store
            .add_listed(
                "fedora",
                Span {
                    from: 150.0,
                    to: 350.0,
                },
            )
            .unwrap();
        assert_eq!(
            store.listed("fedora").unwrap(),
            vec![Span {
                from: 100.0,
                to: 400.0
            }]
        );

        // Instances keep their own record.
        store
            .add_listed(
                "cbs",
                Span {
                    from: 0.0,
                    to: 50.0,
                },
            )
            .unwrap();
        assert_eq!(store.listed("fedora").unwrap().len(), 1);
        assert_eq!(store.listed("cbs").unwrap().len(), 1);
    }

    #[test]
    fn gaps_are_what_is_left_to_ask_for() {
        let mut store = Store::in_memory().unwrap();
        let want = Span {
            from: 0.0,
            to: 1000.0,
        };
        // Nothing listed: the whole window.
        assert_eq!(store.gaps("fedora", want).unwrap(), vec![want]);

        store
            .add_listed(
                "fedora",
                Span {
                    from: 200.0,
                    to: 400.0,
                },
            )
            .unwrap();
        store
            .add_listed(
                "fedora",
                Span {
                    from: 600.0,
                    to: 800.0,
                },
            )
            .unwrap();
        assert_eq!(
            store.gaps("fedora", want).unwrap(),
            vec![
                Span {
                    from: 0.0,
                    to: 200.0
                },
                Span {
                    from: 400.0,
                    to: 600.0
                },
                Span {
                    from: 800.0,
                    to: 1000.0
                },
            ]
        );

        // Covered end to end: the hub is not troubled at all, which is
        // what makes re-running a window free.
        store
            .add_listed(
                "fedora",
                Span {
                    from: 0.0,
                    to: 1000.0,
                },
            )
            .unwrap();
        assert!(store.gaps("fedora", want).unwrap().is_empty());

        // A window inside what is listed is likewise free.
        assert!(
            store
                .gaps(
                    "fedora",
                    Span {
                        from: 300.0,
                        to: 500.0
                    }
                )
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn children_are_asked_for_once() {
        let mut store = Store::in_memory().unwrap();
        store
            .put_builds("fedora", &[build(1, 500.0), build(2, 600.0)])
            .unwrap();
        assert_eq!(
            store
                .builds_needing_children("fedora", 0.0, 1000.0)
                .unwrap(),
            vec![2, 1]
        );

        store
            .mark_children_swept("fedora", &[1], CHILDREN_GEN)
            .unwrap();
        assert_eq!(
            store
                .builds_needing_children("fedora", 0.0, 1000.0)
                .unwrap(),
            vec![2]
        );

        // Re-listing a build must not lose that credit: a sweep crossing
        // the same creation span again would otherwise re-fetch every
        // child it already has.
        store.put_builds("fedora", &[build(1, 500.0)]).unwrap();
        assert_eq!(
            store
                .builds_needing_children("fedora", 0.0, 1000.0)
                .unwrap(),
            vec![2]
        );
    }

    #[test]
    fn a_window_query_takes_builds_by_completion_and_children_by_parent() {
        let mut store = Store::in_memory().unwrap();
        store
            .put_builds("fedora", &[build(1, 500.0), build(2, 5000.0)])
            .unwrap();
        // Child of the in-window build, but completing outside it.
        let mut late = task(11, 1);
        late.completion_ts = Some(9999.0);
        store.put_tasks("fedora", &[late, task(21, 2)]).unwrap();
        store
            .put_hosts(
                "fedora",
                &[(1, "buildvm-x86-01".into(), "x86_64 i386".into())],
            )
            .unwrap();

        let ds = store.dataset_for("fedora", 0.0, 1000.0).unwrap();
        assert_eq!(
            ds.builds.len(),
            1,
            "only the build completing in the window"
        );
        // Its child comes with it even though the child finished later:
        // selecting children by their own completion would split a build
        // across periods.
        assert_eq!(ds.tasks.len(), 1);
        assert_eq!(ds.tasks.values().next().unwrap().task_id, 11);
        assert_eq!(
            ds.host_arches.get("fedora:1").map(String::as_str),
            Some("x86_64 i386")
        );
    }

    #[test]
    fn a_store_from_a_newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lag.sqlite");
        {
            let store = Store::open(&path).unwrap();
            store
                .conn
                .execute(
                    "UPDATE meta SET value = '99' WHERE key = 'schema_version'",
                    [],
                )
                .unwrap();
        }
        match Store::open(&path) {
            Err(e) => assert!(e.contains("schema version 99"), "{e}"),
            Ok(_) => panic!("a store from a newer schema should not open"),
        }
    }
}
