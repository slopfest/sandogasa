// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where ebranch's secrets are kept: the desktop keyring when there is
//! one, as entries named for the service they unlock (#79), so a token
//! another sandogasa tool stored for the same service is this one's
//! too; else the config file. The tool's own tests get a keyring in
//! memory, so `cargo test` never reads or writes the user's.

use sandogasa_config::secret::SecretRef;

sandogasa_config::keyring_fn!();

/// The API key's entry for the Bugzilla at `url`: Red Hat's, or RPM
/// Fusion's, each its own.
pub fn bugzilla(url: &str) -> SecretRef {
    SecretRef::for_url(url, "sandogasa: Bugzilla API key")
}

/// The token's entry for the Forgejo at `url`.
pub fn forgejo(url: &str) -> SecretRef {
    SecretRef::for_url(url, "sandogasa: Forgejo token")
}

/// The GitHub token's entry, which lifts GitHub's rate limit when
/// advisories and fix commits are looked up there.
pub fn github() -> SecretRef {
    SecretRef::for_url("https://api.github.com", "sandogasa: GitHub token")
}

/// The NVD API key's entry.
pub fn nvd() -> SecretRef {
    SecretRef::for_url("https://services.nvd.nist.gov", "sandogasa: NVD API key")
}

/// The secret, first found of: the environment variable `env`, the
/// keyring, then the config file's `from_file`; empty when none has
/// it.
pub fn resolve(env: Option<&str>, secret: &SecretRef, from_file: &str) -> String {
    sandogasa_config::secret::resolve(keyring(), env, secret, Some(from_file)).unwrap_or_default()
}

/// The GitHub token, if any: `GITHUB_TOKEN`, else the keyring's
/// entry. ebranch never asks for one, but another tool may have kept
/// it there.
pub fn github_token() -> Option<String> {
    Some(resolve(Some("GITHUB_TOKEN"), &github(), "")).filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandogasa_config::test_support::EnvGuard;

    #[test]
    fn entries_are_named_for_the_service_they_unlock() {
        assert!(
            bugzilla("https://bugzilla.redhat.com")
                .lookup_command()
                .ends_with("service bugzilla.redhat.com")
        );
        assert!(
            bugzilla("https://bugzilla.rpmfusion.org/")
                .lookup_command()
                .ends_with("service bugzilla.rpmfusion.org")
        );
        assert!(
            forgejo("https://forge.fedoraproject.org")
                .lookup_command()
                .ends_with("service forge.fedoraproject.org")
        );
        assert_eq!(github().service(), "api.github.com");
        assert_eq!(nvd().service(), "services.nvd.nist.gov");
    }

    #[test]
    #[serial_test::serial]
    fn a_github_token_comes_from_the_environment_then_the_keyring() {
        let _env = EnvGuard::new(&[("GITHUB_TOKEN", "")]);
        assert_eq!(github_token(), None);
        keyring().store(&github(), "ghp-from-keyring").unwrap();
        assert_eq!(github_token().as_deref(), Some("ghp-from-keyring"));
        let _env2 = EnvGuard::new(&[("GITHUB_TOKEN", "ghp-from-env")]);
        assert_eq!(github_token().as_deref(), Some("ghp-from-env"));
        keyring().clear(&github()).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn nothing_set_resolves_to_nothing() {
        let entry = forgejo("https://forge.clear.test");
        assert_eq!(resolve(None, &entry, ""), "");
    }
}
