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
use sandogasa_github::{Client, Event, PullRequest, User};
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

    /// Annotated tags the user cut in the window. Derived by
    /// walking each touched repo's tag refs and resolving
    /// annotated-tag objects — `git push --follow-tags` folds
    /// tag creation into the PushEvent so the events stream
    /// alone can't tell us about new tags. Lightweight tags
    /// aren't included since they carry no tagger metadata.
    pub tags_pushed: Vec<TagRef>,
    /// GitHub Releases the user published in the window. Derived
    /// from `ReleaseEvent` with `action == "published"`.
    pub releases_published: Vec<ReleaseRef>,
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

/// A tag the user pushed.
#[derive(Debug, Clone, Serialize)]
pub struct TagRef {
    /// `owner/name` slug.
    pub repo: String,
    /// Tag name (e.g. `v0.11.0`).
    pub tag: String,
    /// Link to the tag tree on github.com.
    pub url: String,
}

/// A GitHub Release the user published.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseRef {
    /// `owner/name` slug.
    pub repo: String,
    /// Tag the release is anchored on.
    pub tag: String,
    /// Optional release title (`release.name`). Empty/missing
    /// becomes `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `release.html_url`.
    pub url: String,
    /// `release.prerelease`.
    pub prerelease: bool,
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

    // Reviewed/commented PRs come from user events, not Search.
    // GitHub Search's `reviewed-by:`/`commenter:` qualifiers match
    // "user has done this at any time" and the temporal filters
    // (`updated:`, `created:`, etc.) apply to the PR itself, not
    // the user's action — so a 2021 PR with any modification
    // in our window (e.g. a comment from someone else) would
    // surface as "reviewed in window". Walking the user's events
    // gives us "the user *actually did* the thing in the window".
    if verbose {
        eprintln!("[github] {base}: fetching user events");
    }
    let events = client
        .user_events(login)
        .map_err(|e| format!("GitHub events on {base}: {e}"))?;
    let (reviewed, commented) = prs_from_events(&events, since, until, cfg.org.as_deref());
    report.reviewed_prs = reviewed;
    report.commented_prs = commented;
    report.releases_published = releases_from_events(&events, since, until, cfg.org.as_deref());
    let touched_repos: BTreeSet<String> = events
        .iter()
        .filter(|e| e.event_type == "PushEvent")
        .filter(|e| event_in_range(e, since, until))
        .filter_map(|e| e.repo_slug().map(String::from))
        .filter(|slug| repo_in_org(slug, cfg.org.as_deref()))
        .collect();

    // Tags can't be derived from the user-events stream: a
    // `git push --follow-tags` folds the tag creation into the
    // existing PushEvent (which lists only the branch ref). Walk
    // the Git Refs API per touched repo instead, resolving
    // annotated tags so we can check the tagger date and identity.
    if verbose {
        eprintln!(
            "[github] {base}: scanning tags across {} touched repo(s)",
            touched_repos.len()
        );
    }
    report.tags_pushed = collect_tags(&client, &touched_repos, &user_obj, since, until, verbose);

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
        && report.tags_pushed.is_empty()
        && report.releases_published.is_empty()
    {
        let mut out = heading;
        out.push_str("No GitHub activity.\n\n");
        return out;
    }

    let total_authored: u64 = report.commits_authored.values().sum();
    let authored_repos = report.commits_authored.len();
    let tag_repos = unique_repos(&report.tags_pushed, |t| &t.repo);
    let release_repos = unique_repos(&report.releases_published, |r| &r.repo);
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
        "- **Commits authored:** {total_authored} across {authored_repos} repo(s)\n",
    ));
    out.push_str(&format!(
        "- **Tags pushed:** {} across {tag_repos} repo(s)\n",
        report.tags_pushed.len(),
    ));
    out.push_str(&format!(
        "- **Releases published:** {} across {release_repos} repo(s)\n\n",
        report.releases_published.len(),
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
    if !report.tags_pushed.is_empty() {
        out.push_str("### Tags pushed\n\n");
        write_tag_list(&mut out, &report.tags_pushed);
    }
    if !report.releases_published.is_empty() {
        out.push_str("### Releases published\n\n");
        write_release_list(&mut out, &report.releases_published);
    }
    out
}

/// Walk the user's events and bucket PRs into `(reviewed,
/// commented)`. Only events whose `created_at` falls inside the
/// reporting window count, regardless of when the PR itself was
/// last touched.
///
/// - `PullRequestReviewEvent` → reviewed.
/// - `PullRequestReviewCommentEvent` → commented (inline diff
///   comment on a PR).
/// - `IssueCommentEvent` → commented, but only when the issue is
///   actually a PR (payload carries `issue.pull_request`).
///
/// Dedup is by `(repo, number)` so multiple events on the same
/// PR collapse to one entry, with the first-seen title winning.
fn prs_from_events(
    events: &[Event],
    since: NaiveDate,
    until: NaiveDate,
    org: Option<&str>,
) -> (Vec<PrRef>, Vec<PrRef>) {
    let mut reviewed: BTreeMap<(String, u64), PrRef> = BTreeMap::new();
    let mut commented: BTreeMap<(String, u64), PrRef> = BTreeMap::new();
    for ev in events {
        if !event_in_range(ev, since, until) {
            continue;
        }
        let Some(repo) = ev.repo_slug() else { continue };
        if !repo_in_org(repo, org) {
            continue;
        }
        let repo = repo.to_string();
        match ev.event_type.as_str() {
            "PullRequestReviewEvent" | "PullRequestReviewCommentEvent" => {
                if let Some(pr) = pr_ref_from_pr_event(&ev.payload, &repo) {
                    let key = (repo.clone(), pr.number);
                    if ev.event_type == "PullRequestReviewEvent" {
                        reviewed.entry(key).or_insert(pr);
                    } else {
                        commented.entry(key).or_insert(pr);
                    }
                }
            }
            "IssueCommentEvent" => {
                // Only count if the issue is actually a PR.
                let is_pr = ev
                    .payload
                    .get("issue")
                    .and_then(|i| i.get("pull_request"))
                    .is_some();
                if !is_pr {
                    continue;
                }
                if let Some(pr) = pr_ref_from_issue_event(&ev.payload, &repo) {
                    let key = (repo.clone(), pr.number);
                    commented.entry(key).or_insert(pr);
                }
            }
            _ => {}
        }
    }
    (
        reviewed.into_values().collect(),
        commented.into_values().collect(),
    )
}

/// Walk the Git Refs API for each repo the user pushed to and
/// collect annotated tags they cut in the window. Lightweight
/// tags are skipped — they carry no tagger info and there's no
/// reliable signal that the user (rather than a co-maintainer)
/// pushed them.
///
/// Match heuristic: an annotated tag belongs to the user when
/// `tagger.date` is inside the window AND the tagger's `name`
/// or `email` matches the user's GitHub profile (case-
/// insensitive). The profile fallback works for users who set a
/// public name even when their commit email differs from the
/// public GitHub email (the common Fedora case where commits
/// are signed with `salimma@fedoraproject.org` but the GitHub
/// account lists a personal address).
fn collect_tags(
    client: &Client,
    repos: &BTreeSet<String>,
    user: &User,
    since: NaiveDate,
    until: NaiveDate,
    verbose: bool,
) -> Vec<TagRef> {
    let mut out: Vec<TagRef> = Vec::new();
    for slug in repos {
        let Some((owner, repo)) = slug.split_once('/') else {
            continue;
        };
        let refs = match client.list_tag_refs(owner, repo) {
            Ok(r) => r,
            Err(e) => {
                if verbose {
                    eprintln!("[github] list_tag_refs failed for {slug}: {e}");
                }
                continue;
            }
        };
        for r in refs {
            if r.object.object_type != "tag" {
                continue;
            }
            let annotated = match client.get_annotated_tag(owner, repo, &r.object.sha) {
                Ok(t) => t,
                Err(e) => {
                    if verbose {
                        eprintln!(
                            "[github] get_annotated_tag failed for {slug}#{}: {e}",
                            r.tag_name()
                        );
                    }
                    continue;
                }
            };
            if !tagger_in_window(&annotated.tagger.date, since, until) {
                continue;
            }
            if !tagger_matches_user(&annotated.tagger, user) {
                continue;
            }
            out.push(TagRef {
                repo: slug.clone(),
                tag: annotated.tag,
                url: format!("https://github.com/{slug}/tree/{}", r.tag_name()),
            });
        }
    }
    out.sort_by(|a, b| a.repo.cmp(&b.repo).then(a.tag.cmp(&b.tag)));
    out
}

fn tagger_in_window(date: &str, since: NaiveDate, until: NaiveDate) -> bool {
    let Some(day) = date.split('T').next() else {
        return false;
    };
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|d| d >= since && d <= until)
        .unwrap_or(false)
}

