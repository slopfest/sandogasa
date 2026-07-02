// SPDX-License-Identifier: Apache-2.0 OR MIT

use reqwest::Client;

use std::collections::HashMap;

use serde::Deserialize;

use crate::acl::{AccessGroups, AccessLevel, AccessResult, AccessUsers, Contributors, ProjectAcls};

const DISTGIT_BASE: &str = "https://src.fedoraproject.org";

#[derive(Debug, Deserialize)]
pub struct PullRequestsResponse {
    pub requests: Vec<PullRequest>,
    #[serde(default)]
    pub total_requests: u64,
    #[serde(default)]
    pub pagination: Option<Pagination>,
}

#[derive(Debug, Deserialize)]
pub struct Pagination {
    #[serde(default)]
    pub pages: u64,
    #[serde(default)]
    pub per_page: u64,
}

/// A Pagure pull request.
#[derive(Debug, Deserialize)]
pub struct PullRequest {
    pub id: u64,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub date_created: Option<String>,
    #[serde(default)]
    pub last_updated: Option<String>,
    #[serde(default)]
    pub project: Option<PullRequestProject>,
}

#[derive(Debug, Deserialize)]
pub struct PullRequestProject {
    pub fullname: String,
}

/// A project returned by the Pagure projects or group endpoints.
#[derive(Debug, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    /// Namespaced path (e.g. `rpms/python-foo`,
    /// `container/python-classroom`, `forks/user/rpms/python-foo`).
    /// `None` only when the API omits it.
    #[serde(default)]
    pub fullname: Option<String>,
    #[serde(default)]
    pub access_users: AccessUsers,
    #[serde(default)]
    pub access_groups: AccessGroups,
}

/// Keep only projects in the `rpms/` namespace. Group listings
/// include everything a group can access — `container/`,
/// `tests/`, `modules/` projects and forks, all reported under
/// their bare `name` — and unlike the projects endpoint, the
/// group endpoint honors neither `namespace=` nor `fork=false`.
/// Projects without a fullname are kept (the API always sends
/// one; only minimal test fixtures omit it).
pub fn retain_rpms_namespace(projects: &mut Vec<ProjectInfo>) {
    projects.retain(|p| p.fullname.as_deref().is_none_or(|f| f.starts_with("rpms/")));
}

#[derive(Debug, Deserialize)]
struct UserProjectsResponse {
    projects: Vec<ProjectInfo>,
    #[serde(default)]
    pagination: Option<Pagination>,
}

#[derive(Debug, Deserialize)]
struct GroupProjectsResponse {
    projects: Vec<ProjectInfo>,
    #[serde(default)]
    pagination: Option<Pagination>,
}

/// Remove duplicate projects by name, keeping the first occurrence.
pub fn dedup_projects(projects: &mut Vec<ProjectInfo>) {
    let mut seen = std::collections::HashSet::new();
    projects.retain(|p| seen.insert(p.name.clone()));
}

/// Reject an identifier that could shape or escape a URL path
/// before it is interpolated into a request URL.
///
/// Package, branch, user, and group names are bare dist-git tokens
/// (`[A-Za-z0-9._+-]`). Anything else — most importantly a path
/// separator (`/`) or a `.`/`..` parent-directory segment, which
/// URL normalization could turn into a request against a different
/// resource — is refused. This matters because some of these
/// values arrive from API responses (e.g. a Bugzilla component
/// name), not just local config.
fn validate_segment(value: &str, what: &str) -> Result<(), Box<dyn std::error::Error>> {
    let safe = !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
    if safe {
        Ok(())
    } else {
        Err(format!(
            "invalid {what} '{value}': expected a bare dist-git name \
             (letters, digits, '.', '_', '+', '-')"
        )
        .into())
    }
}

#[derive(Clone)]
pub struct DistGitClient {
    base_url: String,
    client: Client,
    api_token: Option<String>,
}

impl Default for DistGitClient {
    fn default() -> Self {
        Self::new()
    }
}

impl DistGitClient {
    pub fn new() -> Self {
        sandogasa_cli::install_crypto_provider();

        Self {
            base_url: DISTGIT_BASE.to_string(),
            client: build_http_client(),
            api_token: None,
        }
    }

