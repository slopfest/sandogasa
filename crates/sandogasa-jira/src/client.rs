// SPDX-License-Identifier: Apache-2.0 OR MIT

use reqwest::Client;

use crate::models::{CreatedIssue, FieldMeta, Issue, Myself, NewIssue, Version};

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
    ///
    /// Errors if the base URL is not https, so a token cannot be put
    /// on the wire in cleartext; loopback is allowed, for tests.
    pub fn with_api_key(mut self, key: String) -> Result<Self, String> {
        sandogasa_cli::ensure_secure_url(&self.base_url)?;
        self.auth = Some(Auth::Bearer(key));
        Ok(self)
    }

    /// Attach an Atlassian Cloud API token with the account's email,
    /// sent as basic auth. Build the client on [`gateway_url`] for
    /// this: the site host ignores a scoped token. A scoped token
    /// needs `read:jira-user` for [`myself`](Self::myself) and
    /// `read:jira-work` for issues.
    ///
    /// Errors if the base URL is not https, as [`Self::with_api_key`]
    /// does and for the same reason.
    pub fn with_api_token(mut self, email: String, token: String) -> Result<Self, String> {
        sandogasa_cli::ensure_secure_url(&self.base_url)?;
        self.auth = Some(Auth::Basic { email, token });
        Ok(self)
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

    /// The issues `jql` matches, with `fields` read (an empty list
    /// reads Jira's default set), up to `limit`, through Jira Cloud's
    /// paginated `search/jql`. Public issues are found anonymously.
    pub async fn search(
        &self,
        jql: &str,
        fields: &[&str],
        limit: usize,
    ) -> Result<Vec<Issue>, reqwest::Error> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Page {
            #[serde(default)]
            issues: Vec<Issue>,
            #[serde(default)]
            next_page_token: Option<String>,
            #[serde(default)]
            is_last: Option<bool>,
        }
        let mut out: Vec<Issue> = Vec::new();
        let mut token: Option<String> = None;
        let fields = fields.join(",");
        while out.len() < limit {
            let page_size = (limit - out.len()).min(100).to_string();
            let mut query: Vec<(&str, &str)> = vec![("jql", jql), ("maxResults", &page_size)];
            if !fields.is_empty() {
                query.push(("fields", &fields));
            }
            if let Some(t) = &token {
                query.push(("nextPageToken", t));
            }
            let page: Page = self
                .auth(self.client.get(self.url("search/jql")).query(&query))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let done = page.issues.is_empty()
                || page.is_last.unwrap_or(true)
                || page.next_page_token.is_none();
            out.extend(page.issues);
            if done {
                break;
            }
            token = page.next_page_token;
        }
        out.truncate(limit);
        Ok(out)
    }

    /// A project's versions, released and archived ones included:
    /// what affected versions are chosen from. Public for a public
    /// project.
    pub async fn project_versions(&self, project: &str) -> Result<Vec<Version>, reqwest::Error> {
        self.auth(
            self.client
                .get(self.url(&format!("project/{project}/versions"))),
        )
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
    }

    /// The fields an issue of type `issue_type` (by name) takes in
    /// `project`, from the create metadata: which are required, and
    /// what a choice accepts. `Ok(None)` when the project has no such
    /// issue type for these credentials. Needs credentials that may
    /// create issues there; anonymously Jira answers 404.
    pub async fn create_metadata(
        &self,
        project: &str,
        issue_type: &str,
    ) -> Result<Option<Vec<FieldMeta>>, reqwest::Error> {
        let types = self
            .paged(
                &format!("issue/createmeta/{project}/issuetypes"),
                "issueTypes",
            )
            .await?;
        let Some(id) = types.iter().find_map(|t| {
            (t["name"].as_str()?.eq_ignore_ascii_case(issue_type))
                .then(|| t["id"].as_str().map(str::to_string))
                .flatten()
        }) else {
            return Ok(None);
        };
        let fields = self
            .paged(
                &format!("issue/createmeta/{project}/issuetypes/{id}"),
                "fields",
            )
            .await?;
        Ok(Some(fields.iter().filter_map(field_meta).collect()))
    }

    /// File `issue`, answering its key. Jira's own complaint is kept
    /// in the error (a required field left out, a version the project
    /// does not have), since it is what says how to fix the request.
    pub async fn create_issue(&self, issue: &NewIssue) -> Result<CreatedIssue, String> {
        let resp = self
            .auth(self.client.post(self.url("issue")).json(&issue.to_json()))
            .send()
            .await
            .map_err(|e| format!("could not reach Jira: {e}"))?;
        let status = resp.status();
        if status.is_success() {
            return resp
                .json()
                .await
                .map_err(|e| format!("Jira filed the issue but its answer did not parse: {e}"));
        }
        let body = resp.text().await.unwrap_or_default();
        Err(format!(
            "Jira refused the issue ({status}): {}",
            jira_errors(&body)
        ))
    }

    /// Every item under `key` in a paginated answer (`startAt`,
    /// `maxResults`); the create metadata lists both issue types and
    /// fields this way. `values` is accepted too, as Jira names the
    /// list that in some answers.
    async fn paged(&self, path: &str, key: &str) -> Result<Vec<serde_json::Value>, reqwest::Error> {
        let mut out = Vec::new();
        loop {
            let start = out.len().to_string();
            let page: serde_json::Value = self
                .auth(
                    self.client
                        .get(self.url(path))
                        .query(&[("startAt", start.as_str()), ("maxResults", "50")]),
                )
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let items = page[key]
                .as_array()
                .or_else(|| page["values"].as_array())
                .cloned()
                .unwrap_or_default();
            let total = page["total"].as_u64();
            let got = items.len();
            out.extend(items);
            if got == 0 || total.is_none_or(|t| out.len() as u64 >= t) {
                break;
            }
        }
        Ok(out)
    }
}

