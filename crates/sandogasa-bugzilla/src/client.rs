// SPDX-License-Identifier: Apache-2.0 OR MIT

use reqwest::Client;

use crate::models::{Bug, BugSearchResponse, Comment, CommentResponse, CreateBugResponse};

pub struct BzClient {
    base_url: String,
    client: Client,
    api_key: Option<String>,
}

impl BzClient {
    pub fn new(base_url: &str) -> Self {
        sandogasa_cli::install_crypto_provider();

        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: Client::new(),
            api_key: None,
        }
    }

    /// Attach an API key for authenticated requests.
    ///
    /// Refuses (returns an error) if the client's base URL would
    /// send the key over plaintext `http` to a non-loopback host;
    /// see [`sandogasa_cli::ensure_secure_url`].
    pub fn with_api_key(mut self, key: String) -> Result<Self, Box<dyn std::error::Error>> {
        sandogasa_cli::ensure_secure_url(&self.base_url)?;
        self.api_key = Some(key);
        Ok(self)
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

    /// Fetch multiple bugs by ID in a single request.
    pub async fn bugs(&self, ids: &[u64]) -> Result<Vec<Bug>, reqwest::Error> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let id_params: Vec<String> = ids.iter().map(|id| format!("id={id}")).collect();
        let query = id_params.join("&");
        let resp: BugSearchResponse = self
            .request(&format!("bug?{query}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(resp.bugs)
    }

    /// Search bugs with a query string (e.g. "product=Fedora&component=kernel&status=NEW").
    /// Paginates through all results up to `max_results`. Pass 0 for no limit.
    pub async fn search(&self, query: &str, max_results: u64) -> Result<Vec<Bug>, reqwest::Error> {
        const PAGE_SIZE: u64 = 1000;
        let mut all_bugs = Vec::new();
        let mut offset: u64 = 0;

        loop {
            let limit = if max_results > 0 {
                PAGE_SIZE.min(max_results - offset)
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

    /// Create a new bug. Requires an API key. `fields` is a JSON
    /// object of bug fields (`product`, `component`, `version`,
    /// `summary`, `description`, optionally `blocks`/
    /// `depends_on`, …).
    ///
    /// Returns the parsed response rather than erroring on a
    /// Bugzilla-level rejection: a 400 with `{"error": true,
    /// ...}` (e.g. an invalid component for the product) comes
    /// back as a `CreateBugResponse` with `error == true`, so the
    /// caller can fall back to a different product. Only
    /// transport-level failures surface as `Err`.
    pub async fn create(
        &self,
        fields: &serde_json::Value,
    ) -> Result<CreateBugResponse, reqwest::Error> {
        self.auth(self.client.post(self.url("bug")))
            .json(fields)
            .send()
            .await?
            .json()
            .await
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
        let client = BzClient::new("https://example.com")
            .with_api_key("secret123".to_string())
            .unwrap();
        assert_eq!(client.api_key.as_deref(), Some("secret123"));
    }

    #[test]
    fn with_api_key_rejects_plaintext_remote() {
        // Refuse to attach a key to a plaintext http non-loopback URL.
        let result = BzClient::new("http://bugzilla.example.com").with_api_key("k".to_string());
        assert!(result.is_err());
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
        let client = BzClient::new(&server.uri())
            .with_api_key("key".into())
            .unwrap();

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
        let client = BzClient::new(&server.uri())
            .with_api_key("key".into())
            .unwrap();

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
        let client = BzClient::new(&server.uri())
            .with_api_key("bad".into())
            .unwrap();

        Mock::given(method("GET"))
            .and(path("/rest/valid_login"))
            .respond_with(ResponseTemplate::new(400))
            .mount(&server)
            .await;

        let result = client.valid_login("user@example.com").await;
        assert!(result.is_err());
    }

    // ---- create ----

    #[tokio::test]
    async fn create_posts_and_returns_id() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri())
            .with_api_key("key".into())
            .unwrap();

        Mock::given(method("POST"))
            .and(path("/rest/bug"))
            .and(body_json(serde_json::json!({
                "product": "Fedora EPEL",
                "component": "foo",
                "version": "epel9",
                "summary": "Please branch and build foo in epel9",
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 12345})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let resp = client
            .create(&serde_json::json!({
                "product": "Fedora EPEL",
                "component": "foo",
                "version": "epel9",
                "summary": "Please branch and build foo in epel9",
            }))
            .await
            .unwrap();
        assert_eq!(resp.id, Some(12345));
        assert!(!resp.error);
    }

    #[tokio::test]
    async fn create_surfaces_bugzilla_error_without_erroring() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri())
            .with_api_key("key".into())
            .unwrap();

        // Bugzilla rejects an invalid component with a 400 + error body.
        Mock::given(method("POST"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": true,
                "code": 51,
                "message": "The component 'foo' does not exist in the 'Fedora EPEL' product."
            })))
            .mount(&server)
            .await;

        let resp = client
            .create(&serde_json::json!({"product": "Fedora EPEL", "component": "foo"}))
            .await
            .unwrap();
        assert!(resp.error);
        assert_eq!(resp.id, None);
        assert!(resp.message.unwrap().contains("does not exist"));
    }

    // ---- update ----

    #[tokio::test]
    async fn update_sends_put_with_body() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri())
            .with_api_key("key".into())
            .unwrap();

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
        let client = BzClient::new(&server.uri())
            .with_api_key("key".into())
            .unwrap();

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
        let client = BzClient::new(&server.uri())
            .with_api_key("key".into())
            .unwrap();

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
        let client = BzClient::new(&server.uri())
            .with_api_key("key".into())
            .unwrap();

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
        let client = BzClient::new(&server.uri())
            .with_api_key("key".into())
            .unwrap();

        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let result = client
            .update_many(&[1, 2], &serde_json::json!({"status": "CLOSED"}))
            .await;
        assert!(result.is_err());
    }

    // ---- bug ----

    #[tokio::test]
    async fn bug_returns_parsed_bug() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/bug/12345"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [{
                    "id": 12345,
                    "summary": "Test bug",
                    "status": "NEW",
                    "resolution": "",
                    "product": "Fedora",
                    "component": ["kernel"],
                    "severity": "medium",
                    "priority": "unspecified",
                    "assigned_to": "nobody@fedoraproject.org",
                    "creator": "reporter@example.com",
                    "creation_time": "2025-01-15T10:00:00Z",
                    "last_change_time": "2025-01-16T12:00:00Z"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let bug = client.bug(12345).await.unwrap();
        assert_eq!(bug.id, 12345);
        assert_eq!(bug.summary, "Test bug");
        assert_eq!(bug.status, "NEW");
        assert_eq!(bug.product, "Fedora");
    }

    #[tokio::test]
    async fn bug_returns_error_on_404() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/bug/99999"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let result = client.bug(99999).await;
        assert!(result.is_err());
    }

    // ---- bug_by_alias ----

    #[tokio::test]
    async fn bug_by_alias_returns_parsed_bug() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/bug/CVE-2025-1234"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [{
                    "id": 54321,
                    "summary": "CVE-2025-1234 kernel: buffer overflow",
                    "status": "ASSIGNED",
                    "resolution": "",
                    "product": "Fedora",
                    "component": ["kernel"],
                    "severity": "high",
                    "priority": "urgent",
                    "assigned_to": "dev@example.com",
                    "creator": "secalert@redhat.com",
                    "creation_time": "2025-03-01T08:00:00Z",
                    "last_change_time": "2025-03-02T09:00:00Z",
                    "alias": ["CVE-2025-1234"]
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let bug = client.bug_by_alias("CVE-2025-1234").await.unwrap();
        assert_eq!(bug.id, 54321);
        assert_eq!(bug.alias, vec!["CVE-2025-1234"]);
    }

    // ---- search ----

    #[tokio::test]
    async fn search_returns_bugs() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [
                    {
                        "id": 1,
                        "summary": "Bug 1",
                        "status": "NEW",
                        "resolution": "",
                        "product": "Fedora",
                        "component": ["kernel"],
                        "severity": "medium",
                        "priority": "unspecified",
                        "assigned_to": "nobody@fedoraproject.org",
                        "creator": "reporter@example.com",
                        "creation_time": "2025-01-01T00:00:00Z",
                        "last_change_time": "2025-01-01T00:00:00Z"
                    },
                    {
                        "id": 2,
                        "summary": "Bug 2",
                        "status": "ASSIGNED",
                        "resolution": "",
                        "product": "Fedora",
                        "component": ["glibc"],
                        "severity": "low",
                        "priority": "low",
                        "assigned_to": "dev@example.com",
                        "creator": "reporter@example.com",
                        "creation_time": "2025-01-02T00:00:00Z",
                        "last_change_time": "2025-01-02T00:00:00Z"
                    }
                ],
                "total_matches": 2
            })))
            .expect(1)
            .mount(&server)
            .await;

        let bugs = client.search("product=Fedora", 0).await.unwrap();
        assert_eq!(bugs.len(), 2);
        assert_eq!(bugs[0].id, 1);
        assert_eq!(bugs[1].id, 2);
    }

    #[tokio::test]
    async fn search_with_max_results() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [{
                    "id": 1,
                    "summary": "Bug 1",
                    "status": "NEW",
                    "resolution": "",
                    "product": "Fedora",
                    "component": ["kernel"],
                    "severity": "medium",
                    "priority": "unspecified",
                    "assigned_to": "nobody@fedoraproject.org",
                    "creator": "reporter@example.com",
                    "creation_time": "2025-01-01T00:00:00Z",
                    "last_change_time": "2025-01-01T00:00:00Z"
                }],
                "total_matches": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let bugs = client.search("product=Fedora", 1).await.unwrap();
        assert_eq!(bugs.len(), 1);
    }

    #[tokio::test]
    async fn search_returns_error_on_server_failure() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let result = client.search("product=Fedora", 0).await;
        assert!(result.is_err());
    }

    // ---- comments ----

    #[tokio::test]
    async fn comments_returns_parsed_comments() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/bug/12345/comment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": {
                    "12345": {
                        "comments": [
                            {
                                "id": 1001,
                                "text": "This is the first comment",
                                "creator": "reporter@example.com",
                                "creation_time": "2025-01-15T10:00:00Z",
                                "is_private": false
                            },
                            {
                                "id": 1002,
                                "text": "This is a private comment",
                                "creator": "dev@example.com",
                                "creation_time": "2025-01-16T12:00:00Z",
                                "is_private": true
                            }
                        ]
                    }
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let comments = client.comments(12345).await.unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].id, 1001);
        assert_eq!(comments[0].text, "This is the first comment");
        assert!(!comments[0].is_private);
        assert_eq!(comments[1].id, 1002);
        assert!(comments[1].is_private);
    }

    #[tokio::test]
    async fn comments_returns_empty_for_missing_bucket() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/bug/99999/comment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": {}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let comments = client.comments(99999).await.unwrap();
        assert!(comments.is_empty());
    }

    #[tokio::test]
    async fn comments_returns_error_on_server_failure() {
        let server = MockServer::start().await;
        let client = BzClient::new(&server.uri());

        Mock::given(method("GET"))
            .and(path("/rest/bug/12345/comment"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let result = client.comments(12345).await;
        assert!(result.is_err());
    }
}
