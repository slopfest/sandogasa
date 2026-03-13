// SPDX-License-Identifier: MPL-2.0

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// TOML config for batch ACL application.
///
/// ```toml
/// package = "freerdp"
///
/// [users]
/// ngompa = "admin"
/// salimma = "commit"
/// olduser = "remove"
///
/// [groups]
/// kde-sig = "commit"
/// old-group = "remove"
/// ```
#[derive(Debug, Deserialize)]
pub struct AclConfig {
    pub package: String,
    #[serde(default)]
    pub users: HashMap<String, String>,
    #[serde(default)]
    pub groups: HashMap<String, String>,
}

impl AclConfig {
    pub fn from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), Box<dyn std::error::Error>> {
        let valid_levels = ["ticket", "collaborator", "commit", "admin", "remove"];
        for (name, level) in self.users.iter().chain(self.groups.iter()) {
            if !valid_levels.contains(&level.as_str()) {
                return Err(format!(
                    "invalid ACL level '{}' for '{}' \
                     (valid: ticket, collaborator, commit, admin, remove)",
                    level, name
                )
                .into());
            }
        }
        Ok(())
    }
}

/// Persistent app config stored at
/// `~/.config/sandogasa-pkg-acl/config.toml`.
///
/// ```toml
/// [dist-git]
/// api_token = "..."
/// ```
#[derive(Debug, Deserialize, Serialize)]
pub struct AppConfig {
    #[serde(rename = "dist-git")]
    pub dist_git: DistGitConfig,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DistGitConfig {
    pub api_token: String,
}

impl AppConfig {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("sandogasa-pkg-acl")
            .join("config.toml")
    }

    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path).map_err(|e| {
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
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(path, contents)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_valid_config() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
package = "freerdp"

[users]
ngompa = "admin"
salimma = "commit"
olduser = "remove"

[groups]
kde-sig = "commit"
old-group = "remove"
"#
        )
        .unwrap();

        let config = AclConfig::from_file(tmp.path()).unwrap();
        assert_eq!(config.package, "freerdp");
        assert_eq!(config.users.len(), 3);
        assert_eq!(config.users["ngompa"], "admin");
        assert_eq!(config.users["salimma"], "commit");
        assert_eq!(config.users["olduser"], "remove");
        assert_eq!(config.groups.len(), 2);
        assert_eq!(config.groups["kde-sig"], "commit");
        assert_eq!(config.groups["old-group"], "remove");
    }

    #[test]
    fn parses_config_without_groups() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
package = "freerdp"

[users]
ngompa = "admin"
"#
        )
        .unwrap();

        let config = AclConfig::from_file(tmp.path()).unwrap();
        assert_eq!(config.package, "freerdp");
        assert_eq!(config.users.len(), 1);
        assert!(config.groups.is_empty());
    }

    #[test]
    fn parses_config_without_users() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
package = "freerdp"

[groups]
kde-sig = "commit"
"#
        )
        .unwrap();

        let config = AclConfig::from_file(tmp.path()).unwrap();
        assert!(config.users.is_empty());
        assert_eq!(config.groups.len(), 1);
    }

    #[test]
    fn parses_config_with_all_valid_levels() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
package = "freerdp"

[users]
a = "ticket"
b = "collaborator"
c = "commit"
d = "admin"
e = "remove"
"#
        )
        .unwrap();

        let config = AclConfig::from_file(tmp.path()).unwrap();
        assert_eq!(config.users.len(), 5);
    }

    #[test]
    fn rejects_invalid_acl_level() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
package = "freerdp"

[users]
ngompa = "owner"
"#
        )
        .unwrap();

        let result = AclConfig::from_file(tmp.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid ACL level"));
        assert!(err.contains("owner"));
    }

    #[test]
    fn rejects_invalid_group_level() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
package = "freerdp"

[groups]
kde-sig = "superadmin"
"#
        )
        .unwrap();

        let result = AclConfig::from_file(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn missing_package_field_errors() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
[users]
ngompa = "admin"
"#
        )
        .unwrap();

        let result = AclConfig::from_file(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn nonexistent_file_errors() {
        let result = AclConfig::from_file(Path::new("/tmp/does-not-exist-acl-cfg.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn invalid_toml_errors() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "this is not valid toml [[[").unwrap();
        let result = AclConfig::from_file(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn empty_users_and_groups() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
package = "freerdp"

[users]

[groups]
"#
        )
        .unwrap();

        let config = AclConfig::from_file(tmp.path()).unwrap();
        assert!(config.users.is_empty());
        assert!(config.groups.is_empty());
    }

    // ---- AppConfig ----

    #[test]
    fn app_config_path_ends_with_expected_components() {
        let path = AppConfig::path();
        assert!(path.ends_with("sandogasa-pkg-acl/config.toml"));
    }

    #[test]
    fn app_config_save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let config = AppConfig {
            dist_git: DistGitConfig {
                api_token: "test-token-12345".to_string(),
            },
        };
        config.save_to(&path).unwrap();

        let loaded = AppConfig::load_from(&path).unwrap();
        assert_eq!(loaded.dist_git.api_token, "test-token-12345");
    }

    #[test]
    fn app_config_save_produces_dist_git_section() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let config = AppConfig {
            dist_git: DistGitConfig {
                api_token: "tok".to_string(),
            },
        };
        config.save_to(&path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("[dist-git]"));
        assert!(contents.contains("api_token = \"tok\""));
    }

    #[test]
    fn app_config_save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deep").join("config.toml");

        let config = AppConfig {
            dist_git: DistGitConfig {
                api_token: "key".to_string(),
            },
        };
        config.save_to(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn app_config_load_nonexistent_file_errors() {
        let result = AppConfig::load_from(Path::new("/tmp/does-not-exist-99999.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn app_config_load_invalid_toml_errors() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "this is not valid toml [[[").unwrap();
        let result = AppConfig::load_from(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn app_config_load_missing_field_errors() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "other_field = \"value\"\n").unwrap();
        let result = AppConfig::load_from(tmp.path());
        assert!(result.is_err());
    }
}
