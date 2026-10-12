// SPDX-License-Identifier: Apache-2.0 OR MIT

pub mod claim;
mod client;
pub mod models;

pub use client::{BzClient, WriteOutcome};

/// Whether a string looks like a Bugzilla API key.
///
/// Bugzilla issues a 40-character alphanumeric key. Checking that
/// locally costs nothing and catches the mistake a hidden prompt
/// invites: a key pasted twice reads back as 80 characters, and
/// nothing about the prompt shows it happened. The alternative —
/// asking the server — means sending the credential somewhere just
/// to test it, and on a stock Bugzilla 5 the only forms it reads are
/// a write body and a URL parameter.
///
/// Deliberately a shape check, not a guarantee: a well-formed key can
/// still be revoked or belong to another instance, which only a real
/// request can tell.
pub fn looks_like_api_key(key: &str) -> bool {
    key.len() == 40 && key.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_api_key_is_forty_alphanumeric_characters() {
        assert!(looks_like_api_key(&"a1B2".repeat(10)));
        // The mistake this exists to catch: a hidden prompt shows
        // nothing when a paste lands twice, and the result is exactly
        // double length.
        assert!(!looks_like_api_key(&"a1B2".repeat(20)));
        assert!(!looks_like_api_key(""));
        // Punctuation means something else was pasted — a URL, a
        // password, a line of the config file.
        assert!(!looks_like_api_key(&format!(
            "{}-",
            "a1B2".repeat(9) + "abc"
        )));
    }
}
