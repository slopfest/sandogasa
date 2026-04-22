// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `status` subcommand (v1).
//!
//! For every active tracking issue in the inventory, parse the
//! standardized body that `file-issue` wrote, pull out the MR
//! URL and JIRA key, fetch the JIRA state, and report a
//! per-package row.
//!
//! v1 is JIRA-only — it does not yet query Koji for the
//! currently tagged NVR or Stream for the catch-up NVR. A
//! follow-up will add that to drive "untag" / "rebase"
//! suggestions.

use std::process::ExitCode;

use crate::{gitlab, jira};

const PROPOSED_UPDATES_GROUP: &str = "CentOS/proposed_updates/rpms";
const TRACKING_LABEL: &str = "cpu-sig-tracker";
const GITLAB_BASE: &str = "https://gitlab.com";

#[derive(clap::Args)]
pub struct StatusArgs {
    /// Path to the sandogasa-inventory TOML file.
    #[arg(short, long, default_value = "inventory.toml")]
    pub inventory: String,

    /// Restrict the check to a single release (e.g. `c10s`).
    #[arg(long)]
    pub release: Option<String>,

    /// Emit JSON instead of grouped text.
    #[arg(long)]
    pub json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Row {
    pub release: String,
    pub package: String,
    pub issue_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mr_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_resolution: Option<String>,
    pub jira_resolved: bool,
    pub suggestion: &'static str,
}

pub fn run(args: &StatusArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(args: &StatusArgs) -> Result<(), Box<dyn std::error::Error>> {
    let inventory = sandogasa_inventory::load(&args.inventory)?;

    let releases: Vec<String> = match &args.release {
        Some(r) => {
            if !inventory.inventory.workloads.contains_key(r) {
                return Err(format!(
                    "release '{r}' not found in inventory; available: {:?}",
                    inventory.workload_names()
                )
                .into());
            }
            vec![r.clone()]
        }
        None => inventory.inventory.workloads.keys().cloned().collect(),
    };

    let group_client = gitlab::GroupClient::new(GITLAB_BASE, PROPOSED_UPDATES_GROUP)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let jira_client = jira::client();

    let mut rows: Vec<Row> = Vec::new();
    for release in &releases {
        if args.verbose {
            eprintln!("[cpu-sig-tracker] fetching active issues for {release}");
        }
        let active_label = format!("{TRACKING_LABEL},{release}");
        let active = group_client.list_issues(&active_label, Some("opened"))?;

        // Packages listed for this release in the inventory.
        let wanted: std::collections::HashSet<String> = inventory
            .inventory
            .workloads
            .get(release)
            .map(|w| w.packages.iter().cloned().collect())
            .unwrap_or_default();

        for issue in active {
            let Some(package) = gitlab::package_from_issue_url(&issue.web_url) else {
                continue;
            };
            if !wanted.contains(package) {
                // Issue exists but the package isn't in this
                // workload — skip (could mean the inventory is
                // stale; sync-issues already flags this class).
                continue;
            }

            let body = issue.description.as_deref().unwrap_or("");
            let mr_url = parse_mr_url_from_body(body).map(|s| s.to_string());
            let jira_key = parse_jira_key_from_body(body).map(|s| s.to_string());

            let jira_info = match jira_key.as_deref() {
                Some(key) => fetch_jira(&runtime, &jira_client, key, args.verbose),
                None => None,
            };
            let (jira_status, jira_resolution, jira_resolved) = match jira_info {
                Some((s, r, b)) => (Some(s), r, b),
                None => (None, None, false),
            };

            let suggestion = suggest_next_action(jira_key.as_deref(), jira_resolved);

            rows.push(Row {
                release: release.to_string(),
                package: package.to_string(),
                issue_url: issue.web_url.clone(),
                mr_url,
                jira_key,
                jira_status,
                jira_resolution,
                jira_resolved,
                suggestion,
            });
        }
    }

    rows.sort_by(|a, b| a.release.cmp(&b.release).then(a.package.cmp(&b.package)));

    if args.json {
        let json = serde_json::to_string_pretty(&rows)?;
        println!("{json}");
    } else {
        print_human(&rows);
    }

    Ok(())
}

/// Extract the MR URL from the standardized issue body line
/// `- **MR**: [title](url)`.
fn parse_mr_url_from_body(body: &str) -> Option<&str> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- **MR**: [")
            && let Some(idx) = rest.find("](")
        {
            let after = &rest[idx + 2..];
            if let Some(end) = after.find(')') {
                return Some(&after[..end]);
            }
        }
    }
    None
}

/// Extract the JIRA key from the standardized issue body line
/// `- **JIRA**: [KEY](url)…`.
fn parse_jira_key_from_body(body: &str) -> Option<&str> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- **JIRA**: [")
            && let Some(end) = rest.find(']')
        {
            return Some(&rest[..end]);
        }
    }
    None
}

