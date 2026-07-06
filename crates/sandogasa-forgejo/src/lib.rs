// SPDX-License-Identifier: Apache-2.0 OR MIT

//! HTTP client for the Forgejo / Gitea REST API (v1).
//!
//! Forgejo (and the Gitea it forks) expose a GitHub-ish REST API rooted
//! at `<instance>/api/v1`. This crate covers the surface the sandogasa
//! tools need:
//!
//! - `sandogasa-report`'s merged-PR accounting — the pull requests the
//!   authenticated token owner created, across *every* repo they
//!   contribute to (not just their own org), via the global issue/pull
//!   search ([`Client::my_pull_requests`]).
//! - `ebranch`'s releng-ticket filing — [`Client::create_issue`] and
//!   [`Client::search_issues`] (the latter to avoid filing duplicates).
//! - `fesco-chair`'s agenda queries — [`Client::repo_issues`] (a repo's
//!   issues by state and label names), [`Client::issue`], and
//!   [`Client::issue_comments`].
//!
//! The instance host is always passed in full, so it works against any
//! deployment — codeberg.org, a Fedora Forgejo, a self-hosted Gitea.
//! Mirrors `sandogasa-github`/`sandogasa-gitlab` in shape so downstream
//! tools can treat the forges the same way structurally (host-keyed
//! tokens and identities, an optional owner/org filter, etc.).
//!
//! Because the global search filters by `created=true` (the token
//! owner), the client reports the activity of *whoever owns the token* —
//! which is exactly the self-reporting use case. Point it at your own
//! instance token.
//!
//! ```no_run
//! use sandogasa_forgejo::Client;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::new("https://codeberg.org", "token")?;
//! for pr in client.my_pull_requests("closed", None)? {
//!     if pr.is_merged() {
//!         println!("{}#{} {}", pr.repo_slug().unwrap_or(""), pr.number, pr.title);
//!     }
//! }
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

/// Upper bound on any single Forgejo HTTP request — a hang-catcher
/// rather than a latency cap.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Gitea/Forgejo cap the page size on most list endpoints at 50.
const PAGE_LIMIT: u32 = 50;

/// A Forgejo user as returned by `/api/v1/user`. Only the fields
/// downstream tools currently consume.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct User {
    pub id: u64,
    pub login: String,
}

/// The user reference embedded in a pull-request search result.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserRef {
    pub login: String,
}

/// The repository reference embedded in a pull-request search result.
/// In this compact form `owner` is a bare login string (not a full
/// user object).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RepositoryRef {
    #[serde(default)]
    pub full_name: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub owner: String,
}

/// The `pull_request` sub-object on a search result — the bits that
/// distinguish a merged PR from a merely-closed one.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullInfo {
    #[serde(default)]
    pub merged: bool,
    /// RFC 3339 timestamp, e.g. `2026-06-25T11:22:09+02:00`.
    #[serde(default)]
    pub merged_at: Option<String>,
    #[serde(default)]
    pub draft: bool,
}

/// A pull request as returned by the issue/pull search endpoint (an
/// issue object whose `pull_request` field is populated).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub html_url: String,
    /// RFC 3339 timestamp the PR was opened at.
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub user: Option<UserRef>,
    #[serde(default)]
    pub repository: Option<RepositoryRef>,
    #[serde(default)]
    pub pull_request: Option<PullInfo>,
}

impl PullRequest {
    /// Whether the PR was actually merged (not just closed).
    pub fn is_merged(&self) -> bool {
        self.pull_request.as_ref().is_some_and(|p| p.merged)
    }

    /// The merge timestamp (RFC 3339), if merged.
    pub fn merged_at(&self) -> Option<&str> {
        self.pull_request
            .as_ref()
            .and_then(|p| p.merged_at.as_deref())
    }

    /// The `owner/repo` slug the PR belongs to.
    pub fn repo_slug(&self) -> Option<&str> {
        self.repository
            .as_ref()
            .map(|r| r.full_name.as_str())
            .filter(|s| !s.is_empty())
    }

    /// The PR author's login.
    pub fn author(&self) -> Option<&str> {
        self.user.as_ref().map(|u| u.login.as_str())
    }
}

