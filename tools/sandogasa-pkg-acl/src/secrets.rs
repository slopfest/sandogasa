// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where sandogasa-pkg-acl's dist-git token is kept: the desktop
//! keyring when there is one, as an entry named for the service it
//! unlocks (#79), else the config file. The tool's own tests get a
//! keyring in memory, so `cargo test` never reads or writes the user's.

use sandogasa_config::secret::{SecretRef, SecretStore};

/// Fedora's dist-git, the Pagure instance the token is for.
pub const DISTGIT_BASE: &str = "https://src.fedoraproject.org";

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

/// The dist-git (Pagure) API token's entry, shared with poi-tracker.
pub fn distgit() -> SecretRef {
    SecretRef::for_url(DISTGIT_BASE, "sandogasa: Pagure token")
}

/// [`sandogasa_config::secret::keep_for_file`] with [`keyring`].
pub fn keep_for_file(what: &str, secret: &SecretRef, value: String) -> String {
    sandogasa_config::secret::keep_for_file(keyring(), what, secret, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_is_named_for_the_service() {
        assert_eq!(
            distgit().lookup_command(),
            "secret-tool lookup application sandogasa service src.fedoraproject.org"
        );
    }
}
