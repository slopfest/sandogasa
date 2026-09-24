// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal JIRA REST API client for issue status lookup.
//!
//! Currently scoped to what `cpu-sig-tracker` needs — fetch a
//! single issue, read its status and resolution. Additional
//! endpoints can be added as other callers need them.

mod client;
pub mod models;

pub use client::{JiraClient, cloud_id, gateway_url};
pub use models::{Issue, Myself};
