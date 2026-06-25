// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Forgejo / Gitea activity reporting — pull requests the user
//! opened and merged in a date window. Sourced from the global
//! issue/pull search (`/repos/issues/search?type=pulls&created=true`),
//! which returns the token owner's PRs across every repo they
//! contribute to (e.g. on codeberg.org, where contributions land in
//! other people's repos, not just the user's own org).
//!
//! Structured to mirror `crate::github` so a Forgejo domain renders
//! the same way and plugs into `report::format_markdown` without
//! special-casing. One search call yields both metrics: a PR counts as
//! *opened* when its `created_at` falls in the window, and *merged*
//! when it was merged and its `merged_at` falls in the window.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use sandogasa_forgejo::{Client, Issue, PullRequest};
use serde::Serialize;

use crate::config::ForgejoConfig;

/// A user's Forgejo activity for a single domain.
#[derive(Debug, Default, Serialize)]
pub struct ForgejoReport {
    /// Instance root URL the report was fetched from.
    pub instance: String,
    /// Forgejo login reported on (the token owner).
    pub user: String,
    /// Repo-owner filter, if set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,

    /// PRs opened by the user in the window.
    pub opened_prs: Vec<PrRef>,
    /// PRs by the user that were merged in the window.
    pub merged_prs: Vec<PrRef>,
    /// Issues opened by the user in the window.
    pub opened_issues: Vec<IssueRef>,
    /// Issues by the user that were closed in the window.
    pub closed_issues: Vec<IssueRef>,
}

/// Pointer to an issue for the report's summary lists.
#[derive(Debug, Clone, Serialize)]
pub struct IssueRef {
    /// `owner/name` slug.
    pub repo: String,
    /// Issue number.
    pub number: u64,
    /// Issue title.
    pub title: String,
    /// Issue URL.
    pub url: String,
}

/// Pointer to a pull request for the report's summary lists.
#[derive(Debug, Clone, Serialize)]
pub struct PrRef {
    /// `owner/name` slug.
    pub repo: String,
    /// PR number.
    pub number: u64,
    /// PR title.
    pub title: String,
    /// PR URL (the canonical web link).
    pub url: String,
    /// Current state (`open` / `closed`). A merged PR is `closed`
    /// with `merged = true`; a declined PR is `closed` with
    /// `merged = false`.
    pub state: String,
    /// Whether the PR was merged (vs. closed without merging).
    pub merged: bool,
    /// Whether a closed-but-unmerged PR's commit nonetheless landed on
    /// the target branch (a maintainer applied it out-of-band rather
    /// than clicking merge). Determined by a follow-up API check.
    #[serde(default)]
    pub applied: bool,
}

impl PrRef {
    /// A state marker for the **opened** list, so a PR that was
    /// opened in the window but has since landed or been declined
    /// isn't shown as if it were still open. Empty for an open PR.
    fn status_marker(&self) -> &'static str {
        if self.merged {
            " (merged)"
        } else if self.applied {
            " (applied)"
        } else if self.state == "closed" {
            " (closed)"
        } else {
            ""
        }
    }
}

