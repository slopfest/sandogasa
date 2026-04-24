// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Unified report model and formatting.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::Serialize;

use crate::{bodhi, bugzilla, koji};

/// Full activity report.
#[derive(Debug, Serialize)]
pub struct Report {
    /// FAS username (if filtered).
    pub user: Option<String>,
    /// Domain name.
    pub domain: String,
    /// Reporting period start (inclusive).
    pub since: NaiveDate,
    /// Reporting period end (inclusive).
    pub until: NaiveDate,
    /// Bugzilla section (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bugzilla: Option<bugzilla::BugzillaReport>,
    /// Bodhi section (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bodhi: Option<bodhi::BodhiReport>,
    /// Koji CBS section per domain (empty if not applicable). Keyed
    /// by the CLI domain name so multi-domain runs render each
    /// domain's Koji activity as its own section.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub koji: BTreeMap<String, koji::KojiReport>,
}

/// Format the full report as Markdown.
pub fn format_markdown(
    report: &Report,
    detailed: bool,
    groups: &BTreeMap<String, crate::config::GroupConfig>,
) -> String {
    let mut out = String::new();

    // Header.
    out.push_str(&format!("# Activity Report: {}\n\n", report.domain));
    if let Some(ref user) = report.user {
        out.push_str(&format!("**User:** {user}\n"));
    }
    out.push_str(&format!(
        "**Period:** {} to {}\n\n",
        report.since, report.until
    ));

    // Bugzilla section.
    if let Some(ref bz_report) = report.bugzilla {
        out.push_str(&bugzilla::format_markdown(bz_report, detailed));
    }

    // Bodhi section.
    if let Some(ref bodhi_report) = report.bodhi {
        out.push_str(&bodhi::format_markdown(bodhi_report, detailed));
    }

    // Koji section(s). One per domain — labeled when multiple,
    // bare when a single domain reports Koji activity.
    let multi_koji = report.koji.len() > 1;
    for (domain_name, koji_report) in &report.koji {
        let suffix = multi_koji.then_some(domain_name.as_str());
        out.push_str(&koji::format_markdown(
            koji_report,
            detailed,
            groups,
            suffix,
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn format_header() {
        let report = Report {
            user: Some("testuser".to_string()),
            domain: "fedora".to_string(),
            since: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            until: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            bugzilla: None,
            bodhi: None,
            koji: BTreeMap::new(),
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        assert!(md.contains("# Activity Report: fedora"));
        assert!(md.contains("**User:** testuser"));
        assert!(md.contains("**Period:** 2026-01-01 to 2026-03-31"));
    }

    #[test]
    fn format_koji_single_domain_leaves_heading_bare() {
        let mut koji = BTreeMap::new();
        koji.insert(
            "hyperscale".to_string(),
            koji::KojiReport {
                packages: BTreeMap::new(),
            },
        );
        let report = Report {
            user: None,
            domain: "hyperscale".to_string(),
            since: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            until: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            bugzilla: None,
            bodhi: None,
            koji,
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        // With a single domain, no suffix is added. Empty packages
        // suppresses the heading entirely, so assert via absence.
        assert!(!md.contains("## Koji CBS ("));
    }

    #[test]
    fn format_koji_multi_domain_labels_each() {
        let mut koji = BTreeMap::new();
        let mut hs_pkgs = BTreeMap::new();
        hs_pkgs.insert(
            "foo".to_string(),
            koji::PackageEntry {
                name: "x".to_string(),
                change: koji::ChangeKind::New,
                versions: BTreeMap::new(),
                owner: "u".to_string(),
            },
        );
        koji.insert(
            "hyperscale".to_string(),
            koji::KojiReport { packages: hs_pkgs },
        );
        let mut pu_pkgs = BTreeMap::new();
        pu_pkgs.insert(
            "bar".to_string(),
            koji::PackageEntry {
                name: "x".to_string(),
                change: koji::ChangeKind::New,
                versions: BTreeMap::new(),
                owner: "u".to_string(),
            },
        );
        koji.insert(
            "proposed_updates".to_string(),
            koji::KojiReport { packages: pu_pkgs },
        );
        let report = Report {
            user: None,
            domain: "hyperscale + proposed_updates".to_string(),
            since: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            until: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            bugzilla: None,
            bodhi: None,
            koji,
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        assert!(md.contains("## Koji CBS (hyperscale)"));
        assert!(md.contains("## Koji CBS (proposed_updates)"));
    }

    #[test]
    fn format_empty_report() {
        let report = Report {
            user: None,
            domain: "test".to_string(),
            since: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            until: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            bugzilla: None,
            bodhi: None,
            koji: BTreeMap::new(),
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        assert!(md.contains("# Activity Report: test"));
        assert!(!md.contains("**User:**"));
    }
}
