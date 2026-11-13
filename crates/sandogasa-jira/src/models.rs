// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal JIRA REST v2 types.
//!
//! We only model what callers actually need: an issue's key,
//! summary, status, resolution, type, priority, components and
//! affected versions; a project's versions; the fields an issue type
//! takes; and a new issue to file. Everything else is ignored.

use serde::{Deserialize, Serialize};

/// The account behind the credentials, from `GET myself`.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct Myself {
    pub display_name: String,
    /// Absent when the account hides it.
    #[serde(default)]
    pub email_address: Option<String>,
    /// An Atlassian Cloud account id; a self-hosted Jira has `name`
    /// instead.
    #[serde(default)]
    pub account_id: Option<String>,
}

/// A JIRA issue. Holds just the fields we care about.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct Issue {
    /// Issue key, e.g. "RHEL-12345".
    pub key: String,
    /// Flattened from `fields` for convenience.
    #[serde(rename = "fields")]
    pub fields: IssueFields,
}

impl Issue {
    /// Short summary / title.
    pub fn summary(&self) -> &str {
        &self.fields.summary
    }

    /// Workflow status name (e.g. "Closed", "In Progress", "New").
    pub fn status(&self) -> &str {
        &self.fields.status.name
    }

    /// Resolution name (e.g. "Done", "Won't Fix"). `None` if the
    /// issue is still unresolved.
    pub fn resolution(&self) -> Option<&str> {
        self.fields.resolution.as_ref().map(|r| r.name.as_str())
    }

    /// The releases the fix is in, by name.
    pub fn fix_versions(&self) -> Vec<&str> {
        self.fields
            .fix_versions
            .iter()
            .map(|v| v.name.as_str())
            .collect()
    }

    /// The issue this one duplicates, when it links to one the reader
    /// can see (`Duplicate`, outward).
    pub fn duplicate_of(&self) -> Option<&str> {
        self.fields
            .issuelinks
            .iter()
            .filter(|l| l.link_type.name == "Duplicate")
            .find_map(|l| l.outward_issue.as_ref().map(|i| i.key.as_str()))
    }

    /// True if the issue is resolved (has a non-null resolution).
    pub fn is_resolved(&self) -> bool {
        self.fields.resolution.is_some()
    }

    /// The calendar date JIRA recorded the resolution
    /// transition on, extracted from the ISO-8601 timestamp
    /// JIRA returns (e.g. `"2026-04-22T14:05:12.000+0000"` →
    /// `2026-04-22`). `None` when the issue isn't resolved or
    /// the timestamp is missing / malformed.
    pub fn resolution_date(&self) -> Option<chrono::NaiveDate> {
        Self::date_of(self.fields.resolutiondate.as_deref())
    }

    /// The day the issue was filed.
    pub fn created_date(&self) -> Option<chrono::NaiveDate> {
        Self::date_of(self.fields.created.as_deref())
    }

    fn date_of(ts: Option<&str>) -> Option<chrono::NaiveDate> {
        let date_part = ts?.split(['T', ' ']).next()?;
        chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct IssueFields {
    pub summary: String,
    /// The description, as the v2 API gives it (wiki markup text);
    /// `None` when the issue has none.
    #[serde(default)]
    pub description: Option<String>,
    pub status: NamedField,
    #[serde(default)]
    pub resolution: Option<NamedField>,
    /// ISO-8601 timestamp string (JIRA's native format). Kept
    /// raw so downstream callers can parse the level of detail
    /// they need.
    #[serde(default)]
    pub resolutiondate: Option<String>,
    /// When the issue was filed, same spelling.
    #[serde(default)]
    pub created: Option<String>,
    /// Bug, Story, Task, …
    #[serde(default)]
    pub issuetype: Option<NamedField>,
    #[serde(default)]
    pub priority: Option<NamedField>,
    #[serde(default)]
    pub components: Vec<NamedField>,
    /// The affected versions (`CentOS Stream 9`, `rhel-9.5`), which
    /// is how an issue says which release it is about.
    #[serde(default)]
    pub versions: Vec<NamedField>,
    #[serde(default)]
    pub labels: Vec<String>,
    /// The releases the fix is in (`rhel-10.3`, `CentOS Stream 9`),
    /// set as the issue is worked: an issue closed without any was
    /// closed without a fix being planned for any release.
    #[serde(default, rename = "fixVersions")]
    pub fix_versions: Vec<NamedField>,
    /// The issue's links to other issues (duplicates, clones, …), as far
    /// as the reader can see them: a link to a restricted issue is left
    /// out of the answer.
    #[serde(default)]
    pub issuelinks: Vec<IssueLink>,
}

/// A link between two issues, from the issue's side: the type, and the
/// issue on the other end, outward (this issue *duplicates* it) or
/// inward (it *is duplicated by* this one).
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct IssueLink {
    #[serde(rename = "type")]
    pub link_type: NamedField,
    #[serde(default)]
    pub outward_issue: Option<LinkedIssue>,
    #[serde(default)]
    pub inward_issue: Option<LinkedIssue>,
}

/// The issue at the other end of an [`IssueLink`].
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct LinkedIssue {
    pub key: String,
}

/// JIRA exposes many fields as `{name: ..., ...}`; we only need
/// the name.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct NamedField {
    pub name: String,
}

/// One of a project's versions, from `GET project/<key>/versions`:
/// what an issue's affected and fix versions are chosen from.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub released: bool,
    /// `YYYY-MM-DD`, when set.
    #[serde(default)]
    pub release_date: Option<String>,
}

