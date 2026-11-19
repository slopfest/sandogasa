// SPDX-License-Identifier: Apache-2.0 OR MIT

//! HTTP client for the GitHub REST API.
//!
//! Scoped to the surface `sandogasa-report` needs for activity
//! reports: user identity lookup, token validation, paginated
//! user events (to find which repos a user touched in a window),
//! the Search API for pull requests, per-repo authored-commit
//! counts, and through GraphQL, a user's contributions over a period
//! and their comments on issues and pull requests.
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

use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use sandogasa_cli::http;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::json;

/// A GitHub user as returned by `/users/{username}`. Only the
/// fields downstream tools currently consume.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
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
#[non_exhaustive]
pub struct Repository {
    pub id: u64,
    /// `owner/name`, e.g. `slopfest/sandogasa`.
    #[serde(default)]
    pub full_name: Option<String>,
    /// For event payloads where `repo.name` already carries the
    /// `owner/name` form; preserved verbatim.
    #[serde(default)]
    pub name: Option<String>,
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
#[non_exhaustive]
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
#[non_exhaustive]
pub struct PullRequestRef {
    #[serde(default)]
    pub merged_at: Option<String>,
}

/// The fuller pull-request object from `/repos/{owner}/{repo}/pulls/{n}`:
/// the head and base refs the search result omits, and whether it merged.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct PullRequestDetail {
    pub number: u64,
    pub state: String,
    #[serde(default)]
    pub merged: bool,
    pub head: GitRef,
    pub base: GitRef,
    /// When the PR was opened (RFC 3339).
    #[serde(default)]
    pub created_at: Option<String>,
    /// Who opened it.
    #[serde(default)]
    pub user: Option<Login>,
}

/// An account named only by its login.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct Login {
    pub login: String,
}

/// A commit from `/pulls/{n}/commits` or `/commits`: the sha, the
/// message and the author of the commit itself (not the GitHub account).
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct RepoCommit {
    pub sha: String,
    pub commit: CommitBody,
}

/// The git-level part of a commit.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct CommitBody {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub author: Option<CommitAuthor>,
}

/// A commit's author as git recorded it.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct CommitAuthor {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
}

impl RepoCommit {
    /// The message's first line.
    pub fn title(&self) -> &str {
        self.commit.message.lines().next().unwrap_or("")
    }
}

/// One side of a pull request: the branch name and the commit it points at.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct GitRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

/// What `/compare/{base}...{head}` answers: `ahead_by` counts the commits
/// on head that base lacks, so zero means base already has them all.
#[derive(Debug, Deserialize)]
struct CompareResult {
    #[serde(default)]
    ahead_by: u64,
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
#[non_exhaustive]
pub struct Event {
    pub id: String,
    /// `"PushEvent"`, `"PullRequestEvent"`,
    /// `"PullRequestReviewCommentEvent"`, etc.
    #[serde(rename = "type")]
    pub event_type: String,
    pub repo: Repository,
    pub created_at: String,
    /// Whether the event happened in a public repository. GitHub sets
    /// it on every event; a missing value reads as private.
    #[serde(default)]
    pub public: bool,
    /// Free-form payload — varies per event type.
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// One entry from `/repos/{owner}/{repo}/git/refs/tags`. The
/// underlying `object` distinguishes lightweight tags (point
/// directly to a commit) from annotated tags (point to a Tag
/// object that carries tagger info).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
pub struct AnnotatedTag {
    pub tag: String,
    pub tagger: Tagger,
}

/// `name` + `email` + `date` triple stamped on annotated-tag
/// creation. `date` is ISO 8601 (e.g. `2026-05-15T17:03:15Z`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
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

/// A repository as GraphQL names it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct RepoNode {
    /// `owner/name`.
    #[serde(rename = "nameWithOwner")]
    pub name_with_owner: String,
    #[serde(rename = "isPrivate")]
    pub is_private: bool,
}

/// An issue or pull request as GraphQL gives it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct ItemNode {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// `OPEN` or `CLOSED`, and for a pull request `MERGED`.
    pub state: String,
    pub repository: RepoNode,
}

/// A comment of the user's on an issue or a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct IssueComment {
    /// When it was written, RFC 3339 (`2026-11-18T15:11:37Z`).
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub url: String,
    /// The issue, or the pull request taken as one.
    pub issue: Option<ItemNode>,
    /// The pull request, when the comment is on one.
    #[serde(rename = "pullRequest")]
    pub pull_request: Option<ItemNode>,
}

/// What a user contributed over a period, as GitHub's contribution
/// graph counts it: commits on a repository's default branch (or
/// `gh-pages`), and pull requests reviewed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Contributions {
    /// Per repository (`owner/name`), the commits made.
    pub commits: BTreeMap<String, u64>,
    /// The repositories among them, and among `reviewed`'s, that are
    /// private.
    pub private_repos: BTreeSet<String>,
    /// The pull requests reviewed, each once.
    pub reviewed: Vec<ItemNode>,
    /// Reviews left out because the token cannot read them, and
    /// GitHub's reasons.
    pub unreadable: Unreadable,
    /// Whether a single day named the most repositories GitHub gives
    /// (100), so some may be missing.
    pub capped: bool,
    /// Contributions GitHub counts in private repositories without
    /// naming them. An organization that refuses the token's kind
    /// (classic personal access tokens, for some) leaves its
    /// contributions out altogether, uncounted.
    pub restricted: u64,
}

/// A user's comments on issues and pull requests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Comments {
    pub comments: Vec<IssueComment>,
    /// Comments left out because the token cannot read them, and
    /// GitHub's reasons.
    pub unreadable: Unreadable,
}

