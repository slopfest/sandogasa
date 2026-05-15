// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GitHub activity reporting — PRs authored/merged, reviews,
//! comments, and authored commit counts. Sourced from a
//! combination of the Search Issues API (for PRs in a date
//! window) and `/users/{login}/events` (to discover which repos
//! the user touched, then per-repo authored commits).
//!
//! Structured to mirror `crate::gitlab` so a multi-domain report
//! renders each forge the same way and the merge into
//! `Report.github` plugs into `report::format_markdown` with no
//! special-casing.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use sandogasa_github::{Client, Event, PullRequest};
use serde::Serialize;

use crate::config::GithubConfig;

/// A user's GitHub activity for a single domain.
#[derive(Debug, Default, Serialize)]
pub struct GithubReport {
    /// API base URL the report was fetched from.
    pub instance: String,
    /// GitHub login that was queried (may differ from the
    /// profile's FAS login).
    pub user: String,
    /// Org/user-namespace filter, if set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,

    /// PRs opened by the user in the window.
    pub opened_prs: Vec<PrRef>,
    /// PRs by the user that were merged in the window.
    pub merged_prs: Vec<PrRef>,
    /// PRs the user formally reviewed (any review state).
    /// GitHub Search doesn't expose an "approved-only" filter,
    /// so this is "had at least one review by the user in the
    /// window" — broader than GitLab's `approved_mrs`.
    pub reviewed_prs: Vec<PrRef>,
    /// PRs the user commented on in the window.
    pub commented_prs: Vec<PrRef>,

    /// Per-repo count of commits authored by the user. Computed
    /// only for repos the user pushed to (discovered via the
    /// events endpoint).
    pub commits_authored: BTreeMap<String, u64>,
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
}

/// Build the GitHub activity report for one domain.
pub fn github_report(
    cfg: &GithubConfig,
    user: &str,
    since: NaiveDate,
    until: NaiveDate,
    tokens: &BTreeMap<String, String>,
    verbose: bool,
) -> Result<GithubReport, String> {
    let token = find_token(&cfg.instance, tokens)?;
    let base = cfg.instance.trim_end_matches('/');
    let client =
        Client::new(base, &token).map_err(|e| format!("GitHub client setup on {base}: {e}"))?;

    if verbose {
        eprintln!("[github] {base}: resolving user {user}");
    }
    let user_obj = client
        .user_by_username(user)
        .map_err(|e| format!("GitHub user lookup on {base}: {e}"))?
        .ok_or_else(|| format!("user '{user}' not found on {base}"))?;

    let mut report = GithubReport {
        instance: base.to_string(),
        user: user_obj.login.clone(),
        org: cfg.org.clone(),
        ..Default::default()
    };

    let org_clause = cfg
        .org
        .as_deref()
        .map(|o| format!(" org:{o}"))
        .unwrap_or_default();
    let login = &user_obj.login;

    // PRs opened in window.
    let q = format!("type:pr author:{login} created:{since}..{until}{org_clause}",);
    report.opened_prs = run_pr_search(&client, &q, verbose)?
        .into_iter()
        .map(into_pr_ref)
        .collect();

    // PRs by the user that were merged in window.
    let q = format!("type:pr author:{login} is:merged merged:{since}..{until}{org_clause}",);
    report.merged_prs = run_pr_search(&client, &q, verbose)?
        .into_iter()
        .map(into_pr_ref)
        .collect();

    // PRs the user reviewed (any review state).
    let q = format!("type:pr reviewed-by:{login} updated:{since}..{until}{org_clause}",);
    report.reviewed_prs = run_pr_search(&client, &q, verbose)?
        .into_iter()
        .map(into_pr_ref)
        .collect();

    // PRs the user commented on.
    let q = format!("type:pr commenter:{login} updated:{since}..{until}{org_clause}",);
    report.commented_prs = run_pr_search(&client, &q, verbose)?
        .into_iter()
        .map(into_pr_ref)
        .collect();

    // Commits: walk events, find PushEvents, filter to org if
    // set, then count authored commits per touched repo.
    if verbose {
        eprintln!("[github] {base}: fetching user events to find pushed repos");
    }
    let events = client
        .user_events(login)
        .map_err(|e| format!("GitHub events on {base}: {e}"))?;
    let touched_repos: BTreeSet<String> = events
        .iter()
        .filter(|e| e.event_type == "PushEvent")
        .filter(|e| event_in_range(e, since, until))
        .filter_map(|e| e.repo_slug().map(String::from))
        .filter(|slug| repo_in_org(slug, cfg.org.as_deref()))
        .collect();

    if verbose {
        eprintln!(
            "[github] {base}: counting authored commits across {} touched repo(s)",
            touched_repos.len()
        );
    }
    for slug in &touched_repos {
        let Some((owner, repo)) = slug.split_once('/') else {
            continue;
        };
        match client.count_authored_commits(owner, repo, login, since, until) {
            Ok(n) => {
                if n > 0 {
                    report.commits_authored.insert(slug.clone(), n);
                }
            }
            Err(e) if verbose => {
                eprintln!("[github] {base}: authored-commit lookup failed for {slug}: {e}");
            }
            Err(_) => {}
        }
    }

    Ok(report)
}

