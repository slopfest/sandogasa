// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GitLab REST and GraphQL API client for issues and work items.

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

/// A GitLab user (assignee).
#[derive(Debug, Deserialize)]
pub struct Assignee {
    pub username: String,
}

/// A GitLab issue.
#[derive(Debug, Deserialize)]
pub struct Issue {
    pub iid: u64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub web_url: String,
    #[serde(default)]
    pub assignees: Vec<Assignee>,
    /// ISO-8601 date string (YYYY-MM-DD) or None.
    #[serde(default)]
    pub start_date: Option<String>,
    /// ISO-8601 date string (YYYY-MM-DD) or None.
    #[serde(default)]
    pub due_date: Option<String>,
    /// ISO-8601 timestamp GitLab set when the issue was
    /// created. Useful as a fallback start-date when the
    /// underlying Koji build has been untagged and we can't
    /// recover its creation time.
    #[serde(default)]
    pub created_at: Option<String>,
}

/// A GitLab merge request (minimal fields).
#[derive(Debug, Deserialize)]
pub struct MergeRequest {
    pub iid: u64,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub state: String,
    pub web_url: String,
    pub source_branch: String,
    pub target_branch: String,
}

/// Client for the GitLab REST API v4.
pub struct Client {
    http: reqwest::blocking::Client,
    base_url: String,
    project_path: String,
}

/// Build an HTTP client with the given token.
fn build_http_client(token: &str) -> Result<reqwest::blocking::Client, Box<dyn std::error::Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("private-token"),
        HeaderValue::from_str(token)?,
    );
    Ok(reqwest::blocking::Client::builder()
        .user_agent("sandogasa-gitlab/0.6.2")
        .default_headers(headers)
        .build()?)
}

impl Client {
    /// Create a client from a GitLab project URL and token.
    pub fn from_project_url(url: &str, token: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let (base_url, project_path) = parse_project_url(url)?;
        Self::new(&base_url, &project_path, token)
    }

    /// Create a client with explicit parameters.
    pub fn new(
        base_url: &str,
        project_path: &str,
        token: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let http = build_http_client(token)?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            project_path: project_path.to_string(),
        })
    }

    /// Fetch a merge request by its internal ID (iid).
    pub fn merge_request(&self, iid: u64) -> Result<MergeRequest, Box<dyn std::error::Error>> {
        let encoded = self.project_path.replace('/', "%2F");
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}",
            self.base_url, encoded, iid
        );
        let resp = self.http.get(&url).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab GET {url} failed: {status}: {text}").into());
        }
        Ok(resp.json()?)
    }

    /// Fetch a single issue by its internal ID (iid).
    pub fn issue(&self, iid: u64) -> Result<Issue, Box<dyn std::error::Error>> {
        let encoded = self.project_path.replace('/', "%2F");
        let url = format!(
            "{}/api/v4/projects/{}/issues/{}",
            self.base_url, encoded, iid
        );
        let resp = self.http.get(&url).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab GET {url} failed: {status}: {text}").into());
        }
        Ok(resp.json()?)
    }

    /// Create a new issue.
    pub fn create_issue(
        &self,
        title: &str,
        description: Option<&str>,
        labels: Option<&str>,
    ) -> Result<Issue, Box<dyn std::error::Error>> {
        let mut body = serde_json::json!({"title": title});
        if let Some(desc) = description {
            body["description"] = desc.into();
        }
        if let Some(labels) = labels {
            body["labels"] = labels.into();
        }

        let resp = self.http.post(self.issues_url()).json(&body).send()?;
        check_response(resp)
    }

    /// List issues matching a label and optional state.
    pub fn list_issues(
        &self,
        label: &str,
        state: Option<&str>,
    ) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
        let mut query = vec![("labels", label)];
        if let Some(s) = state {
            query.push(("state", s));
        }
        let resp = self.http.get(self.issues_url()).query(&query).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab API error {status}: {text}").into());
        }
        Ok(resp.json()?)
    }

    /// Add a note (comment) to an issue.
    pub fn add_note(&self, iid: u64, body: &str) -> Result<(), Box<dyn std::error::Error>> {
        let payload = serde_json::json!({ "body": body });
        let resp = self
            .http
            .post(format!("{}/{iid}/notes", self.issues_url()))
            .json(&payload)
            .send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab API error {status}: {text}").into());
        }
        Ok(())
    }

    /// Edit an existing issue.
    pub fn edit_issue(
        &self,
        iid: u64,
        updates: &IssueUpdate,
    ) -> Result<Issue, Box<dyn std::error::Error>> {
        let body = serde_json::to_value(updates)?;
        let resp = self
            .http
            .put(format!("{}/{iid}", self.issues_url()))
            .json(&body)
            .send()?;
        check_response(resp)
    }

    /// Fetch the work-item status for an issue via GraphQL.
    ///
    /// Returns the status name (e.g. "To do", "In progress")
    /// or `None` if the work-item has no status widget.
    pub fn get_work_item_status(
        &self,
        iid: u64,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let query = format!(
            r#"{{ project(fullPath: "{}") {{
                workItems(iids: ["{}"])  {{
                    nodes {{ widgets {{
                        type
                        ... on WorkItemWidgetStatus {{
                            status {{ name }}
                        }}
                    }} }}
                }}
            }} }}"#,
            self.project_path, iid
        );
        let body = serde_json::json!({ "query": query });
        let resp = self.http.post(self.graphql_url()).json(&body).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab GraphQL error {status}: {text}").into());
        }
        let json: serde_json::Value = resp.json()?;
        Ok(parse_work_item_status(&json))
    }

    /// Set the start / due dates on a work item via GraphQL.
    ///
    /// GitLab's REST `PUT /issues/:iid` endpoint silently
    /// ignores `start_date` and doesn't reliably honor
    /// `due_date` for work items, so date updates go through
    /// the `workItemUpdate` mutation's `startAndDueDateWidget`.
    /// Passing `None` for a field leaves it unchanged; passing
    /// `Some("")` clears it.
    pub fn set_work_item_dates(
        &self,
        iid: u64,
        start_date: Option<&str>,
        due_date: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if start_date.is_none() && due_date.is_none() {
            return Ok(());
        }
        let work_item_id = self.get_work_item_id(iid)?;
        let mut widget_fields: Vec<String> = Vec::new();
        if let Some(sd) = start_date {
            widget_fields.push(format!(r#"startDate: "{sd}""#));
        }
        if let Some(dd) = due_date {
            widget_fields.push(format!(r#"dueDate: "{dd}""#));
        }
        let query = format!(
            r#"mutation {{
                workItemUpdate(input: {{
                    id: "{work_item_id}"
                    startAndDueDateWidget: {{ {} }}
                }}) {{
                    errors
                }}
            }}"#,
            widget_fields.join(" "),
        );
        let body = serde_json::json!({ "query": query });
        let resp = self.http.post(self.graphql_url()).json(&body).send()?;
        if !resp.status().is_success() {
            let http_status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab GraphQL error {http_status}: {text}").into());
        }
        let json: serde_json::Value = resp.json()?;
        if let Some(errors) = parse_mutation_errors(&json) {
            return Err(format!("workItemUpdate errors: {errors:?}").into());
        }
        Ok(())
    }

    /// Set the work-item status for an issue via GraphQL.
    ///
    /// Resolves `status` (e.g. "In progress") to its Global ID
    /// by querying the project's allowed statuses, then sends a
    /// `workItemUpdate` mutation.
    pub fn set_work_item_status(
        &self,
        iid: u64,
        status: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let work_item_id = self.get_work_item_id(iid)?;
        let status_id = self.resolve_status_id(status)?;
        let query = format!(
            r#"mutation {{
                workItemUpdate(input: {{
                    id: "{work_item_id}"
                    statusWidget: {{ status: "{status_id}" }}
                }}) {{
                    errors
                }}
            }}"#,
        );
        let body = serde_json::json!({ "query": query });
        let resp = self.http.post(self.graphql_url()).json(&body).send()?;
        if !resp.status().is_success() {
            let http_status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab GraphQL error {http_status}: {text}").into());
        }
        let json: serde_json::Value = resp.json()?;
        if let Some(errors) = parse_mutation_errors(&json) {
            return Err(format!("workItemUpdate errors: {errors:?}").into());
        }
        Ok(())
    }

    /// Fetch the global ID of a work item by IID.
    fn get_work_item_id(&self, iid: u64) -> Result<String, Box<dyn std::error::Error>> {
        let query = format!(
            r#"{{ project(fullPath: "{}") {{
                workItems(iids: ["{}"])  {{
                    nodes {{ id }}
                }}
            }} }}"#,
            self.project_path, iid
        );
        let body = serde_json::json!({ "query": query });
        let resp = self.http.post(self.graphql_url()).json(&body).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab GraphQL error {status}: {text}").into());
        }
        let json: serde_json::Value = resp.json()?;
        parse_work_item_id(&json).ok_or_else(|| "work item not found".into())
    }

    /// Resolve a status name to its Global ID.
    fn resolve_status_id(&self, name: &str) -> Result<String, Box<dyn std::error::Error>> {
        let query = format!(
            r#"{{ project(fullPath: "{}") {{
                workItemTypes(name: ISSUE) {{
                    nodes {{
                        widgetDefinitions {{
                            type
                            ... on WorkItemWidgetDefinitionStatus {{
                                allowedStatuses {{ id name }}
                            }}
                        }}
                    }}
                }}
            }} }}"#,
            self.project_path
        );
        let body = serde_json::json!({ "query": query });
        let resp = self.http.post(self.graphql_url()).json(&body).send()?;
        if !resp.status().is_success() {
            let http_status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab GraphQL error {http_status}: {text}").into());
        }
        let json: serde_json::Value = resp.json()?;
        parse_status_id(&json, name)
            .ok_or_else(|| format!("status {name:?} not found in project").into())
    }

    fn issues_url(&self) -> String {
        let encoded = self.project_path.replace('/', "%2F");
        format!("{}/api/v4/projects/{}/issues", self.base_url, encoded)
    }

    fn graphql_url(&self) -> String {
        format!("{}/api/graphql", self.base_url)
    }
}

