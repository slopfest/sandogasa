// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What was actually happening, for the windows the store can only measure.
//!
//! The store says an architecture stopped being served and can say whether
//! its builders were busy while it did ([`crate::stall`]), which is as far
//! as measurement reaches. It cannot say that the cause was a storage
//! failure, or that half a fleet was disabled for a datacentre move. That
//! knowledge exists in infrastructure tickets and in people's memories, and
//! without somewhere to put it every report re-derives the same anomalies
//! and leaves the reader to ask again what they were.
//!
//! Annotations are read from a file rather than kept in the store, because
//! the store is derived: it gets rebuilt, re-synced and merged with other
//! people's, and a curated note would not survive any of that. They only
//! accumulate — the cause of a day in May 2026 will not change — so unlike
//! the release schedule they are worth committing rather than pointed at.
//!
//! **Matching is by overlap, never by equal dates.** A detected window's
//! edges move: a threshold changes, a day of data arrives, a partial day
//! becomes whole. An annotation pinned to `2026-05-06..08` would silently
//! stop matching the day the detector says `05-06..09`, and silence is the
//! failure mode to design against here. For the same reason an annotation
//! that matches nothing is *reported* rather than dropped: it means either
//! the detector missed an event or the note is wrong about its own dates,
//! and both are worth hearing about.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// A curated note about one measured window.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Note {
    /// Instance key the window belongs to, e.g. `fedora`.
    pub instance: String,
    /// Architecture affected, or absent for something fleet-wide.
    #[serde(default)]
    pub arch: Option<String>,
    /// First affected day, as `"YYYY-MM-DD"`.
    ///
    /// Quoted rather than a bare TOML date: TOML's own date type
    /// deserializes as a table, which `NaiveDate` will not accept.
    pub from: NaiveDate,
    /// Last affected day, inclusive — as a person would write it.
    pub to: NaiveDate,
    /// Short cause, for the report line. Free text, because reality is not
    /// enumerable, but prefer a term already used in the file: `storage`,
    /// `network`, `datacentre-move`, `capacity-reduction`, `mass-rebuild`.
    pub cause: String,
    /// Where this is written down — an infrastructure ticket, a list post.
    #[serde(default)]
    pub ticket: Option<String>,
    /// Anything a reader of the report would want that the cause omits.
    #[serde(default)]
    pub note: Option<String>,
}

impl Note {
    /// Whether this note is about `[from, to)` on `arch` of `instance`.
    ///
    /// The note's own `to` is inclusive, so it covers the whole of that
    /// day; a window starting the morning after still does not overlap.
    pub fn covers(&self, instance: &str, arch: Option<&str>, from: f64, to: f64) -> bool {
        if self.instance != instance {
            return false;
        }
        // A note without an arch is about everything; a window without one
        // is matched by any note for the instance.
        if let (Some(mine), Some(theirs)) = (self.arch.as_deref(), arch)
            && mine != theirs
        {
            return false;
        }
        let (start, end) = self.span();
        start < to && from < end
    }

    /// The note's span as `[from, to)` unix seconds, its `to` day included.
    fn span(&self) -> (f64, f64) {
        let day = |d: NaiveDate| {
            d.and_hms_opt(0, 0, 0)
                .map(|t| t.and_utc().timestamp() as f64)
                .unwrap_or(0.0)
        };
        (day(self.from), day(self.to) + 86_400.0)
    }
}

/// A ready-to-fill annotation for a window nobody has explained.
///
/// Everything the store can know is filled in; what is left blank is what
/// only a person knows. `note` carries the measured facts forward so that
/// whoever writes the cause is not retyping figures the tool already
/// computed — the same reason the rest of this crate does not make a reader
/// recompute anything.
pub fn stub(
    instance: &str,
    arch: Option<&str>,
    from: NaiveDate,
    to: NaiveDate,
    facts: &[(String, String)],
) -> String {
    let mut out = String::from("[[outage]]\n");
    out.push_str(&format!("instance = \"{instance}\"\n"));
    match arch {
        Some(a) => out.push_str(&format!("arch = \"{a}\"\n")),
        // Present but commented, so the shape is obvious to anyone editing
        // a fleet-wide note by hand.
        None => out.push_str("# arch = \"s390x\"   # omit for something fleet-wide\n"),
    }
    out.push_str(&format!("from = \"{from}\"\n"));
    out.push_str(&format!("to = \"{to}\"\n"));
    out.push_str("cause = \"\"   # storage, network, datacentre-move, capacity-reduction\n");
    out.push_str(
        "ticket = \"\"  # e.g. https://forge.fedoraproject.org/infra/tickets/issues/13326\n",
    );
    out.push_str("note = \"\"\"\n");
    out.push_str("What was happening. Measured, for whoever writes the rest:\n");
    for (label, value) in facts {
        out.push_str(&format!("  {label}: {value}\n"));
    }
    out.push_str("\"\"\"\n");
    out
}