fn tagger_matches_user(tagger: &sandogasa_github::Tagger, user: &User) -> bool {
    let name_match = user
        .name
        .as_deref()
        .is_some_and(|n| n.eq_ignore_ascii_case(&tagger.name));
    let email_match = user
        .email
        .as_deref()
        .is_some_and(|e| e.eq_ignore_ascii_case(&tagger.email));
    name_match || email_match
}

/// Walk the user's events and collect GitHub Releases they
/// published in the window. Sourced from `ReleaseEvent` with
/// `action == "published"`. Dedup is by `(repo, tag)`.
fn releases_from_events(
    events: &[Event],
    since: NaiveDate,
    until: NaiveDate,
    org: Option<&str>,
) -> Vec<ReleaseRef> {
    let mut releases: BTreeMap<(String, String), ReleaseRef> = BTreeMap::new();
    for ev in events {
        if !event_in_range(ev, since, until) {
            continue;
        }
        if ev.event_type != "ReleaseEvent" {
            continue;
        }
        if ev.payload.get("action").and_then(|v| v.as_str()) != Some("published") {
            continue;
        }
        let Some(release) = ev.payload.get("release") else {
            continue;
        };
        let Some(tag) = release.get("tag_name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(repo) = ev.repo_slug() else { continue };
        if !repo_in_org(repo, org) {
            continue;
        }
        let name = release
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let url = release
            .get("html_url")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("https://github.com/{repo}/releases/tag/{tag}"));
        let prerelease = release
            .get("prerelease")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let key = (repo.to_string(), tag.to_string());
        releases.entry(key).or_insert(ReleaseRef {
            repo: repo.to_string(),
            tag: tag.to_string(),
            name,
            url,
            prerelease,
        });
    }
    releases.into_values().collect()
}

