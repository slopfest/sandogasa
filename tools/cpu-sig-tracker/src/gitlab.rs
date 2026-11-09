// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Thin wrapper around [`sandogasa_gitlab`] that loads the token
//! from the local config file and constructs project clients.

pub use sandogasa_gitlab::{
    Client, GroupClient, Issue, IssueUpdate, MergeRequest, Note, package_from_issue_url,
    parse_issue_url, parse_mr_url,
};

/// The GitLab token: `GITLAB_TOKEN`, else the keyring's, else the
/// config file's.
pub fn load_token() -> Result<String, Box<dyn std::error::Error>> {
    let file = crate::config::load()
        .ok()
        .and_then(|c| c.gitlab.map(|g| g.access_token));
    sandogasa_config::secret::resolve(
        crate::secrets::keyring(),
        Some("GITLAB_TOKEN"),
        &crate::secrets::gitlab(),
        file.as_deref(),
    )
    .ok_or_else(|| "no GitLab token: run `cpu-sig-tracker config`, or set GITLAB_TOKEN".into())
}

/// Project-level client for `project_path`, with the token loaded
/// from the environment or config file.
pub fn client(base_url: &str, project_path: &str) -> Result<Client, Box<dyn std::error::Error>> {
    Client::new(base_url, project_path, &load_token()?)
}

/// A client for reading a public project: with the token when one is
/// configured, anonymous otherwise, since reading a public merge
/// request needs none. Writes go through [`client`], which insists.
pub fn reader(base_url: &str, project_path: &str) -> Result<Client, Box<dyn std::error::Error>> {
    Client::new(base_url, project_path, &load_token().unwrap_or_default())
}

/// Group-level client for `group_path`, with the token loaded
/// from the environment or config file.
pub fn group_client(
    base_url: &str,
    group_path: &str,
) -> Result<GroupClient, Box<dyn std::error::Error>> {
    GroupClient::new(base_url, group_path, &load_token()?)
}