/// Format the GitHub section as Markdown. `heading_suffix` is
/// `Some("<domain>")` for multi-domain runs, `None` otherwise.
pub fn format_markdown(report: &GithubReport, detail: u8, heading_suffix: Option<&str>) -> String {
    let detailed = detail >= 1;
    let heading = match heading_suffix {
        Some(s) => format!("## GitHub ({s})\n\n"),
        None => "## GitHub\n\n".to_string(),
    };

    if report.opened_prs.is_empty()
        && report.merged_prs.is_empty()
        && report.reviewed_prs.is_empty()
        && report.commented_prs.is_empty()
        && report.commits_authored.is_empty()
    {
        let mut out = heading;
        out.push_str("No GitHub activity.\n\n");
        return out;
    }

    let total_authored: u64 = report.commits_authored.values().sum();
    let authored_repos = report.commits_authored.len();
    let mut out = heading;
    out.push_str(&format!("- **PRs opened:** {}\n", report.opened_prs.len()));
    out.push_str(&format!("- **PRs merged:** {}\n", report.merged_prs.len()));
    out.push_str(&format!(
        "- **PRs reviewed:** {}\n",
        report.reviewed_prs.len()
    ));
    out.push_str(&format!(
        "- **PRs commented on:** {}\n",
        report.commented_prs.len()
    ));
    out.push_str(&format!(
        "- **Commits authored:** {total_authored} across {authored_repos} repo(s)\n\n",
    ));

    if !detailed {
        return out;
    }

    if !report.opened_prs.is_empty() {
        out.push_str("### Opened\n\n");
        write_pr_list(&mut out, &report.opened_prs);
    }
    if !report.merged_prs.is_empty() {
        out.push_str("### Merged\n\n");
        write_pr_list(&mut out, &report.merged_prs);
    }
    if !report.reviewed_prs.is_empty() {
        out.push_str("### Reviewed\n\n");
        write_pr_list(&mut out, &report.reviewed_prs);
    }
    if !report.commented_prs.is_empty() {
        out.push_str("### Commented on\n\n");
        write_pr_list(&mut out, &report.commented_prs);
    }
    if !report.commits_authored.is_empty() {
        out.push_str("### Commits by repo\n\n");
        for (repo, count) in &report.commits_authored {
            out.push_str(&format!("- `{repo}`: {count}\n"));
        }
        out.push('\n');
    }
    out
}

fn run_pr_search(client: &Client, query: &str, verbose: bool) -> Result<Vec<PullRequest>, String> {
    if verbose {
        eprintln!("[github] search: {query}");
    }
    client
        .search_pull_requests(query)
        .map_err(|e| format!("GitHub PR search failed: {e}"))
}

fn into_pr_ref(pr: PullRequest) -> PrRef {
    let repo = pr.repo_slug().unwrap_or_default();
    PrRef {
        repo,
        number: pr.number,
        title: pr.title,
        url: pr.html_url,
    }
}

fn write_pr_list(out: &mut String, prs: &[PrRef]) {
    for pr in prs {
        out.push_str(&format!(
            "- [{}#{}]({}) {}\n",
            pr.repo, pr.number, pr.url, pr.title,
        ));
    }
    out.push('\n');
}

fn event_in_range(event: &Event, since: NaiveDate, until: NaiveDate) -> bool {
    let Some(day) = event.created_at.split('T').next() else {
        return false;
    };
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| d >= since && d <= until)
        .unwrap_or(false)
}

fn repo_in_org(slug: &str, org: Option<&str>) -> bool {
    match org {
        None => true,
        Some(prefix) => {
            // Slug is `owner/name`; we match on the owner part
            // (and tolerate slugs that already start with
            // `<prefix>/`).
            let prefix = prefix.trim_end_matches('/');
            slug == prefix
                || slug.starts_with(&format!("{prefix}/"))
                || slug.split_once('/').is_some_and(|(o, _)| o == prefix)
        }
    }
}

