// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GitLab activity reporting — MRs authored/merged, approvals,
//! comments, and pushed-commit counts for a user in a date window.
//! Fetched from the user-events endpoint and optionally scoped by
//! a `path_with_namespace` prefix so the same instance can be
//! queried for multiple domains (e.g. Hyperscale vs Proposed
//! Updates on gitlab.com).

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use sandogasa_gitlab::{Event, project_summary, user_by_username, user_events};
use serde::Serialize;

use crate::config::GitlabConfig;

/// A user's GitLab activity for a single domain.
#[derive(Debug, Default, Serialize)]
pub struct GitlabReport {
    /// Base URL for the GitLab instance.
    pub instance: String,
    /// GitLab username actually queried (may differ from the CLI
    /// `--user` when the domain config overrides it).
    pub user: String,
    /// Group-prefix filter that shaped this report, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,

    /// MRs the user opened.
    pub opened_mrs: Vec<MrRef>,
    /// MRs the user merged (their own or others').
    pub merged_mrs: Vec<MrRef>,
    /// MRs the user approved.
    pub approved_mrs: Vec<MrRef>,
    /// MRs the user commented on (deduplicated per MR).
    pub commented_mrs: Vec<MrRef>,

    /// Commit totals per project, summed from push events.
    /// Individual commits are not enumerated — GitLab's activity
    /// feed reports them by push, not by commit.
    pub commits_by_project: BTreeMap<String, u64>,
}

/// Pointer back to a merge request in the report's summary lists.
#[derive(Debug, Clone, Serialize)]
pub struct MrRef {
    /// Project path, e.g. `CentOS/Hyperscale/rpms/perf`.
    pub project: String,
    /// Merge request internal ID (project-scoped).
    pub iid: u64,
    /// MR title.
    pub title: String,
}

/// Build the GitLab activity report for one domain.
pub fn gitlab_report(
    cfg: &GitlabConfig,
    user: &str,
    since: NaiveDate,
    until: NaiveDate,
    verbose: bool,
) -> Result<GitlabReport, String> {
    let token = find_token(&cfg.instance)?;
    let base = cfg.instance.trim_end_matches('/');

    if verbose {
        eprintln!("[gitlab] {base}: resolving user {user}");
    }

    let user_obj = user_by_username(base, &token, user)
        .map_err(|e| format!("GitLab user lookup on {base}: {e}"))?
        .ok_or_else(|| format!("user '{user}' not found on {base}"))?;

    // GitLab events are half-open on both ends: after/before days
    // are excluded. Widen by one day and re-clamp client-side.
    let after = since - chrono::Duration::days(1);
    let before = until + chrono::Duration::days(1);

    if verbose {
        eprintln!(
            "[gitlab] {base}: fetching events for user id {} ({user})",
            user_obj.id
        );
    }

    let events = user_events(base, &token, user_obj.id, None, after, before)
        .map_err(|e| format!("GitLab events on {base}: {e}"))?;

    if verbose {
        eprintln!("[gitlab] {base}: fetched {} events", events.len());
    }

    let paths = resolve_project_paths(base, &token, &events, verbose);

    let mut report = GitlabReport {
        instance: base.to_string(),
        user: user.to_string(),
        group: cfg.group.clone(),
        ..Default::default()
    };
    let mut commented_seen: BTreeSet<(u64, u64)> = BTreeSet::new();

    for ev in &events {
        if !in_date_range(&ev.created_at, since, until) {
            continue;
        }
        let Some(path) = paths.get(&ev.project_id) else {
            continue;
        };
        if !path_in_group(path, cfg.group.as_deref()) {
            continue;
        }
        dispatch_event(ev, path, &mut report, &mut commented_seen);
    }

    Ok(report)
}

/// Format the GitLab section as Markdown. `heading_suffix` is
/// `Some("<domain>")` for multi-domain runs and `None` otherwise,
/// mirroring the Koji formatter.
pub fn format_markdown(
    report: &GitlabReport,
    detailed: bool,
    heading_suffix: Option<&str>,
) -> String {
    let heading = match heading_suffix {
        Some(s) => format!("## GitLab ({s})\n\n"),
        None => "## GitLab\n\n".to_string(),
    };

    if report.opened_mrs.is_empty()
        && report.merged_mrs.is_empty()
        && report.approved_mrs.is_empty()
        && report.commented_mrs.is_empty()
        && report.commits_by_project.is_empty()
    {
        let mut out = heading;
        out.push_str("No GitLab activity.\n\n");
        return out;
    }

    let total_commits: u64 = report.commits_by_project.values().sum();
    let mut out = heading;
    out.push_str(&format!("- **MRs opened:** {}\n", report.opened_mrs.len()));
    out.push_str(&format!("- **MRs merged:** {}\n", report.merged_mrs.len()));
    out.push_str(&format!(
        "- **MRs approved:** {}\n",
        report.approved_mrs.len()
    ));
    out.push_str(&format!(
        "- **MRs commented on:** {}\n",
        report.commented_mrs.len()
    ));
    out.push_str(&format!(
        "- **Commits pushed:** {total_commits} across {} project(s)\n\n",
        report.commits_by_project.len()
    ));

    if !detailed {
        return out;
    }

    if !report.opened_mrs.is_empty() {
        out.push_str("### Opened\n\n");
        write_mr_list(&mut out, &report.opened_mrs, &report.instance);
    }
    if !report.merged_mrs.is_empty() {
        out.push_str("### Merged\n\n");
        write_mr_list(&mut out, &report.merged_mrs, &report.instance);
    }
    if !report.approved_mrs.is_empty() {
        out.push_str("### Approved\n\n");
        write_mr_list(&mut out, &report.approved_mrs, &report.instance);
    }
    if !report.commented_mrs.is_empty() {
        out.push_str("### Commented on\n\n");
        write_mr_list(&mut out, &report.commented_mrs, &report.instance);
    }
    if !report.commits_by_project.is_empty() {
        out.push_str("### Commits by project\n\n");
        for (project, count) in &report.commits_by_project {
            out.push_str(&format!("- `{project}`: {count} commits\n"));
        }
        out.push('\n');
    }
    out
}

