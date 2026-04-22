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

    fn make_bug(summary: &str, component: &str, keywords: &[&str], blocks: &[u64]) -> Bug {
        Bug {
            id: 1,
            summary: summary.to_string(),
            status: "NEW".to_string(),
            resolution: String::new(),
            product: "Fedora".to_string(),
            component: vec![component.to_string()],
            severity: String::new(),
            priority: String::new(),
            assigned_to: String::new(),
            creator: String::new(),
            creation_time: chrono::Utc::now(),
            last_change_time: chrono::Utc::now(),
            keywords: keywords.iter().map(|s| s.to_string()).collect(),
            alias: vec![],
            depends_on: vec![],
            blocks: blocks.to_vec(),
            see_also: vec![],
            cc: vec![],
            flags: vec![],
            version: vec![],
            cf_fixed_in: String::new(),
        }
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
