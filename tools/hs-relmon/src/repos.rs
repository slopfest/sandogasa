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
//! `c10s-sig-hyperscale[-…]`. Only a branch somebody builds from
//! counts: a build's release tag names its branch (`hsx.el10` came
//! from `c10s-hsx`), so a branch with no tagged build in CBS — wprof's
//! `c10s-hsx` after its builds moved to EPEL — is passed over and a
//! default pointing at one is flagged. Among the live branches the
//! newest release wins; within it a default that already is one of
//! them stays (a kernel repo defaulting to `c10s-hsk` is deliberate),
//! otherwise `-hs`, then a lettered variant, then a flavor, then the
//! old spelling.

use sandogasa_gitlab::{Client, ProjectUpdate};

use crate::cbs::Build;

/// The merge method Hyperscale's repos should have, from cbspkg's
/// config (`[sigs.hyperscale] merge_method`, read from `table`, the
/// merged file) else the SIG's own in sandogasa-cbs: the same one
/// `cbspkg sig-repo` sets, so neither tool undoes the other.
pub fn wanted_merge_method(table: Option<&toml::Table>) -> Result<String, String> {
    let configured = table
        .and_then(|t| t.get("sigs"))
        .and_then(|s| s.get(sandogasa_cbs::Sig::Hyperscale.name()))
        .and_then(|h| h.get("merge_method"));
    let configured = match configured {
        None => None,
        Some(toml::Value::String(m)) => Some(m.as_str()),
        Some(_) => return Err("[sigs.hyperscale] merge_method is not a string".to_string()),
    };
    sandogasa_cbs::Sig::Hyperscale
        .wanted_merge_method(configured)?
        .ok_or_else(|| "Hyperscale has no merge method".to_string())
}

/// [`wanted_merge_method`] from cbspkg's config files, as cbspkg reads
/// them; an error names them.
pub fn configured_merge_method() -> Result<String, String> {
    let cf = sandogasa_config::ConfigFile::for_tool("cbspkg");
    cf.read_merged()
        .and_then(|t| wanted_merge_method(t.as_ref()))
        .map_err(|e| format!("{e} (in {})", cf.describe_sources()))
}

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

/// The dist tag a build carries for its Hyperscale branch, as
/// `(release, suffix)`: `wprof-0.6-2.hsx.el9` → `(9, "hsx")`,
/// `perf-6.19~rc6-3.hs+fb.el9` → `(9, "hs+fb")`. The last such pair in
/// the release string wins (`hs+asahi.1.hs+asahi.el10s` has two).
pub fn build_branch_tag(release: &str) -> Option<(u32, String)> {
    release.rmatch_indices(".el").find_map(|(i, _)| {
        let version: u32 = release[i + 3..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .ok()?;
        let head = &release[..i];
        let suffix = head.rsplit('.').next()?;
        (suffix.starts_with("hs")).then(|| (version, suffix.to_string()))
    })
}

/// The dist suffix builds from a branch carry: `hs` for `c10s-hs` and
/// the old `c10s-sig-hyperscale`, `hsx` for `c10s-hsx`, `hs+fb` for
/// `c10s-hs+fb`.
fn branch_suffix(name: &str, spelling: Spelling) -> &str {
    match spelling {
        Spelling::Main | Spelling::Legacy => "hs",
        Spelling::Variant | Spelling::Flavor => name.rsplit("s-").next().unwrap_or("hs"),
    }
}

/// The Hyperscale branches of `branches` with a tagged build in CBS.
pub fn live_branches<'a>(
    branches: impl IntoIterator<Item = &'a str>,
    builds: &[Build],
) -> Vec<&'a str> {
    let tags: std::collections::BTreeSet<(u32, String)> = builds
        .iter()
        .filter_map(|b| build_branch_tag(&b.release))
        .collect();
    branches
        .into_iter()
        .filter(|b| {
            parse_hs_branch(b)
                .is_some_and(|(v, s)| tags.contains(&(v, branch_suffix(b, s).to_string())))
        })
        .collect()
}

/// The branch the repo should open on: among the newest release's
/// Hyperscale branches (of those given — the caller passes the live
/// ones), the current default if it is one of them (a chosen variant
/// stays chosen), else the best spelling, ties by name. `None` when
/// there is none.
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
    /// Hyperscale branches exist but none has a tagged build in CBS.
    pub no_live_branch: bool,
    /// The current default is a Hyperscale branch with no tagged build.
    pub default_has_no_builds: bool,
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

/// [`plan_wanting`] Hyperscale's own merge method.
#[cfg(test)]
pub fn plan(
    package: &str,
    archived: bool,
    default_branch: Option<&str>,
    merge_method: Option<&str>,
    branches: &[String],
    builds: &[Build],
) -> RepoPlan {
    let wanted = wanted_merge_method(None).unwrap();
    plan_wanting(
        package,
        archived,
        default_branch,
        merge_method,
        branches,
        builds,
        &wanted,
    )
}

