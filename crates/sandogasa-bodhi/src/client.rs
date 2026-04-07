// SPDX-License-Identifier: MPL-2.0

use reqwest::Client;

use crate::models::{
    BodhiRelease, Comment, CommentsResponse, ReleasesResponse, SingleUpdateResponse, Update,
    UpdatesResponse,
};

const BODHI_API_BASE: &str = "https://bodhi.fedoraproject.org";

pub struct BodhiClient {
    base_url: String,
    client: Client,
}

impl Default for BodhiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl BodhiClient {
    pub fn new() -> Self {
        Self {
            base_url: BODHI_API_BASE.to_string(),
            client: Client::new(),
        }
    }

    pub fn with_base_url(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: Client::new(),
        }
    }

    /// Fetch updates for a given package on a given release.
    ///
    /// `package` is the source package name (e.g. "freerdp").
    /// `release` is the Bodhi release name (e.g. "F42", "EPEL-9").
    /// `statuses` filters by update status (e.g. &["stable", "testing"]).
    ///
    /// Returns all matching updates, paginating through all pages.
    pub async fn updates_for_package(
        &self,
        package: &str,
        release: &str,
        statuses: &[&str],
    ) -> Result<Vec<Update>, reqwest::Error> {
        let mut all_updates = Vec::new();
        let mut page = 1;

        loop {
            let status_params: String = statuses.iter().map(|s| format!("&status={s}")).collect();
            let url = format!(
                "{}/updates/?packages={}&releases={}{}&rows_per_page=100&page={}",
                self.base_url, package, release, status_params, page
            );

            let resp: UpdatesResponse = self
                .client
                .get(&url)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            all_updates.extend(resp.updates);

            if page >= resp.pages {
                break;
            }
            page += 1;
        }

        Ok(all_updates)
    }

    /// Fetch active Fedora and EPEL releases from the Bodhi API.
    ///
    /// Returns releases with state "current", "pending", or "frozen",
    /// excluding Flatpak, Container, ELN, and EPEL-Next variants.
    pub async fn active_releases(&self) -> Result<Vec<BodhiRelease>, reqwest::Error> {
        let url = format!("{}/releases/?chrome=0&rows_per_page=100", self.base_url);

        let resp: ReleasesResponse = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let active: Vec<BodhiRelease> = resp
            .releases
            .into_iter()
            .filter(|r| matches!(r.state.as_str(), "current" | "pending" | "frozen"))
            .filter(|r| matches!(r.id_prefix.as_str(), "FEDORA" | "FEDORA-EPEL") && r.name != "ELN")
            .collect();

        Ok(active)
    }

    /// Fetch a single update by its alias (e.g. "FEDORA-EPEL-2026-f9eaa11e18").
    pub async fn update_by_alias(&self, alias: &str) -> Result<Update, reqwest::Error> {
        let url = format!("{}/updates/{}", self.base_url, alias);
        let resp: SingleUpdateResponse = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.update)
    }

    /// Fetch the most recent updates submitted by a user.
    ///
    /// Returns up to `limit` updates, most recent first.
    pub async fn updates_for_user(
        &self,
        username: &str,
        limit: u32,
    ) -> Result<Vec<Update>, reqwest::Error> {
        let url = format!(
            "{}/updates/?user={}&rows_per_page={}&chrome=0",
            self.base_url, username, limit
        );
        let resp: UpdatesResponse = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.updates)
    }

    /// Fetch the most recent comments by a user.
    ///
    /// Returns up to `limit` comments, most recent first.
    pub async fn comments_for_user(
        &self,
        username: &str,
        limit: u32,
    ) -> Result<Vec<Comment>, reqwest::Error> {
        let url = format!(
            "{}/comments/?user={}&rows_per_page={}&chrome=0",
            self.base_url, username, limit
        );
        let resp: CommentsResponse = self
            .client
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.comments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn new_uses_default_base_url() {
        let client = BodhiClient::new();
        assert_eq!(client.base_url, "https://bodhi.fedoraproject.org");
    }

    #[test]
    fn with_base_url_trims_trailing_slash() {
        let client = BodhiClient::with_base_url("http://localhost:8080/");
        assert_eq!(client.base_url, "http://localhost:8080");
    }

    #[tokio::test]
    async fn updates_for_package_single_page() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [
                    {
                        "alias": "FEDORA-2026-abc",
                        "status": "stable",
                        "builds": [{"nvr": "freerdp-3.23.0-1.fc42"}],
                        "bugs": [],
                        "release": {"name": "F42"}
                    }
                ],
                "total": 1,
                "page": 1,
                "pages": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let updates = client
            .updates_for_package("freerdp", "F42", &["stable"])
            .await
            .unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].alias, "FEDORA-2026-abc");
    }

    #[tokio::test]
    async fn updates_for_package_empty() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [],
                "total": 0,
                "page": 1,
                "pages": 0
            })))
            .mount(&server)
            .await;

        let updates = client
            .updates_for_package("nonexistent", "F42", &["stable", "testing"])
            .await
            .unwrap();
        assert!(updates.is_empty());
    }

    #[tokio::test]
    async fn active_releases_filters_correctly() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/releases/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "releases": [
                    {"name": "F43", "branch": "f43", "id_prefix": "FEDORA", "state": "current"},
                    {"name": "EPEL-9", "branch": "epel9", "id_prefix": "FEDORA-EPEL", "state": "current"},
                    {"name": "ELN", "branch": "eln", "id_prefix": "FEDORA", "state": "current"},
                    {"name": "F42F", "branch": "f42f", "id_prefix": "FEDORA-FLATPAK", "state": "current"},
                    {"name": "F40", "branch": "f40", "id_prefix": "FEDORA", "state": "archived"}
                ],
                "total": 5,
                "page": 1,
                "pages": 1
            })))
            .mount(&server)
            .await;

        let releases = client.active_releases().await.unwrap();
        let names: Vec<_> = releases.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["F43", "EPEL-9"]);
    }

    // ---- update_by_alias ----

    #[tokio::test]
    async fn update_by_alias_returns_update() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/FEDORA-EPEL-2026-abc123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "update": {
                    "alias": "FEDORA-EPEL-2026-abc123",
                    "status": "testing",
                    "from_side_tag": "epel9-build-side-133287",
                    "builds": [
                        {"nvr": "rust-uucore-0.0.28-2.el9"}
                    ],
                    "bugs": [],
                    "release": {"name": "EPEL-9"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let update = client
            .update_by_alias("FEDORA-EPEL-2026-abc123")
            .await
            .unwrap();
        assert_eq!(update.alias, "FEDORA-EPEL-2026-abc123");
        assert_eq!(
            update.from_side_tag.as_deref(),
            Some("epel9-build-side-133287")
        );
        assert_eq!(update.builds.len(), 1);
        assert_eq!(update.builds[0].nvr, "rust-uucore-0.0.28-2.el9");
    }

    #[tokio::test]
    async fn update_by_alias_without_side_tag() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/FEDORA-2026-xyz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "update": {
                    "alias": "FEDORA-2026-xyz",
                    "status": "stable",
                    "builds": [
                        {"nvr": "foo-1.0-1.fc42"},
                        {"nvr": "bar-2.0-1.fc42"}
                    ],
                    "bugs": [],
                    "release": {"name": "F42"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let update = client.update_by_alias("FEDORA-2026-xyz").await.unwrap();
        assert_eq!(update.alias, "FEDORA-2026-xyz");
        assert!(update.from_side_tag.is_none());
        assert_eq!(update.builds.len(), 2);
    }

    // ---- updates_for_user ----

    #[tokio::test]
    async fn updates_for_user_returns_updates() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [
                    {
                        "alias": "FEDORA-2026-b600f85be9",
                        "status": "testing",
                        "builds": [{"nvr": "python-puzpy-0.5.0-2.fc44"}],
                        "bugs": [],
                        "release": {"name": "F44"},
                        "date_submitted": "2026-03-20 23:44:44"
                    }
                ],
                "total": 1,
                "page": 1,
                "pages": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let updates = client.updates_for_user("salimma", 1).await.unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].alias, "FEDORA-2026-b600f85be9");
        assert_eq!(updates[0].release.as_ref().unwrap().name, "F44");
    }

    #[tokio::test]
    async fn updates_for_user_empty() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [],
                "total": 0,
                "page": 1,
                "pages": 0
            })))
            .mount(&server)
            .await;

        let updates = client.updates_for_user("nobody", 1).await.unwrap();
        assert!(updates.is_empty());
    }

    // ---- comments_for_user ----

    #[tokio::test]
    async fn comments_for_user_returns_comments() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/comments/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "comments": [
                    {
                        "id": 4559905,
                        "text": "Testing feedback",
                        "karma": 1,
                        "timestamp": "2026-02-24 11:17:59",
                        "author": "salimma",
                        "update_alias": "FEDORA-EPEL-2026-8e235e20a2"
                    }
                ],
                "total": 1,
                "page": 1,
                "pages": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let comments = client.comments_for_user("salimma", 1).await.unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].karma, 1);
        assert_eq!(
            comments[0].update_alias.as_deref(),
            Some("FEDORA-EPEL-2026-8e235e20a2")
        );
    }

    #[tokio::test]
    async fn comments_for_user_empty() {
        let server = MockServer::start().await;
        let client = BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/comments/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "comments": [],
                "total": 0,
                "page": 1,
                "pages": 0
            })))
            .mount(&server)
            .await;

        let comments = client.comments_for_user("nobody", 1).await.unwrap();
        assert!(comments.is_empty());
    }
}
