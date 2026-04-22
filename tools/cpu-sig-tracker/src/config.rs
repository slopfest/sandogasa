// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Config file (`~/.config/cpu-sig-tracker/config.toml`).

use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub gitlab: Option<GitlabConfig>,
    pub jira: Option<JiraConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GitlabConfig {
    pub access_token: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JiraConfig {
    /// Personal Access Token for authenticated requests.
    pub access_token: String,
}

fn config_file() -> sandogasa_config::ConfigFile {
    sandogasa_config::ConfigFile::for_tool("cpu-sig-tracker")
}

pub fn load() -> Result<Config, Box<dyn std::error::Error>> {
    config_file().load()
}

pub fn save(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    config_file().save(config)
}

pub fn config_path() -> std::path::PathBuf {
    config_file().path().to_path_buf()
}