/// Look up the GitHub API token for an instance.
///
/// Order: instance-specific env var → generic env var →
/// `github_tokens.<hostname>` from the user overlay → error.
fn find_token(instance: &str, tokens: &BTreeMap<String, String>) -> Result<String, String> {
    let var = instance_token_env(instance);
    if let Ok(t) = std::env::var(&var) {
        return Ok(t);
    }
    if let Ok(t) = std::env::var("GITHUB_TOKEN") {
        return Ok(t);
    }
    let host = instance_host(instance);
    if let Some(t) = tokens.get(&host) {
        return Ok(t.clone());
    }
    Err(format!(
        "no GitHub token for {host}: set {var} (instance-specific), \
         GITHUB_TOKEN (generic), or run `sandogasa-report config` to \
         store one in the overlay"
    ))
}

fn instance_token_env(instance: &str) -> String {
    format!(
        "GITHUB_TOKEN_{}",
        instance_host(instance).to_uppercase().replace('.', "_")
    )
}

/// Strip scheme + trailing slash to get the bare hostname. For
/// `api.github.com` we keep the `api.` prefix — that's the
/// canonical token-keying host so a user with both github.com
/// and a GHES `api.example.com` ends up with two distinct keys.
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

    #[test]
    fn repo_in_org_matches_owner_prefix() {
        assert!(repo_in_org("slopfest/sandogasa", Some("slopfest")));
        assert!(repo_in_org("slopfest", Some("slopfest")));
        assert!(!repo_in_org("slopfest-extras/foo", Some("slopfest")));
        assert!(!repo_in_org("other/foo", Some("slopfest")));
    }

    #[test]
    fn repo_in_org_no_filter_matches_all() {
        assert!(repo_in_org("anything/here", None));
    }

    #[test]
    fn event_in_range_parses_iso8601() {
        let mut e: Event = serde_json::from_str(
            r#"{"id":"1","type":"PushEvent","repo":{"id":1,"name":"o/r"},
                "created_at":"2026-02-15T10:00:00Z"}"#,
        )
        .unwrap();
        let s = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let u = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        assert!(event_in_range(&e, s, u));
        e.created_at = "2026-04-01T00:00:00Z".into();
        assert!(!event_in_range(&e, s, u));
    }

    #[test]
    fn instance_token_env_generates_env_var_name() {
        assert_eq!(
            instance_token_env("https://api.github.com"),
            "GITHUB_TOKEN_API_GITHUB_COM"
        );
        assert_eq!(
            instance_token_env("https://api.example.com/"),
            "GITHUB_TOKEN_API_EXAMPLE_COM"
        );
    }

    #[test]
    fn find_token_errors_when_nothing_set() {
        let tokens = BTreeMap::new();
        let err = find_token("https://nonexistent.example.test", &tokens).unwrap_err();
        assert!(err.contains("no GitHub token"));
    }

    #[test]
    fn find_token_falls_back_to_config() {
        let mut tokens = BTreeMap::new();
        tokens.insert(
            "nonexistent.example.test".to_string(),
            "from-config".to_string(),
        );
        let var = instance_token_env("https://nonexistent.example.test");
        if std::env::var(&var).is_ok() || std::env::var("GITHUB_TOKEN").is_ok() {
            return;
        }
        let tok = find_token("https://nonexistent.example.test", &tokens).unwrap();
        assert_eq!(tok, "from-config");
    }

    #[test]
    fn into_pr_ref_extracts_slug_from_html_url() {
        let pr: PullRequest = serde_json::from_str(
            r#"{"number":42,"title":"Fix it","state":"open",
                "html_url":"https://github.com/slopfest/sandogasa/pull/42"}"#,
        )
        .unwrap();
        let r = into_pr_ref(pr);
        assert_eq!(r.repo, "slopfest/sandogasa");
        assert_eq!(r.number, 42);
        assert_eq!(r.title, "Fix it");
    }

    #[test]
    fn format_empty_report() {
        let report = GithubReport {
            instance: "https://api.github.com".into(),
            user: "octocat".into(),
            ..Default::default()
        };
        let md = format_markdown(&report, 0, None);
        assert!(md.contains("## GitHub\n"));
        assert!(md.contains("No GitHub activity"));
    }

    #[test]
    fn format_non_empty_with_suffix() {
        let mut report = GithubReport {
            instance: "https://api.github.com".into(),
            user: "octocat".into(),
            org: Some("slopfest".into()),
            ..Default::default()
        };
        report.opened_prs.push(PrRef {
            repo: "slopfest/sandogasa".into(),
            number: 42,
            title: "Fix build".into(),
            url: "https://github.com/slopfest/sandogasa/pull/42".into(),
        });
        report
            .commits_authored
            .insert("slopfest/sandogasa".into(), 7);
        let md = format_markdown(&report, 1, Some("upstream"));
        assert!(md.contains("## GitHub (upstream)"));
        assert!(md.contains("**PRs opened:** 1"));
        assert!(md.contains("**Commits authored:** 7 across 1 repo(s)"));
        assert!(md.contains("### Opened"));
        assert!(md.contains("#42"));
        assert!(md.contains("Fix build"));
    }
}
