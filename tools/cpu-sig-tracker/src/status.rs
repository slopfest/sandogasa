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
use crate::file_issue::scan_rhel_key;
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

    /// Rewrite tracking issue bodies whose MR/JIRA lines don't
    /// match the standardized format so future runs hit the
    /// fast path. Mutates GitLab state; off by default.
    #[arg(long)]
    pub repair: bool,

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
            let parsed = resolve_body_references(body, args.verbose);
            let mr_url = parsed.mr_url.clone();
            let mr_title = parsed.mr_title.clone();
            let jira_key = parsed.jira_key.clone();

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

            // Issue path: find the project's GitLab client to
            // edit the body for --repair. Construct lazily only
            // when needed.
            if parsed.needs_repair() {
                let gaps = parsed.describe_gaps();
                if args.repair {
                    if let Some(project) = tracking_project_of(&issue.web_url)
                        && mr_url.is_some()
                    {
                        let changed = maybe_repair_issue(RepairCtx {
                            project_path: &project,
                            iid: issue.iid,
                            original_body: body,
                            release,
                            mr_url: mr_url.as_deref(),
                            mr_title: mr_title.as_deref(),
                            jira_key: jira_key.as_deref(),
                        });
                        if changed {
                            eprintln!(
                                "repaired {}: rewrote body to standard format ({gaps})",
                                issue.web_url,
                            );
                        }
                    } else {
                        eprintln!(
                            "skipped {}: {gaps}, and no MR URL found to build a repaired body",
                            issue.web_url,
                        );
                    }
                } else {
                    eprintln!(
                        "note: {}: {gaps}; run with --repair to normalize",
                        issue.web_url,
                    );
                }
            }

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

/// Fields resolved from a tracking issue body, optionally
/// augmented by following the MR.
#[derive(Debug, Default)]
struct BodyRefs {
    mr_url: Option<String>,
    mr_title: Option<String>,
    jira_key: Option<String>,
    /// Whether the structured `- **MR**: [title](url)` line was
    /// parseable from the body.
    structured_mr: bool,
    /// Whether the structured `- **JIRA**: [KEY](url)` line was
    /// parseable from the body.
    structured_jira: bool,
}

impl BodyRefs {
    /// True when at least one of the structured metadata lines
    /// is missing; `--repair` rewrites the body in that case.
    fn needs_repair(&self) -> bool {
        !self.structured_mr || !self.structured_jira
    }

    /// Short description of what's missing, for stderr.
    fn describe_gaps(&self) -> &'static str {
        match (self.structured_mr, self.structured_jira) {
            (false, false) => "missing structured MR and JIRA lines",
            (false, true) => "missing structured MR line",
            (true, false) => "missing structured JIRA line",
            (true, true) => "already standard",
        }
    }
}

/// Parse a tracking issue body for its MR URL and JIRA key.
///
/// Fast path: `- **MR**: [title](url)` and `- **JIRA**: [KEY]`
/// lines that `file-issue` emits. When either is missing we
/// fall back to lenient scanners; when JIRA is still missing
/// but the body has an MR URL, we follow the MR and scan its
/// title + description for `RHEL-\d+`.
fn resolve_body_references(body: &str, verbose: bool) -> BodyRefs {
    let structured_mr_url = structured_mr_url(body);
    let mr_url = structured_mr_url
        .clone()
        .or_else(|| scan_mr_url_in_body(body));

    let structured_jira = structured_jira_key(body);
    let jira_key = structured_jira
        .clone()
        .or_else(|| scan_rhel_key(body))
        .or_else(|| {
            mr_url
                .as_deref()
                .and_then(|u| follow_mr_for_jira(u, verbose))
        });

    let mr_title = match (&structured_mr_url, &mr_url) {
        (Some(_), _) => None, // already in body; no fetch needed
        (None, Some(url)) => fetch_mr_title(url, verbose),
        (None, None) => None,
    };

    BodyRefs {
        mr_url,
        mr_title,
        jira_key,
        structured_mr: structured_mr_url.is_some(),
        structured_jira: structured_jira.is_some(),
    }
}

