// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared keep/explain/remove resolution for reviewer-curated findings.
//!
//! Both `ebranch check-update` and `fedora-review-digest` present a list
//! of machine-generated findings and let the reviewer, per finding,
//! **keep** it (real, stays and still counts), **explain** it (real but
//! acceptable, with a written justification kept on record), or **remove**
//! it (a false positive, dropped). This crate is the shared interactive
//! primitive behind that flow.
//!
//! The caller is responsible for only invoking [`resolve_interactive`]
//! when interactive (a TTY and not `--yes`); a non-interactive run should
//! simply treat every finding as [`Resolution::Keep`].

use std::io::Write;

/// How a reviewer resolved one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Real / unresolved — the default. Still counts (blocks / downvotes).
    Keep,
    /// Accepted with a written justification — kept on record, but no
    /// longer counts against the item.
    Explained(String),
    /// A false positive — dropped entirely.
    Removed,
}

/// The keep/explain/remove choice, before any explanation text is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    Keep,
    Explain,
    Remove,
}

/// Walk `items`, prompting the reviewer to keep / explain / remove each
/// one (Enter keeps). `summary` renders the finding line shown at the
/// prompt. Returns each item paired with its [`Resolution`], in input
/// order — including [`Resolution::Removed`], so callers can act on
/// removals (e.g. strip them from a report) rather than only seeing the
/// survivors.
///
/// Prompts on stderr and reads stdin. Only call this when interactive.
pub fn resolve_interactive<T>(
    items: Vec<T>,
    summary: impl Fn(&T) -> String,
) -> Result<Vec<(T, Resolution)>, String> {
    let noted = resolve_with(
        items,
        summary,
        None,
        false,
        read_line,
        &mut std::io::stderr(),
    )?;
    Ok(noted.into_iter().map(|(t, r, _)| (t, r)).collect())
}

/// Like [`resolve_interactive`], for flows where the resolution is a
/// verdict being recorded rather than a finding being dismissed. Two
/// additions: `default_explanation`, when given, is what pressing
/// Enter at the explanation prompt records (an explicit explanation
/// still wins) — for flows where the explanation is usually the same
/// thing, a destination file say, and typing it per finding would be
/// tedious and error-prone. And `k <note>` keeps with a note, handed
/// back as the third tuple element, for when this one verdict
/// deserves its own words on record.
pub fn resolve_interactive_noted<T>(
    items: Vec<T>,
    summary: impl Fn(&T) -> String,
    default_explanation: Option<&str>,
) -> Result<Vec<(T, Resolution, Option<String>)>, String> {
    resolve_with(
        items,
        summary,
        default_explanation,
        true,
        read_line,
        &mut std::io::stderr(),
    )
}

/// Parse one answer line into a choice. Empty (Enter) defaults to Keep —
/// the safe option that never silently drops or accepts a finding.
/// `e <explanation>` explains in one line instead of being asked for
/// the explanation next; with `notes`, `k <note>` keeps with the note.
/// Returns None for unrecognized input (the caller should re-ask).
fn parse_choice(line: &str, notes: bool) -> Option<(Choice, Option<String>)> {
    let line = line.trim();
    let (head, rest) = match line.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (line, ""),
    };
    if !rest.is_empty() {
        return match head.to_ascii_lowercase().as_str() {
            "e" | "explain" => Some((Choice::Explain, Some(rest.to_string()))),
            "k" | "keep" if notes => Some((Choice::Keep, Some(rest.to_string()))),
            _ => None,
        };
    }
    let choice = match head.to_ascii_lowercase().as_str() {
        "" | "k" | "keep" => Choice::Keep,
        "e" | "explain" => Choice::Explain,
        "r" | "remove" => Choice::Remove,
        _ => return None,
    };
    Some((choice, None))
}

/// Core resolution loop, with the line reader and prompt sink injected so
/// it can be driven in tests without a real terminal.
fn resolve_with<T>(
    items: Vec<T>,
    summary: impl Fn(&T) -> String,
    default_explanation: Option<&str>,
    notes: bool,
    mut read: impl FnMut() -> Result<String, String>,
    mut err: impl Write,
) -> Result<Vec<(T, Resolution, Option<String>)>, String> {
    let total = items.len();
    let mut out = Vec::with_capacity(total);
    for (i, item) in items.into_iter().enumerate() {
        let (resolution, note) = prompt_one(
            i + 1,
            total,
            &summary(&item),
            default_explanation,
            notes,
            &mut read,
            &mut err,
        )?;
        out.push((item, resolution, note));
    }
    Ok(out)
}

/// Prompt for one finding's disposition (Enter keeps it — the safe default
/// that never silently drops or accepts).
fn prompt_one(
    idx: usize,
    total: usize,
    summary: &str,
    default_explanation: Option<&str>,
    notes: bool,
    read: &mut impl FnMut() -> Result<String, String>,
    err: &mut impl Write,
) -> Result<(Resolution, Option<String>), String> {
    let keep = if notes { "(k)eep [k <note>]" } else { "(k)eep" };
    loop {
        let _ = writeln!(err, "[{idx}/{total}] {summary}");
        let _ = write!(err, "  {keep} / (e)xplain [e <why>] / (r)emove [k]: ");
        let _ = err.flush();
        match parse_choice(&read()?, notes) {
            Some((Choice::Keep, note)) => return Ok((Resolution::Keep, note)),
            Some((Choice::Remove, _)) => return Ok((Resolution::Removed, None)),
            Some((Choice::Explain, Some(why))) => return Ok((Resolution::Explained(why), None)),
            Some((Choice::Explain, None)) => {
                match default_explanation {
                    Some(default) => {
                        let _ = write!(err, "    explanation [{default}]: ");
                    }
                    None => {
                        let _ = write!(err, "    explanation: ");
                    }
                }
                let _ = err.flush();
                let why = read()?.trim().to_string();
                if why.is_empty() {
                    match default_explanation {
                        Some(default) => {
                            return Ok((Resolution::Explained(default.to_string()), None));
                        }
                        None => {
                            let _ = writeln!(err, "  an explanation is required (or pick k/r)");
                            continue;
                        }
                    }
                }
                return Ok((Resolution::Explained(why), None));
            }
            None => {
                let _ = writeln!(err, "  enter k, e [<why>], or r");
            }
        }
    }
}

