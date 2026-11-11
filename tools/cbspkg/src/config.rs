// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Config file (`~/.config/cbspkg/config.toml`) and the GitLab
//! instance the SIGs' repos are on.

use sandogasa_config::secret::{ConfigSecret, SecretRef};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub gitlab: Option<GitlabConfig>,
    /// Per package: what `build`, `promote` and `add-pkg` do without
    /// `--variant` or `--rhel` (`[packages.perf]`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub packages: std::collections::BTreeMap<String, PackageConfig>,
}

/// A package's Hyperscale variants, as `--variant` and `--rhel` give
/// them: perf's `variants = ["main"]`, `rhel = ["facebook"]`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageConfig {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rhel: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GitlabConfig {
    /// Empty when the token is in the keyring (#79).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access_token: String,
}

pub fn config_file() -> sandogasa_config::ConfigFile {
    sandogasa_config::ConfigFile::for_tool("cbspkg")
}

pub fn load() -> Result<Config, Box<dyn std::error::Error>> {
    config_file().load()
}

pub fn config_path() -> std::path::PathBuf {
    config_file().path().to_path_buf()
}

/// The GitLab instance: gitlab.com, or `CBSPKG_GITLAB_BASE` (for
/// testing against a local server).
pub fn gitlab_base() -> String {
    std::env::var("CBSPKG_GITLAB_BASE").unwrap_or_else(|_| "https://gitlab.com".to_string())
}

sandogasa_config::keyring_fn!();

/// The GitLab token's keyring entry, for the GitLab instance in use:
/// the same one every sandogasa tool reads for that host.
pub fn gitlab_secret() -> SecretRef {
    SecretRef::for_url(&gitlab_base(), "sandogasa: GitLab token")
}

/// The GitLab token as the config file keeps it, for `config`: in the
/// keyring when there is one, else as `[gitlab] access_token`.
pub fn gitlab_token_secret() -> ConfigSecret<'static> {
    ConfigSecret::new(
        "cbspkg",
        "gitlab",
        "access_token",
        "GitLab token",
        gitlab_secret(),
    )
}

/// The GitLab token: `GITLAB_TOKEN`, else the keyring's, else the
/// config file's.
pub fn gitlab_token() -> Option<String> {
    let file = load().ok().and_then(|c| c.gitlab.map(|g| g.access_token));
    sandogasa_config::secret::resolve(
        keyring(),
        Some("GITLAB_TOKEN"),
        &gitlab_secret(),
        file.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::EnvGuard;

    /// The env pointing the config and GitLab at a temp dir and `base`.
    fn env(dir: &std::path::Path, base: &str) -> EnvGuard {
        let config = dir.display().to_string();
        EnvGuard::new(&[("XDG_CONFIG_HOME", &config), ("CBSPKG_GITLAB_BASE", base)])
            .unset(&["GITLAB_TOKEN"])
    }

    #[test]
    #[serial_test::serial]
    fn the_config_is_saved_read_and_trimmed_in_the_users_config_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = env(tmp.path(), "http://127.0.0.1:1");
        assert_eq!(config_path(), tmp.path().join("cbspkg/config.toml"));
        // No file yet: an error saying how to make one.
        let err = config_file().load_user::<Config>().unwrap_err().to_string();
        assert!(err.contains("Run 'config'"), "{err}");
        config_file()
            .save(&Config {
                gitlab: Some(GitlabConfig {
                    access_token: "in-file".into(),
                }),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(load().unwrap().gitlab.unwrap().access_token, "in-file");
        assert_eq!(gitlab_token().as_deref(), Some("in-file"));
        // Nothing left once the token goes: the file is removed.
        assert!(config_file().remove("gitlab", "access_token").unwrap());
        assert!(!config_path().exists());
    }

    #[test]
    #[serial_test::serial]
    fn the_gitlab_token_comes_from_the_env_then_the_keyring_then_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = env(tmp.path(), "http://127.0.0.1:2");
        assert_eq!(gitlab_base(), "http://127.0.0.1:2");
        assert_eq!(gitlab_secret().service(), "127.0.0.1:2");
        assert_eq!(gitlab_token(), None);
        config_file()
            .save(&Config {
                gitlab: Some(GitlabConfig {
                    access_token: "in-file".into(),
                }),
                ..Default::default()
            })
            .unwrap();
        keyring().store(&gitlab_secret(), "in-keyring").unwrap();
        assert_eq!(gitlab_token().as_deref(), Some("in-keyring"));
        let _token = EnvGuard::new(&[("GITLAB_TOKEN", "in-env")]);
        assert_eq!(gitlab_token().as_deref(), Some("in-env"));
        keyring().clear(&gitlab_secret()).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn gitlab_com_is_the_default_instance() {
        let _env = EnvGuard::new(&[]).unset(&["CBSPKG_GITLAB_BASE"]);
        assert_eq!(gitlab_base(), "https://gitlab.com");
    }

    /// perf's defaults, as the README gives them.
    #[test]
    fn a_package_table_sets_its_variants() {
        let c: Config =
            toml::from_str("[packages.perf]\nvariants = [\"main\"]\nrhel = [\"facebook\"]\n")
                .unwrap();
        assert_eq!(
            c.packages["perf"],
            PackageConfig {
                variants: vec!["main".into()],
                rhel: vec!["facebook".into()],
            }
        );
        let none: Config = toml::from_str("").unwrap();
        assert!(none.packages.is_empty());
        // A misspelt key is an error, not a table quietly ignored.
        assert!(toml::from_str::<Config>("[packages.perf]\nvariant = [\"main\"]\n").is_err());
    }
}