fn fetch_jira(
    runtime: &tokio::runtime::Runtime,
    client: &sandogasa_jira::JiraClient,
    key: &str,
    verbose: bool,
) -> Option<(String, Option<String>, bool)> {
    if verbose {
        eprintln!("[cpu-sig-tracker] fetching JIRA {key}");
    }
    match runtime.block_on(client.issue(key)) {
        Ok(Some(issue)) => Some((
            issue.status().to_string(),
            issue.resolution().map(|s| s.to_string()),
            issue.is_resolved(),
        )),
        Ok(None) => {
            eprintln!("warning: JIRA {key} not found or not visible");
            None
        }
        Err(e) => {
            eprintln!("warning: JIRA {key} lookup failed: {e}");
            None
        }
    }
}

fn suggest_next_action(jira_key: Option<&str>, jira_resolved: bool) -> &'static str {
    match jira_key {
        None => "no-jira",
        Some(_) if jira_resolved => "untag-candidate",
        Some(_) => "in-progress",
    }
}

fn print_human(rows: &[Row]) {
    const H_REL: &str = "RELEASE";
    const H_PKG: &str = "PACKAGE";
    const H_JIRA: &str = "JIRA";
    const H_STATE: &str = "STATE";
    const H_SUG: &str = "SUGGESTION";
    const H_ISSUE: &str = "ISSUE";

    let rel_width = rows
        .iter()
        .map(|r| r.release.chars().count())
        .max()
        .unwrap_or(0)
        .max(H_REL.len());
    let pkg_width = rows
        .iter()
        .map(|r| r.package.chars().count())
        .max()
        .unwrap_or(0)
        .max(H_PKG.len());
    let jira_width = rows
        .iter()
        .map(|r| r.jira_key.as_deref().unwrap_or("—").chars().count())
        .max()
        .unwrap_or(0)
        .max(H_JIRA.len());
    let state_width = rows
        .iter()
        .map(|r| format_jira_state(r).chars().count())
        .max()
        .unwrap_or(0)
        .max(H_STATE.len());
    let suggestion_width = rows
        .iter()
        .map(|r| r.suggestion.chars().count())
        .max()
        .unwrap_or(0)
        .max(H_SUG.len());

    println!(
        "{:<rel_width$}  {:<pkg_width$}  {:<jira_width$}  {:<state_width$}  {:<suggestion_width$}  {}",
        H_REL, H_PKG, H_JIRA, H_STATE, H_SUG, H_ISSUE,
    );
    for r in rows {
        let jira_key = r.jira_key.as_deref().unwrap_or("—");
        let jira_state = format_jira_state(r);
        println!(
            "{:<rel_width$}  {:<pkg_width$}  {:<jira_width$}  {:<state_width$}  {:<suggestion_width$}  {}",
            r.release, r.package, jira_key, jira_state, r.suggestion, r.issue_url,
        );
    }
}

