// SPDX-License-Identifier: Apache-2.0 OR MIT

//! HTTP client for the GitHub REST API.
//!
//! Scoped to the surface `sandogasa-report` needs for activity
//! reports: user identity lookup, token validation, paginated
//! user events (to find which repos a user touched in a window),
//! the Search API for pull requests, and per-repo
//! authored-commit counts.
//!
//! Mirrors `sandogasa-gitlab` in shape so downstream tools can
//! treat the two forges the same way structurally — host-keyed
//! tokens and identities, optional org/group filter, etc.
//!
//! ```no_run
//! use sandogasa_github::Client;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::new("https://api.github.com", "ghp_token")?;
//! let user = client.user_by_username("octocat")?.expect("user exists");
//! let prs = client.search_pull_requests(
//!     &format!("type:pr author:{} created:2026-01-01..2026-03-31", user.login),
//! )?;
//! for pr in prs {
//!     println!("{}: {}", pr.number, pr.title);
//! }
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

/// Default production base URL for the GitHub REST API.
pub const DEFAULT_BASE_URL: &str = "https://api.github.com";

/// Upper bound on any single GitHub HTTP request. GitHub usually
/// responds in well under 5s; this is a hang-catcher rather than
/// a latency cap.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// A GitHub user as returned by `/users/{username}`. Only the
/// fields downstream tools currently consume.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct User {
    pub id: u64,
    pub login: String,
    /// Public display name (`null` when the user hasn't set one).
    #[serde(default)]
    pub name: Option<String>,
    /// Public email (`null` when the user keeps it private).
    #[serde(default)]
    pub email: Option<String>,
}

/// A repository as returned by event payloads and PR responses.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Repository {
    pub id: u64,
    /// `owner/name`, e.g. `slopfest/sandogasa`.
    #[serde(default)]
    pub full_name: Option<String>,
    /// For event payloads where `repo.name` already carries the
    /// `owner/name` form; preserved verbatim.
    #[serde(default)]
    pub name: Option<String>,
    /// Web URL (when present on PR results).
    #[serde(default)]
    pub html_url: Option<String>,
}

impl Repository {
    /// Best-effort `owner/name` extractor. Falls back to `name`
    /// for event payloads where `repo.full_name` is omitted.
    pub fn slug(&self) -> Option<&str> {
        self.full_name.as_deref().or(self.name.as_deref())
    }
}

/// A pull request as returned by the Search Issues endpoint
/// when filtering on `type:pr`. The Search API actually returns
/// "issue" objects with PR-specific fields populated, so we
/// shape this around the union of useful fields rather than the
/// fuller `/repos/{owner}/{repo}/pulls/{number}` model.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    /// Set when the PR has been merged. Some search responses
    /// omit this field for open PRs.
    #[serde(default)]
    pub pull_request: Option<PullRequestRef>,
    pub html_url: String,
    /// The repo this PR lives in — derived from `html_url` since
    /// the search response doesn't include a structured `repo`
    /// object.
    #[serde(default)]
    pub repository_url: Option<String>,
}

impl PullRequest {
    /// Extract the `owner/name` slug from this PR's HTML URL or
    /// `repository_url`.
    pub fn repo_slug(&self) -> Option<String> {
        // html_url shape: `https://github.com/{owner}/{repo}/pull/N`
        if let Some(rest) = self
            .html_url
            .strip_prefix("https://github.com/")
            .or_else(|| self.html_url.strip_prefix("https://"))
        {
            let parts: Vec<&str> = rest.splitn(4, '/').collect();
            if parts.len() >= 2 {
                return Some(format!("{}/{}", parts[0], parts[1]));
            }
        }
        // Fallback: repository_url shape is
        // `https://api.github.com/repos/{owner}/{repo}`.
        if let Some(repo) = &self.repository_url
            && let Some(rest) = repo.split("/repos/").nth(1)
        {
            return Some(rest.to_string());
        }
        None
    }

    /// Whether this PR has been merged. The Search API surfaces
    /// merge state via the optional `pull_request.merged_at`
    /// field; absence means "not merged".
    pub fn merged_at(&self) -> Option<&str> {
        self.pull_request
            .as_ref()
            .and_then(|p| p.merged_at.as_deref())
    }
}

/// Auxiliary block on a search-result issue when it's actually
/// a pull request. The Search Issues endpoint signals "is PR"
/// by populating this; merged state lives here too.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PullRequestRef {
    #[serde(default)]
    pub merged_at: Option<String>,
}

