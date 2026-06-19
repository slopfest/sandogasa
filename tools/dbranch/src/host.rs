// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Identify the host OS via `/etc/os-release`. Used to gate operations
//! that only work on a Debian host — the proposed-update flow needs
//! `gbp dch --stable` (a newer gbp), a Debian stable build chroot, and
//! `dput` to the Debian archive, none of which an Ubuntu host provides.

use std::fs;

/// The `ID=` field of `/etc/os-release` (e.g. `debian`, `ubuntu`),
/// unquoted and lowercased; `None` if the file is missing or has no
/// `ID`.
pub fn os_release_id() -> Option<String> {
    parse_os_release_id(&fs::read_to_string("/etc/os-release").ok()?)
}

/// Whether the host is Debian (`ID=debian`). A missing/unreadable
/// os-release reads as "not Debian" (the safe default for the gate).
pub fn is_debian() -> bool {
    os_release_id().as_deref() == Some("debian")
}

/// Parse the `ID=` value from os-release text, stripping surrounding
/// quotes and lowercasing. Ignores `ID_LIKE=` and other keys.
fn parse_os_release_id(text: &str) -> Option<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("ID="))
        .map(|v| {
            v.trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_ascii_lowercase()
        })
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_id_handles_quotes_and_ignores_id_like() {
        assert_eq!(
            parse_os_release_id("NAME=Debian\nID=debian\nID_LIKE=\n").as_deref(),
            Some("debian")
        );
        // Quoted, and not confused by ID_LIKE appearing first.
        assert_eq!(
            parse_os_release_id("ID_LIKE=\"debian\"\nID=\"ubuntu\"\n").as_deref(),
            Some("ubuntu")
        );
        // Uppercase value is normalized.
        assert_eq!(
            parse_os_release_id("ID=Debian\n").as_deref(),
            Some("debian")
        );
        // No ID at all.
        assert_eq!(parse_os_release_id("NAME=Whatever\n"), None);
    }
}