fn format_jira_state(r: &Row) -> String {
    match (&r.jira_status, &r.jira_resolution) {
        (Some(status), Some(resolution)) => format!("{status} ({resolution})"),
        (Some(status), None) => status.clone(),
        (None, _) => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_BODY: &str = "\
        Lead paragraph explaining why.\n\
        \n\
        - **MR**: [Fix CVE](https://gitlab.com/foo/bar/-/merge_requests/3)\n\
        - **JIRA**: [RHEL-12345](https://issues.redhat.com/browse/RHEL-12345) — summary text\n\
        - **Release**: c10s\n\
        - **Affected build**: xz-5.4-1.el10\n\
        - **Expected fix**: xz-5.6-1.el10\n\
        - **Status**: open\n";

    #[test]
    fn parse_mr_url_from_standard_body() {
        assert_eq!(
            parse_mr_url_from_body(SAMPLE_BODY),
            Some("https://gitlab.com/foo/bar/-/merge_requests/3"),
        );
    }

    #[test]
    fn parse_jira_key_from_standard_body() {
        assert_eq!(parse_jira_key_from_body(SAMPLE_BODY), Some("RHEL-12345"));
    }

    #[test]
    fn parse_mr_url_missing_line_returns_none() {
        let body = "- **JIRA**: [RHEL-1](https://example/)\n";
        assert_eq!(parse_mr_url_from_body(body), None);
    }

    #[test]
    fn parse_jira_key_missing_line_returns_none() {
        let body = "- **MR**: [t](https://example/)\n";
        assert_eq!(parse_jira_key_from_body(body), None);
    }

    #[test]
    fn parse_mr_url_handles_without_lead_paragraph() {
        // Match against a body that starts with the metadata
        // (no lead --note paragraph).
        let body = "\
            - **MR**: [t](https://example/mr)\n\
            - **JIRA**: [RHEL-2](https://issues.redhat.com/browse/RHEL-2)\n";
        assert_eq!(parse_mr_url_from_body(body), Some("https://example/mr"));
        assert_eq!(parse_jira_key_from_body(body), Some("RHEL-2"));
    }

    #[test]
    fn parse_mr_url_handles_jira_not_found_placeholder() {
        // file-issue emits a placeholder when no JIRA key is
        // auto-extracted; parse_jira_key should return None
        // rather than a bogus string.
        let body = "\
            - **MR**: [t](https://example/mr)\n\
            - **JIRA**: _(not found in MR; set with `--jira`)_\n";
        assert_eq!(parse_jira_key_from_body(body), None);
    }

    #[test]
    fn suggest_untag_candidate_when_jira_resolved() {
        assert_eq!(suggest_next_action(Some("RHEL-1"), true), "untag-candidate");
    }

    #[test]
    fn suggest_in_progress_when_jira_open() {
        assert_eq!(suggest_next_action(Some("RHEL-1"), false), "in-progress");
    }

    #[test]
    fn suggest_no_jira_when_key_missing() {
        assert_eq!(suggest_next_action(None, false), "no-jira");
    }

    #[test]
    fn format_jira_state_with_resolution() {
        let r = Row {
            release: "c10s".into(),
            package: "xz".into(),
            issue_url: "u".into(),
            mr_url: None,
            jira_key: Some("RHEL-1".into()),
            jira_status: Some("Closed".into()),
            jira_resolution: Some("Done".into()),
            jira_resolved: true,
            suggestion: "untag-candidate",
        };
        assert_eq!(format_jira_state(&r), "Closed (Done)");
    }

    #[test]
    fn format_jira_state_open_unresolved() {
        let r = Row {
            release: "c10s".into(),
            package: "xz".into(),
            issue_url: "u".into(),
            mr_url: None,
            jira_key: Some("RHEL-1".into()),
            jira_status: Some("In Progress".into()),
            jira_resolution: None,
            jira_resolved: false,
            suggestion: "in-progress",
        };
        assert_eq!(format_jira_state(&r), "In Progress");
    }

    #[test]
    fn format_jira_state_unknown() {
        let r = Row {
            release: "c10s".into(),
            package: "xz".into(),
            issue_url: "u".into(),
            mr_url: None,
            jira_key: None,
            jira_status: None,
            jira_resolution: None,
            jira_resolved: false,
            suggestion: "no-jira",
        };
        assert_eq!(format_jira_state(&r), "unknown");
    }
}
