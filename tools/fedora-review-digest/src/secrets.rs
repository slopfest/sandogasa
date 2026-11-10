// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where fedora-review-digest's Bugzilla API key is kept: the desktop
//! keyring when there is one, as the entry for the Bugzilla it unlocks
//! (#79), else the config file. The keyring is the caller's to pass:
//! the binary's real one, or one in memory in tests, so `cargo test`
//! never reads or writes the user's.

use sandogasa_config::secret::SecretRef;

/// The Bugzilla instance reviews are posted to.
pub const BUGZILLA_URL: &str = "https://bugzilla.redhat.com";

/// The Bugzilla API key's entry: bugzilla.redhat.com's, which every
/// sandogasa tool talking to that Bugzilla reads.
pub fn bugzilla() -> SecretRef {
    SecretRef::for_url(BUGZILLA_URL, "sandogasa: Bugzilla API key")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandogasa_config::secret::{MemoryStore, SecretStore};
    use sandogasa_config::test_support::EnvGuard;

    /// An API key `config` keeps in the keyring is what `--post` then
    /// logs in with, and the file is left without it.
    #[test]
    #[serial_test::serial]
    fn a_key_kept_in_the_keyring_is_the_one_used() {
        let dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::new(&[
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            // Blank is unset, to credentials().
            ("BUGZILLA_API_KEY", ""),
            ("BUGZILLA_EMAIL", ""),
        ]);
        // A key an older `config` left in the file is used, and is
        // moved out of it by the next save.
        let path = dir.path().join("fedora-review-digest/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[bugzilla]\napi_key = \"from-file\"\nemail = \"me@example.com\"\n",
        )
        .unwrap();
        let store = MemoryStore::default();
        let creds = crate::config::credentials(&store).unwrap();
        assert_eq!(creds.api_key, "from-file");
        assert_eq!(creds.email, "me@example.com");

        let cf = sandogasa_config::ConfigFile::for_tool("fedora-review-digest");
        let mut cfg: crate::config::Config = cf.load_user().unwrap();
        let before = crate::config::fill(&store, &mut cfg, false, &sandogasa_config::secret::never);
        cfg.bugzilla.api_key = "from-keyring".into();
        let kept = crate::config::save(&cf, &store, &mut cfg, &before).unwrap();
        assert!(kept, "the email is still in the file");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("api_key"), "{text}");
        assert!(text.contains("me@example.com"), "{text}");
        assert_eq!(
            crate::config::credentials(&store).unwrap().api_key,
            "from-keyring"
        );

        // The environment still wins.
        let _env2 = EnvGuard::new(&[("BUGZILLA_API_KEY", "from-env")]);
        assert_eq!(
            crate::config::credentials(&store).unwrap().api_key,
            "from-env"
        );
        assert_eq!(
            store.lookup(&bugzilla()).unwrap().as_deref(),
            Some("from-keyring")
        );
    }

    /// The key is required, so one turned off is offered on any run:
    /// blank, never as the marker, staying off when left so; asked
    /// whether to use the keyring's again, a yes offers it.
    #[test]
    #[serial_test::serial]
    fn a_key_turned_off_is_offered_blank() {
        use sandogasa_config::secret::{Before, OFF};
        let dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::new(&[("XDG_CONFIG_HOME", &dir.path().to_string_lossy())]);
        let path = dir.path().join("fedora-review-digest/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[bugzilla]\napi_key = \"-\"\nemail = \"me@example.com\"\n",
        )
        .unwrap();
        let store = MemoryStore::default();
        store.store(&bugzilla(), "shared").unwrap();
        let cf = sandogasa_config::ConfigFile::for_tool("fedora-review-digest");

        let mut cfg: crate::config::Config = cf.load_user().unwrap();
        let before = crate::config::fill(&store, &mut cfg, false, &sandogasa_config::secret::never);
        assert_eq!(cfg.bugzilla.api_key, "");
        assert_eq!(before, [Before::Off]);
        crate::config::save(&cf, &store, &mut cfg, &before).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(&format!("api_key = \"{OFF}\"")), "{text}");

        let mut cfg: crate::config::Config = cf.load_user().unwrap();
        // Required, so asked about even on a plain run, yes the default.
        let asked = std::cell::Cell::new(None);
        let before = crate::config::fill(&store, &mut cfg, false, &|_, default| {
            asked.set(Some(default));
            true
        });
        assert_eq!(asked.get(), Some(true));
        assert_eq!(cfg.bugzilla.api_key, "shared");
        crate::config::save(&cf, &store, &mut cfg, &before).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("api_key"), "on again:\n{text}");
    }

    #[test]
    fn the_entry_is_the_bugzillas() {
        assert_eq!(
            bugzilla().lookup_command(),
            "secret-tool lookup application sandogasa service bugzilla.redhat.com"
        );
    }
}
