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
use crate::gitlab;

const PROPOSED_UPDATES_GROUP: &str = "CentOS/proposed_updates/rpms";
const TRACKING_LABEL: &str = "cpu-sig-tracker";
use crate::jira::Verdict;
use crate::utils::{canonical_jira_suffix, format_jira_lines, format_mr_line, gitlab_base};
const KOJI_PROFILE: &str = "cbs";

#[derive(clap::Args)]
pub struct StatusArgs {
    /// Path to the sandogasa-inventory TOML file.
    #[arg(
        short,
        long,
        default_value = "inventory.toml",
        help_heading = "Selection"
    )]
    pub inventory: String,

    /// Narrow to specific packages (repeatable or CSV). When
    /// unset, every tracked package in scope is processed.
    /// Primarily useful with --refresh to skip the extra
    /// work-item-status probes for packages you don't care
    /// about.
    #[arg(
        short = 'p',
        long = "package",
        value_delimiter = ',',
        help_heading = "Selection"
    )]
    pub packages: Vec<String>,

    /// Restrict the check to a single release (e.g. `c10s`).
    #[arg(long, help_heading = "Selection")]
    pub release: Option<String>,

    /// With --refresh, also process closed tracking issues —
    /// backfill their start_date / due_date and reconcile
    /// their work-item status against the current JIRA
    /// resolution (e.g. Done → Won't do when JIRA was
    /// retroactively flipped). Body content is left alone.
    /// No-op without --refresh.
    #[arg(long, requires = "refresh", help_heading = "Writing")]
    pub include_closed: bool,

    /// Rewrite tracking issue bodies to the standardized
    /// format, refreshing the MR state and JIRA status lines
    /// to their current values. Normalizes legacy bodies and
    /// keeps already-standard bodies up to date. Mutates
    /// GitLab state; off by default.
    #[arg(long, help_heading = "Writing")]
    pub refresh: bool,

    /// Emit JSON instead of grouped text.
    #[arg(long, help_heading = "Output")]
    pub json: bool,

    /// Print progress to stderr.
    #[arg(short, long, help_heading = "Output")]
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
    /// Each RHEL issue the update resolves, one per fix it carries
    /// (#86).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub jira: Vec<JiraState>,
    /// Whether every one of them is resolved.
    pub jira_resolved: bool,
    /// Currently-tagged NVR in the proposed_updates Koji tag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed_updates_nvr: Option<String>,
    /// Current CentOS Stream NVR for the same release.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_nvr: Option<String>,
    pub suggestion: &'static str,
    /// What says the change is in stock, or why it does not count, as
    /// untag and retire judge it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub landed: Option<String>,
}

