// SPDX-License-Identifier: MPL-2.0

//! Unified report model and formatting.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::Serialize;

use crate::{bugzilla, koji};

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
    /// Koji CBS section (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub koji: Option<koji::KojiReport>,
    // TODO: pub bodhi: Option<BodhiSection>,
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

    // Koji section.
    if let Some(ref koji_report) = report.koji {
        out.push_str(&koji::format_markdown(koji_report, detailed, groups, None));
    }

    // TODO: Bodhi section

    out
}