/// Build the Forgejo activity report for one domain.
pub fn forgejo_report(
    cfg: &ForgejoConfig,
    user: &str,
    since: NaiveDate,
    until: NaiveDate,
    tokens: &BTreeMap<String, String>,
    verbose: bool,
) -> Result<ForgejoReport, String> {
    let token = find_token(&cfg.instance, tokens)?;
    let base = cfg.instance.trim_end_matches('/');
    let client =
        Client::new(base, &token).map_err(|e| format!("Forgejo client setup on {base}: {e}"))?;

    if verbose {
        eprintln!("[forgejo] {base}: searching pulls created by the token owner");
    }
    // One `state=all` search gives both opened and merged; we filter
    // each by the relevant timestamp.
    let pulls = client
        .my_pull_requests("all", cfg.owner.as_deref())
        .map_err(|e| format!("Forgejo pull search on {base}: {e}"))?;

    let mut report = ForgejoReport {
        instance: base.to_string(),
        user: user.to_string(),
        owner: cfg.owner.clone(),
        ..Default::default()
    };
    for pr in pulls {
        if pr
            .created_at
            .as_deref()
            .is_some_and(|d| date_in_range(d, since, until))
        {
            let mut r = into_pr_ref(&pr);
            // A PR opened in the window but now closed-without-merging
            // may still have landed (commit applied out-of-band). Check
            // whether its head commit is on the target branch.
            if r.state == "closed" && !r.merged {
                r.applied = applied_out_of_band(&client, &r, verbose);
            }
            report.opened_prs.push(r);
        }
        if pr.is_merged()
            && pr
                .merged_at()
                .is_some_and(|d| date_in_range(d, since, until))
        {
            report.merged_prs.push(into_pr_ref(&pr));
        }
    }

    if verbose {
        eprintln!("[forgejo] {base}: searching issues created by the token owner");
    }
    let issues = client
        .my_issues("all", cfg.owner.as_deref())
        .map_err(|e| format!("Forgejo issue search on {base}: {e}"))?;
    for issue in issues {
        if issue
            .created_at
            .as_deref()
            .is_some_and(|d| date_in_range(d, since, until))
        {
            report.opened_issues.push(into_issue_ref(&issue));
        }
        if issue.state == "closed"
            && issue
                .closed_at()
                .is_some_and(|d| date_in_range(d, since, until))
        {
            report.closed_issues.push(into_issue_ref(&issue));
        }
    }
    Ok(report)
}

/// Whether a closed-unmerged PR's commit nonetheless landed on its
/// target branch. Fetches the PR's head/base refs (the search result
/// omits them), then asks whether the head commit is contained in the
/// base branch. Any lookup failure is treated as "not applied" (a
/// warning under `--verbose`) — we only ever *upgrade* the label when
/// we're certain, never falsely claim a contribution landed.
fn applied_out_of_band(client: &Client, pr: &PrRef, verbose: bool) -> bool {
    let Some((owner, repo)) = pr.repo.split_once('/') else {
        return false;
    };
    let detail = match client.pull_request(owner, repo, pr.number) {
        Ok(d) => d,
        Err(e) => {
            if verbose {
                eprintln!(
                    "[forgejo] applied-check: fetch {}#{} failed: {e}",
                    pr.repo, pr.number
                );
            }
            return false;
        }
    };
    match client.commit_contained(owner, repo, &detail.base.ref_name, &detail.head.sha) {
        Ok(contained) => contained,
        Err(e) => {
            if verbose {
                eprintln!(
                    "[forgejo] applied-check: compare for {}#{} failed: {e}",
                    pr.repo, pr.number
                );
            }
            false
        }
    }
}

/// Format the Forgejo section as Markdown.
pub fn format_markdown(report: &ForgejoReport, detail: u8) -> String {
    let detailed = detail >= 1;
    let heading = "### Forgejo\n\n".to_string();

    if report.opened_prs.is_empty()
        && report.merged_prs.is_empty()
        && report.opened_issues.is_empty()
        && report.closed_issues.is_empty()
    {
        let mut out = heading;
        out.push_str("No Forgejo activity.\n\n");
        return out;
    }

    let applied = report.opened_prs.iter().filter(|p| p.applied).count();
    let mut out = heading;
    out.push_str(&format!("- **PRs opened:** {}\n", report.opened_prs.len()));
    out.push_str(&format!("- **PRs merged:** {}\n", report.merged_prs.len()));
    // A closed PR whose commit landed out-of-band — counted separately
    // since the forge reports it as neither merged nor open.
    if applied > 0 {
        out.push_str(&format!("- **PRs applied (landed unmerged):** {applied}\n"));
    }
    out.push_str(&format!(
        "- **Issues opened:** {}\n",
        report.opened_issues.len()
    ));
    out.push_str(&format!(
        "- **Issues closed:** {}\n\n",
        report.closed_issues.len()
    ));

    if !detailed {
        return out;
    }

    if !report.opened_prs.is_empty() {
        out.push_str("#### PRs opened\n\n");
        write_pr_list(&mut out, &report.opened_prs, true);
    }
    if !report.merged_prs.is_empty() {
        out.push_str("#### PRs merged\n\n");
        write_pr_list(&mut out, &report.merged_prs, false);
    }
    if !report.opened_issues.is_empty() {
        out.push_str("#### Issues opened\n\n");
        write_issue_list(&mut out, &report.opened_issues);
    }
    if !report.closed_issues.is_empty() {
        out.push_str("#### Issues closed\n\n");
        write_issue_list(&mut out, &report.closed_issues);
    }
    out
}

