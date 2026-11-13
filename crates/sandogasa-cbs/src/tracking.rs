// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A SIG build's tracking issue, and what is said on it when the build
//! moves (#144).
//!
//! The Proposed Updates SIG tracks each update in an issue in the
//! package's SIG repo, filed by cpu-sig-tracker: labelled with the
//! release (`c10s`), naming the RHEL issue the merge request resolves.
//! Those two are what the SIG's branch is named after
//! (`c10s-proposed-RHEL-306261`), so a build of the branch finds its
//! issue without asking anything else.
//!
//! A Hyperscale package has one open `rfe::new-version` issue at a
//! time, filed by hs-relmon and titled `<package>-<version> is
//! available`; a build at or past that version is what it asks for.
//!
//! What is said is a comment, ending in a marker a person does not see,
//! so a rerun can tell it was said already; and a scoped label, one of
//! `stage::testing` and `stage::release`, so the SIG's issue lists show
//! how far each update has got. This module only decides; the tools
//! read and write GitLab.

use crate::Stage;

/// Whether an issue with `labels` and `description` tracks the Proposed
/// Update built from the SIG branch `branch`
/// (`c10s-proposed-RHEL-306261`): it carries the release's label, and
/// its `- **JIRA**: [RHEL-…](…)` line, as cpu-sig-tracker writes it,
/// names the RHEL issue. A key mentioned anywhere else, such as another
/// update's "see also", does not count.
pub fn tracks_proposed_branch(labels: &[String], description: &str, branch: &str) -> bool {
    let Some((release, key)) = branch.split_once("-proposed-") else {
        return false;
    };
    let link = format!("[{key}]");
    let names_key = description.lines().any(|l| {
        l.trim_start()
            .strip_prefix("- **JIRA**:")
            .is_some_and(|rest| rest.contains(&link))
    });
    labels.iter().any(|l| l == release) && names_key
}

/// The version a Hyperscale `rfe::new-version` issue asks for, from its
/// title as hs-relmon writes it: `7.1` of `ethtool-7.1 is available`.
pub fn requested_version<'a>(title: &'a str, package: &str) -> Option<&'a str> {
    title
        .strip_suffix(" is available")?
        .strip_prefix(package)?
        .strip_prefix('-')
        // A version, not the rest of a longer name: `rust-foo` is not
        // asked for by `rust-foo-bar-1.2 is available`.
        .filter(|v| v.starts_with(|c: char| c.is_ascii_digit()))
}

/// The release of the SIG branch `branch` (`c10s` of
/// `c10s-proposed-RHEL-306261`), the label its tracking issue carries.
pub fn proposed_release(branch: &str) -> Option<&str> {
    branch.split_once("-proposed-").map(|(release, _)| release)
}

/// The marker ending the comment for `nvr` reaching `stage`.
pub fn stage_marker(nvr: &str, stage: Stage) -> String {
    format!("<!-- sandogasa: {nvr} reached {} -->", stage.as_str())
}

/// Whether one of `notes` (comment bodies) says `nvr` reached `stage`
/// already.
pub fn stage_noted<'a>(mut notes: impl Iterator<Item = &'a str>, nvr: &str, stage: Stage) -> bool {
    let marker = stage_marker(nvr, stage);
    notes.any(|n| n.contains(&marker))
}

/// What a build was made from, against its merge request's head.
#[derive(Debug, Clone, PartialEq)]
pub enum Provenance {
    /// Not known: no commit in the build's source, or no head to compare.
    Unknown,
    /// Exactly the merge request's head (its short id).
    Head(String),
    /// The head plus SIG-only commits on top, each `<short id> <subject>`.
    Carried(String, Vec<String>),
    /// A commit that does not contain the merge request's current head.
    Apart(String),
}

/// The URL on an issue's `- **MR**: [title](url)` line, as
/// cpu-sig-tracker writes it.
pub fn tracked_mr_url(description: &str) -> Option<&str> {
    description.lines().find_map(|l| {
        let rest = l.trim_start().strip_prefix("- **MR**:")?;
        // The link's target, not a `](` inside the title.
        let at = rest.find("](http")? + 2;
        rest[at..].split(')').next().filter(|u| !u.is_empty())
    })
}

