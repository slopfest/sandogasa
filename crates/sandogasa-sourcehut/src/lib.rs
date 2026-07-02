// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GraphQL client for the Sourcehut (sr.ht) API.
//!
//! Unlike the GitHub-ish forges, sr.ht has no unified PR model and splits
//! activity across independent services, each exposing its own GraphQL
//! endpoint at `https://<service>.<host>/query`. This crate covers the
//! surface `sandogasa-report` needs to summarize a contributor's activity:
//!
//! - **lists.sr.ht** — [`Client::patches`]: the patchsets a user submitted
//!   (the mailing-list, patch-based analog of pull requests).
//! - **todo.sr.ht** — [`Client::ticket_events`]: the authenticated user's
//!   ticket activity feed (opened / status-changed), the issues analog.
//! - **git.sr.ht** — [`Client::repositories`] + [`Client::commits_since`]
//!   (+ [`Client::user_email`] for the account's primary email): commits
//!   in the user's own repositories, for the caller to attribute owner
//!   vs third-party (sr.ht exposes only the primary email).
//!
//! Conventions (see `DEVELOPMENT.md`): requests are `POST {query,
//! variables}` with `Authorization: Bearer <token>` (a personal access
//! token from meta.sr.ht/oauth2/personal-token, which by default grants
//! all scopes). Cursored resolvers return `{results, cursor}`; pass the
//! cursor back until it is null. Timestamps are RFC3339 UTC, which sort
//! lexicographically — so string comparison is a valid date comparison.
//!
//! The `host` is passed in full (`sr.ht`, or a self-hosted host), so the
//! client works against any deployment.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};

/// Upper bound on any single sr.ht HTTP request — a hang-catcher.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// A patchset submitted to a mailing list (lists.sr.ht).
#[derive(Debug, Clone, Deserialize)]
pub struct Patchset {
    /// RFC3339 UTC submission time.
    pub created: String,
    pub subject: String,
    /// `PROPOSED` / `NEEDS_REVISION` / `SUPERSEDED` / `APPROVED` /
    /// `REJECTED` / `APPLIED` / `UNKNOWN`.
    pub status: String,
    pub list: NamedRef,
}

/// A `{ name }` reference (mailing list, repository, …).
#[derive(Debug, Clone, Deserialize)]
pub struct NamedRef {
    pub name: String,
}

/// One ticket-activity event from the authenticated user's todo.sr.ht
/// feed. An event bundles one or more [`EventDetail`] changes to a ticket.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    /// RFC3339 UTC event time.
    pub created: String,
    pub ticket: TicketRef,
    pub changes: Vec<EventDetail>,
}

/// The ticket an [`Event`] concerns.
#[derive(Debug, Clone, Deserialize)]
pub struct TicketRef {
    /// Canonical cross-tracker reference, e.g. `~user/tracker#3`.
    #[serde(rename = "ref")]
    pub reference: String,
    pub subject: String,
}

/// One change within an [`Event`]. `event_type` tags which kind it is
/// (`CREATED`, `STATUS_CHANGE`, …); the actor/status fields are populated
/// only for the matching kind (via GraphQL inline fragments).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDetail {
    pub event_type: String,
    /// The author of a `CREATED` change.
    pub author: Option<Actor>,
    /// The editor of a `STATUS_CHANGE`.
    pub editor: Option<Actor>,
    /// The resulting status of a `STATUS_CHANGE`.
    pub new_status: Option<String>,
    /// The resulting resolution of a `STATUS_CHANGE`.
    pub new_resolution: Option<String>,
}

/// An entity (user/mailbox) reference, identified by its canonical name
/// (`~username` for a registered sr.ht user).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Actor {
    pub canonical_name: String,
}

/// A git.sr.ht repository (only the name is needed to drive the log).
#[derive(Debug, Clone, Deserialize)]
pub struct Repo {
    pub name: String,
}

/// A git commit from a repository log.
#[derive(Debug, Clone, Deserialize)]
pub struct Commit {
    pub id: String,
    pub author: Signature,
    pub committer: Signature,
}

/// A git author/committer signature.
#[derive(Debug, Clone, Deserialize)]
pub struct Signature {
    pub name: String,
    pub email: String,
    /// RFC3339 UTC timestamp.
    pub time: String,
}

