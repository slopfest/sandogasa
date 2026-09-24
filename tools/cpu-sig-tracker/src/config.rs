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
    /// The Atlassian account the token belongs to, sent with it as
    /// basic auth — what an Atlassian Cloud site wants. Absent, the
    /// token is sent as a bearer, which is a self-hosted Jira's
    /// personal access token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// The API token (Cloud) or personal access token (self-hosted).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jira_config_reads_with_and_without_an_email() {
        // Written before the tracker moved to Atlassian Cloud: a bare
        // token, still a valid file.
        let old: Config = toml::from_str("[jira]\naccess_token = \"pat\"\n").unwrap();
        let jira = old.jira.unwrap();
        assert_eq!(jira.email, None);
        assert_eq!(jira.access_token, "pat");
        let new: Config =
            toml::from_str("[jira]\nemail = \"me@example.com\"\naccess_token = \"tok\"\n").unwrap();
        assert_eq!(new.jira.unwrap().email.as_deref(), Some("me@example.com"));
    }
}
