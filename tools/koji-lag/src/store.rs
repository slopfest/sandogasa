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

/// The schema, one step per version.
///
/// A step is applied to any store below its version and never again, so
/// the list is append-only: editing a step that has shipped changes what
/// new stores get without touching existing ones, and the two then differ
/// silently. To change something already released, add a step.
///
/// The committed `data/store-schema.sql` is what these steps add up to,
/// checked by a test, so a change to the schema shows up as a diff of the
/// schema rather than only as a diff of the code that makes it.
const MIGRATIONS: &[&str] = &[
    // v1: builds, tasks, hosts, channels, and the two coverage records.
    r#"
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
"#,
    // v2: builder configuration history, which is the denominator every
    // utilisation figure needs. `hosts` holds a host's name and arches as
    // they are *now*; this holds what each host was, and when, so
    // "capacity available on 2025-01-16" is a query rather than a guess.
    r#"
CREATE TABLE IF NOT EXISTS host_config (
    instance TEXT NOT NULL,
    host_id INTEGER NOT NULL,
    name TEXT,
    -- Space-separated, as the hub reports it. Kept verbatim rather than
    -- normalised into rows: a handful of revisions carry a Kerberos
    -- principal here instead of architectures, and inventing structure
    -- for that would be inventing meaning.
    arches TEXT,
    enabled INTEGER NOT NULL,
    -- Weight the host accepts at once, which is what Koji schedules
    -- against -- not a task count.
    capacity REAL,
    -- The revision is in force over [create_ts, revoke_ts); a NULL
    -- revoke_ts is the revision still current.
    create_ts REAL NOT NULL,
    revoke_ts REAL,
    PRIMARY KEY (instance, host_id, create_ts)
);
CREATE INDEX IF NOT EXISTS host_config_span
    ON host_config (instance, create_ts, revoke_ts);
"#,
];

/// The schema version this build speaks: the number of steps it knows.
///
/// A store recording a *higher* version is refused, since rows written by
/// a newer binary may mean something this one would misread. A lower one
/// is migrated up.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

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

/// Rows worth analysing, and what was left out of them.
#[derive(Debug)]
pub struct Selection {
    pub dataset: Dataset,
    /// The whole days these rows come from, merged.
    pub whole: Vec<Span>,
    /// Days in range with rows that were left out for being incomplete,
    /// as UTC midnights.
    pub skipped: Vec<f64>,
}

impl Selection {
    /// Whole days covered, for a summary line.
    pub fn days(&self) -> usize {
        (self.whole.iter().map(|s| s.to - s.from).sum::<f64>() / 86_400.0).round() as usize
    }

