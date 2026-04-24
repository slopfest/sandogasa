// SPDX-License-Identifier: Apache-2.0 OR MIT

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

    /// Include GitLab activity (MRs authored/merged/reviewed, commits).
    #[serde(default)]
    pub gitlab: Option<GitlabConfig>,
}

/// Per-domain GitLab settings. If `group` is set, activity events
/// are filtered to projects whose `path_with_namespace` starts
/// with that prefix. Omit `group` to include all user activity on
/// the instance.
#[derive(Debug, Default, Deserialize)]
pub struct GitlabConfig {
    /// GitLab base URL (e.g. `https://gitlab.com`,
    /// `https://salsa.debian.org`).
    pub instance: String,

    /// Group prefix filter (e.g. `CentOS/Hyperscale`,
    /// `CentOS/Hyperscale/rpms`). Matches on path_with_namespace.
    #[serde(default)]
    pub group: Option<String>,

    /// Override the CLI `--user` for this instance. Needed when
    /// the user's GitLab username differs from their FAS login
    /// (e.g. FAS `salimma` vs gitlab.com `michel-slm` vs salsa
    /// `michel`). If unset, the CLI `--user` value is used.
    #[serde(default)]
    pub user: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_config_no_file_returns_default() {
        let cfg = load_config(None).unwrap();
        assert!(cfg.domains.is_empty());
        assert!(cfg.users.is_empty());
        assert!(cfg.groups.is_empty());
    }

    #[test]
    fn load_config_explicit_missing_errors() {
        let result = load_config(Some("/tmp/nonexistent-sandogasa-report-test.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn load_config_from_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[domains.test]
bugzilla = true

[groups.mygroup]
packages = ["pkg1", "pkg2"]
"#,
        )
        .unwrap();
        let cfg = load_config(Some(path.to_str().unwrap())).unwrap();
        assert!(cfg.domains.contains_key("test"));
        assert!(cfg.domains["test"].bugzilla);
        assert_eq!(cfg.groups["mygroup"].packages, vec!["pkg1", "pkg2"]);
    }
}