/// An issue — from either the per-repo issue endpoints (create/search)
/// or the global issue search. The richer fields (`created_at`,
/// `closed_at`, `repository`) are populated by the search; the per-repo
/// endpoints leave them at their defaults.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub body: Option<String>,
    /// RFC 3339 timestamp the issue was opened at.
    #[serde(default)]
    pub created_at: Option<String>,
    /// RFC 3339 timestamp the issue was closed at, if closed.
    #[serde(default)]
    pub closed_at: Option<String>,
    #[serde(default)]
    pub repository: Option<RepositoryRef>,
}

impl Issue {
    /// The `owner/repo` slug the issue belongs to (search results only).
    pub fn repo_slug(&self) -> Option<&str> {
        self.repository
            .as_ref()
            .map(|r| r.full_name.as_str())
            .filter(|s| !s.is_empty())
    }

    /// The close timestamp (RFC 3339), if closed.
    pub fn closed_at(&self) -> Option<&str> {
        self.closed_at.as_deref()
    }
}

/// An issue comment (`/repos/{owner}/{repo}/issues/{n}/comments`) —
/// only the fields downstream tools consume.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IssueComment {
    #[serde(default)]
    pub body: String,
    /// Comment author's login.
    #[serde(default)]
    pub user: Option<UserRef>,
    /// RFC 3339 timestamp the comment was posted at.
    #[serde(default)]
    pub created_at: Option<String>,
}

/// A git ref (branch or commit) as embedded in a pull request's
/// `head` / `base`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitRef {
    pub sha: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
}

/// The fuller pull-request object (`/pulls/{n}`) — the head/base refs
/// the issue/pull *search* result omits. Needed to tell whether a
/// closed-but-unmerged PR's commit nonetheless landed on the target
/// branch (applied out-of-band).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullDetail {
    pub number: u64,
    #[serde(default)]
    pub merged: bool,
    pub head: GitRef,
    pub base: GitRef,
}

/// The relevant slice of a `/compare/{base}...{head}` response.
#[derive(Debug, Clone, Deserialize)]
struct CompareResult {
    #[serde(default)]
    total_commits: u64,
}

/// A Forgejo / Gitea API client bound to one instance.
pub struct Client {
    http: reqwest::blocking::Client,
    base_url: String,
}

impl Client {
    /// Build a client for the instance at `base_url` (its root, e.g.
    /// `https://codeberg.org`; `/api/v1` is appended internally),
    /// authenticated with the given access token.
    pub fn new(base_url: &str, token: &str) -> Result<Self, Box<dyn std::error::Error>> {
        sandogasa_cli::ensure_secure_url(base_url)?;
        let http = build_http_client(token)?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// The pull requests the token owner created, paginated across all
    /// repos they can see. `state` is `open` / `closed` / `all`; `owner`
    /// optionally scopes results to a single repo-owner (user or org).
    ///
    /// The caller filters by [`PullRequest::is_merged`] and
    /// [`PullRequest::merged_at`] for a merged-in-window report — the
    /// search itself can't filter on merge time.
    pub fn my_pull_requests(
        &self,
        state: &str,
        owner: Option<&str>,
    ) -> Result<Vec<PullRequest>, Box<dyn std::error::Error>> {
        self.search_created("pulls", state, owner)
    }

    /// The issues the token owner created, paginated across all repos
    /// they can see. `state` is `open` / `closed` / `all`; `owner`
    /// optionally scopes to a single repo-owner. Pull requests are
    /// excluded (`type=issues`).
    ///
    /// The caller filters by `created_at` (opened in window) and, for
    /// `closed` issues, [`Issue::closed_at`] (closed in window).
    pub fn my_issues(
        &self,
        state: &str,
        owner: Option<&str>,
    ) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
        self.search_created("issues", state, owner)
    }

