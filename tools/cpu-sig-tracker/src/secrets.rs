// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where cpu-sig-tracker's secrets are kept: the desktop keyring when
//! there is one, as entries named for the service they unlock (#79),
//! else the config file. The tool's own tests get a keyring in memory,
//! so `cargo test` never reads or writes the user's.

use sandogasa_config::secret::{SecretRef, Slot};

sandogasa_config::keyring_fn!();

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

/// The secrets `config` holds, for [`sandogasa_config::secret::fill`]
/// and [`sandogasa_config::secret::save_with`]: the GitLab token, then
/// the Jira token, each when its table is there. `config` gives the
/// GitLab table before filling, so the slots line up between the two.
pub fn slots(c: &mut crate::config::Config) -> Vec<Slot<'_>> {
    let mut slots = Vec::new();
    if let Some(g) = c.gitlab.as_mut() {
        slots.push(Slot {
            table: "gitlab",
            key: "access_token",
            what: "GitLab token",
            entry: gitlab(),
            value: &mut g.access_token,
            required: true,
        });
    }
    if let Some(j) = c.jira.as_mut() {
        let entry = jira(j.email.as_deref());
        slots.push(Slot {
            table: "jira",
            key: "access_token",
            what: "JIRA API token",
            entry,
            value: &mut j.access_token,
            // Anonymous access works for public issues.
            required: false,
        });
    }
    slots
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
        let file = sandogasa_config::secret::keep_for_file(
            keyring(),
            "GitLab token",
            &gitlab(),
            "glpat-from-keyring".into(),
        );
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