/// Resolve each event's `project_id` to its `path_with_namespace`.
/// Failed lookups degrade to `project-<id>` so a single broken
/// project doesn't abort the whole report.
fn resolve_project_paths(
    base: &str,
    token: &str,
    events: &[Event],
    verbose: bool,
) -> BTreeMap<u64, String> {
    let mut paths: BTreeMap<u64, String> = BTreeMap::new();
    let unique_ids: BTreeSet<u64> = events.iter().map(|e| e.project_id).collect();
    for pid in &unique_ids {
        match project_summary(base, token, *pid) {
            Ok(p) => {
                paths.insert(*pid, p.path_with_namespace);
            }
            Err(e) => {
                if verbose {
                    eprintln!("[gitlab] {base}: project {pid} lookup failed: {e}");
                }
                paths.insert(*pid, format!("project-{pid}"));
            }
        }
    }
    paths
}

fn in_date_range(created_at: &str, since: NaiveDate, until: NaiveDate) -> bool {
    let Some(day) = created_at.split('T').next() else {
        return false;
    };
    match NaiveDate::parse_from_str(day, "%Y-%m-%d") {
        Ok(d) => d >= since && d <= until,
        Err(_) => false,
    }
}

fn path_in_group(path: &str, group: Option<&str>) -> bool {
    match group {
        None => true,
        Some(prefix) => {
            let prefix = prefix.trim_end_matches('/');
            path == prefix || path.starts_with(&format!("{prefix}/"))
        }
    }
}

fn dispatch_event(
    ev: &Event,
    path: &str,
    report: &mut GitlabReport,
    commented: &mut BTreeSet<(u64, u64)>,
) {
    let is_mr = ev.target_type.as_deref() == Some("MergeRequest");
    match (ev.action_name.as_str(), is_mr) {
        ("opened", true) => push_mr(&mut report.opened_mrs, path, ev),
        ("merged", true) => push_mr(&mut report.merged_mrs, path, ev),
        ("approved", true) => push_mr(&mut report.approved_mrs, path, ev),
        ("commented on", _) => {
            if let Some(note) = &ev.note
                && note.noteable_type.as_deref() == Some("MergeRequest")
                && let Some(iid) = note.noteable_iid
                && commented.insert((ev.project_id, iid))
            {
                report.commented_mrs.push(MrRef {
                    project: path.to_string(),
                    iid,
                    title: ev.target_title.clone().unwrap_or_default(),
                });
            }
        }
        ("pushed to", _) | ("pushed new", _) => {
            if let Some(pd) = &ev.push_data {
                *report
                    .commits_by_project
                    .entry(path.to_string())
                    .or_insert(0) += pd.commit_count;
            }
        }
        _ => {}
    }
}

fn push_mr(dest: &mut Vec<MrRef>, path: &str, ev: &Event) {
    if let (Some(iid), Some(title)) = (ev.target_iid, ev.target_title.clone()) {
        dest.push(MrRef {
            project: path.to_string(),
            iid,
            title,
        });
    }
}

fn write_mr_list(out: &mut String, mrs: &[MrRef], instance: &str) {
    let base = instance.trim_end_matches('/');
    for mr in mrs {
        out.push_str(&format!(
            "- [{}!{}]({base}/{}/-/merge_requests/{}) {}\n",
            mr.project, mr.iid, mr.project, mr.iid, mr.title,
        ));
    }
    out.push('\n');
}

/// Look up the GitLab API token for an instance. Tries an
/// instance-specific env var first, then the generic fallback.
///
/// For `https://salsa.debian.org`, the instance-specific var is
/// `GITLAB_TOKEN_SALSA_DEBIAN_ORG`; for `https://gitlab.com` it's
/// `GITLAB_TOKEN_GITLAB_COM`. The generic `GITLAB_TOKEN` matches
/// the convention used by other sandogasa tools.
fn find_token(instance: &str) -> Result<String, String> {
    let var = instance_token_env(instance);
    if let Ok(t) = std::env::var(&var) {
        return Ok(t);
    }
    std::env::var("GITLAB_TOKEN")
        .map_err(|_| format!("no GitLab token: set {var} (instance-specific) or GITLAB_TOKEN"))
}