fn into_pr_ref(pr: &PullRequest) -> PrRef {
    PrRef {
        repo: pr.repo_slug().unwrap_or_default().to_string(),
        number: pr.number,
        title: pr.title.clone(),
        url: pr.html_url.clone(),
        state: pr.state.clone(),
        merged: pr.is_merged(),
        applied: false,
    }
}

fn into_issue_ref(issue: &Issue) -> IssueRef {
    IssueRef {
        repo: issue.repo_slug().unwrap_or_default().to_string(),
        number: issue.number,
        title: issue.title.clone(),
        url: issue.html_url.clone(),
    }
}

fn write_issue_list(out: &mut String, issues: &[IssueRef]) {
    for it in issues {
        out.push_str(&format!(
            "- [{}#{}]({}) {}\n",
            it.repo, it.number, it.url, it.title,
        ));
    }
    out.push('\n');
}

/// Render a PR list. `with_status` appends a `(merged)`/`(closed)`
/// marker — used for the opened list, where a PR's later fate matters;
/// the merged list is uniformly merged, so it's rendered without.
fn write_pr_list(out: &mut String, prs: &[PrRef], with_status: bool) {
    for pr in prs {
        let marker = if with_status { pr.status_marker() } else { "" };
        out.push_str(&format!(
            "- [{}#{}]({}) {}{marker}\n",
            pr.repo, pr.number, pr.url, pr.title,
        ));
    }
    out.push('\n');
}

/// Whether an RFC 3339 timestamp's date falls within `[since, until]`
/// (inclusive). Only the date part is considered.
fn date_in_range(ts: &str, since: NaiveDate, until: NaiveDate) -> bool {
    let Some(day) = ts.split('T').next() else {
        return false;
    };
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| d >= since && d <= until)
        .unwrap_or(false)
}

/// Look up the Forgejo API token for an instance.
///
/// Order: instance-specific env var → generic env var →
/// `forgejo_tokens.<hostname>` from the user overlay → error.
fn find_token(instance: &str, tokens: &BTreeMap<String, String>) -> Result<String, String> {
    let var = instance_token_env(instance);
    if let Ok(t) = std::env::var(&var) {
        return Ok(t);
    }
    if let Ok(t) = std::env::var("FORGEJO_TOKEN") {
        return Ok(t);
    }
    let host = instance_host(instance);
    if let Some(t) = tokens.get(&host) {
        return Ok(t.clone());
    }
    Err(format!(
        "no Forgejo token for {host}: set {var} (instance-specific), \
         FORGEJO_TOKEN (generic), or run `sandogasa-report config` to \
         store one in the overlay"
    ))
}

fn instance_token_env(instance: &str) -> String {
    format!(
        "FORGEJO_TOKEN_{}",
        instance_host(instance).to_uppercase().replace('.', "_")
    )
}

