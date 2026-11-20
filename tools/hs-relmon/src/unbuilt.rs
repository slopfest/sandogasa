// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `check-unbuilt`: Hyperscale branches with merged commits their
//! newest build does not have ([`sandogasa_cbs::unbuilt`]), each
//! build's branch read from its dist tag.

use std::collections::BTreeMap;

use sandogasa_cbs::unbuilt::Target;

use crate::cbs::Build;
use crate::repos::build_branch_tag;

pub use sandogasa_cbs::unbuilt::DEFAULT_GRACE_DAYS;

/// The branches builds carrying the dist tag `(release, suffix)` come
/// from, the likeliest first. Hyperscale's convention: a package built
/// for both the main repository and a flavor builds both from the main
/// branch (`c9s-hs`), only the target's dist tag differing, and a
/// package mostly for one flavor's use has that flavor's branch
/// (`c9s-hs+fb`). So the flavor's or variant's own branch is tried
/// first, then `c9s-hs`, then its old spelling `c9s-sig-hyperscale`.
pub fn branches_for(release: u32, suffix: &str) -> Vec<String> {
    let mut branches = Vec::new();
    if suffix != "hs" {
        branches.push(format!("c{release}s-{suffix}"));
    }
    branches.push(format!("c{release}s-hs"));
    branches.push(format!("c{release}s-sig-hyperscale"));
    branches
}

/// Whether a build in `tag` counts as built from its branch: a
/// Hyperscale collection's `-candidate`, `-testing` or `-release` tag,
/// as [`crate::prune_tags::TagIndex::load_built`] reads them. A build
/// untagged from all of them, rejected say, does not.
pub fn counts_as_built(tag: &str) -> bool {
    tag.starts_with("hyperscale")
        && tag.contains("-packages-")
        && ["-candidate", "-testing", "-release"]
            .iter()
            .any(|s| tag.ends_with(s))
}

/// `package`'s newest build per Hyperscale branch among `builds`
/// (the highest build id, wherever it is tagged), as targets to check.
pub fn targets(package: &str, builds: impl IntoIterator<Item = Build>) -> Vec<Target> {
    let mut newest: BTreeMap<(u32, String), Build> = BTreeMap::new();
    for b in builds.into_iter().filter(|b| b.name == package) {
        let Some(key) = build_branch_tag(&b.release) else {
            continue;
        };
        match newest.get(&key) {
            Some(kept) if kept.build_id >= b.build_id => {}
            _ => {
                newest.insert(key, b);
            }
        }
    }
    newest
        .into_iter()
        .map(|((release, suffix), b)| Target {
            package: package.to_string(),
            branches: branches_for(release, &suffix),
            nvr: b.nvr,
        })
        .collect()
}

