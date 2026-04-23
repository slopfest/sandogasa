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
use crate::utils::gitlab_base;
const KOJI_PROFILE: &str = "cbs";

#[derive(clap::Args)]
pub struct StatusArgs {
    /// Path to the sandogasa-inventory TOML file.
    #[arg(short, long, default_value = "inventory.toml")]
    pub inventory: String,

    /// Restrict the check to a single release (e.g. `c10s`).
    #[arg(long)]
    pub release: Option<String>,

    /// Narrow to specific packages (repeatable or CSV). When
    /// unset, every tracked package in scope is processed.
    /// Primarily useful with --refresh to skip the extra
    /// work-item-status probes for packages you don't care
    /// about.
    #[arg(short = 'p', long = "package", value_delimiter = ',')]
    pub packages: Vec<String>,

    /// Emit JSON instead of grouped text.
    #[arg(long)]
    pub json: bool,

    /// Rewrite tracking issue bodies to the standardized
    /// format, refreshing the MR state and JIRA status lines
    /// to their current values. Normalizes legacy bodies and
    /// keeps already-standard bodies up to date. Mutates
    /// GitLab state; off by default.
    #[arg(long)]
    pub refresh: bool,

    /// With --refresh, also process closed tracking issues —
    /// backfill their start_date / due_date and reconcile
    /// their work-item status against the current JIRA
    /// resolution (e.g. Done → Won't do when JIRA was
    /// retroactively flipped). Body content is left alone.
    /// No-op without --refresh.
    #[arg(long, requires = "refresh")]
    pub include_closed: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Row {
    pub release: String,
    pub package: String,
    /// GitLab issue state: "opened" or "closed".
    pub issue_state: String,
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
    match build_rows(args) {
        Ok(rows) => {
            if args.json {
                match serde_json::to_string_pretty(&rows) {
                    Ok(j) => println!("{j}"),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print_human(&rows);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn build_rows(args: &StatusArgs) -> Result<Vec<Row>, Box<dyn std::error::Error>> {
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

    let group_client = gitlab::GroupClient::new(&gitlab_base(), PROPOSED_UPDATES_GROUP)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let jira_client = jira::client();

    let package_filter: Option<std::collections::HashSet<&str>> = if args.packages.is_empty() {
        None
    } else {
        Some(args.packages.iter().map(|s| s.as_str()).collect())
    };

    let mut rows: Vec<Row> = Vec::new();
    for release in &releases {
        let state_filter = if args.include_closed {
            // Pass None → GitLab returns both opened and closed.
            None
        } else {
            Some("opened")
        };
        if args.verbose {
            eprintln!(
                "[cpu-sig-tracker] fetching {} tracking issues for {release}",
                state_filter.unwrap_or("all-state"),
            );
        }
        let active_label = format!("{TRACKING_LABEL},{release}");
        let active_all = group_client.list_issues(&active_label, state_filter)?;

        // Client-side narrow by --package. GitLab can't filter
        // issues by the project name they belong to, so we do it
        // after the group list returns.
        let active: Vec<gitlab::Issue> = active_all
            .into_iter()
            .filter(|i| match &package_filter {
                None => true,
                Some(set) => {
                    gitlab::package_from_issue_url(&i.web_url).is_some_and(|p| set.contains(p))
                }
            })
            .collect();

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

        // Testing-tag NVRs are only needed for the work-item
        // status reconciliation in --refresh, so skip the extra
        // Koji call otherwise.
        let testing_nvrs = if args.refresh {
            if args.verbose {
                eprintln!("[cpu-sig-tracker] fetching testing tag for {release}");
            }
            fetch_proposed_updates_testing_nvrs(release, args.verbose)
        } else {
            HashMap::new()
        };

        if args.verbose {
            eprintln!("[cpu-sig-tracker] fetching Stream NVRs for {release}");
        }
        let stream_nvrs = fetch_stream_nvrs(release, &tracked_packages, args.verbose);

        for issue in active {
            let Some(package) = gitlab::package_from_issue_url(&issue.web_url) else {
                continue;
            };

            let body = issue.description.as_deref().unwrap_or("");
            let parsed = resolve_body_references(body, args.verbose, args.refresh);
            let mr_url = parsed.mr_url.clone();
            let mr_title = parsed.mr_title.clone();
            let mr_state = parsed.mr_state.clone();
            let jira_key = parsed.jira_key.clone();

            let jira_info = match jira_key.as_deref() {
                Some(key) => fetch_jira(&runtime, &jira_client, key, args.verbose),
                None => None,
            };
            let (jira_status, jira_resolution, jira_resolved, jira_resolution_date) =
                match jira_info {
                    Some(info) => (
                        Some(info.status),
                        info.resolution,
                        info.resolved,
                        info.resolution_date,
                    ),
                    None => (None, None, false, None),
                };

            let pu_nvr = pu_nvrs.get(package).cloned();
            let stream_nvr = stream_nvrs.get(package).cloned();

            let suggestion = if issue.state == "closed" {
                // Closed issue with a lingering pu build still
                // needs an untag; otherwise nothing to do.
                if pu_nvr.is_some() {
                    "untag-candidate"
                } else {
                    "—"
                }
            } else {
                suggest_next_action(
                    jira_key.as_deref(),
                    jira_resolved,
                    pu_nvr.as_deref(),
                    stream_nvr.as_deref(),
                )
            };

            // Body-vs-live drift detection. Format drift is
            // free (pure body parse); JIRA drift compares the
            // body's current JIRA suffix to the canonical form
            // of the JIRA fetch.
            let jira_drift = jira_suffix_drift(
                parsed.body_jira_suffix.as_deref(),
                jira_status.as_deref(),
                jira_resolution.as_deref(),
            );
            let drift_reasons = {
                let mut reasons: Vec<String> = Vec::new();
                if parsed.needs_refresh() {
                    reasons.push(parsed.describe_gaps());
                }
                if jira_drift {
                    reasons.push("JIRA status drifted from live value".to_string());
                }
                reasons
            };

            let is_closed = issue.state == "closed";

            // Closed issues are historical: we leave the body
            // alone but still reconcile work-item status and
            // backfill missing dates so the GitLab view stays
            // accurate (e.g. a JIRA that later flipped to
            // Won't Do → Done gets its GitLab status corrected).
            // Open issues get the full refresh.
            if args.refresh {
                let project = tracking_project_of(&issue.web_url);
                if is_closed {
                    if let Some(project) = project {
                        let has_build =
                            pu_nvrs.contains_key(package) || testing_nvrs.contains_key(package);
                        maybe_refresh_work_item_status(
                            &project,
                            &issue,
                            jira_resolved,
                            jira_resolution.as_deref(),
                            has_build,
                            args.verbose,
                        );
                        maybe_refresh_dates(
                            &project,
                            &issue,
                            package,
                            release,
                            jira_resolution_date,
                            args.verbose,
                        );
                    }
                } else if let (Some(project), true) = (project, mr_url.is_some()) {
                    let changed = maybe_refresh_issue(RefreshCtx {
                        project_path: &project,
                        iid: issue.iid,
                        original_body: body,
                        release,
                        mr_url: mr_url.as_deref(),
                        mr_title: mr_title.as_deref(),
                        mr_state: mr_state.as_deref(),
                        jira_key: jira_key.as_deref(),
                        jira_status: jira_status.as_deref(),
                        jira_resolution: jira_resolution.as_deref(),
                    });
                    if changed {
                        eprintln!("refreshed {}: rewrote body", issue.web_url);
                    }
                    let has_build =
                        pu_nvrs.contains_key(package) || testing_nvrs.contains_key(package);
                    maybe_refresh_work_item_status(
                        &project,
                        &issue,
                        jira_resolved,
                        jira_resolution.as_deref(),
                        has_build,
                        args.verbose,
                    );
                    maybe_refresh_dates(
                        &project,
                        &issue,
                        package,
                        release,
                        jira_resolution_date,
                        args.verbose,
                    );
                } else if !drift_reasons.is_empty() {
                    eprintln!(
                        "skipped {}: {}, and no MR URL found",
                        issue.web_url,
                        drift_reasons.join("; "),
                    );
                }
            } else if !is_closed && !drift_reasons.is_empty() {
                eprintln!(
                    "note: {}: {}; run with --refresh to update",
                    issue.web_url,
                    drift_reasons.join("; "),
                );
            }

            // By default the status table is in-flight only.
            // --include-closed opts those issues back in so
            // users can see the full picture of what the
            // refresh touched.
            if is_closed && !args.include_closed {
                continue;
            }

            rows.push(Row {
                release: release.to_string(),
                package: package.to_string(),
                issue_state: issue.state.clone(),
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
    Ok(rows)
}

/// Fields resolved from a tracking issue body, possibly
/// augmented by fetching the MR it references.
#[derive(Debug, Default)]
struct BodyRefs {
    mr_url: Option<String>,
    mr_title: Option<String>,
    mr_state: Option<String>,
    jira_key: Option<String>,
    /// Whether the structured `- **MR**: [title](url)` line was
    /// parseable from the body.
    structured_mr: bool,
    /// Whether the MR line in the body carries a ` — <state>`
    /// suffix (present in the current canonical format).
    mr_line_has_suffix: bool,
    /// Whether the structured `- **JIRA**: [KEY](url)` line was
    /// parseable from the body.
    structured_jira: bool,
    /// Whether the JIRA line in the body carries a ` — <status>`
    /// suffix (present in the current canonical format).
    jira_line_has_suffix: bool,
    /// The `<status>` or `<status> (<resolution>)` text that
    /// currently sits on the JIRA line, for drift comparison
    /// against the live JIRA API value.
    body_jira_suffix: Option<String>,
    /// Whether the body still contains legacy format artifacts
    /// that the current canonical format dropped (standalone
    /// `- **Status**:` line, `* Stream MR:` bullet, etc.).
    has_legacy_lines: bool,
}

impl BodyRefs {
    /// True when the body doesn't match the canonical format
    /// (any structured line missing, any required suffix
    /// absent, or a legacy line hanging around).
    fn needs_refresh(&self) -> bool {
        !self.structured_mr
            || !self.structured_jira
            || !self.mr_line_has_suffix
            || !self.jira_line_has_suffix
            || self.has_legacy_lines
    }

    /// Comma-joined list of drift reasons for stderr.
    fn describe_gaps(&self) -> String {
        let mut gaps: Vec<&str> = Vec::new();
        if !self.structured_mr {
            gaps.push("missing structured MR line");
        } else if !self.mr_line_has_suffix {
            gaps.push("MR line missing state suffix");
        }
        if !self.structured_jira {
            gaps.push("missing structured JIRA line");
        } else if !self.jira_line_has_suffix {
            gaps.push("JIRA line missing status suffix");
        }
        if self.has_legacy_lines {
            gaps.push("legacy metadata lines present");
        }
        if gaps.is_empty() {
            "already standard".to_string()
        } else {
            gaps.join("; ")
        }
    }
}

/// Parse a tracking issue body for its MR URL and JIRA key.
///
/// Fast path: `- **MR**: [title](url) — state` and
/// `- **JIRA**: [KEY](url) — status` lines that `file-issue`
/// emits. When either is missing we fall back to lenient
/// scanners. Fetches the MR once when any of (a) the caller
/// forces it (for refresh-time state), (b) structured MR line
/// absent so we need the title, (c) structured JIRA missing
/// so we scan the MR description for a RHEL key.
fn resolve_body_references(body: &str, verbose: bool, force_mr_fetch: bool) -> BodyRefs {
    let structured_mr = parse_mr_line(body);
    let mr_url_from_body = structured_mr.as_ref().map(|(u, _)| u.clone());
    let mr_url = mr_url_from_body
        .clone()
        .or_else(|| scan_mr_url_in_body(body));

    let structured_jira = parse_jira_line(body);
    let jira_from_body = structured_jira
        .as_ref()
        .map(|(k, _)| k.clone())
        .or_else(|| scan_rhel_key(body));

    let should_fetch_mr =
        mr_url.is_some() && (force_mr_fetch || structured_mr.is_none() || jira_from_body.is_none());
    let mr = if should_fetch_mr {
        mr_url.as_deref().and_then(|u| fetch_mr(u, verbose))
    } else {
        None
    };

    let jira_key = jira_from_body.or_else(|| {
        mr.as_ref().and_then(|m| {
            scan_rhel_key(&m.title).or_else(|| m.description.as_deref().and_then(scan_rhel_key))
        })
    });

    BodyRefs {
        mr_url,
        mr_title: mr.as_ref().map(|m| m.title.clone()),
        mr_state: mr.as_ref().map(|m| m.state.clone()),
        jira_key,
        structured_mr: structured_mr.is_some(),
        mr_line_has_suffix: structured_mr
            .as_ref()
            .map(|(_, s)| s.is_some())
            .unwrap_or(false),
        structured_jira: structured_jira.is_some(),
        jira_line_has_suffix: structured_jira
            .as_ref()
            .map(|(_, s)| s.is_some())
            .unwrap_or(false),
        body_jira_suffix: structured_jira.and_then(|(_, s)| s),
        has_legacy_lines: body.lines().any(is_legacy_line),
    }
}

/// Parse the structured `- **MR**: [title](url)[ — state]`
/// line. Returns (url, state) where state is the trimmed text
/// after ` — ` / ` -- ` following the URL's closing paren.
fn parse_mr_line(body: &str) -> Option<(String, Option<String>)> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- **MR**: [")
            && let Some(idx) = rest.find("](")
        {
            let after = &rest[idx + 2..];
            if let Some(end) = after.find(')') {
                let url = after[..end].to_string();
                let tail = after[end + 1..].trim_start();
                let state = strip_em_dash(tail)
                    .map(str::trim)
                    .map(str::to_string)
                    .filter(|s| !s.is_empty());
                return Some((url, state));
            }
        }
    }
    None
}

/// Parse the structured `- **JIRA**: [KEY](url)[ — suffix]`
/// line. Returns (key, suffix) where suffix is the trimmed
/// text after ` — ` / ` -- ` following the URL's closing paren.
fn parse_jira_line(body: &str) -> Option<(String, Option<String>)> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- **JIRA**: [")
            && let Some(key_end) = rest.find(']')
        {
            let key = rest[..key_end].to_string();
            // Skip over the `(url)` part to find the trailing
            // suffix, tolerating no trailing content.
            let after_key = &rest[key_end + 1..];
            let suffix = if let Some(after) = after_key.strip_prefix('(')
                && let Some(end) = after.find(')')
            {
                let tail = after[end + 1..].trim_start();
                strip_em_dash(tail)
                    .map(str::trim)
                    .map(str::to_string)
                    .filter(|s| !s.is_empty())
            } else {
                None
            };
            return Some((key, suffix));
        }
    }
    None
}

/// Strip a leading em-dash or double-hyphen used as a suffix
/// separator and return the text that follows.
fn strip_em_dash(s: &str) -> Option<&str> {
    s.strip_prefix("— ")
        .or_else(|| s.strip_prefix("—"))
        .or_else(|| s.strip_prefix("-- "))
        .or_else(|| s.strip_prefix("--"))
}

/// Compare the JIRA suffix on the body's JIRA line to the
/// canonical suffix derived from the live JIRA fetch. Returns
/// true when they differ (body-state has drifted). Missing
/// live data short-circuits to `false` — we don't claim drift
/// when we can't compare.
fn jira_suffix_drift(
    body_suffix: Option<&str>,
    live_status: Option<&str>,
    live_resolution: Option<&str>,
) -> bool {
    let canonical = canonical_jira_suffix(live_status, live_resolution);
    match (body_suffix, canonical.as_deref()) {
        (Some(body), Some(live)) => body != live,
        (None, Some(_)) => true,
        (Some(_), None) | (None, None) => false,
    }
}

/// The `<status>` or `<status> (<resolution>)` text we'd put
/// after ` — ` on the JIRA line. None when no live status.
fn canonical_jira_suffix(status: Option<&str>, resolution: Option<&str>) -> Option<String> {
    match (status, resolution) {
        (Some(s), Some(r)) => Some(format!("{s} ({r})")),
        (Some(s), None) => Some(s.to_string()),
        (None, _) => None,
    }
}

/// Detect lines from older body layouts that the current
/// canonical format no longer emits (standalone Status line,
/// legacy bullet-style MR/affected rows).
fn is_legacy_line(line: &str) -> bool {
    const PREFIXES: &[&str] = &["- **Status**:", "* Stream MR:", "* Affected", "* Expected"];
    let trimmed = line.trim_start();
    PREFIXES.iter().any(|p| trimmed.starts_with(p))
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

/// Fetch the MR referenced by `url`. Returns the full
/// [`MergeRequest`] so callers can pull whichever fields they
/// need (title, state, description).
fn fetch_mr(url: &str, verbose: bool) -> Option<gitlab::MergeRequest> {
    let (_parsed_base, project, iid) = gitlab::parse_mr_url(url).ok()?;
    let client = gitlab::Client::new(&gitlab_base(), &project).ok()?;
    if verbose {
        eprintln!("[cpu-sig-tracker] fetching MR !{iid} in {project}");
    }
    match client.merge_request(iid) {
        Ok(mr) => Some(mr),
        Err(e) => {
            eprintln!("warning: failed to fetch MR {url}: {e}");
            None
        }
    }
}

/// Given a tracking issue `web_url` like
/// `https://gitlab.com/CentOS/proposed_updates/rpms/<pkg>/-/work_items/<n>`,
/// return the project path `CentOS/proposed_updates/rpms/<pkg>`
/// so callers can construct a project-scoped client.
fn tracking_project_of(web_url: &str) -> Option<String> {
    sandogasa_gitlab::project_path_from_issue_url(web_url)
}

/// Pick the GitLab work-item status that matches current
/// reality.
///
/// - JIRA resolved → `gitlab_status_for_resolution` (either
///   `Done` or `Won't do` depending on how JIRA was closed).
/// - JIRA open + a build is tagged in `-release` or `-testing`
///   → `In progress` (work is in flight).
/// - JIRA open + no build tagged anywhere → `To do` (the
///   not-yet-tagged case).
fn desired_work_item_status(
    jira_resolved: bool,
    jira_resolution: Option<&str>,
    has_build: bool,
) -> &'static str {
    if jira_resolved {
        gitlab_status_for_resolution(jira_resolution)
    } else if has_build {
        "In progress"
    } else {
        "To do"
    }
}

/// Map a JIRA resolution name to the closest GitLab work-item
/// status. Only `Done` / `Fixed` / `Resolved` count as a real
/// fix — every other resolution (Won't Do, Obsolete, Cannot
/// Reproduce, …) maps to `Won't do`.
pub(crate) fn gitlab_status_for_resolution(resolution: Option<&str>) -> &'static str {
    match resolution {
        Some("Done" | "Fixed" | "Resolved") => "Done",
        _ => "Won't do",
    }
}

/// Compare the current GitLab work-item status to what it
/// should be and update it when different. Logged failures are
/// non-fatal — body refresh still counts as progress.
fn maybe_refresh_work_item_status(
    project_path: &str,
    issue: &gitlab::Issue,
    jira_resolved: bool,
    jira_resolution: Option<&str>,
    has_build: bool,
    verbose: bool,
) {
    let desired = desired_work_item_status(jira_resolved, jira_resolution, has_build);
    let client = match gitlab::Client::new(&gitlab_base(), project_path) {
        Ok(c) => c,
        Err(e) => {
            if verbose {
                eprintln!("warning: --refresh work-item status: client for {project_path}: {e}",);
            }
            return;
        }
    };
    let current = match client.get_work_item_status(issue.iid) {
        Ok(c) => c,
        Err(e) => {
            if verbose {
                eprintln!(
                    "warning: --refresh work-item status: fetch for {}!{}: {e}",
                    project_path, issue.iid,
                );
            }
            return;
        }
    };
    if current.as_deref() == Some(desired) {
        return;
    }
    if let Err(e) = client.set_work_item_status(issue.iid, desired) {
        eprintln!(
            "warning: --refresh work-item status: set to {desired} for {}: {e}",
            issue.web_url,
        );
        return;
    }
    eprintln!(
        "work-item status {}: {} → {desired}",
        issue.web_url,
        current.as_deref().unwrap_or("<none>"),
    );
}

/// Reconcile the issue's start_date / due_date with what the
/// live data says.
///
/// - `start_date`: Koji build creation date (probes `-release`
///   then `-testing`). Falls back to the issue's created_at
///   when Koji no longer has the build (retired). Non-fatal on
///   any failure.
/// - `due_date`: JIRA's resolutiondate when the issue is
///   resolved; otherwise left alone.
fn maybe_refresh_dates(
    project_path: &str,
    issue: &gitlab::Issue,
    package: &str,
    release: &str,
    jira_resolution_date: Option<chrono::NaiveDate>,
    verbose: bool,
) {
    let desired_start = crate::file_issue::find_build_start_date(package, release, verbose)
        .or_else(|| {
            issue
                .created_at
                .as_deref()
                .and_then(crate::utils::parse_iso_date)
        });
    let desired_due = jira_resolution_date;

    let current_start_parsed = issue
        .start_date
        .as_deref()
        .and_then(crate::utils::parse_iso_date);
    let current_due_parsed = issue
        .due_date
        .as_deref()
        .and_then(crate::utils::parse_iso_date);

    let start_needs_update = desired_start.is_some() && current_start_parsed != desired_start;
    let due_needs_update = desired_due.is_some() && current_due_parsed != desired_due;

    if !start_needs_update && !due_needs_update {
        return;
    }

    let client = match gitlab::Client::new(&gitlab_base(), project_path) {
        Ok(c) => c,
        Err(e) => {
            if verbose {
                eprintln!("warning: --refresh dates: client for {project_path}: {e}");
            }
            return;
        }
    };

    let start_arg = if start_needs_update {
        desired_start.map(|d| d.format("%Y-%m-%d").to_string())
    } else {
        None
    };
    let due_arg = if due_needs_update {
        desired_due.map(|d| d.format("%Y-%m-%d").to_string())
    } else {
        None
    };

    if let Err(e) = client.set_work_item_dates(issue.iid, start_arg.as_deref(), due_arg.as_deref())
    {
        eprintln!("warning: --refresh dates for {}: {e}", issue.web_url);
        return;
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(d) = &start_arg {
        parts.push(format!("start_date={d}"));
    }
    if let Some(d) = &due_arg {
        parts.push(format!("due_date={d}"));
    }
    eprintln!("dates {}: {}", issue.web_url, parts.join(", "));
}

/// Inputs for [`maybe_refresh_issue`].
struct RefreshCtx<'a> {
    project_path: &'a str,
    iid: u64,
    original_body: &'a str,
    release: &'a str,
    mr_url: Option<&'a str>,
    mr_title: Option<&'a str>,
    mr_state: Option<&'a str>,
    jira_key: Option<&'a str>,
    jira_status: Option<&'a str>,
    jira_resolution: Option<&'a str>,
}

/// Rewrite the tracking issue body to the standardized format
/// so future runs hit the fast path. Keeps the original prose
/// as a lead paragraph and appends the structured metadata.
/// Returns `true` when the body was actually changed.
fn maybe_refresh_issue(ctx: RefreshCtx<'_>) -> bool {
    let Some(url) = ctx.mr_url else { return false };
    let title = ctx.mr_title.unwrap_or(url);
    let new_body = refreshed_body(RefreshedBodyArgs {
        original_body: ctx.original_body,
        release: ctx.release,
        mr_url: url,
        mr_title: title,
        mr_state: ctx.mr_state,
        jira_key: ctx.jira_key,
        jira_status: ctx.jira_status,
        jira_resolution: ctx.jira_resolution,
    });
    // GitLab strips trailing whitespace/newlines from stored
    // descriptions, so compare trimmed values — otherwise the
    // `\n` we always emit at the end triggers a rewrite every
    // run even when nothing changed.
    if new_body.trim_end() == ctx.original_body.trim_end() {
        return false;
    }
    let client = match gitlab::Client::new(&gitlab_base(), ctx.project_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "warning: --refresh: could not build client for {}: {e}",
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
            "warning: --refresh: failed to update {}!{}: {e}",
            ctx.project_path, ctx.iid,
        );
        return false;
    }
    true
}

/// Inputs for [`refreshed_body`].
struct RefreshedBodyArgs<'a> {
    original_body: &'a str,
    release: &'a str,
    mr_url: &'a str,
    mr_title: &'a str,
    mr_state: Option<&'a str>,
    jira_key: Option<&'a str>,
    jira_status: Option<&'a str>,
    jira_resolution: Option<&'a str>,
}

/// Build a standardized body, preserving the original prose
/// (trimmed) as a lead paragraph when it isn't already just
/// the structured metadata.
fn refreshed_body(a: RefreshedBodyArgs<'_>) -> String {
    let lead = extract_lead_paragraph(a.original_body);
    let mr_line = format_mr_line(a.mr_url, a.mr_title, a.mr_state);
    let jira_line = format_jira_line(a.jira_key, a.jira_status, a.jira_resolution);
    let metadata = format!(
        "{mr_line}\n\
         {jira_line}\n\
         - **Release**: {}\n",
        a.release,
    );
    match lead {
        Some(lead) if !lead.is_empty() => format!("{lead}\n\n{metadata}"),
        _ => metadata,
    }
}

/// Build `- **MR**: [title](url) — state` (suffix omitted when
/// state is unknown).
fn format_mr_line(mr_url: &str, mr_title: &str, mr_state: Option<&str>) -> String {
    match mr_state {
        Some(state) => format!("- **MR**: [{mr_title}]({mr_url}) — {state}"),
        None => format!("- **MR**: [{mr_title}]({mr_url})"),
    }
}

/// Build `- **JIRA**: [KEY](url) — status (resolution)` with
/// graceful degradation for missing fields.
fn format_jira_line(
    jira_key: Option<&str>,
    jira_status: Option<&str>,
    jira_resolution: Option<&str>,
) -> String {
    let Some(key) = jira_key else {
        return "- **JIRA**: _(not found in MR; set with `--jira`)_".to_string();
    };
    let url = format!("{}/browse/{key}", crate::utils::jira_base());
    let suffix = match (jira_status, jira_resolution) {
        (Some(s), Some(r)) => format!(" — {s} ({r})"),
        (Some(s), None) => format!(" — {s}"),
        (None, _) => String::new(),
    };
    format!("- **JIRA**: [{key}]({url}){suffix}")
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

struct JiraInfo {
    status: String,
    resolution: Option<String>,
    resolved: bool,
    resolution_date: Option<chrono::NaiveDate>,
}

fn fetch_jira(
    runtime: &tokio::runtime::Runtime,
    client: &sandogasa_jira::JiraClient,
    key: &str,
    verbose: bool,
) -> Option<JiraInfo> {
    if verbose {
        eprintln!("[cpu-sig-tracker] fetching JIRA {key}");
    }
    match runtime.block_on(client.issue(key)) {
        Ok(Some(issue)) => Some(JiraInfo {
            status: issue.status().to_string(),
            resolution: issue.resolution().map(|s| s.to_string()),
            resolved: issue.is_resolved(),
            resolution_date: issue.resolution_date(),
        }),
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
    match proposed_updates_tag(release) {
        Ok(tag) => fetch_koji_nvrs(&tag, verbose),
        Err(e) => {
            eprintln!("warning: {e}; skipping proposed_updates NVR lookup");
            HashMap::new()
        }
    }
}

fn fetch_proposed_updates_testing_nvrs(release: &str, verbose: bool) -> HashMap<String, String> {
    match crate::dump_inventory::proposed_updates_testing_tag(release) {
        Ok(tag) => fetch_koji_nvrs(&tag, verbose),
        Err(e) => {
            eprintln!("warning: {e}; skipping testing-tag NVR lookup");
            HashMap::new()
        }
    }
}

fn fetch_koji_nvrs(tag: &str, verbose: bool) -> HashMap<String, String> {
    match list_tagged(tag, Some(KOJI_PROFILE), None) {
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

/// Extract the "version-release" portion of an NVR for display.
/// Returns None when the NVR doesn't parse cleanly.
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
    // Closed GitLab issues trump JIRA detail — "closed" is the
    // clearest signal to UI readers and makes --include-closed
    // rows scan-able at a glance.
    if r.issue_state == "closed" {
        return "closed".to_string();
    }
    match (&r.jira_status, &r.jira_resolution) {
        (Some(status), Some(resolution)) => format!("{status} ({resolution})"),
        (Some(status), None) => status.clone(),
        (None, _) => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mr_line_returns_url_and_state() {
        let body = "- **MR**: [title](https://example/-/merge_requests/3) — merged\n";
        let (url, state) = parse_mr_line(body).unwrap();
        assert_eq!(url, "https://example/-/merge_requests/3");
        assert_eq!(state.as_deref(), Some("merged"));
    }

    #[test]
    fn parse_mr_line_without_state_suffix() {
        let body = "- **MR**: [title](https://example/-/merge_requests/3)\n";
        let (url, state) = parse_mr_line(body).unwrap();
        assert_eq!(url, "https://example/-/merge_requests/3");
        assert_eq!(state, None);
    }

    #[test]
    fn parse_jira_line_returns_key_and_suffix() {
        let body = "- **JIRA**: [RHEL-1](https://example/) — Closed (Done)\n";
        let (key, suffix) = parse_jira_line(body).unwrap();
        assert_eq!(key, "RHEL-1");
        assert_eq!(suffix.as_deref(), Some("Closed (Done)"));
    }

    #[test]
    fn parse_jira_line_without_suffix() {
        let body = "- **JIRA**: [RHEL-1](https://example/)\n";
        let (_, suffix) = parse_jira_line(body).unwrap();
        assert_eq!(suffix, None);
    }

    #[test]
    fn parse_jira_line_skips_placeholder() {
        // Placeholder text doesn't start with `[` so parser
        // returns None rather than a bogus key.
        let body = "- **JIRA**: _(not found in MR; set with `--jira`)_\n";
        assert_eq!(parse_jira_line(body), None);
    }

    #[test]
    fn parse_mr_line_missing_returns_none() {
        let body = "- **JIRA**: [RHEL-1](https://example/)\n";
        assert_eq!(parse_mr_line(body), None);
    }

    #[test]
    fn jira_suffix_drift_matches_canonical() {
        assert!(!jira_suffix_drift(
            Some("Closed (Done)"),
            Some("Closed"),
            Some("Done"),
        ));
        assert!(!jira_suffix_drift(Some("New"), Some("New"), None));
    }

    #[test]
    fn jira_suffix_drift_detects_stale_body() {
        assert!(jira_suffix_drift(Some("New"), Some("Closed"), Some("Done"),));
        assert!(jira_suffix_drift(Some("In Progress"), Some("Closed"), None,));
    }

    #[test]
    fn jira_suffix_drift_body_missing_but_live_known() {
        // Body has no suffix, live JIRA has a status — that's
        // drift the user should fix.
        assert!(jira_suffix_drift(None, Some("New"), None));
    }

    #[test]
    fn jira_suffix_drift_no_live_data() {
        // Without live data we can't tell; don't cry drift.
        assert!(!jira_suffix_drift(Some("New"), None, None));
        assert!(!jira_suffix_drift(None, None, None));
    }

    #[test]
    fn describe_gaps_lists_multiple_reasons() {
        let refs = BodyRefs {
            structured_mr: true,
            mr_line_has_suffix: false,
            structured_jira: true,
            jira_line_has_suffix: false,
            has_legacy_lines: true,
            ..Default::default()
        };
        assert!(refs.needs_refresh());
        let gaps = refs.describe_gaps();
        assert!(gaps.contains("MR line missing state suffix"), "{gaps}");
        assert!(gaps.contains("JIRA line missing status suffix"), "{gaps}");
        assert!(gaps.contains("legacy metadata lines present"), "{gaps}");
    }

    #[test]
    fn desired_work_item_status_mapping() {
        assert_eq!(desired_work_item_status(true, Some("Done"), true), "Done");
        assert_eq!(desired_work_item_status(true, Some("Done"), false), "Done");
        assert_eq!(
            desired_work_item_status(true, Some("Won't Do"), true),
            "Won't do"
        );
        assert_eq!(
            desired_work_item_status(true, Some("Obsolete"), false),
            "Won't do"
        );
        assert_eq!(desired_work_item_status(false, None, true), "In progress");
        assert_eq!(desired_work_item_status(false, None, false), "To do");
    }

    #[test]
    fn gitlab_status_for_resolution_maps_fix_families_to_done() {
        assert_eq!(gitlab_status_for_resolution(Some("Done")), "Done");
        assert_eq!(gitlab_status_for_resolution(Some("Fixed")), "Done");
        assert_eq!(gitlab_status_for_resolution(Some("Resolved")), "Done");
    }

    #[test]
    fn gitlab_status_for_resolution_everything_else_is_wont_do() {
        assert_eq!(gitlab_status_for_resolution(Some("Won't Do")), "Won't do");
        assert_eq!(gitlab_status_for_resolution(Some("Obsolete")), "Won't do");
        assert_eq!(
            gitlab_status_for_resolution(Some("Cannot Reproduce")),
            "Won't do"
        );
        assert_eq!(gitlab_status_for_resolution(Some("Duplicate")), "Won't do");
        assert_eq!(gitlab_status_for_resolution(None), "Won't do");
    }

    #[test]
    fn describe_gaps_already_standard() {
        let refs = BodyRefs {
            structured_mr: true,
            mr_line_has_suffix: true,
            structured_jira: true,
            jira_line_has_suffix: true,
            has_legacy_lines: false,
            ..Default::default()
        };
        assert!(!refs.needs_refresh());
        assert_eq!(refs.describe_gaps(), "already standard");
    }

    #[test]
    fn is_legacy_line_detects_standalone_status() {
        assert!(is_legacy_line("- **Status**: open"));
        assert!(is_legacy_line("* Stream MR: https://example/"));
        assert!(is_legacy_line("* Affected version-release: 1-1"));
        assert!(!is_legacy_line("- **MR**: [t](u) — merged"));
        assert!(!is_legacy_line("- **JIRA**: [RHEL-1](u) — New"));
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
    fn refreshed_body_with_jira_and_lead() {
        let original = "\
            Legacy context paragraph.\n\
            \n\
            * Stream MR: https://example/-/merge_requests/1\n";
        let out = refreshed_body(RefreshedBodyArgs {
            original_body: original,
            release: "c10s",
            mr_url: "https://example/-/merge_requests/1",
            mr_title: "Fix CVE-2026-0001",
            mr_state: Some("merged"),
            jira_key: Some("RHEL-12345"),
            jira_status: Some("Closed"),
            jira_resolution: Some("Done"),
        });
        assert!(out.starts_with("Legacy context paragraph.\n\n- **MR**:"));
        assert!(out.contains(
            "- **MR**: [Fix CVE-2026-0001](https://example/-/merge_requests/1) — merged"
        ));
        // The JIRA base URL is env-overridable; compute the
        // expected link against the current value so parallel
        // tests mutating the env var don't cause false
        // failures here.
        let expected_jira = format!(
            "- **JIRA**: [RHEL-12345]({}/browse/RHEL-12345) — Closed (Done)",
            crate::utils::jira_base(),
        );
        assert!(out.contains(&expected_jira), "{out}");
        assert!(out.contains("- **Release**: c10s"));
        assert!(
            !out.contains("- **Status**:"),
            "refreshed body should not emit a standalone Status line: {out}",
        );
    }

    #[test]
    fn refreshed_body_without_jira_uses_placeholder() {
        let out = refreshed_body(RefreshedBodyArgs {
            original_body: "",
            release: "c10s",
            mr_url: "https://example/-/merge_requests/1",
            mr_title: "t",
            mr_state: None,
            jira_key: None,
            jira_status: None,
            jira_resolution: None,
        });
        assert!(out.contains("- **JIRA**: _(not found in MR; set with `--jira`)_"));
        // MR state suffix omitted when unknown.
        assert!(out.contains("- **MR**: [t](https://example/-/merge_requests/1)\n"));
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
            issue_state: "opened".into(),
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
            issue_state: "opened".into(),
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
    fn format_jira_state_closed_overrides_jira() {
        // Closed issue → STATE column shows "closed"
        // regardless of the underlying JIRA status.
        let r = Row {
            release: "c10s".into(),
            package: "xz".into(),
            issue_state: "closed".into(),
            issue_url: "u".into(),
            mr_url: None,
            jira_key: Some("RHEL-1".into()),
            jira_status: Some("Closed".into()),
            jira_resolution: Some("Obsolete".into()),
            jira_resolved: true,
            proposed_updates_nvr: None,
            stream_nvr: None,
            suggestion: "—",
        };
        assert_eq!(format_jira_state(&r), "closed");
    }

    #[test]
    fn format_jira_state_unknown() {
        let r = Row {
            release: "c10s".into(),
            package: "xz".into(),
            issue_state: "opened".into(),
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

    // ---- end-to-end wiremock + fake-binary test for the
    // read path (no --refresh) ----

    use crate::test_support::{EnvGuard, install_fake_bin};
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::matchers::{method, path as wiremock_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const STATUS_INVENTORY: &str = r#"
[inventory]
name = "cpu-sig"
description = "t"
maintainer = "t"

[inventory.workloads.c10s]
packages = ["PackageKit"]

[[package]]
name = "PackageKit"
"#;

    fn koji_list_tagged_output(nvr: &str, tag: &str) -> String {
        format!(
            "Build                                                    Tag                                          Built by\n\
             -------                                                  -----                                        --------\n\
             {nvr}    {tag}    alice\n",
        )
    }

    #[test]
    #[serial_test::serial]
    fn build_rows_reads_active_issue_end_to_end() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            // Group fetch: one active tracking issue.
            Mock::given(method("GET"))
                .and(wiremock_path(
                    "/api/v4/groups/CentOS%2Fproposed_updates%2Frpms/issues",
                ))
                .and(query_param("labels", "cpu-sig-tracker,c10s"))
                .and(query_param("state", "opened"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    {
                        "iid": 3,
                        "title": "PackageKit — CVE",
                        "description": "- **MR**: [Fix](https://gitlab.example/foo/bar/-/merge_requests/10) — opened\n\
                                        - **JIRA**: [RHEL-1](https://issues.example/browse/RHEL-1) — In Progress\n\
                                        - **Release**: c10s",
                        "state": "opened",
                        "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/PackageKit/-/issues/3",
                        "assignees": [],
                    }
                ])))
                .mount(&server)
                .await;

            // JIRA fetch.
            Mock::given(method("GET"))
                .and(wiremock_path("/rest/api/2/issue/RHEL-1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "key": "RHEL-1",
                    "fields": {
                        "summary": "s",
                        "status": { "name": "In Progress" }
                    }
                })))
                .mount(&server)
                .await;
        });

        let dir = tempdir().unwrap();
        install_fake_bin(
            dir.path(),
            "koji",
            &[(
                "list-tagged --latest proposed_updates10s-packages-main-release",
                &koji_list_tagged_output(
                    "PackageKit-1.2.8-9~proposed.el10",
                    "proposed_updates10s-packages-main-release",
                ),
            )],
        );
        install_fake_bin(
            dir.path(),
            "fedrq",
            &[(
                "pkgs --src -F line:name,version,release -b c10s PackageKit",
                "PackageKit : 1.2.8 : 8.el10",
            )],
        );
        let inv_path = dir.path().join("inv.toml");
        std::fs::write(&inv_path, STATUS_INVENTORY).unwrap();

        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{existing_path}", dir.path().display());
        let _guard = EnvGuard::new(&[
            ("GITLAB_TOKEN", "test-token"),
            ("CPU_SIG_TRACKER_GITLAB_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_JIRA_BASE", &server.uri()),
            ("PATH", &new_path),
        ]);

        let args = StatusArgs {
            inventory: inv_path.to_string_lossy().into_owned(),
            release: Some("c10s".to_string()),
            packages: vec![],
            json: false,
            refresh: false,
            include_closed: false,
            verbose: false,
        };
        let rows = build_rows(&args).expect("build_rows");

        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.release, "c10s");
        assert_eq!(r.package, "PackageKit");
        assert_eq!(r.issue_state, "opened");
        assert_eq!(r.jira_key.as_deref(), Some("RHEL-1"));
        assert_eq!(r.jira_status.as_deref(), Some("In Progress"));
        assert!(!r.jira_resolved);
        assert_eq!(
            r.proposed_updates_nvr.as_deref(),
            Some("PackageKit-1.2.8-9~proposed.el10"),
        );
        assert_eq!(r.stream_nvr.as_deref(), Some("PackageKit-1.2.8-8.el10"),);
        // pu (1.2.8-9~proposed) is newer than stream (1.2.8-8)
        // so rebase wouldn't fire; JIRA open → in-progress.
        assert_eq!(r.suggestion, "in-progress");
    }

    #[test]
    #[serial_test::serial]
    fn build_rows_refresh_rewrites_body_and_dates() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            // Active issue with legacy body (missing state
            // suffixes + has a stray Status line). --refresh
            // should rewrite the body.
            Mock::given(method("GET"))
                .and(wiremock_path(
                    "/api/v4/groups/CentOS%2Fproposed_updates%2Frpms/issues",
                ))
                .and(query_param("labels", "cpu-sig-tracker,c10s"))
                .and(query_param("state", "opened"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    {
                        "iid": 3,
                        "title": "xz",
                        "description": "- **MR**: [t](https://gitlab.example/redhat/rpms/xz/-/merge_requests/5)\n\
                                        - **JIRA**: [RHEL-2](https://issues.example/browse/RHEL-2)\n\
                                        - **Release**: c10s\n\
                                        - **Status**: open",
                        "state": "opened",
                        "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/xz/-/issues/3",
                        "assignees": [],
                        "start_date": null,
                        "due_date": null,
                        "created_at": "2026-03-01T00:00:00.000Z"
                    }
                ])))
                .mount(&server)
                .await;

            // JIRA fetch.
            Mock::given(method("GET"))
                .and(wiremock_path("/rest/api/2/issue/RHEL-2"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "key": "RHEL-2",
                    "fields": {
                        "summary": "s",
                        "status": { "name": "In Progress" }
                    }
                })))
                .mount(&server)
                .await;

            // MR fetch (refresh forces it even when structured
            // line is present, to capture live state).
            Mock::given(method("GET"))
                .and(wiremock_path(
                    "/api/v4/projects/redhat%2Frpms%2Fxz/merge_requests/5",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 5,
                    "title": "t",
                    "description": null,
                    "state": "opened",
                    "web_url": "https://gitlab.example/redhat/rpms/xz/-/merge_requests/5",
                    "source_branch": "fix",
                    "target_branch": "c10s",
                })))
                .mount(&server)
                .await;

