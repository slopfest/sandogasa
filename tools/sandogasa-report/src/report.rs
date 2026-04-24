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
    // Per-instance GitLab usernames that differ from the FAS login.
    // Keyed by hostname (not CLI domain), since one GitLab
    // instance is serving multiple CLI domains is the common case
    // (hyperscale + proposed-updates both on gitlab.com) and
    // showing the host-level identity once reads more naturally
    // than repeating it per CLI domain.
    let fas_user = report.user.as_deref();
    let mut gitlab_aliases: BTreeMap<String, String> = BTreeMap::new();
    for gl in report.gitlab.values() {
        let host = crate::gitlab::instance_host(&gl.instance);
        if Some(gl.user.as_str()) != fas_user {
            gitlab_aliases
                .entry(host)
                .or_insert_with(|| gl.user.clone());
        }
    }
    match (fas_user, gitlab_aliases.is_empty()) {
        // No aliases: keep the compact single-line form.
        (Some(user), true) => out.push_str(&format!("**User:** `{user}`\n")),
        // FAS user plus per-host GitLab aliases: render as a
        // bulleted identity list. FAS is annotated so it's clear
        // which service the primary username applies to.
        (Some(user), false) => {
            out.push_str("**User:**\n");
            out.push_str(&format!("  - `{user}` (FAS)\n"));
            for (host, alias) in &gitlab_aliases {
                out.push_str(&format!("  - `{alias}` ({host})\n"));
            }
            out.push('\n');
        }
        // No FAS user but GitLab identities are configured (rare —
        // running without --user against GitLab-only domains).
        (None, false) => {
            out.push_str("**User:**\n");
            for (host, alias) in &gitlab_aliases {
                out.push_str(&format!("  - `{alias}` ({host})\n"));
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
        // List form: FAS primary (labeled) + one bullet per
        // instance hostname. The two hyperscale/proposed-updates
        // entries share a host → only one bullet for gitlab.com.
        assert!(md.contains("**User:**\n  - `salimma` (FAS)\n"));
        assert!(md.contains("  - `michel-slm` (gitlab.com)"));
        assert!(md.contains("  - `michel` (salsa.debian.org)"));
        // Blank line separates the identity block from Period.
        // BTreeMap sorts hosts alphabetically → salsa.debian.org
        // is last before the trailing blank.
        assert!(md.contains("(salsa.debian.org)\n\n**Period:**"));
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
