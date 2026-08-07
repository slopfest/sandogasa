// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bugzilla-specific bug classification.

use std::collections::HashSet;

use sandogasa_bugzilla::BzClient;
use sandogasa_bugzilla::models::Bug;

use crate::BugKind;

/// Tracker bug IDs used for FTBFS / FTI classification. Populated
/// by [`lookup_trackers`].
#[derive(Debug, Clone, Default)]
pub struct TrackerIds {
    pub ftbfs: HashSet<u64>,
    pub fti: HashSet<u64>,
}

/// Extract the new version from a release-monitoring bug summary of
/// the form `"<component>-<version> is available"`.
///
/// The match is anchored on `component`: the summary must continue
/// with a `-` and then a digit, so a component is not taken for the
/// prefix of a longer package name — `rust-ctor` does not answer for
/// `rust-ctor-proc-macro-0.0.13`, whose remainder begins a name
/// segment rather than a version.
///
/// Anchoring is what makes this reliable, and why there is no
/// version of this that parses a summary without being told the
/// package. `rust-md-5-0.10.6` splits as `rust-md-5` + `0.10.6` or
/// `rust-md` + `5-0.10.6` with equal justification — both are real
/// Fedora package names — and only the caller's candidate settles
/// it. Splitting from the right the way [`sandogasa_koji::parse_nvr`]
/// does is no help either: a summary carries no release field, and a
/// version may itself contain a `-` (`0.5a1.dev-r2707`).
pub fn extract_new_version(summary: &str, component: &str) -> Option<String> {
    let body = summary.trim().strip_suffix(" is available")?;
    let version = body.strip_prefix(component)?.strip_prefix('-')?;
    version
        .starts_with(|c: char| c.is_ascii_digit())
        .then(|| version.to_string())
}

/// Extract the package name from a package-review bug summary of the
/// form `"Review Request: <package> - <description>"`. The package
/// name is the first word after the prefix; a summary that does not
/// start with it is not a review request.
pub fn review_request_package(summary: &str) -> Option<String> {
    const PREFIX: &str = "review request:";
    let summary = summary.trim();
    let (prefix, rest) = summary.split_at_checked(PREFIX.len())?;
    prefix.eq_ignore_ascii_case(PREFIX).then_some(())?;
    Some(rest.split_whitespace().next()?.to_string())
}

/// Classify a Bugzilla bug into a [`BugKind`].
///
/// Returns `Review` for bugs filed against the "Package Review"
/// component, regardless of content.
pub fn classify(bug: &Bug, trackers: &TrackerIds) -> BugKind {
    if bug.component.iter().any(|c| c == "Package Review") {
        return BugKind::Review;
    }

    // FTBFS / FTI: check if the bug blocks a known tracker.
    if bug.blocks.iter().any(|id| trackers.ftbfs.contains(id)) {
        return BugKind::Ftbfs;
    }
    if bug.blocks.iter().any(|id| trackers.fti.contains(id)) {
        return BugKind::Fti;
    }

    // Security: summary starts with CVE- or SecurityTracking keyword.
    if bug.summary.starts_with("CVE-")
        || bug
            .keywords
            .iter()
            .any(|k| k == "SecurityTracking" || k == "Security")
    {
        return BugKind::Security;
    }

    // Update request: FutureFeature keyword, and a summary naming
    // this component and a new version. Going through
    // `extract_new_version` rather than a bare `starts_with` anchors
    // the component the same way the version comparison does, so a
    // bug against `rust-ctor` is not labelled an update request on
    // the strength of a `rust-ctor-proc-macro-0.0.13` summary.
    if bug.keywords.iter().any(|k| k == "FutureFeature")
        && let Some(component) = bug.component.first()
        && extract_new_version(&bug.summary, component).is_some()
    {
        return BugKind::Update;
    }

    // Branch request.
    if bug.summary.to_lowercase().contains("branch") {
        return BugKind::Branch;
    }

    BugKind::Other
}

