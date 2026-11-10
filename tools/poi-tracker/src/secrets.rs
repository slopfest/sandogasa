// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where poi-tracker's secrets are kept: the desktop keyring when
//! there is one, as entries named for the service they unlock (#79),
//! else the config file. The tool's own tests get a keyring in memory,
//! so `cargo test` never reads or writes the user's.

use sandogasa_config::secret::SecretRef;

/// Fedora's dist-git, the Pagure instance the `[dist-git]` token is for.
pub const DISTGIT_BASE: &str = "https://src.fedoraproject.org";

sandogasa_config::keyring_fn!();

/// The Bugzilla API key's entry, for the instance at `url`.
pub fn bugzilla(url: &str) -> SecretRef {
    SecretRef::for_url(url, "sandogasa: Bugzilla API key")
}

/// The dist-git (Pagure) API token's entry, shared with
/// sandogasa-pkg-acl.
pub fn distgit() -> SecretRef {
    SecretRef::for_url(DISTGIT_BASE, "sandogasa: Pagure token")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_named_for_the_service() {
        assert_eq!(
            bugzilla("https://bugzilla.redhat.com/").lookup_command(),
            "secret-tool lookup application sandogasa service bugzilla.redhat.com"
        );
        assert_eq!(
            distgit().lookup_command(),
            "secret-tool lookup application sandogasa service src.fedoraproject.org"
        );
    }
}
