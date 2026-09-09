// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `check-repos` — the SIG's GitLab repos set up the same way.
//!
//! Two settings drift as repos are forked in from Fedora or created by
//! hand: the default branch, which should be the newest release's
//! Hyperscale branch rather than the `rawhide` or `c10s` the fork
//! arrived with, and the merge method, which should be fast-forward so
//! the branch history stays linear. `check-repos <manifest>` reports
//! every manifest package's repo against that, and `--apply` sets what
//! differs (`PUT /projects/:id`, one confirmation unless `--yes`).
//! Archived repos are read-only and only reported.
//!
//! Hyperscale branches come in several spellings: `c10s-hs` (the
//! main one), `c10s-hsx` and `c10s-hsk` (experimental, kernel),
//! `c10s-hs+fb` and `c10s-hs+asahi` (flavors), and the older
//! `c10s-sig-hyperscale[-…]`. The newest release wins; within it a
//! default that already is one of its Hyperscale branches stays (a
//! kernel repo defaulting to `c10s-hsk` is deliberate), otherwise
//! `-hs`, then a lettered variant, then a flavor, then the old spelling.

use sandogasa_gitlab::{Client, ProjectUpdate};

/// The merge method every SIG repo should use.
pub const MERGE_METHOD: &str = "ff";

/// How a Hyperscale branch is spelled, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Spelling {
    /// `c10s-hs`.
    Main,
    /// `c10s-hsx`, `c10s-hsk`: a lettered variant.
    Variant,
    /// `c10s-hs+fb`, `c10s-hs+asahi`: a flavor.
    Flavor,
    /// `c10s-sig-hyperscale`, `c10s-sig-hyperscale-experimental`: the
    /// old spelling.
    Legacy,
}

/// A Hyperscale branch name taken apart into its CentOS Stream release
/// and its spelling; `None` for anything else (`c10s`, `rawhide`,
/// `main`, `epel10`).
pub fn parse_hs_branch(name: &str) -> Option<(u32, Spelling)> {
    let rest = name.strip_prefix('c')?;
    let (version, tail) = rest.split_once("s-")?;
    let version: u32 = version.parse().ok()?;
    let spelling = if tail == "hs" {
        Spelling::Main
    } else if let Some(suffix) = tail.strip_prefix("hs+") {
        (!suffix.is_empty()).then_some(Spelling::Flavor)?
    } else if let Some(letters) = tail.strip_prefix("hs") {
        (!letters.is_empty() && letters.chars().all(|c| c.is_ascii_lowercase()))
            .then_some(Spelling::Variant)?
    } else if tail == "sig-hyperscale" || tail.starts_with("sig-hyperscale-") {
        Spelling::Legacy
    } else {
        return None;
    };
    Some((version, spelling))
}

/// The branch the repo should open on: among the newest release's
/// Hyperscale branches, the current default if it is one of them
/// (a chosen variant stays chosen), else the best spelling, ties by
/// name. `None` when the repo has no Hyperscale branch at all.
pub fn pick_default_branch<'a>(
    branches: impl IntoIterator<Item = &'a str>,
    current: Option<&str>,
) -> Option<&'a str> {
    let parsed: Vec<(u32, Spelling, &str)> = branches
        .into_iter()
        .filter_map(|b| parse_hs_branch(b).map(|(v, s)| (v, s, b)))
        .collect();
    let newest = parsed.iter().map(|(v, _, _)| *v).max()?;
    let mut candidates: Vec<(Spelling, &str)> = parsed
        .into_iter()
        .filter(|(v, _, _)| *v == newest)
        .map(|(_, s, b)| (s, b))
        .collect();
    if let Some(cur) = current
        && let Some((_, b)) = candidates.iter().find(|(_, b)| *b == cur)
    {
        return Some(b);
    }
    candidates.sort();
    candidates.first().map(|(_, b)| *b)
}

/// One repo's settings against the SIG's conventions.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RepoPlan {
    pub package: String,
    pub archived: bool,
    /// `(current, wanted)` when the default branch should change.
    pub default_branch: Option<(String, String)>,
    /// `(current, wanted)` when the merge method should change.
    pub merge_method: Option<(String, String)>,
    /// The repo has no Hyperscale branch to default to.
    pub no_hs_branch: bool,
}

impl RepoPlan {
    /// Whether `--apply` has anything to send.
    pub fn needs_change(&self) -> bool {
        !self.archived && (self.default_branch.is_some() || self.merge_method.is_some())
    }

    /// The fields to send.
    pub fn update(&self) -> ProjectUpdate {
        ProjectUpdate {
            default_branch: self.default_branch.as_ref().map(|(_, to)| to.clone()),
            merge_method: self.merge_method.as_ref().map(|(_, to)| to.clone()),
        }
    }
}

