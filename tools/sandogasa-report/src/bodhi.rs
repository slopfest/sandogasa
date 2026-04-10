// SPDX-License-Identifier: MPL-2.0

//! Bodhi activity reporting — updates pushed, per-release breakdown.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::Serialize;

use crate::config::DomainConfig;

/// Bodhi activity report.
#[derive(Debug, Serialize)]
pub struct BodhiReport {
    /// Total updates in the period.
    pub total_updates: usize,
    /// Total builds across all updates.
    pub total_builds: usize,
    /// Per-release breakdown: release name → list of updates.
    pub by_release: BTreeMap<String, Vec<UpdateEntry>>,
}

/// A single Bodhi update entry.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateEntry {
    pub alias: String,
    pub status: String,
    pub builds: Vec<String>,
    pub date_submitted: Option<String>,
}

/// Parse a Bodhi date string like "2026-02-25 11:55:26" into a NaiveDate.
fn parse_bodhi_date(date_str: &str) -> Option<NaiveDate> {
    // Bodhi dates are "YYYY-MM-DD HH:MM:SS" — just take the date part.
    let date_part = date_str.split_whitespace().next()?;
    NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()
}

/// Check if a release name matches any of the configured patterns.
fn matches_release(name: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    patterns.iter().any(|pat| {
        if let Some(prefix) = pat.strip_suffix('*') {
            name.starts_with(prefix)
        } else {
            name == pat
        }
    })
}

/// Run Bodhi reporting for a user and domain.
pub async fn bodhi_report(
    username: &str,
    domain: &DomainConfig,
    since: NaiveDate,
    until: NaiveDate,
    verbose: bool,
) -> Result<BodhiReport, String> {
    let client = sandogasa_bodhi::BodhiClient::new();

    if verbose {
        eprintln!("[bodhi] fetching updates for {username}");
    }

    // Fetch a large batch — Bodhi returns most recent first.
    // We stop when we see updates older than our period.
    let updates = client
        .updates_for_user(username, 500)
        .await
        .map_err(|e| format!("Bodhi query failed: {e}"))?;

    let mut by_release: BTreeMap<String, Vec<UpdateEntry>> = BTreeMap::new();
    let mut total_updates = 0;
    let mut total_builds = 0;

    for update in &updates {
        // Filter by date.
        let date = update.date_submitted.as_deref().and_then(parse_bodhi_date);

        if let Some(d) = date {
            if d < since {
                // Updates are sorted most recent first — we're past
                // the period, stop.
                break;
            }
            if d > until {
                continue;
            }
        } else {
            continue;
        }

        // Filter by release pattern.
        let release_name = update
            .release
            .as_ref()
            .map(|r| r.name.clone())
            .unwrap_or_else(|| "unknown".to_string());

        if !matches_release(&release_name, &domain.bodhi_releases) {
            continue;
        }

        let entry = UpdateEntry {
            alias: update.alias.clone(),
            status: update.status.clone(),
            builds: update.builds.iter().map(|b| b.nvr.clone()).collect(),
            date_submitted: update.date_submitted.clone(),
        };

        total_builds += entry.builds.len();
        total_updates += 1;
        by_release.entry(release_name).or_default().push(entry);
    }

    if verbose {
        eprintln!("[bodhi] {total_updates} update(s), {total_builds} build(s)");
    }

    Ok(BodhiReport {
        total_updates,
        total_builds,
        by_release,
    })
}

/// Format the Bodhi report as Markdown.
/// Extract a numeric sort key from a release name.
///
/// "F45" → (45, 0), "EPEL-10.3" → (10, 3), "EPEL-9" → (9, 0).
/// Sorts by major version first, then minor, both descending.
fn release_sort_key(name: &str) -> (u32, u32) {
    let version_str: String = name
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = version_str.splitn(2, '.');
    let major = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor)
}

/// Sort release names by version number, newest first.
fn sorted_releases(by_release: &BTreeMap<String, Vec<UpdateEntry>>) -> Vec<&str> {
    let mut releases: Vec<&str> = by_release.keys().map(|s| s.as_str()).collect();
    releases.sort_by(|a, b| release_sort_key(b).cmp(&release_sort_key(a)));
    releases
}

pub fn format_markdown(report: &BodhiReport, detailed: bool) -> String {
    let mut out = String::new();

    out.push_str("## Bodhi\n\n");

    out.push_str(&format!(
        "**{}** update(s) with **{}** build(s) across **{}** release(s).\n\n",
        report.total_updates,
        report.total_builds,
        report.by_release.len()
    ));

    let releases = sorted_releases(&report.by_release);

    // Per-release summary.
    for release in &releases {
        let updates = &report.by_release[*release];
        let builds: usize = updates.iter().map(|u| u.builds.len()).sum();
        out.push_str(&format!(
            "- **{release}**: {} update(s), {builds} build(s)\n",
            updates.len()
        ));
    }
    out.push('\n');

    if !detailed {
        return out;
    }

    // Detailed: list updates per release.
    for release in &releases {
        let updates = &report.by_release[*release];
        out.push_str(&format!("### {release}\n\n"));
        for u in updates {
            let date = u
                .date_submitted
                .as_deref()
                .and_then(|d| d.split_whitespace().next())
                .unwrap_or("?");
            out.push_str(&format!(
                "- [{}](https://bodhi.fedoraproject.org/updates/{}) \
                 ({}, {date})\n",
                u.alias, u.alias, u.status
            ));
            for nvr in &u.builds {
                out.push_str(&format!("  - {nvr}\n"));
            }
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bodhi_date_valid() {
        let d = parse_bodhi_date("2026-02-25 11:55:26").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 25).unwrap());
    }

    #[test]
    fn parse_bodhi_date_invalid() {
        assert!(parse_bodhi_date("not-a-date").is_none());
    }

    #[test]
    fn matches_release_wildcard() {
        assert!(matches_release("F42", &["F*".to_string()]));
        assert!(matches_release("EPEL-9", &["EPEL-*".to_string()]));
        assert!(!matches_release("F42", &["EPEL-*".to_string()]));
    }

    #[test]
    fn matches_release_exact() {
        assert!(matches_release("F42", &["F42".to_string()]));
        assert!(!matches_release("F43", &["F42".to_string()]));
    }

    #[test]
    fn matches_release_empty_matches_all() {
        assert!(matches_release("anything", &[]));
    }

    #[test]
    fn sort_key_fedora() {
        assert_eq!(release_sort_key("F45"), (45, 0));
        assert_eq!(release_sort_key("F9"), (9, 0));
    }

    #[test]
    fn sort_key_epel() {
        assert_eq!(release_sort_key("EPEL-10"), (10, 0));
        assert_eq!(release_sort_key("EPEL-10.3"), (10, 3));
        assert_eq!(release_sort_key("EPEL-9"), (9, 0));
    }

    #[test]
    fn sort_order_newest_first() {
        let mut map = BTreeMap::new();
        map.insert("F42".to_string(), vec![]);
        map.insert("EPEL-10.1".to_string(), vec![]);
        map.insert("EPEL-10.3".to_string(), vec![]);
        map.insert("F45".to_string(), vec![]);
        map.insert("EPEL-9".to_string(), vec![]);
        let sorted = sorted_releases(&map);
        assert_eq!(
            sorted,
            vec!["F45", "F42", "EPEL-10.3", "EPEL-10.1", "EPEL-9"]
        );
    }
}
