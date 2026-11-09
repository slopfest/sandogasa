// SPDX-License-Identifier: Apache-2.0 OR MIT

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
    sandogasa_config::ConfigFile::for_tool("hs-relmon")
}

pub fn config_path() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    Ok(config_file().path().to_path_buf())
}

pub fn load() -> Result<Config, Box<dyn std::error::Error>> {
    config_file().load()
}

pub fn save(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    config_file().save(config)
}

/// Take `key` out of `table` in the config file: for a secret now in
/// the keyring, which `save` would otherwise leave there. Whether the
/// file, left with nothing in it, was removed.
pub fn remove(table: &str, key: &str) -> Result<bool, Box<dyn std::error::Error>> {
    config_file().remove(table, key)
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

/// The GitLab token's keyring entry: gitlab.com's, which every
/// sandogasa tool talking to gitlab.com reads.
pub fn gitlab_secret() -> SecretRef {
    SecretRef::for_url("https://gitlab.com", "sandogasa: GitLab token")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_deserialize() {
        let toml_str = r#"
[gitlab]
access_token = "glpat-abc123"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.gitlab.as_ref().unwrap().access_token, "glpat-abc123");
    }

    #[test]
    fn test_config_deserialize_no_gitlab() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.gitlab.is_none());
    }

    #[test]
    fn test_config_serialize() {
        let config = Config {
            gitlab: Some(GitlabConfig {
                access_token: "glpat-abc123".into(),
            }),
        };
        let s = toml::to_string_pretty(&config).unwrap();
        assert!(s.contains("[gitlab]"));
        assert!(s.contains("access_token = \"glpat-abc123\""));
    }

    #[test]
    fn test_config_roundtrip() {
        let config = Config {
            gitlab: Some(GitlabConfig {
                access_token: "test-token".into(),
            }),
        };
        let s = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&s).unwrap();
        assert_eq!(parsed.gitlab.unwrap().access_token, "test-token");
    }

    #[test]
    fn test_config_path() {
        let path = config_path().unwrap();
        assert!(path.ends_with("hs-relmon/config.toml"));
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(dir.path().join("config.toml"));
        let config = Config {
            gitlab: Some(GitlabConfig {
                access_token: "test-save-load".into(),
            }),
        };
        cf.save(&config).unwrap();
        let loaded: Config = cf.load().unwrap();
        assert_eq!(loaded.gitlab.unwrap().access_token, "test-save-load");
    }

    #[test]
    fn test_load_missing_file() {
        let cf = sandogasa_config::ConfigFile::from_path("/tmp/hs-relmon-nonexistent/x".into());
        let result: Result<Config, _> = cf.load();
        assert!(result.is_err());
    }
}