/// Look up FTBFS and FTI tracker bug IDs for the given Fedora
/// versions. Always includes the permanent Rawhide trackers.
pub async fn lookup_trackers(bz: &BzClient, versions: &[u32], verbose: bool) -> TrackerIds {
    let mut ftbfs = HashSet::new();
    let mut fti = HashSet::new();

    let mut aliases = vec![
        "RAWHIDEFTBFS".to_string(),
        "RAWHIDEFailsToInstall".to_string(),
    ];
    for ver in versions {
        aliases.push(format!("F{ver}FTBFS"));
        aliases.push(format!("F{ver}FailsToInstall"));
    }

    if verbose {
        eprintln!("[bugclass] looking up FTBFS/FTI tracker bugs");
    }

    let alias_params: Vec<String> = aliases.iter().map(|a| format!("alias={a}")).collect();
    let query = alias_params.join("&");
    if let Ok(bugs) = bz.search(&query, 0).await {
        for bug in &bugs {
            for alias in &bug.alias {
                if alias.ends_with("FTBFS") {
                    ftbfs.insert(bug.id);
                } else if alias.ends_with("FailsToInstall") {
                    fti.insert(bug.id);
                }
            }
        }
    }

    if verbose {
        eprintln!(
            "[bugclass] found {} FTBFS and {} FTI tracker(s)",
            ftbfs.len(),
            fti.len()
        );
    }

    TrackerIds { ftbfs, fti }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct a `Bug` via serde — `Bug` is `#[non_exhaustive]`,
    /// so literal construction is reserved to its own crate.
    fn make_bug(summary: &str, component: &str, keywords: &[&str], blocks: &[u64]) -> Bug {
        serde_json::from_value(serde_json::json!({
            "id": 1,
            "summary": summary,
            "status": "NEW",
            "resolution": "",
            "product": "Fedora",
            "component": [component],
            "severity": "",
            "priority": "",
            "assigned_to": "",
            "creator": "",
            "creation_time": "2026-01-01T00:00:00Z",
            "last_change_time": "2026-01-01T00:00:00Z",
            "keywords": keywords,
            "blocks": blocks,
        }))
        .unwrap()
    }

    #[test]
    fn extract_new_version_handles_real_summaries() {
        assert_eq!(
            extract_new_version(
                "transmission-remote-cli-1.7.1 is available",
                "transmission-remote-cli"
            )
            .as_deref(),
            Some("1.7.1")
        );
        // Version containing a dash is preserved after the first one.
        assert_eq!(
            extract_new_version(
                "python-peak-rules-0.5a1.dev-r2707 is available",
                "python-peak-rules"
            )
            .as_deref(),
            Some("0.5a1.dev-r2707")
        );
    }

    #[test]
    fn extract_new_version_rejects_unrecognized() {
        assert_eq!(extract_new_version("something unrelated", "foo"), None);
        assert_eq!(
            extract_new_version("otherpkg-1.0 is available", "foo"),
            None
        );
    }

    #[test]
    fn extract_new_version_handles_names_ending_in_a_digit() {
        // rust-md-5, rust-sha-1, rust-utf-8 and friends are real
        // packages: the last name segment is a digit, so the version
        // boundary cannot be found without knowing the name.
        assert_eq!(
            extract_new_version("rust-md-5-0.10.6 is available", "rust-md-5").as_deref(),
            Some("0.10.6")
        );
        assert_eq!(
            extract_new_version("rust-sha-1-0.10.1 is available", "rust-sha-1").as_deref(),
            Some("0.10.1")
        );
    }

    #[test]
    fn extract_new_version_rejects_a_longer_package_name() {
        // `rust-ctor` is a prefix of `rust-ctor-proc-macro`, but the
        // bug is about the latter: what follows the prefix is another
        // name segment, not a version.
        assert_eq!(
            extract_new_version("rust-ctor-proc-macro-0.0.13 is available", "rust-ctor"),
            None
        );
        // The package it really names still resolves.
        assert_eq!(
            extract_new_version(
                "rust-ctor-proc-macro-0.0.13 is available",
                "rust-ctor-proc-macro"
            )
            .as_deref(),
            Some("0.0.13")
        );
        // A name that merely starts the same is not a match either.
        assert_eq!(
            extract_new_version("rust-ctor2-1.0 is available", "rust-ctor"),
            None
        );
    }

    #[test]
    fn review_request_package_handles_real_summaries() {
        // Package names contain dashes themselves, so the split is on
        // whitespace, not on the description separator.
        assert_eq!(
            review_request_package(
                "Review Request: rust-linktime-proc-macro - Proc-macro helpers \
                 for linktime crates"
            )
            .as_deref(),
            Some("rust-linktime-proc-macro")
        );
        // No description, an em dash separator, and lowercased prefix.
        assert_eq!(
            review_request_package("Review Request: rust-macrotest").as_deref(),
            Some("rust-macrotest")
        );
        assert_eq!(
            review_request_package("Review request: python-foo — a thing").as_deref(),
            Some("python-foo")
        );
    }

    #[test]
    fn review_request_package_rejects_other_summaries() {
        assert_eq!(review_request_package("rust-foo-1.0 is available"), None);
        assert_eq!(review_request_package("Review Request:"), None);
        assert_eq!(review_request_package("short"), None);
    }

    #[test]
    fn classify_review() {
        let trackers = TrackerIds::default();
        let bug = make_bug("Review Request: rust-foo", "Package Review", &[], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Review);
    }

    #[test]
    fn classify_security_by_summary() {
        let trackers = TrackerIds::default();
        let bug = make_bug("CVE-2026-1234 foo: buffer overflow", "foo", &[], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Security);
    }

    #[test]
    fn classify_security_by_keyword() {
        let trackers = TrackerIds::default();
        let bug = make_bug("foo: buffer overflow", "foo", &["SecurityTracking"], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Security);
    }

    #[test]
    fn classify_update_request() {
        let trackers = TrackerIds::default();
        let bug = make_bug("fish-4.0 is available", "fish", &["FutureFeature"], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Update);
        // A name that merely ends in a digit is still this component's
        // update request.
        let bug = make_bug(
            "rust-md-5-0.10.6 is available",
            "rust-md-5",
            &["FutureFeature"],
            &[],
        );
        assert_eq!(classify(&bug, &trackers), BugKind::Update);
    }

    #[test]
    fn classify_update_request_is_anchored_on_the_component() {
        let trackers = TrackerIds::default();
        // The summary is about rust-ctor-proc-macro, so this is not
        // an update request for rust-ctor, whose name merely prefixes
        // it.
        let bug = make_bug(
            "rust-ctor-proc-macro-0.0.13 is available",
            "rust-ctor",
            &["FutureFeature"],
            &[],
        );
        assert_eq!(classify(&bug, &trackers), BugKind::Other);
    }

    #[test]
    fn classify_branch_request() {
        let trackers = TrackerIds::default();
        let bug = make_bug("Please branch rust-foo for epel10", "foo", &[], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Branch);
    }

    #[test]
    fn classify_ftbfs() {
        let trackers = TrackerIds {
            ftbfs: HashSet::from([999]),
            fti: HashSet::new(),
        };
        let bug = make_bug("foo FTBFS in rawhide", "foo", &[], &[999]);
        assert_eq!(classify(&bug, &trackers), BugKind::Ftbfs);
    }

    #[test]
    fn classify_fti() {
        let trackers = TrackerIds {
            ftbfs: HashSet::new(),
            fti: HashSet::from([888]),
        };
        let bug = make_bug("foo fails to install", "foo", &[], &[888]);
        assert_eq!(classify(&bug, &trackers), BugKind::Fti);
    }

    #[test]
    fn classify_other() {
        let trackers = TrackerIds::default();
        let bug = make_bug("foo crashes on startup", "foo", &[], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Other);
    }
}