/// Extract the status name from a GraphQL work-item response.
fn parse_work_item_status(json: &serde_json::Value) -> Option<String> {
    json.pointer("/data/project/workItems/nodes/0/widgets")
        .and_then(|w| w.as_array())
        .and_then(|widgets| {
            widgets
                .iter()
                .find(|w| w.get("type").and_then(|t| t.as_str()) == Some("STATUS"))
        })
        .and_then(|w| w.pointer("/status/name"))
        .and_then(|n| n.as_str())
        .map(String::from)
}

/// Extract the global ID from a GraphQL work-item response.
fn parse_work_item_id(json: &serde_json::Value) -> Option<String> {
    json.pointer("/data/project/workItems/nodes/0/id")
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// Extract mutation errors from a workItemUpdate response.
fn parse_mutation_errors(json: &serde_json::Value) -> Option<Vec<String>> {
    let errors = json.pointer("/data/workItemUpdate/errors")?.as_array()?;
    if errors.is_empty() {
        return None;
    }
    Some(
        errors
            .iter()
            .filter_map(|e| e.as_str().map(String::from))
            .collect(),
    )
}

/// Find the Global ID of a status by name from an `allowedStatuses` GraphQL response.
fn parse_status_id(json: &serde_json::Value, name: &str) -> Option<String> {
    let types = json
        .pointer("/data/project/workItemTypes/nodes")?
        .as_array()?;
    for work_item_type in types {
        let defs = work_item_type.get("widgetDefinitions")?.as_array()?;
        for def in defs {
            if def.get("type").and_then(|t| t.as_str()) != Some("STATUS") {
                continue;
            }
            let statuses = def.get("allowedStatuses")?.as_array()?;
            for status in statuses {
                if status.get("name").and_then(|n| n.as_str()) == Some(name) {
                    return status.get("id").and_then(|v| v.as_str()).map(String::from);
                }
            }
        }
    }
    None
}

/// Client for group-level GitLab API queries.
pub struct GroupClient {
    http: reqwest::blocking::Client,
    base_url: String,
    group_path: String,
}

impl GroupClient {
    /// Create a group client from a GitLab group URL and token.
    pub fn from_group_url(url: &str, token: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let (base_url, group_path) = parse_project_url(url)?;
        Self::new(&base_url, &group_path, token)
    }

    /// Create a group client with explicit parameters.
    pub fn new(
        base_url: &str,
        group_path: &str,
        token: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let http = build_http_client(token)?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            group_path: group_path.to_string(),
        })
    }

    /// List all issues in the group matching a label,
    /// handling pagination automatically.
    pub fn list_issues(
        &self,
        label: &str,
        state: Option<&str>,
    ) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
        let mut all_issues = Vec::new();
        let mut page = 1u32;
        loop {
            let page_str = page.to_string();
            let mut query = vec![("labels", label), ("per_page", "100"), ("page", &page_str)];
            if let Some(s) = state {
                query.push(("state", s));
            }
            let resp = self.http.get(self.issues_url()).query(&query).send()?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text()?;
                return Err(format!("GitLab API error {status}: {text}").into());
            }
            let next_page = resp
                .headers()
                .get("x-next-page")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let issues: Vec<Issue> = resp.json()?;
            all_issues.extend(issues);
            if next_page.is_empty() {
                break;
            }
            page = next_page.parse()?;
        }
        Ok(all_issues)
    }

    /// Fetch the work-item status for an issue via GraphQL.
    pub fn get_work_item_status(
        &self,
        project_path: &str,
        iid: u64,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        let query = format!(
            r#"{{ project(fullPath: "{}") {{
                workItems(iids: ["{}"])  {{
                    nodes {{ widgets {{
                        type
                        ... on WorkItemWidgetStatus {{
                            status {{ name }}
                        }}
                    }} }}
                }}
            }} }}"#,
            project_path, iid
        );
        let body = serde_json::json!({ "query": query });
        let resp = self.http.post(self.graphql_url()).json(&body).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab GraphQL error {status}: {text}").into());
        }
        let json: serde_json::Value = resp.json()?;
        Ok(parse_work_item_status(&json))
    }

    fn issues_url(&self) -> String {
        let encoded = self.group_path.replace('/', "%2F");
        format!("{}/api/v4/groups/{}/issues", self.base_url, encoded)
    }

    fn graphql_url(&self) -> String {
        format!("{}/api/graphql", self.base_url)
    }
}

