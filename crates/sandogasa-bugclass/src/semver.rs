// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Classify pending version bumps by semver impact.
//!
//! Shared by poi-tracker's `semver-audit` (interactive one-shot
//! view) and sandogasa-pkg-health's `pending_update` check
//! (persisted, aged report) so both classify identically.
//!
//! Classification uses Cargo's compatibility rule (the Rust
//! convention): a change at or before the leftmost non-zero
//! component of the current version is breaking, so `1.4 -> 1.5`
//! is non-breaking but `0.4 -> 0.5` is breaking. Versions that
//! aren't plain dotted integers (pre-releases, dates, snapshots)
//! are reported as [`Bump::NeedsReview`] rather than guessed at.

use serde::Serialize;

/// Semver impact of a pending update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bump {
    /// The packaged version already equals the "available" version
    /// — a stale release-monitoring bug, not a pending update.
    UpToDate,
    /// The spec already carries the new version, but the release
    /// doesn't: no build with it is tagged into the release's
    /// Koji tag chain (it's in flight — a side tag, gating, or
    /// just committed). Nothing to close yet.
    PendingRelease,
    /// Safe under the Cargo compatibility rule (patch, or minor
    /// when the leading component is non-zero).
    NonBreaking,
    /// Changes the version's significant (leftmost non-zero)
    /// component.
    Breaking,
    /// The package is retired on the branch (a `dead.package`
    /// marker is present), so the update request is invalid —
    /// there's no live package to update.
    Retired,
    /// Could not be classified: a non-numeric version, an unknown
    /// current version, or a downgrade.
    NeedsReview,
}

impl Bump {
    /// Short human-readable label for report groupings.
    pub fn label(self) -> &'static str {
        match self {
            Bump::UpToDate => "Up to date (stale bug)",
            Bump::PendingRelease => "Committed, awaiting release",
            Bump::NonBreaking => "Non-breaking",
            Bump::Breaking => "Breaking",
            Bump::Retired => "Retired (update request invalid)",
            Bump::NeedsReview => "Needs review",
        }
    }
}

/// Parse a version into its dot-separated numeric components.
/// Returns `None` if any component isn't a bare non-negative
/// integer (pre-release tags, dates with letters, git snapshots,
/// unexpanded spec macros, ...).
///
/// Semver build metadata (a `+suffix`, e.g. libbpf-sys's
/// `1.7.0+v1.7.0`) must be ignored when determining precedence,
/// so it is stripped before parsing.
pub fn numeric_components(version: &str) -> Option<Vec<u64>> {
    let v = version.trim();
    let v = v.split('+').next().unwrap_or(v);
    if v.is_empty() {
        return None;
    }
    v.split('.').map(|c| c.parse::<u64>().ok()).collect()
}

/// Whether `candidate` is at least `target`, comparing dotted
/// numeric components (shorter versions are zero-padded). Used to
/// decide whether a build addresses a release-monitoring bug.
/// Non-numeric versions only match on exact string equality.
pub fn version_at_least(candidate: &str, target: &str) -> bool {
    match (numeric_components(candidate), numeric_components(target)) {
        (Some(c), Some(t)) => {
            let width = c.len().max(t.len());
            let pad = |v: &[u64]| -> Vec<u64> {
                (0..width).map(|i| v.get(i).copied().unwrap_or(0)).collect()
            };
            pad(&c) >= pad(&t)
        }
        _ => candidate == target,
    }
}

/// Classify a `current -> new` bump using Cargo's compatibility
/// rule: a change at or before the leftmost non-zero component of
/// `current` is breaking.
pub fn classify(current: &str, new: &str) -> Bump {
    let (Some(cur), Some(new_c)) = (numeric_components(current), numeric_components(new)) else {
        return Bump::NeedsReview;
    };
    let width = cur.len().max(new_c.len());
    let cur: Vec<u64> = (0..width)
        .map(|i| cur.get(i).copied().unwrap_or(0))
        .collect();
    let new_c: Vec<u64> = (0..width)
        .map(|i| new_c.get(i).copied().unwrap_or(0))
        .collect();
    if new_c == cur {
        // Same version — the bug is stale, nothing to update.
        return Bump::UpToDate;
    }
    if new_c < cur {
        // Downgrade — unexpected for an "is available" bug.
        return Bump::NeedsReview;
    }
    // Index of the leftmost significant (non-zero) component. An
    // all-zero current version can't anchor the rule.
    let Some(lead) = cur.iter().position(|&x| x != 0) else {
        return Bump::NeedsReview;
    };
    if (0..=lead).any(|i| cur[i] != new_c[i]) {
        Bump::Breaking
    } else {
        Bump::NonBreaking
    }
}