/// The suggestion stock's evidence makes, which outranks Jira's: an
/// untag once the change is in stock, a wait while stock carries the
/// patches without having built past the SIG, and `not-in-stock` when
/// stock lacks one of them. `None` when there is nothing to go on.
fn landed_suggestion(l: &crate::landed::Landed) -> Option<&'static str> {
    match (&l.by, &l.doubt) {
        (Some(_), _) => Some("untag-candidate"),
        (None, Some(_)) if l.patches.as_ref().is_some_and(|c| c.all_carried()) => {
            Some("awaiting-stock")
        }
        (None, Some(_)) => Some("not-in-stock"),
        (None, None) => None,
    }
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

    let group_client = gitlab::group_client(&gitlab_base(), PROPOSED_UPDATES_GROUP)?;

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
            let jira_keys = parsed.jira_keys.clone();

            let infos: Vec<(String, Option<JiraInfo>)> = jira_keys
                .iter()
                .map(|k| (k.clone(), fetch_jira(k, args.verbose)))
                .collect();
            let jira: Vec<JiraState> = infos
                .iter()
                .map(|(key, info)| JiraState {
                    key: key.clone(),
                    status: info.as_ref().map(|i| i.status.clone()),
                    resolution: info.as_ref().and_then(|i| i.resolution.clone()),
                    resolved: info.as_ref().is_some_and(|i| i.resolved),
                    verdict: info.as_ref().map(|i| verdict_word(&i.verdict)),
                })
                .collect();
            // Settled once every issue is, fixed or declined; an issue
            // closed with no sign of a fix settles nothing (#87).
            let (outcome, jira_resolution, jira_resolution_date) = combined_outcome(&infos);
            let jira_resolved = matches!(outcome, Outcome::Fixed | Outcome::Declined);

            let pu_nvr = pu_nvrs.get(package).cloned();
            let stream_nvr = stream_nvrs.get(package).cloned();

            let suggestion = if issue.state == "closed" {
                // Closed issue with a lingering pu build: an untag once
                // RHEL has the fix, a question otherwise (the blktrace
                // issues were closed on an inactivity "Done").
                match (pu_nvr.is_some(), unsettled_suggestion(outcome)) {
                    (false, _) => "—",
                    (true, Some(s)) if !jira_keys.is_empty() => s,
                    (true, _) => "untag-candidate",
                }
            } else {
                match unsettled_suggestion(outcome) {
                    Some(s) if !jira_keys.is_empty() => s,
                    _ => suggest_next_action(
                        jira_keys.first().map(String::as_str),
                        outcome == Outcome::Fixed,
                        pu_nvr.as_deref(),
                        stream_nvr.as_deref(),
                    ),
                }
            };
            // Whether the change is in stock, as untag and retire judge
            // it (crate::landed): stock's patches outrank Jira either
            // way (#89). Asked while the SIG still ships a build, and on
            // a refresh, for the day it landed (#92).
            let landed = (pu_nvr.is_some() || args.refresh).then(|| {
                crate::landed::landed(
                    &issue,
                    package,
                    release,
                    pu_nvr.as_deref(),
                    stream_nvr.as_deref(),
                    crate::landed::JiraFacts {
                        fixed: (outcome == Outcome::Fixed).then_some("every RHEL issue is fixed"),
                        on: jira_resolution_date,
                    },
                    args.verbose,
                )
            });
            let shipped = landed.as_ref().filter(|_| pu_nvr.is_some());
            let suggestion = shipped.and_then(landed_suggestion).unwrap_or(suggestion);
            let landed_note = shipped.and_then(|l| l.by.clone().or(l.doubt.clone()));
            // The day it was fixed: what the issue records, else what the
            // evidence shows, else when Jira settled it, which can be
            // months later (RHEL-170526 closed 2026-10-23, the fix in
            // stock since 2026-04-27).
            let fix_date = crate::landed::stock_fix_on(body)
                .or_else(|| {
                    landed
                        .as_ref()
                        .filter(|l| l.by.is_some())
                        .and_then(|l| l.on)
                })
                .or(jira_resolution_date);

            // Body-vs-live drift detection. Format drift is
            // free (pure body parse); JIRA drift compares the
            // body's current JIRA suffix to the canonical form
            // of the JIRA fetch.
            let jira_drift = jira.iter().any(|j| {
                let body = parsed
                    .body_jira_suffixes
                    .iter()
                    .find(|(k, _)| *k == j.key)
                    .and_then(|(_, s)| s.as_deref());
                jira_suffix_drift(body, j.status.as_deref(), j.resolution.as_deref())
            });
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
                            fix_date,
                            args.verbose,
                        );
                    }
                } else if let (Some(project), true) = (project, mr_url.is_some()) {
                    // How urgent it is (#98), for an issue filed before
                    // file-issue recorded it: the CVEs the change fixes,
                    // rated only when the body has no CVE lines yet.
                    let summaries: Vec<&str> = infos
                        .iter()
                        .filter_map(|(_, i)| i.as_ref().map(|i| i.summary.as_str()))
                        .collect();
                    let cves = crate::triage::update_cves(
                        [
                            mr_title.as_deref().unwrap_or(""),
                            parsed.mr_description.as_deref().unwrap_or(""),
                            body,
                        ]
                        .into_iter()
                        .chain(summaries.iter().copied()),
                        infos
                            .iter()
                            .filter_map(|(_, i)| i.as_ref()?.description.as_deref()),
                    );
                    let new_cve_lines = (crate::triage::parse_cve_lines(body).is_empty()
                        && !cves.is_empty())
                    .then(|| crate::triage::rated_cve_lines(&cves, args.verbose));
                    // The builds, for a body that names them unknown (#101):
                    // stock's build expected to carry the fix, named by the
                    // SIG's, and stock's current build while stock lacks
                    // the change.
                    let sig_build = crate::builds::sig_build(
                        pu_nvr.as_deref(),
                        testing_nvrs.get(package).map(String::as_str),
                    );
                    let expected_build = sig_build.as_deref().and_then(crate::builds::expected_fix);
                    let affected_build = crate::builds::affected(
                        stream_nvr.as_deref(),
                        sig_build.as_deref(),
                        landed.as_ref().is_some_and(|l| l.by.is_some()),
                    );
                    maybe_refresh_labels(
                        &project,
                        &issue,
                        !cves.is_empty(),
                        crate::triage::priority_label(
                            infos
                                .iter()
                                .filter_map(|(_, i)| i.as_ref()?.priority.as_deref()),
                        )
                        .as_deref(),
                    );
                    let changed = maybe_refresh_issue(RefreshCtx {
                        project_path: &project,
                        iid: issue.iid,
                        original_body: body,
                        release,
                        mr_url: mr_url.as_deref(),
                        mr_title: mr_title.as_deref(),
                        mr_state: mr_state.as_deref(),
                        jira: &jira,
                        new_cve_lines: new_cve_lines.as_deref(),
                        builds: (affected_build.as_deref(), expected_build.as_deref()),
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
                    maybe_refresh_dates(&project, &issue, package, release, fix_date, args.verbose);
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
                jira,
                jira_resolved,
                proposed_updates_nvr: pu_nvr,
                stream_nvr,
                suggestion,
                landed: landed_note,
            });
        }
    }

    rows.sort_by(|a, b| a.release.cmp(&b.release).then(a.package.cmp(&b.package)));
    Ok(rows)
}

