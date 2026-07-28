// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Thin wrapper around [`sandogasa_gitlab`] that loads the token
//! from the local config file and constructs project clients.

pub use sandogasa_gitlab::{
    Client, GroupClient, Issue, IssueUpdate, MergeRequest, package_from_issue_url, parse_issue_url,
    parse_mr_url,
};

/// Load the GitLab token from `GITLAB_TOKEN` env or config.
pub fn load_token() -> Result<String, Box<dyn std::error::Error>> {
    let token = std::env::var("GITLAB_TOKEN").ok().or_else(|| {
        crate::config::load()
            .ok()
            .and_then(|c| c.gitlab.map(|g| g.access_token))
    });
    token.ok_or_else(|| {
        "GitLab token not found; set GITLAB_TOKEN or add \
        [gitlab] access_token = \"…\" to the config file"
            .into()
    })
}

/// Project-level client for `project_path`, with the token loaded
/// from the environment or config file.
pub fn client(base_url: &str, project_path: &str) -> Result<Client, Box<dyn std::error::Error>> {
    Client::new(base_url, project_path, &load_token()?)
}

/// Group-level client for `group_path`, with the token loaded
/// from the environment or config file.
pub fn group_client(
    base_url: &str,
    group_path: &str,
) -> Result<GroupClient, Box<dyn std::error::Error>> {
    GroupClient::new(base_url, group_path, &load_token()?)
}
