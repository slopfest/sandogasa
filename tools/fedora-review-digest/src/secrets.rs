// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where fedora-review-digest's Bugzilla API key is kept: the desktop
//! keyring when there is one, as the entry for the Bugzilla it unlocks
//! (#79), else the config file. The crate's own tests get a keyring in
//! memory, so `cargo test` never reads or writes the user's.

use sandogasa_config::secret::{SecretRef, SecretStore};

/// The Bugzilla instance reviews are posted to.
pub const BUGZILLA_URL: &str = "https://bugzilla.redhat.com";

/// The keyring: `secret-tool`, or in this crate's tests one in memory.
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

/// The Bugzilla API key's entry: bugzilla.redhat.com's, which every
/// sandogasa tool talking to that Bugzilla reads.
pub fn bugzilla() -> SecretRef {
    SecretRef::for_url(BUGZILLA_URL, "sandogasa: Bugzilla API key")
}

/// [`sandogasa_config::secret::keep_for_file`] with [`keyring`].
pub fn keep_for_file(what: &str, secret: &SecretRef, value: String) -> String {
    sandogasa_config::secret::keep_for_file(keyring(), what, secret, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sets environment variables for its lifetime, restoring them on
    /// drop. Callers serialize with `#[serial_test::serial]`.
    struct EnvGuard(Vec<(String, Option<String>)>);

    impl EnvGuard {
        fn new(vars: &[(&str, &str)]) -> Self {
            let mut old = Vec::new();
            for (k, v) in vars {
                old.push((k.to_string(), std::env::var(k).ok()));
                // SAFETY: serialized by #[serial_test::serial].
                unsafe { std::env::set_var(k, v) };
            }
            Self(old)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.0 {
                // SAFETY: serialized by #[serial_test::serial].
                unsafe {
                    match v {
                        Some(v) => std::env::set_var(k, v),
                        None => std::env::remove_var(k),
                    }
                }
            }
        }
    }

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
        let creds = crate::config::credentials().unwrap();
        assert_eq!(creds.api_key, "from-file");
        assert_eq!(creds.email, "me@example.com");

        let cf = sandogasa_config::ConfigFile::for_tool("fedora-review-digest");
        let mut cfg: crate::config::Config = cf.load_user().unwrap();
        cfg.bugzilla.api_key = "from-keyring".into();
        let removed = crate::config::save(&cf, cfg).unwrap();
        assert!(!removed, "the email is still in the file");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("api_key"), "{text}");
        assert!(text.contains("me@example.com"), "{text}");
        assert_eq!(
            crate::config::credentials().unwrap().api_key,
            "from-keyring"
        );

        // The environment still wins.
        let _env2 = EnvGuard::new(&[("BUGZILLA_API_KEY", "from-env")]);
        assert_eq!(crate::config::credentials().unwrap().api_key, "from-env");
        keyring().clear(&bugzilla()).unwrap();
    }

    #[test]
    fn the_entry_is_the_bugzillas() {
        assert_eq!(
            bugzilla().lookup_command(),
            "secret-tool lookup application sandogasa service bugzilla.redhat.com"
        );
    }
}