/// One RHEL issue of an update, as Jira reports it now.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct JiraState {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    pub resolved: bool,
    /// What the resolution says (#87): `open`, `fixed`, `unverified`
    /// (closed with no fix version), `declined` or `duplicate`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<&'static str>,
}

/// Fields resolved from a tracking issue body, possibly
/// augmented by fetching the MR it references.
#[derive(Debug, Default)]
struct BodyRefs {
    mr_url: Option<String>,
    mr_title: Option<String>,
    /// The merge request's description, which may name the CVEs it fixes.
    mr_description: Option<String>,
    mr_state: Option<String>,
    /// The RHEL issues the update resolves.
    jira_keys: Vec<String>,
    /// Whether the structured `- **MR**: [title](url)` line was
    /// parseable from the body.
    structured_mr: bool,
    /// Whether the MR line in the body carries a ` — <state>`
    /// suffix (present in the current canonical format).
    mr_line_has_suffix: bool,
    /// Whether the structured `- **JIRA**: [KEY](url)` line was
    /// parseable from the body.
    structured_jira: bool,
    /// Whether every JIRA line in the body carries a ` — <status>`
    /// suffix (present in the current canonical format).
    jira_line_has_suffix: bool,
    /// Each JIRA line's key and the `<status>` or `<status>
    /// (<resolution>)` text that currently sits on it, for drift
    /// comparison against the live JIRA API value.
    body_jira_suffixes: Vec<(String, Option<String>)>,
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

    let structured_jira = parse_jira_lines(body);
    let jira_from_body: Vec<String> = if structured_jira.is_empty() {
        scan_rhel_key(body).into_iter().collect()
    } else {
        structured_jira.iter().map(|(k, _)| k.clone()).collect()
    };

    let should_fetch_mr = mr_url.is_some()
        && (force_mr_fetch || structured_mr.is_none() || jira_from_body.is_empty());
    let mr = if should_fetch_mr {
        mr_url.as_deref().and_then(|u| fetch_mr(u, verbose))
    } else {
        None
    };

    let jira_keys = if jira_from_body.is_empty() {
        mr.as_ref()
            .map(|m| crate::file_issue::extract_jira_keys(&m.title, m.description.as_deref()))
            .unwrap_or_default()
    } else {
        jira_from_body
    };

    BodyRefs {
        mr_url,
        mr_title: mr.as_ref().map(|m| m.title.clone()),
        mr_description: mr.as_ref().and_then(|m| m.description.clone()),
        mr_state: mr.as_ref().map(|m| m.state.clone()),
        jira_keys,
        structured_mr: structured_mr.is_some(),
        mr_line_has_suffix: structured_mr
            .as_ref()
            .map(|(_, s)| s.is_some())
            .unwrap_or(false),
        jira_line_has_suffix: !structured_jira.is_empty()
            && structured_jira.iter().all(|(_, s)| s.is_some()),
        structured_jira: !structured_jira.is_empty(),
        body_jira_suffixes: structured_jira,
        has_legacy_lines: body.lines().any(is_legacy_line),
    }
}

