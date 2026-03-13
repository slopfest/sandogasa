// SPDX-License-Identifier: MPL-2.0

use reqwest::Client;

use crate::acl::{Contributors, ProjectAcls};

const DISTGIT_BASE: &str = "https://src.fedoraproject.org";

pub struct DistGitClient {
    base_url: String,
    client: Client,
    api_token: Option<String>,
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
}
