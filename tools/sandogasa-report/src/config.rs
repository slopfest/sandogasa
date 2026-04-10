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
    pub groups: BTreeMap<String, GroupConfig>,
}

/// A package group with an optional description.
#[derive(Debug, Default, Deserialize)]
pub struct GroupConfig {
    /// Human-readable description (optional).
    #[serde(default)]
    pub description: Option<String>,
    /// Package names in this group.
    #[serde(default)]
    pub packages: Vec<String>,
}

/// Configuration for a reporting domain.
#[derive(Debug, Default, Deserialize)]
pub struct DomainConfig {
    /// Include Bugzilla queries.
    #[serde(default)]
    pub bugzilla: bool,

    /// Fedora versions for FTBFS/FTI tracker lookup (e.g. [43, 44, 45]).
    #[serde(default)]
    pub fedora_versions: Vec<u32>,

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

/// Load the config file.
///
/// If `path` is given, loads from that file. Otherwise looks for
/// `~/.config/sandogasa-report/config.toml` and returns empty
/// defaults if it doesn't exist.
pub fn load_config(path: Option<&str>) -> Result<ReportConfig, String> {
    let cf = match path {
        Some(p) => sandogasa_config::ConfigFile::from_path(p.into()),
        None => sandogasa_config::ConfigFile::for_tool("sandogasa-report"),
    };
    if !cf.path().exists() {
        if path.is_some() {
            return Err(format!("config file not found: {}", cf.path().display()));
        }
        return Ok(ReportConfig::default());
    }
    cf.load::<ReportConfig>()
        .map_err(|e| format!("failed to load config from {}: {e}", cf.path().display()))
}