/// Parse the structured `- **MR**: [title](url)[ — state]`
/// line. Returns (url, state) where state is the trimmed text
/// after ` — ` / ` -- ` following the URL's closing paren.
pub(crate) fn parse_mr_line(body: &str) -> Option<(String, Option<String>)> {
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
fn parse_jira_lines(body: &str) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
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
            out.push((key, suffix));
        }
    }
    out
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
pub(crate) fn scan_mr_url_in_body(body: &str) -> Option<String> {
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
    let client = gitlab::client(&gitlab_base(), &project).ok()?;
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
pub(crate) fn tracking_project_of(web_url: &str) -> Option<String> {
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
    let client = match gitlab::client(&gitlab_base(), project_path) {
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
    fix_date: Option<chrono::NaiveDate>,
    verbose: bool,
) {
    let desired_start = crate::file_issue::find_build_start_date(package, release, verbose)
        .or_else(|| {
            issue
                .created_at
                .as_deref()
                .and_then(crate::utils::parse_iso_date)
        });
    let desired_due = fix_date;

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

    let client = match gitlab::client(&gitlab_base(), project_path) {
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
    jira: &'a [JiraState],
    /// CVE lines to add, for a body that has none.
    new_cve_lines: Option<&'a [String]>,
    /// The affected and expected builds, for lines that say unknown.
    builds: (Option<&'a str>, Option<&'a str>),
}

/// The labels an issue filed before #98 lacks: `security` when the
/// change fixes a CVE and the issue has no type label, and the priority
/// label for the RHEL issues' priority, replacing a stale one.
fn label_changes(
    labels: &[String],
    fixes_cve: bool,
    priority: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let (mut add, remove) = match priority {
        Some(p) => crate::triage::priority_change(labels, p),
        None => (Vec::new(), Vec::new()),
    };
    let typed = crate::file_issue::TYPE_LABELS
        .iter()
        .any(|t| labels.iter().any(|l| l == t));
    if fixes_cve && !typed {
        add.push("security".to_string());
    }
    (add, remove)
}

/// Apply [`label_changes`] to the tracking issue.
fn maybe_refresh_labels(
    project_path: &str,
    issue: &gitlab::Issue,
    fixes_cve: bool,
    priority: Option<&str>,
) {
    let (add, remove) = label_changes(&issue.labels, fixes_cve, priority);
    if add.is_empty() && remove.is_empty() {
        return;
    }
    let client = match gitlab::client(&gitlab_base(), project_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: --refresh: could not build client for {project_path}: {e}");
            return;
        }
    };
    let update = gitlab::IssueUpdate {
        add_labels: (!add.is_empty()).then(|| add.join(",")),
        remove_labels: (!remove.is_empty()).then(|| remove.join(",")),
        ..Default::default()
    };
    match client.edit_issue(issue.iid, &update) {
        Ok(_) => {
            let mut what: Vec<String> = add.iter().map(|l| format!("+{l}")).collect();
            what.extend(remove.iter().map(|l| format!("-{l}")));
            eprintln!("refreshed {}: labels {}", issue.web_url, what.join(" "));
        }
        Err(e) => eprintln!(
            "warning: --refresh: failed to relabel {}: {e}",
            issue.web_url
        ),
    }
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
        jira: ctx.jira,
        new_cve_lines: ctx.new_cve_lines,
        builds: ctx.builds,
    });
    // GitLab strips trailing whitespace/newlines from stored
    // descriptions, so compare trimmed values — otherwise the
    // `\n` we always emit at the end triggers a rewrite every
    // run even when nothing changed.
    if new_body.trim_end() == ctx.original_body.trim_end() {
        return false;
    }
    let client = match gitlab::client(&gitlab_base(), ctx.project_path) {
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
    jira: &'a [JiraState],
    new_cve_lines: Option<&'a [String]>,
    /// What fills unknown build lines (#101).
    builds: (Option<&'a str>, Option<&'a str>),
}

/// Build a standardized body, preserving the original prose
/// (trimmed) as a lead paragraph when it isn't already just
/// the structured metadata.
fn refreshed_body(a: RefreshedBodyArgs<'_>) -> String {
    let lead = extract_lead_paragraph(a.original_body);
    let mr_line = format_mr_line(a.mr_url, a.mr_title, a.mr_state);
    let issues: Vec<(&str, Option<&str>, Option<&str>)> = a
        .jira
        .iter()
        .map(|j| (j.key.as_str(), j.status.as_deref(), j.resolution.as_deref()))
        .collect();
    let jira_line = format_jira_lines(&issues);
    // Lines that record something the live systems do not say — the
    // affected and expected builds the SIG named, the stock fix a
    // person established — survive the rewrite unchanged.
    // Unknown build lines filled from the evidence (#101); lines with a
    // value are the SIG's word and stay.
    let filled = crate::builds::fill(a.original_body, a.builds.0, a.builds.1);
    let kept: String = filled
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            t.starts_with("- **Affected build**:")
                || t.starts_with("- **Expected fix**:")
                || t.starts_with("- **Stock fix**:")
        })
        .map(|l| format!("{}\n", l.trim_end()))
        .collect();
    // CVE lines (#98) sit under the JIRA ones, as file-issue writes
    // them: the body's own, sorted by CVE, else the ones to add.
    let cves: String = match a.new_cve_lines {
        Some(lines) => lines.iter().map(|l| format!("{l}\n")).collect(),
        None => {
            let mut lines: Vec<&str> = a
                .original_body
                .lines()
                .filter(|l| l.trim_start().starts_with(crate::triage::CVE_LINE))
                .map(str::trim_end)
                .collect();
            crate::triage::sort_cve_lines(&mut lines);
            lines.iter().map(|l| format!("{l}\n")).collect()
        }
    };
    let metadata = format!(
        "{mr_line}\n\
         {jira_line}\n\
         {cves}\
         - **Release**: {}\n{kept}",
        a.release,
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
        "- **Stock fix**:",
        "- **CVE**:",
        "- **Status**:",
        "* Stream MR:",
        "* Affected",
        "* Expected",
    ];
    let trimmed = line.trim_start();
    PREFIXES.iter().any(|p| trimmed.starts_with(p))
}

/// Where an update's RHEL issues leave it, taken together (#87).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Some issue is open, or could not be read.
    Open,
    /// None is open, but some was closed without a fix version, or as a
    /// duplicate of an issue that cannot be seen: closed, with nothing
    /// to say a fix shipped.
    Unverified,
    /// Every issue is settled, and some was declined.
    Declined,
    /// Every issue was fixed.
    Fixed,
}

