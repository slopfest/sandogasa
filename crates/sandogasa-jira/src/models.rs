// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal JIRA REST v2 response types.
//!
//! We only model what callers actually need: issue key, summary,
//! status, and resolution. Everything else is ignored.

use serde::Deserialize;

/// A JIRA issue. Holds just the fields we care about.
#[derive(Debug, Clone, Deserialize)]
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

    /// True if the issue is resolved (has a non-null resolution).
    pub fn is_resolved(&self) -> bool {
        self.fields.resolution.is_some()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssueFields {
    pub summary: String,
    pub status: NamedField,
    #[serde(default)]
    pub resolution: Option<NamedField>,
}

/// JIRA exposes many fields as `{name: ..., ...}`; we only need
/// the name.
#[derive(Debug, Clone, Deserialize)]
pub struct NamedField {
    pub name: String,
}
