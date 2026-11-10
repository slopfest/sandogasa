// SPDX-License-Identifier: Apache-2.0 OR MIT

use sandogasa_config::secret::{Before, SecretRef, SecretStore, Slot};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub gitlab: Option<GitlabConfig>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GitlabConfig {
    /// Empty when the token is in the keyring (#79).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access_token: String,
}

pub fn config_file() -> sandogasa_config::ConfigFile {
    sandogasa_config::ConfigFile::for_tool("hs-relmon")
}

pub fn config_path() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    Ok(config_file().path().to_path_buf())
}

pub fn load() -> Result<Config, Box<dyn std::error::Error>> {
    config_file().load()
}

/// The GitLab token's keyring entry: gitlab.com's, which every
/// sandogasa tool talking to gitlab.com reads.
pub fn gitlab_secret() -> SecretRef {
    SecretRef::for_url("https://gitlab.com", "sandogasa: GitLab token")
}

/// The secrets `config` holds, for [`sandogasa_config::secret::fill`]
/// and [`sandogasa_config::secret::save_with`]: the GitLab token, when
/// its table is there.
pub fn secret_slots(c: &mut Config) -> Vec<Slot<'_>> {
    c.gitlab
        .as_mut()
        .map(|g| Slot {
            table: "gitlab",
            key: "access_token",
            what: "GitLab token",
            entry: gitlab_secret(),
            value: &mut g.access_token,
            required: true,
        })
        .into_iter()
        .collect()
}

/// The GitLab token `config` offers as the saved one, from `cf`'s user
/// file alone, never `/etc`'s, which saving would copy into it: the
/// keyring's in `store` first (#79); `review` is the `--all` run. One
/// turned off is offered blank, unless `ask` takes the keyring's back,
/// which is the default, as the token is required. With how it stood,
/// for [`save_token`].
pub fn saved_token(
    cf: &sandogasa_config::ConfigFile,
    store: &dyn SecretStore,
    review: bool,
    ask: &dyn Fn(&str, bool) -> bool,
) -> (String, Vec<Before>) {
    let mut existing: Config = cf.load_user().ok().unwrap_or_default();
    existing.gitlab.get_or_insert_with(Default::default);
    let before = sandogasa_config::secret::fill(
        store,
        "hs-relmon",
        secret_slots(&mut existing),
        review,
        ask,
    );
    let saved = existing.gitlab.map(|g| g.access_token).unwrap_or_default();
    (saved, before)
}