fn instance_token_env(instance: &str) -> String {
    let host = instance
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    format!("GITLAB_TOKEN_{}", host.to_uppercase().replace('.', "_"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event(
        action: &str,
        target_type: Option<&str>,
        project_id: u64,
        iid: Option<u64>,
        title: Option<&str>,
    ) -> Event {
        Event {
            id: 1,
            project_id,
            action_name: action.to_string(),
            target_type: target_type.map(|s| s.to_string()),
            target_iid: iid,
            target_title: title.map(|s| s.to_string()),
            created_at: "2026-02-15T10:00:00Z".to_string(),
            note: None,
            push_data: None,
        }
    }

    #[test]
    fn path_in_group_accepts_exact_prefix() {
        assert!(path_in_group(
            "CentOS/Hyperscale/rpms/perf",
            Some("CentOS/Hyperscale")
        ));
        assert!(path_in_group(
            "CentOS/Hyperscale",
            Some("CentOS/Hyperscale")
        ));
    }

    #[test]
    fn path_in_group_rejects_nonmatch() {
        assert!(!path_in_group(
            "CentOS/Other/foo",
            Some("CentOS/Hyperscale")
        ));
        // Must not match on substring — 'Hyperscale-extras' is not under 'Hyperscale'.
        assert!(!path_in_group(
            "CentOS/Hyperscale-extras/foo",
            Some("CentOS/Hyperscale")
        ));
    }

    #[test]
    fn path_in_group_no_filter_matches_all() {
        assert!(path_in_group("random/path", None));
    }

    #[test]
    fn in_date_range_parses_iso8601() {
        let s = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let u = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        assert!(in_date_range("2026-02-15T10:00:00Z", s, u));
        assert!(!in_date_range("2025-12-31T23:59:59Z", s, u));
        assert!(!in_date_range("2026-04-01T00:00:00Z", s, u));
    }

    #[test]
    fn dispatch_opened_mr() {
        let ev = sample_event("opened", Some("MergeRequest"), 10, Some(5), Some("Fix"));
        let mut report = GitlabReport::default();
        let mut seen = BTreeSet::new();
        dispatch_event(&ev, "a/b", &mut report, &mut seen);
        assert_eq!(report.opened_mrs.len(), 1);
        assert_eq!(report.opened_mrs[0].iid, 5);
    }

    #[test]
    fn dispatch_commented_dedups_per_mr() {
        let note = sandogasa_gitlab::EventNote {
            noteable_type: Some("MergeRequest".to_string()),
            noteable_iid: Some(7),
            body: Some("LGTM".to_string()),
        };
        let mut ev = sample_event("commented on", None, 10, None, Some("Fix X"));
        ev.note = Some(note);
        let mut report = GitlabReport::default();
        let mut seen = BTreeSet::new();
        dispatch_event(&ev, "a/b", &mut report, &mut seen);
        dispatch_event(&ev, "a/b", &mut report, &mut seen);
        assert_eq!(report.commented_mrs.len(), 1);
    }

    #[test]
    fn dispatch_push_accumulates_commits() {
        let mut ev = sample_event("pushed to", None, 10, None, None);
        ev.push_data = Some(sandogasa_gitlab::EventPushData {
            commit_count: 3,
            action: None,
            ref_type: None,
            ref_name: Some("main".to_string()),
            commit_title: None,
        });
        let mut report = GitlabReport::default();
        let mut seen = BTreeSet::new();
        dispatch_event(&ev, "a/b", &mut report, &mut seen);
        dispatch_event(&ev, "a/b", &mut report, &mut seen);
        assert_eq!(report.commits_by_project.get("a/b"), Some(&6));
    }

    #[test]
    fn format_empty_report() {
        let report = GitlabReport {
            instance: "https://gitlab.com".into(),
            group: None,
            ..Default::default()
        };
        let md = format_markdown(&report, false, None);
        assert!(md.contains("## GitLab\n"));
        assert!(md.contains("No GitLab activity"));
    }

    #[test]
    fn format_non_empty_with_suffix() {
        let mut report = GitlabReport {
            instance: "https://gitlab.com".into(),
            group: Some("CentOS/Hyperscale".into()),
            ..Default::default()
        };
        report.opened_mrs.push(MrRef {
            project: "CentOS/Hyperscale/rpms/perf".into(),
            iid: 42,
            title: "Fix build".into(),
        });
        let md = format_markdown(&report, true, Some("hyperscale"));
        assert!(md.contains("## GitLab (hyperscale)"));
        assert!(md.contains("**MRs opened:** 1"));
        assert!(md.contains("### Opened"));
        assert!(md.contains("!42"));
        assert!(md.contains("Fix build"));
    }

    #[test]
    fn instance_token_env_generates_env_var_name() {
        assert_eq!(
            instance_token_env("https://gitlab.com"),
            "GITLAB_TOKEN_GITLAB_COM"
        );
        assert_eq!(
            instance_token_env("https://salsa.debian.org/"),
            "GITLAB_TOKEN_SALSA_DEBIAN_ORG"
        );
    }
}
