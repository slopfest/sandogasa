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

    /// Per-instance GitLab API tokens, keyed by hostname (e.g.
    /// `"gitlab.com"`, `"salsa.debian.org"`). Env vars
    /// (`GITLAB_TOKEN_<HOSTNAME>` or generic `GITLAB_TOKEN`) still
    /// take precedence, so a shell-session override works even
    /// with a saved token. Belongs in the user overlay — the
    /// values are credentials.
    #[serde(default)]
    pub gitlab_tokens: BTreeMap<String, String>,
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
    let overlay_cf = sandogasa_config::ConfigFile::for_tool("sandogasa-report");
    let overlay_path = overlay_cf.path();
    let overlay = if overlay_path.exists() {
        Some(overlay_path)
    } else {
        None
    };
    load_layered(path.map(std::path::Path::new), overlay)
}

/// Core load-and-merge logic, exposed separately so tests can
/// feed explicit overlay paths and stay isolated from whatever
/// happens to live in the developer's real
/// `~/.config/sandogasa-report/config.toml`.
fn load_layered(
    main_path: Option<&std::path::Path>,
    overlay_path: Option<&std::path::Path>,
) -> Result<ReportConfig, String> {
    let main_value = if let Some(p) = main_path {
        if !p.exists() {
            return Err(format!("config file not found: {}", p.display()));
        }
        read_toml_value(p)?
    } else {
        toml::Value::Table(Default::default())
    };

    let overlay_value = if let Some(p) = overlay_path {
        read_toml_value(p)?
    } else {
        toml::Value::Table(Default::default())
    };

    if main_path.is_none() && overlay_path.is_none() {
        return Ok(ReportConfig::default());
    }

    let merged = merge_toml(main_value, overlay_value);
    merged.try_into::<ReportConfig>().map_err(|e| {
        if main_path.is_none() {
            format!(
                "failed to deserialize merged config: {e}\n\
                 \n\
                 The user overlay at {} has per-domain entries that\n\
                 are incomplete without the shared main config. Pass\n\
                 `-c <path>` pointing at the main config that defines\n\
                 `instance`, `group`, etc. for those domains.",
                overlay_path
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<none>".to_string())
            )
        } else {
            format!("failed to deserialize merged config: {e}")
        }
    })
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

    // These exercise `load_layered` directly so the developer's
    // real `~/.config/sandogasa-report/config.toml` doesn't leak
    // into the assertions.

    #[test]
    fn load_layered_no_files_returns_default() {
        let cfg = load_layered(None, None).unwrap();
        assert!(cfg.domains.is_empty());
        assert!(cfg.users.is_empty());
        assert!(cfg.groups.is_empty());
    }

    #[test]
    fn load_layered_missing_main_errors() {
        let result = load_layered(
            Some(std::path::Path::new(
                "/tmp/nonexistent-sandogasa-report-test.toml",
            )),
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn load_layered_main_only() {
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
        let cfg = load_layered(Some(&path), None).unwrap();
        assert!(cfg.domains.contains_key("test"));
        assert!(cfg.domains["test"].bugzilla);
        assert_eq!(cfg.groups["mygroup"].packages, vec!["pkg1", "pkg2"]);
    }

    #[test]
    fn load_layered_overlay_overrides_main() {
        let dir = tempfile::tempdir().unwrap();
        let main_path = dir.path().join("main.toml");
        std::fs::write(
            &main_path,
            r#"
[domains.hyperscale.gitlab]
instance = "https://gitlab.com"
group = "CentOS/Hyperscale"
"#,
        )
        .unwrap();
        let overlay_path = dir.path().join("overlay.toml");
        std::fs::write(
            &overlay_path,
            r#"
[domains.hyperscale.gitlab]
user = "michel-slm"
"#,
        )
        .unwrap();
        let cfg = load_layered(Some(&main_path), Some(&overlay_path)).unwrap();
        let gl = cfg.domains["hyperscale"].gitlab.as_ref().unwrap();
        assert_eq!(gl.instance, "https://gitlab.com");
        assert_eq!(gl.group.as_deref(), Some("CentOS/Hyperscale"));
        assert_eq!(gl.user.as_deref(), Some("michel-slm"));
    }
}
