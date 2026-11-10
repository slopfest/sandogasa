// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where fesco-chair's Forgejo token is kept: the desktop keyring when
//! there is one, as the entry for the forge it unlocks (#79), else the
//! config file. The tool's own tests get a keyring in memory, so
//! `cargo test` never reads or writes the user's.

use sandogasa_config::secret::{SecretRef, SecretStore};

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

/// The Forgejo token's entry: forge.fedoraproject.org's, which every
/// sandogasa tool talking to that forge reads.
pub fn forgejo() -> SecretRef {
    SecretRef::for_url(crate::sources::FORGE_URL, "sandogasa: Forgejo token")
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

    /// A token `config` keeps in the keyring is what the tool then
    /// logs in with, and the file is left without it.
    #[test]
    #[serial_test::serial]
    fn a_token_kept_in_the_keyring_is_the_one_used() {
        let dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::new(&[
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            // Empty is unset, to forge_token().
            ("FORGEJO_TOKEN_FORGE_FEDORAPROJECT_ORG", ""),
            ("FORGEJO_TOKEN", ""),
        ]);
        assert!(crate::sources::forge_token().is_err());

        // A token an older `config` left in the file is used, and is
        // moved out of it by the next save.
        let path = dir.path().join("fesco-chair/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[forgejo]\ntoken = \"from-file\"\n").unwrap();
        assert_eq!(crate::sources::forge_token().unwrap(), "from-file");

        let cf = sandogasa_config::ConfigFile::for_tool("fesco-chair");
        let config = crate::config::Config {
            forgejo: crate::config::ForgejoConfig {
                token: "from-keyring".into(),
            },
        };
        let removed = crate::config::save(&cf, config).unwrap();
        assert!(removed, "nothing else was in the file");
        assert!(!path.exists());
        assert_eq!(crate::sources::forge_token().unwrap(), "from-keyring");

        // The environment still wins, the host's variable first.
        let _env2 = EnvGuard::new(&[("FORGEJO_TOKEN", "from-env")]);
        assert_eq!(crate::sources::forge_token().unwrap(), "from-env");
        let _env3 = EnvGuard::new(&[("FORGEJO_TOKEN_FORGE_FEDORAPROJECT_ORG", "from-host-env")]);
        assert_eq!(crate::sources::forge_token().unwrap(), "from-host-env");
        keyring().clear(&forgejo()).unwrap();
    }

    #[test]
    fn the_entry_is_the_forges() {
        assert_eq!(
            forgejo().lookup_command(),
            "secret-tool lookup application sandogasa service forge.fedoraproject.org"
        );
    }
}