    /// The skipped days as dates, for saying which to sync.
    pub fn skipped_dates(&self) -> Vec<String> {
        self.skipped
            .iter()
            .map(|ts| {
                chrono::DateTime::from_timestamp(*ts as i64, 0)
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| format!("unix {ts:.0}"))
            })
            .collect()
    }
}

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
        // Wait rather than fail when another process holds the write
        // lock: reporting while a sync runs is a normal thing to want,
        // and a batch of rows takes milliseconds to commit.
        self.conn
            .busy_timeout(std::time::Duration::from_secs(30))
            .map_err(|e| e.to_string())?;
        // The version table has to exist before it can be read.
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS meta (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 );",
            )
            .map_err(|e| format!("creating the schema: {e}"))?;

        let at = self.schema_version()?;
        // Refusing is the honest answer for a store from the future: a
        // newer binary's rows may mean something this one would misread,
        // and a store is rebuildable from the hub or an import.
        if at > SCHEMA_VERSION {
            return Err(format!(
                "store was written by schema version {at}, this build speaks {SCHEMA_VERSION}"
            ));
        }
        // Each step in its own transaction, with the version bumped
        // inside it: a migration that fails leaves the store at the last
        // version it fully reached rather than half-way through one.
        for (index, step) in MIGRATIONS.iter().enumerate().skip(at as usize) {
            let version = index + 1;
            self.conn
                .execute_batch(&format!(
                    "BEGIN;
                     {step}
                     INSERT INTO meta (key, value) VALUES ('schema_version', '{version}')
                       ON CONFLICT (key) DO UPDATE SET value = excluded.value;
                     COMMIT;"
                ))
                .map_err(|e| format!("migrating the store to schema version {version}: {e}"))?;
        }
        Ok(())
    }

    /// The schema as SQLite reports it, for the committed snapshot.
    ///
    /// Taken from `sqlite_master` rather than from [`MIGRATIONS`] so it
    /// shows what the steps actually add up to — a column added by a later
    /// step appears where the table defines it, not as a trailing ALTER.
    pub fn schema_sql(&self) -> Result<String, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT sql FROM sqlite_master
                 WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
                 ORDER BY type DESC, name",
            )
            .map_err(|e| e.to_string())?;
        let statements = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(statements
            .iter()
            .map(|sql| format!("{};\n", normalize_ddl(sql)))
            .collect::<Vec<_>>()
            .join("\n"))
    }

    /// The schema version recorded in the store; 0 for a fresh one.
    fn schema_version(&self) -> Result<u32, String> {
        let found: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match found {
            None => Ok(0),
            Some(text) => text
                .parse()
                .map_err(|e| format!("unreadable schema version {text:?}: {e}")),
        }
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

    /// Give builds that have no package name one from their children.
    ///
    /// Koji answers a `build` task's request with a git URL when the source
    /// came from dist-git, and only with an SRPM path for a scratch build of
    /// an uploaded package — so a package name can be parsed from the
    /// request of 2% of builds. The *children* are different: each carries
    /// the SRPM it was handed, so 81% of them name their package. The name
    /// was therefore always in the store, one level down, and nothing but a
    /// local update was needed to put it where every report and query looks
    /// for it.
    ///
    /// Returns how many builds gained a name. Cheap to re-run: it only
    /// touches rows that still have none, so it is safe after every sweep
    /// and as a one-off repair of a store filled before this existed.
    ///
    /// `nvr` cannot be recovered the same way. A child's request holds the
    /// full NVR, but only the package parsed out of it was ever stored, so
    /// repairing that would mean asking the hub for the children again.
    pub fn fill_missing_packages(
        &mut self,
        instance: &str,
        from: f64,
        to: f64,
    ) -> Result<usize, String> {
        // Bounded by completion time so a sync pays for its own window
        // rather than rescanning every build ever stored: `package IS NULL`
        // is not indexed, but the completion range is.
        self.conn
            .execute(
                "UPDATE builds SET package = (
                     SELECT t.package FROM tasks t
                     WHERE t.instance = builds.instance AND t.parent = builds.task_id
                       AND t.package IS NOT NULL
                     LIMIT 1
                 )
                 WHERE instance = ?1 AND package IS NULL
                   AND completion_ts >= ?2 AND completion_ts < ?3
                   AND EXISTS (
                     SELECT 1 FROM tasks t
                     WHERE t.instance = builds.instance AND t.parent = builds.task_id
                       AND t.package IS NOT NULL
                   )",
                params![instance, from, to],
            )
            .map_err(|e| format!("filling package names: {e}"))
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

    /// The whole UTC days in `[from, to)` the store holds completely, as
    /// merged contiguous spans.
    ///
    /// Complete means both halves: the creation span the day depends on is
    /// listed (which reaches `grace` before it, for builds that started
    /// earlier and finished inside it), and every build completing in it
    /// has had its children fetched. This is the only honest basis for
    /// saying a period is covered — a report, a pooled report and an
    /// export all ask it rather than deciding for themselves.
    ///
    /// Candidate days come from what has been listed, so a request with no
    /// lower bound costs a query per day of *data* rather than per day
    /// since 1970.
    pub fn whole_days(
        &self,
        instance: &str,
        from: f64,
        to: f64,
        grace: f64,
    ) -> Result<Vec<Span>, String> {
        let days: Vec<f64> = self
            .listed_days(instance)?
            .into_iter()
            .filter(|d| *d >= from && *d < to)
            .collect();

        let mut whole: Vec<Span> = Vec::new();
        for day in days {
            let end = day + 86_400.0;
            let listed = self.gaps(
                instance,
                Span {
                    from: day - grace,
                    to: end,
                },
            )?;
            if !listed.is_empty() || !self.builds_needing_children(instance, day, end)?.is_empty() {
                continue;
            }
            match whole.last_mut() {
                // Contiguous days join, so a month reads as one span
                // rather than thirty.
                Some(last) if last.to >= day => last.to = end,
                _ => whole.push(Span { from: day, to: end }),
            }
        }
        Ok(whole)
    }

    /// releng's share of each UTC day's builds in `[from, to)`.
    ///
    /// The basis for dating a mass rebuild from evidence rather than from a
    /// schedule; see [`crate::rebuild`]. Selects by completion, matching
    /// how reports select, and counts every build including scratch —
    /// what matters here is who was submitting, and koschei's canaries are
    /// part of the denominator a share is measured against.
    pub fn releng_share_by_day(
        &self,
        instance: &str,
        from: f64,
        to: f64,
    ) -> Result<Vec<crate::rebuild::Day>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT cast(completion_ts / 86400 AS INTEGER) * 86400 AS day,
                        count(*),
                        sum(CASE WHEN owner = 'releng' THEN 1 ELSE 0 END)
                 FROM builds
                 WHERE instance = ?1 AND completion_ts >= ?2 AND completion_ts < ?3
                 GROUP BY day ORDER BY day",
            )
            .map_err(|e| e.to_string())?;
        let days = stmt
            .query_map(params![instance, from, to], |r| {
                Ok(crate::rebuild::Day {
                    at: r.get::<_, i64>(0)? as f64,
                    builds: r.get::<_, i64>(1)? as usize,
                    releng: r.get::<_, i64>(2)? as usize,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(days)
    }

    /// Record builder configuration revisions, replacing any already held
    /// for the same host and creation instant.
    ///
    /// Idempotent by `(instance, host_id, create_ts)`: the hub's history is
    /// append-only in practice, but a revision's `revoke_ts` fills in later
    /// when it is superseded, so re-fetching has to update rather than
    /// duplicate.
    pub fn put_host_config(
        &mut self,
        instance: &str,
        rows: &[sandogasa_kojihub::HostConfig],
    ) -> Result<usize, String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO host_config (instance, host_id, name, arches, enabled,
                                              capacity, create_ts, revoke_ts)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                     ON CONFLICT (instance, host_id, create_ts) DO UPDATE SET
                         name=excluded.name, arches=excluded.arches,
                         enabled=excluded.enabled, capacity=excluded.capacity,
                         revoke_ts=excluded.revoke_ts",
                )
                .map_err(|e| e.to_string())?;
            for r in rows {
                stmt.execute(params![
                    instance,
                    r.host_id,
                    r.name,
                    r.arches,
                    r.enabled as i64,
                    r.capacity,
                    r.create_ts,
                    r.revoke_ts,
                ])
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(rows.len())
    }

    /// Fill in `dataset.capacity`: enabled builder weight per architecture,
    /// averaged over `[from, to)` a day at a time.
    ///
    /// Averaged rather than sampled once, because a single reading
    /// misreports any window a change fell inside — and they do: half the
    /// ppc64le fleet was disabled mid-July 2025 and again for five days that
    /// November, so a monthly figure taken at the midpoint would be either
    /// the before or the after and never the truth.
    ///
    /// Called again by [`Store::analysable`] over the whole requested range,
    /// because that merges one dataset per whole day and `Dataset::merge`
    /// carries no window lengths to weight per-day means by. Recomputing
    /// once over the range the report actually covers is both simpler and
    /// the figure a reader wants.
    fn fill_capacity(
        &self,
        instance: &str,
        from: f64,
        to: f64,
        dataset: &mut Dataset,
    ) -> Result<(), String> {
        let arches: std::collections::BTreeSet<String> = dataset
            .tasks
            .values()
            .map(|t| t.arch.clone())
            .filter(|a| a != "noarch")
            .collect();
        let days = ((to - from) / 86_400.0).ceil().max(1.0) as usize;
        for arch in arches {
            let mut total = 0.0;
            for day in 0..days {
                let at = (from + day as f64 * 86_400.0 + 43_200.0).min(to);
                total += self.capacity_at(instance, &arch, at)?.1;
            }
            let mean = total / days as f64;
            if mean > 0.0 {
                dataset.capacity.insert(arch, mean);
            }
        }

        // Pools, averaged over the same days for the same reason: a fleet
        // that changes size mid-window has no single correct reading.
        let mut totals: BTreeMap<Vec<String>, f64> = BTreeMap::new();
        for day in 0..days {
            let at = (from + day as f64 * 86_400.0 + 43_200.0).min(to);
            for (arches, capacity) in self.pools_at(instance, at)? {
                *totals.entry(arches).or_default() += capacity;
            }
        }
        dataset.pools = totals
            .into_iter()
            .map(|(arches, total)| crate::dataset::Pool {
                arches,
                capacity: total / days as f64,
            })
            .filter(|p| p.capacity > 0.0)
            .collect();
        Ok(())
    }

    /// The median queue wait for one architecture on one day.
    ///
    /// The confirmation half of a two-stage filter. [`Self::arch_wait_by_day`]
    /// reports a *mean*, which is cheap to compute across every day the store
    /// holds and which over-reports: a handful of tasks that sat for days
    /// drags a day's mean over any threshold while nothing was queueing. On
    /// 2025-04-30, four `rust-scc` builds waited 392 hours each and lifted
    /// s390x's daily mean to 1.43h — the same figure F45's rebuild day
    /// produced with 1,110 tasks genuinely queueing. By median they are 47
    /// seconds and 43 minutes.
    ///
    /// A median over every day is the obvious fix and is not affordable:
    /// the window function needs a sort over fourteen million rows, and with
    /// the date predicate applied the planner takes over ten minutes. But
    /// candidates are few — about twenty days in twenty months — so the mean
    /// selects them and this confirms them, one cheap indexed query each.
    ///
    /// Over-reporting is the right failure for the first stage to have: a
    /// false positive gets filtered here, while a false negative would be a
    /// stall nobody ever hears about.
    pub fn median_wait(&self, instance: &str, arch: &str, day: f64) -> Result<Option<f64>, String> {
        self.conn
            .query_row(
                "SELECT wait FROM (
                     SELECT start_ts - create_ts AS wait
                     FROM tasks
                     WHERE instance = ?1 AND arch = ?2 AND method = 'buildArch'
                       AND start_ts IS NOT NULL
                       AND create_ts >= ?3 AND create_ts < ?3 + 86400
                     ORDER BY wait
                 )
                 LIMIT 1 OFFSET (
                     SELECT count(*) / 2 FROM tasks
                     WHERE instance = ?1 AND arch = ?2 AND method = 'buildArch'
                       AND start_ts IS NOT NULL
                       AND create_ts >= ?3 AND create_ts < ?3 + 86400
                 )",
                params![instance, arch, day],
                |r| r.get::<_, Option<f64>>(0),
            )
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other.to_string()),
            })
    }

    /// Builder pools at `at`: sets of architectures that share hosts, with
    /// each host's weight counted once.
    ///
    /// A pool is a connected component of the "some host serves both"
    /// relation, which is the only grouping that both terminates and stays
    /// correct as the fleet changes. Fedora's hosts advertise five distinct
    /// architecture lists — `aarch64`, `x86_64 i386`, `ppc64le`, `s390x`,
    /// `x86_64 i686` — and those are four pools, not five, because x86_64
    /// appears in two of them and pulls i386 and i686 into one component
    /// with it. A grouping by literal arch list would have reported the two
    /// x86 lists as independent fleets with independent headroom.
    ///
    /// Historical revisions carry the same sets in other orders and with
    /// architectures now gone (`aarch64 armhfp`, `i386 x86_64`), so the
    /// component walk normalises rather than matching strings.
    pub fn pools_at(&self, instance: &str, at: f64) -> Result<Vec<(Vec<String>, f64)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT host_id, arches, capacity
                 FROM host_config
                 WHERE instance = ?1 AND enabled = 1
                   AND create_ts <= ?2 AND (revoke_ts IS NULL OR revoke_ts > ?2)",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![instance, at], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;

        // arch -> component id, merged as hosts link architectures together.
        let mut component: BTreeMap<String, usize> = BTreeMap::new();
        let mut hosts: Vec<(Vec<String>, f64)> = Vec::new();
        for row in rows {
            let (_, arches, capacity) = row.map_err(|e| e.to_string())?;
            let arches: Vec<String> = arches
                .split_whitespace()
                .filter(|a| *a != "noarch")
                .map(str::to_string)
                .collect();
            if !arches.is_empty() {
                hosts.push((arches, capacity));
            }
        }
        let mut next = 0usize;
        for (arches, _) in &hosts {
            // The component this host joins: any its architectures already
            // belong to, else a fresh one.
            let id = arches
                .iter()
                .find_map(|a| component.get(a).copied())
                .unwrap_or_else(|| {
                    next += 1;
                    next - 1
                });
            // Everything this host serves is now in that component, and any
            // component they came from is absorbed into it.
            let absorb: Vec<usize> = arches
                .iter()
                .filter_map(|a| component.get(a).copied())
                .filter(|c| *c != id)
                .collect();
            for c in absorb {
                for v in component.values_mut() {
                    if *v == c {
                        *v = id;
                    }
                }
            }
            for a in arches {
                component.insert(a.clone(), id);
            }
        }

        let mut pools: BTreeMap<usize, (Vec<String>, f64)> = BTreeMap::new();
        for (arches, capacity) in &hosts {
            let id = component[&arches[0]];
            let e = pools.entry(id).or_default();
            e.1 += capacity; // once per host, however many arches it serves
        }
        for (arch, id) in &component {
            pools.entry(*id).or_default().0.push(arch.clone());
        }
        Ok(pools.into_values().filter(|(a, _)| !a.is_empty()).collect())
    }

    /// Enabled hosts and their total weight capacity for `arch` at `at`.
    ///
    /// A revision counts when it was in force at that instant and its
    /// architecture list mentions `arch`. Compare against hosts observed
    /// serving work **at the same instant** when checking this: comparing a
    /// noon reading against a whole day's activity once suggested 16
    /// enabled hosts on a day 29 of them ran tasks, which looked like a bug
    /// here and was a bug in the checking.
    pub fn capacity_at(&self, instance: &str, arch: &str, at: f64) -> Result<(i64, f64), String> {
        let pattern = format!("%{arch}%");
        self.conn
            .query_row(
                "SELECT count(*), coalesce(sum(capacity), 0.0)
                 FROM host_config
                 WHERE instance = ?1 AND enabled = 1
                   AND create_ts <= ?2 AND (revoke_ts IS NULL OR revoke_ts > ?2)
                   AND (' ' || arches || ' ') LIKE ?3",
                params![instance, at, pattern],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| e.to_string())
    }

    /// Each architecture's queue wait per UTC day of task *creation*, for
    /// the days in `[from, to)`.
    ///
    /// The basis for finding single-architecture stalls; see
    /// [`crate::stall`], which explains why creation day rather than start
    /// day is the honest bucket here. Counts `buildArch` tasks only —
    /// they are the work that needs a builder of that architecture — and
    /// includes those that never started, since a stall's clearest symptom
    /// is work that never ran at all.
    pub fn arch_wait_by_day(
        &self,
        instance: &str,
        from: f64,
        to: f64,
    ) -> Result<Vec<crate::stall::Day>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT cast(create_ts / 86400 AS INTEGER) * 86400 AS day, arch,
                        count(*),
                        sum(CASE WHEN start_ts IS NOT NULL THEN 1 ELSE 0 END),
                        avg(CASE WHEN start_ts IS NOT NULL
                                 THEN start_ts - create_ts END)
                 FROM tasks
                 WHERE instance = ?1 AND method = 'buildArch'
                   AND arch IS NOT NULL
                   AND create_ts >= ?2 AND create_ts < ?3
                 GROUP BY day, arch ORDER BY day, arch",
            )
            .map_err(|e| e.to_string())?;
        let mut days = stmt
            .query_map(params![instance, from, to], |r| {
                Ok(crate::stall::Day {
                    at: r.get::<_, i64>(0)? as f64,
                    arch: r.get(1)?,
                    created: r.get::<_, i64>(2)? as usize,
                    started: r.get::<_, i64>(3)? as usize,
                    // No started task means no wait to average; such a day
                    // is all `never_started`, which the rule's floor then
                    // declines to judge rather than treating zero as fast.
                    wait: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                    running: 0.0,
                    queued: 0.0,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        self.add_load(instance, from, to, &mut days)?;
        Ok(days)
    }

    /// Fill in each day's mean concurrency and queue depth.
    ///
    /// Both are time integrals over the day rather than counts, which is
    /// what makes them comparable between a day of many short tasks and a
    /// day of few long ones: total seconds spent in the state, divided by
    /// the length of the day, is the mean number of tasks in it. A task
    /// contributes to whichever days it overlaps, so work spanning midnight
    /// is charged to both rather than to the day it happened to start.
    ///
    /// Together they separate the two reasons a queue grows, which no wait
    /// figure can distinguish on its own: during F45's rebuild s390x ran
    /// 25.9 tasks at once against a queue of 643, while on 2026-05-07 it
    /// ran *none at all* against a queue of 412, having managed 5.9 the day
    /// before it. Queue up with throughput up is congestion;
    /// queue up with throughput down is an outage.
    fn add_load(
        &self,
        instance: &str,
        from: f64,
        to: f64,
        days: &mut [crate::stall::Day],
    ) -> Result<(), String> {
        // Integrated in one pass over the overlapping tasks rather than in
        // SQL: the obvious query joins every day against every task that
        // spans it, which took 14 seconds for a single week of this store.
        // A task that completed before the window cannot overlap it, and
        // one still running has no completion, which is the whole
        // predicate — and it reads the (instance, completion_ts) index.
        let mut stmt = self
            .conn
            .prepare(
                "SELECT arch, create_ts, start_ts, completion_ts
                 FROM tasks
                 WHERE instance = ?1 AND method = 'buildArch'
                   AND create_ts < ?3
                   AND (completion_ts >= ?2 OR completion_ts IS NULL)",
            )
            .map_err(|e| e.to_string())?;
        let mut load: std::collections::HashMap<(i64, String), (f64, f64)> = Default::default();
        let rows = stmt
            .query_map(params![instance, from, to], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, Option<f64>>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (arch, create, start, completion) = row.map_err(|e| e.to_string())?;
            // A task that never started stops queueing when it is closed,
            // not at the end of the window: cancelled and failed tasks all
            // carry a completion timestamp, and charging them to the window
            // edge instead made queue depth accumulate monotonically over
            // the whole store — 995 on the first stall measured, 5,511 on
            // the last, which is a bug that reads as a trend.
            let waited = (create, start.or(completion).unwrap_or(to));
            let ran = start.map(|s| (s, completion.unwrap_or(to)));
            for (span, queueing) in [(Some(waited), true), (ran, false)] {
                let Some((begin, end)) = span else { continue };
                let mut day = (begin / 86_400.0).floor() * 86_400.0;
                while day < end.min(to) {
                    let overlap = (end.min(day + 86_400.0) - begin.max(day)).max(0.0) / 86_400.0;
                    let slot = load.entry((day as i64, arch.clone())).or_insert((0.0, 0.0));
                    if queueing {
                        slot.1 += overlap;
                    } else {
                        slot.0 += overlap;
                    }
                    day += 86_400.0;
                }
            }
        }
        for day in days.iter_mut() {
            if let Some((running, queued)) = load.get(&(day.at as i64, day.arch.clone())) {
                day.running = *running;
                day.queued = *queued;
            }
        }
        Ok(())
    }

    /// The rows of `[from, to)` worth analysing, and the days left out.
    ///
    /// Only whole days go in. A day listed but not yet finished holds
    /// builds whose arch tasks have not arrived, and statistics over those
    /// do not read as incomplete — they read as a quiet day. Every consumer
    /// of this store analyses the same way as a result: `report`, `reports`
    /// and `export` all take their rows from here.
    pub fn analysable(
        &self,
        instance: &str,
        from: f64,
        to: f64,
        grace: f64,
    ) -> Result<Selection, String> {
        let whole = self.whole_days(instance, from, to, grace)?;
        let mut dataset = Dataset::new();
        for span in &whole {
            dataset.merge(self.dataset_for(instance, span.from, span.to, grace)?);
        }
        // Over the whole requested range, not per merged day: see
        // `fill_capacity`.
        self.fill_capacity(instance, from, to, &mut dataset)?;
        let mut skipped = Vec::new();
        for day in self.listed_days(instance)? {
            if day < from || day >= to {
                continue;
            }
            if !whole
                .iter()
                .any(|s| s.from <= day && s.to >= day + 86_400.0)
            {
                skipped.push(day);
            }
        }
        Ok(Selection {
            dataset,
            whole,
            skipped,
        })
    }

    /// Every whole UTC day any listed span touches, oldest first.
    pub fn listed_days(&self, instance: &str) -> Result<Vec<f64>, String> {
        let mut days = Vec::new();
        for span in self.listed(instance)? {
            let mut day = (span.from / 86_400.0).floor() * 86_400.0;
            while day < span.to {
                days.push(day);
                day += 86_400.0;
            }
        }
        days.sort_by(f64::total_cmp);
        days.dedup();
        Ok(days)
    }

    /// Everything needed to report on `[from, to)`, as the in-memory
    /// shape the report code already speaks.
    ///
    /// Reports select by completion time, so this is a window query
    /// rather than a file to find: the period a report covers is a
    /// `WHERE` clause, which is why raw data no longer needs collating.
    pub fn dataset_for(
        &self,
        instance: &str,
        from: f64,
        to: f64,
        grace: f64,
    ) -> Result<Dataset, String> {
        let mut dataset = Dataset::new();
        // Coverage is what the store holds whole, never the period asked
        // for. Claiming the request would tell a report that a half-synced
        // month was complete, and the report would say so in its header.
        // The holes between these spans are what it warns about instead.
        for span in self.whole_days(instance, from, to, grace)? {
            dataset.meta.windows.push(crate::dataset::FetchWindow {
                instance: instance.to_string(),
                from: span.from,
                to: span.to,
                fetched: chrono::Utc::now(),
                filtered: false,
            });
        }
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

        self.fill_capacity(instance, from, to, &mut dataset)?;
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

/// One DDL statement, formatted canonically: one column or constraint per
/// line, four spaces a level, comments on their own lines.
///
/// SQLite keeps the *text* a statement was created with, and splices an
/// `ALTER TABLE ADD COLUMN` into that text with its own spacing. So the
/// stored schema reflects how a migration happened to be typed, which
/// would make the committed snapshot churn on whitespace and would leave
/// an added column formatted unlike its neighbours. Reformatting on the
/// way out makes the file depend on what the schema *is* and nothing else:
/// the same statement typed on one line and across twenty produces
/// identical output.
fn normalize_ddl(sql: &str) -> String {
    // "Is this line fresh?" is read off the output rather than tracked, so
    // the two cannot disagree.
    fn end_line(out: &mut String) {
        while out.ends_with(' ') {
            out.pop();
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
    }
    fn indent(out: &mut String, depth: usize) {
        if out.is_empty() || out.ends_with('\n') {
            out.push_str(&"    ".repeat(depth));
        }
    }

    // Only a table's column list is worth breaking up. An index's
    // columns, and a constraint's, read better where they are — and
    // "break the outermost list of a CREATE TABLE" is a rule that gives
    // one answer for any input, which is the whole point.
    let mut words = sql.split_whitespace();
    let is_table = matches!(
        (words.next(), words.next()),
        (Some(first), Some(second))
            if first.eq_ignore_ascii_case("CREATE") && second.eq_ignore_ascii_case("TABLE")
    );

    let mut out = String::new();
    let mut depth = 0usize;
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // A comment runs to the end of its line and takes the line
            // with it, wherever in the statement it appeared.
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                let rest: String = chars.by_ref().take_while(|c| *c != '\n').collect();
                end_line(&mut out);
                indent(&mut out, depth);
                out.push_str("--");
                out.push_str(rest.trim_end());
                end_line(&mut out);
            }
            // Whitespace inside a quoted literal is data, not layout.
            '\'' => {
                indent(&mut out, depth);
                out.push(c);
                for q in chars.by_ref() {
                    out.push(q);
                    if q == '\'' {
                        break;
                    }
                }
            }
            '(' => {
                indent(&mut out, depth);
                out.push('(');
                depth += 1;
                if is_table && depth == 1 {
                    end_line(&mut out);
                }
            }
            ')' => {
                if is_table && depth == 1 {
                    end_line(&mut out);
                }
                depth = depth.saturating_sub(1);
                indent(&mut out, depth);
                out.push(')');
            }
            ',' if is_table && depth == 1 => {
                out.push(',');
                end_line(&mut out);
            }
            c if c.is_whitespace() => {
                if !out.is_empty() && !out.ends_with('\n') && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            c => {
                indent(&mut out, depth);
                out.push(c);
            }
        }
    }
    out.trim_end().to_string()
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

    fn cfg(
        host: i64,
        arches: &str,
        enabled: bool,
        cap: f64,
        from: f64,
        to: Option<f64>,
    ) -> sandogasa_kojihub::HostConfig {
        sandogasa_kojihub::HostConfig {
            host_id: host,
            name: Some(format!("buildvm-{host}.example.org")),
            arches: Some(arches.to_string()),
            enabled,
            capacity: Some(cap),
            create_ts: from,
            revoke_ts: to,
        }
    }

    #[test]
    fn capacity_is_what_was_enabled_at_that_instant() {
        // The denominator every utilisation figure needs: not what the
        // fleet is now, but what it was during the window being measured.
        let mut store = Store::in_memory().unwrap();
        store
            .put_host_config(
                "fedora",
                &[
                    // Two s390x hosts, one of which is disabled partway.
                    cfg(1, "s390x", true, 6.0, 1_000.0, None),
                    cfg(2, "s390x", true, 3.0, 1_000.0, Some(5_000.0)),
                    cfg(2, "s390x", false, 3.0, 5_000.0, None),
                    // Another architecture entirely, never counted here.
                    cfg(3, "x86_64", true, 20.0, 1_000.0, None),
                ],
            )
            .unwrap();
        assert_eq!(
            store.capacity_at("fedora", "s390x", 2_000.0).unwrap(),
            (2, 9.0)
        );
        // After host 2 is disabled, only host 1 remains.
        assert_eq!(
            store.capacity_at("fedora", "s390x", 6_000.0).unwrap(),
            (1, 6.0)
        );
        // Before any revision existed there was nothing.
        assert_eq!(
            store.capacity_at("fedora", "s390x", 500.0).unwrap(),
            (0, 0.0)
        );
        assert_eq!(
            store.capacity_at("fedora", "x86_64", 2_000.0).unwrap(),
            (1, 20.0)
        );
    }

    #[test]
    fn a_multi_arch_host_counts_for_each_of_its_arches() {
        let mut store = Store::in_memory().unwrap();
        store
            .put_host_config("fedora", &[cfg(1, "x86_64 i386", true, 4.0, 0.0, None)])
            .unwrap();
        for arch in ["x86_64", "i386"] {
            assert_eq!(store.capacity_at("fedora", arch, 10.0).unwrap(), (1, 4.0));
        }
        // And a substring of an arch name is not that arch: matching on
        // "386" must not pick up "i386" for a query about "86".
        assert_eq!(
            store.capacity_at("fedora", "s390x", 10.0).unwrap(),
            (0, 0.0)
        );
    }

    #[test]
    fn refetching_the_history_updates_a_revision_rather_than_duplicating_it() {
        // A revision's revoke_ts is filled in only once it is superseded,
        // so the same fetch run twice must not double the fleet.
        let mut store = Store::in_memory().unwrap();
        store
            .put_host_config("fedora", &[cfg(1, "s390x", true, 6.0, 1_000.0, None)])
            .unwrap();
        assert_eq!(
            store.capacity_at("fedora", "s390x", 2_000.0).unwrap(),
            (1, 6.0)
        );
        store
            .put_host_config(
                "fedora",
                &[cfg(1, "s390x", true, 6.0, 1_000.0, Some(3_000.0))],
            )
            .unwrap();
        assert_eq!(
            store.capacity_at("fedora", "s390x", 2_000.0).unwrap(),
            (1, 6.0)
        );
        // Now revoked, so it no longer counts afterwards.
        assert_eq!(
            store.capacity_at("fedora", "s390x", 4_000.0).unwrap(),
            (0, 0.0)
        );
    }

    #[test]
    fn a_days_load_is_the_time_spent_not_the_task_count() {
        // Three tasks on one day, arranged so counting them and integrating
        // them give different answers: two ran for a quarter of the day at
        // the same time, one for half of it.
        const DAY: f64 = 86_400.0;
        let mut store = Store::in_memory().unwrap();
        let at = |id, arch: &str, create: f64, start: f64, done: f64| TaskRecord {
            arch: arch.into(),
            create_ts: create,
            start_ts: Some(start),
            completion_ts: Some(done),
            ..task(id, 0)
        };
        store
            .put_tasks(
                "fedora",
                &[
                    at(1, "s390x", DAY, DAY, DAY + DAY / 4.0),
                    at(2, "s390x", DAY, DAY, DAY + DAY / 4.0),
                    at(3, "s390x", DAY, DAY + DAY / 2.0, DAY * 2.0),
                    // Queued a quarter of the day before starting.
                    at(4, "x86_64", DAY, DAY + DAY / 4.0, DAY + DAY / 2.0),
                ],
            )
            .unwrap();
        let days = store.arch_wait_by_day("fedora", DAY, DAY * 2.0).unwrap();

        let s390x = days.iter().find(|d| d.arch == "s390x").unwrap();
        // 0.25 + 0.25 + 0.5 of a day's worth of running.
        assert!((s390x.running - 1.0).abs() < 1e-9, "{:?}", s390x);
        // Task 3 waited half a day; the others started at once.
        assert!((s390x.queued - 0.5).abs() < 1e-9, "{:?}", s390x);
        assert_eq!(s390x.created, 3);

        let x86 = days.iter().find(|d| d.arch == "x86_64").unwrap();
        assert!((x86.running - 0.25).abs() < 1e-9, "{x86:?}");
        assert!((x86.queued - 0.25).abs() < 1e-9, "{x86:?}");
    }

    #[test]
    fn pools_join_architectures_that_share_a_builder() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("s.sqlite")).unwrap();
        // Fedora's actual shape: x86_64 appears in two different host
        // lists, which pulls i386 and i686 into one pool with it, while
        // s390x and ppc64le keep their own.
        let rows: Vec<sandogasa_kojihub::HostConfig> = [
            (1i64, "x86_64 i386", 10.0),
            (2, "i386 x86_64", 10.0), // same set, written the other way
            (3, "x86_64 i686", 5.0),  // links i686 in through x86_64
            (4, "s390x", 4.0),
            (5, "ppc64le", 2.0),
            (6, "ppc64le", 2.0),
        ]
        .into_iter()
        .map(
            |(host_id, arches, capacity)| sandogasa_kojihub::HostConfig {
                host_id,
                name: Some(format!("buildhw-{host_id}")),
                arches: Some(arches.to_string()),
                enabled: true,
                capacity: Some(capacity),
                create_ts: 0.0,
                revoke_ts: None,
            },
        )
        .collect();
        store.put_host_config("fedora", &rows).unwrap();

        let mut pools = store.pools_at("fedora", 100.0).unwrap();
        pools.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            pools,
            vec![
                (
                    vec!["i386".to_string(), "i686".into(), "x86_64".into()],
                    25.0
                ),
                (vec!["ppc64le".to_string()], 4.0),
                (vec!["s390x".to_string()], 4.0),
            ]
        );
        // Each host's weight counts once however many arches it serves, so
        // the x86 pool is 10 + 10 + 5 and not doubled for i386.
        assert_eq!(pools[0].1, 25.0);
    }

    #[test]
    fn work_spanning_midnight_is_charged_to_both_days() {
        // The reason for integrating rather than grouping by start day: a
        // task running across midnight was serving both days, and an
        // outage on the second one must not be hidden by it.
        const DAY: f64 = 86_400.0;
        let mut store = Store::in_memory().unwrap();
        store
            .put_tasks(
                "fedora",
                &[TaskRecord {
                    arch: "s390x".into(),
                    create_ts: DAY + DAY / 2.0,
                    start_ts: Some(DAY + DAY / 2.0),
                    completion_ts: Some(DAY * 2.0 + DAY / 2.0),
                    ..task(1, 0)
                }],
            )
            .unwrap();
        let days = store.arch_wait_by_day("fedora", DAY, DAY * 3.0).unwrap();
        // Created on day one, so that is the only row — but its second half
        // day of running belongs to day two, which has no row of its own.
        let first = days.iter().find(|d| d.at == DAY).unwrap();
        assert!((first.running - 0.5).abs() < 1e-9, "{first:?}");
    }

    #[test]
    fn an_unfinished_task_is_charged_only_to_the_window() {
        // A task still running when the window ends must not contribute
        // beyond it, or the last day of any report reads as overloaded.
        const DAY: f64 = 86_400.0;
        let mut store = Store::in_memory().unwrap();
        store
            .put_tasks(
                "fedora",
                &[TaskRecord {
                    arch: "s390x".into(),
                    create_ts: DAY,
                    start_ts: Some(DAY + DAY / 2.0),
                    completion_ts: None,
                    ..task(1, 0)
                }],
            )
            .unwrap();
        let days = store.arch_wait_by_day("fedora", DAY, DAY * 2.0).unwrap();
        let day = days.iter().find(|d| d.arch == "s390x").unwrap();
        assert_eq!(day.started, 1);
        assert!((day.running - 0.5).abs() < 1e-9, "{day:?}");
        assert!((day.queued - 0.5).abs() < 1e-9, "{day:?}");
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

        let ds = store
            .dataset_for("fedora", 0.0, 1000.0, 3.0 * 86_400.0)
            .unwrap();
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
    fn formatting_a_statement_does_not_depend_on_how_it_was_typed() {
        // The same table, typed three ways: across lines with deep
        // indentation, all on one line, and as SQLite leaves it after an
        // ALTER. All three must export identically, which is what keeps
        // the committed schema from churning on whitespace.
        let sprawling = "CREATE TABLE t (\n                  a TEXT NOT NULL,\n\
                         \n     -- why b exists\n       b INTEGER DEFAULT 0,\n\
                         PRIMARY KEY (a)\n)";
        let one_line = "CREATE TABLE t (a TEXT NOT NULL, -- why b exists\nb INTEGER DEFAULT 0, PRIMARY KEY (a))";
        let altered = "CREATE TABLE t (a TEXT NOT NULL,   -- why b exists\n b INTEGER DEFAULT 0, PRIMARY KEY (a))";
        let want = "CREATE TABLE t (\n    a TEXT NOT NULL,\n    -- why b exists\n    \
                    b INTEGER DEFAULT 0,\n    PRIMARY KEY (a)\n)";
        assert_eq!(normalize_ddl(sprawling), want);
        assert_eq!(normalize_ddl(one_line), want);
        assert_eq!(normalize_ddl(altered), want);
    }

    #[test]
    fn only_a_tables_own_column_list_is_broken_up() {
        // An index stays on one line however it was typed, so the schema
        // file does not turn five short definitions into thirty lines.
        assert_eq!(
            normalize_ddl("CREATE INDEX i\n  ON t (a,\n  b)"),
            "CREATE INDEX i ON t (a, b)"
        );
    }

    #[test]
    fn formatting_leaves_quoted_text_alone() {
        // Whitespace inside a literal is data, not layout.
        let sql = "CREATE TABLE t (a TEXT DEFAULT 'two  words')";
        assert!(
            normalize_ddl(sql).contains("'two  words'"),
            "{}",
            normalize_ddl(sql)
        );
    }

    /// Snapshot test: the committed schema must match what the store
    /// builds. Regenerate with `UPDATE_SCHEMA=1 cargo test -p koji-lag
    /// store_schema_up_to_date`.
    #[test]
    fn a_build_takes_its_package_name_from_its_children() {
        // The shape Koji actually produces: a dist-git build whose request
        // is a git URL, so nothing parsed a package from it, with children
        // that were handed the SRPM and did.
        let mut store = Store::in_memory().unwrap();
        let mut parent = build(1, 100.0);
        parent.package = None;
        let mut orphan = build(2, 100.0);
        orphan.package = None;
        store.put_builds("fedora", &[parent, orphan]).unwrap();
        let mut child = task(11, 1);
        child.package = Some("gcc".to_string());
        let mut nameless = task(12, 1);
        nameless.package = None;
        store.put_tasks("fedora", &[child, nameless]).unwrap();

        assert_eq!(
            store.fill_missing_packages("fedora", 0.0, 9999.0).unwrap(),
            1
        );
        let ds = store.dataset_for("fedora", 0.0, 9999.0, 0.0).unwrap();
        assert_eq!(ds.builds["fedora:1"].package.as_deref(), Some("gcc"));
        // A build whose children name nothing keeps its absence, rather
        // than borrowing a name from some other build.
        assert_eq!(ds.builds["fedora:2"].package, None);
        // Idempotent: nothing left to fill.
        assert_eq!(
            store.fill_missing_packages("fedora", 0.0, 9999.0).unwrap(),
            0
        );
    }

    #[test]
    fn store_schema_up_to_date() {
        let store = Store::in_memory().unwrap();
        let expected = store.schema_sql().unwrap();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/store-schema.sql");
        if std::env::var("UPDATE_SCHEMA").is_ok() {
            let header = "-- Generated by `UPDATE_SCHEMA=1 cargo test -p koji-lag \
                          store_schema_up_to_date`.\n-- The schema koji-lag's SQLite store \
                          is migrated to; see src/store.rs MIGRATIONS.\n\n";
            std::fs::write(&path, format!("{header}{expected}")).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(&path)
            .expect("schema file missing; run UPDATE_SCHEMA=1 cargo test");
        let body: String = on_disk
            .lines()
            .skip_while(|l| l.starts_with("--") || l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            body.trim(),
            expected.trim(),
            "schema drift; run UPDATE_SCHEMA=1 cargo test -p koji-lag store_schema_up_to_date"
        );
    }

    #[test]
    fn a_store_from_an_older_schema_is_migrated_not_refused() {
        // What every existing store looks like to a build with more
        // migrations than it was written by: the steps it is missing get
        // applied, and its rows survive.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lag.sqlite");
        {
            let mut store = Store::open(&path).unwrap();
            store.put_builds("fedora", &[build(7, 200.0)]).unwrap();
            // Pretend it was written before the current version.
            store
                .conn
                .execute(
                    "UPDATE meta SET value = '0' WHERE key = 'schema_version'",
                    [],
                )
                .unwrap();
        }
        let store = Store::open(&path).expect("an older store must open");
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        assert_eq!(store.counts().unwrap()["fedora"].builds, 1, "rows kept");
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
