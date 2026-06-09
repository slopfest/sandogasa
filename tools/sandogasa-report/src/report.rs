// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Unified report model and formatting.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::Serialize;

use crate::{bodhi, bugzilla, github, gitlab, koji};

/// Full activity report.
///
/// Per-domain activity (Bodhi, Koji, GitLab, GitHub) is rendered as
/// one section per domain in CLI `--domain` order. Bugzilla is
/// aggregated across all domains into a single section, placed
/// after the last domain block that references it.
#[derive(Debug, Serialize)]
pub struct Report {
    /// FAS username (if filtered).
    pub user: Option<String>,
    /// Combined domain label (CLI `--domain` values joined).
    pub domain: String,
    /// Reporting period start (inclusive).
    pub since: NaiveDate,
    /// Reporting period end (inclusive).
    pub until: NaiveDate,
    /// Per-domain activity blocks, in CLI `--domain` order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<DomainReport>,
    /// Aggregated Bugzilla section (one query across all domains
    /// that enable Bugzilla).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bugzilla: Option<bugzilla::BugzillaReport>,
    /// Render hint (not serialized): the aggregated Bugzilla section
    /// is emitted after this many domain blocks, matching the
    /// position of the last domain that references Bugzilla. `0`
    /// renders it before all blocks.
    #[serde(skip)]
    pub bugzilla_after: usize,
}

/// One domain's per-domain activity. Each service is present only
/// when that domain enables it and produced a report.
#[derive(Debug, Serialize)]
pub struct DomainReport {
    /// CLI domain name (e.g. "hyperscale").
    pub name: String,
    /// Bodhi section for this domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bodhi: Option<bodhi::BodhiReport>,
    /// Koji CBS section for this domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub koji: Option<koji::KojiReport>,
    /// GitLab section for this domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gitlab: Option<gitlab::GitlabReport>,
    /// GitHub section for this domain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<github::GithubReport>,
}

impl DomainReport {
    /// Whether this domain produced any per-domain activity. Empty
    /// domains are not added to the report.
    pub fn has_content(&self) -> bool {
        self.bodhi.is_some()
            || self.koji.is_some()
            || self.gitlab.is_some()
            || self.github.is_some()
    }
}

