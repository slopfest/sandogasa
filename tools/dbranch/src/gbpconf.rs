// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal reader for git-buildpackage config (`~/.gbp.conf`,
//! `<repo>/debian/gbp.conf`). dbranch uses it to learn which branches
//! are plumbing (`upstream-branch`, the pristine-tar branch) and the
//! user's notion of the Debian branch — rather than hardcoding names.
//!
//! Note: `debian-branch` in a repo's `debian/gbp.conf` is typically
//! *self-referential* (each release branch names itself), so it is
//! **not** a reliable merge source. The merge source is detected from
//! the global `~/.gbp.conf` value plus conventional names — see
//! [`crate::plan::detect_debian_branch`].

use std::path::Path;

/// The gbp settings dbranch cares about. Fields are `None` when the
/// key is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GbpConfig {
    pub debian_branch: Option<String>,
    pub upstream_branch: Option<String>,
    pub pristine_tar: Option<bool>,
}

impl GbpConfig {
    /// Combine two configs: `self`'s set fields win, `fallback`
    /// fills any that are `None` (repo config over home config).
    pub fn or(self, fallback: GbpConfig) -> GbpConfig {
        GbpConfig {
            debian_branch: self.debian_branch.or(fallback.debian_branch),
            upstream_branch: self.upstream_branch.or(fallback.upstream_branch),
            pristine_tar: self.pristine_tar.or(fallback.pristine_tar),
        }
    }
}

/// Parse a gbp config (Python ConfigParser style). Only keys in the
/// `[DEFAULT]` section (or before any section header) are read, which
/// is where the branch settings live.
pub fn parse(text: &str) -> GbpConfig {
    let mut cfg = GbpConfig::default();
    // Keys before any `[section]` belong to DEFAULT.
    let mut in_default = true;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_default = section.eq_ignore_ascii_case("DEFAULT");
            continue;
        }
        if !in_default {
            continue;
        }
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let val = val.trim().trim_matches(['"', '\'']).to_string();
        match key.trim() {
            "debian-branch" => cfg.debian_branch = Some(val),
            "upstream-branch" => cfg.upstream_branch = Some(val),
            "pristine-tar" => cfg.pristine_tar = Some(is_truthy(&val)),
            _ => {}
        }
    }
    cfg
}

/// ConfigParser booleans: `1/yes/true/on` are true.
fn is_truthy(val: &str) -> bool {
    matches!(
        val.to_ascii_lowercase().as_str(),
        "1" | "yes" | "true" | "on"
    )
}

/// Read `~/.gbp.conf` (empty config if absent/unreadable).
pub fn read_home() -> GbpConfig {
    std::env::var_os("HOME")
        .map(|h| Path::new(&h).join(".gbp.conf"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse(&t))
        .unwrap_or_default()
}

/// Read `<repo>/debian/gbp.conf` (empty config if absent/unreadable).
pub fn read_repo(repo: &Path) -> GbpConfig {
    std::fs::read_to_string(repo.join("debian/gbp.conf"))
        .ok()
        .map(|t| parse(&t))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reads_default_section_keys() {
        // The shape of a real ~/.gbp.conf.
        let cfg = parse(
            "\
[DEFAULT]
# debian-branch = main
debian-branch = debian/unstable
upstream-branch = upstream/latest
pristine-tar = True
[buildpackage]
[import-orig]
",
        );
        assert_eq!(cfg.debian_branch.as_deref(), Some("debian/unstable"));
        assert_eq!(cfg.upstream_branch.as_deref(), Some("upstream/latest"));
        assert_eq!(cfg.pristine_tar, Some(true));
    }

    #[test]
    fn parse_handles_a_repo_gbp_conf() {
        // damo's per-branch debian/gbp.conf (debian-branch is the
        // branch itself — not a usable merge source).
        let cfg = parse(
            "\
pristine-tar = True
debian-branch = noble
upstream-branch = upstream
",
        );
        assert_eq!(cfg.debian_branch.as_deref(), Some("noble"));
        assert_eq!(cfg.upstream_branch.as_deref(), Some("upstream"));
        assert_eq!(cfg.pristine_tar, Some(true));
    }

    #[test]
    fn parse_ignores_non_default_sections() {
        let cfg = parse("[buildpackage]\nupstream-branch = wrong\n");
        assert_eq!(cfg.upstream_branch, None);
    }
}