/// Strip scheme + trailing slash to get the bare hostname — the
/// token-keying host.
pub(crate) fn instance_host(instance: &str) -> String {
    instance
        .trim_end_matches('/')
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(json: &str) -> PullRequest {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn date_in_range_parses_rfc3339() {
        let s = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let u = NaiveDate::from_ymd_opt(2026, 6, 30).unwrap();
        assert!(date_in_range("2026-06-25T11:22:09+02:00", s, u));
        assert!(!date_in_range("2026-07-01T00:00:00Z", s, u));
        assert!(!date_in_range("garbage", s, u));
    }

    #[test]
    fn into_pr_ref_pulls_slug_and_url() {
        let p = pr(r#"{"number":35,"title":"Fix select box","state":"closed",
            "html_url":"https://codeberg.org/o/r/pulls/35",
            "repository":{"full_name":"o/r","name":"r","owner":"o"}}"#);
        let r = into_pr_ref(&p);
        assert_eq!(r.repo, "o/r");
        assert_eq!(r.number, 35);
        assert_eq!(r.url, "https://codeberg.org/o/r/pulls/35");
    }

    #[test]
    fn instance_token_env_generates_env_var_name() {
        assert_eq!(
            instance_token_env("https://codeberg.org/"),
            "FORGEJO_TOKEN_CODEBERG_ORG"
        );
    }

    #[test]
    fn find_token_errors_when_nothing_set() {
        let tokens = BTreeMap::new();
        let err = find_token("https://nonexistent.example.test", &tokens).unwrap_err();
        assert!(err.contains("no Forgejo token"));
    }

    #[test]
    fn find_token_falls_back_to_config() {
        let mut tokens = BTreeMap::new();
        tokens.insert(
            "nonexistent.example.test".to_string(),
            "from-config".to_string(),
        );
        let var = instance_token_env("https://nonexistent.example.test");
        if std::env::var(&var).is_ok() || std::env::var("FORGEJO_TOKEN").is_ok() {
            return;
        }
        let tok = find_token("https://nonexistent.example.test", &tokens).unwrap();
        assert_eq!(tok, "from-config");
    }

    #[test]
    fn format_empty_report() {
        let report = ForgejoReport {
            instance: "https://codeberg.org".into(),
            user: "michelin".into(),
            ..Default::default()
        };
        let md = format_markdown(&report, 0);
        assert!(md.contains("### Forgejo\n"));
        assert!(md.contains("No Forgejo activity"));
    }

    #[test]
    fn format_non_empty_lists_merged() {
        let mut report = ForgejoReport {
            instance: "https://codeberg.org".into(),
            user: "michelin".into(),
            ..Default::default()
        };
        report.merged_prs.push(PrRef {
            repo: "ptesarik/libkdumpfile".into(),
            number: 92,
            title: "Drop removed bfd macros".into(),
            url: "https://codeberg.org/ptesarik/libkdumpfile/pulls/92".into(),
            state: "closed".into(),
            merged: true,
            applied: false,
        });
        let md = format_markdown(&report, 1);
        assert!(md.contains("### Forgejo\n"));
        assert!(md.contains("**PRs merged:** 1"));
        assert!(md.contains("#### PRs merged"));
        assert!(md.contains("ptesarik/libkdumpfile#92"));
        assert!(md.contains("Drop removed bfd macros"));
        // The merged list isn't annotated with a redundant marker.
        assert!(!md.contains("(merged)"));
    }

    #[test]
    fn opened_list_marks_closed_and_merged_state() {
        let mut report = ForgejoReport {
            instance: "https://codeberg.org".into(),
            user: "michelin".into(),
            ..Default::default()
        };
        // Opened then declined (closed, not merged, not applied) → "(closed)".
        report.opened_prs.push(PrRef {
            repo: "ptesarik/libkdumpfile".into(),
            number: 92,
            title: "Drop removed bfd macros".into(),
            url: "https://codeberg.org/ptesarik/libkdumpfile/pulls/92".into(),
            state: "closed".into(),
            merged: false,
            applied: false,
        });
        // Opened and still open → no marker.
        report.opened_prs.push(PrRef {
            repo: "o/r".into(),
            number: 7,
            title: "still going".into(),
            url: "https://codeberg.org/o/r/pulls/7".into(),
            state: "open".into(),
            merged: false,
            applied: false,
        });
        // Closed without merging, but the commit landed out-of-band → "(applied)".
        report.opened_prs.push(PrRef {
            repo: "o/r".into(),
            number: 8,
            title: "taken via cherry-pick".into(),
            url: "https://codeberg.org/o/r/pulls/8".into(),
            state: "closed".into(),
            merged: false,
            applied: true,
        });
        let md = format_markdown(&report, 1);
        assert!(md.contains("#92](https://codeberg.org/ptesarik/libkdumpfile/pulls/92) Drop removed bfd macros (closed)"));
        assert!(md.contains("#7](https://codeberg.org/o/r/pulls/7) still going\n"));
        assert!(!md.contains("still going ("));
        assert!(
            md.contains("#8](https://codeberg.org/o/r/pulls/8) taken via cherry-pick (applied)")
        );
        // The summary surfaces the applied count (one of the three).
        assert!(md.contains("- **PRs applied (landed unmerged):** 1"));
    }

    #[test]
    fn applied_count_omitted_when_zero() {
        let mut report = ForgejoReport {
            instance: "https://codeberg.org".into(),
            user: "michelin".into(),
            ..Default::default()
        };
        report.opened_prs.push(PrRef {
            repo: "o/r".into(),
            number: 1,
            title: "x".into(),
            url: "https://codeberg.org/o/r/pulls/1".into(),
            state: "open".into(),
            merged: false,
            applied: false,
        });
        let md = format_markdown(&report, 0);
        assert!(!md.contains("PRs applied"));
    }

    #[test]
    fn issues_render_opened_and_closed() {
        let mut report = ForgejoReport {
            instance: "https://codeberg.org".into(),
            user: "michelin".into(),
            ..Default::default()
        };
        report.opened_issues.push(IssueRef {
            repo: "ptesarik/libkdumpfile".into(),
            number: 91,
            title: "build fails with binutils 2.46".into(),
            url: "https://codeberg.org/ptesarik/libkdumpfile/issues/91".into(),
        });
        let md = format_markdown(&report, 1);
        assert!(md.contains("- **Issues opened:** 1"));
        assert!(md.contains("- **Issues closed:** 0"));
        assert!(md.contains("#### Issues opened"));
        assert!(md.contains(
            "[ptesarik/libkdumpfile#91](https://codeberg.org/ptesarik/libkdumpfile/issues/91) build fails with binutils 2.46"
        ));
    }

    #[test]
    fn into_issue_ref_pulls_slug_and_url() {
        let issue: Issue = serde_json::from_str(
            r#"{"number":91,"title":"build fails","state":"closed",
                "html_url":"https://codeberg.org/o/r/issues/91",
                "repository":{"full_name":"o/r","name":"r","owner":"o"}}"#,
        )
        .unwrap();
        let r = into_issue_ref(&issue);
        assert_eq!(r.repo, "o/r");
        assert_eq!(r.number, 91);
        assert_eq!(r.url, "https://codeberg.org/o/r/issues/91");
    }

    #[test]
    fn report_partitions_opened_and_merged_by_timestamp() {
        // Built by faking the client's output via the public type:
        // we exercise the partition logic, not the HTTP layer.
        let since = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let until = NaiveDate::from_ymd_opt(2026, 6, 30).unwrap();
        let pulls = vec![
            // opened + merged in window.
            pr(r#"{"number":1,"title":"a","state":"closed",
                "html_url":"https://codeberg.org/o/r/pulls/1",
                "created_at":"2026-06-10T00:00:00Z",
                "repository":{"full_name":"o/r","name":"r","owner":"o"},
                "pull_request":{"merged":true,"merged_at":"2026-06-20T00:00:00Z","draft":false}}"#),
            // opened in window, still open (not merged).
            pr(r#"{"number":2,"title":"b","state":"open",
                "html_url":"https://codeberg.org/o/r/pulls/2",
                "created_at":"2026-06-15T00:00:00Z",
                "repository":{"full_name":"o/r","name":"r","owner":"o"}}"#),
            // merged in window but opened earlier — merged only.
            pr(r#"{"number":3,"title":"c","state":"closed",
                "html_url":"https://codeberg.org/o/r/pulls/3",
                "created_at":"2026-05-01T00:00:00Z",
                "repository":{"full_name":"o/r","name":"r","owner":"o"},
                "pull_request":{"merged":true,"merged_at":"2026-06-05T00:00:00Z","draft":false}}"#),
            // outside the window entirely.
            pr(r#"{"number":4,"title":"d","state":"closed",
                "html_url":"https://codeberg.org/o/r/pulls/4",
                "created_at":"2026-01-01T00:00:00Z",
                "repository":{"full_name":"o/r","name":"r","owner":"o"},
                "pull_request":{"merged":true,"merged_at":"2026-01-02T00:00:00Z","draft":false}}"#),
        ];
        let mut report = ForgejoReport::default();
        for p in pulls {
            if p.created_at
                .as_deref()
                .is_some_and(|d| date_in_range(d, since, until))
            {
                report.opened_prs.push(into_pr_ref(&p));
            }
            if p.is_merged()
                && p.merged_at()
                    .is_some_and(|d| date_in_range(d, since, until))
            {
                report.merged_prs.push(into_pr_ref(&p));
            }
        }
        let opened: Vec<u64> = report.opened_prs.iter().map(|p| p.number).collect();
        let merged: Vec<u64> = report.merged_prs.iter().map(|p| p.number).collect();
        assert_eq!(opened, vec![1, 2]);
        assert_eq!(merged, vec![1, 3]);
    }
}