/// Judge one repo from what GitLab reports and what CBS has tagged,
/// wanting merge requests to land by `wanted_method`.
pub fn plan_wanting(
    package: &str,
    archived: bool,
    default_branch: Option<&str>,
    merge_method: Option<&str>,
    branches: &[String],
    builds: &[Build],
    wanted_method: &str,
) -> RepoPlan {
    let hs: Vec<&str> = branches
        .iter()
        .map(String::as_str)
        .filter(|b| parse_hs_branch(b).is_some())
        .collect();
    let live = live_branches(hs.iter().copied(), builds);
    let wanted = pick_default_branch(live.iter().copied(), default_branch);
    let default_has_no_builds =
        default_branch.is_some_and(|d| hs.contains(&d) && !live.contains(&d));
    let default_branch_change = match (default_branch, wanted) {
        (Some(cur), Some(want)) if cur != want => Some((cur.to_string(), want.to_string())),
        (None, Some(want)) => Some((String::new(), want.to_string())),
        _ => None,
    };
    let merge_change = match merge_method {
        Some(m) if m != wanted_method => Some((m.to_string(), wanted_method.to_string())),
        _ => None,
    };
    RepoPlan {
        package: package.to_string(),
        archived,
        default_branch: default_branch_change,
        merge_method: merge_change,
        no_hs_branch: hs.is_empty(),
        no_live_branch: !hs.is_empty() && live.is_empty(),
        default_has_no_builds,
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
    if plan.no_live_branch {
        parts.push("no Hyperscale branch has a tagged build".to_string());
    }
    if plan.default_has_no_builds
        && let Some(d) = plan.default_branch.as_ref().map(|(from, _)| from.as_str())
    {
        parts.push(format!("{d} has no tagged builds"));
    } else if plan.default_has_no_builds {
        parts.push("the default branch has no tagged builds".to_string());
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

/// Fetch a repo's settings and branches and judge them against the
/// package's tagged builds and `wanted_method`.
pub fn plan_for_repo(
    client: &Client,
    package: &str,
    builds: &[Build],
    wanted_method: &str,
) -> Result<RepoPlan, Box<dyn std::error::Error>> {
    let status = client.project_status()?;
    let branches: Vec<String> = client
        .list_branches()?
        .into_iter()
        .map(|b| b.name)
        .collect();
    Ok(plan_wanting(
        package,
        status.archived,
        status.default_branch.as_deref(),
        status.merge_method.as_deref(),
        &branches,
        builds,
        wanted_method,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbspkgs_merge_method_for_hyperscale_is_the_one_wanted() {
        assert_eq!(wanted_merge_method(None).unwrap(), "ff");
        let table = |t: &str| -> toml::Table { toml::from_str(t).unwrap() };
        let other = table("[sigs.proposed_updates]\nmerge_method = \"merge\"\n");
        assert_eq!(wanted_merge_method(Some(&other)).unwrap(), "ff");
        let set = table("[sigs.hyperscale]\nmerge_method = \"rebase_merge\"\n");
        assert_eq!(wanted_merge_method(Some(&set)).unwrap(), "rebase_merge");
        let p = plan_wanting("rpm", false, None, Some("ff"), &[], &[], "rebase_merge");
        assert_eq!(
            p.merge_method,
            Some(("ff".to_string(), "rebase_merge".to_string()))
        );
        let bad = table("[sigs.hyperscale]\nmerge_method = \"squash\"\n");
        assert!(
            wanted_merge_method(Some(&bad))
                .unwrap_err()
                .contains("squash")
        );
        let typed = table("[sigs.hyperscale]\nmerge_method = 1\n");
        assert!(
            wanted_merge_method(Some(&typed))
                .unwrap_err()
                .contains("not a string")
        );
    }

    #[test]
    #[serial_test::serial]
    fn the_merge_method_is_read_from_cbspkgs_config() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cbspkg");
        std::fs::create_dir_all(&dir).unwrap();
        let _env = sandogasa_config::test_support::EnvGuard::new(&[(
            "XDG_CONFIG_HOME",
            &tmp.path().display().to_string(),
        )]);
        assert_eq!(configured_merge_method().unwrap(), "ff");
        std::fs::write(
            dir.join("config.toml"),
            "[sigs.hyperscale]\nmerge_method = \"squash\"\n",
        )
        .unwrap();
        let err = configured_merge_method().unwrap_err();
        assert!(err.contains("cbspkg/config.toml"), "{err}");
    }

    /// One tagged build per Hyperscale branch named, so every such
    /// branch counts as live.
    fn hs_builds(branches: &[String]) -> Vec<Build> {
        branches
            .iter()
            .filter_map(|b| {
                let (v, s) = parse_hs_branch(b)?;
                let suffix = branch_suffix(b, s);
                Some(Build {
                    build_id: 1,
                    name: "pkg".into(),
                    version: "1.0".into(),
                    release: format!("1.{suffix}.el{v}"),
                    nvr: format!("pkg-1.0-1.{suffix}.el{v}"),
                })
            })
            .collect()
    }

    #[test]
    fn a_build_names_its_branch_by_its_dist_tag() {
        assert_eq!(build_branch_tag("2.hsx.el9"), Some((9, "hsx".into())));
        assert_eq!(build_branch_tag("3.hs+fb.el9"), Some((9, "hs+fb".into())));
        assert_eq!(
            build_branch_tag("1.hs+asahi.1.hs+asahi.el10s"),
            Some((10, "hs+asahi".into()))
        );
        assert_eq!(build_branch_tag("1.hs1.hsk.el10"), Some((10, "hsk".into())));
        assert_eq!(
            build_branch_tag("3.el9"),
            None,
            "stock, no Hyperscale suffix"
        );
        assert_eq!(build_branch_tag("13.hs.el9"), Some((9, "hs".into())));
    }

    #[test]
    fn only_branches_with_tagged_builds_are_live_and_a_dead_default_is_flagged() {
        // wprof: branches for both releases, builds only from c9s-hsx,
        // default left on c10s-hsx by hand.
        let branches: Vec<String> = ["c10s-hsx", "c9s-hsx", "main"].map(String::from).to_vec();
        let builds = vec![Build {
            build_id: 1,
            name: "wprof".into(),
            version: "0.6".into(),
            release: "2.hsx.el9".into(),
            nvr: "wprof-0.6-2.hsx.el9".into(),
        }];
        assert_eq!(
            live_branches(branches.iter().map(String::as_str), &builds),
            ["c9s-hsx"]
        );
        let p = plan(
            "wprof",
            false,
            Some("c10s-hsx"),
            Some("ff"),
            &branches,
            &builds,
        );
        assert_eq!(
            p.default_branch,
            Some(("c10s-hsx".into(), "c9s-hsx".into()))
        );
        assert!(p.default_has_no_builds && !p.no_live_branch && !p.no_hs_branch);
        assert_eq!(
            render(&p),
            "wprof: default branch c10s-hsx → c9s-hsx; c10s-hsx has no tagged builds\n"
        );
        // No build from any Hyperscale branch: the default stays, flagged.
        let p = plan("gone", false, Some("c10s-hsx"), Some("ff"), &branches, &[]);
        assert!(p.default_branch.is_none() && p.no_live_branch && p.default_has_no_builds);
        assert!(
            render(&p).contains("no Hyperscale branch has a tagged build"),
            "{}",
            render(&p)
        );
        assert!(
            render(&p).contains("the default branch has no tagged builds"),
            "{}",
            render(&p)
        );
    }

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
        let p = plan(
            "tar",
            false,
            Some("c10s"),
            Some("merge"),
            &branches,
            &hs_builds(&branches),
        );
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

        let one = ["c10s-hs".to_string()];
        let fine = plan(
            "crun",
            false,
            Some("c10s-hs"),
            Some("ff"),
            &one,
            &hs_builds(&one),
        );
        assert!(!fine.needs_change());
        assert_eq!(render(&fine), "crun: ok\n");

        // Archived: reported, never changed.
        let one9 = ["c9s-hs".to_string()];
        let archived = plan(
            "old",
            true,
            Some("c10s"),
            Some("merge"),
            &one9,
            &hs_builds(&one9),
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
            &[],
        );
        assert!(none.no_hs_branch && none.default_branch.is_none() && none.needs_change());
        assert!(render(&none).contains("no Hyperscale branch"));
    }

    #[test]
    fn apply_with_yes_sets_every_repo_that_needs_it_and_nothing_else() {
        let branches: Vec<String> = ["c10s", "c9s-hs"].map(String::from).to_vec();
        let plans = vec![
            plan(
                "tar",
                false,
                Some("c10s"),
                Some("merge"),
                &branches,
                &hs_builds(&branches),
            ),
            plan(
                "fine",
                false,
                Some("c9s-hs"),
                Some("ff"),
                &branches,
                &hs_builds(&branches),
            ),
            plan(
                "old",
                true,
                Some("c10s"),
                Some("merge"),
                &branches,
                &hs_builds(&branches),
            ),
            plan(
                "broken",
                false,
                Some("c10s"),
                Some("merge"),
                &branches,
                &hs_builds(&branches),
            ),
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