/// The comment for `nvr` reaching `stage`, by being tagged into `tag`,
/// and for a release, untagged from `untagged` (`-testing`), if it was;
/// with what it was built from, so SIG-only commits that never go to
/// CentOS Stream are on record where the update is tracked.
pub fn stage_note(
    nvr: &str,
    stage: Stage,
    tag: &str,
    untagged: Option<&str>,
    built_from: &Provenance,
) -> String {
    let what = match (stage, untagged) {
        (Stage::Release, Some(testing)) => {
            format!("`{nvr}` is released: tagged into `{tag}` and untagged from `{testing}`.")
        }
        (Stage::Release, None) => format!("`{nvr}` is released: tagged into `{tag}`."),
        (Stage::Testing, _) => format!("`{nvr}` is ready for testing: tagged into `{tag}`."),
        (Stage::Candidate, _) => format!("`{nvr}` is built: tagged into `{tag}`."),
    };
    let from = match built_from {
        Provenance::Unknown => String::new(),
        Provenance::Head(commit) => {
            format!("\n\nBuilt from `{commit}`, the merge request's head.")
        }
        Provenance::Carried(commit, carried) => {
            let mut s = format!(
                "\n\nBuilt from `{commit}`: the merge request's head and these SIG-only \
                 commits, which are not part of the merge request:\n"
            );
            for line in carried {
                let (id, subject) = line.split_once(' ').unwrap_or((line, ""));
                s.push_str(&format!("\n- `{id}` {subject}"));
            }
            s
        }
        Provenance::Apart(commit) => format!(
            "\n\nBuilt from `{commit}`, which does not contain the merge request's current head."
        ),
    };
    format!("{what}{from}\n\n{}", stage_marker(nvr, stage))
}

