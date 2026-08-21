// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What kind of work a build is.
//!
//! Every figure this crate reports has to be per class, because the classes
//! differ in priority, in architecture coverage and in what a delay means:
//! a mass rebuild queueing behind itself is a throughput cost, while a
//! maintainer waiting four hours is a person blocked. Adding them together
//! produced the "60,000 delay-hours" figure that turned out to describe
//! releng waiting for releng.
//!
//! **Classify on `target`, not on the submitting account, wherever a target
//! exists for the purpose.** Accounts are renamed without notice, and a
//! filter built on one reports *absence* rather than failing — which is the
//! worst way for it to be wrong, because absence reads as a finding. Two
//! renames are already in the data:
//!
//! - ELN's bulk sync ran as
//!   `distrobuildsync-eln/jenkins-continuous-infra.apps.ci.centos.org`
//!   until 2026-01-08 and as `eln-buildsync` after it. An owner filter for
//!   the newer name reported that ELN built nothing for 31 straight weeks.
//! - koschei runs as `koschei/koschei-backend01.<dc>.fedoraproject.org`,
//!   and the datacentre in that hostname changed from `iad2` to `rdu3`. An
//!   exact match on `koschei` matches none of its 2.3 million builds.
//!
//! So the account tests here are prefixes, never equalities, and they are
//! backed by a target test wherever Koji gives us one: releng's rebuild
//! goes to `fNN-rebuild`, and everything ELN builds goes to `eln`,
//! `eln-candidate` or an `eln-build-side-NNNNNN` side tag.

use crate::dataset::BuildRecord;

/// The kind of work a build represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Class {
    /// releng's mass rebuild: bulk, deprioritised, contends with itself.
    MassRebuild,
    /// ELN's bulk sync, rebuilding what Rawhide builds that ELN tracks.
    ElnSync,
    /// A person building for ELN by hand, usually repairing a package that
    /// failed there rather than waiting for the next sync.
    ElnFix,
    /// koschei's dependency canaries. Skips s390x almost entirely, so it is
    /// invisible to the architecture question despite dominating any count
    /// of builds.
    Koschei,
    /// Automated CI on a change: packit on dist-git PRs, zuul, the CentOS
    /// CI jenkins. Contributor-facing latency.
    Ci,
    /// Other automation that is neither CI nor a bulk rebuild.
    Service,
    /// A maintainer's scratch build — testing, by hand.
    HandScratch,
    /// A maintainer's official build. The thing that must stay fast.
    Official,
}

impl Class {
    /// Path segment and CSV value for this class.
    pub fn slug(self) -> &'static str {
        match self {
            Class::MassRebuild => "mass-rebuild",
            Class::ElnSync => "eln-sync",
            Class::ElnFix => "eln-fix",
            Class::Koschei => "koschei",
            Class::Ci => "ci",
            Class::Service => "service",
            Class::HandScratch => "hand-scratch",
            Class::Official => "official",
        }
    }

    /// Human-readable name for report headings.
    pub fn label(self) -> &'static str {
        match self {
            Class::MassRebuild => "mass rebuild",
            Class::ElnSync => "ELN sync",
            Class::ElnFix => "ELN fix by hand",
            Class::Koschei => "koschei",
            Class::Ci => "CI",
            Class::Service => "other automation",
            Class::HandScratch => "scratch by hand",
            Class::Official => "maintainer official",
        }
    }

    /// Whether this is automated bulk work submitted faster than the
    /// builders can drain it.
    ///
    /// Bulk classes absorb their own delay — both run at priority 25,
    /// beneath maintainer work at 19-20 — so a long queue wait here is a
    /// throughput cost rather than anybody being held up. Reports should
    /// say which of the two a figure describes.
    pub fn bulk(self) -> bool {
        matches!(self, Class::MassRebuild | Class::ElnSync)
    }

    /// Whether this class is filler: work submitted to occupy builders that
    /// would otherwise idle, and which gives way to everything else.
    ///
    /// koschei runs at priority 50 against maintainer work at 19-20, and it
    /// is enormous — 84% of all builds in this store. Counting it as offered
    /// load makes an idle fleet look saturated: March 2026 put ppc64le at
    /// 1.39 utilisation with every class answering in about a minute.
    pub fn filler(self) -> bool {
        matches!(self, Class::Koschei)
    }

    /// Every class, for iterating report sections in a stable order.
    pub fn all() -> [Class; 8] {
        [
            Class::MassRebuild,
            Class::ElnSync,
            Class::ElnFix,
            Class::Koschei,
            Class::Ci,
            Class::Service,
            Class::HandScratch,
            Class::Official,
        ]
    }
}

/// Which class a build belongs to.
///
/// Taken as parts rather than a record so the store can classify in a query
/// and the tests can state a case in one line.
pub fn of(owner: Option<&str>, target: Option<&str>, scratch: bool) -> Class {
    let owner = owner.unwrap_or("");
    let target = target.unwrap_or("");
    // ELN first: its sync bot's account name also contains
    // `jenkins-continuous-infra`, which the CI test below matches, and the
    // bulk sync is the more specific answer.
    if eln_target(target) || eln_sync(owner) {
        return if eln_sync(owner) {
            Class::ElnSync
        } else if ci(owner) {
            Class::Ci
        } else {
            Class::ElnFix
        };
    }
    // A mass rebuild is what goes to an `fNN-rebuild` target. The owner
    // test is a fallback for the handful releng sends elsewhere.
    if target.ends_with("-rebuild") || owner == "releng" {
        return Class::MassRebuild;
    }
    if owner.starts_with("koschei/") {
        return Class::Koschei;
    }
    if ci(owner) {
        return Class::Ci;
    }
    if service(owner) {
        return Class::Service;
    }
    if scratch {
        return Class::HandScratch;
    }
    Class::Official
}