/// Read one line from stdin.
fn read_line() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| format!("reading input: {e}"))?;
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn reader(lines: &[&str]) -> impl FnMut() -> Result<String, String> {
        let mut q: VecDeque<String> = lines.iter().map(|l| format!("{l}\n")).collect();
        move || Ok(q.pop_front().unwrap_or_default())
    }

    #[test]
    fn parse_choice_defaults_to_keep() {
        for line in ["", "   ", "k", "Keep"] {
            assert_eq!(parse_choice(line, false), Some((Choice::Keep, None)));
        }
    }

    #[test]
    fn parse_choice_explain_and_remove() {
        assert_eq!(parse_choice("e", false), Some((Choice::Explain, None)));
        // The explanation may ride on the same line.
        assert_eq!(
            parse_choice("e keep-rust.toml", false),
            Some((Choice::Explain, Some("keep-rust.toml".to_string())))
        );
        assert_eq!(
            parse_choice("explain because reasons", true),
            Some((Choice::Explain, Some("because reasons".to_string())))
        );
        assert_eq!(
            parse_choice("EXPLAIN", false),
            Some((Choice::Explain, None))
        );
        assert_eq!(parse_choice("r", false), Some((Choice::Remove, None)));
        assert_eq!(parse_choice("remove", false), Some((Choice::Remove, None)));
    }

    #[test]
    fn parse_choice_unrecognized() {
        assert_eq!(parse_choice("x", false), None);
        assert_eq!(parse_choice("yes", false), None);
    }

    #[test]
    fn a_note_rides_on_keep_only_where_notes_are_on() {
        assert_eq!(
            parse_choice("k not used anymore", true),
            Some((Choice::Keep, Some("not used anymore".to_string())))
        );
        assert_eq!(
            parse_choice("Keep  gone upstream ", true),
            Some((Choice::Keep, Some("gone upstream".to_string())))
        );
        // Without notes, trailing text is junk (re-ask), never a
        // silently dropped annotation.
        assert_eq!(parse_choice("k not used anymore", false), None);
        // And a note cannot ride on e/r even with notes on.
        assert_eq!(parse_choice("r stale", true), None);
    }

    #[test]
    fn resolve_keeps_explains_removes_in_order() {
        let items = vec!["a", "b", "c", "d"];
        // a: Enter→keep, b: explain "because", c: remove, d: "k"→keep.
        let read = reader(&["", "e", "because", "r", "k"]);
        let mut sink = Vec::new();
        let out = resolve_with(items, |s| s.to_string(), None, false, read, &mut sink).unwrap();
        assert_eq!(out[0], ("a", Resolution::Keep, None));
        assert_eq!(
            out[1],
            ("b", Resolution::Explained("because".to_string()), None)
        );
        assert_eq!(out[2], ("c", Resolution::Removed, None));
        assert_eq!(out[3], ("d", Resolution::Keep, None));
    }

    #[test]
    fn notes_come_back_beside_the_resolution() {
        let items = vec!["a", "b"];
        let read = reader(&["k superseded by b", ""]);
        let mut sink = Vec::new();
        let out = resolve_with(items, |s| s.to_string(), None, true, read, &mut sink).unwrap();
        assert_eq!(
            out[0],
            ("a", Resolution::Keep, Some("superseded by b".to_string()))
        );
        assert_eq!(out[1], ("b", Resolution::Keep, None));
        let shown = String::from_utf8(sink).unwrap();
        assert!(shown.contains("(k)eep [k <note>] /"));
    }

    #[test]
    fn resolve_reasks_on_junk_and_blank_explanation() {
        let items = vec!["only"];
        // junk → reask; explain then blank → reask the choice; explain
        // again with a real reason → Explained.
        let read = reader(&["huh", "e", "", "e", "real reason"]);
        let mut sink = Vec::new();
        let out = resolve_with(items, |s| s.to_string(), None, false, read, &mut sink).unwrap();
        assert_eq!(out[0].1, Resolution::Explained("real reason".to_string()));
    }

    #[test]
    fn blank_explanation_takes_the_default_when_one_is_set() {
        let items = vec!["a", "b"];
        // a: explain, Enter → the default. b: explain, explicit text
        // → the explicit text wins.
        let read = reader(&["e", "", "e", "elsewhere.toml"]);
        let mut sink = Vec::new();
        let out = resolve_with(
            items,
            |s| s.to_string(),
            Some("default.toml"),
            true,
            read,
            &mut sink,
        )
        .unwrap();
        assert_eq!(out[0].1, Resolution::Explained("default.toml".to_string()));
        assert_eq!(
            out[1].1,
            Resolution::Explained("elsewhere.toml".to_string())
        );
        let shown = String::from_utf8(sink).unwrap();
        assert!(shown.contains("explanation [default.toml]: "));
    }
}
