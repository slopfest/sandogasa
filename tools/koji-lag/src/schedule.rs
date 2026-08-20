// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fedora's release schedule, as published in the fedora-pgm-schedule repo.
//!
//! The schedule says when a mass rebuild or a freeze was *meant* to happen,
//! which is worth reporting beside when it actually did. It is read from a
//! path rather than vendored: it changes after the fact — its git log
//! carries commits like "updating f48 schedule with correct dates" — so a
//! copy in this repo would be a copy of one moment's opinion.
//!
//! The files are MS Project XML, one directory per release
//! (`f-45/Fedora.Schedule.xml`), and they are not uniform across the 38
//! releases published: names drift, milestones come and go, and the older
//! ones have no mass rebuild at all. What follows is written to that.

use std::path::Path;

use chrono::NaiveDate;

/// A window the schedule names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Fedora release the schedule belongs to.
    pub release: u32,
    pub kind: Kind,
    /// The task's own name, kept because it varies and is worth quoting.
    pub name: String,
    pub start: NaiveDate,
    /// Inclusive last day.
    pub end: NaiveDate,
}

/// The kinds of window worth reporting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    MassRebuild,
    BetaFreeze,
    FinalFreeze,
}

impl Kind {
    /// The path segment a report for this kind is filed under.
    pub fn slug(self) -> &'static str {
        match self {
            Kind::MassRebuild => "mass-rebuild",
            Kind::BetaFreeze => "beta-freeze",
            Kind::FinalFreeze => "final-freeze",
        }
    }
}

/// Every event the schedules under `dir` name, oldest release first.
///
/// `dir` is a checkout of the schedule repo: one `f-NN/Fedora.Schedule.xml`
/// per release. Releases whose schedule names no such window — everything
/// before F25 — simply contribute nothing.
pub fn events(dir: &Path) -> Result<Vec<Event>, String> {
    let mut found = Vec::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut releases: Vec<(u32, std::path::PathBuf)> = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        let Some(release) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("f-"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let file = path.join("Fedora.Schedule.xml");
        if file.is_file() {
            releases.push((release, file));
        }
    }
    releases.sort();
    for (release, file) in releases {
        let xml = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        found.extend(events_in(&xml, release)?);
    }
    Ok(found)
}

/// The events one schedule names.
pub fn events_in(xml: &str, release: u32) -> Result<Vec<Event>, String> {
    let mut out: Vec<Event> = Vec::new();
    for (name, start, end) in tasks(xml)? {
        let Some(kind) = classify(&name) else {
            continue;
        };
        // Keep the longest span for each kind. The schedules carry both a
        // range task ("Mass Rebuild: RPMs", 15 January to 4 February) and
        // zero-length milestones inside it ("Mass Rebuild starts"), and the
        // range is the one to trust: where the two disagree — F45 says
        // 07-15 for the range and 07-22 for the milestone — the range
        // matches when releng actually started submitting.
        match out.iter_mut().find(|e| e.kind == kind) {
            Some(existing) if end - start > existing.end - existing.start => {
                *existing = Event {
                    release,
                    kind,
                    name,
                    start,
                    end,
                };
            }
            Some(_) => {}
            None => out.push(Event {
                release,
                kind,
                name,
                start,
                end,
            }),
        }
    }
    out.sort_by_key(|e| (e.kind, e.start));
    Ok(out)
}

/// Which window a task name refers to, if any.
///
/// Names drift across releases — "Mass Rebuild", "Mass Rebuild: RPMs",
/// "Mass Rebuild: RPMs first, then …", "Mass rebuild (if needed)" — so this
/// matches loosely and then excludes the tasks that merely *mention* a
/// window: change checkpoints, reminders, and the infrastructure freezes,
/// which are a different thing entirely from a package freeze.
fn classify(name: &str) -> Option<Kind> {
    let lower = name.to_lowercase();
    if lower.contains("checkpoint")
        || lower.contains("reminder")
        || lower.contains("infrastructure")
        || lower.contains("string")
        || lower.contains("screenshot")
    {
        return None;
    }
    if lower.contains("mass rebuild") {
        return Some(Kind::MassRebuild);
    }
    if lower.contains("beta freeze") {
        return Some(Kind::BetaFreeze);
    }
    if lower.contains("final freeze") {
        return Some(Kind::FinalFreeze);
    }
    None
}