/// Repositories' recent annotated tags, and the queries that failed.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RecentTags {
    /// Per repository (`owner/name`), its annotated tags.
    pub tags: BTreeMap<String, Vec<AnnotatedTag>>,
    /// Per failed query, the repositories it asked about and why.
    pub failures: Vec<String>,
}

/// Commits per repository, and the queries that failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CommitCounts {
    /// Per repository (`owner/name`) with any, the commits.
    pub counts: BTreeMap<String, u64>,
    /// Per failed query, the repositories it asked about and why.
    pub failures: Vec<String>,
}

/// Items GitHub withheld from an answer, such as those in an
/// organization that refuses classic personal access tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Unreadable {
    pub count: usize,
    /// GitHub's messages, each once.
    pub reasons: BTreeSet<String>,
}

impl Unreadable {
    fn add(&mut self, count: usize, errors: Option<Vec<GraphqlError>>) {
        self.count += count;
        if count > 0 {
            self.reasons
                .extend(errors.unwrap_or_default().into_iter().map(|e| e.message));
        }
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
        Ok(Some(http::blocking_json_ok(
            resp,
            &format!("GitHub GET {url}"),
        )?))
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
            let batch: SearchIssuesResponse = http::blocking_json_ok(resp, "GitHub search")?;
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
            let batch: Vec<Event> = http::blocking_json_ok(resp, &format!("GitHub GET {url}"))?;
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
            let batch: Vec<serde_json::Value> =
                http::blocking_json_ok(resp, &format!("GitHub GET {url}"))?;
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
        let resp = http::blocking_ok(resp, &format!("GitHub GET {url}"))?;
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
        Ok(http::blocking_json_ok(resp, &format!("GitHub GET {url}"))?)
    }