/// Parse the structured `- **MR**: [title](url)` line.
fn structured_mr_url(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- **MR**: [")
            && let Some(idx) = rest.find("](")
        {
            let after = &rest[idx + 2..];
            if let Some(end) = after.find(')') {
                return Some(after[..end].to_string());
            }
        }
    }
    None
}

/// Parse the structured `- **JIRA**: [KEY](url)` line.
fn structured_jira_key(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- **JIRA**: [")
            && let Some(end) = rest.find(']')
        {
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// Scan a body for any `.../-/merge_requests/<N>` URL and
/// return it trimmed at a whitespace or closing-paren boundary.
/// Case where the body has `* Stream MR: https://.../-/merge_requests/10`
/// or similar ad-hoc forms.
fn scan_mr_url_in_body(body: &str) -> Option<String> {
    const SEP: &str = "/-/merge_requests/";
    let idx = body.find(SEP)?;
    // Walk left to find the start of the URL (https:// or http://).
    let prefix_bound = body[..idx].rfind(|c: char| c.is_whitespace() || c == '<' || c == '(');
    let start = prefix_bound.map(|p| p + 1).unwrap_or(0);
    let url_start_str = &body[start..];
    if !url_start_str.starts_with("http://") && !url_start_str.starts_with("https://") {
        return None;
    }
    // Walk right to the end of the URL — stop at whitespace,
    // closing bracket/paren/angle, or markdown punctuation.
    let rest = &body[idx + SEP.len()..];
    let suffix_len = rest
        .find(|c: char| {
            c.is_whitespace() || matches!(c, ')' | '>' | ']' | ',' | '.' | ';') || c == '`'
        })
        .unwrap_or(rest.len());
    let digits = &rest[..suffix_len];
    // Must start with at least one digit.
    if !digits.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    // Cut at the last ascii digit run.
    let digit_len = digits.chars().take_while(|c| c.is_ascii_digit()).count();
    let end = idx + SEP.len() + digit_len;
    Some(body[start..end].to_string())
}

/// Fetch the MR referenced by `url` and return its stored title,
/// for use in the standardized `- **MR**: [title](url)` line.
fn fetch_mr_title(url: &str, verbose: bool) -> Option<String> {
    let (base, project, iid) = gitlab::parse_mr_url(url).ok()?;
    let client = gitlab::Client::new(&base, &project).ok()?;
    if verbose {
        eprintln!("[cpu-sig-tracker] fetching MR !{iid} in {project} for title");
    }
    match client.merge_request(iid) {
        Ok(mr) => Some(mr.title),
        Err(e) => {
            eprintln!("warning: failed to fetch MR {url} for title: {e}");
            None
        }
    }
}

/// Fetch the MR referenced by `url`, scan its title and
/// description for a `RHEL-\d+` key, and return it.
fn follow_mr_for_jira(url: &str, verbose: bool) -> Option<String> {
    let (base, project, iid) = gitlab::parse_mr_url(url).ok()?;
    let client = gitlab::Client::new(&base, &project).ok()?;
    if verbose {
        eprintln!("[cpu-sig-tracker] fetching MR !{iid} in {project} for JIRA key");
    }
    let mr = match client.merge_request(iid) {
        Ok(mr) => mr,
        Err(e) => {
            eprintln!("warning: failed to fetch MR {url} for JIRA lookup: {e}");
            return None;
        }
    };
    if let Some(key) = scan_rhel_key(&mr.title) {
        return Some(key);
    }
    mr.description.as_deref().and_then(scan_rhel_key)
}

/// Given a tracking issue `web_url` like
/// `https://gitlab.com/CentOS/proposed_updates/rpms/<pkg>/-/work_items/<n>`,
/// return the project path `CentOS/proposed_updates/rpms/<pkg>`
/// so callers can construct a project-scoped client.
fn tracking_project_of(web_url: &str) -> Option<String> {
    sandogasa_gitlab::project_path_from_issue_url(web_url)
}

/// Inputs for [`maybe_repair_issue`].
struct RepairCtx<'a> {
    project_path: &'a str,
    iid: u64,
    original_body: &'a str,
    release: &'a str,
    mr_url: Option<&'a str>,
    mr_title: Option<&'a str>,
    jira_key: Option<&'a str>,
}

/// Rewrite the tracking issue body to the standardized format
/// so future runs hit the fast path. Keeps the original prose
/// as a lead paragraph and appends the structured metadata.
/// Returns `true` when the body was actually changed.
fn maybe_repair_issue(ctx: RepairCtx<'_>) -> bool {
    let Some(url) = ctx.mr_url else { return false };
    let title = ctx.mr_title.unwrap_or(url);
    let new_body = repaired_body(ctx.original_body, ctx.release, url, title, ctx.jira_key);
    if new_body == ctx.original_body {
        return false;
    }
    let client = match gitlab::Client::new(GITLAB_BASE, ctx.project_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "warning: --repair: could not build client for {}: {e}",
                ctx.project_path,
            );
            return false;
        }
    };
    let update = gitlab::IssueUpdate {
        description: Some(new_body),
        ..Default::default()
    };
    if let Err(e) = client.edit_issue(ctx.iid, &update) {
        eprintln!(
            "warning: --repair: failed to update {}!{}: {e}",
            ctx.project_path, ctx.iid,
        );
        return false;
    }
    true
}

