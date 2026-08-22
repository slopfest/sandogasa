// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The interesting windows, in one chronological list.
//!
//! Two kinds of thing are worth reporting on and they arrive from opposite
//! directions. A mass rebuild is announced — the schedule names it, and
//! [`crate::rebuild`] finds when it actually ran. A single-architecture
//! stall is announced by nobody and only [`crate::stall`] knows it
//! happened. Fourteen months of this store holds three of the first and
//! nineteen of the second.
//!
//! They share one flat directory of `<date>-<what>` because filing by
//! release cannot hold them both: fourteen of those stalls belong to no
//! release event at all, and a tree of `f-45/mass-rebuild/` would have
//! nowhere to put them. The release is an *attribute* of an event here,
//! contributed by the schedule, rather than the shape of the tree — which
//! also means a reader sees everything that happened in order, which is
//! the question people actually ask.
//!
//! Each event carries what measurement can say (how long, how far behind,
//! whether the builders were working) and what it cannot (why), the latter
//! from [`crate::annotate`].

use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::{annotate, rebuild, schedule, stall, store::Store};

/// What kind of window this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// releng submitting a release's worth of builds.
    MassRebuild,
    /// One architecture far behind its peers while its builders worked.
    Congestion,
    /// One architecture far behind while its builders did less than usual.
    Outage,
}

impl Kind {
    pub fn slug(self) -> &'static str {
        match self {
            Kind::MassRebuild => "mass-rebuild",
            Kind::Congestion => "congestion",
            Kind::Outage => "outage",
        }
    }
}

/// One window worth its own directory.
///
/// `Deserialize` as well as `Serialize`, so that `event.json` can be read
/// back and re-rendered without the store — see [`reannotate`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub kind: Kind,
    /// The architecture affected, for a stall; absent for a rebuild, which
    /// is about all of them.
    pub arch: Option<String>,
    pub from: f64,
    /// Exclusive end.
    pub to: f64,
    pub days: usize,
    /// Release whose cycle this fell in, when a schedule was supplied.
    pub release: Option<u32>,
    /// What the schedule announced, for a rebuild: `(start, end)`.
    pub announced: Option<(NaiveDate, NaiveDate)>,
    /// Measured facts, as label and value, in reporting order.
    pub facts: Vec<(String, String)>,
    /// Why it happened, if anybody wrote it down.
    pub causes: Vec<annotate::Note>,
}

impl Event {
    /// Directory name: the date first, so a listing sorts chronologically.
    pub fn slug(&self) -> String {
        let date = day_name(self.from);
        match &self.arch {
            Some(arch) => format!("{date}-{arch}-{}", self.kind.slug()),
            None => format!("{date}-{}", self.kind.slug()),
        }
    }

    /// The human summary, which is the file most readers will open.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let span = format!(
            "{} .. {} ({} day{})",
            day_name(self.from),
            day_name(self.to - 86_400.0),
            self.days,
            if self.days == 1 { "" } else { "s" }
        );
        out.push_str(&match &self.arch {
            Some(arch) => format!("{} on {arch}\n", self.kind.slug()),
            None => format!("{}\n", self.kind.slug()),
        });
        out.push_str(&format!("{}\n\n", "=".repeat(60)));
        out.push_str(&format!("  when       {span}\n"));
        if let Some(release) = self.release {
            out.push_str(&format!("  release    f{release}\n"));
        }
        if let Some((start, end)) = self.announced {
            // The comparison the schedule is here for. F45's rebuild was
            // announced over four weeks and submitted in four days, so
            // quoting only one of the two misleads either way.
            out.push_str(&format!("  announced  {start} .. {end}\n"));
        }
        for (label, value) in &self.facts {
            out.push_str(&format!("  {label:<10} {value}\n"));
        }
        out.push('\n');
        // Only an outage is missing something when it has no cause. A
        // rebuild's cause is that it is a rebuild, and congestion's is the
        // load in the figures above; saying "not recorded" of either would
        // manufacture a gap.
        if self.causes.is_empty() && self.kind == Kind::Outage {
            out.push_str("  cause      not recorded — see unexplained.toml beside this tree\n");
        }
        for note in &self.causes {
            out.push_str(&format!("  cause      {}\n", note.cause));
            if let Some(ticket) = &note.ticket {
                out.push_str(&format!("  ticket     {ticket}\n"));
            }
            if let Some(text) = &note.note {
                out.push('\n');
                for line in text.trim().lines() {
                    out.push_str(&format!("  {line}\n"));
                }
            }
        }
        out
    }
}

