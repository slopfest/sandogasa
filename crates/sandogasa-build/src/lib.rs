// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Build-script helper for the sandogasa tools: records which git
//! checkout a binary was built from, so `--version` can say
//! `v0.22.0-74-ge11cff1-dirty` — the last tag, the commits on top of
//! it, the commit, and whether the tree had uncommitted changes —
//! instead of the bare crate version.
//!
//! A tool's `build.rs` is one call, `sandogasa_build::emit_git_describe()`
//! in its `main`:
//!
//! ```no_run
//! sandogasa_build::emit_git_describe();
//! ```
//!
//! and its clap command uses `sandogasa_cli::version!()` /
//! `sandogasa_cli::banner!()`, which read the value this emits. When
//! there is no checkout to describe — a crates.io install, a distro
//! build from the tarball — nothing is emitted and the tool shows the
//! crate version, so those builds stay reproducible.

use std::path::Path;
use std::process::Command;

/// The environment variable the tool's crate reads at compile time.
pub const ENV: &str = "SANDOGASA_GIT_DESCRIBE";

/// Run from a tool's `build.rs`: emit `cargo:rustc-env=<ENV>=<git
/// describe --tags --dirty --always>` when the manifest directory is
/// inside a git checkout, and the `rerun-if-changed` lines that keep
/// the value current — the checkout's HEAD, index and tags, and the
/// tool's own sources for the dirty flag.
pub fn emit_git_describe() {
    let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    let Some(git_dir) = git(&manifest_dir, &["rev-parse", "--absolute-git-dir"]) else {
        return;
    };
    let Some(describe) = git(
        &manifest_dir,
        &["describe", "--tags", "--dirty", "--always"],
    ) else {
        return;
    };
    println!("cargo:rustc-env={ENV}={describe}");
    for rel in ["HEAD", "index", "packed-refs", "refs/tags"] {
        println!(
            "cargo:rerun-if-changed={}",
            Path::new(&git_dir).join(rel).display()
        );
    }
    for rel in ["src", "Cargo.toml", "build.rs"] {
        println!(
            "cargo:rerun-if-changed={}",
            Path::new(&manifest_dir).join(rel).display()
        );
    }
}

/// One git command's trimmed stdout; `None` when git is missing, the
/// directory is not a checkout, or the command fails.
fn git(dir: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_returns_none_outside_a_checkout() {
        let dir = std::env::temp_dir();
        // Even where git exists, a temp dir is no checkout: `rev-parse`
        // fails, and the helper says nothing rather than erroring.
        assert_eq!(
            git(dir.to_str().unwrap(), &["rev-parse", "--absolute-git-dir"]),
            None
        );
        assert_eq!(
            git(dir.to_str().unwrap(), &["--version", "--nonsense-flag"]),
            None
        );
    }
}