            // Body rewrite — PUT on the issue.
            Mock::given(method("PUT"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues/3",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 3, "title": "t", "state": "opened",
                    "web_url": "https://example", "assignees": []
                })))
                .mount(&server)
                .await;

            // GraphQL (work-item status reconcile + date set).
            Mock::given(method("POST"))
                .and(wiremock_path("/api/graphql"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "project": {
                            "workItems": {
                                "nodes": [{
                                    "id": "gid://gitlab/WorkItem/3",
                                    "widgets": [{
                                        "type": "STATUS",
                                        "status": { "name": "To do" }
                                    }],
                                    "namespace": {
                                        "workItemTypes": {
                                            "nodes": [{
                                                "name": "Issue",
                                                "widgetDefinitions": [{
                                                    "type": "STATUS",
                                                    "allowedStatuses": [{
                                                        "id": "gid://gitlab/status/1",
                                                        "name": "In progress"
                                                    }]
                                                }]
                                            }]
                                        }
                                    }
                                }]
                            }
                        },
                        "workItemUpdate": { "errors": [] }
                    }
                })))
                .mount(&server)
                .await;
        });

        let dir = tempdir().unwrap();
        install_fake_bin(
            dir.path(),
            "koji",
            &[
                (
                    "list-tagged --latest proposed_updates10s-packages-main-release",
                    &koji_list_tagged_output(
                        "xz-5.6.4-1~proposed.el10",
                        "proposed_updates10s-packages-main-release",
                    ),
                ),
                (
                    "list-tagged --quiet proposed_updates10s-packages-main-testing",
                    "",
                ),
            ],
        );
        install_fake_bin(
            dir.path(),
            "fedrq",
            &[(
                "pkgs --src -F line:name,version,release -b c10s xz",
                "xz : 5.6.4 : 1.el10",
            )],
        );
        let inv = STATUS_INVENTORY.replace("PackageKit", "xz");
        let inv_path = dir.path().join("inv.toml");
        std::fs::write(&inv_path, &inv).unwrap();

        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{existing_path}", dir.path().display());
        let _guard = EnvGuard::new(&[
            ("GITLAB_TOKEN", "test-token"),
            ("CPU_SIG_TRACKER_GITLAB_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_JIRA_BASE", &server.uri()),
            ("PATH", &new_path),
        ]);

        let args = StatusArgs {
            inventory: inv_path.to_string_lossy().into_owned(),
            release: Some("c10s".to_string()),
            packages: vec![],
            json: false,
            refresh: true,
            include_closed: false,
            verbose: false,
        };
        let rows = build_rows(&args).expect("refresh build_rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].package, "xz");
    }
}