/// Every event in `[from, to)`, oldest first.
///
/// `schedule` and `notes` are both optional in effect: an empty slice
/// simply leaves the release attribution and the causes off.
pub fn assemble(
    store: &Store,
    instance: &str,
    from: f64,
    to: f64,
    schedule: &[schedule::Event],
    notes: &[annotate::Note],
) -> Result<Vec<Event>, String> {
    let mut events = Vec::new();

    for window in rebuild::windows(
        &rebuild::days(store, instance, from, to)?,
        rebuild::Rule::default(),
    ) {
        let announced = release_of(schedule, window.from).and_then(|r| {
            schedule
                .iter()
                .find(|e| e.release == r && e.kind == schedule::Kind::MassRebuild)
                .map(|e| (e.start, e.end))
        });
        events.push(Event {
            kind: Kind::MassRebuild,
            arch: None,
            from: window.from,
            to: window.to,
            days: window.days(),
            release: release_of(schedule, window.from),
            announced,
            facts: vec![
                ("builds".into(), window.builds.to_string()),
                (
                    "releng".into(),
                    format!(
                        "{} ({:.0}% of the window)",
                        window.releng,
                        100.0 * window.share()
                    ),
                ),
            ],
            causes: Vec::new(),
        });
    }

    let arch_days = store.arch_wait_by_day(instance, from, to)?;
    let rule = stall::Rule::default();
    for found in stall::stalls(&arch_days, rule) {
        // Second stage: the daily means that selected this window are
        // over-inclusive, so confirm with an exact median on its days. A
        // window whose best day is under the floor by median was a few
        // stuck tasks and not a queue -- see `Store::median_wait`.
        let mut best: Option<f64> = None;
        let mut day = found.from;
        while day < found.to {
            if let Some(m) = store.median_wait(instance, &found.arch, day)? {
                best = Some(best.map_or(m, |b: f64| b.max(m)));
            }
            day += 86_400.0;
        }
        if best.is_some_and(|m| m < rule.median_floor) {
            continue;
        }
        let ordinary = stall::baseline(&arch_days, &found.arch);
        let verdict = found.verdict(ordinary);
        events.push(Event {
            kind: match verdict {
                stall::Verdict::Outage => Kind::Outage,
                stall::Verdict::Congestion => Kind::Congestion,
            },
            arch: Some(found.arch.clone()),
            from: found.from,
            to: found.to,
            days: found.days(),
            release: release_of(schedule, found.from),
            announced: None,
            facts: vec![
                (
                    "worst wait".into(),
                    format!(
                        "{:.1}h, against {:.0}m for the other architectures ({:.0}x)",
                        found.worst / 3_600.0,
                        found.others / 60.0,
                        found.factor()
                    ),
                ),
                (
                    "throughput".into(),
                    format!(
                        "{:.1} tasks at once, {:.1} at its worst, against {ordinary:.1} ordinarily",
                        found.running, found.trough
                    ),
                ),
                ("queue".into(), format!("{:.0} tasks waiting", found.queued)),
                (
                    "tasks".into(),
                    format!(
                        "{} created, {} never ran",
                        found.created, found.never_started
                    ),
                ),
            ],
            causes: Vec::new(),
        });
    }

    events.sort_by(|a, b| {
        a.from
            .total_cmp(&b.from)
            .then_with(|| a.slug().cmp(&b.slug()))
    });

    // Causes last, so every event exists to be matched against.
    let windows: Vec<(Option<String>, f64, f64)> = events
        .iter()
        .map(|e| (e.arch.clone(), e.from, e.to))
        .collect();
    let matched = annotate::match_windows(notes, instance, &windows);
    for (event, hits) in events.iter_mut().zip(&matched.per_window) {
        event.causes = hits.iter().map(|n| (*n).clone()).collect();
    }
    Ok(events)
}

