// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `act` subcommand.
//!
//! The cull workflow's last verb: walk the standing verdicts and give
//! the packages away — orphan what the user owns, drop the user's own
//! ACL where they are admin — one confirmed action at a time, with
//! the books adjusted as each package goes (the entry leaves the cull
//! inventory, the package leaves the personal inventory).
//!
//! Before any orphan-class prompt, the package's reverse dependencies
//! are probed distro-wide through fedrq — every subpackage, so
//! feature capabilities (`crate(x/feature)`) cannot hide the way they
//! can in an inventory-scoped closure — because orphaning starts the
//! retirement clock, and retirement is what strands dependents. A
//! package something still requires defaults to skip; enacting it
//! anyway is a deliberate choice, as is `g <user>`, which hands the
//! package to a named person instead of the orphan pool.

use serde::Serialize;

/// One answer at the act prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Enact the classified action (orphan / remove own ACL).
    Enact,
    /// Give the package to this user instead.
    Give(String),
    /// Leave the verdict standing for a later pass.
    Skip,
    /// Stop the walk.
    Quit,
}

/// Parse one prompt answer. Enter (or `s`) skips — the safe default
/// for actions that change ownership on a server. `g <user>` is only
/// meaningful where giving is (owner-level packages).
pub fn parse_choice(line: &str, allow_give: bool) -> Option<Choice> {
    let line = line.trim();
    let (head, rest) = match line.split_once(char::is_whitespace) {
        Some((head, rest)) => (head, rest.trim()),
        None => (line, ""),
    };
    if !rest.is_empty() {
        return match allow_give && head.eq_ignore_ascii_case("g") {
            true => Some(Choice::Give(rest.to_string())),
            false => None,
        };
    }
    match head.to_ascii_lowercase().as_str() {
        "" | "s" | "skip" => Some(Choice::Skip),
        "y" | "yes" => Some(Choice::Enact),
        "q" | "quit" => Some(Choice::Quit),
        _ => None,
    }
}

/// The branches worth probing for a package, from its dist-git
/// branch list: rawhide (where an orphan's retirement lands) plus
/// every EPEL branch it carries — an EPEL-only package's dependents
/// are invisible from rawhide, and which EPEL that is belongs to the
/// package, not to a flag (python39-* lives on epel8). Minor-versioned
/// branches normalize to their fedrq name (epel10.1 → epel10).
pub fn probe_branches(git_branches: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for b in git_branches {
        let normalized = match b.split_once('.') {
            Some((head, _)) if head.starts_with("epel") => head,
            _ => b.as_str(),
        };
        let epel = normalized
            .strip_prefix("epel")
            .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()));
        if (normalized == "rawhide" || epel) && !out.iter().any(|o| o == normalized) {
            out.push(normalized.to_string());
        }
    }
    out
}

/// What the reverse-dependency probe learned about one package on one
/// branch.
#[derive(Debug, Serialize)]
pub enum Probe {
    /// No binaries on this branch (retired there, or never present).
    Absent,
    /// Source packages that require any of its subpackages' provides.
    Dependents(Vec<String>),
}

/// Keep only the dependents that are someone else's problem: the
/// package's own subpackages requiring each other prove nothing, and
/// fedrq's literal `(none)` rows are noise.
pub fn external_dependents(package: &str, raw: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = raw
        .into_iter()
        .filter(|s| s != "(none)" && s != package)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Probe a package's reverse dependencies on one branch: all of its
/// subpackages (so feature-gated capabilities are covered), resolved
/// to requiring source packages.
pub fn probe_dependents(branch: &str, package: &str) -> Result<Probe, String> {
    let fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(branch.to_string()),
        repo: None,
    };
    let binaries = fedrq
        .subpkgs_names(package)
        .map_err(|e| format!("subpackages of {package} on {branch}: {e}"))?;
    if binaries.is_empty() {
        return Ok(Probe::Absent);
    }
    let raw = fedrq
        .whatrequires(&binaries)
        .map_err(|e| format!("whatrequires for {package} on {branch}: {e}"))?;
    Ok(Probe::Dependents(external_dependents(package, raw)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_is_the_default_and_junk_reasks() {
        for line in ["", "  ", "s", "Skip"] {
            assert_eq!(parse_choice(line, true), Some(Choice::Skip));
        }
        assert_eq!(parse_choice("y", true), Some(Choice::Enact));
        assert_eq!(parse_choice("q", false), Some(Choice::Quit));
        assert_eq!(parse_choice("x", true), None);
        assert_eq!(parse_choice("orphan it", true), None);
    }

    #[test]
    fn give_names_a_user_and_only_where_giving_applies() {
        assert_eq!(
            parse_choice("g decathorpe", true),
            Some(Choice::Give("decathorpe".to_string()))
        );
        assert_eq!(
            parse_choice("G  someone ", true),
            Some(Choice::Give("someone".to_string()))
        );
        // Bare g has no target; admin-level prompts take no g at all.
        assert_eq!(parse_choice("g", true), None);
        assert_eq!(parse_choice("g someone", false), None);
    }

    #[test]
    fn probe_branches_keep_rawhide_and_the_packages_own_epels() {
        let git: Vec<String> = [
            "rawhide", "f45", "f44", "epel8", "epel10.1", "epel10.0", "main",
        ]
        .map(String::from)
        .into();
        assert_eq!(probe_branches(&git), ["rawhide", "epel8", "epel10"]);
        // EPEL-only package: rawhide absent, its own EPEL present.
        let epel_only: Vec<String> = ["epel8".to_string()].into();
        assert_eq!(probe_branches(&epel_only), ["epel8"]);
        // Non-branch noise never probes.
        assert!(probe_branches(&["epel-playground".to_string(), "main".to_string()]).is_empty());
    }

    #[test]
    fn own_subpackages_and_none_rows_are_not_dependents() {
        let raw = vec![
            "(none)".to_string(),
            "rust-buf-min".to_string(),
            "rust-v_htmlescape".to_string(),
            "rust-v_htmlescape".to_string(),
        ];
        assert_eq!(
            external_dependents("rust-buf-min", raw),
            ["rust-v_htmlescape"]
        );
        assert!(external_dependents("x", vec!["(none)".into(), "x".into()]).is_empty());
    }
}
