// SPDX-License-Identifier: MPL-2.0

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub gitlab: Option<GitlabConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GitlabConfig {
    pub access_token: String,
}

pub fn config_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME")
                .unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        });
    Ok(config_dir.join("hs-relmon").join("config.toml"))
}

pub fn load() -> Result<Config, Box<dyn std::error::Error>> {
    let path = config_path()?;
    let contents = std::fs::read_to_string(&path)?;
    Ok(toml::from_str(&contents)?)
}

pub fn save(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let contents = toml::to_string_pretty(config)?;
    std::fs::write(&path, contents)?;
    Ok(())
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
        assert_eq!(
            config.gitlab.as_ref().unwrap().access_token,
            "glpat-abc123"
        );
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
        assert_eq!(
            parsed.gitlab.unwrap().access_token,
            "test-token"
        );
    }

    #[test]
    fn test_config_path() {
        let path = config_path().unwrap();
        assert!(path.ends_with("hs-relmon/config.toml"));
    }
}
