// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Detect GitLab-synced packages that are no longer shipped
//! (`sync-gitlab --mark-unshipped`).
//!
//! A package is considered unshipped when its GitLab project is
//! **archived** *and* it has no build tagged into a CBS `-release`
//! tag for any currently valid release. The "released" check is
//! CBS-specific (CentOS koji), distinct from the Fedora
//! `dead.package` probe used by `prune-retired`/`triage-retired`.
//!
//! Lifecycle nuance per SIG:
//! - **Hyperscale** ships for both RHEL `N` and CentOS Stream `Ns`,
//!   so a release build in either `hyperscaleN-*-release` or
//!   `hyperscaleNs-*-release` counts.
//! - **Proposed Updates** targets CentOS Stream only, so only
//!   `proposed_updatesNs-*-release` counts.

use std::collections::{BTreeMap, HashSet};

use crate::prune_retired::apply_marker;
use sandogasa_inventory::Inventory;

/// A CBS SIG whose release tags follow a known naming scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sig {
    Hyperscale,
    ProposedUpdates,
}

impl Sig {
    /// The CBS tag prefix for this SIG (`hyperscale`,
    /// `proposed_updates`).
    pub fn tag_prefix(self) -> &'static str {
        match self {
            Sig::Hyperscale => "hyperscale",
            Sig::ProposedUpdates => "proposed_updates",
        }
    }

    /// Whether a RHEL (`N`, non-stream) release tag counts as
    /// shipped for this SIG. Hyperscale ships for RHEL too;
    /// Proposed Updates is CentOS Stream only.
    pub fn ships_rhel(self) -> bool {
        matches!(self, Sig::Hyperscale)
    }

    /// Resolve the SIG from a `sync-gitlab` preset or group URL.
    /// Returns `None` for sources without a CBS release lifecycle
    /// (e.g. the `centos-stream` preset — the main distro).
    pub fn from_source(preset: Option<&str>, group_url: &str) -> Option<Sig> {
        let hay = preset.unwrap_or(group_url).to_lowercase();
        if hay.contains("hyperscale") {
            Some(Sig::Hyperscale)
        } else if hay.contains("proposed") {
            Some(Sig::ProposedUpdates)
        } else {
            None
        }
    }
}

/// The glob pattern for this SIG's release tags, e.g.
/// `hyperscale*-release`. Caller lists matching tags via koji,
/// then narrows with [`release_tag_matches`].
pub fn release_tag_glob(sig: Sig) -> String {
    format!("{}*-release", sig.tag_prefix())
}

/// Whether a CBS tag is a release tag this SIG ships through, for
/// one of `releases`.
///
/// Parses the version token right after the SIG prefix
/// (`hyperscale10s-...` → `10s`): the digits are the major
/// release and a trailing `s` marks a CentOS Stream tag. RHEL
/// (non-`s`) tags count only for SIGs that [`Sig::ships_rhel`].
pub fn release_tag_matches(tag: &str, sig: Sig, releases: &[u32]) -> bool {
    let Some(rest) = tag.strip_prefix(sig.tag_prefix()) else {
        return false;
    };
    if !tag.ends_with("-release") {
        return false;
    }
    let Some(version_token) = rest.split('-').next() else {
        return false;
    };
    let (digits, is_stream) = match version_token.strip_suffix('s') {
        Some(d) => (d, true),
        None => (version_token, false),
    };
    let Ok(major) = digits.parse::<u32>() else {
        return false;
    };
    if !releases.contains(&major) {
        return false;
    }
    is_stream || sig.ships_rhel()
}

/// How an archived GitLab project's CBS state maps to inventory
/// markers, split by whether release builds remain.
pub struct Classification<'a> {
    /// Archived and no CBS release build → `unshipped` tombstone.
    pub unshipped: BTreeMap<&'a str, String>,
    /// Archived but still has CBS release builds → `archived_builds`
    /// cleanup candidate (still ships; hs-relmon should untag).
    pub archived_builds: BTreeMap<&'a str, String>,
}

/// Classify the synced packages into the two marker categories.
///
/// Only archived projects are ever marked; an archived project is
/// `unshipped` when absent from `shipped`, otherwise it is an
/// `archived_builds` cleanup candidate. Non-archived packages map
/// to neither (their markers get cleared on apply). Pure.
pub fn classify<'a>(
    synced: &'a [String],
    archived: &HashSet<String>,
    shipped: &HashSet<String>,
) -> Classification<'a> {
    let mut c = Classification {
        unshipped: BTreeMap::new(),
        archived_builds: BTreeMap::new(),
    };
    for name in synced {
        if !archived.contains(name) {
            continue;
        }
        if shipped.contains(name) {
            c.archived_builds.insert(
                name.as_str(),
                "archived in GitLab; CBS builds remain (run hs-relmon to prune)".to_string(),
            );
        } else {
            c.unshipped.insert(
                name.as_str(),
                "archived in GitLab, no released CBS build".to_string(),
            );
        }
    }
    c
}