/// One field of the create metadata.
fn field_meta(f: &serde_json::Value) -> Option<FieldMeta> {
    Some(FieldMeta {
        field_id: f["fieldId"]
            .as_str()
            .or_else(|| f["key"].as_str())?
            .to_string(),
        name: f["name"].as_str().unwrap_or_default().to_string(),
        required: f["required"].as_bool().unwrap_or(false),
        has_default_value: f["hasDefaultValue"].as_bool().unwrap_or(false),
        allowed_values: f["allowedValues"]
            .as_array()
            .map(|vs| {
                vs.iter()
                    .filter_map(|v| v["name"].as_str().or_else(|| v["value"].as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Jira's complaint in an error body: its `errorMessages` and the
/// per-field `errors`, or the body itself when it is neither.
fn jira_errors(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.trim().to_string();
    };
    let mut parts: Vec<String> = v["errorMessages"]
        .as_array()
        .map(|m| {
            m.iter()
                .filter_map(|x| x.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    if let Some(errors) = v["errors"].as_object() {
        let mut fields: Vec<_> = errors.iter().collect();
        fields.sort_by(|a, b| a.0.cmp(b.0));
        parts.extend(
            fields
                .into_iter()
                .map(|(k, e)| format!("{k}: {}", e.as_str().unwrap_or_default())),
        );
    }
    if parts.is_empty() {
        body.trim().to_string()
    } else {
        parts.join("; ")
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

    /// A token is only attached to an https base URL (loopback aside,
    /// for tests), so it never goes over the wire in cleartext.
    #[test]
    fn a_token_is_refused_for_a_plaintext_base_url() {
        let plain = || JiraClient::new("http://issues.example.com");
        assert!(plain().with_api_key("k".into()).is_err());
        assert!(plain().with_api_token("a@b".into(), "t".into()).is_err());
        let https = || JiraClient::new("https://issues.example.com");
        assert!(https().with_api_key("k".into()).is_ok());
        assert!(https().with_api_token("a@b".into(), "t".into()).is_ok());
    }
    use wiremock::matchers::{
        body_json, header, method, path, query_param, query_param_is_missing,
    };
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
            .with_api_token("me@example.com".into(), "secret-token".into())
            .unwrap();

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
        let client = JiraClient::new(&server.uri())
            .with_api_token("me@example.com".into(), "bad".into())
            .unwrap();
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
        let client = JiraClient::new(&server.uri())
            .with_api_key("secret-token".to_string())
            .unwrap();

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

    #[tokio::test]
    async fn search_follows_the_page_token_and_reads_the_fields() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri());
        let issue = |key: &str, versions: &[&str]| {
            serde_json::json!({
                "key": key,
                "fields": {
                    "summary": "CVE-2025-27363: out of bounds write in FreeType <= 2.13.0",
                    "status": {"name": "Closed"},
                    "resolution": {"name": "Duplicate"},
                    "issuetype": {"name": "Bug"},
                    "priority": {"name": "Critical"},
                    "components": [{"name": "freetype"}],
                    "versions": versions.iter().map(|v| serde_json::json!({"name": v})).collect::<Vec<_>>(),
                }
            })
        };
        let jql = "project = RHEL AND component = freetype";
        Mock::given(method("GET"))
            .and(path("/rest/api/2/search/jql"))
            .and(query_param("jql", jql))
            .and(query_param("fields", "summary,versions"))
            .and(query_param_is_missing("nextPageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issues": [issue("RHEL-83280", &["CentOS Stream 9", "rhel-9.5"])],
                "nextPageToken": "page2",
                "isLast": false,
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/2/search/jql"))
            .and(query_param("nextPageToken", "page2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issues": [issue("RHEL-83281", &["rhel-8.10"])],
                "isLast": true,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let found = client
            .search(jql, &["summary", "versions"], 10)
            .await
            .unwrap();
        assert_eq!(
            found.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
            ["RHEL-83280", "RHEL-83281"]
        );
        let first = &found[0].fields;
        assert_eq!(first.issuetype.as_ref().unwrap().name, "Bug");
        assert_eq!(first.priority.as_ref().unwrap().name, "Critical");
        assert_eq!(first.components[0].name, "freetype");
        assert_eq!(
            first
                .versions
                .iter()
                .map(|v| v.name.as_str())
                .collect::<Vec<_>>(),
            ["CentOS Stream 9", "rhel-9.5"]
        );
    }

    #[tokio::test]
    async fn search_stops_at_the_limit() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri());
        let issue = |key: &str| serde_json::json!({"key": key, "fields": {"summary": "s", "status": {"name": "New"}}});
        // One page holding more than asked for, and a token that must
        // not be followed.
        Mock::given(method("GET"))
            .and(path("/rest/api/2/search/jql"))
            .and(query_param("maxResults", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issues": [issue("RHEL-1"), issue("RHEL-2")],
                "nextPageToken": "more",
                "isLast": false,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let found = client.search("project = RHEL", &[], 1).await.unwrap();
        assert_eq!(found.len(), 1);
    }

    #[tokio::test]
    async fn project_versions_lists_released_and_archived_state() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri());
        Mock::given(method("GET"))
            .and(path("/rest/api/2/project/RHEL/versions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": "1", "name": "rhel-9.8", "archived": false, "released": true, "releaseDate": "2026-05-19"},
                {"id": "2", "name": "rhel-9.9", "archived": false, "released": false},
            ])))
            .mount(&server)
            .await;
        let versions = client.project_versions("RHEL").await.unwrap();
        assert_eq!(versions.len(), 2);
        assert!(versions[0].released);
        assert_eq!(versions[0].release_date.as_deref(), Some("2026-05-19"));
        assert!(!versions[1].released && !versions[1].archived);
    }

    #[tokio::test]
    async fn create_metadata_finds_the_type_and_its_fields() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri())
            .with_api_token("me@example.com".into(), "tok".into())
            .unwrap();
        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/createmeta/RHEL/issuetypes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issueTypes": [{"id": "1", "name": "Story"}, {"id": "2", "name": "Bug"}],
                "total": 2,
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/2/issue/createmeta/RHEL/issuetypes/2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "fields": [
                    {"fieldId": "summary", "name": "Summary", "required": true, "hasDefaultValue": false},
                    {"fieldId": "versions", "name": "Affects Versions", "required": true,
                     "allowedValues": [{"name": "CentOS Stream 9"}, {"name": "rhel-9.9"}]},
                    {"fieldId": "customfield_10868", "name": "Product", "required": false,
                     "hasDefaultValue": true, "allowedValues": [{"value": "Red Hat Enterprise Linux"}]},
                ],
                "total": 3,
            })))
            .mount(&server)
            .await;

        let fields = client
            .create_metadata("RHEL", "bug")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fields.len(), 3);
        let versions = fields.iter().find(|f| f.field_id == "versions").unwrap();
        assert!(versions.required);
        assert_eq!(versions.allowed_values, ["CentOS Stream 9", "rhel-9.9"]);
        let product = fields
            .iter()
            .find(|f| f.field_id == "customfield_10868")
            .unwrap();
        assert!(product.has_default_value);
        assert_eq!(product.allowed_values, ["Red Hat Enterprise Linux"]);
        // A type the project does not have is not an error.
        assert!(
            client
                .create_metadata("RHEL", "Epic")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn create_issue_sends_the_fields_and_answers_the_key() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri())
            .with_api_token("me@example.com".into(), "tok".into())
            .unwrap();
        let mut issue = NewIssue::new(
            "RHEL",
            "Bug",
            "CVE-2025-27363: out of bounds write in FreeType <= 2.13.0",
        );
        issue.description = "severity: high (CVSS 3.1: 8.1)".into();
        issue.components = vec!["freetype".into()];
        issue.versions = vec!["CentOS Stream 9".into(), "rhel-9.9".into()];
        issue.priority = Some("Critical".into());
        Mock::given(method("POST"))
            .and(path("/rest/api/2/issue"))
            .and(body_json(serde_json::json!({"fields": {
                "project": {"key": "RHEL"},
                "issuetype": {"name": "Bug"},
                "summary": "CVE-2025-27363: out of bounds write in FreeType <= 2.13.0",
                "description": "severity: high (CVSS 3.1: 8.1)",
                "components": [{"name": "freetype"}],
                "versions": [{"name": "CentOS Stream 9"}, {"name": "rhel-9.9"}],
                "priority": {"name": "Critical"},
            }})))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(serde_json::json!({"id": "1", "key": "RHEL-200001"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let created = client.create_issue(&issue).await.unwrap();
        assert_eq!(created.key, "RHEL-200001");
    }

    #[tokio::test]
    async fn a_refused_issue_says_what_jira_objected_to() {
        let server = MockServer::start().await;
        let client = JiraClient::new(&server.uri())
            .with_api_token("me@example.com".into(), "tok".into())
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/rest/api/2/issue"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "errorMessages": [],
                "errors": {"versions": "Version name 'rhel-9.99' is not valid"},
            })))
            .mount(&server)
            .await;
        let err = client
            .create_issue(&NewIssue::new("RHEL", "Bug", "x"))
            .await
            .unwrap_err();
        assert!(err.contains("400"), "{err}");
        assert!(
            err.contains("versions: Version name 'rhel-9.99' is not valid"),
            "{err}"
        );
    }
}