    /// Paginate the global issue/pull search for items the token owner
    /// created (`created=true`). `kind` is `pulls` or `issues`; the
    /// result is deserialized into the caller's chosen type.
    fn search_created<T: serde::de::DeserializeOwned>(
        &self,
        kind: &str,
        state: &str,
        owner: Option<&str>,
    ) -> Result<Vec<T>, Box<dyn std::error::Error>> {
        let url = format!("{}/api/v1/repos/issues/search", self.base_url);
        let limit = PAGE_LIMIT.to_string();
        let mut out: Vec<T> = Vec::new();
        let mut page = 1u32;
        loop {
            let page_str = page.to_string();
            let mut query: Vec<(&str, &str)> = vec![
                ("type", kind),
                ("state", state),
                ("created", "true"),
                ("limit", &limit),
                ("page", &page_str),
            ];
            if let Some(o) = owner {
                query.push(("owner", o));
            }
            let resp = self.http.get(&url).query(&query).send()?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text()?;
                return Err(format!("Forgejo {kind} search failed: {status}: {text}").into());
            }
            let batch: Vec<T> = resp.json()?;
            let n = batch.len() as u32;
            out.extend(batch);
            if n < PAGE_LIMIT {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Fetch the fuller pull-request object for `owner/repo#number` —
    /// in particular its `head` and `base` refs, which the search
    /// result omits.
    pub fn pull_request(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<PullDetail, Box<dyn std::error::Error>> {
        let url = format!(
            "{}/api/v1/repos/{owner}/{repo}/pulls/{number}",
            self.base_url
        );
        let resp = self.http.get(&url).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!(
                "Forgejo get pull {owner}/{repo}#{number} failed: {status}: {text}"
            )
            .into());
        }
        Ok(resp.json()?)
    }

    /// Whether `sha` is already contained in `branch` — i.e. an
    /// ancestor of (or equal to) the branch tip. Asks the compare
    /// endpoint how many commits `branch` is behind `sha`; zero means
    /// `sha` adds nothing, so it's already on the branch. Used to spot
    /// a closed-but-unmerged PR whose commit landed out-of-band (a
    /// maintainer cherry-picked or fast-forwarded it).
    pub fn commit_contained(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        sha: &str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let url = format!(
            "{}/api/v1/repos/{owner}/{repo}/compare/{branch}...{sha}",
            self.base_url
        );
        let resp = self.http.get(&url).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(
                format!("Forgejo compare {branch}...{sha} failed: {status}: {text}").into(),
            );
        }
        let cmp: CompareResult = resp.json()?;
        Ok(cmp.total_commits == 0)
    }

    /// File a new issue on `owner/repo` and return it.
    pub fn create_issue(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        body: &str,
    ) -> Result<Issue, Box<dyn std::error::Error>> {
        let url = format!("{}/api/v1/repos/{owner}/{repo}/issues", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "title": title, "body": body }))
            .send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("Forgejo create issue failed: {status}: {text}").into());
        }
        Ok(resp.json()?)
    }

    /// List issues (not pulls) on `owner/repo`, filtered by `state`
    /// (`open` / `closed` / `all`) and label names — an issue must
    /// carry *every* listed label to match (Forgejo ANDs the
    /// comma-joined `labels` filter). Paginates through all pages.
    pub fn repo_issues(
        &self,
        owner: &str,
        repo: &str,
        state: &str,
        labels: &[&str],
    ) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
        self.repo_items("issues", owner, repo, state, labels)
    }

    /// List pull requests on `owner/repo` by `state`, in the same
    /// issue shape as [`Self::repo_issues`] (the per-repo issues
    /// endpoint with `type=pulls`; issues and PRs share one number
    /// space).
    pub fn repo_pulls(
        &self,
        owner: &str,
        repo: &str,
        state: &str,
    ) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
        self.repo_items("pulls", owner, repo, state, &[])
    }

    /// Paginate `/repos/{owner}/{repo}/issues` for `kind` (`issues` /
    /// `pulls`).
    fn repo_items(
        &self,
        kind: &str,
        owner: &str,
        repo: &str,
        state: &str,
        labels: &[&str],
    ) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
        let url = format!("{}/api/v1/repos/{owner}/{repo}/issues", self.base_url);
        let limit = PAGE_LIMIT.to_string();
        let labels = labels.join(",");
        let mut out: Vec<Issue> = Vec::new();
        let mut page = 1u32;
        loop {
            let page_str = page.to_string();
            let mut query: Vec<(&str, &str)> = vec![
                ("type", kind),
                ("state", state),
                ("limit", &limit),
                ("page", &page_str),
            ];
            if !labels.is_empty() {
                query.push(("labels", &labels));
            }
            let resp = self.http.get(&url).query(&query).send()?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text()?;
                return Err(format!("Forgejo {kind} list failed: {status}: {text}").into());
            }
            let batch: Vec<Issue> = resp.json()?;
            let n = batch.len() as u32;
            out.extend(batch);
            if n < PAGE_LIMIT {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Fetch a single issue by number.
    pub fn issue(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<Issue, Box<dyn std::error::Error>> {
        let url = format!(
            "{}/api/v1/repos/{owner}/{repo}/issues/{number}",
            self.base_url
        );
        let resp = self.http.get(&url).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("Forgejo issue {owner}/{repo}#{number}: {status}: {text}").into());
        }
        Ok(resp.json()?)
    }

    /// Fetch an issue's comments, oldest first, paginated.
    pub fn issue_comments(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<Vec<IssueComment>, Box<dyn std::error::Error>> {
        let url = format!(
            "{}/api/v1/repos/{owner}/{repo}/issues/{number}/comments",
            self.base_url
        );
        let limit = PAGE_LIMIT.to_string();
        let mut out: Vec<IssueComment> = Vec::new();
        let mut page = 1u32;
        loop {
            let page_str = page.to_string();
            let resp = self
                .http
                .get(&url)
                .query(&[("limit", limit.as_str()), ("page", &page_str)])
                .send()?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text()?;
                return Err(format!(
                    "Forgejo comments for {owner}/{repo}#{number}: {status}: {text}"
                )
                .into());
            }
            let batch: Vec<IssueComment> = resp.json()?;
            let n = batch.len() as u32;
            out.extend(batch);
            if n < PAGE_LIMIT {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Search issues (not pulls) on `owner/repo` matching `query`, across
    /// any state — used to spot an already-filed ticket before creating a
    /// duplicate.
    pub fn search_issues(
        &self,
        owner: &str,
        repo: &str,
        query: &str,
    ) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
        let url = format!("{}/api/v1/repos/{owner}/{repo}/issues", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[("type", "issues"), ("state", "all"), ("q", query)])
            .send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("Forgejo issue search failed: {status}: {text}").into());
        }
        Ok(resp.json()?)
    }
}

