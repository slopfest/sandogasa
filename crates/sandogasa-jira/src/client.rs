// SPDX-License-Identifier: Apache-2.0 OR MIT

use reqwest::Client;

use crate::models::Issue;

/// Minimal JIRA REST v2 client.
///
/// Configured with a base URL like `https://issues.redhat.com`.
/// Public issues work anonymously. For private issues, set a
/// Personal Access Token via [`with_api_key`].
pub struct JiraClient {
    base_url: String,
    client: Client,
    api_key: Option<String>,
}

impl JiraClient {
    /// Construct a new client. The base URL should NOT include the
    /// `/rest/api/2` path — that's added by each endpoint.
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: Client::new(),
            api_key: None,
        }
    }

    /// Attach a Personal Access Token for authenticated requests.
    pub fn with_api_key(mut self, key: String) -> Self {
        self.api_key = Some(key);
        self
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/rest/api/2/{}",
            self.base_url,
            path.trim_start_matches('/')
        )
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref key) = self.api_key {
            req.bearer_auth(key)
        } else {
            req
        }
    }

    /// Fetch a single issue by key (e.g. "RHEL-12345").
    ///
    /// Returns `Ok(None)` for 404 (issue not found or not visible
    /// to the current credentials); `Err` for other errors.
    pub async fn issue(&self, key: &str) -> Result<Option<Issue>, reqwest::Error> {
        let resp = self
            .auth(self.client.get(self.url(&format!("issue/{key}"))))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let issue: Issue = resp.error_for_status()?.json().await?;
        Ok(Some(issue))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn new_trims_trailing_slashes() {
        let client = JiraClient::new("https://issues.redhat.com/");
        assert_eq!(client.base_url, "https://issues.redhat.com");
        let client = JiraClient::new("https://issues.redhat.com///");
        assert_eq!(client.base_url, "https://issues.redhat.com");
    }

    #[test]
    fn url_composes_rest_path() {
        let client = JiraClient::new("https://issues.redhat.com");
        assert_eq!(
            client.url("issue/RHEL-123"),
            "https://issues.redhat.com/rest/api/2/issue/RHEL-123"
        );
    }

    #[tokio::test]
    async fn issue_returns_parsed_issue() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/RHEL-12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "key": "RHEL-12345",
                "fields": {
                    "summary": "CVE-2026-0001 xz: example",
                    "status": {"name": "In Progress"},
                    "resolution": null
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let issue = client.issue("RHEL-12345").await.unwrap().unwrap();
        assert_eq!(issue.key, "RHEL-12345");
        assert_eq!(issue.summary(), "CVE-2026-0001 xz: example");
        assert_eq!(issue.status(), "In Progress");
        assert!(!issue.is_resolved());
        assert_eq!(issue.resolution(), None);
    }

    #[tokio::test]
    async fn issue_returns_resolved() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/RHEL-6789"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "key": "RHEL-6789",
                "fields": {
                    "summary": "Closed issue",
                    "status": {"name": "Closed"},
                    "resolution": {"name": "Done"}
                }
            })))
            .mount(&server)
            .await;

        let issue = client.issue("RHEL-6789").await.unwrap().unwrap();
        assert!(issue.is_resolved());
        assert_eq!(issue.resolution(), Some("Done"));
        assert_eq!(issue.status(), "Closed");
    }

    #[tokio::test]
    async fn issue_returns_none_on_404() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/DOES-NOTEXIST"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let result = client.issue("DOES-NOTEXIST").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn issue_propagates_500_error() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/ANY-1"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let result = client.issue("ANY-1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn with_api_key_sends_bearer_auth() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri()).with_api_key("secret-token".to_string());

        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/RHEL-1"))
            .and(header("authorization", "Bearer secret-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "key": "RHEL-1",
                "fields": {
                    "summary": "",
                    "status": {"name": "New"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let _ = client.issue("RHEL-1").await.unwrap();
    }

    #[tokio::test]
    async fn issue_parses_without_resolution_field() {
        // JIRA sometimes omits `resolution` entirely rather than
        // returning null. Our model uses #[serde(default)] to tolerate.
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/RHEL-2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "key": "RHEL-2",
                "fields": {
                    "summary": "no-resolution field",
                    "status": {"name": "New"}
                }
            })))
            .mount(&server)
            .await;

        let issue = client.issue("RHEL-2").await.unwrap().unwrap();
        assert!(!issue.is_resolved());
    }
}
