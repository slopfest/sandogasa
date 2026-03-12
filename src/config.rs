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
    #[serde(default)]
    pub email: String,
}

impl AppConfig {
    pub fn path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("fedora-cve-triage")
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

/// Per-run config for the js-fps subcommand
#[derive(Debug, Deserialize)]
pub struct JsFpsConfig {
    pub tracker_bug: String,
    pub products: Vec<String>,
    pub components: Vec<String>,
    pub statuses: Vec<String>,
    pub reason: String,
}

impl JsFpsConfig {
    pub fn from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        Ok(config)
    }
}

/// Per-run config for the unshipped-tools subcommand
#[derive(Debug, Deserialize)]
pub struct UnshippedToolsConfig {
    pub tracker_bug: String,
    pub products: Vec<String>,
    pub components: Vec<String>,
    pub statuses: Vec<String>,
    pub reason: String,
}

impl UnshippedToolsConfig {
    pub fn from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        Ok(config)
    }
}

/// Per-run config for the bodhi-check subcommand
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct BodhiCheckConfig {
    pub tracker_bug: String,
    pub products: Vec<String>,
    #[serde(default)]
    pub components: Vec<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
    pub statuses: Vec<String>,
    pub reason: String,
    pub lag_tolerance: i64,
}

impl BodhiCheckConfig {
    pub fn from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn js_fps_config_parses_valid_toml() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
tracker_bug = "CVE-FalsePositive-Unshipped"
products = ["Fedora", "Fedora EPEL"]
components = ["vulnerability"]
statuses = ["NEW", "ASSIGNED"]
reason = "This CVE affects a JavaScript/NodeJS package not shipped in Fedora."
"#
        )
        .unwrap();

        let config = JsFpsConfig::from_file(tmp.path()).unwrap();
        assert_eq!(config.tracker_bug, "CVE-FalsePositive-Unshipped");
        assert_eq!(config.products, vec!["Fedora", "Fedora EPEL"]);
        assert_eq!(config.components, vec!["vulnerability"]);
        assert_eq!(config.statuses, vec!["NEW", "ASSIGNED"]);
        assert!(config.reason.contains("JavaScript"));
    }

    #[test]
    fn js_fps_config_missing_field_errors() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
tracker_bug = "CVE-FalsePositive-Unshipped"
products = ["Fedora"]
"#
        )
        .unwrap();

        let result = JsFpsConfig::from_file(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn js_fps_config_nonexistent_file_errors() {
        let result = JsFpsConfig::from_file(Path::new("/tmp/does-not-exist-12345.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn js_fps_config_empty_arrays() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
tracker_bug = "TRACKER-1"
products = []
components = []
statuses = []
reason = "test"
"#
        )
        .unwrap();

        let config = JsFpsConfig::from_file(tmp.path()).unwrap();
        assert!(config.products.is_empty());
        assert!(config.components.is_empty());
        assert!(config.statuses.is_empty());
    }

    // ---- AppConfig ----

    #[test]
    fn app_config_path_ends_with_expected_components() {
        let path = AppConfig::path();
        assert!(path.ends_with("fedora-cve-triage/config.toml"));
    }

    #[test]
    fn app_config_save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let config = AppConfig {
            bugzilla: BugzillaConfig {
                api_key: "test-key-12345".to_string(),
                email: "user@example.com".to_string(),
            },
        };
        config.save_to(&path).unwrap();

        let loaded = AppConfig::load_from(&path).unwrap();
        assert_eq!(loaded.bugzilla.api_key, "test-key-12345");
        assert_eq!(loaded.bugzilla.email, "user@example.com");
    }

    #[test]
    fn app_config_save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deep").join("config.toml");

        let config = AppConfig {
            bugzilla: BugzillaConfig {
                api_key: "key".to_string(),
                email: "user@example.com".to_string(),
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
        write!(tmp, "[bugzilla]\n").unwrap();
        let result = AppConfig::load_from(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn app_config_load_without_email_defaults_to_empty() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "[bugzilla]\napi_key = \"key-123\"\n").unwrap();
        let config = AppConfig::load_from(tmp.path()).unwrap();
        assert_eq!(config.bugzilla.api_key, "key-123");
        assert_eq!(config.bugzilla.email, "");
    }

    // ---- UnshippedToolsConfig ----

    #[test]
    fn unshipped_tools_config_parses_valid_toml() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
tracker_bug = "CVE-FalsePositive-Unshipped"
products = ["Fedora", "Fedora EPEL"]
components = ["vulnerability"]
statuses = ["NEW", "ASSIGNED"]
reason = "This CVE affects a tool not shipped in Fedora."
"#
        )
        .unwrap();

        let config = UnshippedToolsConfig::from_file(tmp.path()).unwrap();
        assert_eq!(config.tracker_bug, "CVE-FalsePositive-Unshipped");
        assert_eq!(config.products, vec!["Fedora", "Fedora EPEL"]);
        assert_eq!(config.components, vec!["vulnerability"]);
        assert_eq!(config.statuses, vec!["NEW", "ASSIGNED"]);
        assert!(config.reason.contains("not shipped"));
    }

    #[test]
    fn unshipped_tools_config_missing_field_errors() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