    pub fn with_base_url(base_url: &str) -> Self {
        sandogasa_cli::install_crypto_provider();

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: build_http_client(),
            api_token: None,
        }
    }

    pub fn with_token(mut self, token: String) -> Self {
        self.api_token = Some(token);
        self
    }

    /// Fetch the spec file for a package on a given dist-git branch.
    ///
    /// `package` is the source RPM name (e.g. "pcem").
    /// `branch` is the dist-git branch (e.g. "rawhide", "f43", "epel9").
    pub async fn fetch_spec(
        &self,
        package: &str,
        branch: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        validate_segment(package, "package name")?;
        validate_segment(branch, "branch name")?;
        let url = format!(
            "{}/rpms/{}/raw/{}/f/{}.spec",
            self.base_url, package, branch, package
        );
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let body = resp.text().await?;
        Ok(body)
    }

    /// Check whether a package is retired on a given dist-git
    /// branch by looking for the `dead.package` marker file
    /// Fedora uses to mark a retired branch. Returns `Ok(true)`
    /// when the file is present, `Ok(false)` on 404 (no marker,
    /// i.e. live), and surfaces other HTTP errors.
    pub async fn is_retired(
        &self,
        package: &str,
        branch: &str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        validate_segment(package, "package name")?;
        validate_segment(branch, "branch name")?;
        let url = format!(
            "{}/rpms/{}/raw/{}/f/dead.package",
            self.base_url, package, branch
        );
        let resp = self.client.get(&url).send().await?;
        match resp.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            other => Err(format!("dist-git GET {url} returned {other}").into()),
        }
    }

    /// List the git branches of an RPM package (e.g. `rawhide`,
    /// `f43`, `epel9`). Returns the branch names as Pagure reports
    /// them, in the order given by the API. Errors when the
    /// project doesn't exist; use [`Self::project_branches`] to
    /// treat that as a signal instead.
    pub async fn list_branches(
        &self,
        package: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        self.project_branches(package)
            .await?
            .ok_or_else(|| format!("no such package: rpms/{package}").into())
    }

    /// Like [`Self::list_branches`], but returns `Ok(None)` when
    /// the project doesn't exist (HTTP 404) — callers use this to
    /// distinguish "package gone from dist-git" from transport
    /// failures.
    pub async fn project_branches(
        &self,
        package: &str,
    ) -> Result<Option<Vec<String>>, Box<dyn std::error::Error>> {
        validate_segment(package, "package name")?;
        let url = format!("{}/api/0/rpms/{}/git/branches", self.base_url, package);
        let resp = self.client.get(&url).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = resp.error_for_status()?;
        #[derive(serde::Deserialize)]
        struct Branches {
            branches: Vec<String>,
        }
        let body: Branches = resp.json().await?;
        Ok(Some(body.branches))
    }

    /// Fetch ACLs for an RPM package.
    pub async fn get_acls(&self, package: &str) -> Result<ProjectAcls, Box<dyn std::error::Error>> {
        validate_segment(package, "package name")?;
        let url = format!("{}/api/0/rpms/{}", self.base_url, package);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let acls: ProjectAcls = resp.json().await?;
        Ok(acls)
    }

    /// Fetch contributors for an RPM package.
    ///
    /// Unlike `get_acls`, this includes branch patterns for collaborators.
    pub async fn get_contributors(
        &self,
        package: &str,
    ) -> Result<Contributors, Box<dyn std::error::Error>> {
        validate_segment(package, "package name")?;
        let url = format!("{}/api/0/rpms/{}/contributors", self.base_url, package);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let contributors: Contributors = resp.json().await?;
        Ok(contributors)
    }

    /// Set an ACL for a user or group on an RPM package.
    ///
    /// `user_type` must be `"user"` or `"group"`.
    /// `acl` is one of `"ticket"`, `"collaborator"`, `"commit"`, `"admin"`.
    pub async fn set_acl(
        &self,
        package: &str,
        user_type: &str,
        name: &str,
        acl: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        validate_segment(package, "package name")?;
        let url = format!("{}/api/0/rpms/{}/git/modifyacls", self.base_url, package);
        let form = [("user_type", user_type), ("name", name), ("acl", acl)];
        self.auth(self.client.post(&url))
            .form(&form)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Remove all ACLs for a user or group on an RPM package.
    pub async fn remove_acl(
        &self,
        package: &str,
        user_type: &str,
        name: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        validate_segment(package, "package name")?;
        let url = format!("{}/api/0/rpms/{}/git/modifyacls", self.base_url, package);
        let form = [("user_type", user_type), ("name", name), ("acl", "")];
        self.auth(self.client.post(&url))
            .form(&form)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Verify the API token and return the authenticated username.
    pub async fn verify_token(&self) -> Result<String, Box<dyn std::error::Error>> {
        let url = format!("{}/api/0/-/whoami", self.base_url);
        let resp = self
            .auth(self.client.post(&url))
            .send()
            .await?
            .error_for_status()?;
        #[derive(serde::Deserialize)]
        struct WhoAmI {
            username: String,
        }
        let whoami: WhoAmI = resp.json().await?;
        Ok(whoami.username)
    }

    /// Transfer ownership of an RPM package to another user.
    ///
    /// To orphan a package, pass `"orphan"` as `new_owner`.
    pub async fn give_package(
        &self,
        package: &str,
        new_owner: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        validate_segment(package, "package name")?;
        let url = format!("{}/api/0/rpms/{}", self.base_url, package);
        let form = [("main_admin", new_owner)];
        self.auth(self.client.patch(&url))
            .form(&form)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Check whether a user exists on the Pagure instance.
    pub async fn user_exists(&self, username: &str) -> Result<bool, Box<dyn std::error::Error>> {
        validate_segment(username, "username")?;
        let url = format!("{}/api/0/user/{}", self.base_url, username);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }

    /// Check whether a group exists on the Pagure instance.
    pub async fn group_exists(&self, group: &str) -> Result<bool, Box<dyn std::error::Error>> {
        validate_segment(group, "group name")?;
        let url = format!("{}/api/0/group/{}", self.base_url, group);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }

    /// Fetch members of a Pagure group.
    pub async fn get_group_members(
        &self,
        group: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        validate_segment(group, "group name")?;
        let url = format!("{}/api/0/group/{}", self.base_url, group);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        #[derive(serde::Deserialize)]
        struct GroupInfo {
            members: Vec<String>,
        }
        let info: GroupInfo = resp.json().await?;
        Ok(info.members)
    }

    /// Check whether a user has at least `required` access on a
    /// package, considering both direct and group membership.
    pub async fn check_access(
        &self,
        acls: &ProjectAcls,
        username: &str,
        required: AccessLevel,
    ) -> Result<AccessResult, Box<dyn std::error::Error>> {
        // Check direct access first
        if let Some(level) = acls.user_level(username)
            && level >= required
        {
            return Ok(AccessResult::Direct(level));
        }

        // Check groups with sufficient access
        let candidates = acls.groups_with_level(required);
        for (group, level) in candidates {
            let members = self.get_group_members(group).await?;
            if members.iter().any(|m| m == username) {
                return Ok(AccessResult::ViaGroup {
                    level,
                    group: group.to_string(),
                });
            }
        }

        Ok(AccessResult::Insufficient {
            level: acls.user_level(username),
        })
    }

    /// Fetch a user's activity stats (actions per day).
    ///
    /// Returns a map of date strings (YYYY-MM-DD) to action counts.
    pub async fn user_activity_stats(
        &self,
        username: &str,
    ) -> Result<HashMap<String, u64>, Box<dyn std::error::Error>> {
        validate_segment(username, "username")?;
        let url = format!("{}/api/0/user/{}/activity/stats", self.base_url, username);
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let stats: HashMap<String, u64> = resp.json().await?;
        Ok(stats)
    }

    /// Fetch pull requests filed by a user.
    ///
    /// `status` can be "all", "Open", "Merged", or "Closed".
    pub async fn user_pull_requests(
        &self,
        username: &str,
        status: &str,
        limit: u32,
    ) -> Result<Vec<PullRequest>, Box<dyn std::error::Error>> {
        validate_segment(username, "username")?;
        let url = format!(
            "{}/api/0/user/{}/requests/filed?per_page={}&status={}&page=1",
            self.base_url, username, limit, status
        );
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let data: PullRequestsResponse = resp.json().await?;
        Ok(data.requests)
    }

    /// Fetch pull requests actionable by a user (awaiting their review).
    ///
    /// Returns up to `limit` PRs and an estimate of the total count
    /// (derived from pagination metadata, since `total_requests` is
    /// capped to the page size by the Pagure API).
    pub async fn user_actionable_pull_requests(
        &self,
        username: &str,
        limit: u32,
    ) -> Result<(Vec<PullRequest>, u64), Box<dyn std::error::Error>> {
        validate_segment(username, "username")?;
        let url = format!(
            "{}/api/0/user/{}/requests/actionable?per_page={}&page=1",
            self.base_url, username, limit
        );
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let data: PullRequestsResponse = resp.json().await?;
        // Pagure's total_requests is capped to per_page; use pagination
        // metadata for a better estimate.
        let total = match data.pagination {
            Some(p) if p.pages > 1 => {
                // Last page may be partial, but pages * per_page is
                // a reasonable upper bound.
                p.pages * p.per_page
            }
            _ => data.requests.len() as u64,
        };
        Ok((data.requests, total))
    }

    /// Fetch all RPM packages a user has access to.
    ///
    /// Paginates automatically, returning all pages. Retries on
    /// transient server errors (502/503/504). Deduplicates by name.
    /// An optional `pattern` filters by project name (supports `*`
    /// wildcards, e.g. `"python-*"`).
    ///
    /// `fork=false` is essential: without it Pagure includes the
    /// user's *forks* in the listing, and a fork is reported under
    /// its bare package name with the user as `owner` — making
    /// `forks/<user>/rpms/<pkg>` indistinguishable from a real
    /// `rpms/<pkg>` the user owns.
    pub async fn user_projects(
        &self,
        username: &str,
        per_page: u32,
        pattern: Option<&str>,
    ) -> Result<Vec<ProjectInfo>, Box<dyn std::error::Error>> {
        validate_segment(username, "username")?;
        let mut all = Vec::new();
        let mut page = 1u64;
        let pattern_param = pattern.map(|p| format!("&pattern={p}")).unwrap_or_default();
        loop {
            let url = format!(
                "{}/api/0/projects?namespace=rpms&fork=false&username={}&per_page={}&page={}{}",
                self.base_url, username, per_page, page, pattern_param
            );
            eprint!("\r  fetching page {page}...");
            let resp = self.get_with_retry(&url).await?;
            let data: UserProjectsResponse = resp.json().await?;
            all.extend(data.projects);
            match data.pagination {
                Some(ref p) if page < p.pages => page += 1,
                _ => break,
            }
        }
        dedup_projects(&mut all);
        eprintln!("\r  fetched {} package(s)", all.len());
        Ok(all)
    }

    /// Fetch a user's directly-maintained RPM packages from the
    /// Pagure owner-alias dump (`/extras/pagure_owner_alias.json`)
    /// — a single ~3 MB request covering every rpm, instead of the
    /// expensive per-user project scan.
    ///
    /// Trade-off: the dump records only direct owner/admin/commit
    /// maintainers. Collaborator- and ticket-level grants are NOT
    /// included, and neither is group-derived access. The returned
    /// `ProjectInfo`s list the user under `commit` (the dump
    /// doesn't distinguish levels).
    pub async fn user_packages_fast(
        &self,
        username: &str,
    ) -> Result<Vec<ProjectInfo>, Box<dyn std::error::Error>> {
        validate_segment(username, "username")?;
        let url = format!("{}/extras/pagure_owner_alias.json", self.base_url);
        eprintln!("  fetching owner-alias dump...");
        let resp = self.get_with_retry(&url).await?;
        #[derive(serde::Deserialize)]
        struct OwnerAlias {
            rpms: std::collections::BTreeMap<String, Vec<String>>,
        }
        let data: OwnerAlias = resp.json().await?;
        Ok(data
            .rpms
            .into_iter()
            .filter(|(_, users)| users.iter().any(|u| u == username))
            .map(|(name, _)| ProjectInfo {
                fullname: Some(format!("rpms/{name}")),
                name,
                access_users: AccessUsers {
                    commit: vec![username.to_string()],
                    ..Default::default()
                },
                access_groups: AccessGroups::default(),
            })
            .collect())
    }

    /// Fetch all RPM packages a group has access to.
    ///
    /// Paginates automatically, returning all pages. Retries on
    /// transient server errors (502/503/504). Deduplicates by name.
    /// An optional `pattern` filters by project name (supports `*`
    /// wildcards, e.g. `"python-*"`).
    pub async fn group_projects(
        &self,
        group: &str,
        per_page: u32,
        pattern: Option<&str>,
    ) -> Result<Vec<ProjectInfo>, Box<dyn std::error::Error>> {
        validate_segment(group, "group name")?;
        let mut all = Vec::new();
        let mut page = 1u64;
        let pattern_param = pattern.map(|p| format!("&pattern={p}")).unwrap_or_default();
        loop {
            let url = format!(
                "{}/api/0/group/{}?projects=true&per_page={}&page={}{}",
                self.base_url, group, per_page, page, pattern_param
            );
            eprint!("\r  fetching page {page}...");
            let resp = self.get_with_retry(&url).await?;
            let data: GroupProjectsResponse = resp.json().await?;
            all.extend(data.projects);
            match data.pagination {
                Some(ref p) if page < p.pages => page += 1,
                _ => break,
            }
        }
        let fetched = all.len();
        retain_rpms_namespace(&mut all);
        let dropped = fetched - all.len();
        dedup_projects(&mut all);
        if dropped > 0 {
            eprintln!(
                "\r  fetched {} package(s) ({dropped} non-rpms \
                 project(s) skipped)",
                all.len()
            );
        } else {
            eprintln!("\r  fetched {} package(s)", all.len());
        }
        Ok(all)
    }

    /// GET with retry on transient server errors (500/502/503/504).
    async fn get_with_retry(
        &self,
        url: &str,
    ) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
        let mut last_err = None;
        for attempt in 0..=3u32 {
            // Transport failures (connection reset, timeout, DNS)
            // are just as transient as a 5xx — retry them with the
            // same backoff instead of aborting the whole sync.
            let problem = match self.client.get(url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status == reqwest::StatusCode::INTERNAL_SERVER_ERROR
                        || status == reqwest::StatusCode::BAD_GATEWAY
                        || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
                        || status == reqwest::StatusCode::GATEWAY_TIMEOUT
                    {
                        format!("{status}")
                    } else {
                        return Ok(resp.error_for_status()?);
                    }
                }
                Err(e) => format!("{e}"),
            };
            let delay = std::time::Duration::from_secs(1 << attempt);
            eprintln!(
                "  {problem}, retrying in {}s ({}/3)",
                delay.as_secs(),
                attempt + 1,
            );
            tokio::time::sleep(delay).await;
            last_err = Some(format!("{problem} for {url}"));
        }
        Err(last_err.unwrap().into())
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref token) = self.api_token {
            req.header("Authorization", format!("token {token}"))
        } else {
            req
        }
    }
}

