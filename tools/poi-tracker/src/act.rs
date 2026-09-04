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
//! feature capabilities cannot hide the way they can in an
//! inventory-scoped closure — because orphaning starts the retirement
//! clock, and retirement is what strands dependents. Each requirer is
//! resolved to its source *and* classified: a dependent that reaches
//! the package only through a `foo+feature` subpackage is flagged
//! severable (dropping that optional feature breaks the edge), while
//! a base-package requirer is a hard block that makes orphan
//! reconfirm. `g <user>` hands the package to a named person instead
//! of the orphan pool.

use serde::Serialize;

/// One answer at the act prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Enact the classified action (orphan / remove own ACL).
    Enact,
    /// Give the package to this user instead.
    Give(String),
    /// Lift the verdict: the package leaves the cull inventory
    /// without any server action, optionally filed into an essential
    /// inventory so it never comes back as a candidate. Without one
    /// it returns to candidacy on the next kondo run.
    Uncull(Option<String>),
    /// Leave the verdict standing for a later pass.
    Skip,
    /// Stop the walk.
    Quit,
}

/// The answers for one package: enacting only where an action exists
/// (ask-level packages have none), giving only where giving does
/// (owner-level); unculling, skipping and quitting are always open.
pub fn menu(
    allow_enact: bool,
    allow_give: bool,
    enact_word: &'static str,
) -> Vec<sandogasa_review::Choice> {
    use sandogasa_review::{Arg, Choice};
    let mut m = Vec::new();
    if allow_enact {
        m.push(Choice::new('y', enact_word));
    }
    if allow_give {
        m.push(Choice::with('g', "give", Arg::Required("user")));
    }
    m.push(Choice::with('u', "uncull", Arg::Optional("inventory")));
    m.push(Choice::new('s', "skip"));
    m.push(Choice::new('q', "quit"));
    m
}

/// The [`Choice`] an answer to [`menu`] means. Enter (the menu's
/// default) and end of input skip — the safe default for actions that
/// change ownership on a server.
pub fn from_answer(answer: sandogasa_review::Answer) -> Choice {
    use sandogasa_review::Answer;
    match answer {
        Answer::Pick { key: 'y', .. } => Choice::Enact,
        Answer::Pick { key: 'g', arg } => Choice::Give(arg.unwrap_or_default()),
        Answer::Pick { key: 'u', arg } => Choice::Uncull(arg),
        Answer::Pick { key: 'q', .. } | Answer::Quit => Choice::Quit,
        Answer::Pick { .. } | Answer::All => Choice::Skip,
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
    Dependents(Vec<Dependent>),
}

/// One requiring source package, and whether the requirement is
/// severable: `feature_only` means every requiring binary of it is a
/// `foo+feature` subpackage, so dropping that optional feature would
/// break the edge — a soft block, unlike a hard dependency from the
/// base package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dependent {
    pub source: String,
    pub feature_only: bool,
}

/// Resolve `(binary, source)` requirers into dependent source
/// packages, dropping the package's own subpackages and fedrq's
/// `(none)` rows. A source is `feature_only` when every one of its
/// requiring binaries is a `+feature` subpackage — the `+` lives in
/// the binary name, while the source is what fedrq resolved it to.
pub fn resolve_dependents(package: &str, requirers: Vec<(String, String)>) -> Vec<Dependent> {
    // Source → did any *base* (non-`+feature`) binary of it require us?
    let mut hard: std::collections::BTreeMap<String, bool> = std::collections::BTreeMap::new();
    for (binary, source) in requirers {
        if binary == "(none)" || source == package {
            continue;
        }
        let is_feature = binary.contains('+');
        let entry = hard.entry(source).or_insert(false);
        *entry = *entry || !is_feature;
    }
    hard.into_iter()
        .map(|(source, had_hard)| Dependent {
            source,
            feature_only: !had_hard,
        })
        .collect()
}