/// Every `<Task>` with a usable name and dates, as `(name, start, end)`.
fn tasks(xml: &str) -> Result<Vec<(String, NaiveDate, NaiveDate)>, String> {
    use quick_xml::events::Event as Xml;
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = Vec::new();
    let (mut in_task, mut field, mut name, mut start, mut end) =
        (false, String::new(), String::new(), None, None);
    loop {
        match reader.read_event() {
            Ok(Xml::Start(e)) => {
                // The element names are namespaced in the file itself, so
                // compare on the local name only.
                let local = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                if local == "Task" {
                    in_task = true;
                    (name, start, end) = (String::new(), None, None);
                }
                field = local;
            }
            Ok(Xml::Text(t)) if in_task => {
                // quick-xml 0.40 made decoding and entity-unescaping
                // explicit for text nodes.
                let decoded = t.decode().map_err(|e| e.to_string())?;
                let text = quick_xml::escape::unescape(&decoded)
                    .map_err(|e| e.to_string())?
                    .into_owned();
                match field.as_str() {
                    "Name" => name = text,
                    // "2026-07-15T08:00:00" — the time of day is the
                    // schedule's working-hours convention, not a fact.
                    "Start" => start = parse_date(&text),
                    "Finish" => end = parse_date(&text),
                    _ => {}
                }
            }
            Ok(Xml::End(e)) => {
                if String::from_utf8_lossy(e.local_name().as_ref()) == "Task" {
                    if let (Some(s), Some(f)) = (start, end)
                        && !name.is_empty()
                    {
                        out.push((std::mem::take(&mut name), s, f));
                    }
                    in_task = false;
                }
                field.clear();
            }
            Ok(Xml::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(format!("schedule XML: {e}")),
        }
    }
    Ok(out)
}

fn parse_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.get(..10)?, "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// The shape F41 onwards use: a range task, milestones inside it, and
    /// tasks that merely mention a rebuild.
    const MODERN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Project xmlns="http://schemas.microsoft.com/project">
  <Title>Fedora 45 Schedule</Title>
  <Tasks>
    <Task><Name>Change Checkpoint: Proposal submission deadline (Changes requiring mass rebuild)</Name>
      <Start>2026-06-30T08:00:00</Start><Finish>2026-06-30T17:00:00</Finish></Task>
    <Task><Name>Mass Rebuild: RPMs</Name>
      <Start>2026-07-15T08:00:00</Start><Finish>2026-08-11T17:00:00</Finish></Task>
    <Task><Name>Mass Rebuild starts</Name>
      <Start>2026-07-22T08:00:00</Start><Finish>2026-07-22T08:00:00</Finish></Task>
    <Task><Name>Beta Freeze</Name>
      <Start>2026-08-25T14:00:00</Start><Finish>2026-09-15T17:00:00</Finish></Task>
    <Task><Name>Beta Infrastructure Change Freeze</Name>
      <Start>2026-08-20T08:00:00</Start><Finish>2026-09-20T17:00:00</Finish></Task>
    <Task><Name>Final Freeze</Name>
      <Start>2026-10-06T14:00:00</Start><Finish>2026-10-20T17:00:00</Finish></Task>
    <Task><Name>Website String Freeze</Name>
      <Start>2026-09-01T08:00:00</Start><Finish>2026-09-08T17:00:00</Finish></Task>
  </Tasks>
</Project>"#;

    #[test]
    fn the_range_wins_over_a_milestone_that_disagrees_with_it() {
        let events = events_in(MODERN, 45).unwrap();
        let rebuild = events
            .iter()
            .find(|e| e.kind == Kind::MassRebuild)
            .expect("a mass rebuild");
        // F45's real schedule carries exactly this disagreement: the range
        // starts 07-15 and the milestone claims 07-22. Koji says releng
        // began submitting on the 15th, so the range is the honest source.
        assert_eq!(rebuild.start, day(2026, 7, 15));
        assert_eq!(rebuild.end, day(2026, 8, 11));
        assert_eq!(rebuild.name, "Mass Rebuild: RPMs");
        assert_eq!(rebuild.release, 45);
    }

    #[test]
    fn tasks_that_only_mention_a_window_are_not_windows() {
        let events = events_in(MODERN, 45).unwrap();
        // A change checkpoint about rebuild-requiring proposals is not a
        // rebuild; an infrastructure freeze is not a package freeze; a
        // website string freeze is neither.
        assert_eq!(events.len(), 3, "{events:?}");
        let beta = events.iter().find(|e| e.kind == Kind::BetaFreeze).unwrap();
        assert_eq!(beta.start, day(2026, 8, 25));
        assert_eq!(beta.end, day(2026, 9, 15));
        let fin = events.iter().find(|e| e.kind == Kind::FinalFreeze).unwrap();
        assert_eq!(fin.start, day(2026, 10, 6));
    }

    #[test]
    fn the_older_spelling_still_parses() {
        // F25 wrote it in lower case and hedged about it.
        let xml = r#"<Project xmlns="http://schemas.microsoft.com/project"><Tasks>
            <Task><Name>Mass rebuild (if needed)</Name>
              <Start>2016-07-08T08:00:00</Start><Finish>2016-07-29T17:00:00</Finish></Task>
            </Tasks></Project>"#;
        let events = events_in(xml, 25).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, Kind::MassRebuild);
        assert_eq!(events[0].start, day(2016, 7, 8));
    }

    #[test]
    fn a_schedule_with_no_such_window_yields_nothing() {
        // Everything before F25 names no mass rebuild at all, and that is
        // not an error — those releases predate the practice.
        let xml = r#"<Project xmlns="http://schemas.microsoft.com/project"><Tasks>
            <Task><Name>Feature Freeze</Name>
              <Start>2009-03-24T08:00:00</Start><Finish>2009-03-24T17:00:00</Finish></Task>
            </Tasks></Project>"#;
        assert!(events_in(xml, 11).unwrap().is_empty());
    }

    #[test]
    fn a_task_without_dates_is_skipped_rather_than_failing() {
        let xml = r#"<Project xmlns="http://schemas.microsoft.com/project"><Tasks>
            <Task><Name>Mass Rebuild: RPMs</Name></Task>
            <Task><Name>Mass Rebuild: RPMs</Name>
              <Start>2026-01-14T08:00:00</Start><Finish>2026-02-03T17:00:00</Finish></Task>
            </Tasks></Project>"#;
        let events = events_in(xml, 44).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].start, day(2026, 1, 14));
    }
}