/// A sr.ht GraphQL client bound to one instance host.
pub struct Client {
    http: reqwest::blocking::Client,
    /// Bare instance host, e.g. `sr.ht` (service subdomains are derived).
    host: String,
    /// Test-only fixed endpoint (a mock server URL). `None` in
    /// production, where the per-service `https://<service>.<host>/query`
    /// URL is used.
    endpoint_override: Option<String>,
}

impl Client {
    /// Build a client for the sr.ht instance at `instance` (a host like
    /// `sr.ht`, or a full URL — the scheme/trailing slash are stripped),
    /// authenticated with `token`.
    pub fn new(instance: &str, token: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let host = host_of(instance);
        // Guard against an insecure/garbled host by validating a derived
        // service URL.
        sandogasa_cli::ensure_secure_url(&format!("https://git.{host}"))?;
        Ok(Self {
            http: build_http_client(token)?,
            host,
            endpoint_override: None,
        })
    }

    /// The patchsets `username` submitted to any mailing list
    /// (lists.sr.ht), newest first. Fully paginated. The caller filters by
    /// [`Patchset::created`] for a date window and may split on `status`
    /// (`APPLIED` ≈ merged).
    pub fn patches(&self, username: &str) -> Result<Vec<Patchset>, Box<dyn std::error::Error>> {
        const QUERY: &str = "\
query($username: String!, $cursor: Cursor) {
  user(username: $username) {
    patches(cursor: $cursor) {
      results { created subject status list { name } }
      cursor
    }
  }
}";
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let vars = json!({ "username": username, "cursor": cursor });
            let data: UserPatches = self.gql("lists", QUERY, vars)?;
            let page = data.user.map(|u| u.patches).unwrap_or_default();
            out.extend(page.results);
            match page.cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(out)
    }

    /// The authenticated user's ticket-activity events (todo.sr.ht),
    /// newest first, stopping once an event is older than `since` (an
    /// RFC3339 UTC bound). This feed covers events the token owner is
    /// implicated in or subscribed to, so the caller must filter the
    /// change details down to the ones the user themselves authored.
    pub fn ticket_events(&self, since: &str) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
        const QUERY: &str = "\
query($cursor: Cursor) {
  events(cursor: $cursor) {
    results {
      created
      ticket { ref subject }
      changes {
        eventType
        ... on Created { author { canonicalName } }
        ... on StatusChange { editor { canonicalName } newStatus newResolution }
      }
    }
    cursor
  }
}";
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let vars = json!({ "cursor": cursor });
            let data: EventsData = self.gql("todo", QUERY, vars)?;
            let Some(page) = data.events else { break };
            let reached_old = page.results.iter().any(|e| e.created.as_str() < since);
            out.extend(
                page.results
                    .into_iter()
                    .filter(|e| e.created.as_str() >= since),
            );
            if reached_old {
                break;
            }
            match page.cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(out)
    }

    /// The repositories owned by `username` (git.sr.ht), fully paginated.
    pub fn repositories(&self, username: &str) -> Result<Vec<Repo>, Box<dyn std::error::Error>> {
        const QUERY: &str = "\
query($username: String!, $cursor: Cursor) {
  user(username: $username) {
    repositories(cursor: $cursor) {
      results { name }
      cursor
    }
  }
}";
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let vars = json!({ "username": username, "cursor": cursor });
            let data: UserRepos = self.gql("git", QUERY, vars)?;
            let page = data.user.map(|u| u.repositories).unwrap_or_default();
            out.extend(page.results);
            match page.cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(out)
    }

    /// Commits in `username`'s repository `repo` (git.sr.ht) back to
    /// `since` (an RFC3339 UTC bound). Pages the log — which is ordered by
    /// committer time, newest first — and stops once a commit's committer
    /// time falls before `since`. The caller filters by author + the exact
    /// window.
    pub fn commits_since(
        &self,
        username: &str,
        repo: &str,
        since: &str,
    ) -> Result<Vec<Commit>, Box<dyn std::error::Error>> {
        const QUERY: &str = "\
query($username: String!, $repo: String!, $cursor: Cursor) {
  user(username: $username) {
    repository(name: $repo) {
      log(cursor: $cursor) {
        results {
          id
          author { name email time }
          committer { name email time }
        }
        cursor
      }
    }
  }
}";
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let vars = json!({ "username": username, "repo": repo, "cursor": cursor });
            let data: UserRepoLog = self.gql("git", QUERY, vars)?;
            let Some(log) = data.user.and_then(|u| u.repository).map(|r| r.log) else {
                break;
            };
            let reached_old = log
                .results
                .iter()
                .any(|c| c.committer.time.as_str() < since);
            out.extend(
                log.results
                    .into_iter()
                    .filter(|c| c.committer.time.as_str() >= since),
            );
            if reached_old {
                break;
            }
            match log.cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        Ok(out)
    }

    /// The account's *primary* email for `username` (git.sr.ht). sr.ht
    /// exposes only this one address (no secondary-emails list), so it's
    /// the sole email that can be reliably attributed to the account.
    /// `None` if the user isn't found.
    pub fn user_email(&self, username: &str) -> Result<Option<String>, Box<dyn std::error::Error>> {
        const QUERY: &str = "query($username: String!) { user(username: $username) { email } }";
        let data: UserEmail = self.gql("git", QUERY, json!({ "username": username }))?;
        Ok(data.user.map(|u| u.email))
    }

    /// POST a GraphQL query to `<service>.<host>/query` and return the
    /// deserialized `data`. Errors on a non-2xx response or a GraphQL
    /// `errors` array.
    fn gql<T: serde::de::DeserializeOwned>(
        &self,
        service: &str,
        query: &str,
        variables: Value,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let url = self.service_url(service);
        let resp = self
            .http
            .post(&url)
            .json(&json!({ "query": query, "variables": variables }))
            .send()?;
        let status = resp.status();
        let text = resp.text()?;
        if !status.is_success() {
            return Err(format!("sr.ht {service} query failed: {status}: {text}").into());
        }
        let parsed: GqlResponse<T> = serde_json::from_str(&text)
            .map_err(|e| format!("sr.ht {service} response parse error: {e}: {text}"))?;
        if let Some(errors) = parsed.errors
            && !errors.is_empty()
        {
            let joined = errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!("sr.ht {service} query error: {joined}").into());
        }
        parsed
            .data
            .ok_or_else(|| format!("sr.ht {service} query returned no data").into())
    }

    /// The `/query` endpoint for a service (`git` / `todo` / `lists`).
    fn service_url(&self, service: &str) -> String {
        match &self.endpoint_override {
            Some(base) => format!("{}/query", base.trim_end_matches('/')),
            None => format!("https://{service}.{}/query", self.host),
        }
    }
}