/// `check-unbuilt`'s report of `outcome`, over `builds` builds: the
/// branches with merged commits not built as a table, each one's
/// commits (the newest ten, or `all_commits`), a summary line, then the
/// builds not compared, one line per kind naming their packages, or
/// with `verbose` each build and why.
pub fn report(
    outcome: &sandogasa_cbs::unbuilt::Outcome,
    builds: usize,
    grace_days: u32,
    all_commits: bool,
    verbose: bool,
) -> String {
    use sandogasa_cbs::unbuilt;
    let mut out = String::new();
    if !outcome.unbuilt.is_empty() {
        out.push_str(&format!(
            "{} branch(es) with merged commits not built:\n\n{}",
            outcome.unbuilt.len(),
            unbuilt::table(&outcome.unbuilt)
        ));
        let limit = (!all_commits).then_some(unbuilt::SHOWN);
        for u in &outcome.unbuilt {
            out.push('\n');
            out.push_str(&unbuilt::render(u, limit));
            if limit.is_some_and(|l| u.commits.len() > l) {
                // render ends with the "and N older" line it was cut at.
                out.pop();
                out.push_str(" (--all-commits lists them)\n");
            }
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "{builds} build(s): {} branch(es) not built, {} with commits from the last \
         {grace_days} day(s) only{}\n",
        outcome.unbuilt.len(),
        outcome.recent.len(),
        match outcome.problems.len() {
            0 => String::new(),
            n => format!(", {n} could not be checked"),
        }
    ));
    let skipped = outcome.unsourced.len() + outcome.unplaced.len();
    if skipped > 0 {
        out.push_str(&format!(
            "Not compared, {skipped} build(s){}:\n",
            if verbose { "" } else { " (-v lists them)" }
        ));
        // A note is `<package>: <nvr>: <why>`, an unsourced entry the NVR.
        let package_of_nvr = |nvr: &str| {
            let mut parts = nvr.rsplitn(3, '-');
            parts.nth(2).unwrap_or(nvr).to_string()
        };
        let kinds = [
            (
                "no commit recorded (built from an uploaded SRPM?)",
                outcome
                    .unsourced
                    .iter()
                    .map(|n| (package_of_nvr(n), n.clone()))
                    .collect::<Vec<_>>(),
            ),
            (
                "no branch holds the commit they were built from",
                outcome
                    .unplaced
                    .iter()
                    .map(|e| {
                        let (p, rest) = e.split_once(": ").unwrap_or((e, ""));
                        (p.to_string(), rest.to_string())
                    })
                    .collect(),
            ),
        ];
        for (why, items) in kinds {
            if items.is_empty() {
                continue;
            }
            match verbose {
                true => {
                    out.push_str(&format!("  {why} ({}):\n", items.len()));
                    for (_, detail) in &items {
                        out.push_str(&format!("    {detail}\n"));
                    }
                }
                false => {
                    let mut names: Vec<&str> = items.iter().map(|(p, _)| p.as_str()).collect();
                    names.dedup();
                    out.push_str(&format!(
                        "  {why} ({}): {}\n",
                        items.len(),
                        names.join(", ")
                    ));
                }
            }
        }
    }
    out
}

/// A package's open issue, as `check-unbuilt --update-issues` reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenIssue {
    pub package: String,
    /// The issue's project (`CentOS/Hyperscale/rpms/openssh`).
    pub project: String,
    pub iid: u64,
    pub web_url: String,
    pub description: String,
}

/// A change `check-unbuilt --update-issues` makes to an issue: its
/// not-built line from `old` to `new` (`None` for none), and the whole
/// description it then has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineChange {
    pub package: String,
    pub project: String,
    pub iid: u64,
    pub web_url: String,
    pub old: Option<String>,
    pub new: Option<String>,
    pub description: String,
}

