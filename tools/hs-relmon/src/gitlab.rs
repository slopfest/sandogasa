// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Thin wrapper around [`sandogasa_gitlab`] that adds token loading
//! from the local config file and provides convenience constructors
//! matching the original single-argument API.

// Re-export everything from the library crate.
pub use sandogasa_gitlab::{
    Assignee, Client, GroupClient, Issue, IssueUpdate, ProjectStatus, package_from_issue_url,
    parse_project_url, project_path_from_issue_url, validate_token,
};

/// Load the GitLab token from `GITLAB_TOKEN` env var or config file.
pub fn load_token() -> Result<String, Box<dyn std::error::Error>> {
    let token = std::env::var("GITLAB_TOKEN").ok().or_else(|| {
        crate::config::load()
            .ok()
            .and_then(|c| c.gitlab.map(|g| g.access_token))
    });
    token.ok_or_else(|| {
        "GitLab token not found; set GITLAB_TOKEN \
        or run 'hs-relmon config'"
            .into()
    })
}

/// Project-level client for a project URL, with the token loaded
/// from the environment or config file.
pub fn client_from_project_url(url: &str) -> Result<Client, Box<dyn std::error::Error>> {
    let token = load_token()?;
    let (base_url, project_path) = parse_project_url(url)?;
    Client::new(&base_url, &project_path, &token)
}

/// Group-level client for a group URL, with the token loaded from
/// the environment or config file.
pub fn group_client_from_group_url(url: &str) -> Result<GroupClient, Box<dyn std::error::Error>> {
    let token = load_token()?;
    let (base_url, group_path) = parse_project_url(url)?;
    GroupClient::new(&base_url, &group_path, &token)
}
