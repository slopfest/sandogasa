// SPDX-License-Identifier: MPL-2.0

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Global app config stored at ~/.config/fedora-cve-triage/config.toml
#[derive(Debug, Deserialize, Serialize)]
pub struct AppConfig {
    pub bugzilla: BugzillaConfig,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct BugzillaConfig {
    pub api_key: String,
}

impl AppConfig {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("fedora-cve-triage")
            .join("config.toml")
    }

    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let path = Self::path();
        let contents = std::fs::read_to_string(&path).map_err(|e| {
            format!(
                "Could not read {}: {}. Run 'config' to set up.",
                path.display(),
                e
            )
        })?;
        let config: Self = toml::from_str(&contents)?;
        Ok(config)
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(&path, contents)?;
        Ok(())
    }
}

/// Per-run config for the js-fps subcommand
#[derive(Debug, Deserialize)]
pub struct JsFpsConfig {
    pub tracker_bug: String,
    pub products: Vec<String>,
    pub components: Vec<String>,
    pub statuses: Vec<String>,
}

impl JsFpsConfig {
    pub fn from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        Ok(config)
    }
}
