// SPDX-License-Identifier: Apache-2.0 OR MIT

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
