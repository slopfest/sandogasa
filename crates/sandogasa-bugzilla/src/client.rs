// SPDX-License-Identifier: MPL-2.0

use reqwest::Client;

use crate::models::{Bug, BugSearchResponse, Comment, CommentResponse};

pub struct BzClient {
    base_url: String,
    client: Client,
    api_key: Option<String>,
}

impl BzClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: Client::new(),
            api_key: None,
        }
    }

    pub fn with_api_key(mut self, key: String) -> Self {
        self.api_key = Some(key);
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}/rest/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if let Some(ref key) = self.api_key {
            req.bearer_auth(key)
        } else {
            req
        }
    }

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        self.auth(self.client.get(self.url(path)))
    }

    /// Fetch a single bug by numeric ID.
    pub async fn bug(&self, id: u64) -> Result<Bug, reqwest::Error> {
        self.bug_by_id(&id.to_string()).await
    }

    /// Fetch a single bug by alias (e.g. "CVE-FalsePositive-Unshipped") or numeric ID string.
    pub async fn bug_by_alias(&self, id_or_alias: &str) -> Result<Bug, reqwest::Error> {
        self.bug_by_id(id_or_alias).await
    }

    async fn bug_by_id(&self, id: &str) -> Result<Bug, reqwest::Error> {
        let resp: BugSearchResponse = self
            .request(&format!("bug/{id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.bugs.into_iter().next().unwrap())
    }

    /// Search bugs with a query string (e.g. "product=Fedora&component=kernel&status=NEW").
    /// Paginates through all results up to `max_results`. Pass 0 for no limit.
    pub async fn search(&self, query: &str, max_results: u64) -> Result<Vec<Bug>, reqwest::Error> {
        const PAGE_SIZE: u64 = 1000;
        let mut all_bugs = Vec::new();
        let mut offset: u64 = 0;

        loop {
            let limit = if max_results > 0 {
                PAGE_SIZE.min(max_results - offset as u64)
            } else {
                PAGE_SIZE
            };

            let resp: BugSearchResponse = self
                .request(&format!("bug?{query}&limit={limit}&offset={offset}"))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;

            let total = resp.total_matches.unwrap_or(resp.bugs.len() as u64);
            all_bugs.extend(resp.bugs);

            offset = all_bugs.len() as u64;
            if offset >= total || (max_results > 0 && offset >= max_results) {
                break;
            }
        }

        Ok(all_bugs)
    }

    /// Fetch comments for a bug.
    pub async fn comments(&self, bug_id: u64) -> Result<Vec<Comment>, reqwest::Error> {
        let resp: CommentResponse = self
            .request(&format!("bug/{bug_id}/comment"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp
            .bugs
            .into_values()
            .next()
            .map(|b| b.comments)
            .unwrap_or_default())
    }

    /// Validate the API key by checking if the login is recognized.
    ///
    /// Calls `GET /rest/valid_login?login={email}` with the configured
    /// API key.  Returns `Ok(true)` when valid, `Ok(false)` when the
    /// login is not recognized, and `Err` on network or auth errors
    /// (e.g. invalid API key → 400).
    pub async fn valid_login(&self, email: &str) -> Result<bool, reqwest::Error> {
        let url = format!("{}/rest/valid_login?login={email}", self.base_url);
        let resp: serde_json::Value = self
            .auth(self.client.get(&url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp["result"].as_bool().unwrap_or(false))
    }

    /// Update a bug. Requires an API key. The body is a JSON object with fields to update.
    pub async fn update(&self, id: u64, body: &serde_json::Value) -> Result<(), reqwest::Error> {
        self.auth(self.client.put(self.url(&format!("bug/{id}"))))
            .json(body)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Update multiple bugs in a single request. The `ids` are injected into the body.
    pub async fn update_many(
        &self,
        ids: &[u64],
        body: &serde_json::Value,
    ) -> Result<(), reqwest::Error> {
        let mut body = body.clone();
        body.as_object_mut()
            .unwrap()
            .insert("ids".to_string(), serde_json::json!(ids));
        // Use the first ID in the URL (required by the endpoint), but
        // the `ids` field in the body controls which bugs are updated.
        self.auth(self.client.put(self.url(&format!("bug/{}", ids[0]))))
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ---- new / URL normalization ----

    #[test]
    fn new_trims_trailing_slash() {
        let client = BzClient::new("https://bugzilla.redhat.com/");
        assert_eq!(client.base_url, "https://bugzilla.redhat.com");
    }

    #[test]
    fn new_preserves_url_without_trailing_slash() {
        let client = BzClient::new("https://bugzilla.redhat.com");
        assert_eq!(client.base_url, "https://bugzilla.redhat.com");
    }

    #[test]
    fn new_trims_multiple_trailing_slashes() {
        let client = BzClient::new("https://bugzilla.redhat.com///");
        assert_eq!(client.base_url, "https://bugzilla.redhat.com");
    }

    #[test]
    fn new_no_api_key_by_default() {
        let client = BzClient::new("https://example.com");
        assert!(client.api_key.is_none());
    }

    // ---- with_api_key ----

    #[test]
    fn with_api_key_sets_key() {
        let client = BzClient::new("https://example.com").with_api_key("secret123".to_string());
        assert_eq!(client.api_key.as_deref(), Some("secret123"));
    }

    // ---- url() ----

    #[test]
    fn url_constructs_rest_path() {
        let client = BzClient::new("https://bugzilla.redhat.com");
        assert_eq!(
            client.url("bug/12345"),
            "https://bugzilla.redhat.com/rest/bug/12345"
        );
    }

    #[test]
    fn url_trims_leading_slash_from_path() {
        let client = BzClient::new("https://bugzilla.redhat.com");
        assert_eq!(
            client.url("/bug/12345"),
            "https://bugzilla.redhat.com/rest/bug/12345"
        );
    }

    #[test]
    fn url_with_query_string() {
        let client = BzClient::new("https://bugzilla.redhat.com");
        assert_eq!(
            client.url("bug?product=Fedora&status=NEW"),
            "https://bugzilla.redhat.com/rest/bug?product=Fedora&status=NEW"
        );
    }

    // ---- valid_login ----

    #[tokio::test]
    async fn valid_login_returns_true() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri()).with_api_key("key".into());

        Mock::given(method("GET"))
            .and(path("/rest/valid_login"))
            .and(query_param("login", "user@example.com"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"result": true})),
            )
            .expect(1)
            .mount(&server)
            .await;

        assert!(client.valid_login("user@example.com").await.unwrap());
    }

    #[tokio::test]
    async fn valid_login_returns_false() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri()).with_api_key("key".into());

        Mock::given(method("GET"))
            .and(path("/rest/valid_login"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"result": false})),
            )
            .expect(1)
            .mount(&server)
            .await;

        assert!(!client.valid_login("unknown@example.com").await.unwrap());
    }

    #[tokio::test]
    async fn valid_login_returns_error_on_bad_key() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri()).with_api_key("bad".into());

        Mock::given(method("GET"))
            .and(path("/rest/valid_login"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;

        let result = client.valid_login("user@example.com").await;
        assert!(result.is_err());
    }

    // ---- update ----

    #[tokio::test]
    async fn update_sends_put_with_body() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri()).with_api_key("key".into());

        Mock::given(method("PUT"))
            .and(path("/rest/bug/42"))
            .and(body_json(serde_json::json!({"status": "CLOSED"})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        client
            .update(42, &serde_json::json!({"status": "CLOSED"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn update_returns_error_on_server_failure() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri()).with_api_key("key".into());

        Mock::given(method("PUT"))
            .and(path("/rest/bug/99"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let result = client
            .update(99, &serde_json::json!({"status": "CLOSED"}))
            .await;
        assert!(result.is_err());
    }

    // ---- update_many ----

    #[tokio::test]
    async fn update_many_sends_ids_in_body() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri()).with_api_key("key".into());

        Mock::given(method("PUT"))
            .and(path("/rest/bug/1"))
            .and(body_json(serde_json::json!({
                "status": "CLOSED",
                "resolution": "NOTABUG",
                "ids": [1, 2, 3]
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        client
            .update_many(
                &[1, 2, 3],
                &serde_json::json!({"status": "CLOSED", "resolution": "NOTABUG"}),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn update_many_single_id() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri()).with_api_key("key".into());

        Mock::given(method("PUT"))
            .and(path("/rest/bug/42"))
            .and(body_json(serde_json::json!({
                "status": "CLOSED",
                "ids": [42]
            })))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        client
            .update_many(&[42], &serde_json::json!({"status": "CLOSED"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn update_many_returns_error_on_server_failure() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri()).with_api_key("key".into());

        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let result = client
            .update_many(&[1, 2], &serde_json::json!({"status": "CLOSED"}))
            .await;
        assert!(result.is_err());
    }
}