fn pr_ref_from_pr_event(payload: &serde_json::Value, repo: &str) -> Option<PrRef> {
    let pr = payload.get("pull_request")?;
    let number = pr.get("number").and_then(|n| n.as_u64())?;
    let title = pr
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let url = pr
        .get("html_url")
        .and_then(|u| u.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("https://github.com/{repo}/pull/{number}"));
    Some(PrRef {
        repo: repo.to_string(),
        number,
        title,
        url,
    })
}

fn pr_ref_from_issue_event(payload: &serde_json::Value, repo: &str) -> Option<PrRef> {
    let issue = payload.get("issue")?;
    let number = issue.get("number").and_then(|n| n.as_u64())?;
    let title = issue
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let url = issue
        .get("pull_request")
        .and_then(|p| p.get("html_url"))
        .and_then(|u| u.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("https://github.com/{repo}/pull/{number}"));
    Some(PrRef {
        repo: repo.to_string(),
        number,
        title,
        url,
    })
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

fn write_tag_list(out: &mut String, tags: &[TagRef]) {
    let mut by_repo: BTreeMap<&str, Vec<&TagRef>> = BTreeMap::new();
    for t in tags {
        by_repo.entry(t.repo.as_str()).or_default().push(t);
    }
    for (repo, entries) in by_repo {
        let formatted: Vec<String> = entries
            .iter()
            .map(|t| format!("[{}]({})", t.tag, t.url))
            .collect();
        out.push_str(&format!("- `{repo}`: {}\n", formatted.join(", ")));
    }
    out.push('\n');
}

fn write_release_list(out: &mut String, releases: &[ReleaseRef]) {
    for r in releases {
        let prerelease = if r.prerelease { " (prerelease)" } else { "" };
        let suffix = match &r.name {
            Some(name) if name != &r.tag => format!(" — {name}"),
            _ => String::new(),
        };
        out.push_str(&format!(
            "- [{} {}]({}){suffix}{prerelease}\n",
            r.repo, r.tag, r.url,
        ));
    }
    out.push('\n');
}

fn unique_repos<T>(items: &[T], repo_of: impl Fn(&T) -> &String) -> usize {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for it in items {
        seen.insert(repo_of(it).as_str());
    }
    seen.len()
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

    fn make_event(
        event_type: &str,
        repo: &str,
        created_at: &str,
        payload: serde_json::Value,
    ) -> Event {
        serde_json::from_value(serde_json::json!({
            "id": "1",
            "type": event_type,
            "repo": {"id": 1, "name": repo},
            "created_at": created_at,
            "payload": payload,
        }))
        .unwrap()
    }

    #[test]
    fn prs_from_events_review_in_window() {
        let events = vec![make_event(
            "PullRequestReviewEvent",
            "slopfest/sandogasa",
            "2026-02-15T10:00:00Z",
            serde_json::json!({
                "pull_request": {
                    "number": 42,
                    "title": "Add foo",
                    "html_url": "https://github.com/slopfest/sandogasa/pull/42"
                }
            }),
        )];
        let (reviewed, commented) = prs_from_events(
            &events,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            None,
        );
        assert_eq!(reviewed.len(), 1);
        assert_eq!(reviewed[0].number, 42);
        assert!(commented.is_empty());
    }

    #[test]
    fn prs_from_events_skips_old_pr_with_only_other_user_activity() {
        // The original bug: a 2021 PR the user reviewed long ago
        // shows up in Search because someone else's comment in
        // the window bumps the PR's `updated:` timestamp. With
        // events, only the user's own actions count — an event
        // from 2021 falls outside the window and gets dropped.
        let events = vec![make_event(
            "PullRequestReviewEvent",
            "starship/starship",
            "2021-08-15T10:00:00Z",
            serde_json::json!({
                "pull_request": {"number": 2612, "title": "old", "html_url": "x"}
            }),
        )];
        let (reviewed, commented) = prs_from_events(
            &events,
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
            None,
        );
        assert!(reviewed.is_empty());
        assert!(commented.is_empty());
    }

    #[test]
    fn prs_from_events_issue_comment_only_if_pr() {
        // IssueCommentEvent fires for both issues and PRs; only
        // the PR variant should count as a "PR commented on".
        let on_issue = make_event(
            "IssueCommentEvent",
            "slopfest/sandogasa",
            "2026-02-15T10:00:00Z",
            serde_json::json!({
                "issue": {"number": 1, "title": "issue-only"}
                // No `pull_request` field → it's an issue.
            }),
        );
        let on_pr = make_event(
            "IssueCommentEvent",
            "slopfest/sandogasa",
            "2026-02-15T10:00:00Z",
            serde_json::json!({
                "issue": {
                    "number": 5,
                    "title": "PR thread",
                    "pull_request": {"html_url": "https://github.com/slopfest/sandogasa/pull/5"}
                }
            }),
        );
        let (_, commented) = prs_from_events(
            &[on_issue, on_pr],
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            None,
        );
        assert_eq!(commented.len(), 1);
        assert_eq!(commented[0].number, 5);
    }

    #[test]
    fn prs_from_events_dedups_multiple_events_on_same_pr() {
        // Two comments + one review on the same PR collapse to
        // one entry in each bucket.
        let events = vec![
            make_event(
                "PullRequestReviewEvent",
                "o/r",
                "2026-02-10T10:00:00Z",
                serde_json::json!({"pull_request": {"number": 7, "title": "x", "html_url": "https://github.com/o/r/pull/7"}}),
            ),
            make_event(
                "PullRequestReviewCommentEvent",
                "o/r",
                "2026-02-11T10:00:00Z",
                serde_json::json!({"pull_request": {"number": 7, "title": "x", "html_url": "https://github.com/o/r/pull/7"}}),
            ),
            make_event(
                "PullRequestReviewCommentEvent",
                "o/r",
                "2026-02-12T10:00:00Z",
                serde_json::json!({"pull_request": {"number": 7, "title": "x", "html_url": "https://github.com/o/r/pull/7"}}),
            ),
        ];
        let (reviewed, commented) = prs_from_events(
            &events,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            None,
        );
        assert_eq!(reviewed.len(), 1);
        assert_eq!(commented.len(), 1);
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

    fn make_tagger(name: &str, email: &str, date: &str) -> sandogasa_github::Tagger {
        sandogasa_github::Tagger {
            name: name.into(),
            email: email.into(),
            date: date.into(),
        }
    }

    fn make_user(login: &str, name: Option<&str>, email: Option<&str>) -> User {
        User {
            id: 1,
            login: login.into(),
            name: name.map(String::from),
            email: email.map(String::from),
        }
    }

    #[test]
    fn tagger_matches_user_by_name_or_email() {
        let user = make_user(
            "michel-slm",
            Some("Michel Lind"),
            Some("michel@michel-slm.name"),
        );
        // Different email (commit identity) — name match wins.
        assert!(tagger_matches_user(
            &make_tagger(
                "Michel Lind",
                "salimma@fedoraproject.org",
                "2026-05-15T17:03:15Z"
            ),
            &user
        ));
        // Different name (display name change) — email match wins.
        assert!(tagger_matches_user(
            &make_tagger(
                "Someone Else",
                "michel@michel-slm.name",
                "2026-05-15T17:03:15Z"
            ),
            &user
        ));
        // Neither — reject.
        assert!(!tagger_matches_user(
            &make_tagger("Other", "other@example.com", "2026-05-15T17:03:15Z"),
            &user
        ));
    }

    #[test]
    fn tagger_matches_is_case_insensitive() {
        let user = make_user("u", Some("Michel Lind"), Some("M@example.com"));
        assert!(tagger_matches_user(
            &make_tagger("MICHEL LIND", "other@example.com", "2026-05-15T00:00:00Z"),
            &user
        ));
        assert!(tagger_matches_user(
            &make_tagger("other", "m@example.com", "2026-05-15T00:00:00Z"),
            &user
        ));
    }

    #[test]
    fn tagger_in_window_parses_iso8601() {
        let s = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let u = NaiveDate::from_ymd_opt(2026, 5, 31).unwrap();
        assert!(tagger_in_window("2026-05-15T17:03:15Z", s, u));
        assert!(!tagger_in_window("2026-06-01T00:00:00Z", s, u));
        assert!(!tagger_in_window("garbage", s, u));
    }

    #[test]
    fn releases_from_events_extracts_published() {
        let events = vec![
            make_event(
                "ReleaseEvent",
                "slopfest/sandogasa",
                "2026-05-15T10:00:00Z",
                serde_json::json!({
                    "action": "published",
                    "release": {
                        "tag_name": "v0.11.0",
                        "name": "Release v0.11.0",
                        "html_url": "https://github.com/slopfest/sandogasa/releases/tag/v0.11.0",
                        "prerelease": false
                    }
                }),
            ),
            // edited action — skip.
            make_event(
                "ReleaseEvent",
                "slopfest/sandogasa",
                "2026-05-15T11:00:00Z",
                serde_json::json!({
                    "action": "edited",
                    "release": {"tag_name": "v0.10.0", "name": "old", "html_url": "x"}
                }),
            ),
        ];
        let releases = releases_from_events(
            &events,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            None,
        );
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].tag, "v0.11.0");
        assert_eq!(releases[0].name.as_deref(), Some("Release v0.11.0"));
        assert!(!releases[0].prerelease);
    }

    #[test]
    fn releases_from_events_handles_missing_name_and_prerelease() {
        let events = vec![make_event(
            "ReleaseEvent",
            "o/r",
            "2026-05-15T10:00:00Z",
            serde_json::json!({
                "action": "published",
                "release": {
                    "tag_name": "v0.0.1-rc1",
                    "html_url": "https://github.com/o/r/releases/tag/v0.0.1-rc1",
                    "prerelease": true
                }
            }),
        )];
        let releases = releases_from_events(
            &events,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            None,
        );
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].name, None);
        assert!(releases[0].prerelease);
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

    #[test]
    fn format_renders_tags_and_releases() {
        let mut report = GithubReport {
            instance: "https://api.github.com".into(),
            user: "octocat".into(),
            ..Default::default()
        };
        report.tags_pushed.push(TagRef {
            repo: "slopfest/sandogasa".into(),
            tag: "v0.11.0".into(),
            url: "https://github.com/slopfest/sandogasa/tree/v0.11.0".into(),
        });
        report.tags_pushed.push(TagRef {
            repo: "slopfest/sandogasa".into(),
            tag: "v0.10.2".into(),
            url: "https://github.com/slopfest/sandogasa/tree/v0.10.2".into(),
        });
        report.releases_published.push(ReleaseRef {
            repo: "slopfest/sandogasa".into(),
            tag: "v0.11.0".into(),
            name: Some("Release v0.11.0".into()),
            url: "https://github.com/slopfest/sandogasa/releases/tag/v0.11.0".into(),
            prerelease: false,
        });
        let md = format_markdown(&report, 1, None);
        assert!(md.contains("**Tags pushed:** 2 across 1 repo(s)"));
        assert!(md.contains("**Releases published:** 1 across 1 repo(s)"));
        assert!(md.contains("### Tags pushed"));
        assert!(md.contains("v0.11.0"));
        assert!(md.contains("v0.10.2"));
        assert!(md.contains("### Releases published"));
        assert!(md.contains("Release v0.11.0"));
    }
}