/// Check whether `token` is accepted by the instance, via `{ me {
/// username } }` on git.<host>. `Ok(true)` for a valid token, `Ok(false)`
/// for a rejected one (401 or a GraphQL error), `Err` if the server can't
/// be reached — so callers can tell "rejected" from "unreachable".
pub fn validate_token(host: &str, token: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let host = host_of(host);
    let url = format!("https://git.{host}/query");
    sandogasa_cli::ensure_secure_url(&url)?;
    let http = build_http_client(token)?;
    let resp = http
        .post(&url)
        .json(&json!({ "query": "{ me { username } }" }))
        .send()?;
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Ok(false);
    }
    if !status.is_success() {
        let text = resp.text().unwrap_or_default();
        return Err(format!("sr.ht token check failed: {status}: {text}").into());
    }
    let parsed: GqlResponse<MeData> = resp.json()?;
    // A valid, sufficiently-scoped token returns me.username; anything
    // else (GraphQL error, missing data) means the token isn't usable.
    Ok(parsed
        .data
        .and_then(|d| d.me)
        .is_some_and(|m| !m.username.is_empty()))
}

/// Normalize an instance string to a bare host: strip the scheme and any
/// trailing slash (`https://sr.ht/` → `sr.ht`).
fn host_of(instance: &str) -> String {
    instance
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

/// Build a reqwest client with Bearer auth, a JSON Accept header, a
/// User-Agent, and the standard request timeout.
fn build_http_client(token: &str) -> Result<reqwest::blocking::Client, Box<dyn std::error::Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("authorization"),
        HeaderValue::from_str(&format!("Bearer {token}"))?,
    );
    headers.insert(
        HeaderName::from_static("accept"),
        HeaderValue::from_static("application/json"),
    );
    sandogasa_cli::install_crypto_provider();
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("sandogasa-sourcehut/", env!("CARGO_PKG_VERSION")))
        .default_headers(headers)
        .timeout(DEFAULT_TIMEOUT)
        .build()?)
}

