// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Thin wrapper around [`sandogasa_gitlab`] that adds token loading
//! from the local config file and provides convenience constructors
//! matching the original single-argument API.

// Re-export everything from the library crate.
pub use sandogasa_gitlab::{
    Assignee, Issue, IssueUpdate, package_from_issue_url, parse_project_url,
    project_path_from_issue_url, validate_token,
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

/// Project-level GitLab client that loads the token automatically.
pub struct Client(sandogasa_gitlab::Client);

impl Client {
    /// Create a client from a project URL, loading the token
    /// from the environment or config file.
    pub fn from_project_url(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let token = load_token()?;
        let (base_url, project_path) = parse_project_url(url)?;
        Ok(Self(sandogasa_gitlab::Client::new(
            &base_url,
            &project_path,
            &token,
        )?))
    }

    /// Create a client with explicit parameters.
    pub fn new(
        base_url: &str,
        project_path: &str,
        token: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self(sandogasa_gitlab::Client::new(
            base_url,
            project_path,
            token,
        )?))
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

    pub fn add_note(&self, iid: u64, body: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.0.add_note(iid, body)
    }

    pub fn edit_issue(
        &self,
        iid: u64,
        updates: &IssueUpdate,
    ) -> Result<Issue, Box<dyn std::error::Error>> {
        self.0.edit_issue(iid, updates)
    }

    pub fn get_work_item_status(
        &self,
        iid: u64,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        self.0.get_work_item_status(iid)
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
    /// Create a group client from a group URL, loading the token
    /// from the environment or config file.
    pub fn from_group_url(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let token = load_token()?;
        let (base_url, group_path) = parse_project_url(url)?;
        Ok(Self(sandogasa_gitlab::GroupClient::new(
            &base_url,
            &group_path,
            &token,
        )?))
    }

    pub fn list_issues(
        &self,
        label: &str,
        state: Option<&str>,
    ) -> Result<Vec<Issue>, Box<dyn std::error::Error>> {
        self.0.list_issues(label, state)
    }

    pub fn get_work_item_status(
        &self,
        project_path: &str,
        iid: u64,
    ) -> Result<Option<String>, Box<dyn std::error::Error>> {
        self.0.get_work_item_status(project_path, iid)
    }
}
