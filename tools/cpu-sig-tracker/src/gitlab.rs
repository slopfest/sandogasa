// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Thin wrapper around [`sandogasa_gitlab`] that loads the token
//! from the local config file and constructs project clients.

pub use sandogasa_gitlab::{Issue, MergeRequest, package_from_issue_url, parse_mr_url};

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

/// Project-level GitLab client that loads the token automatically.
pub struct Client(sandogasa_gitlab::Client);

impl Client {
    /// Create a client from explicit base URL + project path.
    pub fn new(base_url: &str, project_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let token = load_token()?;
        Ok(Self(sandogasa_gitlab::Client::new(
            base_url,
            project_path,
            &token,
        )?))
    }

    pub fn merge_request(&self, iid: u64) -> Result<MergeRequest, Box<dyn std::error::Error>> {
        self.0.merge_request(iid)
    }

    pub fn create_issue(
        &self,
        title: &str,
        description: Option<&str>,
        labels: Option<&str>,
    ) -> Result<Issue, Box<dyn std::error::Error>> {
        self.0.create_issue(title, description, labels)
    }

    pub fn list_issues(
        &self,
        label: &str,
        state: Option<&str>,
    ) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
        self.0.list_issues(label, state)
    }

    pub fn set_work_item_status(
        &self,
        iid: u64,
        status: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.0.set_work_item_status(iid, status)
    }
}

/// Group-level GitLab client that loads the token automatically.
pub struct GroupClient(sandogasa_gitlab::GroupClient);

impl GroupClient {
    pub fn new(base_url: &str, group_path: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let token = load_token()?;
        Ok(Self(sandogasa_gitlab::GroupClient::new(
            base_url, group_path, &token,
        )?))
    }

    pub fn list_issues(
        &self,
        label: &str,
        state: Option<&str>,
    ) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
        self.0.list_issues(label, state)
    }
}
