// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `status` subcommand.
//!
//! For every active tracking issue in the inventory, parse the
//! standardized body that `file-issue` wrote, pull out the MR
//! URL and JIRA key, fetch the JIRA state, look up the
//! currently-tagged proposed_updates NVR in Koji, and look up
//! the current CentOS Stream NVR via fedrq. From those five
//! inputs compute a "what should I do next" suggestion per
//! row.

use std::collections::HashMap;
use std::process::ExitCode;

use sandogasa_fedrq::Fedrq;
use sandogasa_koji::{TaggedBuild, list_tagged, parse_nvr};
use sandogasa_rpmvercmp::compare_evr;

use crate::dump_inventory::proposed_updates_tag;
use crate::{gitlab, jira};

const PROPOSED_UPDATES_GROUP: &str = "CentOS/proposed_updates/rpms";
const TRACKING_LABEL: &str = "cpu-sig-tracker";
const GITLAB_BASE: &str = "https://gitlab.com";
const KOJI_PROFILE: &str = "cbs";

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
    /// Currently-tagged NVR in the proposed_updates Koji tag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed_updates_nvr: Option<String>,
    /// Current CentOS Stream NVR for the same release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_nvr: Option<String>,
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

        // Status is driven by tracking issues, not the inventory.
        // Packages without a Koji `-release` tag still get a row
        // (retired builds / pre-tag MRs), which is the point of
        // inverting the driver. The inventory is only consulted
        // later (via sync-issues) for gap analysis.
        let tracked_packages: Vec<String> = active
            .iter()
            .filter_map(|i| gitlab::package_from_issue_url(&i.web_url).map(|s| s.to_string()))
            .collect();

        if args.verbose {
            eprintln!("[cpu-sig-tracker] fetching proposed_updates tag for {release}");
        }
        let pu_nvrs = fetch_proposed_updates_nvrs(release, args.verbose);

        if args.verbose {
            eprintln!("[cpu-sig-tracker] fetching Stream NVRs for {release}");
        }
        let stream_nvrs = fetch_stream_nvrs(release, &tracked_packages, args.verbose);

        for issue in active {
            let Some(package) = gitlab::package_from_issue_url(&issue.web_url) else {
                continue;
            };

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

            let pu_nvr = pu_nvrs.get(package).cloned();
            let stream_nvr = stream_nvrs.get(package).cloned();

            let suggestion = suggest_next_action(
                jira_key.as_deref(),
                jira_resolved,
                pu_nvr.as_deref(),
                stream_nvr.as_deref(),
            );

            rows.push(Row {
                release: release.to_string(),
                package: package.to_string(),
                issue_url: issue.web_url.clone(),
                mr_url,
                jira_key,
                jira_status,
                jira_resolution,
                jira_resolved,
                proposed_updates_nvr: pu_nvr,
                stream_nvr,
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

fn fetch_proposed_updates_nvrs(release: &str, verbose: bool) -> HashMap<String, String> {
    let tag = match proposed_updates_tag(release) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("warning: {e}; skipping proposed_updates NVR lookup");
            return HashMap::new();
        }
    };
    match list_tagged(&tag, Some(KOJI_PROFILE), None) {
        Ok(builds) => nvr_map_by_name(&builds),
        Err(e) => {
            if verbose {
                eprintln!("warning: koji list-tagged {tag} failed: {e}");
            }
            HashMap::new()
        }
    }
}

/// Convert a list of tagged builds into a `name → nvr` map
/// suitable for per-package lookup.
fn nvr_map_by_name(builds: &[TaggedBuild]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for b in builds {
        if let Some((name, _, _)) = parse_nvr(&b.nvr) {
            map.insert(name.to_string(), b.nvr.clone());
        }
    }
    map
}

fn fetch_stream_nvrs(release: &str, packages: &[String], verbose: bool) -> HashMap<String, String> {
    if packages.is_empty() {
        return HashMap::new();
    }
    let fq = Fedrq {
        branch: Some(release.to_string()),
        repo: None,
    };
    match fq.src_nvrs(packages) {
        Ok(nvrs) => nvrs
            .into_iter()
            .filter_map(|nvr| parse_nvr(&nvr).map(|(n, _, _)| (n.to_string(), nvr.clone())))
            .collect(),
        Err(e) => {
            if verbose {
                eprintln!("warning: fedrq src_nvrs for {release} failed: {e}");
            }
            HashMap::new()
        }
    }
}

fn suggest_next_action(
    jira_key: Option<&str>,
    jira_resolved: bool,
    pu_nvr: Option<&str>,
    stream_nvr: Option<&str>,
) -> &'static str {
    if jira_key.is_none() {
        return "no-jira";
    }
    // Package has no build tagged into proposed_updates -release
    // — either it was already untagged (retire the issue) or it
    // hasn't been tagged yet (MR in flight, build pending).
    if pu_nvr.is_none() {
        return if jira_resolved {
            "retire-issue"
        } else {
            "not-yet-tagged"
        };
    }
    if jira_resolved {
        return "untag-candidate";
    }
    if stream_newer_than_proposed(pu_nvr, stream_nvr) {
        return "rebase";
    }
    "in-progress"
}

