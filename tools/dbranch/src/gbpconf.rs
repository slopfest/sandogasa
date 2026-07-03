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

/// The `key` of a config line (`key = value`), or `None` for blanks,
/// comments, and section headers.
fn key_of(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t.starts_with('#') || t.starts_with(';') {
        return None;
    }
    t.split_once('=').map(|(k, _)| k.trim())
}

/// Return `text` with `key` set to `value`, preserving all other
/// lines, comments, and formatting. If the key is already present its
/// value is rewritten in place (keeping its indentation). Otherwise it
/// is inserted right after the `after` key's line when given and
/// present (so e.g. `debian-tag` sits under `debian-branch`), else
/// appended at the end. Used to set a new PPA branch's `debian-branch`
/// (point it at itself) and `debian-tag` (the `ubuntu/%(version)s`
/// tag format).
pub fn set_key(text: &str, key: &str, value: &str, after: Option<&str>) -> String {
    let exists = text.lines().any(|l| key_of(l) == Some(key));
    let mut out: Vec<String> = Vec::with_capacity(text.lines().count() + 1);
    let mut done = false;
    for raw in text.lines() {
        let this_key = key_of(raw);
        let indent = &raw[..raw.len() - raw.trim_start().len()];
        if exists {
            // Rewrite the existing line in place.
            if !done && this_key == Some(key) {
                out.push(format!("{indent}{key} = {value}"));
                done = true;
                continue;
            }
        }
        out.push(raw.to_string());
        // Insert a brand-new key right after its anchor line.
        if !exists && !done && after.is_some() && this_key == after {
            out.push(format!("{indent}{key} = {value}"));
            done = true;
        }
    }
    if !done {
        out.push(format!("{key} = {value}"));
    }
    let mut result = out.join("\n");
    if text.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Build a fresh `debian/gbp.conf` for a rebuild branch that has none.
///
/// A package whose maintainer isn't the person doing the rebuild often
/// has no `debian/gbp.conf` on its Debian branch (kept clean so it can be
/// contributed upstream). The rebuild branch still needs one so `gbp dch`
/// / `gbp tag` operate on *this* branch: `debian-branch` points at the
/// branch itself and `debian-tag` (when given) uses the branch's
/// namespace format (e.g. `ubuntu/%(version)s`) — a `debian/*` branch
/// (backports) omits it, since gbp's default `debian/%(version)s` is
/// already right. Kept minimal on purpose — plumbing branches
/// (`upstream-branch`, `pristine-tar`) are left to gbp's defaults /
/// `~/.gbp.conf` rather than guessed here.
pub fn new_config(debian_branch: &str, debian_tag: Option<&str>) -> String {
    match debian_tag {
        Some(tag) => {
            format!("[DEFAULT]\ndebian-branch = {debian_branch}\ndebian-tag = {tag}\n")
        }
        None => format!("[DEFAULT]\ndebian-branch = {debian_branch}\n"),
    }
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

    #[test]
    fn set_key_rewrites_in_place() {
        let text = "[DEFAULT]\npristine-tar = True\ndebian-branch = debian/unstable\nupstream-branch = upstream\n";
        let out = set_key(text, "debian-branch", "ubuntu/questing", None);
        assert_eq!(
            out,
            "[DEFAULT]\npristine-tar = True\ndebian-branch = ubuntu/questing\nupstream-branch = upstream\n"
        );
        // Idempotent.
        assert_eq!(set_key(&out, "debian-branch", "ubuntu/questing", None), out);
        // Comments are not mistaken for the key.
        assert_eq!(
            parse(&out).debian_branch.as_deref(),
            Some("ubuntu/questing")
        );
    }

    #[test]
    fn set_key_inserts_after_anchor() {
        // debian-tag lands right under debian-branch, not at the end.
        let text = "[DEFAULT]\npristine-tar = True\ndebian-branch = ubuntu/questing\nupstream-branch = upstream\n";
        let out = set_key(
            text,
            "debian-tag",
            "ubuntu/%(version)s",
            Some("debian-branch"),
        );
        assert_eq!(
            out,
            "[DEFAULT]\npristine-tar = True\ndebian-branch = ubuntu/questing\ndebian-tag = ubuntu/%(version)s\nupstream-branch = upstream\n"
        );
        // Idempotent (already present → rewritten in place, not added).
        assert_eq!(
            set_key(
                &out,
                "debian-tag",
                "ubuntu/%(version)s",
                Some("debian-branch")
            ),
            out
        );
    }

    #[test]
    fn new_config_is_minimal_and_parses_back() {
        let text = new_config("ubuntu/resolute", Some("ubuntu/%(version)s"));
        assert_eq!(
            text,
            "[DEFAULT]\ndebian-branch = ubuntu/resolute\ndebian-tag = ubuntu/%(version)s\n"
        );
        let cfg = parse(&text);
        assert_eq!(cfg.debian_branch.as_deref(), Some("ubuntu/resolute"));
        // Plumbing keys are intentionally left to gbp's defaults.
        assert_eq!(cfg.upstream_branch, None);
        assert_eq!(cfg.pristine_tar, None);
    }

    #[test]
    fn new_config_without_tag_sets_branch_only() {
        // A backports branch keeps gbp's default debian/%(version)s tag.
        let text = new_config("debian/trixie-backports", None);
        assert_eq!(text, "[DEFAULT]\ndebian-branch = debian/trixie-backports\n");
        assert!(!text.contains("debian-tag"));
    }

    #[test]
    fn set_key_appends_when_anchor_absent() {
        // No anchor present → fall back to appending at the end.
        let out = set_key(
            "pristine-tar = True\n",
            "debian-tag",
            "ubuntu/%(version)s",
            Some("debian-branch"),
        );
        assert_eq!(
            out,
            "pristine-tar = True\ndebian-tag = ubuntu/%(version)s\n"
        );
    }
}