/// Split a GitLab issue or work item URL just before the
/// `/-/issues/<n>` or `/-/work_items/<n>` tail, returning the
/// project portion. When neither separator is present, returns
/// the input unchanged so callers that pass bare project URLs
/// still get a useful result.
fn project_part_of_issue_url(web_url: &str) -> &str {
    for sep in ["/-/issues/", "/-/work_items/"] {
        if let Some(idx) = web_url.find(sep) {
            return &web_url[..idx];
        }
    }
    web_url
}

/// Extract the package name from a GitLab issue or work item
/// web_url.
///
/// Example: `"https://gitlab.com/CentOS/Hyperscale/rpms/ethtool/-/issues/1"`
/// returns `Some("ethtool")`. Also accepts the work-items form
/// `"...ethtool/-/work_items/1"`.
pub fn package_from_issue_url(web_url: &str) -> Option<&str> {
    let project_part = project_part_of_issue_url(web_url);
    let name = project_part.rsplit('/').next()?;
    if name.is_empty() { None } else { Some(name) }
}

/// Extract the project path from a GitLab issue or work item
/// web_url.
///
/// Example: `"https://gitlab.com/CentOS/Hyperscale/rpms/ethtool/-/issues/1"`
/// returns `Some("CentOS/Hyperscale/rpms/ethtool")`. Also
/// accepts the work-items form.
pub fn project_path_from_issue_url(web_url: &str) -> Option<String> {
    let project_part = project_part_of_issue_url(web_url);
    let rest = project_part
        .strip_prefix("https://")
        .or_else(|| project_part.strip_prefix("http://"))?;
    let slash = rest.find('/')?;
    let path = &rest[slash + 1..];
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

/// Parameters for editing an issue.
#[derive(Debug, Default, serde::Serialize)]
pub struct IssueUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add_labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove_labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_event: Option<String>,
    /// ISO-8601 date string (YYYY-MM-DD). GitLab stores this
    /// on the issue as its start date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    /// ISO-8601 date string (YYYY-MM-DD). GitLab stores this
    /// on the issue as its due date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
}

fn check_response(resp: reqwest::blocking::Response) -> Result<Issue, Box<dyn std::error::Error>> {
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text()?;
        return Err(format!("GitLab API error {status}: {text}").into());
    }
    Ok(resp.json()?)
}

/// Check whether a token is valid by calling `GET /api/v4/user`.
pub fn validate_token(base_url: &str, token: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("private-token"),
        HeaderValue::from_str(token)?,
    );
    let client = reqwest::blocking::Client::builder()
        .user_agent("sandogasa-gitlab/0.6.2")
        .default_headers(headers)
        .build()?;
    let url = format!("{}/api/v4/user", base_url.trim_end_matches('/'));
    let resp = client.get(&url).send()?;
    Ok(resp.status().is_success())
}

/// A project name returned by the GitLab group projects API.
#[derive(Debug, Deserialize)]
pub struct GroupProject {
    pub name: String,
    pub path: String,
}

/// List all projects under a GitLab group (public, no auth needed).
///
/// `group_url` is the full URL, e.g.
/// `https://gitlab.com/CentOS/Hyperscale/rpms`.
/// Paginates automatically and retries on 500/502/503/504.
pub fn list_group_projects(
    group_url: &str,
) -> Result<Vec<GroupProject>, Box<dyn std::error::Error>> {
    let (base_url, group_path) = parse_project_url(group_url)?;
    let encoded = group_path.replace('/', "%2F");
    let client = reqwest::blocking::Client::builder()
        .user_agent("sandogasa-gitlab")
        .build()?;
    let mut all = Vec::new();
    let mut page = 1u32;
    loop {
        let url = format!(
            "{}/api/v4/groups/{}/projects?per_page=100&page={}&simple=true&include_subgroups=false",
            base_url, encoded, page
        );
        eprint!("\r  fetching page {page}...");
        let resp = get_with_retry_blocking(&client, &url)?;
        let next_page = resp
            .headers()
            .get("x-next-page")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let projects: Vec<GroupProject> = resp.json()?;
        all.extend(projects);
        if next_page.is_empty() {
            break;
        }
        page = next_page.parse()?;
    }
    eprintln!("\r  fetched {} project(s)", all.len());
    Ok(all)
}

