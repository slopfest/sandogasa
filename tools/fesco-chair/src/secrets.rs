// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where fesco-chair's Forgejo token is kept: the desktop keyring when
//! there is one, as the entry for the forge it unlocks (#79), else the
//! config file. The tool's own tests get a keyring in memory, so
//! `cargo test` never reads or writes the user's.

use sandogasa_config::secret::SecretRef;

sandogasa_config::keyring_fn!();

/// The Forgejo token's entry: forge.fedoraproject.org's, which every
/// sandogasa tool talking to that forge reads.
pub fn forgejo() -> SecretRef {
    SecretRef::for_url(crate::sources::FORGE_URL, "sandogasa: Forgejo token")
}

#[cfg(test)]
mod tests {
    use super::*;

    use sandogasa_config::test_support::EnvGuard;

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
        let removed = crate::config::save(&cf, config, &[]).unwrap();
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
