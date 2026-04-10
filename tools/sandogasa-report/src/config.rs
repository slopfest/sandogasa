// SPDX-License-Identifier: MPL-2.0

//! Configuration for sandogasa-report.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Top-level config.
#[derive(Debug, Default, Deserialize)]
pub struct ReportConfig {
    /// FAS username → Bugzilla email mapping.
    #[serde(default)]
    pub users: BTreeMap<String, String>,

    /// Named domain presets.
    #[serde(default)]
    pub domains: BTreeMap<String, DomainConfig>,

    /// Package groups for categorical reporting.
    #[serde(default)]
    pub groups: BTreeMap<String, Vec<String>>,
}

/// Configuration for a reporting domain.
#[derive(Debug, Default, Deserialize)]
pub struct DomainConfig {
    /// Include Bugzilla queries.
    #[serde(default)]
    pub bugzilla: bool,

    /// Include Bodhi queries.
    #[serde(default)]
    pub bodhi: bool,

    /// Bodhi release name patterns (e.g. "F*", "EPEL-*").
    #[serde(default)]
    pub bodhi_releases: Vec<String>,

    /// Koji CLI profile (e.g. "cbs").
    #[serde(default)]
    pub koji_profile: Option<String>,

    /// Koji tag patterns with brace expansion.
    #[serde(default)]
    pub koji_tags: Vec<String>,
}

/// Load the config from `~/.config/sandogasa-report/config.toml`.
pub fn load_config() -> Result<ReportConfig, String> {
    let cf = sandogasa_config::ConfigFile::for_tool("sandogasa-report");
    match cf.load::<ReportConfig>() {
        Ok(config) => Ok(config),
        Err(e) => {
            let path = cf.path().display();
            Err(format!("failed to load config from {path}: {e}"))
        }
    }
}