/// Blocking GET with retry on transient server errors.
fn get_with_retry_blocking(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<reqwest::blocking::Response, Box<dyn std::error::Error>> {
    let mut last_err = None;
    for attempt in 0..=3u32 {
        let resp = client.get(url).send()?;
        let status = resp.status();
        if status == reqwest::StatusCode::INTERNAL_SERVER_ERROR
            || status == reqwest::StatusCode::BAD_GATEWAY
            || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
            || status == reqwest::StatusCode::GATEWAY_TIMEOUT
        {
            let delay = std::time::Duration::from_secs(1 << attempt);
            eprintln!(
                "  {status}, retrying in {}s ({}/3)",
                delay.as_secs(),
                attempt + 1,
            );
            std::thread::sleep(delay);
            last_err = Some(format!("{status} for {url}"));
            continue;
        }
        if !resp.status().is_success() {
            let text = resp.text()?;
            return Err(format!("GitLab API error {status}: {text}").into());
        }
        return Ok(resp);
    }
    Err(last_err.unwrap().into())
}

/// Parse a GitLab project URL into (base_url, project_path).
///
/// Example: `https://gitlab.com/CentOS/Hyperscale/rpms/perf`
/// returns `("https://gitlab.com", "CentOS/Hyperscale/rpms/perf")`
pub fn parse_project_url(url: &str) -> Result<(String, String), String> {
    let url = url.trim_end_matches('/');
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| format!("invalid GitLab URL: {url}"))?;

    let slash = rest
        .find('/')
        .ok_or_else(|| format!("no project path in URL: {url}"))?;

    let host = &rest[..slash];
    let path = &rest[slash + 1..];

    if path.is_empty() {
        return Err(format!("no project path in URL: {url}"));
    }

    let scheme = if url.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    Ok((format!("{scheme}://{host}"), path.to_string()))
}

/// Parse a merge request URL into its components.
///
/// Example:
/// `https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42`
/// returns `("https://gitlab.com", "redhat/centos-stream/rpms/xz", 42)`.
pub fn parse_mr_url(url: &str) -> Result<(String, String, u64), String> {
    let trimmed = url.trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .ok_or_else(|| format!("invalid GitLab URL: {url}"))?;
    let slash = rest
        .find('/')
        .ok_or_else(|| format!("no project path in URL: {url}"))?;
    let host = &rest[..slash];
    let path = &rest[slash + 1..];

    let scheme = if trimmed.starts_with("https://") {
        "https"
    } else {
        "http"
    };

    let (project, iid_str) = path
        .rsplit_once("/-/merge_requests/")
        .ok_or_else(|| format!("not a merge request URL: {url}"))?;
    // `iid_str` may have trailing query or fragment; strip them.
    let iid_str = iid_str.split(['?', '#']).next().unwrap_or(iid_str);
    let iid: u64 = iid_str
        .parse()
        .map_err(|_| format!("invalid merge request IID in URL: {url}"))?;

    if project.is_empty() {
        return Err(format!("no project path in URL: {url}"));
    }

    Ok((format!("{scheme}://{host}"), project.to_string(), iid))
}

/// Parse a GitLab issue / work-item URL into its components.
///
/// Accepts both the legacy `/-/issues/<n>` path and the newer
/// `/-/work_items/<n>` form. Example:
/// `https://gitlab.com/CentOS/proposed_updates/rpms/xz/-/work_items/1`
/// returns `("https://gitlab.com", "CentOS/proposed_updates/rpms/xz", 1)`.
pub fn parse_issue_url(url: &str) -> Result<(String, String, u64), String> {
    let trimmed = url.trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .ok_or_else(|| format!("invalid GitLab URL: {url}"))?;
    let slash = rest
        .find('/')
        .ok_or_else(|| format!("no project path in URL: {url}"))?;
    let host = &rest[..slash];
    let path = &rest[slash + 1..];

    let scheme = if trimmed.starts_with("https://") {
        "https"
    } else {
        "http"
    };

    let (project, iid_str) = path
        .rsplit_once("/-/issues/")
        .or_else(|| path.rsplit_once("/-/work_items/"))
        .ok_or_else(|| format!("not an issue or work-item URL: {url}"))?;
    let iid_str = iid_str.split(['?', '#']).next().unwrap_or(iid_str);
    let iid: u64 = iid_str
        .parse()
        .map_err(|_| format!("invalid issue IID in URL: {url}"))?;

    if project.is_empty() {
        return Err(format!("no project path in URL: {url}"));
    }

    Ok((format!("{scheme}://{host}"), project.to_string(), iid))
}

/// A GitLab user as returned by `/users?username=<name>`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct User {
    pub id: u64,
    pub username: String,
}

/// Look up a user by username on a specific GitLab instance.
/// Returns `Ok(None)` if the server returns 200 with an empty list
/// (no user with that name on that instance).
pub fn user_by_username(
    base_url: &str,
    token: &str,
    username: &str,
) -> Result<Option<User>, Box<dyn std::error::Error>> {
    let http = build_http_client(token)?;
    let url = format!("{}/api/v4/users", base_url.trim_end_matches('/'));
    let resp = http.get(&url).query(&[("username", username)]).send()?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text()?;
        return Err(format!("GitLab GET {url} failed: {status}: {text}").into());
    }
    let users: Vec<User> = resp.json()?;
    Ok(users.into_iter().next())
}

/// One entry from the user-activity events endpoint. Fields are
/// sparse — GitLab only populates the ones relevant to each
/// `action_name`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Event {
    pub id: u64,
    pub project_id: u64,
    pub action_name: String,
    #[serde(default)]
    pub target_type: Option<String>,
    #[serde(default)]
    pub target_iid: Option<u64>,
    #[serde(default)]
    pub target_title: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub note: Option<EventNote>,
    #[serde(default)]
    pub push_data: Option<EventPushData>,
}

