// SPDX-License-Identifier: Apache-2.0 OR MIT

use sandogasa_config::secret::{ConfigSecret, SecretRef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub gitlab: Option<GitlabConfig>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GitlabConfig {
    /// Empty when the token is in the keyring (#79).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access_token: String,
}

pub fn config_file() -> sandogasa_config::ConfigFile {
    sandogasa_config::ConfigFile::for_tool("hs-relmon")
}

pub fn config_path() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    Ok(config_file().path().to_path_buf())
}

pub fn load() -> Result<Config, Box<dyn std::error::Error>> {
    config_file().load()
}

/// The GitLab token's keyring entry: gitlab.com's, which every
/// sandogasa tool talking to gitlab.com reads.
pub fn gitlab_secret() -> SecretRef {
    SecretRef::for_url("https://gitlab.com", "sandogasa: GitLab token")
}

/// The GitLab token as the config file keeps it, for `config`: in the
/// keyring when there is one, else as `[gitlab] access_token`.
pub fn gitlab_token_secret() -> ConfigSecret<'static> {
    ConfigSecret::new(
        "hs-relmon",
        "gitlab",
        "access_token",
        "GitLab token",
        gitlab_secret(),
    )
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

    /// The token is required, so one turned off for hs-relmon is asked
    /// about by name, with yes the default.
    #[test]
    fn an_off_token_is_asked_about_for_hs_relmon() {
        use sandogasa_config::secret::{MemoryStore, SecretStore};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[gitlab]\naccess_token = \"-\"\n").unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(path);
        let store = MemoryStore::default();
        store.store(&gitlab_secret(), "glpat-shared").unwrap();
        let (saved, _) = gitlab_token_secret()
            .saved(&cf, &store, false, &|q, default_yes| {
                assert!(q.contains("GitLab token: off for hs-relmon"), "{q}");
                assert!(default_yes);
                true
            })
            .unwrap();
        assert_eq!(saved, "glpat-shared");
    }

    /// What `config` saves is what the tool reads back as its token.
    #[test]
    fn the_saved_token_is_the_one_the_config_reads() {
        use sandogasa_config::secret::{Before, NoKeyring};
        let dir = tempfile::tempdir().unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(dir.path().join("config.toml"));
        gitlab_token_secret()
            .save(&cf, &NoKeyring, "glpat-file".into(), &[Before::Absent])
            .unwrap();
        let loaded: Config = cf.load().unwrap();
        assert_eq!(loaded.gitlab.unwrap().access_token, "glpat-file");
    }
}
