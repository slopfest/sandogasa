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

/// Extract the new version from a release-monitoring bug summary
/// of the form `"<component>-<version> is available"`.
pub fn extract_new_version(summary: &str, component: &str) -> Option<String> {
    let body = summary.trim().strip_suffix(" is available")?;
    // The component prefix is followed by a single `-`; the rest is
    // the version (which may itself contain `-`, e.g. `1.0-r2707`).
    let rest = body.strip_prefix(component)?;
    let version = rest.strip_prefix('-').unwrap_or(rest);
    (!version.is_empty()).then(|| version.to_string())
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

    // Update request: FutureFeature keyword + "is available" summary.
    if bug.keywords.iter().any(|k| k == "FutureFeature") {
        let component = bug.component.first().map(|s| s.as_str()).unwrap_or("");
        if !component.is_empty()
            && bug.summary.starts_with(component)
            && bug.summary.contains("is available")
        {
            return BugKind::Update;
        }
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