/// The scoped label for a build at `stage`, if the stage has one.
pub fn stage_label(stage: Stage) -> Option<&'static str> {
    match stage {
        Stage::Candidate => None,
        Stage::Testing => Some("stage::testing"),
        Stage::Release => Some("stage::release"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESCRIPTION: &str = "\
- **MR**: [Update git to 2.52.1](https://gitlab.com/redhat/centos-stream/rpms/git/-/merge_requests/32) — opened
- **JIRA**: [RHEL-306261](https://redhat.atlassian.net/browse/RHEL-306261) — New
- **Release**: c10s
";

    fn labels(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_proposed_branch_finds_its_issue_by_release_and_rhel_issue() {
        let on = labels(&["c10s", "cpu-sig-tracker", "priority::major"]);
        assert!(tracks_proposed_branch(
            &on,
            DESCRIPTION,
            "c10s-proposed-RHEL-306261"
        ));
        // The other release's issue, another RHEL issue, a key that is
        // only the start of this one, or no SIG branch at all.
        assert!(!tracks_proposed_branch(
            &labels(&["c9s"]),
            DESCRIPTION,
            "c10s-proposed-RHEL-306261"
        ));
        assert!(!tracks_proposed_branch(
            &on,
            DESCRIPTION,
            "c10s-proposed-RHEL-306262"
        ));
        assert!(!tracks_proposed_branch(
            &on,
            DESCRIPTION,
            "c10s-proposed-RHEL-30626"
        ));
        assert!(!tracks_proposed_branch(&on, DESCRIPTION, "c10s"));
        // Named only in passing, not as the issue's own JIRA line.
        let in_passing = "- **JIRA**: [RHEL-1](j)\nSee also RHEL-306261 and [RHEL-306261](x).";
        assert!(!tracks_proposed_branch(
            &on,
            in_passing,
            "c10s-proposed-RHEL-306261"
        ));
        assert_eq!(proposed_release("c10s-proposed-RHEL-306261"), Some("c10s"));
        assert_eq!(proposed_release("c10s"), None);
    }

    #[test]
    fn a_comment_says_where_the_build_went_and_is_known_again() {
        let nvr = "git-2.52.1-1~proposed.el10";
        let testing = stage_note(
            nvr,
            Stage::Testing,
            "proposed_updates10s-packages-main-testing",
            None,
            &Provenance::Unknown,
        );
        assert!(testing.starts_with(&format!(
            "`{nvr}` is ready for testing: tagged into `proposed_updates10s-packages-main-testing`."
        )));
        let release = stage_note(
            nvr,
            Stage::Release,
            "proposed_updates10s-packages-main-release",
            Some("proposed_updates10s-packages-main-testing"),
            &Provenance::Unknown,
        );
        assert!(release.contains("untagged from `proposed_updates10s-packages-main-testing`"));
        assert!(
            stage_note(nvr, Stage::Release, "r", None, &Provenance::Unknown)
                .ends_with(&stage_marker(nvr, Stage::Release))
        );
        assert!(
            stage_note(nvr, Stage::Candidate, "c", None, &Provenance::Unknown).contains("is built")
        );
        let notes = ["set status to **In progress**", testing.as_str()];
        assert!(stage_noted(notes.into_iter(), nvr, Stage::Testing));
        assert!(!stage_noted(notes.into_iter(), nvr, Stage::Release));
        assert!(!stage_noted(
            notes.into_iter(),
            "git-2.52.1-2~proposed.el10",
            Stage::Testing
        ));
    }

    #[test]
    fn testing_and_release_have_scoped_labels() {
        assert_eq!(stage_label(Stage::Testing), Some("stage::testing"));
        assert_eq!(stage_label(Stage::Release), Some("stage::release"));
        assert_eq!(stage_label(Stage::Candidate), None);
    }

    #[test]
    fn a_comment_says_what_the_build_was_made_from() {
        let nvr = "git-2.52.1-1~proposed.el9";
        let note = |from: &Provenance| stage_note(nvr, Stage::Testing, "t", None, from);
        assert!(
            note(&Provenance::Head("7315dba".into()))
                .contains("Built from `7315dba`, the merge request's head.")
        );
        let carried = note(&Provenance::Carried(
            "fb3be7c".into(),
            vec!["fb3be7c Pin Ruby 3 for CBS's el9 buildroot".into()],
        ));
        assert!(
            carried.contains("SIG-only commits, which are not part of the merge request"),
            "{carried}"
        );
        assert!(
            carried.contains("\n- `fb3be7c` Pin Ruby 3 for CBS's el9 buildroot\n"),
            "{carried}"
        );
        assert!(carried.ends_with(&stage_marker(nvr, Stage::Testing)));
        assert!(
            note(&Provenance::Apart("0123456".into()))
                .contains("does not contain the merge request's current head")
        );
        assert!(!note(&Provenance::Unknown).contains("Built from"));
    }

    #[test]
    fn the_merge_request_is_read_from_the_issues_mr_line() {
        assert_eq!(
            tracked_mr_url(DESCRIPTION),
            Some("https://gitlab.com/redhat/centos-stream/rpms/git/-/merge_requests/32")
        );
        assert_eq!(tracked_mr_url("- **JIRA**: [RHEL-1](j)"), None);
        assert_eq!(
            tracked_mr_url(
                "- **MR**: [Backport [CVE-1](x) fix](https://gitlab.com/r/-/merge_requests/32) — opened"
            ),
            Some("https://gitlab.com/r/-/merge_requests/32")
        );
    }

    #[test]
    fn a_hyperscale_issue_asks_for_a_version_in_its_title() {
        assert_eq!(
            requested_version("ethtool-7.1 is available", "ethtool"),
            Some("7.1")
        );
        assert_eq!(
            requested_version("rust-foo-bar-1.2.3 is available", "rust-foo-bar"),
            Some("1.2.3")
        );
        assert_eq!(requested_version("ethtool-7.1 is available", "eth"), None);
        assert_eq!(requested_version("ethtool- is available", "ethtool"), None);
        assert_eq!(requested_version("Rebase ethtool", "ethtool"), None);
        assert_eq!(
            requested_version("rust-foo-bar-1.2 is available", "rust-foo"),
            None
        );
    }
}