    /// Fetch the fuller pull-request object for `owner/repo#number` — in
    /// particular its `head` and `base` refs, which the search result
    /// omits.
    pub fn pull_request(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<PullRequestDetail, Box<dyn std::error::Error>> {
        let url = format!("{}/repos/{owner}/{repo}/pulls/{number}", self.base_url);
        let resp = self.http.get(&url).send()?;
        Ok(http::blocking_json_ok(resp, &format!("GitHub GET {url}"))?)
    }

    /// Whether `owner/repo` is public now: `Some(false)` for a private
    /// repository, `None` when it is gone or the token cannot see it.
    pub fn repository_public(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<Option<bool>, Box<dyn std::error::Error>> {
        #[derive(Deserialize)]
        struct Visibility {
            private: bool,
        }
        let url = format!("{}/repos/{owner}/{repo}", self.base_url);
        let resp = self.http.get(&url).send()?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let v: Visibility = http::blocking_json_ok(resp, &format!("GitHub GET {url}"))?;
        Ok(Some(!v.private))
    }

    /// The commits a pull request carries, oldest first (one page; a
    /// PR of more than a hundred commits is not what this is for).
    pub fn pull_request_commits(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<Vec<RepoCommit>, Box<dyn std::error::Error>> {
        let url = format!(
            "{}/repos/{owner}/{repo}/pulls/{number}/commits",
            self.base_url
        );
        let resp = self.http.get(&url).query(&[("per_page", "100")]).send()?;
        Ok(http::blocking_json_ok(resp, &format!("GitHub GET {url}"))?)
    }

    /// Commits on `branch` by `author` (a login or an email) since an
    /// RFC 3339 instant, newest first; one page of up to a hundred,
    /// which is plenty for one person's commits on one branch since a
    /// PR was opened.
    pub fn branch_commits_by(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        author: &str,
        since: &str,
    ) -> Result<Vec<RepoCommit>, Box<dyn std::error::Error>> {
        let url = format!("{}/repos/{owner}/{repo}/commits", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[
                ("sha", branch),
                ("author", author),
                ("since", since),
                ("per_page", "100"),
            ])
            .send()?;
        Ok(http::blocking_json_ok(resp, &format!("GitHub GET {url}"))?)
    }

    /// Whether `sha` is already on `branch`: GitHub's compare of the
    /// branch to the sha reports how many commits the sha is ahead by,
    /// and zero means the branch has every one of them.
    pub fn commit_contained(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        sha: &str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let url = format!(
            "{}/repos/{owner}/{repo}/compare/{branch}...{sha}",
            self.base_url
        );
        let resp = self.http.get(&url).send()?;
        let cmp: CompareResult =
            http::blocking_json_ok(resp, &format!("GitHub compare {branch}...{sha}"))?;
        Ok(cmp.ahead_by == 0)
    }

    /// What `login` contributed from `from` to `to` (both included):
    /// the repositories committed to with a count each, and the pull
    /// requests reviewed. GitHub answers for at most a year at a time,
    /// so a longer period is asked a year at a time; reviews come 100
    /// to a request. Private repositories are included where the token
    /// can read them.
    pub fn contributions(
        &self,
        login: &str,
        from: chrono::NaiveDate,
        to: chrono::NaiveDate,
    ) -> Result<Contributions, Box<dyn std::error::Error>> {
        let mut out = Contributions::default();
        // Windows of a year at most, newest asked last; one that names
        // the most repositories GitHub gives (100) is split in two, so
        // none goes missing.
        let mut windows = Vec::new();
        let mut start = from;
        while start <= to {
            let end = (start + chrono::Duration::days(364)).min(to);
            windows.push((start, end));
            start = end + chrono::Duration::days(1);
        }
        windows.reverse();
        'windows: while let Some((start, end)) = windows.pop() {
            let mut after: Option<String> = None;
            let mut first = true;
            loop {
                let answer: GraphqlResponse<ContributionsData> = self.graphql_answer(
                    CONTRIBUTIONS_QUERY,
                    &json!({
                        "login": login,
                        "from": format!("{start}T00:00:00Z"),
                        "to": format!("{end}T23:59:59Z"),
                        "after": after,
                    }),
                )?;
                let data = answer
                    .data
                    .ok_or_else(|| graphql_failure(answer.errors.clone()))?;
                let collection = data
                    .user
                    .ok_or_else(|| format!("no GitHub user {login}"))?
                    .contributions_collection;
                if first {
                    let named = collection.commit_contributions_by_repository.len();
                    if named >= MAX_REPOSITORIES {
                        if start < end {
                            let half = start + (end - start) / 2;
                            windows.push((half + chrono::Duration::days(1), end));
                            windows.push((start, half));
                            continue 'windows;
                        }
                        out.capped = true;
                    }
                    out.restricted += collection.restricted_contributions_count;
                    for by_repo in collection.commit_contributions_by_repository {
                        let repo = by_repo.repository;
                        if repo.is_private {
                            out.private_repos.insert(repo.name_with_owner.clone());
                        }
                        *out.commits.entry(repo.name_with_owner).or_default() +=
                            by_repo.contributions.total_count;
                    }
                    first = false;
                }
                let reviews = collection.pull_request_review_contributions;
                let withheld = reviews
                    .nodes
                    .iter()
                    .filter(|n| n.as_ref().and_then(|n| n.pull_request.as_ref()).is_none())
                    .count();
                out.unreadable.add(withheld, answer.errors);
                for pr in reviews
                    .nodes
                    .into_iter()
                    .flatten()
                    .filter_map(|n| n.pull_request)
                {
                    if pr.repository.is_private {
                        out.private_repos
                            .insert(pr.repository.name_with_owner.clone());
                    }
                    if !out.reviewed.iter().any(|r| r.url == pr.url) {
                        out.reviewed.push(pr);
                    }
                }
                match reviews.page_info.next() {
                    Some(cursor) => after = Some(cursor),
                    None => break,
                }
            }
        }
        Ok(out)
    }

    /// `login`'s comments on issues and pull requests edited on or
    /// after `since`, newest edit first, read 100 to a request until
    /// they are older. A comment written in a period is at least as new,
    /// so the caller can filter on `created_at`.
    pub fn issue_comments(
        &self,
        login: &str,
        since: chrono::NaiveDate,
    ) -> Result<Comments, Box<dyn std::error::Error>> {
        let since = format!("{since}T00:00:00Z");
        let mut out = Comments::default();
        let mut after: Option<String> = None;
        loop {
            let answer: GraphqlResponse<IssueCommentsData> = self.graphql_answer(
                ISSUE_COMMENTS_QUERY,
                &json!({"login": login, "after": after}),
            )?;
            let data = answer
                .data
                .ok_or_else(|| graphql_failure(answer.errors.clone()))?;
            let page = data
                .user
                .ok_or_else(|| format!("no GitHub user {login}"))?
                .issue_comments;
            let mut older = false;
            let mut withheld = 0;
            for node in page.nodes {
                let Some(comment) = node else {
                    withheld += 1;
                    continue;
                };
                // RFC 3339 in UTC compares as text.
                if comment.updated_at < since {
                    older = true;
                    break;
                }
                out.comments.push(comment);
            }
            // Only those before the first older comment: the rest are
            // older too, as far as anyone can tell.
            out.unreadable.add(withheld, answer.errors);
            match page.page_info.next() {
                Some(cursor) if !older => after = Some(cursor),
                _ => break,
            }
        }
        Ok(out)
    }

    /// The login of the token's owner (`/user`).
    pub fn token_login(&self) -> Result<String, Box<dyn std::error::Error>> {
        let url = format!("{}/user", self.base_url);
        let resp = self.http.get(&url).send()?;
        let user: User = http::blocking_json_ok(resp, &format!("GitHub GET {url}"))?;
        Ok(user.login)
    }

    /// The private repositories the token can read (its owner's, its
    /// collaborations and its organizations') pushed to on or after
    /// `since`, as `owner/name`, newest push first. GitHub's
    /// contributions count private repositories' commits only as a
    /// number, without the repositories, so a report finds them here.
    pub fn private_repos_pushed_since(
        &self,
        since: chrono::NaiveDate,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let since = format!("{since}T00:00:00Z");
        let url = format!("{}/user/repos", self.base_url);
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let page_str = page.to_string();
            let resp = self
                .http
                .get(&url)
                .query(&[
                    ("visibility", "private"),
                    ("affiliation", "owner,collaborator,organization_member"),
                    ("sort", "pushed"),
                    ("direction", "desc"),
                    ("per_page", "100"),
                    ("page", &page_str),
                ])
                .send()?;
            let batch: Vec<PushedRepo> =
                http::blocking_json_ok(resp, &format!("GitHub GET {url}"))?;
            let n = batch.len();
            let mut older = false;
            for repo in batch {
                // Never pushed to: nothing to count, wherever GitHub
                // sorts it.
                let Some(pushed) = repo.pushed_at.as_deref() else {
                    continue;
                };
                // RFC 3339 in UTC compares as text.
                if pushed < since.as_str() {
                    older = true;
                    break;
                }
                out.push(repo.full_name);
            }
            if older || n < 100 {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Each of `repos`' (`owner/name`) annotated tags among its 50
    /// newest, by the date of the commit tagged, with who tagged it and
    /// when (in UTC). Ten repositories go to a query. A repository the
    /// token cannot read is left out, and a query that fails is
    /// recorded in [`RecentTags::failures`] while the others count; a
    /// tag made for an old commit can fall outside the 50.
    pub fn recent_annotated_tags(&self, repos: &[String]) -> RecentTags {
        let mut out = RecentTags::default();
        for chunk in repos.chunks(10) {
            let mut params = Vec::new();
            let mut fields = Vec::new();
            let mut variables = serde_json::Map::new();
            for (i, slug) in chunk.iter().enumerate() {
                let Some((owner, name)) = slug.split_once('/') else {
                    continue;
                };
                params.push(format!("$o{i}: String!, $n{i}: String!"));
                fields.push(format!(
                    "r{i}: repository(owner: $o{i}, name: $n{i}) {{ {TAG_REFS} }}"
                ));
                variables.insert(format!("o{i}"), json!(owner));
                variables.insert(format!("n{i}"), json!(name));
            }
            if fields.is_empty() {
                continue;
            }
            let query = format!("query({}) {{ {} }}", params.join(", "), fields.join(" "));
            // A repository that cannot be read comes back null, with an
            // error beside the others' data.
            let data = match self
                .graphql_answer::<BTreeMap<String, Option<TagRepo>>>(
                    &query,
                    &serde_json::Value::Object(variables),
                )
                .and_then(|a| a.data.ok_or_else(|| graphql_failure(a.errors)))
            {
                Ok(data) => data,
                Err(e) => {
                    out.failures.push(format!("{}: {e}", chunk.join(", ")));
                    continue;
                }
            };
            for (i, slug) in chunk.iter().enumerate() {
                let Some(Some(repo)) = data.get(&format!("r{i}")) else {
                    continue;
                };
                let tags: Vec<AnnotatedTag> = repo
                    .refs
                    .nodes
                    .iter()
                    .filter_map(|r| {
                        let actor = r.target.as_ref()?.tagger.as_ref()?;
                        Some(AnnotatedTag {
                            tag: r.name.clone(),
                            tagger: actor.tagger()?,
                        })
                    })
                    .collect();
                out.tags.insert(slug.clone(), tags);
            }
        }
        out
    }

    /// The commits `login` authored from `since` to `until` (both
    /// included) on each of `repos`' (`owner/name`) default branch, as
    /// GitHub links commits to the account: 25 repositories to a query,
    /// after one asking for the account's id. Repositories without any
    /// are left out; one the token cannot read too. A query that fails
    /// is recorded in [`CommitCounts::failures`] while the others count.
    pub fn commit_counts(
        &self,
        login: &str,
        repos: &[String],
        since: chrono::NaiveDate,
        until: chrono::NaiveDate,
    ) -> Result<CommitCounts, Box<dyn std::error::Error>> {
        let mut out = CommitCounts::default();
        if repos.is_empty() {
            return Ok(out);
        }
        let answer: GraphqlResponse<UserIdData> =
            self.graphql_answer(USER_ID_QUERY, &json!({"login": login}))?;
        let id = answer
            .data
            .ok_or_else(|| graphql_failure(answer.errors))?
            .user
            .ok_or_else(|| format!("no GitHub user {login}"))?
            .id;
        for chunk in repos.chunks(25) {
            let mut params =
                vec!["$id: ID!, $since: GitTimestamp!, $until: GitTimestamp!".to_string()];
            let mut fields = Vec::new();
            let mut variables = serde_json::Map::new();
            variables.insert("id".into(), json!(id));
            variables.insert("since".into(), json!(format!("{since}T00:00:00Z")));
            variables.insert("until".into(), json!(format!("{until}T23:59:59Z")));
            for (i, slug) in chunk.iter().enumerate() {
                let Some((owner, name)) = slug.split_once('/') else {
                    continue;
                };
                params.push(format!("$o{i}: String!, $n{i}: String!"));
                fields.push(format!(
                    "r{i}: repository(owner: $o{i}, name: $n{i}) {{ {HISTORY} }}"
                ));
                variables.insert(format!("o{i}"), json!(owner));
                variables.insert(format!("n{i}"), json!(name));
            }
            if fields.is_empty() {
                continue;
            }
            let query = format!("query({}) {{ {} }}", params.join(", "), fields.join(" "));
            let data = match self
                .graphql_answer::<BTreeMap<String, Option<HistoryRepo>>>(
                    &query,
                    &serde_json::Value::Object(variables),
                )
                .and_then(|a| a.data.ok_or_else(|| graphql_failure(a.errors)))
            {
                Ok(data) => data,
                Err(e) => {
                    out.failures.push(format!("{}: {e}", chunk.join(", ")));
                    continue;
                }
            };
            for (i, slug) in chunk.iter().enumerate() {
                let count = data
                    .get(&format!("r{i}"))
                    .and_then(Option::as_ref)
                    .and_then(|r| r.default_branch_ref.as_ref())
                    .and_then(|b| b.target.as_ref())
                    .and_then(|t| t.history.as_ref())
                    .map_or(0, |h| h.total_count);
                if count > 0 {
                    out.counts.insert(slug.clone(), count);
                }
            }
        }
        Ok(out)
    }

    /// The whole answer to a GraphQL query, errors included, for a
    /// caller that can use partial data.
    fn graphql_answer<T: serde::de::DeserializeOwned>(
        &self,
        query: &str,
        variables: &serde_json::Value,
    ) -> Result<GraphqlResponse<T>, Box<dyn std::error::Error>> {
        let url = graphql_url(&self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&json!({"query": query, "variables": variables}))
            .send()?;
        let what = format!("GitHub GraphQL {url}");
        let text = http::blocking_ok(resp, &what)?.text()?;
        // Decoded here rather than by reqwest, whose error does not say
        // which field did not fit.
        serde_json::from_str(&text)
            .map_err(|e| format!("{what}: cannot read the answer: {e}").into())
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

/// An error for a GraphQL answer without data: GitHub's messages, if
/// any.
fn graphql_failure(errors: Option<Vec<GraphqlError>>) -> Box<dyn std::error::Error> {
    let messages: Vec<String> = errors
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.message)
        .collect();
    if messages.is_empty() {
        "GitHub GraphQL: no data in the answer".into()
    } else {
        format!("GitHub GraphQL: {}", messages.join("; ")).into()
    }
}

/// The GraphQL endpoint for a REST base: `https://api.github.com/graphql`,
/// or a GHES instance's `/api/graphql` for its `/api/v3`.
fn graphql_url(base: &str) -> String {
    match base.strip_suffix("/api/v3") {
        Some(host) => format!("{host}/api/graphql"),
        None => format!("{base}/graphql"),
    }
}

/// The most repositories `commitContributionsByRepository` names.
const MAX_REPOSITORIES: usize = 100;

const CONTRIBUTIONS_QUERY: &str = "\
query($login: String!, $from: DateTime!, $to: DateTime!, $after: String) {
  user(login: $login) {
    contributionsCollection(from: $from, to: $to) {
      restrictedContributionsCount
      commitContributionsByRepository(maxRepositories: 100) {
        repository { nameWithOwner isPrivate }
        contributions { totalCount }
      }
      pullRequestReviewContributions(first: 100, after: $after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          pullRequest { number title url state repository { nameWithOwner isPrivate } }
        }
      }
    }
  }
}";

/// A repository's 50 newest tags and, for an annotated one, its tagger.
const TAG_REFS: &str = "refs(refPrefix: \"refs/tags/\", first: 50, \
    orderBy: {field: TAG_COMMIT_DATE, direction: DESC}) { \
    nodes { name target { ... on Tag { tagger { name email date } } } } }";

const USER_ID_QUERY: &str = "query($login: String!) { user(login: $login) { id } }";

/// The commits by `$id` from `$since` to `$until` on a repository's
/// default branch.
const HISTORY: &str = "defaultBranchRef { target { ... on Commit { \
    history(author: {id: $id}, since: $since, until: $until) { totalCount } } } }";

const ISSUE_COMMENTS_QUERY: &str = "\
query($login: String!, $after: String) {
  user(login: $login) {
    issueComments(first: 100, after: $after, orderBy: {field: UPDATED_AT, direction: DESC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        createdAt updatedAt url
        issue { number title url state repository { nameWithOwner isPrivate } }
        pullRequest { number title url state repository { nameWithOwner isPrivate } }
      }
    }
  }
}";

#[derive(Deserialize)]
struct TagRepo {
    refs: TagRefs,
}

#[derive(Deserialize)]
struct TagRefs {
    nodes: Vec<TagRefNode>,
}

#[derive(Deserialize)]
struct TagRefNode {
    name: String,
    /// `{}` for a lightweight tag's commit, which has no tagger.
    target: Option<TagTarget>,
}

#[derive(Deserialize)]
struct TagTarget {
    tagger: Option<GitActor>,
}

/// A tagger as GraphQL gives it: any field may be null, and the date
/// is in the tagger's own time zone.
#[derive(Deserialize)]
struct GitActor {
    name: Option<String>,
    email: Option<String>,
    date: Option<String>,
}

impl GitActor {
    /// The tagger, its date in UTC as REST gives it; none without a
    /// readable date.
    fn tagger(&self) -> Option<Tagger> {
        let date = chrono::DateTime::parse_from_rfc3339(self.date.as_deref()?).ok()?;
        Some(Tagger {
            name: self.name.clone().unwrap_or_default(),
            email: self.email.clone().unwrap_or_default(),
            date: date.to_utc().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        })
    }
}

#[derive(Deserialize)]
struct UserIdData {
    user: Option<UserId>,
}

#[derive(Deserialize)]
struct UserId {
    id: String,
}

#[derive(Deserialize)]
struct HistoryRepo {
    #[serde(rename = "defaultBranchRef")]
    default_branch_ref: Option<HistoryBranch>,
}

#[derive(Deserialize)]
struct HistoryBranch {
    target: Option<HistoryTarget>,
}

#[derive(Deserialize)]
struct HistoryTarget {
    history: Option<Count>,
}

#[derive(Deserialize)]
struct PushedRepo {
    full_name: String,
    pushed_at: Option<String>,
}

#[derive(Deserialize)]
struct GraphqlResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

impl PageInfo {
    /// The cursor of the next page, if there is one.
    fn next(self) -> Option<String> {
        self.end_cursor.filter(|_| self.has_next_page)
    }
}

#[derive(Deserialize)]
struct ContributionsData {
    user: Option<ContributionsUser>,
}

#[derive(Deserialize)]
struct ContributionsUser {
    #[serde(rename = "contributionsCollection")]
    contributions_collection: Collection,
}

#[derive(Deserialize)]
struct Collection {
    #[serde(rename = "restrictedContributionsCount", default)]
    restricted_contributions_count: u64,
    #[serde(rename = "commitContributionsByRepository")]
    commit_contributions_by_repository: Vec<CommitsByRepo>,
    #[serde(rename = "pullRequestReviewContributions")]
    pull_request_review_contributions: ReviewPage,
}

#[derive(Deserialize)]
struct CommitsByRepo {
    repository: RepoNode,
    contributions: Count,
}

#[derive(Deserialize)]
struct Count {
    #[serde(rename = "totalCount")]
    total_count: u64,
}

#[derive(Deserialize)]
struct ReviewPage {
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
    /// `null` where the token cannot read the review.
    nodes: Vec<Option<ReviewNode>>,
}

#[derive(Deserialize)]
struct ReviewNode {
    #[serde(rename = "pullRequest")]
    pull_request: Option<ItemNode>,
}

#[derive(Deserialize)]
struct IssueCommentsData {
    user: Option<IssueCommentsUser>,
}

#[derive(Deserialize)]
struct IssueCommentsUser {
    #[serde(rename = "issueComments")]
    issue_comments: CommentPage,
}

#[derive(Deserialize)]
struct CommentPage {
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
    /// `null` where the token cannot read the comment.
    nodes: Vec<Option<IssueComment>>,
}

/// Build a reqwest client preconfigured for the GitHub API: the
/// Bearer token, the recommended JSON Accept header, a User-Agent
/// (GitHub requires one), and our standard request timeout.
fn build_http_client(token: &str) -> Result<reqwest::blocking::Client, Box<dyn std::error::Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    headers.insert(
        HeaderName::from_static("x-github-api-version"),
        HeaderValue::from_static("2022-11-28"),
    );
    Ok(http::blocking_builder(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ))
    .default_headers(headers)
    .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn graphql_body(data: serde_json::Value, errors: serde_json::Value) -> String {
        json!({"data": data, "errors": errors}).to_string()
    }

    fn pr(repo: &str, n: u64, private: bool) -> serde_json::Value {
        json!({"number": n, "title": "t", "state": "OPEN",
               "url": format!("https://github.com/{repo}/pull/{n}"),
               "repository": {"nameWithOwner": repo, "isPrivate": private}})
    }

    #[test]
    fn graphql_lives_beside_the_rest_api() {
        assert_eq!(
            graphql_url("https://api.github.com"),
            "https://api.github.com/graphql"
        );
        assert_eq!(
            graphql_url("https://ghe.example.com/api/v3"),
            "https://ghe.example.com/api/graphql"
        );
    }

    #[test]
    fn contributions_are_read_a_year_and_a_page_at_a_time() {
        let mut server = mockito::Server::new();
        let collection = |commits: serde_json::Value, reviews: serde_json::Value, next: bool| {
            json!({"user": {"contributionsCollection": {
                "restrictedContributionsCount": 2,
                "commitContributionsByRepository": commits,
                "pullRequestReviewContributions": {
                    "pageInfo": {"hasNextPage": next, "endCursor": if next { "c1" } else { "" }},
                    "nodes": reviews
                }
            }}})
        };
        let commits = json!([
            {"repository": {"nameWithOwner": "o/pub", "isPrivate": false},
             "contributions": {"totalCount": 3}},
            {"repository": {"nameWithOwner": "o/priv", "isPrivate": true},
             "contributions": {"totalCount": 1}}
        ]);
        // First year, first page of reviews: one withheld.
        let first = server
            .mock("POST", "/graphql")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::Regex("2025-01-01T00:00:00Z".into()),
                mockito::Matcher::Regex(r#""after":null"#.into()),
            ]))
            .with_body(graphql_body(
                collection(
                    commits.clone(),
                    json!([{"pullRequest": pr("o/pub", 1, false)}, null]),
                    true,
                ),
                json!([{"message": "`x` forbids access"}]),
            ))
            .create();
        // Its second page: the commits are not counted again.
        let second = server
            .mock("POST", "/graphql")
            .match_body(mockito::Matcher::Regex(r#""after":"c1""#.into()))
            .with_body(graphql_body(
                collection(
                    commits,
                    json!([{"pullRequest": pr("o/pub", 1, false)}, {"pullRequest": pr("o/priv", 2, true)}]),
                    false,
                ),
                json!(null),
            ))
            .create();
        // The second year starts the day after the first ends.
        let year2 = server
            .mock("POST", "/graphql")
            .match_body(mockito::Matcher::Regex("2026-01-01T00:00:00Z".into()))
            .with_body(graphql_body(
                collection(
                    json!([{"repository": {"nameWithOwner": "o/pub", "isPrivate": false},
                            "contributions": {"totalCount": 4}}]),
                    json!([]),
                    false,
                ),
                json!(null),
            ))
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let c = client
            .contributions("me", day("2025-01-01"), day("2026-02-01"))
            .unwrap();
        first.assert();
        second.assert();
        year2.assert();
        assert_eq!(
            c.commits,
            BTreeMap::from([("o/priv".to_string(), 1), ("o/pub".to_string(), 7)])
        );
        assert_eq!(c.private_repos, BTreeSet::from(["o/priv".to_string()]));
        let reviewed: Vec<u64> = c.reviewed.iter().map(|p| p.number).collect();
        assert_eq!(reviewed, [1, 2]);
        assert_eq!(c.unreadable.count, 1);
        assert_eq!(
            c.unreadable.reasons,
            BTreeSet::from(["`x` forbids access".to_string()])
        );
        assert_eq!(c.restricted, 4);
    }

    #[test]
    fn a_window_naming_100_repositories_is_split() {
        let mut server = mockito::Server::new();
        let repos = |prefix: &str, n: usize| -> serde_json::Value {
            (0..n)
                .map(|i| {
                    json!({"repository": {"nameWithOwner": format!("{prefix}/{i}"), "isPrivate": false},
                           "contributions": {"totalCount": 1}})
                })
                .collect()
        };
        let answer = |commits: serde_json::Value| {
            graphql_body(
                json!({"user": {"contributionsCollection": {
                    "commitContributionsByRepository": commits,
                    "pullRequestReviewContributions": {
                        "pageInfo": {"hasNextPage": false, "endCursor": null}, "nodes": []}
                }}}),
                json!(null),
            )
        };
        let window = |from: &str, to: &str| {
            mockito::Matcher::AllOf(vec![
                mockito::Matcher::Regex(format!(r#""from":"{from}T00:00:00Z""#)),
                mockito::Matcher::Regex(format!(r#""to":"{to}T23:59:59Z""#)),
            ])
        };
        let both = server
            .mock("POST", "/graphql")
            .match_body(window("2026-01-01", "2026-01-02"))
            .with_body(answer(repos("both", 100)))
            .create();
        let first = server
            .mock("POST", "/graphql")
            .match_body(window("2026-01-01", "2026-01-01"))
            .with_body(answer(repos("a", 2)))
            .create();
        // A single day at the limit cannot be split: counted, and capped.
        let second = server
            .mock("POST", "/graphql")
            .match_body(window("2026-01-02", "2026-01-02"))
            .with_body(answer(repos("b", 100)))
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let c = client
            .contributions("me", day("2026-01-01"), day("2026-01-02"))
            .unwrap();
        both.assert();
        first.assert();
        second.assert();
        assert_eq!(c.commits.len(), 102);
        assert!(!c.commits.contains_key("both/0"));
        assert!(c.capped);
    }

    #[test]
    fn comments_are_read_back_to_the_day_asked() {
        let mut server = mockito::Server::new();
        let comment = |at: &str| {
            json!({"createdAt": at, "updatedAt": at, "url": "u",
                   "issue": pr("o/r", 1, false), "pullRequest": null})
        };
        let mock = server
            .mock("POST", "/graphql")
            .with_body(graphql_body(
                json!({"user": {"issueComments": {
                    "pageInfo": {"hasNextPage": true, "endCursor": "next"},
                    "nodes": [comment("2026-02-02T00:00:00Z"), null, comment("2026-01-02T00:00:00Z"), null]
                }}}),
                json!([{"message": "withheld"}]),
            ))
            .expect(1)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let c = client.issue_comments("me", day("2026-02-01")).unwrap();
        mock.assert();
        assert_eq!(c.comments.len(), 1);
        assert_eq!(c.comments[0].issue.as_ref().unwrap().number, 1);
        assert_eq!(c.unreadable.count, 1);
        // A user GitHub does not know is an error.
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/graphql")
            .with_body(graphql_body(json!({"user": null}), json!(null)))
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        assert!(client.issue_comments("ghost", day("2026-02-01")).is_err());
        // No data at all: GitHub's message.
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/graphql")
            .with_body(graphql_body(json!(null), json!([{"message": "bad query"}])))
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let err = client
            .contributions("me", day("2026-01-01"), day("2026-01-02"))
            .unwrap_err();
        assert_eq!(err.to_string(), "GitHub GraphQL: bad query");
        assert_eq!(
            graphql_failure(None).to_string(),
            "GitHub GraphQL: no data in the answer"
        );
    }

    #[test]
    fn private_repos_are_read_until_an_older_push() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/user/repos")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("visibility".into(), "private".into()),
                mockito::Matcher::UrlEncoded("sort".into(), "pushed".into()),
                mockito::Matcher::UrlEncoded("page".into(), "1".into()),
            ]))
            .with_body(
                json!([
                    {"full_name": "o/new", "pushed_at": "2026-02-20T00:00:00Z"},
                    {"full_name": "o/empty", "pushed_at": null},
                    {"full_name": "o/newer", "pushed_at": "2026-02-10T00:00:00Z"},
                    {"full_name": "o/old", "pushed_at": "2025-01-01T00:00:00Z"}
                ])
                .to_string(),
            )
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        assert_eq!(
            client
                .private_repos_pushed_since(day("2026-02-01"))
                .unwrap(),
            ["o/new", "o/newer"]
        );
        mock.assert();
    }

    #[test]
    fn annotated_tags_are_read_ten_repos_to_a_query() {
        let mut server = mockito::Server::new();
        // In the tagger's time zone: 2026-02-10T23:30-05:00 is the 11th
        // in UTC.
        let tagger = json!({"name": "Me", "email": null, "date": "2026-02-10T23:30:00-05:00"});
        let mock = server
            .mock("POST", "/graphql")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::Regex(r#""o0":"o""#.into()),
                mockito::Matcher::Regex(r#""n0":"a""#.into()),
                mockito::Matcher::Regex(r#""n1":"gone""#.into()),
            ]))
            .with_body(graphql_body(
                json!({
                    "r0": {"refs": {"nodes": [
                        {"name": "v1", "target": {"tagger": tagger}},
                        {"name": "undated", "target": {"tagger": {"name": "Me", "email": "e", "date": null}}},
                        {"name": "light", "target": {}}
                    ]}},
                    "r1": null
                }),
                json!([{"message": "Could not resolve to a Repository"}]),
            ))
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let recent = client.recent_annotated_tags(&[
            "o/a".to_string(),
            "o/gone".to_string(),
            "bad".to_string(),
        ]);
        mock.assert();
        assert!(recent.failures.is_empty());
        assert_eq!(recent.tags.len(), 1);
        let tags = &recent.tags["o/a"];
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].tag, "v1");
        assert_eq!(tags[0].tagger.email, "");
        assert_eq!(tags[0].tagger.date, "2026-02-11T04:30:00Z");
        // Nothing to ask about: no request.
        assert!(
            client
                .recent_annotated_tags(&["bad".to_string()])
                .tags
                .is_empty()
        );
        // A failed query is recorded; the others still count.
        let mut down = mockito::Server::new();
        down.mock("POST", "/graphql").with_status(502).create();
        let client = Client::new(&down.url(), "tok").unwrap();
        let recent = client.recent_annotated_tags(&["o/a".to_string()]);
        assert!(recent.tags.is_empty());
        assert_eq!(recent.failures.len(), 1);
        assert!(
            recent.failures[0].starts_with("o/a: "),
            "{:?}",
            recent.failures
        );
    }

    #[test]
    fn commits_are_counted_twenty_five_repos_to_a_query() {
        let mut server = mockito::Server::new();
        let id = server
            .mock("POST", "/graphql")
            .match_body(mockito::Matcher::Regex(
                "user\\(login: \\$login\\) \\{ id \\}".into(),
            ))
            .with_body(graphql_body(json!({"user": {"id": "U_1"}}), json!(null)))
            .create();
        let history =
            |n: u64| json!({"defaultBranchRef": {"target": {"history": {"totalCount": n}}}});
        let counts = server
            .mock("POST", "/graphql")
            .match_body(mockito::Matcher::AllOf(vec![
                mockito::Matcher::Regex("history\\(author".into()),
                mockito::Matcher::Regex(r#""id":"U_1""#.into()),
                mockito::Matcher::Regex(r#""since":"2026-02-01T00:00:00Z""#.into()),
            ]))
            .with_body(graphql_body(
                json!({"r0": history(3), "r1": history(0), "r2": null, "r3": {"defaultBranchRef": null}}),
                json!([{"message": "Could not resolve"}]),
            ))
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let repos: Vec<String> = ["o/a", "o/none", "o/gone", "o/empty"]
            .map(String::from)
            .into();
        let c = client
            .commit_counts("me", &repos, day("2026-02-01"), day("2026-02-28"))
            .unwrap();
        id.assert();
        counts.assert();
        assert_eq!(c.counts, BTreeMap::from([("o/a".to_string(), 3)]));
        assert!(c.failures.is_empty());
        // No repositories: no request at all.
        assert!(
            client
                .commit_counts("me", &[], day("2026-02-01"), day("2026-02-28"))
                .unwrap()
                .counts
                .is_empty()
        );
    }

    #[test]
    fn the_token_names_its_owner() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/user")
            .with_body(r#"{"id": 1, "login": "me"}"#)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        assert_eq!(client.token_login().unwrap(), "me");
    }

    #[test]
    fn pull_request_reads_head_and_base() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/repos/o/r/pulls/92")
            .with_status(200)
            .with_body(
                r#"{"number": 92, "state": "closed", "merged": false,
                    "head": {"ref": "fix", "sha": "8b8aa8b"},
                    "base": {"ref": "main", "sha": "0000000"}}"#,
            )
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let pr = client.pull_request("o", "r", 92).unwrap();
        mock.assert();
        assert_eq!(pr.head.sha, "8b8aa8b");
        assert_eq!(pr.base.ref_name, "main");
        assert!(!pr.merged);
    }

    #[test]
    fn branch_commits_by_filters_by_author_and_since() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/repos/o/r/commits")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("sha".into(), "main".into()),
                mockito::Matcher::UrlEncoded("author".into(), "michel-slm".into()),
                mockito::Matcher::UrlEncoded("since".into(), "2026-03-04T00:00:00Z".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"[{"sha": "abc", "commit": {"message": "Ignore it\n\nbody",
                     "author": {"name": "Michel Lind", "email": "m@example.org"}}}]"#,
            )
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        let commits = client
            .branch_commits_by("o", "r", "main", "michel-slm", "2026-03-04T00:00:00Z")
            .unwrap();
        mock.assert();
        assert_eq!(commits[0].title(), "Ignore it");
        assert_eq!(
            commits[0].commit.author.as_ref().unwrap().name,
            "Michel Lind"
        );
    }

    #[test]
    fn commit_contained_reads_ahead_by() {
        // ahead_by == 0 (status "identical" or "behind") → the sha is on
        // the branch already.
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/repos/o/r/compare/main...8b8aa8b")
            .with_status(200)
            .with_body(r#"{"status": "behind", "ahead_by": 0, "behind_by": 4}"#)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        assert!(
            client
                .commit_contained("o", "r", "main", "8b8aa8b")
                .unwrap()
        );
        mock.assert();

        let mut server = mockito::Server::new();
        server
            .mock("GET", "/repos/o/r/compare/main...other")
            .with_status(200)
            .with_body(r#"{"status": "diverged", "ahead_by": 2, "behind_by": 4}"#)
            .create();
        let client = Client::new(&server.url(), "tok").unwrap();
        assert!(!client.commit_contained("o", "r", "main", "other").unwrap());
    }

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
    fn repository_public_reads_the_private_flag() {
        let mut server = mockito::Server::new();
        let public = server
            .mock("GET", "/repos/o/pub")
            .with_body(r#"{"id": 1, "private": false}"#)
            .create();
        let private = server
            .mock("GET", "/repos/o/priv")
            .with_body(r#"{"id": 2, "private": true}"#)
            .create();
        let gone = server
            .mock("GET", "/repos/o/gone")
            .with_status(404)
            .create();
        let broken = server.mock("GET", "/repos/o/err").with_status(500).create();
        let client = Client::new(&server.url(), "tok").unwrap();
        assert_eq!(client.repository_public("o", "pub").unwrap(), Some(true));
        assert_eq!(client.repository_public("o", "priv").unwrap(), Some(false));
        assert_eq!(client.repository_public("o", "gone").unwrap(), None);
        assert!(client.repository_public("o", "err").is_err());
        for m in [public, private, gone, broken] {
            m.assert();
        }
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
        };
        assert_eq!(repo.slug(), Some("o/r"));
        // Event payloads only ship `name`.
        let evt_repo = Repository {
            id: 1,
            full_name: None,
            name: Some("o/r".into()),
        };
        assert_eq!(evt_repo.slug(), Some("o/r"));
    }
}