/// Note payload attached to `commented on` events.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventNote {
    #[serde(default)]
    pub noteable_type: Option<String>,
    #[serde(default)]
    pub noteable_iid: Option<u64>,
    #[serde(default)]
    pub body: Option<String>,
}

/// Push payload attached to `pushed to` / `pushed new` events.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EventPushData {
    #[serde(default)]
    pub commit_count: u64,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub ref_type: Option<String>,
    #[serde(default, rename = "ref")]
    pub ref_name: Option<String>,
    #[serde(default)]
    pub commit_title: Option<String>,
}

/// Fetch a user's activity events within `[after, before)` — GitLab's
/// event endpoint is half-open on both sides and rejects both-null.
/// Results are paginated at 100/page; this follows every page until
/// a short page arrives.
///
/// Callers that want events on a closed `[since, until]` day range
/// should pass `after = since - 1` and `before = until + 1`, since
/// events ON the boundary day are excluded by GitLab.
pub fn user_events(
    base_url: &str,
    token: &str,
    user_id: u64,
    action: Option<&str>,
    after: chrono::NaiveDate,
    before: chrono::NaiveDate,
) -> Result<Vec<Event>, Box<dyn std::error::Error>> {
    let http = build_http_client(token)?;
    let endpoint = format!(
        "{}/api/v4/users/{}/events",
        base_url.trim_end_matches('/'),
        user_id
    );
    let after_str = after.to_string();
    let before_str = before.to_string();
    let mut out: Vec<Event> = Vec::new();
    let mut page = 1u32;
    loop {
        let page_str = page.to_string();
        let mut query: Vec<(&str, &str)> = vec![
            ("per_page", "100"),
            ("page", &page_str),
            ("after", &after_str),
            ("before", &before_str),
        ];
        if let Some(a) = action {
            query.push(("action", a));
        }
        let resp = http.get(&endpoint).query(&query).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab GET {endpoint} failed: {status}: {text}").into());
        }
        let batch: Vec<Event> = resp.json()?;
        let n = batch.len();
        out.extend(batch);
        if n < 100 {
            break;
        }
        page += 1;
    }
    Ok(out)
}

/// Minimal project identity: what you need to filter events by
/// `path_with_namespace` prefix and render a human-readable link.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectSummary {
    pub id: u64,
    pub path_with_namespace: String,
    pub web_url: String,
}

/// Look up a project's `path_with_namespace` from its numeric ID.
/// Used to map event `project_id` → group-prefix filter.
pub fn project_summary(
    base_url: &str,
    token: &str,
    project_id: u64,
) -> Result<ProjectSummary, Box<dyn std::error::Error>> {
    let http = build_http_client(token)?;
    let url = format!(
        "{}/api/v4/projects/{}",
        base_url.trim_end_matches('/'),
        project_id
    );
    let resp = http.get(&url).send()?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text()?;
        return Err(format!("GitLab GET {url} failed: {status}: {text}").into());
    }
    Ok(resp.json()?)
}

