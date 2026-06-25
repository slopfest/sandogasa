// SPDX-License-Identifier: Apache-2.0 OR MIT

//! HTTP client for the Forgejo / Gitea REST API (v1).
//!
//! Forgejo (and the Gitea it forks) expose a GitHub-ish REST API rooted
//! at `<instance>/api/v1`. This crate covers the surface two sandogasa
//! tools need:
//!
//! - `sandogasa-report`'s merged-PR accounting — the pull requests the
//!   authenticated token owner created, across *every* repo they
//!   contribute to (not just their own org), via the global issue/pull
//!   search ([`Client::my_pull_requests`]).
//! - `ebranch`'s releng-ticket filing — [`Client::create_issue`] and
//!   [`Client::search_issues`] (the latter to avoid filing duplicates).
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

/// An issue as returned by the per-repo issue endpoints — the fields
/// the releng-ticket workflow needs.
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
        let url = format!("{}/api/v1/repos/issues/search", self.base_url);
        let limit = PAGE_LIMIT.to_string();
        let mut out: Vec<PullRequest> = Vec::new();
        let mut page = 1u32;
        loop {
            let page_str = page.to_string();
            let mut query: Vec<(&str, &str)> = vec![
                ("type", "pulls"),
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
                return Err(format!("Forgejo pull search failed: {status}: {text}").into());
            }
            let batch: Vec<PullRequest> = resp.json()?;
            let n = batch.len() as u32;
            out.extend(batch);
            if n < PAGE_LIMIT {
                break;
            }
            page += 1;
        }
        Ok(out)
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
