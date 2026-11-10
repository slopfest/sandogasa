// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared helpers for integration-style tests across the
//! subcommand modules: installing fake `koji` / `fedrq`
//! binaries on PATH, and the workspace's scoped env-var guard
//! ([`EnvGuard`]), re-exported.
//!
//! The module itself is gated on `cfg(test)` at the `main.rs`
//! mod declaration — everything here is compiled out of
//! production builds.

#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Write a shell script that `dispatches` on its arguments by
/// iterating `cases` in order and printing the first matching
/// response. Each case is `(substring_to_match_in_args,
/// stdout_body)`. Unmatched calls print "fake-<name>: no
/// match" to stderr and exit 1.
///
/// `name` is the basename of the script (e.g. `"koji"` or
/// `"fedrq"`). Returns the absolute path to the script.
pub fn install_fake_bin(dir: &Path, name: &str, cases: &[(&str, &str)]) -> std::path::PathBuf {
    let mut script = String::from("#!/bin/sh\nargs=\"$*\"\n");
    for (needle, body) in cases {
        // Double-quote the needle so POSIX shell doesn't try
        // to parse `--flag`-style tokens as operators in the
        // unquoted half of the glob pattern.
        script.push_str(&format!(
            "case \"$args\" in *\"{needle}\"*) cat <<'__FAKE_EOF__'\n{body}\n__FAKE_EOF__\nexit 0;; esac\n",
        ));
    }
    script.push_str(&format!(
        "echo \"fake-{name}: no match for: $args\" >&2\nexit 1\n",
    ));
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

pub use sandogasa_config::test_support::EnvGuard;