/// A field an issue type takes when an issue is created, from the
/// create metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct FieldMeta {
    /// `summary`, `versions`, `customfield_10868`, …
    pub field_id: String,
    pub name: String,
    pub required: bool,
    /// Whether Jira fills it in when left out.
    pub has_default_value: bool,
    /// The names (or values) it accepts, when it is a choice; empty
    /// for a free field.
    pub allowed_values: Vec<String>,
}

/// An issue to file with [`crate::JiraClient::create_issue`].
///
/// The named fields cover what a bug report needs; anything else a
/// project requires goes in `extra`, keyed by field id, in the shape
/// the REST API takes (`{"value": "…"}` for a choice).
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct NewIssue {
    /// Project key, e.g. `RHEL`.
    pub project: String,
    /// Issue type name, e.g. `Bug`.
    pub issue_type: String,
    pub summary: String,
    /// Plain text or Jira wiki markup, as REST v2 takes it.
    pub description: String,
    pub components: Vec<String>,
    /// Affected version names.
    pub versions: Vec<String>,
    /// Priority name, e.g. `Critical`; `None` leaves Jira's default.
    pub priority: Option<String>,
    pub labels: Vec<String>,
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl NewIssue {
    /// A new issue of `issue_type` in `project` with `summary`; the
    /// rest is set on the fields.
    pub fn new(project: &str, issue_type: &str, summary: &str) -> Self {
        NewIssue {
            project: project.to_string(),
            issue_type: issue_type.to_string(),
            summary: summary.to_string(),
            ..Default::default()
        }
    }

    /// The request body REST v2's `POST issue` takes.
    pub fn to_json(&self) -> serde_json::Value {
        let named = |names: &[String]| -> Vec<serde_json::Value> {
            names
                .iter()
                .map(|n| serde_json::json!({ "name": n }))
                .collect()
        };
        let mut fields = serde_json::Map::new();
        fields.insert("project".into(), serde_json::json!({ "key": self.project }));
        fields.insert(
            "issuetype".into(),
            serde_json::json!({ "name": self.issue_type }),
        );
        fields.insert("summary".into(), self.summary.clone().into());
        if !self.description.is_empty() {
            fields.insert("description".into(), self.description.clone().into());
        }
        if !self.components.is_empty() {
            fields.insert("components".into(), named(&self.components).into());
        }
        if !self.versions.is_empty() {
            fields.insert("versions".into(), named(&self.versions).into());
        }
        if let Some(p) = &self.priority {
            fields.insert("priority".into(), serde_json::json!({ "name": p }));
        }
        if !self.labels.is_empty() {
            fields.insert("labels".into(), self.labels.clone().into());
        }
        for (k, v) in &self.extra {
            fields.insert(k.clone(), v.clone());
        }
        serde_json::json!({ "fields": fields })
    }
}

/// What Jira answers to a created issue.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct CreatedIssue {
    pub id: String,
    /// The new issue's key, e.g. `RHEL-170528`.
    pub key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_versions_and_a_visible_duplicate_are_read() {
        let issue: Issue = serde_json::from_value(serde_json::json!({
            "key": "RHEL-1",
            "fields": {
                "summary": "s",
                "status": {"name": "Closed"},
                "resolution": {"name": "Duplicate"},
                "fixVersions": [{"name": "rhel-10.3"}],
                "issuelinks": [
                    {"type": {"name": "Cloners"}, "outwardIssue": {"key": "RHEL-7"}},
                    {"type": {"name": "Duplicate"}, "inwardIssue": {"key": "RHEL-8"}},
                    {"type": {"name": "Duplicate"}, "outwardIssue": {"key": "RHEL-9"}},
                ],
            }
        }))
        .unwrap();
        assert_eq!(issue.fix_versions(), ["rhel-10.3"]);
        // RHEL-8 is duplicated by this one; this one duplicates RHEL-9.
        assert_eq!(issue.duplicate_of(), Some("RHEL-9"));
        let bare: Issue = serde_json::from_value(serde_json::json!({
            "key": "RHEL-2",
            "fields": {"summary": "s", "status": {"name": "New"}}
        }))
        .unwrap();
        assert!(bare.fix_versions().is_empty() && bare.duplicate_of().is_none());
    }
}