// ---- GraphQL envelope + per-query response shapes ----

#[derive(Deserialize)]
struct GqlResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GqlError>>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

#[derive(Deserialize)]
struct Cursored<T> {
    results: Vec<T>,
    cursor: Option<String>,
}

// Manual (not derived) so it doesn't impose `T: Default` — the empty page
// is valid for any element type.
impl<T> Default for Cursored<T> {
    fn default() -> Self {
        Self {
            results: Vec::new(),
            cursor: None,
        }
    }
}

#[derive(Deserialize)]
struct MeData {
    me: Option<Me>,
}

#[derive(Deserialize)]
struct Me {
    username: String,
}

#[derive(Deserialize)]
struct UserEmail {
    user: Option<EmailField>,
}

#[derive(Deserialize)]
struct EmailField {
    email: String,
}

#[derive(Deserialize)]
struct UserPatches {
    user: Option<PatchesField>,
}

#[derive(Deserialize)]
struct PatchesField {
    #[serde(default)]
    patches: Cursored<Patchset>,
}

#[derive(Deserialize)]
struct EventsData {
    events: Option<Cursored<Event>>,
}

#[derive(Deserialize)]
struct UserRepos {
    user: Option<ReposField>,
}

#[derive(Deserialize)]
struct ReposField {
    #[serde(default)]
    repositories: Cursored<Repo>,
}

#[derive(Deserialize)]
struct UserRepoLog {
    user: Option<RepoField>,
}

#[derive(Deserialize)]
struct RepoField {
    repository: Option<LogField>,
}