/// Wire response wrapper for the Search Issues endpoint.
#[derive(Debug, Deserialize)]
struct SearchIssuesResponse {
    total_count: u64,
    items: Vec<PullRequest>,
}

/// One entry from `/users/{username}/events`. Fields are
/// sparse; we keep just enough to identify the event type,
/// associated repo, and the actor.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    pub id: String,
    /// `"PushEvent"`, `"PullRequestEvent"`,
    /// `"PullRequestReviewCommentEvent"`, etc.
    #[serde(rename = "type")]
    pub event_type: String,
    pub repo: Repository,
    pub created_at: String,
    /// Free-form payload — varies per event type.
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// One entry from `/repos/{owner}/{repo}/git/refs/tags`. The
/// underlying `object` distinguishes lightweight tags (point
/// directly to a commit) from annotated tags (point to a Tag
/// object that carries tagger info).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitTagRef {
    /// The fully-qualified ref, e.g. `refs/tags/v0.11.0`.
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub object: GitObject,
}

impl GitTagRef {
    /// Strip the `refs/tags/` prefix to get just the tag name.
    pub fn tag_name(&self) -> &str {
        self.ref_name
            .strip_prefix("refs/tags/")
            .unwrap_or(&self.ref_name)
    }
}

/// The thing a Git ref points at. `object_type` is `"commit"`
/// for lightweight tags and `"tag"` for annotated ones.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitObject {
    #[serde(rename = "type")]
    pub object_type: String,
    pub sha: String,
}

/// An annotated-tag object from
/// `/repos/{owner}/{repo}/git/tags/{sha}`. Only annotated tags
/// carry tagger metadata; lightweight tags don't have an
/// addressable tag object.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnnotatedTag {
    pub tag: String,
    pub tagger: Tagger,
}

/// `name` + `email` + `date` triple stamped on annotated-tag
/// creation. `date` is ISO 8601 (e.g. `2026-05-15T17:03:15Z`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Tagger {
    pub name: String,
    pub email: String,
    pub date: String,
}

impl Event {
    /// Helper for callers walking events to find touched repos —
    /// returns the `owner/name` slug if available.
    pub fn repo_slug(&self) -> Option<&str> {
        self.repo.slug()
    }
}

/// Blocking GitHub REST client.
pub struct Client {
    http: reqwest::blocking::Client,
    base_url: String,
}

impl Client {
    /// Build a client for `base_url` (typically
    /// `https://api.github.com`, or a GHES instance's `/api/v3`
    /// endpoint) authenticated with the given personal access
    /// token.
    pub fn new(base_url: &str, token: &str) -> Result<Self, Box<dyn std::error::Error>> {
        sandogasa_cli::ensure_secure_url(base_url)?;
        let http = build_http_client(token)?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

    /// Resolve a username to its full user object. Returns `None`
    /// when the API responds with 404 (no such user); other
    /// errors are returned as `Err`.
    pub fn user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<User>, Box<dyn std::error::Error>> {
        let url = format!("{}/users/{}", self.base_url, username);
        let resp = self.http.get(&url).send()?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitHub GET {url} failed: {status}: {text}").into());
        }
        Ok(Some(resp.json()?))
    }

