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
use sandogasa_forgejo::{Client, PullRequest};
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
}

impl PrRef {
    /// A state marker for the **opened** list, so a PR that was
    /// opened in the window but has since landed or been declined
    /// isn't shown as if it were still open. Empty for an open PR.
    fn status_marker(&self) -> &'static str {
        if self.merged {
            " (merged)"
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
            report.opened_prs.push(into_pr_ref(&pr));
        }
        if pr.is_merged()
            && pr
                .merged_at()
                .is_some_and(|d| date_in_range(d, since, until))
        {
            report.merged_prs.push(into_pr_ref(&pr));
        }
    }
    Ok(report)
}

/// Format the Forgejo section as Markdown.
pub fn format_markdown(report: &ForgejoReport, detail: u8) -> String {
    let detailed = detail >= 1;
    let heading = "### Forgejo\n\n".to_string();

    if report.opened_prs.is_empty() && report.merged_prs.is_empty() {
        let mut out = heading;
        out.push_str("No Forgejo activity.\n\n");
        return out;
    }

    let mut out = heading;
    out.push_str(&format!("- **PRs opened:** {}\n", report.opened_prs.len()));
    out.push_str(&format!(
        "- **PRs merged:** {}\n\n",
        report.merged_prs.len()
    ));

    if !detailed {
        return out;
    }

    if !report.opened_prs.is_empty() {
        out.push_str("#### Opened\n\n");
        write_pr_list(&mut out, &report.opened_prs, true);
    }
    if !report.merged_prs.is_empty() {
        out.push_str("#### Merged\n\n");
        write_pr_list(&mut out, &report.merged_prs, false);
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
    }
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
        });
        let md = format_markdown(&report, 1);
        assert!(md.contains("### Forgejo\n"));
        assert!(md.contains("**PRs merged:** 1"));
        assert!(md.contains("#### Merged"));
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
        // Opened then declined (closed, not merged) → "(closed)".
        report.opened_prs.push(PrRef {
            repo: "ptesarik/libkdumpfile".into(),
            number: 92,
            title: "Drop removed bfd macros".into(),
            url: "https://codeberg.org/ptesarik/libkdumpfile/pulls/92".into(),
            state: "closed".into(),
            merged: false,
        });
        // Opened and still open → no marker.
        report.opened_prs.push(PrRef {
            repo: "o/r".into(),
            number: 7,
            title: "still going".into(),
            url: "https://codeberg.org/o/r/pulls/7".into(),
            state: "open".into(),
            merged: false,
        });
        let md = format_markdown(&report, 1);
        assert!(md.contains("#92](https://codeberg.org/ptesarik/libkdumpfile/pulls/92) Drop removed bfd macros (closed)"));
        assert!(md.contains("#7](https://codeberg.org/o/r/pulls/7) still going\n"));
        assert!(!md.contains("still going ("));
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