/// Check whether a token is accepted by the instance, hitting
/// `/api/v1/user`. `Ok(true)` for a valid token, `Ok(false)` for 401,
/// `Err` for anything else (so callers can tell "rejected" from
/// "couldn't reach the server").
pub fn validate_token(base_url: &str, token: &str) -> Result<bool, Box<dyn std::error::Error>> {
    sandogasa_cli::ensure_secure_url(base_url)?;
    let http = build_http_client(token)?;
    let url = format!("{}/api/v1/user", base_url.trim_end_matches('/'));
    let resp = http.get(&url).send()?;
    let status = resp.status();
    if status.is_success() {
        return Ok(true);
    }
    if status.as_u16() == 401 {
        return Ok(false);
    }
    let text = resp.text().unwrap_or_default();
    Err(format!("Forgejo /user check failed: {status}: {text}").into())
}

/// Build a reqwest client preconfigured for the Forgejo API: the
/// `token` auth scheme Gitea/Forgejo expect, a JSON Accept header, a
/// User-Agent, and our standard request timeout.
fn build_http_client(token: &str) -> Result<reqwest::blocking::Client, Box<dyn std::error::Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&format!("token {token}"))?,
    );
    headers.insert(
        HeaderName::from_static("accept"),
        HeaderValue::from_static("application/json"),
    );
    sandogasa_cli::install_crypto_provider();
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("sandogasa-forgejo/", env!("CARGO_PKG_VERSION")))
        .default_headers(headers)
        .timeout(DEFAULT_TIMEOUT)
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_plaintext_remote() {
        // A token must not be sent to a plaintext http non-loopback URL.
        assert!(Client::new("http://forge.example.com", "tok").is_err());
    }

    #[test]
    fn my_pull_requests_filters_to_merged() {
        let mut server = mockito::Server::new();
        let body = r#"[
            {"number": 92, "title": "Closed not merged", "state": "closed",
             "html_url": "https://codeberg.org/o/r/pulls/92",
             "user": {"login": "michelin"},
             "repository": {"full_name": "o/r", "name": "r", "owner": "o"},
             "pull_request": {"merged": false, "merged_at": null, "draft": false}},
            {"number": 35, "title": "Fix select box", "state": "closed",
             "html_url": "https://codeberg.org/o2/r2/pulls/35",
             "user": {"login": "michelin"},
             "repository": {"full_name": "o2/r2", "name": "r2", "owner": "o2"},
             "pull_request": {"merged": true, "merged_at": "2026-06-25T11:22:09+02:00", "draft": false}}
        ]"#;
        let mock = server
            .mock("GET", "/api/v1/repos/issues/search")
            .match_header("authorization", "token tok")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("type".into(), "pulls".into()),
                mockito::Matcher::UrlEncoded("state".into(), "closed".into()),
                mockito::Matcher::UrlEncoded("created".into(), "true".into()),
            ]))
            .with_status(200)
            .with_body(body)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let prs = client.my_pull_requests("closed", None).unwrap();
        mock.assert();
        assert_eq!(prs.len(), 2);
        let merged: Vec<_> = prs.iter().filter(|p| p.is_merged()).collect();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].number, 35);
        assert_eq!(merged[0].repo_slug(), Some("o2/r2"));
        assert_eq!(merged[0].merged_at(), Some("2026-06-25T11:22:09+02:00"));
        assert_eq!(merged[0].author(), Some("michelin"));
    }

    #[test]
    fn my_issues_searches_issues_not_pulls() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/repos/issues/search")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("type".into(), "issues".into()),
                mockito::Matcher::UrlEncoded("created".into(), "true".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"[{"number":91,"title":"build fails","state":"closed",
                     "html_url":"https://codeberg.org/o/r/issues/91",
                     "created_at":"2026-06-19T22:16:28+02:00",
                     "closed_at":"2026-06-24T11:41:33+02:00",
                     "repository":{"full_name":"o/r","name":"r","owner":"o"}}]"#,
            )
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let issues = client.my_issues("all", None).unwrap();
        mock.assert();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].number, 91);
        assert_eq!(issues[0].repo_slug(), Some("o/r"));
        assert_eq!(issues[0].closed_at(), Some("2026-06-24T11:41:33+02:00"));
    }

    #[test]
    fn my_pull_requests_scopes_to_owner() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/repos/issues/search")
            .match_query(mockito::Matcher::UrlEncoded(
                "owner".into(),
                "ptesarik".into(),
            ))
            .with_status(200)
            .with_body("[]")
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let prs = client.my_pull_requests("closed", Some("ptesarik")).unwrap();
        mock.assert();
        assert!(prs.is_empty());
    }

    #[test]
    fn pull_request_returns_head_and_base() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/repos/o/r/pulls/92")
            .with_status(200)
            .with_body(
                r#"{"number":92,"merged":false,
                    "head":{"sha":"8b8aa8b","ref":"fix-branch"},
                    "base":{"sha":"deadbeef","ref":"tip"}}"#,
            )
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let pr = client.pull_request("o", "r", 92).unwrap();
        mock.assert();
        assert_eq!(pr.head.sha, "8b8aa8b");
        assert_eq!(pr.base.ref_name, "tip");
        assert!(!pr.merged);
    }

    #[test]
    fn commit_contained_reads_compare_total() {
        // total_commits == 0 → the sha is already on the branch.
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/repos/o/r/compare/tip...8b8aa8b")
            .with_status(200)
            .with_body(r#"{"total_commits":0}"#)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        assert!(client.commit_contained("o", "r", "tip", "8b8aa8b").unwrap());
        mock.assert();

        // Non-zero → not contained (the branch is ahead of the sha).
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/api/v1/repos/o/r/compare/tip...other")
            .with_status(200)
            .with_body(r#"{"total_commits":3}"#)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        assert!(!client.commit_contained("o", "r", "tip", "other").unwrap());
    }

    #[test]
    fn create_issue_posts_title_and_body() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v1/repos/releng/tickets/issues")
            .match_header("authorization", "token tok")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "title": "Branch epel9 for foo stalled",
                "body": "See rhbz#123",
            })))
            .with_status(201)
            .with_body(
                r#"{"number": 7, "title": "Branch epel9 for foo stalled", "state": "open",
                    "html_url": "https://forge.example/releng/tickets/issues/7"}"#,
            )
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let issue = client
            .create_issue(
                "releng",
                "tickets",
                "Branch epel9 for foo stalled",
                "See rhbz#123",
            )
            .unwrap();
        mock.assert();
        assert_eq!(issue.number, 7);
        assert_eq!(issue.state, "open");
    }

    #[test]
    fn search_issues_returns_matches() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/repos/releng/tickets/issues")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("type".into(), "issues".into()),
                mockito::Matcher::UrlEncoded("state".into(), "all".into()),
                mockito::Matcher::UrlEncoded("q".into(), "rhbz#123".into()),
            ]))
            .with_status(200)
            .with_body(r#"[{"number": 7, "title": "stalled", "state": "open"}]"#)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let issues = client
            .search_issues("releng", "tickets", "rhbz#123")
            .unwrap();
        mock.assert();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].number, 7);
    }

    #[test]
    fn repo_issues_filters_by_state_and_labels() {
        let mut server = mockito::Server::new();
        let body = r#"[
            {"number": 3620, "title": "Council Engineering Rep", "state": "open",
             "html_url": "https://forge.example.org/fesco/tickets/issues/3620"},
            {"number": 3623, "title": "Forgejo distgit migration", "state": "open",
             "html_url": "https://forge.example.org/fesco/tickets/issues/3623"}
        ]"#;
        let mock = server
            .mock("GET", "/api/v1/repos/fesco/tickets/issues")
            .match_header("authorization", "token tok")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("type".into(), "issues".into()),
                mockito::Matcher::UrlEncoded("state".into(), "open".into()),
                mockito::Matcher::UrlEncoded("labels".into(), "meeting".into()),
            ]))
            .with_status(200)
            .with_body(body)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let issues = client
            .repo_issues("fesco", "tickets", "open", &["meeting"])
            .unwrap();
        mock.assert();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].number, 3620);
        assert_eq!(issues[1].title, "Forgejo distgit migration");
    }

    #[test]
    fn repo_issues_omits_empty_labels_filter() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/repos/fesco/tickets/issues")
            // Exact query string — proves no `labels` param is sent
            // when none was requested.
            .match_query(mockito::Matcher::Exact(
                "type=issues&state=all&limit=50&page=1".into(),
            ))
            .with_status(200)
            .with_body("[]")
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let issues = client.repo_issues("fesco", "tickets", "all", &[]).unwrap();
        mock.assert();
        assert!(issues.is_empty());
    }

    #[test]
    fn issue_fetches_by_number_and_surfaces_errors() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/repos/fesco/tickets/issues/3620")
            .match_header("authorization", "token tok")
            .with_status(200)
            .with_body(
                r#"{"number": 3620, "title": "Council Engineering Rep", "state": "open",
                    "html_url": "https://forge.example.org/fesco/tickets/issues/3620"}"#,
            )
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let issue = client.issue("fesco", "tickets", 3620).unwrap();
        mock.assert();
        assert_eq!(issue.number, 3620);

        let mut server = mockito::Server::new();
        server
            .mock("GET", "/api/v1/repos/fesco/tickets/issues/9999")
            .with_status(404)
            .with_body("not found")
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let err = client
            .issue("fesco", "tickets", 9999)
            .unwrap_err()
            .to_string();
        assert!(err.contains("fesco/tickets#9999"), "{err}");
    }

    #[test]
    fn repo_pulls_queries_type_pulls() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/repos/fesco/docs/issues")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("type".into(), "pulls".into()),
                mockito::Matcher::UrlEncoded("state".into(), "open".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"[{"number": 28, "title": "Clarify updates policy", "state": "open",
                     "html_url": "https://forge.example.org/fesco/docs/pulls/28"}]"#,
            )
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let pulls = client.repo_pulls("fesco", "docs", "open").unwrap();
        mock.assert();
        assert_eq!(pulls.len(), 1);
        assert_eq!(pulls[0].number, 28);
    }

    #[test]
    fn issue_comments_fetches_bodies_in_order() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v1/repos/fesco/tickets/issues/3616/comments")
            .match_header("authorization", "token tok")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("limit".into(), "50".into()),
                mockito::Matcher::UrlEncoded("page".into(), "1".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"[
                    {"body": "+1", "user": {"login": "zbyszek"},
                     "created_at": "2026-06-13T13:26:49Z"},
                    {"body": "After a week: APPROVED (+3, 0, 0)\n",
                     "user": {"login": "zbyszek"},
                     "created_at": "2026-06-21T10:18:51Z"}
                ]"#,
            )
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let comments = client.issue_comments("fesco", "tickets", 3616).unwrap();
        mock.assert();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].body, "+1");
        assert!(
            comments[1].body.contains("APPROVED"),
            "{}",
            comments[1].body
        );
    }

    #[test]
    fn validate_token_distinguishes_invalid_from_error() {
        let mut server = mockito::Server::new();
        let ok = server
            .mock("GET", "/api/v1/user")
            .match_header("authorization", "token good")
            .with_status(200)
            .with_body(r#"{"id": 1, "login": "michelin"}"#)
            .create();
        assert!(validate_token(&server.url(), "good").unwrap());
        ok.assert();

        let mut server = mockito::Server::new();
        let bad = server
            .mock("GET", "/api/v1/user")
            .with_status(401)
            .with_body("unauthorized")
            .create();
        assert!(!validate_token(&server.url(), "bad").unwrap());
        bad.assert();
    }
}