    /// Run the Search Issues endpoint with the caller-supplied
    /// query string and return every PR across all result pages.
    ///
    /// GitHub caps Search results at 1000 items total; if a
    /// query exceeds that, the caller needs to narrow it (e.g.
    /// by splitting the date window).
    pub fn search_pull_requests(
        &self,
        query: &str,
    ) -> Result<Vec<PullRequest>, Box<dyn std::error::Error>> {
        let mut out: Vec<PullRequest> = Vec::new();
        let mut page = 1u32;
        loop {
            let page_str = page.to_string();
            let url = format!("{}/search/issues", self.base_url);
            let resp = self
                .http
                .get(&url)
                .query(&[("q", query), ("per_page", "100"), ("page", &page_str)])
                .send()?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text()?;
                return Err(format!("GitHub search failed: {status}: {text}").into());
            }
            let batch: SearchIssuesResponse = resp.json()?;
            let n = batch.items.len();
            out.extend(batch.items);
            if n < 100 || out.len() as u64 >= batch.total_count {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Paginate through the user-events endpoint up to GitHub's
    /// 300-event cap. Returns the most recent events first.
    pub fn user_events(&self, username: &str) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        let mut out: Vec<Event> = Vec::new();
        let mut page = 1u32;
        loop {
            let url = format!("{}/users/{}/events", self.base_url, username);
            let page_str = page.to_string();
            let resp = self
                .http
                .get(&url)
                .query(&[("per_page", "100"), ("page", &page_str)])
                .send()?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text()?;
                return Err(format!("GitHub GET {url} failed: {status}: {text}").into());
            }
            let batch: Vec<Event> = resp.json()?;
            let n = batch.len();
            out.extend(batch);
            // GitHub serves at most 300 events / 3 pages of 100.
            if n < 100 || page >= 3 {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Count commits in `owner/repo` authored by `author` within
    /// `[since, until]` (inclusive). Used as a cross-check
    /// against push-event counts — see the GitLab equivalent
    /// for the rationale.
    ///
    /// 404 and 409 responses are treated as "0 commits" rather
    /// than errors. 404 means the repo was deleted or made
    /// private between the events scan and this call; 409 means
    /// the repo exists but is empty. Either way, a single
    /// missing repo shouldn't abort the surrounding report.
    /// The trade-off: real auth failures targeting a single
    /// repo would also be hidden, but GitHub returns 401/403 for
    /// those, not 404/409.
    pub fn count_authored_commits(
        &self,
        owner: &str,
        repo: &str,
        author: &str,
        since: chrono::NaiveDate,
        until: chrono::NaiveDate,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        let url = format!("{}/repos/{}/{}/commits", self.base_url, owner, repo);
        let since_str = format!("{since}T00:00:00Z");
        let until_str = format!("{until}T23:59:59Z");
        let mut total: u64 = 0;
        let mut page = 1u32;
        loop {
            let page_str = page.to_string();
            let query: Vec<(&str, &str)> = vec![
                ("author", author),
                ("since", &since_str),
                ("until", &until_str),
                ("per_page", "100"),
                ("page", &page_str),
            ];
            let resp = self.http.get(&url).query(&query).send()?;
            // GitHub returns 409 Conflict for empty repositories
            // and 404 if the repo was deleted. Treat both as
            // "no commits" rather than hard errors so a single
            // gone repo doesn't abort the whole report.
            if resp.status().as_u16() == 404 || resp.status().as_u16() == 409 {
                break;
            }
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text()?;
                return Err(format!("GitHub GET {url} failed: {status}: {text}").into());
            }
            let batch: Vec<serde_json::Value> = resp.json()?;
            let n = batch.len() as u64;
            total += n;
            if n < 100 {
                break;
            }
            page += 1;
        }
        Ok(total)
    }

    /// List all tag refs for `owner/repo`. Returns an empty list
    /// on 404 (gone repo) and 409 (empty repo) so callers can
    /// iterate over many repos without per-repo error handling.
    pub fn list_tag_refs(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<GitTagRef>, Box<dyn std::error::Error>> {
        let url = format!("{}/repos/{}/{}/git/refs/tags", self.base_url, owner, repo);
        let resp = self.http.get(&url).send()?;
        // 404 = repo gone or no tags ref namespace; 409 = empty repo.
        if resp.status().as_u16() == 404 || resp.status().as_u16() == 409 {
            return Ok(Vec::new());
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitHub GET {url} failed: {status}: {text}").into());
        }
        // GitHub returns an object (not an array) when there's
        // exactly one match. The standard `/git/refs/tags` shape
        // returns an array for the namespace listing; we only see
        // the object form when someone queries a single ref. Be
        // defensive and accept both.
        let body: serde_json::Value = resp.json()?;
        if body.is_array() {
            Ok(serde_json::from_value(body)?)
        } else {
            Ok(vec![serde_json::from_value(body)?])
        }
    }

    /// Fetch one annotated-tag object by SHA. Use only when the
    /// matching `GitTagRef.object.object_type == "tag"` —
    /// lightweight tags (where `object_type == "commit"`) have no
    /// addressable tag object and this endpoint will 404.
    pub fn get_annotated_tag(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
    ) -> Result<AnnotatedTag, Box<dyn std::error::Error>> {
        let url = format!(
            "{}/repos/{}/{}/git/tags/{}",
            self.base_url, owner, repo, sha
        );
        let resp = self.http.get(&url).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitHub GET {url} failed: {status}: {text}").into());
        }
        Ok(resp.json()?)
    }
}

/// Check whether `token` works against `base_url` by hitting
/// `/user`. Returns `Ok(true)` for valid tokens, `Ok(false)`
/// for 401s, and `Err` for other transport / server errors so
/// callers can distinguish "tried and was rejected" from
/// "couldn't reach the server".
pub fn validate_token(base_url: &str, token: &str) -> Result<bool, Box<dyn std::error::Error>> {
    sandogasa_cli::ensure_secure_url(base_url)?;
    let http = build_http_client(token)?;
    let url = format!("{}/user", base_url.trim_end_matches('/'));
    let resp = http.get(&url).send()?;
    let status = resp.status();
    if status.is_success() {
        return Ok(true);
    }
    if status.as_u16() == 401 {
        return Ok(false);
    }
    let text = resp.text().unwrap_or_default();
    Err(format!("GitHub /user check failed: {status}: {text}").into())
}

/// Build a reqwest client preconfigured for the GitHub API: the
/// Bearer token, the recommended JSON Accept header, a User-Agent
/// (GitHub requires one), and our standard request timeout.
fn build_http_client(token: &str) -> Result<reqwest::blocking::Client, Box<dyn std::error::Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    headers.insert(
        HeaderName::from_static("accept"),
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        HeaderName::from_static("x-github-api-version"),
        HeaderValue::from_static("2022-11-28"),
    );
    sandogasa_cli::install_crypto_provider();
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("sandogasa-github/", env!("CARGO_PKG_VERSION")))
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
        assert!(Client::new("http://api.example.com", "tok").is_err());
    }

    #[test]
    fn user_by_username_returns_user() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/users/octocat")
            .match_header("authorization", "Bearer tok")
            .with_status(200)
            .with_body(r#"{"id": 1, "login": "octocat"}"#)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let user = client.user_by_username("octocat").unwrap().unwrap();
        assert_eq!(user.id, 1);
        assert_eq!(user.login, "octocat");
        mock.assert();
    }

    #[test]
    fn user_by_username_404_is_none() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/users/ghost")
            .with_status(404)
            .with_body(r#"{"message": "Not Found"}"#)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        assert!(client.user_by_username("ghost").unwrap().is_none());
        mock.assert();
    }

    #[test]
    fn search_pull_requests_paginates() {
        let mut server = mockito::Server::new();
        // First page: full 100 items, total_count says 105.
        let items_page1 = (1..=100)
            .map(|i| {
                format!(
                    r#"{{"number":{i},"title":"PR {i}","state":"closed","html_url":"https://github.com/o/r/pull/{i}","pull_request":{{"merged_at":"2026-02-01T10:00:00Z"}}}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let mock_p1 = server
            .mock("GET", "/search/issues")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("page".into(), "1".into()),
                mockito::Matcher::UrlEncoded("per_page".into(), "100".into()),
            ]))
            .with_status(200)
            .with_body(format!(
                r#"{{"total_count":105,"incomplete_results":false,"items":[{items_page1}]}}"#
            ))
            .create();
        let mock_p2 = server
            .mock("GET", "/search/issues")
            .match_query(mockito::Matcher::UrlEncoded("page".into(), "2".into()))
            .with_status(200)
            .with_body(
                r#"{"total_count":105,"incomplete_results":false,"items":[
                    {"number":101,"title":"x","state":"open","html_url":"https://github.com/o/r/pull/101"},
                    {"number":102,"title":"y","state":"open","html_url":"https://github.com/o/r/pull/102"},
                    {"number":103,"title":"z","state":"open","html_url":"https://github.com/o/r/pull/103"},
                    {"number":104,"title":"w","state":"open","html_url":"https://github.com/o/r/pull/104"},
                    {"number":105,"title":"v","state":"open","html_url":"https://github.com/o/r/pull/105"}
                ]}"#,
            )
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let prs = client
            .search_pull_requests("type:pr author:octocat")
            .unwrap();
        assert_eq!(prs.len(), 105);
        mock_p1.assert();
        mock_p2.assert();
    }

    #[test]
    fn pull_request_repo_slug_from_html_url() {
        let pr = PullRequest {
            number: 42,
            title: "x".into(),
            state: "open".into(),
            pull_request: None,
            html_url: "https://github.com/slopfest/sandogasa/pull/42".into(),
            repository_url: None,
        };
        assert_eq!(pr.repo_slug().as_deref(), Some("slopfest/sandogasa"));
    }

    #[test]
    fn pull_request_repo_slug_from_repository_url() {
        let pr = PullRequest {
            number: 42,
            title: "x".into(),
            state: "open".into(),
            pull_request: None,
            html_url: "https://github.com/o/r/issues/42".into(),
            repository_url: Some("https://api.github.com/repos/slopfest/sandogasa".into()),
        };
        // html_url is consulted first, but works as a fallback path.
        let pr2 = PullRequest {
            html_url: "garbage".into(),
            ..pr
        };
        assert_eq!(pr2.repo_slug().as_deref(), Some("slopfest/sandogasa"));
    }

    #[test]
    fn pull_request_merged_at_via_helper() {
        let pr: PullRequest = serde_json::from_str(
            r#"{"number":1,"title":"t","state":"closed",
                "html_url":"https://github.com/o/r/pull/1",
                "pull_request":{"merged_at":"2026-02-01T10:00:00Z"}}"#,
        )
        .unwrap();
        assert_eq!(pr.merged_at(), Some("2026-02-01T10:00:00Z"));
    }

    #[test]
    fn user_events_pagination_stops_at_300() {
        let mut server = mockito::Server::new();
        // GitHub serves at most 3 pages of events. Page 1 and 2
        // return full pages, page 3 returns a final 50 to test the
        // short-page break path.
        let make_event = |i: u64| {
            format!(
                r#"{{"id":"{i}","type":"PushEvent","repo":{{"id":1,"name":"o/r"}},"created_at":"2026-02-15T10:00:00Z"}}"#
            )
        };
        let page1: String = (1..=100).map(make_event).collect::<Vec<_>>().join(",");
        let page2: String = (101..=200).map(make_event).collect::<Vec<_>>().join(",");
        let page3: String = (201..=250).map(make_event).collect::<Vec<_>>().join(",");
        let m1 = server
            .mock("GET", "/users/octocat/events")
            .match_query(mockito::Matcher::UrlEncoded("page".into(), "1".into()))
            .with_status(200)
            .with_body(format!("[{page1}]"))
            .create();
        let m2 = server
            .mock("GET", "/users/octocat/events")
            .match_query(mockito::Matcher::UrlEncoded("page".into(), "2".into()))
            .with_status(200)
            .with_body(format!("[{page2}]"))
            .create();
        let m3 = server
            .mock("GET", "/users/octocat/events")
            .match_query(mockito::Matcher::UrlEncoded("page".into(), "3".into()))
            .with_status(200)
            .with_body(format!("[{page3}]"))
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let events = client.user_events("octocat").unwrap();
        assert_eq!(events.len(), 250);
        m1.assert();
        m2.assert();
        m3.assert();
    }

    #[test]
    fn count_authored_commits_paginates_and_handles_409() {
        let mut server = mockito::Server::new();
        let m1 = server
            .mock("GET", "/repos/o/r/commits")
            .match_query(mockito::Matcher::UrlEncoded("page".into(), "1".into()))
            .with_status(200)
            .with_body(format!("[{}]", vec!["{}"; 100].join(",")))
            .create();
        let m2 = server
            .mock("GET", "/repos/o/r/commits")
            .match_query(mockito::Matcher::UrlEncoded("page".into(), "2".into()))
            .with_status(200)
            .with_body("[{},{}]")
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let n = client
            .count_authored_commits(
                "o",
                "r",
                "octocat",
                chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                chrono::NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            )
            .unwrap();
        assert_eq!(n, 102);
        m1.assert();
        m2.assert();
    }

    #[test]
    fn count_authored_commits_empty_repo_returns_zero() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/repos/o/empty/commits")
            .match_query(mockito::Matcher::Any)
            .with_status(409)
            .with_body(r#"{"message":"Git Repository is empty."}"#)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let n = client
            .count_authored_commits(
                "o",
                "empty",
                "x",
                chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                chrono::NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            )
            .unwrap();
        assert_eq!(n, 0);
        mock.assert();
    }

    #[test]
    fn validate_token_distinguishes_invalid_from_error() {
        let mut server = mockito::Server::new();
        // Valid token.
        let ok = server
            .mock("GET", "/user")
            .match_header("authorization", "Bearer good")
            .with_status(200)
            .with_body(r#"{"id": 1, "login": "octocat"}"#)
            .create();
        assert!(validate_token(&server.url(), "good").unwrap());
        ok.assert();
        // Wrong token → 401.
        let bad = server
            .mock("GET", "/user")
            .match_header("authorization", "Bearer bad")
            .with_status(401)
            .with_body(r#"{"message": "Bad credentials"}"#)
            .create();
        assert!(!validate_token(&server.url(), "bad").unwrap());
        bad.assert();
    }

    #[test]
    fn repository_slug_prefers_full_name() {
        let repo = Repository {
            id: 1,
            full_name: Some("o/r".into()),
            name: Some("ignored".into()),
            html_url: None,
        };
        assert_eq!(repo.slug(), Some("o/r"));
        // Event payloads only ship `name`.
        let evt_repo = Repository {
            id: 1,
            full_name: None,
            name: Some("o/r".into()),
            html_url: None,
        };
        assert_eq!(evt_repo.slug(), Some("o/r"));
    }
}