tracker_bug = "CVE-FalsePositive-Unshipped"
products = ["Fedora"]
"#
        )
        .unwrap();

        let result = UnshippedToolsConfig::from_file(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn unshipped_tools_config_nonexistent_file_errors() {
        let result =
            UnshippedToolsConfig::from_file(Path::new("/tmp/does-not-exist-unshipped.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn unshipped_tools_config_empty_arrays() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
tracker_bug = "TRACKER"
products = []
components = []
statuses = []
reason = "test"
"#
        )
        .unwrap();

        let config = UnshippedToolsConfig::from_file(tmp.path()).unwrap();
        assert!(config.products.is_empty());
        assert!(config.components.is_empty());
        assert!(config.statuses.is_empty());
    }

    // ---- BodhiCheckConfig ----

    #[test]
    fn bodhi_check_config_parses_valid_toml() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
tracker_bug = "CVE-AlreadyFixed"
products = ["Fedora", "Fedora EPEL"]
components = ["freerdp"]
statuses = ["NEW", "ASSIGNED"]
reason = "This bug is already fixed in a published Bodhi update."
lag_tolerance = 7
"#
        )
        .unwrap();

        let config = BodhiCheckConfig::from_file(tmp.path()).unwrap();
        assert_eq!(config.tracker_bug, "CVE-AlreadyFixed");
        assert_eq!(config.products, vec!["Fedora", "Fedora EPEL"]);
        assert_eq!(config.components, vec!["freerdp"]);
        assert!(config.assignees.is_empty());
        assert_eq!(config.statuses, vec!["NEW", "ASSIGNED"]);
        assert_eq!(config.reason, "This bug is already fixed in a published Bodhi update.");
        assert_eq!(config.lag_tolerance, 7);
    }

    #[test]
    fn bodhi_check_config_with_assignees() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
tracker_bug = "CVE-AlreadyFixed"
products = ["Fedora"]
assignees = ["user@example.com", "other@example.com"]
statuses = ["NEW"]
reason = "Already fixed."
lag_tolerance = 0
"#
        )
        .unwrap();

        let config = BodhiCheckConfig::from_file(tmp.path()).unwrap();
        assert!(config.components.is_empty());
        assert_eq!(
            config.assignees,
            vec!["user@example.com", "other@example.com"]
        );
    }

    #[test]
    fn bodhi_check_config_with_components_and_assignees() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
tracker_bug = "CVE-AlreadyFixed"
products = ["Fedora"]
components = ["freerdp"]
assignees = ["user@example.com"]
statuses = ["NEW"]
reason = "Already fixed."
lag_tolerance = 0
"#
        )
        .unwrap();

        let config = BodhiCheckConfig::from_file(tmp.path()).unwrap();
        assert_eq!(config.components, vec!["freerdp"]);
        assert_eq!(config.assignees, vec!["user@example.com"]);
    }

    #[test]
    fn bodhi_check_config_missing_field_errors() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
products = ["Fedora"]
"#
        )
        .unwrap();

        let result = BodhiCheckConfig::from_file(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn bodhi_check_config_nonexistent_file_errors() {
        let result = BodhiCheckConfig::from_file(Path::new("/tmp/does-not-exist-bodhi.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn bodhi_check_config_empty_arrays() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
tracker_bug = "TRACKER"
products = []
components = []
statuses = []
reason = "test"
lag_tolerance = 0
"#
        )
        .unwrap();

        let config = BodhiCheckConfig::from_file(tmp.path()).unwrap();
        assert!(config.products.is_empty());
        assert!(config.components.is_empty());
        assert!(config.statuses.is_empty());
        assert_eq!(config.lag_tolerance, 0);
    }
}
