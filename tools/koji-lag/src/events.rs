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

use chrono::NaiveDate;
use serde::Serialize;

use crate::{annotate, rebuild, schedule, stall, store::Store};

/// What kind of window this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
            out.push_str("  cause      not recorded — add it to data/outages.toml\n");
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
    for found in stall::stalls(&arch_days, stall::Rule::default()) {
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
) -> Vec<&'a annotate::Note> {
    let windows: Vec<(Option<String>, f64, f64)> = events
        .iter()
        .map(|e| (e.arch.clone(), e.from, e.to))
        .collect();
    annotate::match_windows(notes, instance, &windows).unmatched
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

fn day_name(ts: f64) -> String {
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
        assert!(rendered.contains("data/outages.toml"), "{rendered}");
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
        let left = unmatched(&events, "fedora", &notes);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].cause, "datacentre-move");
    }
}
