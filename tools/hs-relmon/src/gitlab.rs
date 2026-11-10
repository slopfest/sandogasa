// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Thin wrapper around [`sandogasa_gitlab`] that adds token loading
//! from the local config file and provides convenience constructors
//! matching the original single-argument API.

use sandogasa_config::secret::SecretStore;

// Re-export everything from the library crate.
pub use sandogasa_gitlab::{
    Assignee, Client, GroupClient, Issue, IssueUpdate, ProjectStatus, package_from_issue_url,
    parse_project_url, project_path_from_issue_url, validate_token,
};

/// The GitLab token: `GITLAB_TOKEN`, else the keyring's in `store`
/// (shared with the other sandogasa tools, #79), else the config file's.
/// The keyring is the binary's to pass, as this library is built without
/// `cfg(test)` for the binary's tests.
pub fn load_token(store: &dyn SecretStore) -> Result<String, Box<dyn std::error::Error>> {
    let file = crate::config::load()
        .ok()
        .and_then(|c| c.gitlab.map(|g| g.access_token));
    sandogasa_config::secret::resolve(
        store,
        Some("GITLAB_TOKEN"),
        &crate::config::gitlab_secret(),
        file.as_deref(),
    )
    .ok_or_else(|| "GitLab token not found; set GITLAB_TOKEN or run 'hs-relmon config'".into())
}

/// Project-level client for a project URL, with the token loaded
/// from the environment or config file.
pub fn client_from_project_url(
    store: &dyn SecretStore,
    url: &str,
) -> Result<Client, Box<dyn std::error::Error>> {
    let token = load_token(store)?;
    let (base_url, project_path) = parse_project_url(url)?;
    Client::new(&base_url, &project_path, &token)
}

/// Group-level client for a group URL, with the token loaded from
/// the environment or config file.
pub fn group_client_from_group_url(
    store: &dyn SecretStore,
    url: &str,
) -> Result<GroupClient, Box<dyn std::error::Error>> {
    let token = load_token(store)?;
    let (base_url, group_path) = parse_project_url(url)?;
    GroupClient::new(&base_url, &group_path, &token)
}
