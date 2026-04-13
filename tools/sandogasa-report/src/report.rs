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
    /// Koji CBS section (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub koji: Option<koji::KojiReport>,
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

    // Koji section.
    if let Some(ref koji_report) = report.koji {
        out.push_str(&koji::format_markdown(koji_report, detailed, groups, None));
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
            koji: None,
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        assert!(md.contains("# Activity Report: fedora"));
        assert!(md.contains("**User:** testuser"));
        assert!(md.contains("**Period:** 2026-01-01 to 2026-03-31"));
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
            koji: None,
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        assert!(md.contains("# Activity Report: test"));
        assert!(!md.contains("**User:**"));
    }
}