/// Format the full report as Markdown.
///
/// `detail` is a count: 0 = summary, 1 = detailed with Bodhi
/// multi-build updates collapsed to a count, 2 = detailed with
/// every build listed. Only Bodhi distinguishes levels 1 and 2;
/// the other sections treat `detail >= 1` uniformly.
pub fn format_markdown(
    report: &Report,
    detail: u8,
    groups: &BTreeMap<String, crate::config::GroupConfig>,
) -> String {
    let mut out = String::new();

    // Header.
    out.push_str(&format!("# Activity Report: {}\n\n", report.domain));
    // Per-instance forge usernames that differ from the FAS login.
    // Keyed by hostname (not CLI domain), since one forge instance
    // commonly serves multiple CLI domains (e.g. hyperscale and
    // proposed-updates both on gitlab.com); showing the host-level
    // identity once reads more naturally than repeating it per
    // domain. GitHub aliases merge into the same map so a person
    // with the same handle on github.com and salsa.debian.org
    // gets one line per host regardless of forge.
    let fas_user = report.user.as_deref();
    let mut gitlab_aliases: BTreeMap<String, String> = BTreeMap::new();
    for dr in &report.domains {
        if let Some(ref gl) = dr.gitlab {
            let host = crate::gitlab::instance_host(&gl.instance);
            if Some(gl.user.as_str()) != fas_user {
                gitlab_aliases
                    .entry(host)
                    .or_insert_with(|| gl.user.clone());
            }
        }
    }
    for dr in &report.domains {
        if let Some(ref gh) = dr.github {
            let host = crate::github::instance_host(&gh.instance);
            if Some(gh.user.as_str()) != fas_user {
                gitlab_aliases
                    .entry(host)
                    .or_insert_with(|| gh.user.clone());
            }
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

    // One section per domain, in CLI --domain order. The aggregated
    // Bugzilla section is emitted after `bugzilla_after` blocks, so
    // it lands right after the last domain that references it.
    for (i, dr) in report.domains.iter().enumerate() {
        if i == report.bugzilla_after
            && let Some(ref bz_report) = report.bugzilla
        {
            out.push_str(&bugzilla::format_markdown(bz_report, detail));
        }
        out.push_str(&format_domain(dr, detail, groups));
    }
    // Bugzilla placed after every block (or when there are none).
    if report.bugzilla_after >= report.domains.len()
        && let Some(ref bz_report) = report.bugzilla
    {
        out.push_str(&bugzilla::format_markdown(bz_report, detail));
    }

    out
}

/// Render a single domain's block: a `## <name>` heading followed by
/// each present service at `###` level. Returns an empty string when
/// every service renders empty, so no bare heading is emitted.
fn format_domain(
    dr: &DomainReport,
    detail: u8,
    groups: &BTreeMap<String, crate::config::GroupConfig>,
) -> String {
    let mut body = String::new();
    if let Some(ref bodhi_report) = dr.bodhi {
        body.push_str(&bodhi::format_markdown(bodhi_report, detail));
    }
    if let Some(ref koji_report) = dr.koji {
        body.push_str(&koji::format_markdown(koji_report, detail, groups));
    }
    if let Some(ref gl_report) = dr.gitlab {
        body.push_str(&gitlab::format_markdown(gl_report, detail));
    }
    if let Some(ref gh_report) = dr.github {
        body.push_str(&github::format_markdown(gh_report, detail));
    }
    if body.is_empty() {
        return String::new();
    }
    format!("## {}\n\n{body}", dr.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn report(user: Option<&str>, domain: &str, domains: Vec<DomainReport>) -> Report {
        Report {
            user: user.map(String::from),
            domain: domain.to_string(),
            since: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            until: NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            domains,
            bugzilla: None,
            bugzilla_after: 0,
        }
    }

    fn domain(name: &str) -> DomainReport {
        DomainReport {
            name: name.to_string(),
            bodhi: None,
            koji: None,
            gitlab: None,
            github: None,
        }
    }

    fn koji_with(pkg: &str) -> koji::KojiReport {
        let mut packages = BTreeMap::new();
        packages.insert(
            pkg.to_string(),
            koji::PackageEntry {
                name: pkg.to_string(),
                change: koji::ChangeKind::New,
                versions: BTreeMap::new(),
                owner: "u".to_string(),
            },
        );
        koji::KojiReport { packages }
    }

    fn gitlab_with(instance: &str, user: &str) -> gitlab::GitlabReport {
        gitlab::GitlabReport {
            instance: instance.to_string(),
            user: user.to_string(),
            ..Default::default()
        }
    }

    fn empty_bugzilla() -> bugzilla::BugzillaReport {
        bugzilla::BugzillaReport {
            reviews: Default::default(),
            security: Default::default(),
            updates: Default::default(),
            branches: Default::default(),
            ftbfs: Default::default(),
            fti: Default::default(),
            other: Default::default(),
        }
    }

    #[test]
    fn format_header() {
        let r = report(Some("testuser"), "fedora", vec![]);
        let md = format_markdown(&r, 0, &BTreeMap::new());
        assert!(md.contains("# Activity Report: fedora"));
        assert!(md.contains("**User:** `testuser`"));
        assert!(md.contains("**Period:** 2026-01-01 to 2026-03-31"));
    }

    #[test]
    fn format_koji_nested_under_domain_heading() {
        let mut d = domain("hyperscale");
        d.koji = Some(koji_with("foo"));
        let r = report(None, "hyperscale", vec![d]);
        let md = format_markdown(&r, 0, &BTreeMap::new());
        // Domain heading at ##, service heading nested at ###.
        let dom = md.find("## hyperscale").unwrap();
        let koji = md.find("### Koji CBS").unwrap();
        assert!(dom < koji, "domain heading precedes its service heading");
        // No legacy "## Koji CBS (suffix)" labeled heading.
        assert!(!md.contains("Koji CBS ("));
    }

    #[test]
    fn format_domain_blocks_follow_cli_order() {
        // Domains in non-alphabetical CLI order must render in that
        // order, not sorted.
        let mut pu = domain("proposed_updates");
        pu.koji = Some(koji_with("bar"));
        let mut hs = domain("hyperscale");
        hs.koji = Some(koji_with("foo"));
        let r = report(None, "proposed_updates + hyperscale", vec![pu, hs]);
        let md = format_markdown(&r, 0, &BTreeMap::new());
        let pu_pos = md.find("## proposed_updates").unwrap();
        let hs_pos = md.find("## hyperscale").unwrap();
        assert!(pu_pos < hs_pos, "blocks follow CLI order, not alphabetical");
    }

    #[test]
    fn format_bugzilla_after_last_referencing_domain() {
        // fedora + epel reference Bugzilla; upstream does not. The
        // aggregated Bugzilla section lands after epel (the last
        // referencing domain) and before upstream.
        let mut fedora = domain("fedora");
        fedora.koji = Some(koji_with("a"));
        let mut epel = domain("epel");
        epel.koji = Some(koji_with("b"));
        let mut upstream = domain("upstream");
        upstream.koji = Some(koji_with("c"));
        let mut r = report(
            Some("u"),
            "fedora + epel + upstream",
            vec![fedora, epel, upstream],
        );
        r.bugzilla = Some(empty_bugzilla());
        r.bugzilla_after = 2; // after fedora + epel blocks
        let md = format_markdown(&r, 0, &BTreeMap::new());
        let epel_pos = md.find("## epel").unwrap();
        let bz_pos = md.find("## Bugzilla").unwrap();
        let upstream_pos = md.find("## upstream").unwrap();
        assert!(
            epel_pos < bz_pos,
            "Bugzilla follows the last referencing domain"
        );
        assert!(
            bz_pos < upstream_pos,
            "Bugzilla precedes later non-referencing domains"
        );
    }

    #[test]
    fn format_bugzilla_before_all_when_after_zero() {
        let mut d = domain("upstream");
        d.gitlab = Some(gitlab_with("https://gitlab.com", "u"));
        let mut r = report(Some("u"), "upstream", vec![d]);
        r.bugzilla = Some(empty_bugzilla());
        r.bugzilla_after = 0;
        let md = format_markdown(&r, 0, &BTreeMap::new());
        assert!(md.find("## Bugzilla").unwrap() < md.find("## upstream").unwrap());
    }

    #[test]
    fn format_header_lists_gitlab_aliases_when_different() {
        let mut hs = domain("hyperscale");
        hs.gitlab = Some(gitlab_with("https://gitlab.com", "michel-slm"));
        let mut pu = domain("proposed-updates");
        pu.gitlab = Some(gitlab_with("https://gitlab.com", "michel-slm"));
        let mut deb = domain("debian");
        deb.gitlab = Some(gitlab_with("https://salsa.debian.org", "michel"));
        let r = report(
            Some("salimma"),
            "hyperscale + proposed-updates + debian",
            vec![hs, pu, deb],
        );
        let md = format_markdown(&r, 0, &BTreeMap::new());
        // List form: FAS primary (labeled) + one bullet per
        // instance hostname. The two gitlab.com entries dedupe to a
        // single bullet.
        assert!(md.contains("**User:**\n  - `salimma` (FAS)\n"));
        assert!(md.contains("  - `michel-slm` (gitlab.com)"));
        assert!(md.contains("  - `michel` (salsa.debian.org)"));
        // Aliases are keyed by host in a BTreeMap, so salsa.debian.org
        // sorts last before the trailing blank.
        assert!(md.contains("(salsa.debian.org)\n\n**Period:**"));
    }

    #[test]
    fn format_header_omits_gitlab_line_when_users_match() {
        let mut hs = domain("hyperscale");
        hs.gitlab = Some(gitlab_with("https://gitlab.com", "salimma"));
        let r = report(Some("salimma"), "hyperscale", vec![hs]);
        let md = format_markdown(&r, 0, &BTreeMap::new());
        // No override bullet when the domain user matches the CLI user.
        assert!(!md.contains("- GitLab `"));
    }

    #[test]
    fn format_empty_report() {
        let r = report(None, "test", vec![]);
        let md = format_markdown(&r, 0, &BTreeMap::new());
        assert!(md.contains("# Activity Report: test"));
        assert!(!md.contains("**User:**"));
    }
}
