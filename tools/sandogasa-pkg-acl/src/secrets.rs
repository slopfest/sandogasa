// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where sandogasa-pkg-acl's dist-git token is kept: the desktop
//! keyring when there is one, as an entry named for the service it
//! unlocks (#79), else the config file. The tool's own tests get a
//! keyring in memory, so `cargo test` never reads or writes the user's.

use sandogasa_config::secret::SecretRef;

/// Fedora's dist-git, the Pagure instance the token is for.
pub const DISTGIT_BASE: &str = "https://src.fedoraproject.org";

sandogasa_config::keyring_fn!();

/// The dist-git (Pagure) API token's entry, shared with poi-tracker.
pub fn distgit() -> SecretRef {
    SecretRef::for_url(DISTGIT_BASE, "sandogasa: Pagure token")
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