/// The update's [`Outcome`], with the resolution its work item is
/// judged by (`Done` for a fix, else the first declining one) and the
/// day the last issue was resolved, both only once it is settled.
fn combined_outcome(
    infos: &[(String, Option<JiraInfo>)],
) -> (Outcome, Option<String>, Option<chrono::NaiveDate>) {
    let all: Option<Vec<&JiraInfo>> = infos.iter().map(|(_, i)| i.as_ref()).collect();
    let Some(all) = all.filter(|a| !a.is_empty()) else {
        return (Outcome::Open, None, None);
    };
    let is = |f: fn(&Verdict) -> bool| all.iter().any(|i| f(&i.verdict));
    let outcome = if is(|v| matches!(v, Verdict::Open(_))) {
        Outcome::Open
    } else if is(|v| matches!(v, Verdict::Unverified(_) | Verdict::Duplicate(_))) {
        Outcome::Unverified
    } else if is(|v| matches!(v, Verdict::Declined(_))) {
        Outcome::Declined
    } else {
        Outcome::Fixed
    };
    let resolution = match outcome {
        Outcome::Fixed => Some("Done".to_string()),
        Outcome::Declined => all.iter().find_map(|i| match &i.verdict {
            Verdict::Declined(r) => Some(r.clone()),
            _ => None,
        }),
        _ => None,
    };
    let date = matches!(outcome, Outcome::Fixed | Outcome::Declined)
        .then(|| all.iter().filter_map(|i| i.resolution_date).max())
        .flatten();
    (outcome, resolution, date)
}

/// The suggestion for an update whose issues are settled without a
/// fix, or `None` when they are open or fixed and the usual
/// [`suggest_next_action`] applies.
fn unsettled_suggestion(outcome: Outcome) -> Option<&'static str> {
    match outcome {
        Outcome::Declined => Some("jira-declined"),
        Outcome::Unverified => Some("jira-unverified"),
        Outcome::Open | Outcome::Fixed => None,
    }
}

struct JiraInfo {
    status: String,
    /// The priority Red Hat set (`Major`), for the issue's label (#98).
    priority: Option<String>,
    summary: String,
    /// The description, which sets out the CVEs of an update fixing
    /// several.
    description: Option<String>,
    resolution: Option<String>,
    resolved: bool,
    resolution_date: Option<chrono::NaiveDate>,
    verdict: Verdict,
}

/// One RHEL issue as Jira reports it, its verdict following a visible
/// duplicate one link deep.
fn fetch_jira(key: &str, verbose: bool) -> Option<JiraInfo> {
    let issue = crate::jira::fetch_or_warn(key, verbose)?;
    let verdict = match crate::jira::verdict_of(&issue) {
        Verdict::Duplicate(Some(of)) => crate::jira::fetch_or_warn(&of, verbose)
            .map(|original| crate::jira::verdict_of(&original))
            .unwrap_or(Verdict::Duplicate(Some(of))),
        v => v,
    };
    Some(JiraInfo {
        status: issue.status().to_string(),
        priority: issue.fields.priority.as_ref().map(|p| p.name.clone()),
        summary: issue.fields.summary.clone(),
        description: issue.fields.description.clone(),
        resolution: issue.resolution().map(|s| s.to_string()),
        resolved: issue.is_resolved(),
        resolution_date: issue.resolution_date(),
        verdict,
    })
}

/// A verdict as one word, for the status table and `--json`.
fn verdict_word(v: &Verdict) -> &'static str {
    match v {
        Verdict::Open(_) => "open",
        Verdict::Fixed { .. } => "fixed",
        Verdict::Unverified(_) => "unverified",
        Verdict::Declined(_) => "declined",
        Verdict::Duplicate(_) => "duplicate",
    }
}

pub(crate) fn fetch_proposed_updates_nvrs(release: &str, verbose: bool) -> HashMap<String, String> {
    match proposed_updates_tag(release) {
        Ok(tag) => fetch_koji_nvrs(&tag, verbose),
        Err(e) => {
            eprintln!("warning: {e}; skipping proposed_updates NVR lookup");
            HashMap::new()
        }
    }
}