/// The file [`stub`]s are written into, with the instructions that make it
/// usable from anywhere.
///
/// Two routes on purpose. `data/outages.toml` is a path inside a checkout
/// and means nothing to somebody running an installed binary, which is what
/// the old "see data/outages.toml" advice assumed of everyone.
pub fn stub_file(stubs: &[String]) -> String {
    let mut out = String::from(
        "# Windows koji-lag detected and nobody has explained.\n\
         #\n\
         # Fill in `cause` and `ticket`, then either:\n\
         #\n\
         #   - use it here and now, from any directory:\n\
         #       koji-lag annotate --events <this tree> --annotations <this file>\n\
         #     which rewrites the events without touching the store, or\n\
         #\n\
         #   - contribute it, so everyone gets it: paste the stanza into\n\
         #     tools/koji-lag/data/outages.toml in a sandogasa checkout.\n\
         #\n\
         # Dates are quoted strings; TOML's own date type deserializes as a\n\
         # table, which will not load. `to` is inclusive.\n\
         #\n\
         # Notes match windows by *overlap*, never by equal dates, so write\n\
         # the dates of the event as you understand it -- the window's edges\n\
         # move when a threshold changes or a day of data arrives.\n\n",
    );
    out.push_str(&stubs.join("\n"));
    out
}

/// The annotations shipped with the tool.
///
/// Compiled in rather than read from `data/`, because that directory is a
/// source tree and an installed binary has no access to it. An operator
/// with more to add passes a file of their own, which is merged with
/// these.
pub fn builtin() -> Result<Vec<Note>, String> {
    parse(include_str!("../data/outages.toml"))
}

/// Every note in a file, and it is an error for one to be malformed rather
/// than a reason to skip it — a dropped annotation is invisible.
pub fn read(path: &std::path::Path) -> Result<Vec<Note>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Parse the annotation file's contents.
pub fn parse(text: &str) -> Result<Vec<Note>, String> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default, rename = "outage")]
        outages: Vec<Note>,
    }
    let file: File = toml::from_str(text).map_err(|e| e.to_string())?;
    for note in &file.outages {
        if note.to < note.from {
            return Err(format!(
                "note for {} {} ends ({}) before it starts ({})",
                note.instance,
                note.arch.as_deref().unwrap_or("all arches"),
                note.to,
                note.from
            ));
        }
    }
    Ok(file.outages)
}

/// Which notes apply to each window, and which notes applied to nothing.
///
/// `windows` is `(arch, from, to)` per measured window. The unmatched list
/// is the point of returning a struct rather than just the pairings: it is
/// how a stale or mistaken annotation gets noticed.
#[derive(Debug, Default, PartialEq)]
pub struct Matched<'a> {
    /// Per input window, in the order given, the notes covering it.
    pub per_window: Vec<Vec<&'a Note>>,
    /// Notes that covered no window at all.
    pub unmatched: Vec<&'a Note>,
}

