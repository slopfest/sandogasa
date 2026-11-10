// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Config file (`~/.config/cbspkg/config.toml`) and the GitLab
//! instance the SIGs' repos are on.

use sandogasa_config::secret::{SecretRef, SecretStore};
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

#[derive(Debug, Serialize, Deserialize)]
pub struct GitlabConfig {
    /// Empty when the token is in the keyring (#79).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access_token: String,
}

fn config_file() -> sandogasa_config::ConfigFile {
    sandogasa_config::ConfigFile::for_tool("cbspkg")
}

pub fn load() -> Result<Config, Box<dyn std::error::Error>> {
    config_file().load()
}

/// The config file alone, without `/etc`, for the `config` command to
/// rewrite.
pub fn load_user() -> Result<Config, Box<dyn std::error::Error>> {
    config_file().load_user()
}

pub fn save(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    config_file().save(config)
}

/// Take `key` out of `table` in the config file: for a secret now in
/// the keyring, which `save` would otherwise leave there. Whether the
/// file, left with nothing in it, was removed.
pub fn remove(table: &str, key: &str) -> Result<bool, Box<dyn std::error::Error>> {
    config_file().remove(table, key)
}

pub fn config_path() -> std::path::PathBuf {
    config_file().path().to_path_buf()
}

/// The GitLab instance: gitlab.com, or `CBSPKG_GITLAB_BASE` (for
/// testing against a local server).
pub fn gitlab_base() -> String {
    std::env::var("CBSPKG_GITLAB_BASE").unwrap_or_else(|_| "https://gitlab.com".to_string())
}

/// The keyring: `secret-tool`, or in this crate's tests one in memory,
/// so `cargo test` never reads or writes the user's.
pub fn keyring() -> &'static dyn SecretStore {
    #[cfg(test)]
    {
        static MEMORY: std::sync::OnceLock<sandogasa_config::secret::MemoryStore> =
            std::sync::OnceLock::new();
        MEMORY.get_or_init(Default::default)
    }
    #[cfg(not(test))]
    {
        &sandogasa_config::secret::SecretTool
    }
}

/// The GitLab token's keyring entry, for the GitLab instance in use:
/// the same one every sandogasa tool reads for that host.
pub fn gitlab_secret() -> SecretRef {
    SecretRef::for_url(&gitlab_base(), "sandogasa: GitLab token")
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
        EnvGuard::new(&[
            ("XDG_CONFIG_HOME", Some(&config)),
            ("CBSPKG_GITLAB_BASE", Some(base)),
            ("GITLAB_TOKEN", None),
        ])
    }

    #[test]
    #[serial_test::serial]
    fn the_config_is_saved_read_and_trimmed_in_the_users_config_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let _env = env(tmp.path(), "http://127.0.0.1:1");
        assert_eq!(config_path(), tmp.path().join("cbspkg/config.toml"));
        // No file yet: an error saying how to make one.
        let err = load_user().unwrap_err().to_string();
        assert!(err.contains("Run 'config'"), "{err}");
        save(&Config {
            gitlab: Some(GitlabConfig {
                access_token: "in-file".into(),
            }),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(load().unwrap().gitlab.unwrap().access_token, "in-file");
        assert_eq!(gitlab_token().as_deref(), Some("in-file"));
        // Nothing left once the token goes: the file is removed.
        assert!(remove("gitlab", "access_token").unwrap());
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
        save(&Config {
            gitlab: Some(GitlabConfig {
                access_token: "in-file".into(),
            }),
            ..Default::default()
        })
        .unwrap();
        keyring().store(&gitlab_secret(), "in-keyring").unwrap();
        assert_eq!(gitlab_token().as_deref(), Some("in-keyring"));
        let _token = EnvGuard::new(&[("GITLAB_TOKEN", Some("in-env"))]);
        assert_eq!(gitlab_token().as_deref(), Some("in-env"));
        keyring().clear(&gitlab_secret()).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn gitlab_com_is_the_default_instance() {
        let _env = EnvGuard::new(&[("CBSPKG_GITLAB_BASE", None)]);
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