/// Notes that matched no event, for the caller to warn about.
pub fn unmatched<'a>(
    events: &[Event],
    instance: &str,
    notes: &'a [annotate::Note],
    scanned: (f64, f64),
) -> Vec<&'a annotate::Note> {
    let windows: Vec<(Option<String>, f64, f64)> = events
        .iter()
        .map(|e| (e.arch.clone(), e.from, e.to))
        .collect();
    let (from, to) = scanned;
    annotate::match_windows(notes, instance, &windows)
        .unmatched
        .into_iter()
        // A note about November is not a mistake when only May was asked
        // for. Warning about it anyway teaches people that these warnings
        // are noise, and the one case worth hearing -- a note overlapping
        // what was scanned and still matching nothing -- gets lost in it.
        .filter(|n| {
            let a = day_start(n.from);
            let b = day_start(n.to) + 86_400.0;
            a < to && b > from
        })
        .collect()
}

fn day_start(d: NaiveDate) -> f64 {
    d.and_hms_opt(0, 0, 0)
        .expect("midnight")
        .and_utc()
        .timestamp() as f64
}

/// The release whose cycle `ts` falls in: the most recent one whose mass
/// rebuild had started by then.
///
/// Rough on purpose. A precise answer would need each release's whole
/// span, and the schedules disagree about their own end dates — what this
/// is for is saying "this happened during f45's cycle" beside an event.
fn release_of(schedule: &[schedule::Event], ts: f64) -> Option<u32> {
    schedule
        .iter()
        .filter(|e| e.kind == schedule::Kind::MassRebuild)
        .filter(|e| midnight(e.start) <= ts)
        .max_by_key(|e| e.start)
        .map(|e| e.release)
}

fn midnight(date: NaiveDate) -> f64 {
    date.and_hms_opt(0, 0, 0)
        .map(|t| t.and_utc().timestamp() as f64)
        .unwrap_or(0.0)
}

pub fn day_name(ts: f64) -> String {
    chrono::DateTime::from_timestamp(ts as i64, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| format!("unix {ts:.0}"))
}

/// Write `events/<slug>/` under `root`, one directory per event.
///
/// Returns what was written. The per-window report is left to the caller,
/// which already knows how to build one.
pub fn write(root: &Path, event: &Event) -> Result<Vec<PathBuf>, String> {
    let dir = root.join("events").join(event.slug());
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut written = Vec::new();
    for (name, body) in [
        ("event.txt", event.render()),
        (
            "event.json",
            format!(
                "{}\n",
                serde_json::to_string_pretty(event).map_err(|e| e.to_string())?
            ),
        ),
    ] {
        let path = dir.join(name);
        std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
        written.push(path);
    }
    Ok(written)
}

/// Where an event's files go, for a caller writing the report beside them.
/// Re-match annotations against an events tree already on disk.
///
/// The store is not opened. Everything a rendering needs is in each
/// `event.json` — kind, architecture, dates, the measured facts — so
/// splicing in a cause that somebody has just written costs a directory
/// walk rather than the two minutes of SQL that found the windows. Which is
/// the point: nobody will annotate an outage if doing so means re-running
/// the detection.
///
/// Returns the events as they now stand, so the caller can rewrite the
/// stub file from them.
pub fn reannotate(
    root: &Path,
    instance: &str,
    notes: &[annotate::Note],
) -> Result<Vec<Event>, String> {
    let dir = root.join("events");
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("{}: no events tree here", dir.display()));
        }
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let file = path.join("event.json");
        let Ok(body) = std::fs::read_to_string(&file) else {
            continue;
        };
        let mut event: Event =
            serde_json::from_str(&body).map_err(|e| format!("{}: {e}", file.display()))?;
        let windows = [(event.arch.clone(), event.from, event.to)];
        event.causes = annotate::match_windows(notes, instance, &windows)
            .per_window
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        out.push(event);
    }
    Ok(out)
}

