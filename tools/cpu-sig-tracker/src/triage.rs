// SPDX-License-Identifier: Apache-2.0 OR MIT

//! How urgent a tracking issue's update is, as the issue records it
//! (#98): the CVEs it fixes, each with its severity and a link to the
//! page listing its advisories, and a `priority::` label from the
//! priority Red Hat set on the RHEL issues. `file-issue` records both,
//! and `status --refresh` fills them in on issues filed without them.

use sandogasa_cve::severity::Rating;

/// What a CVE line starts with.
pub const CVE_LINE: &str = "- **CVE**:";

/// The scope of the priority labels: `priority::major`.
pub const PRIORITY_SCOPE: &str = "priority::";

/// Jira's priorities, most urgent first. `Undefined` is not one.
const PRIORITIES: [&str; 5] = ["Blocker", "Critical", "Major", "Normal", "Minor"];

/// The CVEs named in `texts`, each once, in the order first named.
pub fn cves_in<'a>(texts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in texts.into_iter().flat_map(crate::sync_updates::cve_ids) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// The CVEs a RHEL issue's description sets out, as `cpu-sig-tracker
/// jira` drafts it: a section per CVE, headed `CVE-…:` when there are
/// several, each ending in the CVE's NVD link. Only those lines count,
/// so a CVE the text mentions in passing, such as an older one a fix
/// regressed, is not taken as one the update fixes.
pub fn described_cves(description: &str) -> Vec<String> {
    cves_in(description.lines().map(str::trim).filter(|l| {
        let id = l
            .strip_prefix("https://nvd.nist.gov/vuln/detail/")
            .or_else(|| l.strip_suffix(':'));
        id.is_some_and(|id| crate::sync_updates::cve_ids(id) == [id.to_ascii_uppercase()])
    }))
}

/// The CVEs an update fixes: those named in `texts` (the merge request's
/// title and description, the tracking issue's body, the RHEL issues'
/// summaries), then those its RHEL issues' `descriptions` set out
/// ([`described_cves`]), each once. An update fixing several CVEs names
/// them in its RHEL issue's description, not in a title.
pub fn update_cves<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    descriptions: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut out = cves_in(texts);
    for id in descriptions.into_iter().flat_map(described_cves) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// The label for the most urgent of `priorities` (Jira's names):
/// `priority::critical`. `None` when none is a priority Jira ranks.
pub fn priority_label<'a>(priorities: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let rank = |p: &str| {
        PRIORITIES
            .iter()
            .position(|k| k.eq_ignore_ascii_case(p.trim()))
    };
    priorities
        .into_iter()
        .filter_map(rank)
        .min()
        .map(|i| format!("{PRIORITY_SCOPE}{}", PRIORITIES[i].to_ascii_lowercase()))
}

/// A body's line for `cve`: its severity by the first of `ratings` (Red
/// Hat's, when it rated it), linking Red Hat's page for it, which lists
/// the advisories that fix it, and NVD's.
pub fn cve_line(cve: &str, ratings: &[Rating]) -> String {
    let rated = match ratings.first() {
        Some(r) => format!("{} ({r})", r.level.as_str()),
        None => "not rated yet".to_string(),
    };
    format!(
        "{CVE_LINE} [{cve}](https://access.redhat.com/security/cve/{cve}) — {rated}; \
         [NVD](https://nvd.nist.gov/vuln/detail/{cve})"
    )
}

/// The CVEs a body's CVE lines name.
pub fn parse_cve_lines(body: &str) -> Vec<String> {
    cves_in(
        body.lines()
            .filter(|l| l.trim_start().starts_with(CVE_LINE)),
    )
}

/// The CVE lines for `cves`, each rated by Red Hat, NVD and GitHub
/// (what `jira` rates a CVE by).
pub fn rated_cve_lines(cves: &[String], verbose: bool) -> Vec<String> {
    let Ok(runtime) = crate::jira::runtime() else {
        return cves.iter().map(|c| cve_line(c, &[])).collect();
    };
    cves.iter()
        .map(|cve| {
            let ratings = runtime.block_on(crate::jira_issue::cve_ratings(cve, verbose));
            cve_line(cve, &ratings)
        })
        .collect()
}