/// Save the GitLab `token`, in the keyring `store` when there is one and
/// out of `cf` then, `before` being what [`saved_token`] said. Whether
/// `cf` is still there.
pub fn save_token(
    cf: &sandogasa_config::ConfigFile,
    store: &dyn SecretStore,
    token: String,
    before: &[Before],
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut cfg = Config {
        gitlab: Some(GitlabConfig {
            access_token: token,
        }),
    };
    sandogasa_config::secret::save_with(cf, store, "hs-relmon", &mut cfg, before, secret_slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandogasa_config::secret::{MemoryStore, never};

    #[test]
    fn test_config_deserialize() {
        let toml_str = r#"
[gitlab]
access_token = "glpat-abc123"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.gitlab.as_ref().unwrap().access_token, "glpat-abc123");
    }

    #[test]
    fn test_config_deserialize_no_gitlab() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.gitlab.is_none());
    }

    #[test]
    fn test_config_serialize() {
        let config = Config {
            gitlab: Some(GitlabConfig {
                access_token: "glpat-abc123".into(),
            }),
        };
        let s = toml::to_string_pretty(&config).unwrap();
        assert!(s.contains("[gitlab]"));
        assert!(s.contains("access_token = \"glpat-abc123\""));
    }

    #[test]
    fn test_config_roundtrip() {
        let config = Config {
            gitlab: Some(GitlabConfig {
                access_token: "test-token".into(),
            }),
        };
        let s = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&s).unwrap();
        assert_eq!(parsed.gitlab.unwrap().access_token, "test-token");
    }

    #[test]
    fn test_config_path() {
        let path = config_path().unwrap();
        assert!(path.ends_with("hs-relmon/config.toml"));
    }

    #[test]
    fn test_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(dir.path().join("config.toml"));
        let config = Config {
            gitlab: Some(GitlabConfig {
                access_token: "test-save-load".into(),
            }),
        };
        cf.save(&config).unwrap();
        let loaded: Config = cf.load().unwrap();
        assert_eq!(loaded.gitlab.unwrap().access_token, "test-save-load");
    }

    #[test]
    fn test_load_missing_file() {
        let cf = sandogasa_config::ConfigFile::from_path("/tmp/hs-relmon-nonexistent/x".into());
        let result: Result<Config, _> = cf.load();
        assert!(result.is_err());
    }

    /// A token in an old file is offered keyring-first, then moved to
    /// the keyring, and the file left with nothing is removed.
    #[test]
    fn the_token_is_offered_from_the_keyring_and_moved_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[gitlab]\naccess_token = \"glpat-stale\"\n").unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(path.clone());
        let store = MemoryStore::default();
        let (saved, before) = saved_token(&cf, &store, false, &never);
        assert_eq!(saved, "glpat-stale");
        assert_eq!(before, [Before::Set { keyring: None }]);
        store.store(&gitlab_secret(), "glpat-newer").unwrap();
        let (saved, before) = saved_token(&cf, &store, false, &never);
        assert_eq!(saved, "glpat-newer");
        // Enter keeps it: quietly, as the keyring holds it.
        assert!(!save_token(&cf, &store, saved, &before).unwrap());
        assert!(!path.exists());
        // With nothing saved anywhere, a new one goes to the keyring.
        let empty = MemoryStore::default();
        let (_, before) = saved_token(&cf, &empty, false, &never);
        assert_eq!(before, [Before::Absent]);
        assert!(!save_token(&cf, &empty, "glpat-new".into(), &before).unwrap());
        assert_eq!(
            empty.lookup(&gitlab_secret()).unwrap().as_deref(),
            Some("glpat-new")
        );
    }

    /// A token turned off is offered blank, and asked about only when the
    /// keyring has one to take back; taken back, it leaves the file.
    #[test]
    fn an_off_token_is_offered_only_when_taken_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[gitlab]\naccess_token = \"-\"\n").unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(path.clone());
        let store = MemoryStore::default();
        let unasked = |_: &str, _: bool| -> bool { panic!("nothing to take back") };
        assert_eq!(
            saved_token(&cf, &store, false, &unasked),
            (String::new(), vec![Before::Off])
        );
        store.store(&gitlab_secret(), "glpat-shared").unwrap();
        assert_eq!(
            saved_token(&cf, &store, false, &never),
            (String::new(), vec![Before::Off])
        );
        // Required: asked on a plain run, yes the default.
        let (saved, before) = saved_token(&cf, &store, false, &|q, default_yes| {
            assert!(q.contains("GitLab token: off for hs-relmon"), "{q}");
            default_yes
        });
        assert_eq!(saved, "glpat-shared");
        assert!(!save_token(&cf, &store, saved, &before).unwrap());
        assert!(!path.exists());
    }

    /// `config` saves what the user's file holds, never what `/etc`
    /// ships, which the tool still reads beneath it.
    #[test]
    fn an_etc_value_is_not_copied_into_the_users_file() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        let etc = dir.path().join("etc.toml");
        std::fs::write(&etc, "[gitlab]\naccess_token = \"etc-token\"\n").unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(user.clone()).with_system_path(etc);
        let loaded: Config = cf.load().unwrap();
        assert_eq!(loaded.gitlab.unwrap().access_token, "etc-token");
        let store = MemoryStore::default();
        let (saved, before) = saved_token(&cf, &store, false, &never);
        assert_eq!(saved, "", "the /etc token is not offered");
        assert_eq!(before, [Before::Absent]);
        // Kept in the keyring, and nothing of /etc's written in its place.
        assert!(!save_token(&cf, &store, "glpat-mine".into(), &before).unwrap());
        assert!(!user.exists());
        assert_eq!(
            store.lookup(&gitlab_secret()).unwrap().as_deref(),
            Some("glpat-mine")
        );
    }
}