/// Collect the set of source package names that currently have a
/// build in any of this SIG's release tags for `releases`,
/// querying CBS via koji (profile `cbs`).
pub fn shipped_packages(
    sig: Sig,
    releases: &[u32],
    verbose: bool,
) -> Result<HashSet<String>, String> {
    let glob = release_tag_glob(sig);
    let tags: Vec<String> = sandogasa_koji::list_tags(&glob, Some("cbs"))?
        .into_iter()
        .filter(|t| release_tag_matches(t, sig, releases))
        .collect();
    if verbose {
        eprintln!(
            "[poi-tracker] {} release tag(s) for releases {releases:?}: {}",
            tags.len(),
            tags.join(", ")
        );
    }
    let mut shipped = HashSet::new();
    for tag in &tags {
        if verbose {
            eprintln!("[poi-tracker] listing packages in {tag}");
        }
        for name in sandogasa_koji::list_tagged_package_names(tag, Some("cbs"))? {
            shipped.insert(name);
        }
    }
    Ok(shipped)
}

/// Outcome of applying GitLab archival markers.
pub struct MarkOutcome {
    /// Packages now marked `unshipped` (archived + no builds),
    /// sorted.
    pub unshipped: Vec<String>,
    /// Packages now marked `archived_builds` (archived + builds),
    /// sorted.
    pub archived_builds: Vec<String>,
    /// Total markers changed (set or cleared) across both fields.
    pub changed: usize,
}

/// Apply both archival markers to `inventory` for the `synced`
/// packages, given the archived and shipped sets. Sets `unshipped`
/// / `archived_builds` per [`classify`] and clears whichever no
/// longer applies (bidirectional).
pub fn mark(
    inventory: &mut Inventory,
    synced: &[String],
    archived: &HashSet<String>,
    shipped: &HashSet<String>,
) -> MarkOutcome {
    let c = classify(synced, archived, shipped);
    let changed = apply_marker(inventory, synced, &c.unshipped, |p| &mut p.unshipped)
        + apply_marker(inventory, synced, &c.archived_builds, |p| {
            &mut p.archived_builds
        });
    // BTreeMap keys are already sorted.
    MarkOutcome {
        unshipped: c.unshipped.keys().map(|s| s.to_string()).collect(),
        archived_builds: c.archived_builds.keys().map(|s| s.to_string()).collect(),
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sig_from_preset_and_url() {
        assert_eq!(
            Sig::from_source(Some("hyperscale"), ""),
            Some(Sig::Hyperscale)
        );
        assert_eq!(
            Sig::from_source(Some("proposed-updates"), ""),
            Some(Sig::ProposedUpdates)
        );
        assert_eq!(
            Sig::from_source(None, "https://gitlab.com/CentOS/Hyperscale/rpms"),
            Some(Sig::Hyperscale)
        );
        assert_eq!(
            Sig::from_source(None, "https://gitlab.com/CentOS/proposed_updates/rpms"),
            Some(Sig::ProposedUpdates)
        );
        // centos-stream (main distro) has no CBS release lifecycle.
        assert_eq!(Sig::from_source(Some("centos-stream"), ""), None);
    }

    #[test]
    fn hyperscale_counts_rhel_and_stream() {
        let r = &[9, 10];
        assert!(release_tag_matches(
            "hyperscale10s-packages-main-release",
            Sig::Hyperscale,
            r
        ));
        // RHEL (non-stream) variant counts for Hyperscale.
        assert!(release_tag_matches(
            "hyperscale9-packages-facebook-release",
            Sig::Hyperscale,
            r
        ));
        // el8 is not a valid release here.
        assert!(!release_tag_matches(
            "hyperscale8s-packages-main-release",
            Sig::Hyperscale,
            r
        ));
        // Not a release tag.
        assert!(!release_tag_matches(
            "hyperscale10s-packages-main-testing",
            Sig::Hyperscale,
            r
        ));
    }

    #[test]
    fn proposed_updates_is_stream_only() {
        let r = &[9, 10];
        assert!(release_tag_matches(
            "proposed_updates9s-packages-main-release",
            Sig::ProposedUpdates,
            r
        ));
        // A bare-RHEL proposed_updates tag (shouldn't exist, but
        // guard anyway) does not count for a Stream-only SIG.
        assert!(!release_tag_matches(
            "proposed_updates9-packages-main-release",
            Sig::ProposedUpdates,
            r
        ));
        // Wrong SIG prefix.
        assert!(!release_tag_matches(
            "hyperscale9s-packages-main-release",
            Sig::ProposedUpdates,
            r
        ));
    }

    #[test]
    fn classify_splits_unshipped_from_archived_builds() {
        let synced = vec![
            "live-shipped".to_string(),     // active, shipped -> neither
            "archived-shipped".to_string(), // archived + builds -> archived_builds
            "archived-gone".to_string(),    // archived + no builds -> unshipped
            "live-unshipped".to_string(),   // not archived -> neither
        ];
        let archived = set(&["archived-shipped", "archived-gone"]);
        let shipped = set(&["live-shipped", "archived-shipped", "live-unshipped"]);
        let c = classify(&synced, &archived, &shipped);
        assert_eq!(
            c.unshipped.keys().collect::<Vec<_>>(),
            vec![&"archived-gone"]
        );
        assert_eq!(
            c.archived_builds.keys().collect::<Vec<_>>(),
            vec![&"archived-shipped"]
        );
    }
}