/// Judge one repo from what GitLab reports.
pub fn plan(
    package: &str,
    archived: bool,
    default_branch: Option<&str>,
    merge_method: Option<&str>,
    branches: &[String],
) -> RepoPlan {
    let wanted = pick_default_branch(branches.iter().map(String::as_str), default_branch);
    let default_branch_change = match (default_branch, wanted) {
        (Some(cur), Some(want)) if cur != want => Some((cur.to_string(), want.to_string())),
        (None, Some(want)) => Some((String::new(), want.to_string())),
        _ => None,
    };
    let merge_change = match merge_method {
        Some(m) if m != MERGE_METHOD => Some((m.to_string(), MERGE_METHOD.to_string())),
        _ => None,
    };
    RepoPlan {
        package: package.to_string(),
        archived,
        default_branch: default_branch_change,
        merge_method: merge_change,
        no_hs_branch: wanted.is_none(),
    }
}

/// What is off in one repo, as a phrase: "default branch c10s → c9s-hs;
/// merge method merge → ff", or "ok".
pub fn changes(plan: &RepoPlan) -> String {
    let mut parts = Vec::new();
    if let Some((from, to)) = &plan.default_branch {
        parts.push(format!(
            "default branch {} → {to}",
            if from.is_empty() { "(none)" } else { from }
        ));
    }
    if let Some((from, to)) = &plan.merge_method {
        parts.push(format!("merge method {from} → {to}"));
    }
    if plan.no_hs_branch {
        parts.push("no Hyperscale branch (c*s-hs, -hsx, -hs+fb, -sig-hyperscale…)".to_string());
    }
    if parts.is_empty() {
        "ok".to_string()
    } else {
        parts.join("; ")
    }
}

/// One line per repo: what is off, or that it is fine.
pub fn render(plan: &RepoPlan) -> String {
    let archived = if plan.archived {
        " [archived: read-only, not changed]"
    } else {
        ""
    };
    format!("{}: {}{archived}\n", plan.package, changes(plan))
}

