// SPDX-License-Identifier: MPL-2.0

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;

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
}

/// Client for the GitLab REST API v4.
pub struct Client {
    http: reqwest::blocking::Client,
    base_url: String,
    project_path: String,
}

impl Client {
    /// Create a client for the given GitLab project URL.
    ///
    /// Reads the authentication token from `GITLAB_TOKEN`, falling
    /// back to the config file.
    pub fn from_project_url(
        url: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let token = std::env::var("GITLAB_TOKEN").ok().or_else(|| {
            crate::config::load()
                .ok()
                .and_then(|c| c.gitlab.map(|g| g.access_token))
        });
        let token = token.ok_or(
            "GitLab token not found; set GITLAB_TOKEN \
            or run 'hs-relmon config'",
        )?;
        let (base_url, project_path) = parse_project_url(url)?;
        Self::new(&base_url, &project_path, &token)
    }

    /// Create a client with explicit parameters.
    pub fn new(
        base_url: &str,
        project_path: &str,
        token: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("private-token"),
            HeaderValue::from_str(token)?,
        );
        let http = reqwest::blocking::Client::builder()
            .user_agent("hs-relmon/0.2.0")
            .default_headers(headers)
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            project_path: project_path.to_string(),
        })
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

        let resp = self.http.post(&self.issues_url()).json(&body).send()?;
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
        let resp = self
            .http
            .get(&self.issues_url())
            .query(&query)
            .send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(
                format!("GitLab API error {status}: {text}").into(),
            );
        }
        Ok(resp.json()?)
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
            .put(&format!("{}/{iid}", self.issues_url()))
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
    ) -> Result<Option<String>, Box<dyn std::error::Error>>
    {
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
        let resp = self
            .http
            .post(&self.graphql_url())
            .json(&body)
            .send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!(
                "GitLab GraphQL error {status}: {text}"
            )
            .into());
        }
        let json: serde_json::Value = resp.json()?;
        Ok(parse_work_item_status(&json))
    }

    fn issues_url(&self) -> String {
        let encoded = self.project_path.replace('/', "%2F");
        format!(
            "{}/api/v4/projects/{}/issues",
            self.base_url, encoded
        )
    }

    fn graphql_url(&self) -> String {
        format!("{}/api/graphql", self.base_url)
    }
}

/// Extract the status name from a GraphQL work-item response.
fn parse_work_item_status(
    json: &serde_json::Value,
) -> Option<String> {
    json.pointer("/data/project/workItems/nodes/0/widgets")
        .and_then(|w| w.as_array())
        .and_then(|widgets| {
            widgets.iter().find(|w| {
                w.get("type").and_then(|t| t.as_str())
                    == Some("STATUS")
            })
        })
        .and_then(|w| w.pointer("/status/name"))
        .and_then(|n| n.as_str())
        .map(String::from)
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
}

fn check_response(
    resp: reqwest::blocking::Response,
) -> Result<Issue, Box<dyn std::error::Error>> {
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text()?;
        return Err(format!("GitLab API error {status}: {text}").into());
    }
    Ok(resp.json()?)
}

/// Check whether a token is valid by calling `GET /api/v4/user`.
pub fn validate_token(
    base_url: &str,
    token: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("private-token"),
        HeaderValue::from_str(token)?,
    );
    let client = reqwest::blocking::Client::builder()
        .user_agent("hs-relmon/0.2.0")
        .default_headers(headers)
        .build()?;
    let url = format!(
        "{}/api/v4/user",
        base_url.trim_end_matches('/')
    );
    let resp = client.get(&url).send()?;
    Ok(resp.status().is_success())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_project_url() {
        let (base, path) = parse_project_url(
            "https://gitlab.com/CentOS/Hyperscale/rpms/perf",
        )
        .unwrap();
        assert_eq!(base, "https://gitlab.com");
        assert_eq!(path, "CentOS/Hyperscale/rpms/perf");
    }

    #[test]
    fn test_parse_project_url_trailing_slash() {
        let (base, path) =
            parse_project_url("https://gitlab.com/group/project/")
                .unwrap();
        assert_eq!(base, "https://gitlab.com");
        assert_eq!(path, "group/project");
    }

    #[test]
    fn test_parse_project_url_http() {
        let (base, path) = parse_project_url(
            "http://gitlab.example.com/group/project",
        )
        .unwrap();
        assert_eq!(base, "http://gitlab.example.com");
        assert_eq!(path, "group/project");
    }

    #[test]
    fn test_parse_project_url_no_scheme() {
        assert!(
            parse_project_url("gitlab.com/group/project").is_err()
        );
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
        // None fields should be absent
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
        let json = r#"{
            "iid": 1,
            "title": "t",
            "description": null,
            "state": "opened",
            "web_url": "u"
        }"#;
        let issue: Issue = serde_json::from_str(json).unwrap();
        assert!(issue.description.is_none());
        assert!(issue.assignees.is_empty());
    }

    #[test]
    fn test_issue_deserialize_null_description() {
        let json = r#"{
            "iid": 1,
            "title": "t",
            "description": null,
            "state": "opened",
            "web_url": "u"
        }"#;
        let issue: Issue = serde_json::from_str(json).unwrap();
        assert!(issue.description.is_none());
    }

    #[test]
    fn test_graphql_url() {
        let client = Client::new(
            "https://gitlab.com",
            "CentOS/Hyperscale/rpms/perf",
            "fake-token",
        )
        .unwrap();
        assert_eq!(
            client.graphql_url(),
            "https://gitlab.com/api/graphql"
        );
    }

    #[test]
    fn test_parse_work_item_status_found() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "data": {
                    "project": {
                        "workItems": {
                            "nodes": [{
                                "widgets": [
                                    { "type": "ASSIGNEES" },
                                    {
                                        "type": "STATUS",
                                        "status": {
                                            "name": "To do"
                                        }
                                    }
                                ]
                            }]
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        assert_eq!(
            parse_work_item_status(&json).as_deref(),
            Some("To do")
        );
    }

    #[test]
    fn test_parse_work_item_status_in_progress() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "data": {
                    "project": {
                        "workItems": {
                            "nodes": [{
                                "widgets": [
                                    {
                                        "type": "STATUS",
                                        "status": {
                                            "name": "In progress"
                                        }
                                    }
                                ]
                            }]
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        assert_eq!(
            parse_work_item_status(&json).as_deref(),
            Some("In progress")
        );
    }

    #[test]
    fn test_parse_work_item_status_no_status_widget() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "data": {
                    "project": {
                        "workItems": {
                            "nodes": [{
                                "widgets": [
                                    { "type": "ASSIGNEES" },
                                    { "type": "LABELS" }
                                ]
                            }]
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        assert!(parse_work_item_status(&json).is_none());
    }

    #[test]
    fn test_parse_work_item_status_empty_nodes() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "data": {
                    "project": {
                        "workItems": {
                            "nodes": []
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        assert!(parse_work_item_status(&json).is_none());
    }

    #[test]
    fn test_parse_work_item_status_null_status() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{
                "data": {
                    "project": {
                        "workItems": {
                            "nodes": [{
                                "widgets": [
                                    {
                                        "type": "STATUS",
                                        "status": null
                                    }
                                ]
                            }]
                        }
                    }
                }
            }"#,
        )
        .unwrap();
        assert!(parse_work_item_status(&json).is_none());
    }
}