pub(crate) fn fetch_proposed_updates_testing_nvrs(
    release: &str,
    verbose: bool,
) -> HashMap<String, String> {
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

pub(crate) fn fetch_stream_nvrs(
    release: &str,
    packages: &[String],
    verbose: bool,
) -> HashMap<String, String> {
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

/// True when stock Stream's NVR is the SIG's minus the `~proposed`
/// release suffix (`blktrace-1.3.0-13.el10` for
/// `blktrace-1.3.0-13~proposed.el10`). A hint, not proof that the
/// change landed: blktrace's stock `-13` was a mass rebuild, and the
/// SIG's `-13~proposed` had been numbered off the affected release.
/// Whether the change is in stock is read from stock's history.
pub(crate) fn stream_shares_release_number(pu_nvr: Option<&str>, stream_nvr: Option<&str>) -> bool {
    match (pu_nvr, stream_nvr) {
        (Some(pu), Some(stream)) => {
            pu.contains("~proposed") && pu.replace("~proposed", "") == stream
        }
        _ => false,
    }
}

/// True when both NVRs are known AND the Stream V-R is strictly
/// greater than the proposed_updates V-R (RPM ordering). If
/// either side is missing we play it safe and return false —
/// suggestion falls through to `in-progress`.
pub(crate) fn stream_newer_than_proposed(pu_nvr: Option<&str>, stream_nvr: Option<&str>) -> bool {
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
pub(crate) fn vr_of_nvr(nvr: &str) -> Option<String> {
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
    let keys: Vec<String> = rows
        .iter()
        .map(|r| {
            if r.jira.is_empty() {
                UNKNOWN.to_string()
            } else {
                r.jira
                    .iter()
                    .map(|j| j.key.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            }
        })
        .collect();
    let jira_width = col_width(H_JIRA, keys.iter().map(|s| s.as_str()));
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
        let jira_key = &keys[i];
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
    let notes: Vec<String> = rows
        .iter()
        .filter_map(|r| {
            r.landed
                .as_ref()
                .map(|l| format!("{} {}: {l}", r.release, r.package))
        })
        .collect();
    if !notes.is_empty() {
        println!();
        for n in notes {
            println!("{n}");
        }
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
    let states: Vec<String> = r
        .jira
        .iter()
        .map(|j| {
            let state = canonical_jira_suffix(j.status.as_deref(), j.resolution.as_deref())
                .unwrap_or_else(|| "unknown".to_string());
            // A closed issue that is not a fix says so, as "Done"
            // alone can mean closed for inactivity (#87).
            match j.verdict {
                Some(v @ ("unverified" | "declined" | "duplicate")) => format!("{state}, {v}"),
                _ => state,
            }
        })
        .collect();
    if states.is_empty() {
        "unknown".to_string()
    } else {
        states.join("; ")
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
        let (key, suffix) = parse_jira_lines(body).remove(0);
        assert_eq!(key, "RHEL-1");
        assert_eq!(suffix.as_deref(), Some("Closed (Done)"));
    }

    #[test]
    fn parse_jira_line_without_suffix() {
        let body = "- **JIRA**: [RHEL-1](https://example/)\n";
        let (_, suffix) = parse_jira_lines(body).remove(0);
        assert_eq!(suffix, None);
    }

    #[test]
    fn parse_jira_line_skips_placeholder() {
        // Placeholder text doesn't start with `[` so parser
        // returns None rather than a bogus key.
        let body = "- **JIRA**: _(not found in MR; set with `--jira`)_\n";
        assert!(parse_jira_lines(body).is_empty());
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
    fn stream_shares_release_number_means_the_same_nvr_without_the_suffix() {
        assert!(stream_shares_release_number(
            Some("blktrace-1.3.0-13~proposed.el10"),
            Some("blktrace-1.3.0-13.el10")
        ));
        // A newer stock build is past the SIG's, not the SIG's own.
        assert!(!stream_shares_release_number(
            Some("blktrace-1.3.0-13~proposed.el10"),
            Some("blktrace-1.3.0-14.el10")
        ));
        assert!(!stream_shares_release_number(
            Some("x-1-1.el10"),
            Some("x-1-1.el10")
        ));
        assert!(!stream_shares_release_number(None, Some("x-1-1.el10")));
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
    #[serial_test::serial]
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
            jira: &[JiraState {
                key: "RHEL-12345".into(),
                status: Some("Closed".into()),
                resolution: Some("Done".into()),
                resolved: true,
                verdict: None,
            }],
            new_cve_lines: None,
            builds: (None, None),
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
    fn a_body_with_two_jira_lines_is_read_and_rewritten_with_both() {
        let body = "- **MR**: [t](https://example/-/merge_requests/1) — opened\n\
                    - **JIRA**: [RHEL-303720](https://example/browse/RHEL-303720) — New\n\
                    - **JIRA**: [RHEL-303900](https://example/browse/RHEL-303900)\n\
                    - **Release**: c9s\n";
        let refs = resolve_body_references(body, false, false);
        assert_eq!(refs.jira_keys, ["RHEL-303720", "RHEL-303900"]);
        // The second line has no status yet: the body is not canonical.
        assert!(refs.structured_jira && !refs.jira_line_has_suffix);
        let jira = [
            JiraState {
                key: "RHEL-303720".into(),
                status: Some("New".into()),
                resolution: None,
                resolved: false,
                verdict: None,
            },
            JiraState {
                key: "RHEL-303900".into(),
                status: Some("Closed".into()),
                resolution: Some("Done".into()),
                resolved: true,
                verdict: None,
            },
        ];
        let out = refreshed_body(RefreshedBodyArgs {
            original_body: body,
            release: "c9s",
            mr_url: "https://example/-/merge_requests/1",
            mr_title: "t",
            mr_state: Some("opened"),
            jira: &jira,
            new_cve_lines: None,
            builds: (None, None),
        });
        assert!(out.contains("[RHEL-303720]("), "{out}");
        assert!(out.contains("[RHEL-303900]("), "{out}");
        assert!(
            out.contains("/browse/RHEL-303900) — Closed (Done)\n"),
            "{out}"
        );
    }

    #[test]
    fn stock_s_evidence_outranks_jira_s() {
        use crate::landed::Landed;
        use crate::patches::Comparison;
        let carried = Comparison {
            matched: vec![("a".into(), "b".into(), "upstream ac49526d".into())],
            missing: vec![],
        };
        let missing = Comparison {
            matched: vec![],
            missing: vec!["a".into()],
        };
        let l = |by: Option<&str>, doubt: Option<&str>, c: Option<Comparison>| Landed {
            by: by.map(str::to_string),
            doubt: doubt.map(str::to_string),
            patches: c,
            ..Default::default()
        };
        // PackageKit c10s: Jira declined, stock carries the patch.
        assert_eq!(
            landed_suggestion(&l(Some("stock carries it"), None, Some(carried.clone()))),
            Some("untag-candidate")
        );
        assert_eq!(
            landed_suggestion(&l(None, Some("not yet past"), Some(carried))),
            Some("awaiting-stock")
        );
        assert_eq!(
            landed_suggestion(&l(None, Some("stock does not carry a"), Some(missing))),
            Some("not-in-stock")
        );
        assert_eq!(landed_suggestion(&l(None, None, None)), None);
    }

    #[test]
    fn an_update_is_settled_by_its_issues_verdicts() {
        let info = |verdict: Verdict, day: u32| JiraInfo {
            priority: None,
            summary: String::new(),
            description: None,
            status: "Closed".into(),
            resolution: None,
            resolved: !matches!(verdict, Verdict::Open(_)),
            resolution_date: chrono::NaiveDate::from_ymd_opt(2026, 12, day),
            verdict,
        };
        let fixed = |day| {
            info(
                Verdict::Fixed {
                    resolution: "Done".into(),
                    fix_versions: vec!["rhel-10.3".into()],
                },
                day,
            )
        };
        let k = |s: &str| s.to_string();
        assert_eq!(
            combined_outcome(&[(k("RHEL-1"), Some(fixed(1))), (k("RHEL-2"), Some(fixed(9)))]),
            (
                Outcome::Fixed,
                Some("Done".into()),
                chrono::NaiveDate::from_ymd_opt(2026, 12, 9)
            )
        );
        // blktrace: closed Done with no fix version settles nothing.
        assert_eq!(
            combined_outcome(&[(
                k("RHEL-123606"),
                Some(info(Verdict::Unverified("Done".into()), 1))
            )]),
            (Outcome::Unverified, None, None)
        );
        // PackageKit c10s: declined, and the work item reads so.
        assert_eq!(
            combined_outcome(&[(
                k("RHEL-170526"),
                Some(info(Verdict::Declined("Not a Bug".into()), 3))
            )]),
            (
                Outcome::Declined,
                Some("Not a Bug".into()),
                chrono::NaiveDate::from_ymd_opt(2026, 12, 3)
            )
        );
        // One open, or one unread, keeps the update open.
        assert_eq!(
            combined_outcome(&[
                (k("RHEL-1"), Some(fixed(1))),
                (k("RHEL-2"), Some(info(Verdict::Open("New".into()), 1)))
            ])
            .0,
            Outcome::Open
        );
        assert_eq!(combined_outcome(&[(k("RHEL-1"), None)]).0, Outcome::Open);
        assert_eq!(combined_outcome(&[]).0, Outcome::Open);
        assert_eq!(
            unsettled_suggestion(Outcome::Unverified),
            Some("jira-unverified")
        );
        assert_eq!(
            unsettled_suggestion(Outcome::Declined),
            Some("jira-declined")
        );
        assert_eq!(unsettled_suggestion(Outcome::Fixed), None);
    }

    #[test]
    fn refreshed_body_without_jira_uses_placeholder() {
        let out = refreshed_body(RefreshedBodyArgs {
            original_body: "",
            release: "c10s",
            mr_url: "https://example/-/merge_requests/1",
            mr_title: "t",
            mr_state: None,
            jira: &[],
            new_cve_lines: None,
            builds: (None, None),
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
            jira: vec![JiraState {
                key: "RHEL-1".into(),
                status: Some("Closed".into()),
                resolution: Some("Done".into()),
                resolved: true,
                verdict: None,
            }],
            jira_resolved: true,
            proposed_updates_nvr: None,
            stream_nvr: None,
            suggestion: "untag-candidate",
            landed: None,
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
            jira: vec![JiraState {
                key: "RHEL-1".into(),
                status: Some("In Progress".into()),
                resolution: None,
                resolved: false,
                verdict: None,
            }],
            jira_resolved: false,
            proposed_updates_nvr: None,
            stream_nvr: None,
            suggestion: "in-progress",
            landed: None,
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
            jira: vec![JiraState {
                key: "RHEL-1".into(),
                status: Some("Closed".into()),
                resolution: Some("Obsolete".into()),
                resolved: true,
                verdict: None,
            }],
            jira_resolved: true,
            proposed_updates_nvr: None,
            stream_nvr: None,
            suggestion: "—",
            landed: None,
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
            jira: vec![],
            jira_resolved: false,
            proposed_updates_nvr: None,
            stream_nvr: None,
            suggestion: "no-jira",
            landed: None,
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
                "list-tagged --latest -- proposed_updates10s-packages-main-release",
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
                "pkgs --src -F line:name,version,release -b c10s -- PackageKit",
                "PackageKit : 1.2.8 : 8.el10",
            )],
        );
        let inv_path = dir.path().join("inv.toml");
        std::fs::write(&inv_path, STATUS_INVENTORY).unwrap();

        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{existing_path}", dir.path().display());
        let _guard = EnvGuard::new(&[
            ("GITLAB_TOKEN", "test-token"),
            // The developer's own config must not leak into a test.
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
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
        assert_eq!(r.jira.len(), 1);
        assert_eq!(r.jira[0].key, "RHEL-1");
        assert_eq!(r.jira[0].status.as_deref(), Some("In Progress"));
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
                    "list-tagged --latest -- proposed_updates10s-packages-main-release",
                    &koji_list_tagged_output(
                        "xz-5.6.4-1~proposed.el10",
                        "proposed_updates10s-packages-main-release",
                    ),
                ),
                (
                    "list-tagged --quiet -- proposed_updates10s-packages-main-testing",
                    "",
                ),
            ],
        );
        install_fake_bin(
            dir.path(),
            "fedrq",
            &[(
                "pkgs --src -F line:name,version,release -b c10s -- xz",
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
            // The developer's own config must not leak into a test.
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
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

    #[test]
    fn refresh_adds_cve_lines_under_jira_and_keeps_existing_ones() {
        let jira = [JiraState {
            key: "RHEL-303721".into(),
            status: Some("New".into()),
            resolution: None,
            resolved: false,
            verdict: None,
        }];
        let original = "- **MR**: [t](https://example/-/merge_requests/90) — opened\n\
                        - **JIRA**: [RHEL-303721](https://x/browse/RHEL-303721) — New\n\
                        - **Release**: c10s\n\
                        - **Affected build**: _(unknown)_\n";
        let line = crate::triage::cve_line("CVE-2026-119741", &[]);
        let args = |body, new: Option<&'static [String]>| RefreshedBodyArgs {
            original_body: body,
            release: "c10s",
            mr_url: "https://example/-/merge_requests/90",
            mr_title: "t",
            mr_state: Some("opened"),
            jira: &jira,
            new_cve_lines: new,
            builds: (None, None),
        };
        let lines: &'static [String] = Box::leak(vec![line.clone()].into_boxed_slice());
        let out = refreshed_body(args(original, Some(lines)));
        assert!(
            out.contains(&format!("— New\n{line}\n- **Release**: c10s\n")),
            "{out}"
        );
        assert!(out.contains("- **Affected build**: _(unknown)_"), "{out}");
        // Refreshed again, the line is the body's own and stays put.
        let again = refreshed_body(args(Box::leak(out.clone().into_boxed_str()), None));
        assert_eq!(again, out);
    }

    #[test]
    fn refresh_labels_a_cve_fix_security_and_corrects_its_priority() {
        let labels = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            label_changes(
                &labels(&["c10s", "cpu-sig-tracker"]),
                true,
                Some("priority::major")
            ),
            (
                vec!["priority::major".to_string(), "security".to_string()],
                vec![]
            )
        );
        // A type someone set is left alone.
        assert_eq!(
            label_changes(
                &labels(&["bugfix", "priority::minor"]),
                true,
                Some("priority::major")
            ),
            (
                vec!["priority::major".to_string()],
                vec!["priority::minor".to_string()]
            )
        );
        assert_eq!(
            label_changes(
                &labels(&["security", "priority::major"]),
                true,
                Some("priority::major")
            ),
            (vec![], vec![])
        );
        assert_eq!(label_changes(&labels(&[]), false, None), (vec![], vec![]));
    }

    #[test]
    fn refresh_fills_unknown_build_lines_from_the_evidence() {
        let jira = [JiraState {
            key: "RHEL-303721".into(),
            status: Some("New".into()),
            resolution: None,
            resolved: false,
            verdict: None,
        }];
        let original = "- **MR**: [t](https://example/-/merge_requests/90) — opened\n\
                        - **JIRA**: [RHEL-303721](https://x/browse/RHEL-303721) — New\n\
                        - **Release**: c10s\n\
                        - **Affected build**: _(unknown)_\n\
                        - **Expected fix**: _(unknown)_\n";
        let out = refreshed_body(RefreshedBodyArgs {
            original_body: original,
            release: "c10s",
            mr_url: "https://example/-/merge_requests/90",
            mr_title: "t",
            mr_state: Some("opened"),
            jira: &jira,
            new_cve_lines: None,
            builds: (Some("rsync-3.5.0-1.el10"), Some("rsync-3.5.0-2.el10")),
        });
        assert!(
            out.ends_with(
                "- **Affected build**: rsync-3.5.0-1.el10\n\
                 - **Expected fix**: rsync-3.5.0-2.el10\n"
            ),
            "{out}"
        );
    }
}