/// Decide a package's bump given a possibly-unreadable current
/// version. A missing current version is treated as `Retired` when
/// the branch carries a `dead.package` marker (the update request
/// is moot), otherwise `NeedsReview`.
pub fn classify_with_status(current: Option<&str>, new: &str, retired: bool) -> Bump {
    match current {
        Some(cur) => classify(cur, new),
        None if retired => Bump::Retired,
        None => Bump::NeedsReview,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_same_version_is_up_to_date() {
        assert_eq!(classify("0.6.1", "0.6.1"), Bump::UpToDate);
        // Zero-padding: 1.4 == 1.4.0.
        assert_eq!(classify("1.4", "1.4.0"), Bump::UpToDate);
        // Build metadata is ignored for precedence.
        assert_eq!(classify("1.7.0+v1.6.0", "1.7.0+v1.7.0"), Bump::UpToDate);
    }

    #[test]
    fn classify_follows_cargo_rule() {
        assert_eq!(classify("1.4.0", "1.5.0"), Bump::NonBreaking);
        assert_eq!(classify("1.4.0", "1.4.1"), Bump::NonBreaking);
        assert_eq!(classify("1.4", "1.4.1"), Bump::NonBreaking);
        assert_eq!(classify("1.4.0", "2.0.0"), Bump::Breaking);
        // Pre-1.0: the minor is the significant component.
        assert_eq!(classify("0.4.0", "0.5.0"), Bump::Breaking);
        assert_eq!(classify("0.4.0", "0.4.1"), Bump::NonBreaking);
        // Pre-0.1: the patch is significant (^0.0.3 is exact).
        assert_eq!(classify("0.0.3", "0.0.4"), Bump::Breaking);
    }

    #[test]
    fn classify_ignores_build_metadata() {
        // Semver build metadata (`+...`) is ignored for precedence:
        // rust-libbpf-sys 1.6.2 -> 1.7.0+v1.7.0 is a plain minor bump.
        assert_eq!(classify("1.6.2", "1.7.0+v1.7.0"), Bump::NonBreaking);
        assert_eq!(classify("1.6.2+v1.6.2", "2.0.0"), Bump::Breaking);
        assert!(version_at_least("1.7.0+v1.7.0", "1.7.0"));
    }

    #[test]
    fn classify_oddities_need_review() {
        // Non-numeric versions.
        assert_eq!(classify("1.4.0rc1", "1.4.0"), Bump::NeedsReview);
        assert_eq!(classify("1.0", "2.0rc1"), Bump::NeedsReview);
        assert_eq!(classify("5.000a", "5.000b"), Bump::NeedsReview);
        assert_eq!(classify("1.2.3", "1.2.4.dev-r1"), Bump::NeedsReview);
        assert_eq!(classify("abc", "def"), Bump::NeedsReview);
        // Downgrade.
        assert_eq!(classify("1.5.0", "1.4.0"), Bump::NeedsReview);
        // All-zero current can't anchor the rule.
        assert_eq!(classify("0.0.0", "0.0.1"), Bump::NeedsReview);
    }

    #[test]
    fn classify_with_status_handles_missing_current() {
        assert_eq!(classify_with_status(None, "1.0", true), Bump::Retired);
        assert_eq!(classify_with_status(None, "1.0", false), Bump::NeedsReview);
        assert_eq!(
            classify_with_status(Some("0.9"), "1.0", false),
            Bump::Breaking
        );
    }

    #[test]
    fn version_at_least_compares_numerically() {
        assert!(version_at_least("1.10.0", "1.9.0"));
        assert!(version_at_least("1.9.0", "1.9"));
        assert!(!version_at_least("1.8.9", "1.9.0"));
        // Non-numeric: exact match only.
        assert!(version_at_least("1.0rc1", "1.0rc1"));
        assert!(!version_at_least("1.0rc2", "1.0rc1"));
    }

    #[test]
    fn bump_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&Bump::PendingRelease).unwrap(),
            "\"pending-release\""
        );
        assert_eq!(
            serde_json::to_string(&Bump::UpToDate).unwrap(),
            "\"up-to-date\""
        );
    }
}
