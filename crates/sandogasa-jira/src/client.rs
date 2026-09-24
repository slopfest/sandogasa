// SPDX-License-Identifier: Apache-2.0 OR MIT

use reqwest::Client;

use crate::models::{Issue, Myself};

/// Minimal JIRA REST v2 client.
///
/// Configured with a base URL like `https://redhat.atlassian.net`.
/// Public issues work anonymously. For anything else, an Atlassian
/// Cloud site takes an API token from
/// <https://id.atlassian.com/manage-profile/security/api-tokens>
/// beside the account's email, as basic auth
/// ([`with_api_token`](Self::with_api_token)) — through Atlassian's
/// API gateway rather than the site itself, since a scoped token is
/// honoured only there: [`cloud_id`] reads the site's tenant id and
/// [`gateway_url`] turns it into the base URL. A self-hosted Jira
/// takes a personal access token as a bearer
/// ([`with_api_key`](Self::with_api_key)). Give a Cloud site its own
/// host, not a redirecting alias: an `Authorization` header does not
/// survive a redirect to another host.
pub struct JiraClient {
    base_url: String,
    client: Client,
    auth: Option<Auth>,
}

/// How requests are authenticated.
enum Auth {
    /// A self-hosted Jira's personal access token.
    Bearer(String),
    /// An Atlassian Cloud API token, sent as basic auth with the
    /// account's email.
    Basic { email: String, token: String },
}

impl JiraClient {
    /// Construct a new client. The base URL should NOT include the
    /// `/rest/api/2` path — that's added by each endpoint.
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: build_http_client(),
            auth: None,
        }
    }

    /// Attach a self-hosted Jira's personal access token, sent as a
    /// bearer. An Atlassian Cloud site ignores an API token sent this
    /// way; use [`with_api_token`](Self::with_api_token) there.
    pub fn with_api_key(mut self, key: String) -> Self {
        self.auth = Some(Auth::Bearer(key));
        self
    }

    /// Attach an Atlassian Cloud API token with the account's email,
    /// sent as basic auth. Build the client on [`gateway_url`] for
    /// this: the site host ignores a scoped token. A scoped token
    /// needs `read:jira-user` for [`myself`](Self::myself) and
    /// `read:jira-work` for issues.
    pub fn with_api_token(mut self, email: String, token: String) -> Self {
        self.auth = Some(Auth::Basic { email, token });
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
        match &self.auth {
            Some(Auth::Bearer(key)) => req.bearer_auth(key),
            Some(Auth::Basic { email, token }) => req.basic_auth(email, Some(token)),
            None => req,
        }
    }

    /// Who the credentials belong to (`GET myself`), which is also
    /// the check that they work: Jira answers 401 to an anonymous or
    /// rejected request, so the error's status tells a bad token from
    /// an unreachable server.
    pub async fn myself(&self) -> Result<Myself, reqwest::Error> {
        self.auth(self.client.get(self.url("myself")))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
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

/// An Atlassian Cloud site's tenant id, from its public
/// `_edge/tenant_info` endpoint — what [`gateway_url`] needs.
pub async fn cloud_id(site_url: &str) -> Result<String, reqwest::Error> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TenantInfo {
        cloud_id: String,
    }
    let info: TenantInfo = build_http_client()
        .get(format!(
            "{}/_edge/tenant_info",
            site_url.trim_end_matches('/')
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(info.cloud_id)
}

/// The base URL for a Cloud site's API through Atlassian's gateway,
/// `https://api.atlassian.com/ex/jira/<cloud id>`, which accepts the
/// site's API tokens, scoped ones included.
pub fn gateway_url(cloud_id: &str) -> String {
    format!("https://api.atlassian.com/ex/jira/{cloud_id}")
}

/// Build the crate's HTTP client with the shared sandogasa defaults
/// (user agent, request timeout, crypto provider). Panics only where
/// `Client::new()` would too (TLS backend init).
fn build_http_client() -> Client {
    sandogasa_cli::http::builder(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ))
    .build()
    .expect("build reqwest client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn new_trims_trailing_slashes() {
        let client = JiraClient::new("https://redhat.atlassian.net/");
        assert_eq!(client.base_url, "https://redhat.atlassian.net");
        let client = JiraClient::new("https://redhat.atlassian.net///");
        assert_eq!(client.base_url, "https://redhat.atlassian.net");
    }

    #[test]
    fn url_composes_rest_path() {
        let client = JiraClient::new("https://redhat.atlassian.net");
        assert_eq!(
            client.url("issue/RHEL-123"),
            "https://redhat.atlassian.net/rest/api/2/issue/RHEL-123"
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
    async fn with_api_token_sends_basic_auth_and_myself_reads_the_account() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri())
            .with_api_token("me@example.com".into(), "secret-token".into());

        Mock::given(method("GET"))
            .and(path("/rest/api/2/myself"))
            .and(header(
                "authorization",
                "Basic bWVAZXhhbXBsZS5jb206c2VjcmV0LXRva2Vu",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "accountId": "5b10a2844c20165700ede21g",
                "emailAddress": "me@example.com",
                "displayName": "Me Myself"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let me = client.myself().await.unwrap();
        assert_eq!(me.display_name, "Me Myself");
        assert_eq!(me.email_address.as_deref(), Some("me@example.com"));
    }

    #[tokio::test]
    async fn cloud_id_reads_the_tenant_and_names_the_gateway() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_edge/tenant_info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "cloudId": "2b9e35e3-6bd3-4cec-b838-f4249ee02432"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let id = cloud_id(&format!("{}/", server.uri())).await.unwrap();
        assert_eq!(
            gateway_url(&id),
            "https://api.atlassian.com/ex/jira/2b9e35e3-6bd3-4cec-b838-f4249ee02432"
        );
    }

    #[tokio::test]
    async fn myself_reports_a_rejected_token_by_status() {
        let server = MockServer::start().await;
        let client =
            JiraClient::new(&server.uri()).with_api_token("me@example.com".into(), "bad".into());
        Mock::given(method("GET"))
            .and(path("/rest/api/2/myself"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let err = client.myself().await.unwrap_err();
        assert_eq!(err.status(), Some(reqwest::StatusCode::UNAUTHORIZED));
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
