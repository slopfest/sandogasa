// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Thin wrapper around [`sandogasa_jira`] for the Red Hat JIRA
//! (`https://issues.redhat.com`). Loads the API token from config
//! if present; otherwise falls back to anonymous access (which
//! works for public issues).

/// Base URL for Red Hat JIRA. Hardcoded since that's the only
/// JIRA instance this tool currently talks to.
pub const BASE_URL: &str = "https://issues.redhat.com";

/// Build a JIRA client, loading an optional token from the
/// environment (`JIRA_TOKEN`) or the local config.
pub fn client() -> sandogasa_jira::JiraClient {
    let token = std::env::var("JIRA_TOKEN").ok().or_else(|| {
        crate::config::load()
            .ok()
            .and_then(|c| c.jira.map(|j| j.access_token))
    });
    let c = sandogasa_jira::JiraClient::new(BASE_URL);
    match token {
        Some(t) => c.with_api_key(t),
        None => c,
    }
}