/// The changes `outcome`, a check of `targets`, makes to `issues` (one
/// per package, the first given for each): a package gets the line for
/// its branches past the grace period, or loses it once none are. One
/// that cannot be told keeps its line as it is: none of its builds was
/// compared, or one could not be checked or has no commit recorded (its
/// branch may be behind). A build no branch holds, an old one usually,
/// is left out without that.
pub fn line_changes(
    outcome: &sandogasa_cbs::unbuilt::Outcome,
    targets: &[Target],
    issues: &[OpenIssue],
) -> Vec<LineChange> {
    use sandogasa_cbs::unbuilt;
    let untold = |p: &str| !unbuilt::told(outcome, targets, p);
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for issue in issues {
        let p = issue.package.as_str();
        if seen.contains(&p) || untold(p) {
            continue;
        }
        seen.push(p);
        let mine: Vec<unbuilt::Unbuilt> = outcome
            .unbuilt
            .iter()
            .filter(|u| u.package == p)
            .cloned()
            .collect();
        let new = unbuilt::body_line(&mine);
        let old = issue
            .description
            .lines()
            .find(|l| l.trim_start().starts_with(unbuilt::LINE))
            .map(|l| l.trim().to_string());
        if old == new {
            continue;
        }
        out.push(LineChange {
            package: issue.package.clone(),
            project: issue.project.clone(),
            iid: issue.iid,
            web_url: issue.web_url.clone(),
            description: unbuilt::with_body_line(&issue.description, new.as_deref()),
            old,
            new,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(id: i64, name: &str, release: &str) -> Build {
        Build {
            build_id: id,
            name: name.into(),
            version: "9.9p1".into(),
            release: release.into(),
            nvr: format!("{name}-9.9p1-{release}"),
        }
    }

    /// One repository where `rpm`'s build lacks one commit of `c9s-hs`.
    struct Repo;

    impl sandogasa_cbs::unbuilt::Repository for Repo {
        fn containing(&self, _: &Target, _: &str) -> Result<Vec<String>, String> {
            Ok(vec!["c9s-hs".into()])
        }
        fn since(
            &self,
            t: &Target,
            _: &str,
            _: &str,
        ) -> Result<Vec<sandogasa_gitlab::RepoCommit>, String> {
            match t.package.as_str() {
                "rpm" => Ok(vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "c1", "title": "Fix", "committed_date": "2026-11-01T00:00:00Z"
                    }))
                    .unwrap(),
                ]),
                _ => Ok(vec![]),
            }
        }
    }

    #[test]
    fn an_issue_gets_its_line_only_when_it_can_be_told() {
        let target = |p: &str| Target {
            package: p.into(),
            branches: vec!["c9s-hs".into()],
            nvr: format!("{p}-1-1.hs.el9"),
        };
        let cutoff = chrono::DateTime::parse_from_rfc3339("2026-11-17T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut outcome = sandogasa_cbs::unbuilt::check(
            &[target("rpm"), target("ethtool")],
            &[Some("a".into()), Some("b".into())],
            &Repo,
            cutoff,
        );
        // ethtool's old el8 build is on no branch, anaconda's only one
        // too, and qemu's current build records no commit.
        outcome
            .unplaced
            .push("ethtool: ethtool-5.16-1.hs.el8: none of c8s-hs holds it".into());
        outcome
            .unplaced
            .push("anaconda: anaconda-1-1.hs.el9: none of c9s-hs holds it".into());
        outcome.unsourced.push("qemu-1-1.hs.el9".into());
        let targets = [
            target("rpm"),
            target("ethtool"),
            Target {
                nvr: "ethtool-5.16-1.hs.el8".into(),
                ..target("ethtool")
            },
            target("anaconda"),
            target("qemu"),
        ];
        let issue = |p: &str, d: &str| OpenIssue {
            package: p.into(),
            project: format!("g/{p}"),
            iid: 1,
            web_url: format!("u/{p}"),
            description: d.into(),
        };
        let issues = [
            issue(
                "ethtool",
                "table\n\n- **Not built**: 2 commit(s) on `c9s-hs`",
            ),
            issue("ethtool", "a second issue, ignored"),
            issue("anaconda", "table\n\n- **Not built**: 1 commit(s)"),
            issue("qemu", "table\n\n- **Not built**: 120 commit(s)"),
            issue("rpm", "table"),
            issue("unchecked", "table\n\n- **Not built**: 1"),
        ];
        let changes = line_changes(&outcome, &targets, &issues);
        // ethtool is built now (its stale el8 build aside), so its line
        // goes; rpm gains one; anaconda and qemu cannot be told;
        // unchecked was not asked about.
        let summary: Vec<(&str, Option<&str>)> = changes
            .iter()
            .map(|c| (c.package.as_str(), c.new.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("ethtool", None),
                (
                    "rpm",
                    Some(
                        "- **Not built**: 1 commit(s) on `c9s-hs` since rpm-1-1.hs.el9, \
                         the newest from 2026-11-01"
                    )
                ),
            ]
        );
        assert_eq!(changes[0].description, "table");
        assert_eq!(changes[1].old, None);
        assert!(
            changes[1]
                .description
                .starts_with("table\n\n- **Not built**: 1")
        );
    }

    #[test]
    fn the_report_leads_with_the_branches_and_sums_up_the_rest() {
        let target = |p: &str| Target {
            package: p.into(),
            branches: vec!["c9s-hs".into()],
            nvr: format!("{p}-1-1.hs.el9"),
        };
        let cutoff = chrono::DateTime::parse_from_rfc3339("2026-11-17T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut outcome =
            sandogasa_cbs::unbuilt::check(&[target("rpm")], &[Some("a".into())], &Repo, cutoff);
        outcome
            .unplaced
            .push("anaconda: anaconda-1-1.hs.el8: none of c8s-hs holds it".into());
        outcome.unsourced.push("qemu-11.1.1-1.2.hs.el9".into());
        outcome.unsourced.push("qemu-11.1.1-1.2.hs+fb.el9".into());
        let text = report(&outcome, 4, 3, false, false);
        assert!(
            text.starts_with("1 branch(es) with merged commits not built:\n\n  PACKAGE"),
            "{text}"
        );
        assert!(
            text.contains("rpm c9s-hs, since a:\n  2026-11-01 c1 Fix\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "4 build(s): 1 branch(es) not built, 0 with commits from the last 3 day(s) only\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "Not compared, 3 build(s) (-v lists them):\n  no commit recorded (built from an \
                 uploaded SRPM?) (2): qemu\n  no branch holds the commit they were built from \
                 (1): anaconda\n"
            ),
            "{text}"
        );
        let verbose = report(&outcome, 4, 3, true, true);
        assert!(
            verbose.contains("    qemu-11.1.1-1.2.hs+fb.el9\n"),
            "{verbose}"
        );
        assert!(
            verbose.contains("    anaconda-1-1.hs.el8: none of c8s-hs holds it\n"),
            "{verbose}"
        );
        // A long list is cut, saying how to see it all.
        let mut many = outcome.clone();
        let first = many.unbuilt[0].commits[0].clone();
        many.unbuilt[0].commits = vec![first; 12];
        assert!(
            report(&many, 4, 3, false, false).contains("and 2 older (--all-commits lists them)\n")
        );
        assert!(!report(&many, 4, 3, true, false).contains("older"));
        // A title that happens to end in "older" gets no hint.
        let mut titled = outcome.clone();
        titled.unbuilt[0].commits[0].title = "Drop support for anything older".into();
        assert!(!report(&titled, 4, 3, false, false).contains("--all-commits"));
        // Nothing found: the summary alone.
        let none = sandogasa_cbs::unbuilt::Outcome::default();
        assert_eq!(
            report(&none, 0, 3, false, false),
            "0 build(s): 0 branch(es) not built, 0 with commits from the last 3 day(s) only\n"
        );
    }

    #[test]
    fn only_a_collections_tags_count_as_built() {
        assert!(counts_as_built("hyperscale9s-packages-facebook-testing"));
        assert!(counts_as_built("hyperscale10s-packages-main-candidate"));
        assert!(counts_as_built("hyperscale9-packages-main-release"));
        assert!(!counts_as_built("hyperscale9s-packages-main-el9s-build"));
        assert!(!counts_as_built("trash"));
        assert!(!counts_as_built(
            "proposed_updates10s-packages-main-release"
        ));
    }

    #[test]
    fn a_dist_tag_names_its_branch() {
        assert_eq!(
            branches_for(9, "hs+fb"),
            ["c9s-hs+fb", "c9s-hs", "c9s-sig-hyperscale"]
        );
        assert_eq!(
            branches_for(10, "hsx"),
            ["c10s-hsx", "c10s-hs", "c10s-sig-hyperscale"]
        );
        assert_eq!(branches_for(10, "hs"), ["c10s-hs", "c10s-sig-hyperscale"]);
    }

    #[test]
    fn the_newest_build_per_branch_is_checked() {
        let builds = [
            build(10, "openssh", "16.1.hs+fb.el9"),
            // In testing, newer: counts as built.
            build(12, "openssh", "17.1.hs+fb.el9"),
            build(11, "openssh", "30.1.hs+fb.el10"),
            build(5, "openssh", "16.1.hs.el9"),
            build(13, "other", "1.hs.el9"),
            // Not a Hyperscale dist tag.
            build(14, "openssh", "1.el9"),
        ];
        let got: Vec<(String, Vec<String>)> = targets("openssh", builds)
            .into_iter()
            .map(|t| (t.nvr, t.branches))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "openssh-9.9p1-16.1.hs.el9".to_string(),
                    vec!["c9s-hs".to_string(), "c9s-sig-hyperscale".to_string()]
                ),
                (
                    "openssh-9.9p1-17.1.hs+fb.el9".to_string(),
                    branches_for(9, "hs+fb")
                ),
                (
                    "openssh-9.9p1-30.1.hs+fb.el10".to_string(),
                    branches_for(10, "hs+fb")
                ),
            ]
        );
    }
}