/// Write `unexplained.toml` beside an events tree, or remove it when
/// everything has a cause.
///
/// Removed rather than left stale: a file listing outages that have since
/// been explained is worse than no file, because the next person believes
/// it.
pub fn write_unexplained(
    root: &Path,
    instance: &str,
    events: &[Event],
) -> Result<Option<PathBuf>, String> {
    let stubs: Vec<String> = events
        .iter()
        .filter(|e| e.kind == Kind::Outage && e.causes.is_empty())
        .map(|e| {
            annotate::stub(
                instance,
                e.arch.as_deref(),
                day_of(e.from),
                // `to` is exclusive here and inclusive in a note, as a
                // person would write it.
                day_of(e.to - 86_400.0),
                &e.facts,
            )
        })
        .collect();
    let path = root.join("unexplained.toml");
    if stubs.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(None);
    }
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    std::fs::write(&path, annotate::stub_file(&stubs))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(path))
}

fn day_of(ts: f64) -> NaiveDate {
    DateTime::from_timestamp(ts as i64, 0)
        .expect("in range")
        .date_naive()
}

pub fn dir(root: &Path, event: &Event) -> PathBuf {
    root.join("events").join(event.slug())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(arch: &str, from: &str, to: &str, cause: &str) -> annotate::Note {
        annotate::Note {
            instance: "fedora".into(),
            arch: Some(arch.into()),
            from: from.parse().unwrap(),
            to: to.parse().unwrap(),
            cause: cause.into(),
            ticket: Some("https://example.invalid/1".into()),
            note: None,
        }
    }

    fn event(kind: Kind, arch: Option<&str>, from: &str, days: usize) -> Event {
        let start = midnight(from.parse::<NaiveDate>().unwrap());
        Event {
            kind,
            arch: arch.map(str::to_string),
            from: start,
            to: start + days as f64 * 86_400.0,
            days,
            release: None,
            announced: None,
            facts: vec![("queue".into(), "374 tasks waiting".into())],
            causes: Vec::new(),
        }
    }

    /// A store holding one releng burst and one architecture left behind
    /// by it, which is the shape `assemble` exists to recognise.
    fn store_with_a_rebuild_and_a_stall() -> (tempfile::TempDir, crate::store::Store) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = crate::store::Store::open(&dir.path().join("s.sqlite")).unwrap();
        let day = 86_400.0;
        let mut builds = Vec::new();
        let mut tasks = Vec::new();
        let mut tid = 1i64;
        for d in 0..8 {
            // Every day carries a baseline of ordinary builds, above the
            // rule's floor of 500 so the day is judged at all; days 2 to 5
            // add a releng burst on top, which is what a rebuild looks
            // like as a *share* rather than as a raw count.
            let rebuilding = (2..6).contains(&d);
            let ordinary = 600;
            let burst = if rebuilding { 900 } else { 0 };
            for i in 0..(ordinary + burst) {
                let releng = i >= ordinary;
                let parent = 100_000 + tid;
                let created = day * d as f64 + (i % 600) as f64;
                builds.push(crate::dataset::BuildRecord {
                    instance: "fedora".into(),
                    task_id: parent,
                    package: Some(format!("p{i}")),
                    nvr: None,
                    target: Some(if releng {
                        "f45-rebuild".into()
                    } else {
                        "f45".into()
                    }),
                    owner: Some(if releng {
                        "releng".into()
                    } else {
                        "someone".into()
                    }),
                    scratch: false,
                    state: 2,
                    create_ts: created,
                    start_ts: Some(created),
                    completion_ts: Some(created + 600.0),
                    priority: Some(if releng { 25 } else { 20 }),
                    host_id: None,
                });
                for arch in ["x86_64", "s390x"] {
                    tid += 1;
                    // s390x waits hours while the rebuild runs and seconds
                    // otherwise; x86_64 always answers at once.
                    let wait = if arch == "s390x" && rebuilding {
                        4.0 * 3_600.0
                    } else {
                        30.0
                    };
                    tasks.push(crate::dataset::TaskRecord {
                        instance: "fedora".into(),
                        task_id: 500_000 + tid,
                        parent: Some(parent),
                        arch: arch.into(),
                        method: crate::dataset::BUILD_ARCH.into(),
                        package: Some(format!("p{i}")),
                        state: 2,
                        create_ts: created,
                        start_ts: Some(created + wait),
                        completion_ts: Some(created + wait + 600.0),
                        host_id: Some(1),
                        channel_id: None,
                        weight: Some(1.5),
                    });
                }
            }
        }
        store.put_builds("fedora", &builds).unwrap();
        store.put_tasks("fedora", &tasks).unwrap();
        (dir, store)
    }

    #[test]
    fn assemble_finds_the_rebuild_and_the_architecture_it_left_behind() {
        let (_dir, store) = store_with_a_rebuild_and_a_stall();
        let events = assemble(&store, "fedora", 0.0, 86_400.0 * 8.0, &[], &[]).unwrap();

        let rebuilds: Vec<&Event> = events
            .iter()
            .filter(|e| e.kind == Kind::MassRebuild)
            .collect();
        assert_eq!(rebuilds.len(), 1, "{events:#?}");
        assert!(rebuilds[0].arch.is_none(), "a rebuild is about every arch");
        assert!(rebuilds[0].days >= 3, "{:?}", rebuilds[0].days);

        // The architecture that queued is named; the one that kept up is not.
        let stalls: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(e.kind, Kind::Congestion | Kind::Outage))
            .collect();
        assert!(!stalls.is_empty(), "{events:#?}");
        assert!(
            stalls.iter().all(|e| e.arch.as_deref() == Some("s390x")),
            "{stalls:#?}"
        );

        // Every event carries the measured facts its stanza and rendering
        // are built from.
        assert!(events.iter().all(|e| !e.facts.is_empty()));
        // Nothing was annotated, so nothing claims a cause.
        assert!(events.iter().all(|e| e.causes.is_empty()));
    }

    #[test]
    fn assemble_attaches_a_cause_to_the_window_it_covers() {
        let (_dir, store) = store_with_a_rebuild_and_a_stall();
        // Dated by overlap rather than exactly, which is the point of the
        // matching: 1970-01-04 falls inside the stall without being its edge.
        let notes = vec![note("s390x", "1970-01-04", "1970-01-04", "storage")];
        let events = assemble(&store, "fedora", 0.0, 86_400.0 * 8.0, &[], &notes).unwrap();
        let stall = events
            .iter()
            .find(|e| e.arch.as_deref() == Some("s390x"))
            .expect("a stall");
        assert_eq!(stall.causes.len(), 1, "{stall:#?}");
        assert_eq!(stall.causes[0].cause, "storage");
        // And the note is not reported as having matched nothing.
        assert!(unmatched(&events, "fedora", &notes, (0.0, 86_400.0 * 8.0)).is_empty());
    }

    #[test]
    fn a_slug_sorts_by_date_and_names_what_happened() {
        let e = event(Kind::Outage, Some("s390x"), "2026-05-06", 3);
        assert_eq!(e.slug(), "2026-05-06-s390x-outage");
        let r = event(Kind::MassRebuild, None, "2026-07-15", 4);
        assert_eq!(r.slug(), "2026-07-15-mass-rebuild");
        // Chronological order is lexical order, which is the point of
        // putting the date first in a flat directory.
        let mut slugs = [r.slug(), e.slug()];
        slugs.sort();
        assert_eq!(
            slugs,
            ["2026-05-06-s390x-outage", "2026-07-15-mass-rebuild"]
        );
    }

    #[test]
    fn an_outage_with_no_cause_says_so() {
        // The whole reason annotations exist: silence here would read as
        // "there was nothing to say", which is not the same as "nobody
        // wrote it down".
        let rendered = event(Kind::Outage, Some("s390x"), "2026-05-06", 3).render();
        assert!(rendered.contains("not recorded"), "{rendered}");
        // Points at the stub beside the tree, not at a path inside a
        // checkout the reader may not have.
        assert!(rendered.contains("unexplained.toml"), "{rendered}");
        assert!(
            rendered.contains("2026-05-06 .. 2026-05-08 (3 days)"),
            "{rendered}"
        );
    }

    #[test]
    fn a_cause_and_its_ticket_are_rendered() {
        let mut e = event(Kind::Outage, Some("s390x"), "2026-05-06", 3);
        e.causes = vec![note("s390x", "2026-05-06", "2026-05-08", "storage")];
        let rendered = e.render();
        assert!(rendered.contains("cause      storage"), "{rendered}");
        assert!(rendered.contains("example.invalid"), "{rendered}");
        assert!(!rendered.contains("not recorded"), "{rendered}");
    }

    #[test]
    fn a_rebuild_shows_announced_beside_observed() {
        let mut e = event(Kind::MassRebuild, None, "2026-07-15", 4);
        e.release = Some(45);
        e.announced = Some(("2026-07-15".parse().unwrap(), "2026-08-11".parse().unwrap()));
        let rendered = e.render();
        // Four weeks announced, four days run — both must be visible.
        assert!(
            rendered.contains("announced  2026-07-15 .. 2026-08-11"),
            "{rendered}"
        );
        assert!(
            rendered.contains("2026-07-15 .. 2026-07-18 (4 days)"),
            "{rendered}"
        );
        assert!(rendered.contains("release    f45"), "{rendered}");
        // A rebuild is its own cause, and congestion's is the load in the
        // figures above; saying "not recorded" of either invents a gap.
        assert!(!rendered.contains("not recorded"), "{rendered}");
        let congestion = event(Kind::Congestion, Some("s390x"), "2026-06-13", 1).render();
        assert!(!congestion.contains("not recorded"), "{congestion}");
    }

    #[test]
    fn the_release_is_the_cycle_in_progress() {
        let sched = vec![
            schedule::Event {
                release: 44,
                kind: schedule::Kind::MassRebuild,
                name: "Mass Rebuild: RPMs".into(),
                start: "2026-01-14".parse().unwrap(),
                end: "2026-02-03".parse().unwrap(),
            },
            schedule::Event {
                release: 45,
                kind: schedule::Kind::MassRebuild,
                name: "Mass Rebuild: RPMs".into(),
                start: "2026-07-15".parse().unwrap(),
                end: "2026-08-11".parse().unwrap(),
            },
        ];
        // A stall in May falls in f45's cycle, which began in January's
        // rebuild... no: the most recent rebuild to have started by May is
        // f44's, so May belongs to the cycle f44's rebuild opened.
        assert_eq!(
            release_of(&sched, midnight("2026-05-06".parse().unwrap())),
            Some(44)
        );
        assert_eq!(
            release_of(&sched, midnight("2026-07-20".parse().unwrap())),
            Some(45)
        );
        // Before any rebuild in the schedule, there is nothing to say.
        assert_eq!(
            release_of(&sched, midnight("2025-06-01".parse().unwrap())),
            None
        );
    }

    #[test]
    fn an_unmatched_note_is_returned_for_warning() {
        let events = vec![event(Kind::Outage, Some("s390x"), "2026-05-06", 3)];
        let notes = vec![
            note("s390x", "2026-05-06", "2026-05-08", "storage"),
            note("ppc64le", "2025-11-06", "2025-11-11", "datacentre-move"),
        ];
        // A range covering both notes: the ppc64le one describes a window
        // that was looked for and not found, which is worth saying.
        let wide = (day_start(d("2025-01-01")), day_start(d("2027-01-01")));
        let left = unmatched(&events, "fedora", &notes, wide);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].cause, "datacentre-move");
    }

    #[test]
    fn an_unexplained_outage_gets_a_stanza_it_can_be_annotated_with() {
        let root = tempfile::tempdir().unwrap();
        let mut outage = event(Kind::Outage, Some("s390x"), "2026-05-06", 3);
        outage.facts = vec![("queue".to_string(), "341 tasks waiting".to_string())];
        let explained = {
            let mut e = event(Kind::Outage, Some("ppc64le"), "2025-11-11", 1);
            e.causes = vec![note(
                "ppc64le",
                "2025-11-06",
                "2025-11-11",
                "datacentre-move",
            )];
            e
        };
        // Congestion is not unexplained -- its cause is the load beside it.
        let congestion = event(Kind::Congestion, Some("s390x"), "2026-07-15", 4);
        let events = vec![outage, explained, congestion];

        let path = write_unexplained(root.path(), "fedora", &events)
            .unwrap()
            .expect("a stub for the one unexplained outage");
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.matches("[[outage]]").count(), 1, "{body}");
        assert!(body.contains(r#"arch = "s390x""#), "{body}");
        assert!(body.contains(r#"from = "2026-05-06""#), "{body}");
        // `to` is inclusive in a note and exclusive in a window.
        assert!(body.contains(r#"to = "2026-05-08""#), "{body}");
        // The facts travel, so nobody retypes what was measured.
        assert!(body.contains("341 tasks waiting"), "{body}");
        // Both routes, because one of them names a path only a checkout has.
        assert!(body.contains("koji-lag annotate --events"), "{body}");
        assert!(body.contains("data/outages.toml"), "{body}");

        // Explained everywhere: the file goes, rather than lingering to be
        // believed by the next reader.
        let done = vec![event(Kind::Congestion, Some("s390x"), "2026-07-15", 4)];
        assert!(
            write_unexplained(root.path(), "fedora", &done)
                .unwrap()
                .is_none()
        );
        assert!(!path.exists());
    }

    #[test]
    fn annotations_are_applied_to_a_tree_without_reopening_the_store() {
        let root = tempfile::tempdir().unwrap();
        let outage = event(Kind::Outage, Some("s390x"), "2026-05-06", 3);
        write(root.path(), &outage).unwrap();
        assert!(
            std::fs::read_to_string(dir(root.path(), &outage).join("event.txt"))
                .unwrap()
                .contains("not recorded")
        );

        let notes = vec![note("s390x", "2026-05-06", "2026-05-08", "storage")];
        let events = reannotate(root.path(), "fedora", &notes).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].causes.len(), 1);
        for e in &events {
            write(root.path(), e).unwrap();
        }
        let rendered =
            std::fs::read_to_string(dir(root.path(), &outage).join("event.txt")).unwrap();
        assert!(rendered.contains("cause      storage"), "{rendered}");
        assert!(!rendered.contains("not recorded"), "{rendered}");

        // A note for another instance is not this instance's business.
        let elsewhere = reannotate(root.path(), "cbs", &notes).unwrap();
        assert!(elsewhere[0].causes.is_empty());
    }

    #[test]
    fn a_note_outside_the_scanned_range_is_not_called_unmatched() {
        // Asking for May 2026 and being told a November 2025 note matched
        // nothing is noise: November was never looked at.
        let events = vec![event(Kind::Outage, Some("s390x"), "2026-05-06", 3)];
        let notes = vec![
            note("s390x", "2026-05-06", "2026-05-08", "storage"),
            note("ppc64le", "2025-11-06", "2025-11-11", "datacentre-move"),
        ];
        let may = (day_start(d("2026-05-01")), day_start(d("2026-06-01")));
        assert!(unmatched(&events, "fedora", &notes, may).is_empty());
    }

    fn d(s: &str) -> NaiveDate {
        s.parse().expect("date")
    }
}