/// Count commits in `project_id` authored by `author` within
/// `[since, until]` (inclusive). GitLab's commits endpoint
/// matches the `author` parameter against both name and email
/// fields, so passing the GitLab username usually works; if the
/// user authored commits under an email only, pass that email
/// string instead.
///
/// Intended as a cross-check against the push-event count: a big
/// gap (pushed >> authored) flags mirror activity — the user
/// pushed commits they didn't author.
pub fn count_authored_commits(
    base_url: &str,
    token: &str,
    project_id: u64,
    author: &str,
    since: chrono::NaiveDate,
    until: chrono::NaiveDate,
) -> Result<u64, Box<dyn std::error::Error>> {
    let http = build_http_client(token)?;
    let endpoint = format!(
        "{}/api/v4/projects/{}/repository/commits",
        base_url.trim_end_matches('/'),
        project_id
    );
    // Both bounds are inclusive on this endpoint (unlike the
    // events endpoint), so pass the days as-is.
    let since_str = format!("{since}T00:00:00Z");
    let until_str = format!("{until}T23:59:59Z");
    let mut total: u64 = 0;
    let mut page = 1u32;
    loop {
        let page_str = page.to_string();
        let query: Vec<(&str, &str)> = vec![
            ("per_page", "100"),
            ("page", &page_str),
            ("author", author),
            ("since", &since_str),
            ("until", &until_str),
        ];
        let resp = http.get(&endpoint).query(&query).send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("GitLab GET {endpoint} failed: {status}: {text}").into());
        }
        // The endpoint returns an array; we only need the length
        // so decode as a generic array and count.
        let batch: Vec<serde_json::Value> = resp.json()?;
        let n = batch.len() as u64;
        total += n;
        if n < 100 {
            break;
        }
        page += 1;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_project_url() {
        let (base, path) =
            parse_project_url("https://gitlab.com/CentOS/Hyperscale/rpms/perf").unwrap();
        assert_eq!(base, "https://gitlab.com");
        assert_eq!(path, "CentOS/Hyperscale/rpms/perf");
    }

    #[test]
    fn test_parse_project_url_trailing_slash() {
        let (base, path) = parse_project_url("https://gitlab.com/group/project/").unwrap();
        assert_eq!(base, "https://gitlab.com");
        assert_eq!(path, "group/project");
    }

    #[test]
    fn test_parse_project_url_http() {
        let (base, path) = parse_project_url("http://gitlab.example.com/group/project").unwrap();
        assert_eq!(base, "http://gitlab.example.com");
        assert_eq!(path, "group/project");
    }

    #[test]
    fn test_parse_project_url_no_scheme() {
        assert!(parse_project_url("gitlab.com/group/project").is_err());
    }

    #[test]
    fn test_parse_project_url_no_path() {
        assert!(parse_project_url("https://gitlab.com/").is_err());
        assert!(parse_project_url("https://gitlab.com").is_err());
    }

    #[test]
    fn test_issues_url() {
        let client = Client::new(
            "https://gitlab.com",
            "CentOS/Hyperscale/rpms/perf",
            "fake-token",
        )
        .unwrap();
        assert_eq!(
            client.issues_url(),
            "https://gitlab.com/api/v4/projects/CentOS%2FHyperscale%2Frpms%2Fperf/issues"
        );
    }

    #[test]
    fn test_issue_update_serialization() {
        let update = IssueUpdate {
            title: Some("new title".into()),
            add_labels: Some("bug".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&update).unwrap();
        assert_eq!(json["title"], "new title");
        assert_eq!(json["add_labels"], "bug");
        assert!(json.get("description").is_none());
        assert!(json.get("state_event").is_none());
    }

    #[test]
    fn test_issue_deserialize() {
        let json = r#"{
            "iid": 42,
            "title": "Test issue",
            "description": "Some description",
            "state": "opened",
            "web_url": "https://gitlab.com/group/project/-/issues/42",
            "assignees": [
                {"username": "alice"},
                {"username": "bob"}
            ]
        }"#;
        let issue: Issue = serde_json::from_str(json).unwrap();
        assert_eq!(issue.iid, 42);
        assert_eq!(issue.title, "Test issue");
        assert_eq!(issue.description.as_deref(), Some("Some description"));
        assert_eq!(issue.state, "opened");
        assert_eq!(issue.assignees.len(), 2);
        assert_eq!(issue.assignees[0].username, "alice");
        assert_eq!(issue.assignees[1].username, "bob");
    }

    #[test]
    fn test_issue_deserialize_no_assignees() {
        let json =
            r#"{"iid": 1, "title": "t", "description": null, "state": "opened", "web_url": "u"}"#;
        let issue: Issue = serde_json::from_str(json).unwrap();
        assert!(issue.description.is_none());
        assert!(issue.assignees.is_empty());
    }

    #[test]
    fn test_graphql_url() {
        let client = Client::new(
            "https://gitlab.com",
            "CentOS/Hyperscale/rpms/perf",
            "fake-token",
        )
        .unwrap();
        assert_eq!(client.graphql_url(), "https://gitlab.com/api/graphql");
    }

    #[test]
    fn test_parse_work_item_status_found() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"data":{"project":{"workItems":{"nodes":[{"widgets":[{"type":"ASSIGNEES"},{"type":"STATUS","status":{"name":"To do"}}]}]}}}}"#,
        ).unwrap();
        assert_eq!(parse_work_item_status(&json).as_deref(), Some("To do"));
    }

    #[test]
    fn test_parse_work_item_status_in_progress() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"data":{"project":{"workItems":{"nodes":[{"widgets":[{"type":"STATUS","status":{"name":"In progress"}}]}]}}}}"#,
        ).unwrap();
        assert_eq!(
            parse_work_item_status(&json).as_deref(),
            Some("In progress")
        );
    }

    #[test]
    fn test_parse_work_item_status_no_status_widget() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"data":{"project":{"workItems":{"nodes":[{"widgets":[{"type":"ASSIGNEES"},{"type":"LABELS"}]}]}}}}"#,
        ).unwrap();
        assert!(parse_work_item_status(&json).is_none());
    }

    #[test]
    fn test_parse_work_item_status_empty_nodes() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"data":{"project":{"workItems":{"nodes":[]}}}}"#).unwrap();
        assert!(parse_work_item_status(&json).is_none());
    }

    #[test]
    fn test_parse_work_item_status_null_status() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"data":{"project":{"workItems":{"nodes":[{"widgets":[{"type":"STATUS","status":null}]}]}}}}"#,
        ).unwrap();
        assert!(parse_work_item_status(&json).is_none());
    }

    #[test]
    fn test_package_from_issue_url() {
        assert_eq!(
            package_from_issue_url("https://gitlab.com/CentOS/Hyperscale/rpms/ethtool/-/issues/1"),
            Some("ethtool")
        );
        assert_eq!(
            package_from_issue_url("https://gitlab.com/group/project/-/issues/42"),
            Some("project")
        );
    }

    #[test]
    fn test_package_from_issue_url_no_issues_path() {
        assert_eq!(
            package_from_issue_url("https://gitlab.com/group/project"),
            Some("project")
        );
    }

    #[test]
    fn test_package_from_issue_url_empty() {
        assert_eq!(package_from_issue_url(""), None);
    }

    #[test]
    fn test_package_from_issue_url_work_items_form() {
        assert_eq!(
            package_from_issue_url(
                "https://gitlab.com/CentOS/proposed_updates/rpms/PackageKit/-/work_items/1"
            ),
            Some("PackageKit"),
        );
    }

    #[test]
    fn test_project_path_from_issue_url_work_items_form() {
        assert_eq!(
            project_path_from_issue_url(
                "https://gitlab.com/CentOS/proposed_updates/rpms/PackageKit/-/work_items/1"
            )
            .as_deref(),
            Some("CentOS/proposed_updates/rpms/PackageKit"),
        );
    }

    #[test]
    fn test_project_path_from_issue_url() {
        assert_eq!(
            project_path_from_issue_url(
                "https://gitlab.com/CentOS/Hyperscale/rpms/ethtool/-/issues/1"
            )
            .as_deref(),
            Some("CentOS/Hyperscale/rpms/ethtool")
        );
    }

    #[test]
    fn test_project_path_from_issue_url_no_issues() {
        assert_eq!(
            project_path_from_issue_url("https://gitlab.com/group/project").as_deref(),
            Some("group/project")
        );
    }

    #[test]
    fn test_project_path_from_issue_url_no_scheme() {
        assert!(project_path_from_issue_url("gitlab.com/group/project").is_none());
    }

    #[test]
    fn test_parse_work_item_id_found() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"data":{"project":{"workItems":{"nodes":[{"id":"gid://gitlab/WorkItem/42"}]}}}}"#,
        )
        .unwrap();
        assert_eq!(
            parse_work_item_id(&json).as_deref(),
            Some("gid://gitlab/WorkItem/42")
        );
    }

    #[test]
    fn test_parse_work_item_id_empty() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"data":{"project":{"workItems":{"nodes":[]}}}}"#).unwrap();
        assert!(parse_work_item_id(&json).is_none());
    }

    #[test]
    fn test_parse_mutation_errors_none() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"data":{"workItemUpdate":{"errors":[]}}}"#).unwrap();
        assert!(parse_mutation_errors(&json).is_none());
    }

    #[test]
    fn test_parse_mutation_errors_present() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"data":{"workItemUpdate":{"errors":["something went wrong"]}}}"#,
        )
        .unwrap();
        let errors = parse_mutation_errors(&json).unwrap();
        assert_eq!(errors, vec!["something went wrong"]);
    }

    #[test]
    fn test_parse_status_id_found() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"data":{"project":{"workItemTypes":{"nodes":[{"widgetDefinitions":[{"type":"ASSIGNEES"},{"type":"STATUS","allowedStatuses":[{"id":"gid://gitlab/WorkItems::Statuses::Custom::Status/1","name":"To do"},{"id":"gid://gitlab/WorkItems::Statuses::Custom::Status/2","name":"In progress"}]}]}]}}}}"#,
        ).unwrap();
        assert_eq!(
            parse_status_id(&json, "In progress").as_deref(),
            Some("gid://gitlab/WorkItems::Statuses::Custom::Status/2")
        );
    }

    #[test]
    fn test_parse_status_id_not_found() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"data":{"project":{"workItemTypes":{"nodes":[{"widgetDefinitions":[{"type":"STATUS","allowedStatuses":[{"id":"gid://id/1","name":"To do"}]}]}]}}}}"#,
        ).unwrap();
        assert!(parse_status_id(&json, "In progress").is_none());
    }

    #[test]
    fn test_group_client_issues_url() {
        let client =
            GroupClient::new("https://gitlab.com", "CentOS/Hyperscale/rpms", "fake-token").unwrap();
        assert_eq!(
            client.issues_url(),
            "https://gitlab.com/api/v4/groups/CentOS%2FHyperscale%2Frpms/issues"
        );
    }

    #[test]
    fn test_group_client_graphql_url() {
        let client =
            GroupClient::new("https://gitlab.com", "CentOS/Hyperscale/rpms", "fake-token").unwrap();
        assert_eq!(client.graphql_url(), "https://gitlab.com/api/graphql");
    }

    #[test]
    fn test_add_note_success() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v4/projects/g%2Fp/issues/1/notes")
            .match_header("private-token", "tok")
            .match_body(mockito::Matcher::Json(serde_json::json!({"body": "hello"})))
            .with_status(201)
            .with_body("{}")
            .create();
        let client = Client::new(&server.url(), "g/p", "tok").unwrap();
        client.add_note(1, "hello").unwrap();
        mock.assert();
    }

    #[test]
    fn test_add_note_error() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v4/projects/g%2Fp/issues/1/notes")
            .with_status(403)
            .with_body("forbidden")
            .create();
        let client = Client::new(&server.url(), "g/p", "tok").unwrap();
        let err = client.add_note(1, "x").unwrap_err();
        assert!(err.to_string().contains("403"), "{}", err);
        mock.assert();
    }

    #[test]
    fn test_edit_issue_success() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("PUT", "/api/v4/projects/g%2Fp/issues/5")
            .match_header("private-token", "tok")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"iid":5,"title":"t","description":null,"state":"closed","web_url":"https://example.com/-/issues/5"}"#)
            .create();
        let client = Client::new(&server.url(), "g/p", "tok").unwrap();
        let updates = IssueUpdate {
            state_event: Some("close".into()),
            ..Default::default()
        };
        let issue = client.edit_issue(5, &updates).unwrap();
        assert_eq!(issue.state, "closed");
        mock.assert();
    }

    #[test]
    fn test_edit_issue_error() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("PUT", "/api/v4/projects/g%2Fp/issues/5")
            .with_status(404)
            .with_body("not found")
            .create();
        let client = Client::new(&server.url(), "g/p", "tok").unwrap();
        let updates = IssueUpdate::default();
        let err = client.edit_issue(5, &updates).unwrap_err();
        assert!(err.to_string().contains("404"), "{}", err);
        mock.assert();
    }

    #[test]
    fn test_create_issue_success() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/api/v4/projects/g%2Fp/issues")
            .match_header("private-token", "tok")
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(r#"{"iid":10,"title":"new issue","description":"desc","state":"opened","web_url":"https://example.com/-/issues/10"}"#)
            .create();
        let client = Client::new(&server.url(), "g/p", "tok").unwrap();
        let issue = client
            .create_issue("new issue", Some("desc"), Some("bug"))
            .unwrap();
        assert_eq!(issue.iid, 10);
        assert_eq!(issue.title, "new issue");
        mock.assert();
    }

    #[test]
    fn test_list_issues_success() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v4/projects/g%2Fp/issues")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("labels".into(), "relmon".into()),
                mockito::Matcher::UrlEncoded("state".into(), "opened".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"[{"iid":1,"title":"t","description":null,"state":"opened","web_url":"u"}]"#,
            )
            .create();
        let client = Client::new(&server.url(), "g/p", "tok").unwrap();
        let issues = client.list_issues("relmon", Some("opened")).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].iid, 1);
        mock.assert();
    }

    #[test]
    fn test_list_issues_error() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v4/projects/g%2Fp/issues")
            .match_query(mockito::Matcher::Any)
            .with_status(500)
            .with_body("internal error")
            .create();
        let client = Client::new(&server.url(), "g/p", "tok").unwrap();
        let err = client.list_issues("relmon", None).unwrap_err();
        assert!(err.to_string().contains("500"), "{}", err);
        mock.assert();
    }

    // --- parse_mr_url ---

    #[test]
    fn parse_mr_url_standard() {
        let (base, project, iid) =
            parse_mr_url("https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42")
                .unwrap();
        assert_eq!(base, "https://gitlab.com");
        assert_eq!(project, "redhat/centos-stream/rpms/xz");
        assert_eq!(iid, 42);
    }

    #[test]
    fn parse_mr_url_strips_trailing_slash() {
        let (_, _, iid) =
            parse_mr_url("https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42/")
                .unwrap();
        assert_eq!(iid, 42);
    }

    #[test]
    fn parse_mr_url_strips_query() {
        let (_, _, iid) =
            parse_mr_url("https://gitlab.com/a/b/-/merge_requests/7?commit_id=abc").unwrap();
        assert_eq!(iid, 7);
    }

    #[test]
    fn parse_mr_url_strips_fragment() {
        let (_, _, iid) =
            parse_mr_url("https://gitlab.com/a/b/-/merge_requests/7#note_123").unwrap();
        assert_eq!(iid, 7);
    }

    #[test]
    fn parse_mr_url_rejects_issue_url() {
        assert!(parse_mr_url("https://gitlab.com/a/b/-/issues/1").is_err());
    }

    #[test]
    fn parse_mr_url_rejects_non_numeric_iid() {
        assert!(parse_mr_url("https://gitlab.com/a/b/-/merge_requests/abc").is_err());
    }

    #[test]
    fn parse_mr_url_rejects_no_scheme() {
        assert!(parse_mr_url("gitlab.com/a/b/-/merge_requests/1").is_err());
    }

    #[test]
    fn parse_issue_url_handles_legacy_form() {
        let (base, project, iid) =
            parse_issue_url("https://gitlab.com/group/project/-/issues/42").unwrap();
        assert_eq!(base, "https://gitlab.com");
        assert_eq!(project, "group/project");
        assert_eq!(iid, 42);
    }

    #[test]
    fn parse_issue_url_handles_work_items_form() {
        let (base, project, iid) =
            parse_issue_url("https://gitlab.com/CentOS/proposed_updates/rpms/xz/-/work_items/1")
                .unwrap();
        assert_eq!(base, "https://gitlab.com");
        assert_eq!(project, "CentOS/proposed_updates/rpms/xz");
        assert_eq!(iid, 1);
    }

    #[test]
    fn parse_issue_url_strips_query_and_fragment() {
        let (_, _, iid) =
            parse_issue_url("https://gitlab.com/a/b/-/work_items/7?note=123#xyz").unwrap();
        assert_eq!(iid, 7);
    }

    #[test]
    fn parse_issue_url_rejects_mr_url() {
        assert!(parse_issue_url("https://gitlab.com/a/b/-/merge_requests/1").is_err());
    }

    #[test]
    fn parse_issue_url_rejects_non_numeric_iid() {
        assert!(parse_issue_url("https://gitlab.com/a/b/-/issues/xyz").is_err());
    }

    #[test]
    fn user_by_username_returns_first_match() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v4/users?username=alice")
            .match_header("private-token", "tok")
            .with_status(200)
            .with_body(r#"[{"id": 42, "username": "alice"}]"#)
            .create();
        let user = user_by_username(&server.url(), "tok", "alice").unwrap();
        assert_eq!(user.as_ref().map(|u| u.id), Some(42));
        assert_eq!(user.as_ref().map(|u| u.username.as_str()), Some("alice"));
        mock.assert();
    }

    #[test]
    fn user_by_username_empty_list_is_none() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v4/users?username=ghost")
            .with_status(200)
            .with_body("[]")
            .create();
        let user = user_by_username(&server.url(), "tok", "ghost").unwrap();
        assert!(user.is_none());
        mock.assert();
    }

    #[test]
    fn user_events_single_page() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", mockito::Matcher::Any)
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("page".into(), "1".into()),
                mockito::Matcher::UrlEncoded("per_page".into(), "100".into()),
                mockito::Matcher::UrlEncoded("after".into(), "2026-01-01".into()),
                mockito::Matcher::UrlEncoded("before".into(), "2026-03-31".into()),
                mockito::Matcher::UrlEncoded("action".into(), "created".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"[{"id": 1, "project_id": 10, "action_name": "opened",
                    "target_type": "MergeRequest", "target_iid": 123,
                    "target_title": "Fix X", "created_at": "2026-02-15T10:00:00Z"}]"#,
            )
            .create();
        let events = user_events(
            &server.url(),
            "tok",
            42,
            Some("created"),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            chrono::NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
        )
        .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].target_iid, Some(123));
        assert_eq!(events[0].action_name, "opened");
        mock.assert();
    }

    #[test]
    fn event_deserializes_push_data() {
        let json = r#"{
            "id": 5,
            "project_id": 10,
            "action_name": "pushed to",
            "created_at": "2026-02-15T10:00:00Z",
            "push_data": {"commit_count": 3, "ref": "main", "action": "pushed",
                          "ref_type": "branch", "commit_title": "Fix typo"}
        }"#;
        let e: Event = serde_json::from_str(json).unwrap();
        let push = e.push_data.unwrap();
        assert_eq!(push.commit_count, 3);
        assert_eq!(push.ref_name.as_deref(), Some("main"));
    }

    #[test]
    fn count_authored_commits_paginates_and_sums() {
        let mut server = mockito::Server::new();
        let mock_p1 = server
            .mock("GET", "/api/v4/projects/10/repository/commits")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("page".into(), "1".into()),
                mockito::Matcher::UrlEncoded("per_page".into(), "100".into()),
                mockito::Matcher::UrlEncoded("author".into(), "michel-slm".into()),
            ]))
            .with_status(200)
            // 100 entries → paginator fetches another page.
            .with_body(format!("[{}]", vec!["{}"; 100].join(",")))
            .create();
        let mock_p2 = server
            .mock("GET", "/api/v4/projects/10/repository/commits")
            .match_query(mockito::Matcher::UrlEncoded("page".into(), "2".into()))
            .with_status(200)
            .with_body("[{},{},{}]")
            .create();
        let n = count_authored_commits(
            &server.url(),
            "tok",
            10,
            "michel-slm",
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            chrono::NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
        )
        .unwrap();
        assert_eq!(n, 103);
        mock_p1.assert();
        mock_p2.assert();
    }

    #[test]
    fn project_summary_returns_path() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v4/projects/10")
            .with_status(200)
            .with_body(
                r#"{"id": 10, "path_with_namespace": "CentOS/Hyperscale/rpms/perf",
                    "web_url": "https://gitlab.com/CentOS/Hyperscale/rpms/perf"}"#,
            )
            .create();
        let p = project_summary(&server.url(), "tok", 10).unwrap();
        assert_eq!(p.path_with_namespace, "CentOS/Hyperscale/rpms/perf");
        mock.assert();
    }
}
