// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Unified report model and formatting.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::Serialize;

use crate::{bodhi, bugzilla, gitlab, koji};

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

    /// GitLab section per domain (empty if not applicable). Keyed
    /// by CLI domain name; only domains with `[domains.X.gitlab]`
    /// in the config appear.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub gitlab: BTreeMap<String, gitlab::GitlabReport>,
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
    // Collect per-domain GitLab username overrides — those whose
    // GitLab user differs from the CLI user, grouped by alias so
    // one override spanning several domains appears on one line.
    let cli_user = report.user.as_deref();
    let mut aliases: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (domain, gl) in &report.gitlab {
        if Some(gl.user.as_str()) != cli_user {
            aliases
                .entry(gl.user.as_str())
                .or_default()
                .push(domain.as_str());
        }
    }
    match (report.user.as_deref(), aliases.is_empty()) {
        // No aliases: keep the compact single-line form.
        (Some(user), true) => out.push_str(&format!("**User:** `{user}`\n")),
        // CLI user plus aliases: render as a bulleted list so
        // each identity stands alone. Usernames are backticked
        // for consistency — they're identifiers, not prose.
        (Some(user), false) => {
            out.push_str("**User:**\n");
            out.push_str(&format!("  - `{user}`\n"));
            for (alias, domains) in &aliases {
                out.push_str(&format!("  - `{alias}` (GitLab: {})\n", domains.join(", ")));
            }
            out.push('\n');
        }
        // No CLI user but config-declared aliases (rare — e.g.
        // running with per-domain users only).
        (None, false) => {
            out.push_str("**User:**\n");
            for (alias, domains) in &aliases {
                out.push_str(&format!("  - `{alias}` (GitLab: {})\n", domains.join(", ")));
            }
            out.push('\n');
        }
        (None, true) => {}
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

    // GitLab section(s). Same pattern as Koji — labeled per-domain
    // when more than one has GitLab activity configured.
    let multi_gitlab = report.gitlab.len() > 1;
    for (domain_name, gl_report) in &report.gitlab {
        let suffix = multi_gitlab.then_some(domain_name.as_str());
        out.push_str(&gitlab::format_markdown(gl_report, detailed, suffix));
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
            gitlab: BTreeMap::new(),
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        assert!(md.contains("# Activity Report: fedora"));
        assert!(md.contains("**User:** `testuser`"));
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
            gitlab: BTreeMap::new(),
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
            gitlab: BTreeMap::new(),
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        assert!(md.contains("## Koji CBS (hyperscale)"));
        assert!(md.contains("## Koji CBS (proposed_updates)"));
    }

    #[test]
    fn format_header_lists_gitlab_aliases_when_different() {
        let mut gitlab = BTreeMap::new();
        gitlab.insert(
            "hyperscale".to_string(),
            gitlab::GitlabReport {
                instance: "https://gitlab.com".into(),
                user: "michel-slm".into(),
                ..Default::default()
            },
        );
        gitlab.insert(
            "proposed-updates".to_string(),
            gitlab::GitlabReport {
                instance: "https://gitlab.com".into(),
                user: "michel-slm".into(),
                ..Default::default()
            },
        );
        gitlab.insert(
            "debian".to_string(),
            gitlab::GitlabReport {
                instance: "https://salsa.debian.org".into(),
                user: "michel".into(),
                ..Default::default()
            },
        );
        let report = Report {
            user: Some("salimma".to_string()),
            domain: "hyperscale + proposed-updates + debian".to_string(),
            since: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            until: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            bugzilla: None,
            bodhi: None,
            koji: BTreeMap::new(),
            gitlab,
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        // List form: User heading followed by CLI user and one
        // bullet per distinct alias; the michel-slm line lists
        // both domains that share it. All usernames are backticked.
        assert!(md.contains("**User:**\n  - `salimma`\n"));
        assert!(md.contains("  - `michel-slm` (GitLab: hyperscale, proposed-updates)"));
        assert!(md.contains("  - `michel` (GitLab: debian)"));
        // Blank line separates the identity block from Period.
        assert!(md.contains("proposed-updates)\n\n**Period:**"));
    }

    #[test]
    fn format_header_omits_gitlab_line_when_users_match() {
        let mut gitlab = BTreeMap::new();
        gitlab.insert(
            "hyperscale".to_string(),
            gitlab::GitlabReport {
                instance: "https://gitlab.com".into(),
                user: "salimma".into(),
                ..Default::default()
            },
        );
        let report = Report {
            user: Some("salimma".to_string()),
            domain: "hyperscale".to_string(),
            since: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            until: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            bugzilla: None,
            bodhi: None,
            koji: BTreeMap::new(),
            gitlab,
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        // No override bullet when the domain user matches the CLI user.
        assert!(!md.contains("- GitLab `"));
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
            gitlab: BTreeMap::new(),
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        assert!(md.contains("# Activity Report: test"));
        assert!(!md.contains("**User:**"));
    }
}