/// Build a standardized body, preserving the original prose
/// (trimmed) as a lead paragraph when it isn't already just
/// the structured metadata.
fn repaired_body(
    original_body: &str,
    release: &str,
    mr_url: &str,
    mr_title: &str,
    jira_key: Option<&str>,
) -> String {
    let lead = extract_lead_paragraph(original_body);
    let jira_line = match jira_key {
        Some(key) => {
            let url = format!("https://issues.redhat.com/browse/{key}");
            format!("- **JIRA**: [{key}]({url})")
        }
        None => "- **JIRA**: _(not found in MR; set with `--jira`)_".to_string(),
    };
    let metadata = format!(
        "- **MR**: [{mr_title}]({mr_url})\n\
         {jira_line}\n\
         - **Release**: {release}\n\
         - **Status**: open\n",
    );
    match lead {
        Some(lead) if !lead.is_empty() => format!("{lead}\n\n{metadata}"),
        _ => metadata,
    }
}

/// Strip the existing structured metadata lines (and their
/// blank-line separator) from a body, leaving only the prose
/// that preceded them. Returns None if there's no prose.
fn extract_lead_paragraph(body: &str) -> Option<String> {
    let mut lead_lines: Vec<&str> = Vec::new();
    for line in body.lines() {
        if is_metadata_line(line) {
            break;
        }
        lead_lines.push(line);
    }
    let joined = lead_lines.join("\n");
    let trimmed = joined.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn is_metadata_line(line: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "- **MR**:",
        "- **JIRA**:",
        "- **Release**:",
        "- **Affected build**:",
        "- **Expected fix**:",
        "- **Status**:",
        "* Stream MR:",
        "* Affected",
        "* Expected",
    ];
    let trimmed = line.trim_start();
    PREFIXES.iter().any(|p| trimmed.starts_with(p))
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
    fn structured_mr_url_from_standard_body() {
        assert_eq!(
            structured_mr_url(SAMPLE_BODY).as_deref(),
            Some("https://gitlab.com/foo/bar/-/merge_requests/3"),
        );
    }

    #[test]
    fn structured_jira_key_from_standard_body() {
        assert_eq!(
            structured_jira_key(SAMPLE_BODY).as_deref(),
            Some("RHEL-12345"),
        );
    }

    #[test]
    fn structured_mr_url_missing_line_returns_none() {
        let body = "- **JIRA**: [RHEL-1](https://example/)\n";
        assert_eq!(structured_mr_url(body), None);
    }

    #[test]
    fn structured_jira_key_missing_line_returns_none() {
        let body = "- **MR**: [t](https://example/)\n";
        assert_eq!(structured_jira_key(body), None);
    }

    #[test]
    fn structured_jira_key_skips_placeholder() {
        // file-issue emits a placeholder when no JIRA key is
        // auto-extracted; structured_jira_key shouldn't treat
        // the placeholder text as a real key.
        let body = "- **JIRA**: _(not found in MR; set with `--jira`)_\n";
        // The placeholder doesn't start with `[` so the
        // structured parser returns None.
        assert_eq!(structured_jira_key(body), None);
    }

    #[test]
    fn scan_mr_url_handles_bullet_label() {
        let body = "\
            * Stream MR: https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/10\n\
            * Affected version-release: 1:5.6.2-3.el10\n";
        assert_eq!(
            scan_mr_url_in_body(body).as_deref(),
            Some("https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/10"),
        );
    }

    #[test]
    fn scan_mr_url_handles_markdown_link() {
        let body = "See [the MR](https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42) for details.";
        assert_eq!(
            scan_mr_url_in_body(body).as_deref(),
            Some("https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42"),
        );
    }

    #[test]
    fn scan_mr_url_returns_none_when_absent() {
        assert_eq!(scan_mr_url_in_body("no merge request here"), None);
    }

    #[test]
    fn scan_mr_url_requires_digits() {
        // Path present but no numeric iid — reject.
        assert_eq!(
            scan_mr_url_in_body("https://gitlab.com/foo/bar/-/merge_requests/"),
            None
        );
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
    fn extract_lead_paragraph_preserves_prose() {
        let body = "\
            Context about why we're tracking.\n\
            \n\
            - **MR**: [t](https://example/mr)\n\
            - **JIRA**: [RHEL-1](https://example/)\n";
        assert_eq!(
            extract_lead_paragraph(body).as_deref(),
            Some("Context about why we're tracking."),
        );
    }

    #[test]
    fn extract_lead_paragraph_returns_none_when_only_metadata() {
        let body = "- **MR**: [t](https://example/mr)\n- **JIRA**: [RHEL-1](https://example/)\n";
        assert_eq!(extract_lead_paragraph(body), None);
    }

    #[test]
    fn extract_lead_paragraph_stops_at_legacy_bullet() {
        let body = "\
            Fix for CVE-2024-X.\n\
            \n\
            * Stream MR: https://example/-/merge_requests/1\n\
            * Affected version-release: 1.0-1\n";
        assert_eq!(
            extract_lead_paragraph(body).as_deref(),
            Some("Fix for CVE-2024-X."),
        );
    }

    #[test]
    fn repaired_body_with_jira_and_lead() {
        let original = "\
            Legacy context paragraph.\n\
            \n\
            * Stream MR: https://example/-/merge_requests/1\n";
        let out = repaired_body(
            original,
            "c10s",
            "https://example/-/merge_requests/1",
            "Fix CVE-2026-0001",
            Some("RHEL-12345"),
        );
        assert!(out.starts_with("Legacy context paragraph.\n\n- **MR**:"));
        assert!(out.contains("- **MR**: [Fix CVE-2026-0001](https://example/-/merge_requests/1)"));
        assert!(
            out.contains("- **JIRA**: [RHEL-12345](https://issues.redhat.com/browse/RHEL-12345)")
        );
        assert!(out.contains("- **Release**: c10s"));
        assert!(out.contains("- **Status**: open"));
    }

    #[test]
    fn repaired_body_without_jira_uses_placeholder() {
        let out = repaired_body("", "c10s", "https://example/-/merge_requests/1", "t", None);
        assert!(out.contains("- **JIRA**: _(not found in MR; set with `--jira`)_"));
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
