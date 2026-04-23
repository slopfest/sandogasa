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

    /// The calendar date JIRA recorded the resolution
    /// transition on, extracted from the ISO-8601 timestamp
    /// JIRA returns (e.g. `"2026-04-22T14:05:12.000+0000"` →
    /// `2026-04-22`). `None` when the issue isn't resolved or
    /// the timestamp is missing / malformed.
    pub fn resolution_date(&self) -> Option<chrono::NaiveDate> {
        let ts = self.fields.resolutiondate.as_deref()?;
        let date_part = ts.split(['T', ' ']).next()?;
        chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssueFields {
    pub summary: String,
    pub status: NamedField,
    #[serde(default)]
    pub resolution: Option<NamedField>,
    /// ISO-8601 timestamp string (JIRA's native format). Kept
    /// raw so downstream callers can parse the level of detail
    /// they need.
    #[serde(default)]
    pub resolutiondate: Option<String>,
}

/// JIRA exposes many fields as `{name: ..., ...}`; we only need
/// the name.
#[derive(Debug, Clone, Deserialize)]
pub struct NamedField {
    pub name: String,
}
