// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal JIRA REST API client.
//!
//! Scoped to what `cpu-sig-tracker` needs: fetch an issue and read
//! its status and resolution, search with JQL, read a project's
//! versions and the fields an issue type takes, and file an issue.
//! Additional endpoints can be added as other callers need them.

mod client;
pub mod models;

pub use client::{JiraClient, cloud_id, gateway_url};
pub use models::{
    CreatedIssue, FieldMeta, Issue, IssueLink, LinkedIssue, Myself, NewIssue, Version,
};
