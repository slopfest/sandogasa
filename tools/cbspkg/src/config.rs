// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Config file (`~/.config/cbspkg/config.toml`) and the GitLab
//! instance the SIGs' repos are on.

use sandogasa_config::secret::{SecretRef, SecretStore};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub gitlab: Option<GitlabConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GitlabConfig {
    /// Empty when the token is in the keyring (#79).
    #[serde(default, skip_serializing_if = "String::is_empty")]
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

/// Take `key` out of `table` in the config file: for a secret now in
/// the keyring, which `save` would otherwise leave there.
pub fn remove(table: &str, key: &str) -> Result<(), Box<dyn std::error::Error>> {
    config_file().remove(table, key)
}

pub fn config_path() -> std::path::PathBuf {
    config_file().path().to_path_buf()
}

/// The GitLab instance: gitlab.com, or `CBSPKG_GITLAB_BASE` (for
/// testing against a local server).
pub fn gitlab_base() -> String {
    std::env::var("CBSPKG_GITLAB_BASE").unwrap_or_else(|_| "https://gitlab.com".to_string())
}

/// The keyring: `secret-tool`, or in this crate's tests one in memory,
/// so `cargo test` never reads or writes the user's.
pub fn keyring() -> &'static dyn SecretStore {
    #[cfg(test)]
    {
        static MEMORY: std::sync::OnceLock<sandogasa_config::secret::MemoryStore> =
            std::sync::OnceLock::new();
        MEMORY.get_or_init(Default::default)
    }
    #[cfg(not(test))]
    {
        &sandogasa_config::secret::SecretTool
    }
}

/// The GitLab token's keyring entry, for the GitLab instance in use:
/// the same one every sandogasa tool reads for that host.
pub fn gitlab_secret() -> SecretRef {
    SecretRef::for_url(&gitlab_base(), "sandogasa: GitLab token")
}

/// The GitLab token: `GITLAB_TOKEN`, else the keyring's, else the
/// config file's.
pub fn gitlab_token() -> Option<String> {
    let file = load().ok().and_then(|c| c.gitlab.map(|g| g.access_token));
    sandogasa_config::secret::resolve(
        keyring(),
        Some("GITLAB_TOKEN"),
        &gitlab_secret(),
        file.as_deref(),
    )
}
