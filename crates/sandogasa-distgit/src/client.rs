// SPDX-License-Identifier: Apache-2.0 OR MIT

use reqwest::Client;

use std::collections::HashMap;

use serde::Deserialize;

use crate::acl::{AccessLevel, AccessResult, Contributors, ProjectAcls};

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
        Self {
            base_url: DISTGIT_BASE.to_string(),
            client: Client::new(),
            api_token: None,
        }
    }

    pub fn with_base_url(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: Client::new(),
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
        let url = format!(
            "{}/rpms/{}/raw/{}/f/{}.spec",
            self.base_url, package, branch, package
        );
        let resp = self.client.get(&url).send().await?.error_for_status()?;
        let body = resp.text().await?;
        Ok(body)
    }

    /// Fetch ACLs for an RPM package.
    pub async fn get_acls(&self, package: &str) -> Result<ProjectAcls, Box<dyn std::error::Error>> {
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
        let url = format!("{}/api/0/user/{}", self.base_url, username);
        let resp = self.client.get(&url).send().await?;
        Ok(resp.status().is_success())
    }

    /// Fetch members of a Pagure group.
    pub async fn get_group_members(
        &self,
        group: &str,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
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

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref token) = self.api_token {
            req.header("Authorization", format!("token {token}"))
        } else {
            req
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::{AccessGroups, AccessUsers};
    use wiremock::matchers::{header, method, path};
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
                        {"branches": "epel*", "user": "epel-packagers-sig"}
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
        assert_eq!(
            contribs.groups.collaborators[0].name(),
            "epel-packagers-sig"
        );
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
            .and(path("/api/0/group/python-sig"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "members": ["salimma", "dcavalca"],
                "name": "python-sig"
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
                admin: vec!["python-sig".to_string()],
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
                assert_eq!(group, "python-sig");
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
            .and(path("/api/0/group/python-sig"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "members": ["dcavalca"],
                "name": "python-sig"
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
                admin: vec!["python-sig".to_string()],
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
}
