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
    resolve_with(items, summary, read_line, &mut std::io::stderr())
}

/// Parse one answer line into a choice. Empty (Enter) defaults to Keep —
/// the safe option that never silently drops or accepts a finding.
/// Returns None for unrecognized input (the caller should re-ask).
fn parse_choice(line: &str) -> Option<Choice> {
    match line.trim().to_ascii_lowercase().as_str() {
        "" | "k" | "keep" => Some(Choice::Keep),
        "e" | "explain" => Some(Choice::Explain),
        "r" | "remove" => Some(Choice::Remove),
        _ => None,
    }
}

/// Core resolution loop, with the line reader and prompt sink injected so
/// it can be driven in tests without a real terminal.
fn resolve_with<T>(
    items: Vec<T>,
    summary: impl Fn(&T) -> String,
    mut read: impl FnMut() -> Result<String, String>,
    mut err: impl Write,
) -> Result<Vec<(T, Resolution)>, String> {
    let total = items.len();
    let mut out = Vec::with_capacity(total);
    for (i, item) in items.into_iter().enumerate() {
        let resolution = prompt_one(i + 1, total, &summary(&item), &mut read, &mut err)?;
        out.push((item, resolution));
    }
    Ok(out)
}

/// Prompt for one finding's disposition (Enter keeps it — the safe default
/// that never silently drops or accepts).
fn prompt_one(
    idx: usize,
    total: usize,
    summary: &str,
    read: &mut impl FnMut() -> Result<String, String>,
    err: &mut impl Write,
) -> Result<Resolution, String> {
    loop {
        let _ = writeln!(err, "[{idx}/{total}] {summary}");
        let _ = write!(err, "  (k)eep / (e)xplain / (r)emove [k]: ");
        let _ = err.flush();
        match parse_choice(&read()?) {
            Some(Choice::Keep) => return Ok(Resolution::Keep),
            Some(Choice::Remove) => return Ok(Resolution::Removed),
            Some(Choice::Explain) => {
                let _ = write!(err, "    explanation: ");
                let _ = err.flush();
                let why = read()?.trim().to_string();
                if why.is_empty() {
                    let _ = writeln!(err, "  an explanation is required (or pick k/r)");
                    continue;
                }
                return Ok(Resolution::Explained(why));
            }
            None => {
                let _ = writeln!(err, "  enter k, e, or r");
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
        assert_eq!(parse_choice(""), Some(Choice::Keep));
        assert_eq!(parse_choice("   "), Some(Choice::Keep));
        assert_eq!(parse_choice("k"), Some(Choice::Keep));
        assert_eq!(parse_choice("Keep"), Some(Choice::Keep));
    }

    #[test]
    fn parse_choice_explain_and_remove() {
        assert_eq!(parse_choice("e"), Some(Choice::Explain));
        assert_eq!(parse_choice("EXPLAIN"), Some(Choice::Explain));
        assert_eq!(parse_choice("r"), Some(Choice::Remove));
        assert_eq!(parse_choice("remove"), Some(Choice::Remove));
    }

    #[test]
    fn parse_choice_unrecognized() {
        assert_eq!(parse_choice("x"), None);
        assert_eq!(parse_choice("yes"), None);
    }

    #[test]
    fn resolve_keeps_explains_removes_in_order() {
        let items = vec!["a", "b", "c", "d"];
        // a: Enter→keep, b: explain "because", c: remove, d: "k"→keep.
        let read = reader(&["", "e", "because", "r", "k"]);
        let mut sink = Vec::new();
        let out = resolve_with(items, |s| s.to_string(), read, &mut sink).unwrap();
        assert_eq!(out[0], ("a", Resolution::Keep));
        assert_eq!(out[1], ("b", Resolution::Explained("because".to_string())));
        assert_eq!(out[2], ("c", Resolution::Removed));
        assert_eq!(out[3], ("d", Resolution::Keep));
    }

    #[test]
    fn resolve_reasks_on_junk_and_blank_explanation() {
        let items = vec!["only"];
        // junk → reask; explain then blank → reask the choice; explain
        // again with a real reason → Explained.
        let read = reader(&["huh", "e", "", "e", "real reason"]);
        let mut sink = Vec::new();
        let out = resolve_with(items, |s| s.to_string(), read, &mut sink).unwrap();
        assert_eq!(out[0].1, Resolution::Explained("real reason".to_string()));
    }
}
