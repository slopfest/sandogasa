// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where cpu-sig-tracker's secrets are kept: the desktop keyring when
//! there is one, as entries named for the service they unlock (#79),
//! else the config file. The tool's own tests get a keyring in memory,
//! so `cargo test` never reads or writes the user's.

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

/// The GitLab token's entry, for the GitLab instance in use.
pub fn gitlab() -> SecretRef {
    let base = crate::utils::gitlab_base();
    SecretRef::for_url(&base, "sandogasa: GitLab token")
}

/// The Jira token's entry, for the Atlassian account `email` when the
/// token is one's.
pub fn jira(email: Option<&str>) -> SecretRef {
    let r = SecretRef::for_url(&crate::utils::jira_base(), "sandogasa: Jira API token");
    match email {
        Some(e) => r.account(e),
        None => r,
    }
}

/// [`sandogasa_config::secret::keep_for_file`] with [`keyring`].
pub fn keep_for_file(what: &str, secret: &SecretRef, value: String) -> String {
    sandogasa_config::secret::keep_for_file(keyring(), what, secret, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvGuard;

    /// A token `config` keeps in the keyring is what the tool then
    /// logs in with, and the file is left without it.
    #[test]
    #[serial_test::serial]
    fn a_token_kept_in_the_keyring_is_the_one_used() {
        let dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::new(&[
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            ("CPU_SIG_TRACKER_GITLAB_BASE", "https://gitlab.keyring.test"),
            // Empty is unset, to resolve().
            ("GITLAB_TOKEN", ""),
        ]);
        assert!(crate::gitlab::load_token().is_err());
        let file = keep_for_file("GitLab token", &gitlab(), "glpat-from-keyring".into());
        assert_eq!(file, "", "the file is left without it");
        assert_eq!(crate::gitlab::load_token().unwrap(), "glpat-from-keyring");
        // The environment still wins.
        let _env2 = EnvGuard::new(&[("GITLAB_TOKEN", "glpat-from-env")]);
        assert_eq!(crate::gitlab::load_token().unwrap(), "glpat-from-env");
        keyring().clear(&gitlab()).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn the_jira_token_is_keyed_by_its_account() {
        let r = jira(Some("me@example.com"));
        assert!(
            r.lookup_command()
                .ends_with("service redhat.atlassian.net account me@example.com"),
            "{}",
            r.lookup_command()
        );
        assert!(!jira(None).lookup_command().contains("account"));
    }
}