/// Which class a stored build belongs to.
pub fn of_build(build: &BuildRecord) -> Class {
    of(
        build.owner.as_deref(),
        build.target.as_deref(),
        build.scratch,
    )
}

/// A target ELN builds into: the tag itself, its candidate, or a side tag.
fn eln_target(target: &str) -> bool {
    target == "eln" || target.starts_with("eln-")
}

fn eln_sync(owner: &str) -> bool {
    owner == "eln-buildsync" || owner.starts_with("distrobuildsync-eln/")
}

fn ci(owner: &str) -> bool {
    owner == "packit" || owner == "zuul" || owner.contains("/jenkins-continuous-infra.")
}

fn service(owner: &str) -> bool {
    owner.starts_with("the-new-hotness/") || owner.ends_with("-bot")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_of_elns_accounts_are_the_same_class() {
        // The rename that made an owner filter report 31 weeks of silence.
        for owner in [
            "eln-buildsync",
            "distrobuildsync-eln/jenkins-continuous-infra.apps.ci.centos.org",
        ] {
            assert_eq!(
                of(Some(owner), Some("eln-build-side-144253"), false),
                Class::ElnSync,
                "{owner}"
            );
        }
    }

    #[test]
    fn koschei_is_matched_by_prefix_across_datacentres() {
        // `owner = 'koschei'` matches none of its 2.3 million builds, and
        // the hostname's datacentre changed mid-store.
        for dc in ["iad2", "rdu3"] {
            let owner = format!("koschei/koschei-backend01.{dc}.fedoraproject.org");
            assert_eq!(of(Some(&owner), Some("rawhide"), true), Class::Koschei);
        }
    }

    #[test]
    fn a_rebuild_is_known_by_its_target() {
        // Every release's rebuild goes to its own target, so this holds
        // without knowing which account releng runs as this year.
        for target in ["f43-rebuild", "f44-rebuild", "f45-rebuild"] {
            assert_eq!(of(Some("releng"), Some(target), false), Class::MassRebuild);
            assert_eq!(of(Some("someone"), Some(target), false), Class::MassRebuild);
        }
        // And releng's occasional side-tag build still counts.
        assert_eq!(
            of(Some("releng"), Some("f45-build-side-128194"), false),
            Class::MassRebuild
        );
    }

    #[test]
    fn a_person_building_for_eln_is_not_the_sync() {
        // The case worth keeping separate: a packager whose package failed
        // in ELN, building it themselves rather than waiting for a sync.
        assert_eq!(
            of(Some("yselkowitz"), Some("eln-candidate"), false),
            Class::ElnFix
        );
        // While CI aimed at eln is CI, not a hand fix.
        assert_eq!(of(Some("packit"), Some("eln"), true), Class::Ci);
        assert_eq!(of(Some("zuul"), Some("eln"), true), Class::Ci);
    }

    #[test]
    fn the_centos_ci_bot_is_ci_but_elns_sync_bot_is_not() {
        // Both accounts carry `jenkins-continuous-infra`; only one of them
        // is doing CI, so order of tests matters and this pins it.
        assert_eq!(
            of(
                Some("bpeck/jenkins-continuous-infra.apps.ci.centos.org"),
                Some("rawhide"),
                true
            ),
            Class::Ci
        );
        assert_eq!(
            of(
                Some("distrobuildsync-eln/jenkins-continuous-infra.apps.ci.centos.org"),
                Some("eln-build-side-115746"),
                false
            ),
            Class::ElnSync
        );
    }

    #[test]
    fn maintainers_split_by_scratchness_and_bots_do_not() {
        assert_eq!(
            of(Some("decathorpe"), Some("rawhide"), false),
            Class::Official
        );
        assert_eq!(
            of(Some("decathorpe"), Some("rawhide"), true),
            Class::HandScratch
        );
        assert_eq!(
            of(Some("the-new-hotness/release-monitoring.org"), None, true),
            Class::Service
        );
        assert_eq!(of(Some("bodhidev-bot"), None, false), Class::Service);
    }

    #[test]
    fn an_unattributed_build_is_official_rather_than_dropped() {
        // Builds whose owner was never swept must land somewhere; the
        // conservative answer is the class that must stay fast, so they
        // show up in the figure people care about rather than vanishing.
        assert_eq!(of(None, None, false), Class::Official);
        assert_eq!(of(None, None, true), Class::HandScratch);
    }

    #[test]
    fn only_bulk_classes_are_bulk() {
        let bulk: Vec<_> = Class::all().into_iter().filter(|c| c.bulk()).collect();
        assert_eq!(bulk, vec![Class::MassRebuild, Class::ElnSync]);
    }

    #[test]
    fn slugs_are_unique() {
        let mut slugs: Vec<_> = Class::all().iter().map(|c| c.slug()).collect();
        slugs.sort_unstable();
        let before = slugs.len();
        slugs.dedup();
        assert_eq!(slugs.len(), before);
    }
}
