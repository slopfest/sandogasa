// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Config file (`~/.config/cbspkg/config.toml`) and the GitLab
//! instance the SIGs' repos are on.

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub gitlab: Option<GitlabConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GitlabConfig {
    pub access_token: String,
}

fn config_file() -> sandogasa_config::ConfigFile {
    sandogasa_config::ConfigFile::for_tool("cbspkg")
}

pub fn load() -> Result<Config, Box<dyn std::error::Error>> {
    config_file().load()
}

/// The config file alone, without `/etc`, for the `config` command to
/// rewrite.
pub fn load_user() -> Result<Config, Box<dyn std::error::Error>> {
    config_file().load_user()
}

pub fn save(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    config_file().save(config)
}

pub fn config_path() -> std::path::PathBuf {
    config_file().path().to_path_buf()
}

/// The GitLab instance: gitlab.com, or `CBSPKG_GITLAB_BASE` (for
/// testing against a local server).
pub fn gitlab_base() -> String {
    std::env::var("CBSPKG_GITLAB_BASE").unwrap_or_else(|_| "https://gitlab.com".to_string())
}

/// The GitLab token: `GITLAB_TOKEN`, else the config file's.
pub fn gitlab_token() -> Option<String> {
    std::env::var("GITLAB_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
        .or_else(|| load().ok()?.gitlab.map(|g| g.access_token))
}