/// What `apply` did.
#[derive(Debug, Default, PartialEq)]
pub struct ApplyOutcome {
    pub set: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// Set the repos that need it, one question each unless `assume_yes`:
/// `y` sets this one, `s` (Enter) skips it, `a` sets this and every
/// remaining one, `q` skips the rest. `set` performs one repo's change;
/// it is injected so tests need no GitLab.
pub fn apply(
    plans: &[RepoPlan],
    assume_yes: bool,
    set: impl Fn(&RepoPlan) -> Result<(), Box<dyn std::error::Error>>,
) -> Result<ApplyOutcome, Box<dyn std::error::Error>> {
    use sandogasa_review::{Answer, Choice, Menu};
    const CHOICES: [Choice; 2] = [Choice::new('y', "yes"), Choice::new('s', "skip")];
    let mut out = ApplyOutcome::default();
    let mut all = assume_yes;
    let mut pending = plans.iter().filter(|p| p.needs_change()).peekable();
    while let Some(plan) = pending.next() {
        let go = all || {
            match sandogasa_review::ask(
                &format!("{}: set {}?", plan.package, changes(plan)),
                &Menu {
                    choices: &CHOICES,
                    default: Some('s'),
                    all: true,
                    quit: true,
                    default_arg: None,
                },
            )? {
                Answer::Pick { key: 'y', .. } => true,
                Answer::All => {
                    all = true;
                    true
                }
                Answer::Quit => {
                    out.skipped += 1 + pending.count();
                    return Ok(out);
                }
                _ => false,
            }
        };
        if !go {
            out.skipped += 1;
            continue;
        }
        match set(plan) {
            Ok(()) => {
                eprintln!("{}: set", plan.package);
                out.set += 1;
            }
            Err(e) => {
                eprintln!("{}: {e}", plan.package);
                out.failed += 1;
            }
        }
    }
    Ok(out)
}

/// Fetch a repo's settings and branches and judge them.
pub fn plan_for_repo(
    client: &Client,
    package: &str,
) -> Result<RepoPlan, Box<dyn std::error::Error>> {
    let status = client.project_status()?;
    let branches: Vec<String> = client
        .list_branches()?
        .into_iter()
        .map(|b| b.name)
        .collect();
    Ok(plan(
        package,
        status.archived,
        status.default_branch.as_deref(),
        status.merge_method.as_deref(),
        &branches,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyperscale_branches_parse_in_every_spelling() {
        assert_eq!(parse_hs_branch("c10s-hs"), Some((10, Spelling::Main)));
        assert_eq!(parse_hs_branch("c9s-hsx"), Some((9, Spelling::Variant)));
        assert_eq!(parse_hs_branch("c10s-hsk"), Some((10, Spelling::Variant)));
        assert_eq!(parse_hs_branch("c10s-hs+fb"), Some((10, Spelling::Flavor)));
        assert_eq!(
            parse_hs_branch("c10s-hs+asahi"),
            Some((10, Spelling::Flavor))
        );
        assert_eq!(
            parse_hs_branch("c9s-sig-hyperscale"),
            Some((9, Spelling::Legacy))
        );
        assert_eq!(
            parse_hs_branch("c10s-sig-hyperscale-v257"),
            Some((10, Spelling::Legacy))
        );
        for other in [
            "c10s", "rawhide", "main", "epel10", "c10s-hs+", "c10s-hs1", "c8s-sig",
        ] {
            assert_eq!(parse_hs_branch(other), None, "{other}");
        }
    }

    #[test]
    fn the_newest_release_wins_and_a_chosen_variant_stays() {
        // Newest release first, whatever its spelling.
        assert_eq!(
            pick_default_branch(
                ["c9s", "c9s-hs", "c10s", "c10s-hsx", "rawhide"],
                Some("c10s")
            ),
            Some("c10s-hsx")
        );
        // Within a release: main, then variant, then flavor, then legacy.
        assert_eq!(
            pick_default_branch(
                ["c10s-hs+fb", "c10s-hsx", "c10s-hs", "c10s-sig-hyperscale"],
                None
            ),
            Some("c10s-hs")
        );
        assert_eq!(
            pick_default_branch(["c10s-hs+fb", "c10s-sig-hyperscale", "c10s-hsk"], None),
            Some("c10s-hsk")
        );
        // kernel: the default is already a Hyperscale branch of the
        // newest release, so it stays even though -hs+asahi would sort
        // after it anyway and -sig-hyperscale too.
        let kernel = [
            "c10s-hs+asahi",
            "c10s-hsk",
            "c10s-sig-hyperscale",
            "c9s-hsx",
        ];
        assert_eq!(
            pick_default_branch(kernel, Some("c10s-hsk")),
            Some("c10s-hsk")
        );
        assert_eq!(
            pick_default_branch(kernel, Some("c9s-hsx")),
            Some("c10s-hsk"),
            "an older release does not stay"
        );
        assert_eq!(pick_default_branch(["c10s", "main"], Some("main")), None);
    }

    #[test]
    fn plan_names_what_differs_and_nothing_else() {
        let branches: Vec<String> = ["c10s", "c9s", "c9s-hs"].map(String::from).to_vec();
        let p = plan("tar", false, Some("c10s"), Some("merge"), &branches);
        assert_eq!(p.default_branch, Some(("c10s".into(), "c9s-hs".into())));
        assert_eq!(p.merge_method, Some(("merge".into(), "ff".into())));
        assert!(p.needs_change() && !p.no_hs_branch);
        assert_eq!(
            render(&p),
            "tar: default branch c10s → c9s-hs; merge method merge → ff\n"
        );
        let update = p.update();
        assert_eq!(update.default_branch.as_deref(), Some("c9s-hs"));
        assert_eq!(update.merge_method.as_deref(), Some("ff"));

        let fine = plan(
            "crun",
            false,
            Some("c10s-hs"),
            Some("ff"),
            &["c10s-hs".to_string()],
        );
        assert!(!fine.needs_change());
        assert_eq!(render(&fine), "crun: ok\n");

        // Archived: reported, never changed.
        let archived = plan(
            "old",
            true,
            Some("c10s"),
            Some("merge"),
            &["c9s-hs".to_string()],
        );
        assert!(!archived.needs_change());
        assert!(render(&archived).ends_with("[archived: read-only, not changed]\n"));

        // No Hyperscale branch: the merge method can still be fixed.
        let none = plan(
            "new",
            false,
            Some("main"),
            Some("merge"),
            &["main".to_string()],
        );
        assert!(none.no_hs_branch && none.default_branch.is_none() && none.needs_change());
        assert!(render(&none).contains("no Hyperscale branch"));
    }

    #[test]
    fn apply_with_yes_sets_every_repo_that_needs_it_and_nothing_else() {
        let branches: Vec<String> = ["c10s", "c9s-hs"].map(String::from).to_vec();
        let plans = vec![
            plan("tar", false, Some("c10s"), Some("merge"), &branches),
            plan("fine", false, Some("c9s-hs"), Some("ff"), &branches),
            plan("old", true, Some("c10s"), Some("merge"), &branches),
            plan("broken", false, Some("c10s"), Some("merge"), &branches),
        ];
        let touched = std::cell::RefCell::new(Vec::new());
        let outcome = apply(&plans, true, |p| {
            touched.borrow_mut().push(p.package.clone());
            if p.package == "broken" {
                Err("nope".into())
            } else {
                Ok(())
            }
        })
        .unwrap();
        assert_eq!(*touched.borrow(), ["tar", "broken"]);
        assert_eq!(
            outcome,
            ApplyOutcome {
                set: 1,
                skipped: 0,
                failed: 1
            }
        );
    }
}
