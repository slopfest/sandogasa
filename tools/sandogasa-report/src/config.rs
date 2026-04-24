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

/// Load config with a per-user overlay.
///
/// The main config (passed via `-c`) holds the shared structure —
/// domains, groups, koji tags, GitLab instance URLs. Personal
/// settings that vary per user (GitLab usernames, Bugzilla email
/// maps, anything else someone wants to override locally) go in
/// `~/.config/sandogasa-report/config.toml`, which is auto-loaded
/// and deep-merged on top of the main config. Overlay values win
/// at every nesting level, and missing values simply don't
/// override anything.
///
/// - `-c PATH` + user config present → merge, overlay wins
/// - `-c PATH` only → main loaded as-is
/// - No `-c`, user config present → user config is the only source
/// - No `-c`, no user config → empty defaults
pub fn load_config(path: Option<&str>) -> Result<ReportConfig, String> {
    let main_value = if let Some(p) = path {
        let main_path = std::path::PathBuf::from(p);
        if !main_path.exists() {
            return Err(format!("config file not found: {}", main_path.display()));
        }
        read_toml_value(&main_path)?
    } else {
        toml::Value::Table(Default::default())
    };

    let user_cf = sandogasa_config::ConfigFile::for_tool("sandogasa-report");
    let user_present = user_cf.path().exists();
    let user_value = if user_present {
        read_toml_value(user_cf.path())?
    } else {
        toml::Value::Table(Default::default())
    };

    if path.is_none() && !user_present {
        return Ok(ReportConfig::default());
    }

    let merged = merge_toml(main_value, user_value);
    merged
        .try_into::<ReportConfig>()
        .map_err(|e| format!("failed to deserialize merged config: {e}"))
}

fn read_toml_value(path: &std::path::Path) -> Result<toml::Value, String> {
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("parsing {}: {e}", path.display()))
}

/// Deep-merge two TOML values: tables recurse, every other type
/// is overwritten by the overlay. Used to layer a per-user config
/// on top of a shared main config.
fn merge_toml(base: toml::Value, overlay: toml::Value) -> toml::Value {
    use toml::Value;
    match (base, overlay) {
        (Value::Table(mut b), Value::Table(o)) => {
            for (k, v) in o {
                let merged = match b.remove(&k) {
                    Some(existing) => merge_toml(existing, v),
                    None => v,
                };
                b.insert(k, merged);
            }
            Value::Table(b)
        }
        (_, overlay) => overlay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toml_table(input: &str) -> toml::Value {
        toml::from_str(input).unwrap()
    }

    #[test]
    fn merge_overlay_overrides_scalar() {
        let base = toml_table("[a]\nx = 1\ny = 2\n");
        let overlay = toml_table("[a]\nx = 99\n");
        let merged = merge_toml(base, overlay);
        assert_eq!(merged["a"]["x"].as_integer(), Some(99));
        assert_eq!(merged["a"]["y"].as_integer(), Some(2));
    }

    #[test]
    fn merge_overlay_adds_new_keys() {
        let base = toml_table("[a]\nx = 1\n");
        let overlay = toml_table("[b]\ny = 2\n");
        let merged = merge_toml(base, overlay);
        assert_eq!(merged["a"]["x"].as_integer(), Some(1));
        assert_eq!(merged["b"]["y"].as_integer(), Some(2));
    }

    #[test]
    fn merge_recurses_into_nested_tables() {
        let base = toml_table(
            r#"
[domains.hyperscale.gitlab]
instance = "https://gitlab.com"
group = "CentOS/Hyperscale"
"#,
        );
        let overlay = toml_table(
            r#"
[domains.hyperscale.gitlab]
user = "michel-slm"
"#,
        );
        let merged = merge_toml(base, overlay);
        let gl = &merged["domains"]["hyperscale"]["gitlab"];
        assert_eq!(gl["instance"].as_str(), Some("https://gitlab.com"));
        assert_eq!(gl["group"].as_str(), Some("CentOS/Hyperscale"));
        assert_eq!(gl["user"].as_str(), Some("michel-slm"));
    }

    #[test]
    fn merge_overlay_replaces_arrays_wholesale() {
        // Arrays are not deep-merged — the overlay wins as-is.
        let base = toml_table("fedora_versions = [42, 43]");
        let overlay = toml_table("fedora_versions = [44]");
        let merged = merge_toml(base, overlay);
        let arr = merged["fedora_versions"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0].as_integer(), Some(44));
    }

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