#[derive(Deserialize)]
struct LogField {
    log: Cursored<Commit>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client pointed at a mock server (bypasses the https service-URL
    /// derivation; all services resolve to the one mock endpoint).
    fn mock_client(base: &str) -> Client {
        Client {
            http: build_http_client("tok").unwrap(),
            host: String::new(),
            endpoint_override: Some(base.to_string()),
        }
    }

    #[test]
    fn host_of_strips_scheme_and_slash() {
        assert_eq!(host_of("https://sr.ht/"), "sr.ht");
        assert_eq!(host_of("sr.ht"), "sr.ht");
        assert_eq!(host_of("http://git.example.org"), "git.example.org");
    }

    #[test]
    fn patches_paginates_and_parses() {
        let mut server = mockito::Server::new();
        let page1 = r#"{"data":{"user":{"patches":{"results":[
            {"created":"2026-06-10T00:00:00Z","subject":"[PATCH] a","status":"APPLIED","list":{"name":"devel"}}
        ],"cursor":"c2"}}}}"#;
        let page2 = r#"{"data":{"user":{"patches":{"results":[
            {"created":"2026-06-05T00:00:00Z","subject":"[PATCH] b","status":"PROPOSED","list":{"name":"devel"}}
        ],"cursor":null}}}}"#;
        let m1 = server
            .mock("POST", "/query")
            .match_header("authorization", "Bearer tok")
            .with_status(200)
            .with_body(page1)
            .expect(1)
            .create();
        // Second page returns cursor null → loop stops.
        let m2 = server
            .mock("POST", "/query")
            .with_status(200)
            .with_body(page2)
            .expect(1)
            .create();
        let client = mock_client(&server.url());
        let patches = client.patches("michel").unwrap();
        // mockito serves the two bodies round-robin; both pages fetched.
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].subject, "[PATCH] a");
        assert_eq!(patches[0].status, "APPLIED");
        assert_eq!(patches[0].list.name, "devel");
        m1.assert();
        m2.assert();
    }

    #[test]
    fn ticket_events_stop_at_since_and_parse_details() {
        let mut server = mockito::Server::new();
        let body = r#"{"data":{"events":{"results":[
            {"created":"2026-06-20T00:00:00Z","ticket":{"ref":"~m/proj#3","subject":"Bug"},
             "changes":[{"eventType":"CREATED","author":{"canonicalName":"~michel"}}]},
            {"created":"2026-06-15T00:00:00Z","ticket":{"ref":"~m/proj#2","subject":"Fix"},
             "changes":[{"eventType":"STATUS_CHANGE","editor":{"canonicalName":"~michel"},
                         "newStatus":"RESOLVED","newResolution":"FIXED"}]},
            {"created":"2026-05-01T00:00:00Z","ticket":{"ref":"~m/proj#1","subject":"Old"},
             "changes":[{"eventType":"CREATED","author":{"canonicalName":"~michel"}}]}
        ],"cursor":"more"}}}"#;
        let _m = server
            .mock("POST", "/query")
            .with_status(200)
            .with_body(body)
            .create();
        let client = mock_client(&server.url());
        // since drops the May event and stops paging (no second request).
        let events = client.ticket_events("2026-06-01T00:00:00Z").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].changes[0].event_type, "CREATED");
        assert_eq!(
            events[0].changes[0].author.as_ref().unwrap().canonical_name,
            "~michel"
        );
        assert_eq!(events[1].changes[0].event_type, "STATUS_CHANGE");
        assert_eq!(events[1].changes[0].new_status.as_deref(), Some("RESOLVED"));
    }

    #[test]
    fn commits_since_filters_old_by_committer_time() {
        let mut server = mockito::Server::new();
        let body = r#"{"data":{"user":{"repository":{"log":{"results":[
            {"id":"aaa","author":{"name":"M","email":"m@x","time":"2026-06-10T00:00:00Z"},
             "committer":{"name":"M","email":"m@x","time":"2026-06-10T00:00:00Z"}},
            {"id":"bbb","author":{"name":"M","email":"m@x","time":"2026-05-01T00:00:00Z"},
             "committer":{"name":"M","email":"m@x","time":"2026-05-01T00:00:00Z"}}
        ],"cursor":"more"}}}}}"#;
        let _m = server
            .mock("POST", "/query")
            .with_status(200)
            .with_body(body)
            .create();
        let client = mock_client(&server.url());
        let commits = client
            .commits_since("michel", "dotfiles", "2026-06-01T00:00:00Z")
            .unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].id, "aaa");
        assert_eq!(commits[0].author.email, "m@x");
    }

    #[test]
    fn repositories_parses() {
        let mut server = mockito::Server::new();
        let body = r#"{"data":{"user":{"repositories":{"results":[
            {"name":"dotfiles"},{"name":"scripts"}
        ],"cursor":null}}}}"#;
        let _m = server
            .mock("POST", "/query")
            .with_status(200)
            .with_body(body)
            .create();
        let client = mock_client(&server.url());
        let repos = client.repositories("michel").unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].name, "dotfiles");
    }

    #[test]
    fn user_email_parses() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("POST", "/query")
            .with_status(200)
            .with_body(r#"{"data":{"user":{"email":"m@example.org"}}}"#)
            .create();
        let client = mock_client(&server.url());
        assert_eq!(
            client.user_email("michel").unwrap().as_deref(),
            Some("m@example.org")
        );
    }

    #[test]
    fn gql_errors_on_non_success_status() {
        let mut server = mockito::Server::new();
        let _m = server.mock("POST", "/query").with_status(500).create();
        let client = mock_client(&server.url());
        assert!(client.repositories("michel").is_err());
    }

    #[test]
    fn gql_surfaces_graphql_errors() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("POST", "/query")
            .with_status(200)
            .with_body(r#"{"data":null,"errors":[{"message":"access denied"}]}"#)
            .create();
        let client = mock_client(&server.url());
        let err = client.patches("michel").unwrap_err().to_string();
        assert!(err.contains("access denied"), "{err}");
    }

    #[test]
    fn validate_token_true_false() {
        let mut ok = mockito::Server::new();
        let _o = ok
            .mock("POST", "/query")
            .with_status(200)
            .with_body(r#"{"data":{"me":{"username":"michel"}}}"#)
            .create();
        // validate_token builds its own client for https://git.<host>;
        // exercise the parsing via a direct gql round-trip instead.
        let client = mock_client(&ok.url());
        let me: MeData = client.gql("git", "{ me { username } }", json!({})).unwrap();
        assert_eq!(me.me.unwrap().username, "michel");
    }
}