/// The label changes that make `labels` carry `want` as their only
/// priority: `(add, remove)`, both empty when they already do.
pub fn priority_change(labels: &[String], want: &str) -> (Vec<String>, Vec<String>) {
    let remove: Vec<String> = labels
        .iter()
        .filter(|l| l.starts_with(PRIORITY_SCOPE) && l.as_str() != want)
        .cloned()
        .collect();
    let add = if labels.iter().any(|l| l == want) {
        Vec::new()
    } else {
        vec![want.to_string()]
    };
    (add, remove)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandogasa_cve::severity::Level;

    #[test]
    fn the_most_urgent_jira_priority_names_the_label() {
        assert_eq!(
            priority_label(["Normal", "Critical", "Major"]).as_deref(),
            Some("priority::critical")
        );
        assert_eq!(
            priority_label(["major"]).as_deref(),
            Some("priority::major")
        );
        assert_eq!(priority_label(["Undefined"]), None);
        assert_eq!(priority_label([]), None);
    }

    #[test]
    fn a_cve_line_gives_the_severity_and_links_the_advisories() {
        let ratings = [Rating {
            level: Level::High,
            source: "Red Hat".into(),
            detail: "Important (CVSS 3: 7.5)".into(),
        }];
        let line = cve_line("CVE-2026-119741", &ratings);
        assert_eq!(
            line,
            "- **CVE**: [CVE-2026-119741](https://access.redhat.com/security/cve/CVE-2026-119741) \
             — high (Red Hat: Important (CVSS 3: 7.5)); \
             [NVD](https://nvd.nist.gov/vuln/detail/CVE-2026-119741)"
        );
        assert!(cve_line("CVE-2026-1", &[]).contains("— not rated yet;"));
        let body = format!("- **MR**: x\n{line}\n- **Release**: c10s\n");
        assert_eq!(parse_cve_lines(&body), ["CVE-2026-119741"]);
        // A CVE named elsewhere in the body is not a CVE line.
        assert!(parse_cve_lines("Fixes CVE-2026-2.\n").is_empty());
    }

    #[test]
    fn a_rhel_issues_description_sets_out_the_cves_it_fixes() {
        // As `jira` drafts it for git's 2.52.1 update (RHEL-306261).
        let several = "Update git to 2.52.1 to address CVEs announced in https://github.blog/x/\n\
                       \n\
                       CVE-2026-111760:\n\
                       Severity: high (Red Hat: Important (CVSS 3: 8.8))\n\
                       \n\
                       https://nvd.nist.gov/vuln/detail/CVE-2026-111760\n\
                       \n\
                       CVE-2026-113408:\n\
                       Severity: medium\n\
                       \n\
                       A regression of the fix for CVE-2020-0001.\n\
                       \n\
                       https://nvd.nist.gov/vuln/detail/CVE-2026-113408\n";
        assert_eq!(
            described_cves(several),
            ["CVE-2026-111760", "CVE-2026-113408"]
        );
        // One CVE: no heading, only its NVD link.
        assert_eq!(
            described_cves("Severity: high\n\nhttps://nvd.nist.gov/vuln/detail/CVE-2026-1234\n"),
            ["CVE-2026-1234"]
        );
        // A heading that is not a CVE alone, or prose, is not one.
        assert!(described_cves("See CVE-2026-1234:\nCVE-2026-1234 is bad").is_empty());
        assert_eq!(
            update_cves(["Fix CVE-2026-1234"], [several]),
            ["CVE-2026-1234", "CVE-2026-111760", "CVE-2026-113408"]
        );
        assert_eq!(
            update_cves(["Fix CVE-2026-111760"], [several]),
            ["CVE-2026-111760", "CVE-2026-113408"]
        );
    }

    #[test]
    fn cves_are_gathered_once_in_order() {
        assert_eq!(
            cves_in([
                "Fix sum2 overflow and CVE-2026-119741",
                "Resolves: RHEL-1\ncve-2026-119741, CVE-2026-2000",
            ]),
            ["CVE-2026-119741", "CVE-2026-2000"]
        );
    }

    #[test]
    fn a_priority_label_replaces_another_and_is_left_alone_when_right() {
        let labels: Vec<String> = ["c10s", "priority::normal", "security"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            priority_change(&labels, "priority::major"),
            (
                vec!["priority::major".to_string()],
                vec!["priority::normal".to_string()]
            )
        );
        let right: Vec<String> = ["priority::major"].map(String::from).to_vec();
        assert_eq!(priority_change(&right, "priority::major"), (vec![], vec![]));
    }
}