/// Probe a package's reverse dependencies on one branch: all of its
/// subpackages (so feature-gated capabilities are covered), resolved
/// to requiring source packages with their severability.
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
    let requirers = fedrq
        .whatrequires_binaries(&binaries)
        .map_err(|e| format!("whatrequires for {package} on {branch}: {e}"))?;
    Ok(Probe::Dependents(resolve_dependents(package, requirers)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str, allow_enact: bool, allow_give: bool) -> Option<Choice> {
        let choices = menu(allow_enact, allow_give, "orphan");
        let m = sandogasa_review::Menu {
            choices: &choices,
            default: Some('s'),
            all: false,
            quit: false,
            default_arg: None,
        };
        sandogasa_review::parse_answer(line, &m).map(from_answer)
    }

    #[test]
    fn skip_is_the_default_and_junk_reasks() {
        for line in ["", "  ", "s", "Skip"] {
            assert_eq!(parse(line, true, true), Some(Choice::Skip));
        }
        assert_eq!(parse("y", true, true), Some(Choice::Enact));
        assert_eq!(parse("q", false, false), Some(Choice::Quit));
        assert_eq!(parse("x", true, true), None);
        assert_eq!(parse("orphan it", true, true), None);
        // Ask-level prompts have no action to enact.
        assert_eq!(parse("y", false, false), None);
    }

    #[test]
    fn give_names_a_user_and_only_where_giving_applies() {
        assert_eq!(
            parse("g decathorpe", true, true),
            Some(Choice::Give("decathorpe".to_string()))
        );
        assert_eq!(
            parse("G  someone ", true, true),
            Some(Choice::Give("someone".to_string()))
        );
        // Bare g parses with no user yet (ask prompts for it); admin-level
        // prompts take no g at all.
        assert_eq!(parse("g", true, true), Some(Choice::Give(String::new())));
        assert_eq!(parse("g someone", true, false), None);
    }

    #[test]
    fn uncull_is_always_open_and_optionally_files_as_essential() {
        assert_eq!(parse("u", true, true), Some(Choice::Uncull(None)));
        assert_eq!(
            parse("u keep-tools.toml", false, false),
            Some(Choice::Uncull(Some("keep-tools.toml".to_string())))
        );
        assert_eq!(parse("uncull", false, false), Some(Choice::Uncull(None)));
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
    fn dependents_resolve_to_sources_and_flag_feature_only_edges() {
        // breezy requires via its base binary (hard); python-dulwich
        // only via python3-dulwich+merge (severable) — and that
        // feature binary's base name (python3-dulwich) is NOT the
        // source (python-dulwich), so the source must come from fedrq,
        // not from stripping the `+`.
        let requirers = vec![
            ("(none)".to_string(), "(none)".to_string()),
            ("breezy".to_string(), "breezy".to_string()),
            (
                "python3-dulwich+merge".to_string(),
                "python-dulwich".to_string(),
            ),
        ];
        let deps = resolve_dependents("python-merge3", requirers);
        assert_eq!(
            deps,
            [
                Dependent {
                    source: "breezy".into(),
                    feature_only: false
                },
                Dependent {
                    source: "python-dulwich".into(),
                    feature_only: true
                },
            ]
        );
    }

    #[test]
    fn a_source_with_both_edges_is_a_hard_dependency() {
        // If any base binary requires us, the source is a hard block
        // even when another of its subpackages is a feature edge.
        let deps = resolve_dependents(
            "libx",
            vec![
                ("app".into(), "app".into()),
                ("app+extra".into(), "app".into()),
            ],
        );
        assert_eq!(
            deps,
            [Dependent {
                source: "app".into(),
                feature_only: false
            }]
        );
    }

    #[test]
    fn own_subpackages_and_none_rows_are_not_dependents() {
        let deps = resolve_dependents(
            "rust-buf-min",
            vec![
                ("(none)".into(), "(none)".into()),
                ("rust-buf-min".into(), "rust-buf-min".into()),
                ("rust-buf-min+std".into(), "rust-buf-min".into()),
                (
                    "rust-v_htmlescape+default".into(),
                    "rust-v_htmlescape".into(),
                ),
            ],
        );
        // Own subpackages (base and +feature) drop out; the external
        // requirer remains, feature-only.
        assert_eq!(
            deps,
            [Dependent {
                source: "rust-v_htmlescape".into(),
                feature_only: true
            }]
        );
    }
}
