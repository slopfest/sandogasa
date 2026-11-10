// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where sandogasa-report's forge tokens are kept: the desktop keyring
//! when there is one, one entry per forge instance, named for the
//! service it unlocks (#79), else the user overlay. The entries
//! themselves are named by each backend's `secret()`. The tool's own
//! tests get a keyring in memory, so `cargo test` never reads or
//! writes the user's.

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

/// [`sandogasa_config::secret::keep_for_file`] with [`keyring`].
pub fn keep_for_file(what: &str, secret: &SecretRef, value: String) -> String {
    sandogasa_config::secret::keep_for_file(keyring(), what, secret, value)
}