/// True when both NVRs are known AND the Stream V-R is strictly
/// greater than the proposed_updates V-R (RPM ordering). If
/// either side is missing we play it safe and return false —
/// suggestion falls through to `in-progress`.
fn stream_newer_than_proposed(pu_nvr: Option<&str>, stream_nvr: Option<&str>) -> bool {
    use std::cmp::Ordering;
    let Some(pu) = pu_nvr else {
        return false;
    };
    let Some(stream) = stream_nvr else {
        return false;
    };
    let Some(pu_vr) = vr_of_nvr(pu) else {
        return false;
    };
    let Some(stream_vr) = vr_of_nvr(stream) else {
        return false;
    };
    compare_evr(&stream_vr, &pu_vr) == Ordering::Greater
}

/// Extract the "version-release" portion of an NVR. Returns
/// None when the NVR doesn't parse cleanly.
fn vr_of_nvr(nvr: &str) -> Option<String> {
    let (_, v, r) = parse_nvr(nvr)?;
    Some(format!("{v}-{r}"))
}

fn print_human(rows: &[Row]) {
    const H_REL: &str = "RELEASE";
    const H_PKG: &str = "PACKAGE";
    const H_JIRA: &str = "JIRA";
    const H_STATE: &str = "STATE";
    const H_CUR: &str = "CURRENT";
    const H_STREAM: &str = "STREAM";
    const H_SUG: &str = "SUGGESTION";
    const H_ISSUE: &str = "ISSUE";
    const UNKNOWN: &str = "—";

    let rel_width = col_width(H_REL, rows.iter().map(|r| r.release.as_str()));
    let pkg_width = col_width(H_PKG, rows.iter().map(|r| r.package.as_str()));
    let jira_width = col_width(
        H_JIRA,
        rows.iter()
            .map(|r| r.jira_key.as_deref().unwrap_or(UNKNOWN)),
    );
    let states: Vec<String> = rows.iter().map(format_jira_state).collect();
    let state_width = col_width(H_STATE, states.iter().map(|s| s.as_str()));
    let cur_vrs: Vec<String> = rows
        .iter()
        .map(|r| {
            r.proposed_updates_nvr
                .as_deref()
                .and_then(vr_of_nvr)
                .unwrap_or_else(|| UNKNOWN.to_string())
        })
        .collect();
    let cur_width = col_width(H_CUR, cur_vrs.iter().map(|s| s.as_str()));
    let stream_vrs: Vec<String> = rows
        .iter()
        .map(|r| {
            r.stream_nvr
                .as_deref()
                .and_then(vr_of_nvr)
                .unwrap_or_else(|| UNKNOWN.to_string())
        })
        .collect();
    let stream_width = col_width(H_STREAM, stream_vrs.iter().map(|s| s.as_str()));
    let suggestion_width = col_width(H_SUG, rows.iter().map(|r| r.suggestion));

    println!(
        "{:<rel_width$}  {:<pkg_width$}  {:<jira_width$}  {:<state_width$}  {:<cur_width$}  {:<stream_width$}  {:<suggestion_width$}  {}",
        H_REL, H_PKG, H_JIRA, H_STATE, H_CUR, H_STREAM, H_SUG, H_ISSUE,
    );
    for (i, r) in rows.iter().enumerate() {
        let jira_key = r.jira_key.as_deref().unwrap_or(UNKNOWN);
        println!(
            "{:<rel_width$}  {:<pkg_width$}  {:<jira_width$}  {:<state_width$}  {:<cur_width$}  {:<stream_width$}  {:<suggestion_width$}  {}",
            r.release,
            r.package,
            jira_key,
            states[i],
            cur_vrs[i],
            stream_vrs[i],
            r.suggestion,
            r.issue_url,
        );
    }
}