/// Upper bound on any single HTTP request — a hang-catcher rather than
/// a latency cap. reqwest's default client has *no* timeout, so a hung
/// connection would otherwise block forever.
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Build the crate's HTTP client with the standard request timeout.
/// Panics only where `Client::new()` would too (TLS backend init).
fn build_http_client() -> Client {
    Client::builder()
        .timeout(DEFAULT_TIMEOUT)
        .build()
        .expect("build reqwest client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::{AccessGroups, AccessUsers};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn new_uses_default_base_url() {
        let client = DistGitClient::new();
        assert_eq!(client.base_url, "https://src.fedoraproject.org");
    }

    #[test]
    fn with_base_url_trims_trailing_slash() {
        let client = DistGitClient::with_base_url("http://localhost:8080/");
        assert_eq!(client.base_url, "http://localhost:8080");
    }

    #[test]
    fn with_base_url_preserves_url_without_trailing_slash() {
        let client = DistGitClient::with_base_url("http://localhost:8080");
        assert_eq!(client.base_url, "http://localhost:8080");
    }

    #[test]
    fn new_has_no_token_by_default() {
        let client = DistGitClient::new();
        assert!(client.api_token.is_none());
    }

    #[test]
    fn with_token_sets_token() {
        let client = DistGitClient::new().with_token("secret".to_string());
        assert_eq!(client.api_token.as_deref(), Some("secret"));
    }

    // ---- get_acls ----

    #[tokio::test]
    async fn get_acls_returns_parsed_acls() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/rpms/freerdp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_users": {
                    "owner": ["ngompa"],
                    "admin": ["salimma"],
                    "commit": ["dcavalca"],
                    "collaborator": [],
                    "ticket": []
                },
                "access_groups": {
                    "admin": [],
                    "commit": ["kde-sig"],
                    "collaborator": [],
                    "ticket": []
                },
                "name": "freerdp",
                "namespace": "rpms"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let acls = client.get_acls("freerdp").await.unwrap();
        assert_eq!(acls.access_users.owner, vec!["ngompa"]);
        assert_eq!(acls.access_users.admin, vec!["salimma"]);
        assert_eq!(acls.access_users.commit, vec!["dcavalca"]);
        assert_eq!(acls.access_groups.commit, vec!["kde-sig"]);
    }

    #[tokio::test]
    async fn get_acls_returns_error_on_404() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/rpms/nonexistent"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let result = client.get_acls("nonexistent").await;
        assert!(result.is_err());
    }

    // ---- get_contributors ----

    #[tokio::test]
    async fn get_contributors_returns_collaborator_branches() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/rpms/python-zope-testing/contributors"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "groups": {
                    "admin": [],
                    "collaborators": [
                        {"branches": "epel*", "user": "btrfs-sig"}
                    ],
                    "commit": [],
                    "ticket": []
                },
                "users": {
                    "admin": ["orion"],
                    "collaborators": [],
                    "commit": [],
                    "ticket": []
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let contribs = client
            .get_contributors("python-zope-testing")
            .await
            .unwrap();
        assert_eq!(contribs.users.admin, vec!["orion"]);
        assert_eq!(contribs.groups.collaborators.len(), 1);
        assert_eq!(contribs.groups.collaborators[0].name(), "btrfs-sig");
        assert_eq!(contribs.groups.collaborators[0].branches(), Some("epel*"));
    }

    #[tokio::test]
    async fn get_contributors_returns_error_on_404() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/rpms/nonexistent/contributors"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let result = client.get_contributors("nonexistent").await;
        assert!(result.is_err());
    }

    // ---- set_acl ----

    #[tokio::test]
    async fn set_acl_sends_post_with_form_data() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri()).with_token("mytoken".to_string());

        Mock::given(method("POST"))
            .and(path("/api/0/rpms/freerdp/git/modifyacls"))
            .and(header("Authorization", "token mytoken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "ACL updated"
            })))
            .expect(1)
            .mount(&server)
            .await;

        client
            .set_acl("freerdp", "user", "salimma", "commit")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn set_acl_group() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri()).with_token("tok".to_string());

        Mock::given(method("POST"))
            .and(path("/api/0/rpms/freerdp/git/modifyacls"))
            .and(header("Authorization", "token tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "ACL updated"
            })))
            .expect(1)
            .mount(&server)
            .await;

        client
            .set_acl("freerdp", "group", "kde-sig", "commit")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn set_acl_returns_error_on_403() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri()).with_token("badtoken".to_string());

        Mock::given(method("POST"))
            .and(path("/api/0/rpms/freerdp/git/modifyacls"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let result = client.set_acl("freerdp", "user", "salimma", "commit").await;
        assert!(result.is_err());
    }

    // ---- remove_acl ----

    #[tokio::test]
    async fn remove_acl_sends_empty_acl() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri()).with_token("mytoken".to_string());

        Mock::given(method("POST"))
            .and(path("/api/0/rpms/freerdp/git/modifyacls"))
            .and(header("Authorization", "token mytoken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "ACL updated"
            })))
            .expect(1)
            .mount(&server)
            .await;

        client
            .remove_acl("freerdp", "user", "olduser")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn remove_acl_returns_error_on_server_failure() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri()).with_token("tok".to_string());

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let result = client.remove_acl("freerdp", "group", "old-group").await;
        assert!(result.is_err());
    }

    // ---- verify_token ----

    #[tokio::test]
    async fn verify_token_returns_username() {
        let server = MockServer::start().await;
        let client =
            DistGitClient::with_base_url(&server.uri()).with_token("goodtoken".to_string());

        Mock::given(method("POST"))
            .and(path("/api/0/-/whoami"))
            .and(header("Authorization", "token goodtoken"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"username": "salimma"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let username = client.verify_token().await.unwrap();
        assert_eq!(username, "salimma");
    }

    #[tokio::test]
    async fn verify_token_returns_error_on_401() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri()).with_token("badtoken".to_string());

        Mock::given(method("POST"))
            .and(path("/api/0/-/whoami"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let result = client.verify_token().await;
        assert!(result.is_err());
    }

    // ---- get_group_members ----

    #[tokio::test]
    async fn get_group_members_returns_members() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/group/kde-sig"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "display_name": "KDE SIG",
                "description": "KDE Special Interest Group",
                "creator": {"name": "ngompa"},
                "date_created": "1234567890",
                "group_type": "user",
                "members": ["ngompa", "salimma", "dcavalca"],
                "name": "kde-sig"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let members = client.get_group_members("kde-sig").await.unwrap();
        assert_eq!(members, vec!["ngompa", "salimma", "dcavalca"]);
    }

    #[tokio::test]
    async fn get_group_members_returns_error_on_404() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/group/nonexistent"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let result = client.get_group_members("nonexistent").await;
        assert!(result.is_err());
    }

    // ---- check_access ----

    #[tokio::test]
    async fn check_access_direct_admin() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec![],
                admin: vec!["salimma".to_string()],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
        };

        let result = client
            .check_access(&acls, "salimma", AccessLevel::Admin)
            .await
            .unwrap();
        assert!(result.is_sufficient());
        assert!(matches!(result, AccessResult::Direct(AccessLevel::Admin)));
    }

    #[tokio::test]
    async fn check_access_direct_owner_satisfies_admin() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec!["ngompa".to_string()],
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
        };

        let result = client
            .check_access(&acls, "ngompa", AccessLevel::Admin)
            .await
            .unwrap();
        assert!(result.is_sufficient());
        assert!(matches!(result, AccessResult::Direct(AccessLevel::Owner)));
    }

    #[tokio::test]
    async fn check_access_via_group() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/group/python-packagers-sig"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "members": ["salimma", "dcavalca"],
                "name": "python-packagers-sig"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec![],
                admin: vec![],
                commit: vec!["salimma".to_string()],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec!["python-packagers-sig".to_string()],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
        };

        let result = client
            .check_access(&acls, "salimma", AccessLevel::Admin)
            .await
            .unwrap();
        assert!(result.is_sufficient());
        match result {
            AccessResult::ViaGroup { level, group } => {
                assert_eq!(level, AccessLevel::Admin);
                assert_eq!(group, "python-packagers-sig");
            }
            _ => panic!("expected ViaGroup"),
        }
    }

    #[tokio::test]
    async fn check_access_insufficient_with_lower_level() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec![],
                admin: vec![],
                commit: vec!["salimma".to_string()],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
        };

        let result = client
            .check_access(&acls, "salimma", AccessLevel::Admin)
            .await
            .unwrap();
        assert!(!result.is_sufficient());
        match result {
            AccessResult::Insufficient { level } => {
                assert_eq!(level, Some(AccessLevel::Commit));
            }
            _ => panic!("expected Insufficient"),
        }
    }

    #[tokio::test]
    async fn check_access_no_access_at_all() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec!["ngompa".to_string()],
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
        };

        let result = client
            .check_access(&acls, "unknown", AccessLevel::Admin)
            .await
            .unwrap();
        assert!(!result.is_sufficient());
        match result {
            AccessResult::Insufficient { level } => {
                assert_eq!(level, None);
            }
            _ => panic!("expected Insufficient"),
        }
    }

    #[tokio::test]
    async fn check_access_via_group_not_member() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/group/python-packagers-sig"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "members": ["dcavalca"],
                "name": "python-packagers-sig"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec![],
                admin: vec![],
                commit: vec!["salimma".to_string()],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec!["python-packagers-sig".to_string()],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
        };

        let result = client
            .check_access(&acls, "salimma", AccessLevel::Admin)
            .await
            .unwrap();
        assert!(!result.is_sufficient());
        match result {
            AccessResult::Insufficient { level } => {
                assert_eq!(level, Some(AccessLevel::Commit));
            }
            _ => panic!("expected Insufficient"),
        }
    }

    // ---- give_package ----

    #[tokio::test]
    async fn give_package_sends_patch_with_main_admin() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri()).with_token("mytoken".to_string());

        Mock::given(method("PATCH"))
            .and(path("/api/0/rpms/freerdp"))
            .and(header("Authorization", "token mytoken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "Project \"freerdp\" has been given to \"dcavalca\""
            })))
            .expect(1)
            .mount(&server)
            .await;

        client.give_package("freerdp", "dcavalca").await.unwrap();
    }

    #[tokio::test]
    async fn give_package_orphan() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri()).with_token("tok".to_string());

        Mock::given(method("PATCH"))
            .and(path("/api/0/rpms/freerdp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "Project \"freerdp\" has been given to \"orphan\""
            })))
            .expect(1)
            .mount(&server)
            .await;

        client.give_package("freerdp", "orphan").await.unwrap();
    }

    #[tokio::test]
    async fn give_package_returns_error_on_403() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri()).with_token("bad".to_string());

        Mock::given(method("PATCH"))
            .and(path("/api/0/rpms/freerdp"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let result = client.give_package("freerdp", "dcavalca").await;
        assert!(result.is_err());
    }

    // ---- user_exists ----

    #[tokio::test]
    async fn user_exists_returns_true_for_existing_user() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/user/salimma"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "user": {"name": "salimma"}
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client.user_exists("salimma").await.unwrap());
    }

    #[tokio::test]
    async fn user_exists_returns_false_for_missing_user() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/user/nonexistent"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        assert!(!client.user_exists("nonexistent").await.unwrap());
    }

    // ---- user_activity_stats ----

    #[tokio::test]
    async fn user_activity_stats_returns_map() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/user/salimma/activity/stats"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "2025-01-15": 5,
                "2025-01-16": 12,
                "2025-01-17": 0
            })))
            .expect(1)
            .mount(&server)
            .await;

        let stats = client.user_activity_stats("salimma").await.unwrap();
        assert_eq!(stats.get("2025-01-15"), Some(&5));
        assert_eq!(stats.get("2025-01-16"), Some(&12));
        assert_eq!(stats.get("2025-01-17"), Some(&0));
    }

    #[tokio::test]
    async fn user_activity_stats_returns_error_on_404() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/user/nonexistent/activity/stats"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let result = client.user_activity_stats("nonexistent").await;
        assert!(result.is_err());
    }

    // ---- user_pull_requests ----

    #[tokio::test]
    async fn user_pull_requests_returns_prs() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/user/salimma/requests/filed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "requests": [
                    {
                        "id": 1,
                        "title": "Update to 2.0",
                        "status": "Open",
                        "date_created": "1700000000",
                        "last_updated": "1700001000",
                        "project": {"fullname": "rpms/freerdp"}
                    },
                    {
                        "id": 2,
                        "title": "Fix FTBFS",
                        "status": "Merged",
                        "date_created": "1700002000",
                        "last_updated": "1700003000",
                        "project": {"fullname": "rpms/pcem"}
                    }
                ],
                "total_requests": 2
            })))
            .expect(1)
            .mount(&server)
            .await;

        let prs = client
            .user_pull_requests("salimma", "all", 10)
            .await
            .unwrap();
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].id, 1);
        assert_eq!(prs[0].title, "Update to 2.0");
        assert_eq!(prs[0].status, "Open");
        assert_eq!(prs[0].project.as_ref().unwrap().fullname, "rpms/freerdp");
        assert_eq!(prs[1].id, 2);
        assert_eq!(prs[1].status, "Merged");
    }

    #[tokio::test]
    async fn user_pull_requests_returns_error_on_404() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/user/nonexistent/requests/filed"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let result = client.user_pull_requests("nonexistent", "all", 10).await;
        assert!(result.is_err());
    }

    // ---- user_actionable_pull_requests ----

    #[tokio::test]
    async fn user_actionable_pull_requests_single_page() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/user/salimma/requests/actionable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "requests": [
                    {
                        "id": 10,
                        "title": "Review needed",
                        "status": "Open"
                    }
                ],
                "total_requests": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let (prs, total) = client
            .user_actionable_pull_requests("salimma", 50)
            .await
            .unwrap();
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].id, 10);
        assert_eq!(total, 1);
    }

    #[tokio::test]
    async fn user_actionable_pull_requests_with_pagination() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/user/salimma/requests/actionable"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "requests": [
                    {"id": 1, "title": "PR 1", "status": "Open"},
                    {"id": 2, "title": "PR 2", "status": "Open"}
                ],
                "total_requests": 2,
                "pagination": {
                    "pages": 5,
                    "per_page": 2
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let (prs, total) = client
            .user_actionable_pull_requests("salimma", 2)
            .await
            .unwrap();
        assert_eq!(prs.len(), 2);
        // 5 pages * 2 per_page = 10 estimated total
        assert_eq!(total, 10);
    }

    #[tokio::test]
    async fn user_actionable_pull_requests_returns_error_on_server_failure() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/user/salimma/requests/actionable"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let result = client.user_actionable_pull_requests("salimma", 50).await;
        assert!(result.is_err());
    }

    // ---- fetch_spec ----

    #[tokio::test]
    async fn fetch_spec_returns_spec_content() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        let spec_content = "Name: pcem\nVersion: 17\nRelease: 1%{?dist}\nSummary: PC emulator\n";
        Mock::given(method("GET"))
            .and(path("/rpms/pcem/raw/rawhide/f/pcem.spec"))
            .respond_with(ResponseTemplate::new(200).set_body_string(spec_content))
            .expect(1)
            .mount(&server)
            .await;

        let body = client.fetch_spec("pcem", "rawhide").await.unwrap();
        assert!(body.contains("Name: pcem"));
        assert!(body.contains("Version: 17"));
    }

    #[tokio::test]
    async fn fetch_spec_returns_error_on_404() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rpms/nonexistent/raw/rawhide/f/nonexistent.spec"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let result = client.fetch_spec("nonexistent", "rawhide").await;
        assert!(result.is_err());
    }

    // ---- is_retired ----

    #[tokio::test]
    async fn is_retired_true_when_dead_package_present() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(path("/rpms/old-pkg/raw/rawhide/f/dead.package"))
            .respond_with(ResponseTemplate::new(200).set_body_string("retired upstream\n"))
            .mount(&server)
            .await;
        assert!(client.is_retired("old-pkg", "rawhide").await.unwrap());
    }

    #[tokio::test]
    async fn is_retired_false_on_404() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(path("/rpms/live-pkg/raw/rawhide/f/dead.package"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        assert!(!client.is_retired("live-pkg", "rawhide").await.unwrap());
    }

    #[tokio::test]
    async fn is_retired_branch_aware() {
        // A package retired only on epel10 should not look
        // retired when we query rawhide.
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(path("/rpms/foo/raw/rawhide/f/dead.package"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rpms/foo/raw/epel10/f/dead.package"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;
        assert!(!client.is_retired("foo", "rawhide").await.unwrap());
        assert!(client.is_retired("foo", "epel10").await.unwrap());
    }

    #[tokio::test]
    async fn is_retired_surfaces_other_errors() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(path("/rpms/foo/raw/rawhide/f/dead.package"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        assert!(client.is_retired("foo", "rawhide").await.is_err());
    }

    // ---- validate_segment ----

    #[test]
    fn validate_segment_accepts_real_names() {
        for name in [
            "freerdp",
            "python-django3",
            "gtk+",
            "rust-nu_cli",
            "f43",
            "epel9",
            "rawhide",
        ] {
            assert!(
                validate_segment(name, "test").is_ok(),
                "{name} should be valid"
            );
        }
    }

    #[test]
    fn validate_segment_rejects_path_shaping() {
        for bad in [
            "", ".", "..", "foo/bar", "../etc", "a b", "x&y=z", "pkg?q", "%2e%2e",
        ] {
            assert!(
                validate_segment(bad, "test").is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[tokio::test]
    async fn is_retired_rejects_traversal_package() {
        // Never reaches the network — rejected before the request.
        let client = DistGitClient::with_base_url("https://src.fedoraproject.org");
        assert!(client.is_retired("..", "rawhide").await.is_err());
        assert!(client.is_retired("foo/bar", "rawhide").await.is_err());
    }

    #[tokio::test]
    async fn list_branches_rejects_traversal_package() {
        let client = DistGitClient::with_base_url("https://src.fedoraproject.org");
        assert!(client.list_branches("../../admin").await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn get_with_retry_retries_transport_errors() {
        // Nothing listens on port 1: every attempt fails at the
        // transport level. The old code aborted on the first
        // connection error; now it retries and eventually returns
        // an error mentioning the URL. Paused time fast-forwards
        // the backoff sleeps.
        let client = DistGitClient::with_base_url("http://127.0.0.1:1");
        let result = client.user_projects("salimma", 100, None).await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("127.0.0.1:1"), "unexpected error: {err}");
    }

    // ---- user_packages_fast ----

    #[tokio::test]
    async fn user_packages_fast_filters_direct_maintainers() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(path("/extras/pagure_owner_alias.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "rpms": {
                    "freerdp": ["salimma", "dcavalca"],
                    "systemd": ["zbyszek"],
                    "pcem": ["salimma"]
                },
                "container": {}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let projects = client.user_packages_fast("salimma").await.unwrap();
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["freerdp", "pcem"]);
        // Direct access is synthesized so --no-groups filtering keeps them.
        assert!(
            projects[0]
                .access_users
                .commit
                .contains(&"salimma".to_string())
        );
    }

    // ---- list_branches ----

    #[tokio::test]
    async fn list_branches_returns_branch_names() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(path("/api/0/rpms/python-django3/git/branches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "branches": ["epel8", "epel9", "rawhide"],
                "total_branches": 3
            })))
            .expect(1)
            .mount(&server)
            .await;
        let branches = client.list_branches("python-django3").await.unwrap();
        assert_eq!(branches, vec!["epel8", "epel9", "rawhide"]);
    }

    #[tokio::test]
    async fn list_branches_errors_on_404() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(path("/api/0/rpms/nonexistent/git/branches"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        assert!(client.list_branches("nonexistent").await.is_err());
    }

    // ---- project_branches ----

    #[tokio::test]
    async fn project_branches_none_on_404() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(path("/api/0/rpms/removed-pkg/git/branches"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        assert!(
            client
                .project_branches("removed-pkg")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn project_branches_surfaces_server_errors() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());
        Mock::given(method("GET"))
            .and(path("/api/0/rpms/flaky-pkg/git/branches"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        assert!(client.project_branches("flaky-pkg").await.is_err());
    }

    // ---- user_projects ----

    fn project_json(name: &str, owner: &str, groups: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "namespace": "rpms",
            "access_users": {
                "owner": [owner],
                "admin": [],
                "commit": [],
                "collaborator": [],
                "ticket": []
            },
            "access_groups": {
                "admin": [],
                "commit": groups,
                "collaborator": [],
                "ticket": []
            }
        })
    }

    #[tokio::test]
    async fn user_projects_single_page() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/projects"))
            .and(query_param("namespace", "rpms"))
            .and(query_param("username", "salimma"))
            // Forks must be excluded: they masquerade as the real
            // package with the user as owner.
            .and(query_param("fork", "false"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projects": [
                    project_json("freerdp", "salimma", &[]),
                    project_json("systemd", "ngompa", &["btrfs-sig"])
                ],
                "pagination": { "pages": 1, "per_page": 100 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let projects = client.user_projects("salimma", 100, None).await.unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].name, "freerdp");
        assert_eq!(projects[1].name, "systemd");
        assert!(
            projects[0]
                .access_users
                .owner
                .contains(&"salimma".to_string())
        );
        assert!(projects[1].access_groups.contains_group("btrfs-sig"));
    }

    #[tokio::test]
    async fn user_projects_paginates() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/projects"))
            .and(query_param("username", "salimma"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projects": [project_json("aaa", "salimma", &[])],
                "pagination": { "pages": 2, "per_page": 100 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/0/projects"))
            .and(query_param("username", "salimma"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projects": [project_json("zzz", "salimma", &[])],
                "pagination": { "pages": 2, "per_page": 100 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let projects = client.user_projects("salimma", 100, None).await.unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].name, "aaa");
        assert_eq!(projects[1].name, "zzz");
    }

    #[tokio::test]
    async fn user_projects_empty() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/projects"))
            .and(query_param("username", "nobody"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "projects": [] })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let projects = client.user_projects("nobody", 100, None).await.unwrap();
        assert!(projects.is_empty());
    }

    #[tokio::test]
    async fn user_projects_retries_on_transient_error() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        // First request fails with 503; the retry succeeds.
        Mock::given(method("GET"))
            .and(path("/api/0/projects"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/api/0/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "projects": [project_json("aaa", "salimma", &[])],
                "pagination": { "pages": 1, "per_page": 100 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let projects = client.user_projects("salimma", 100, None).await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "aaa");
    }

    // ---- group_projects ----

    #[tokio::test]
    async fn group_projects_single_page() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/group/btrfs-sig"))
            .and(query_param("projects", "true"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "members": ["salimma", "dcavalca"],
                "projects": [
                    project_json("systemd", "ngompa", &["btrfs-sig"])
                ],
                "pagination": { "pages": 1, "per_page": 100 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let projects = client.group_projects("btrfs-sig", 100, None).await.unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "systemd");
    }

    #[tokio::test]
    async fn group_projects_keeps_only_rpms_namespace() {
        // A group's project list includes everything it can
        // access — container/tests projects and forks, all
        // reported under their bare name (found live:
        // container/python-classroom imported into a
        // python-packagers-sig inventory as "python-classroom").
        // The group endpoint honors neither namespace= nor
        // fork=false, so the filtering is client-side.
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        let with_fullname = |name: &str, fullname: &str| {
            let mut p = project_json(name, "someone", &["python-packagers-sig"]);
            p["fullname"] = serde_json::json!(fullname);
            p
        };
        Mock::given(method("GET"))
            .and(path("/api/0/group/python-packagers-sig"))
            .and(query_param("projects", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "members": ["salimma"],
                "projects": [
                    with_fullname("python-foo", "rpms/python-foo"),
                    with_fullname("python-classroom", "container/python-classroom"),
                    with_fullname("python-classroom", "tests/python-classroom"),
                    with_fullname("python-foo", "forks/someone/rpms/python-foo"),
                ],
                "pagination": { "pages": 1, "per_page": 100 }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let projects = client
            .group_projects("python-packagers-sig", 100, None)
            .await
            .unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "python-foo");
        assert_eq!(projects[0].fullname.as_deref(), Some("rpms/python-foo"));
    }

    #[tokio::test]
    async fn group_projects_not_found() {
        let server = MockServer::start().await;
        let client = DistGitClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path("/api/0/group/nonexistent"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let result = client.group_projects("nonexistent", 100, None).await;
        assert!(result.is_err());
    }
}