pub fn match_windows<'a>(
    notes: &'a [Note],
    instance: &str,
    windows: &[(Option<String>, f64, f64)],
) -> Matched<'a> {
    let mut used = vec![false; notes.len()];
    let per_window = windows
        .iter()
        .map(|(arch, from, to)| {
            notes
                .iter()
                .enumerate()
                .filter(|(i, note)| {
                    let hit = note.covers(instance, arch.as_deref(), *from, *to);
                    if hit {
                        used[*i] = true;
                    }
                    hit
                })
                .map(|(_, note)| note)
                .collect()
        })
        .collect();
    let unmatched = notes
        .iter()
        .zip(&used)
        .filter(|(note, used)| !**used && note.instance == instance)
        .map(|(note, _)| note)
        .collect();
    Matched {
        per_window,
        unmatched,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = r#"
[[outage]]
instance = "fedora"
arch = "s390x"
from = "2026-05-06"
to = "2026-05-08"
cause = "storage"
ticket = "https://forge.fedoraproject.org/infra/tickets/issues/13326"
note = "Builders stayed enabled at full capacity and served nothing."

[[outage]]
instance = "fedora"
arch = "ppc64le"
from = "2025-11-06"
to = "2025-11-11"
cause = "datacentre-move"
"#;

    fn day(y: i32, m: u32, d: u32) -> f64 {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp() as f64
    }

    #[test]
    fn a_note_is_read_whole() {
        let notes = parse(FILE).unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].cause, "storage");
        assert_eq!(notes[0].arch.as_deref(), Some("s390x"));
        assert!(notes[0].ticket.as_deref().unwrap().ends_with("13326"));
        // Optional fields stay optional.
        assert_eq!(notes[1].ticket, None);
        assert_eq!(notes[1].note, None);
    }

    #[test]
    fn a_note_matches_a_window_whose_edges_moved() {
        // The reason matching is by overlap: the detector said 05-06..08
        // when this note was written and may say 05-06..09 tomorrow, or
        // 05-07..08 under a different threshold. All three must match.
        let notes = parse(FILE).unwrap();
        let n = &notes[0];
        for (from, to) in [
            (day(2026, 5, 6), day(2026, 5, 9)),
            (day(2026, 5, 6), day(2026, 5, 10)),
            (day(2026, 5, 7), day(2026, 5, 9)),
            // Touching only the last annotated day still counts.
            (day(2026, 5, 8), day(2026, 5, 12)),
        ] {
            assert!(n.covers("fedora", Some("s390x"), from, to), "{from} {to}");
        }
        // The day after the note ends does not.
        assert!(!n.covers("fedora", Some("s390x"), day(2026, 5, 9), day(2026, 5, 12)));
        // Nor does the right dates on the wrong architecture or instance.
        assert!(!n.covers("fedora", Some("x86_64"), day(2026, 5, 6), day(2026, 5, 9)));
        assert!(!n.covers("stream", Some("s390x"), day(2026, 5, 6), day(2026, 5, 9)));
    }

    #[test]
    fn a_note_with_no_arch_covers_the_whole_fleet() {
        let notes = parse(
            r#"
[[outage]]
instance = "fedora"
from = "2026-03-01"
to = "2026-03-01"
cause = "network"
"#,
        )
        .unwrap();
        for arch in ["s390x", "x86_64"] {
            assert!(notes[0].covers("fedora", Some(arch), day(2026, 3, 1), day(2026, 3, 2)));
        }
    }

    #[test]
    fn a_note_matching_nothing_is_reported() {
        // The case this exists for: an annotation whose dates are wrong, or
        // one whose event the detector has stopped finding. Silence here
        // would mean a report that looks annotated and is not.
        let notes = parse(FILE).unwrap();
        let windows = vec![(Some("s390x".to_string()), day(2026, 5, 6), day(2026, 5, 9))];
        let matched = match_windows(&notes, "fedora", &windows);
        assert_eq!(matched.per_window[0].len(), 1);
        assert_eq!(matched.per_window[0][0].cause, "storage");
        assert_eq!(matched.unmatched.len(), 1, "{:?}", matched.unmatched);
        assert_eq!(matched.unmatched[0].cause, "datacentre-move");
    }

    #[test]
    fn notes_for_another_instance_are_not_reported_as_unmatched() {
        // Reporting a CentOS note as unmatched while looking at a Fedora
        // store would make the warning useless.
        let notes = parse(
            r#"
[[outage]]
instance = "stream"
arch = "s390x"
from = "2026-05-06"
to = "2026-05-08"
cause = "storage"
"#,
        )
        .unwrap();
        let matched = match_windows(&notes, "fedora", &[]);
        assert!(matched.unmatched.is_empty());
    }

    #[test]
    fn one_window_can_carry_more_than_one_note() {
        // A rebuild window that also had an outage in it, which is exactly
        // the case a single `cause` field could not express.
        let notes = parse(
            r#"
[[outage]]
instance = "fedora"
arch = "s390x"
from = "2026-07-15"
to = "2026-07-18"
cause = "mass-rebuild"

[[outage]]
instance = "fedora"
arch = "s390x"
from = "2026-07-17"
to = "2026-07-17"
cause = "network"
"#,
        )
        .unwrap();
        let windows = vec![(
            Some("s390x".to_string()),
            day(2026, 7, 15),
            day(2026, 7, 19),
        )];
        let matched = match_windows(&notes, "fedora", &windows);
        assert_eq!(matched.per_window[0].len(), 2);
        assert!(matched.unmatched.is_empty());
    }

    #[test]
    fn the_committed_file_parses_and_matches_what_it_describes() {
        // A typo in the shipped annotations would otherwise surface only
        // when somebody ran a report against a real store.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("data/outages.toml");
        let notes = read(&path).expect("data/outages.toml parses");
        assert!(!notes.is_empty());
        for note in &notes {
            assert!(!note.cause.is_empty(), "{note:?} has no cause");
            assert!(note.to >= note.from);
        }
        // And each one matches the window the detector actually reports for
        // it, so the file cannot drift out of contact with the data.
        let windows = vec![
            (Some("s390x".to_string()), day(2026, 5, 6), day(2026, 5, 9)),
            (
                Some("ppc64le".to_string()),
                day(2025, 11, 11),
                day(2025, 11, 12),
            ),
        ];
        let matched = match_windows(&notes, "fedora", &windows);
        for (i, hits) in matched.per_window.iter().enumerate() {
            assert_eq!(hits.len(), 1, "window {i} matched {hits:?}");
        }
        assert!(
            matched.unmatched.is_empty(),
            "annotations matching no detected window: {:?}",
            matched
                .unmatched
                .iter()
                .map(|n| &n.cause)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_backwards_note_is_an_error_not_a_note_that_matches_nothing() {
        let err = parse(
            r#"
[[outage]]
instance = "fedora"
from = "2026-05-08"
to = "2026-05-06"
cause = "storage"
"#,
        )
        .unwrap_err();
        assert!(err.contains("ends"), "{err}");
    }
}