/// Compute a column width that fits both the header and the
/// widest row value.
fn col_width<'a>(header: &str, values: impl IntoIterator<Item = &'a str>) -> usize {
    values
        .into_iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(0)
        .max(header.chars().count())
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
    fn suggest_untag_candidate_when_jira_resolved_and_pu_tagged() {
        // Build still in proposed_updates -release, JIRA closed:
        // the candidate action is to untag the build.
        assert_eq!(
            suggest_next_action(Some("RHEL-1"), true, Some("xz-5.4-1.el10"), None),
            "untag-candidate"
        );
    }

    #[test]
    fn suggest_untag_candidate_wins_over_rebase() {
        // Even if Stream is newer, a resolved JIRA means we can
        // untag — the proposed_updates build is no longer needed.
        assert_eq!(
            suggest_next_action(
                Some("RHEL-1"),
                true,
                Some("xz-5.4-1.el10"),
                Some("xz-5.6-1.el10"),
            ),
            "untag-candidate"
        );
    }

    #[test]
    fn suggest_not_yet_tagged_when_no_pu_nvr_and_open() {
        // No proposed_updates build yet: MR in flight, but the
        // build hasn't been tagged for release. Informational.
        assert_eq!(
            suggest_next_action(Some("RHEL-1"), false, None, None),
            "not-yet-tagged"
        );
        assert_eq!(
            suggest_next_action(Some("RHEL-1"), false, None, Some("xz-5.6-1.el10")),
            "not-yet-tagged"
        );
    }

    #[test]
    fn suggest_retire_issue_when_no_pu_nvr_and_resolved() {
        // Build was already untagged (retired) but the tracking
        // issue is still open — JIRA is closed so we just need
        // to close the issue.
        assert_eq!(
            suggest_next_action(Some("RHEL-1"), true, None, None),
            "retire-issue"
        );
        assert_eq!(
            suggest_next_action(Some("RHEL-1"), true, None, Some("xz-5.6-1.el10")),
            "retire-issue"
        );
    }

    #[test]
    fn suggest_rebase_when_stream_newer_than_proposed() {
        assert_eq!(
            suggest_next_action(
                Some("RHEL-1"),
                false,
                Some("xz-5.4-1.el10"),
                Some("xz-5.6-1.el10"),
            ),
            "rebase"
        );
    }

    #[test]
    fn suggest_in_progress_when_stream_older_or_equal() {
        assert_eq!(
            suggest_next_action(
                Some("RHEL-1"),
                false,
                Some("xz-5.6-1.el10"),
                Some("xz-5.4-1.el10"),
            ),
            "in-progress"
        );
        assert_eq!(
            suggest_next_action(
                Some("RHEL-1"),
                false,
                Some("xz-5.6-1.el10"),
                Some("xz-5.6-1.el10"),
            ),
            "in-progress"
        );
    }

    #[test]
    fn suggest_in_progress_when_pu_known_but_stream_unknown() {
        // We have a proposed_updates build but can't look up
        // Stream — stay in-progress rather than guess rebase.
        assert_eq!(
            suggest_next_action(Some("RHEL-1"), false, Some("xz-5.4-1.el10"), None),
            "in-progress"
        );
    }

    #[test]
    fn suggest_no_jira_when_key_missing() {
        assert_eq!(suggest_next_action(None, false, None, None), "no-jira");
    }

    #[test]
    fn vr_of_nvr_standard() {
        assert_eq!(vr_of_nvr("xz-5.4-1.el10"), Some("5.4-1.el10".to_string()));
    }

    #[test]
    fn vr_of_nvr_hyphenated_name() {
        assert_eq!(
            vr_of_nvr("intel-gpu-tools-1.28-2.el10"),
            Some("1.28-2.el10".to_string())
        );
    }

    #[test]
    fn vr_of_nvr_invalid() {
        assert_eq!(vr_of_nvr("nohyphens"), None);
    }

    #[test]
    fn stream_newer_than_proposed_detects_newer_version() {
        assert!(stream_newer_than_proposed(
            Some("xz-5.4-1.el10"),
            Some("xz-5.6-1.el10"),
        ));
    }

    #[test]
    fn stream_newer_than_proposed_detects_newer_release() {
        assert!(stream_newer_than_proposed(
            Some("xz-5.4-1.el10"),
            Some("xz-5.4-2.el10"),
        ));
    }

    #[test]
    fn stream_newer_than_proposed_false_when_equal() {
        assert!(!stream_newer_than_proposed(
            Some("xz-5.4-1.el10"),
            Some("xz-5.4-1.el10"),
        ));
    }

    #[test]
    fn stream_newer_than_proposed_false_when_stream_older() {
        assert!(!stream_newer_than_proposed(
            Some("xz-5.6-1.el10"),
            Some("xz-5.4-1.el10"),
        ));
    }

    #[test]
    fn stream_newer_than_proposed_false_when_either_missing() {
        assert!(!stream_newer_than_proposed(None, Some("xz-5.4-1.el10")));
        assert!(!stream_newer_than_proposed(Some("xz-5.4-1.el10"), None));
    }

    #[test]
    fn nvr_map_by_name_uses_parsed_name_as_key() {
        let builds = vec![
            TaggedBuild {
                nvr: "xz-5.4-1.el10".to_string(),
                tag: "t".to_string(),
                owner: "u".to_string(),
            },
            TaggedBuild {
                nvr: "yum-utils-4.0-1.el10".to_string(),
                tag: "t".to_string(),
                owner: "u".to_string(),
            },
        ];
        let map = nvr_map_by_name(&builds);
        assert_eq!(map.get("xz").map(String::as_str), Some("xz-5.4-1.el10"));
        assert_eq!(
            map.get("yum-utils").map(String::as_str),
            Some("yum-utils-4.0-1.el10")
        );
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
            proposed_updates_nvr: None,
            stream_nvr: None,
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
            proposed_updates_nvr: None,
            stream_nvr: None,
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
            proposed_updates_nvr: None,
            stream_nvr: None,
            suggestion: "no-jira",
        };
        assert_eq!(format_jira_state(&r), "unknown");
    }
}
