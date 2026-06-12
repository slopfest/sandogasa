// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Detect inventory packages no longer carried on any active
//! branch (`prune-retired`).
//!
//! A package is a prune candidate when its dist-git project is
//! gone entirely (404), when none of its branches is an active
//! release branch, or when it is retired (`dead.package`) on
//! every active branch it has. A package retired on only *some*
//! branches stays. The default action is to mark candidates
//! `unshipped` in the inventory rather than delete them: a fresh
//! `sync-distgit` would re-add retired packages (their ACLs
//! remain), and keeping the tombstone lets `triage-retired`
//! keep closing their remaining bugs. `--remove` deletes
//! entries outright for those who want that.

use sandogasa_distgit::DistGitClient;
use sandogasa_inventory::Inventory;

use crate::triage_retired::{RETRY_ATTEMPTS, retry};

/// Why a package is no longer carried anywhere.
#[derive(Debug, Clone, PartialEq)]
pub enum PruneReason {
    /// `rpms/<name>` no longer exists in dist-git.
    ProjectGone,
    /// The project exists but has no branch in the active set
    /// (only EOL branches remain).
    NoActiveBranch,
    /// `dead.package` is present on every active branch the
    /// project has.
    RetiredEverywhere(Vec<String>),
}

impl PruneReason {
    /// One-line human description for the plan listing.
    pub fn describe(&self) -> String {
        match self {
            PruneReason::ProjectGone => "dist-git project gone (404)".to_string(),
            PruneReason::NoActiveBranch => "no branch on any active release".to_string(),
            PruneReason::RetiredEverywhere(branches) => {
                format!("retired on every active branch ({})", branches.join(", "))
            }
        }
    }
}

/// A package that is no longer carried on any supported branch.
#[derive(Debug, Clone)]
pub struct PruneCandidate {
    pub package: String,
    pub reason: PruneReason,
}

/// Result of a `prune-retired` scan.
pub struct RunReport {
    pub packages_checked: usize,
    /// Names of every package the scan checked (scoped by the
    /// walk filter), for bidirectional marker updates.
    pub checked: Vec<String>,
    pub candidates: Vec<PruneCandidate>,
}

/// Apply scan results to the inventory's `unshipped` markers:
/// set (or refresh) the reason on candidates, clear it on checked
/// packages that no longer qualify (revived or unretired).
/// Packages outside the scan scope are left alone. Returns the
/// number of packages whose marker changed.
pub fn apply_unshipped_marks(
    inventory: &mut Inventory,
    checked: &[String],
    candidates: &[PruneCandidate],
) -> usize {
    let reasons: std::collections::BTreeMap<&str, String> = candidates
        .iter()
        .map(|c| (c.package.as_str(), c.reason.describe()))
        .collect();
    let mut changed = 0usize;
    for name in checked {
        if let Some(pkg) = inventory.find_package_mut(name) {
            let new = reasons.get(name.as_str()).cloned();
            if new != pkg.unshipped {
                pkg.unshipped = new;
                changed += 1;
            }
        }
    }
    changed
}

/// Order active branches for checking: rawhide first (the most
/// likely live branch, so the per-package scan short-circuits
/// early), then Fedora releases newest-first, then EPEL
/// newest-first — where the minor-less branch (`epel10`) is the
/// *latest* minor from EPEL 10 onwards and sorts before its
/// versioned siblings (`epel10.2`). Unrecognized names keep
/// their relative order at the end.
pub fn order_active_branches(mut branches: Vec<String>) -> Vec<String> {
    fn key(branch: &str) -> (u8, i64, i64) {
        if branch == "rawhide" {
            return (0, 0, 0);
        }
        if let Some(n) = branch.strip_prefix('f')
            && let Ok(n) = n.parse::<i64>()
        {
            return (1, -n, 0);
        }
        if let Some(rest) = branch.strip_prefix("epel") {
            let (major, minor) = match rest.split_once('.') {
                Some((maj, min)) => (maj.parse::<i64>(), min.parse::<i64>().ok()),
                None => (rest.parse::<i64>(), None),
            };
            if let Ok(major) = major {
                // No minor = the latest minor: sort it first.
                let minor_rank = match minor {
                    None => i64::MIN,
                    Some(m) => -m,
                };
                return (2, -major, minor_rank);
            }
        }
        (3, 0, 0)
    }
    branches.sort_by_key(|b| key(b));
    branches
}

/// Intersect a project's branches with the active branch set,
/// preserving the active-set order.
pub fn relevant_branches(project: &[String], active: &[String]) -> Vec<String> {
    active
        .iter()
        .filter(|b| project.iter().any(|p| p == *b))
        .cloned()
        .collect()
}

/// Scan the inventory for packages that are no longer carried on
/// any branch in `active`. Read-only — the caller decides what to
/// do with the candidates.
pub async fn run(
    inventory: &Inventory,
    dg: &DistGitClient,
    active: &[String],
    filter: &crate::WalkFilterArgs,
    verbose: bool,
) -> Result<RunReport, String> {
    let mut candidates = Vec::new();
    let mut checked = Vec::new();

    for pkg in &inventory.package {
        if !filter.matches(&pkg.name) {
            continue;
        }
        checked.push(pkg.name.clone());

        let branches = retry(
            &format!("project_branches({})", pkg.name),
            RETRY_ATTEMPTS,
            || dg.project_branches(&pkg.name),
            verbose,
        )
        .await
        .map_err(|e| format!("dist-git branches for {}: {e}", pkg.name))?;

        let Some(branches) = branches else {
            if verbose {
                eprintln!("[poi-tracker] {}: project gone", pkg.name);
            }
            candidates.push(PruneCandidate {
                package: pkg.name.clone(),
                reason: PruneReason::ProjectGone,
            });
            continue;
        };

        let relevant = relevant_branches(&branches, active);
        if relevant.is_empty() {
            if verbose {
                eprintln!(
                    "[poi-tracker] {}: no active branch (has: {})",
                    pkg.name,
                    branches.join(", ")
                );
            }
            candidates.push(PruneCandidate {
                package: pkg.name.clone(),
                reason: PruneReason::NoActiveBranch,
            });
            continue;
        }

        let mut all_retired = true;
        for branch in &relevant {
            if verbose {
                eprintln!(
                    "[poi-tracker] {}: checking retirement on {branch}",
                    pkg.name
                );
            }
            let retired = retry(
                &format!("is_retired({}, {branch})", pkg.name),
                RETRY_ATTEMPTS,
                || dg.is_retired(&pkg.name, branch),
                verbose,
            )
            .await
            .map_err(|e| format!("dist-git is_retired for {} on {branch}: {e}", pkg.name))?;
            if !retired {
                all_retired = false;
                break;
            }
        }
        if all_retired {
            candidates.push(PruneCandidate {
                package: pkg.name.clone(),
                reason: PruneReason::RetiredEverywhere(relevant),
            });
        }
    }

    Ok(RunReport {
        packages_checked: checked.len(),
        checked,
        candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| i.to_string()).collect()
    }

    #[test]
    fn order_active_branches_rawhide_fedora_then_epel() {
        // Bodhi's listing order is alphabetical-ish; checking
        // order wants newest/most-likely-live first.
        let bodhi = s(&[
            "epel10.2", "epel10", "epel8", "epel9", "f43", "f44", "rawhide",
        ]);
        assert_eq!(
            order_active_branches(bodhi),
            s(&[
                "rawhide", "f44", "f43", "epel10", "epel10.2", "epel9", "epel8",
            ])
        );
    }

    #[test]
    fn order_active_branches_minors_descend_after_latest() {
        let branches = s(&["epel10.1", "epel10", "epel10.2"]);
        assert_eq!(
            order_active_branches(branches),
            s(&["epel10", "epel10.2", "epel10.1"])
        );
    }

    #[test]
    fn relevant_branches_intersects_in_active_order() {
        let project = s(&["epel8", "epel9", "f38", "main", "rawhide"]);
        let active = s(&["rawhide", "f44", "f43", "epel9", "epel8"]);
        assert_eq!(
            relevant_branches(&project, &active),
            s(&["rawhide", "epel9", "epel8"])
        );
    }

    #[test]
    fn relevant_branches_empty_for_eol_only_project() {
        // Only EOL branches: nothing active carries it.
        let project = s(&["el6", "f20", "f25"]);
        let active = s(&["rawhide", "f44", "epel9"]);
        assert!(relevant_branches(&project, &active).is_empty());
    }

    #[test]
    fn relevant_branches_epel_minor_versions_match_exactly() {
        // Bodhi reports both epel10 (latest minor) and epel10.N
        // (older minors still current); dist-git has matching
        // branch names, so plain string equality is correct.
        let project = s(&["epel10", "rawhide"]);
        let active = s(&["rawhide", "epel10.2", "epel10"]);
        assert_eq!(
            relevant_branches(&project, &active),
            s(&["rawhide", "epel10"])
        );
    }

    fn inv(packages: &[(&str, Option<&str>)]) -> sandogasa_inventory::Inventory {
        let mut toml =
            String::from("[inventory]\nname = \"t\"\ndescription = \"t\"\nmaintainer = \"t\"\n");
        for (name, unshipped) in packages {
            toml.push_str(&format!("\n[[package]]\nname = \"{name}\"\n"));
            if let Some(reason) = unshipped {
                toml.push_str(&format!("unshipped = \"{reason}\"\n"));
            }
        }
        sandogasa_inventory::parse(&toml).unwrap()
    }

    #[test]
    fn apply_marks_sets_clears_and_skips_unchecked() {
        let mut inventory = inv(&[
            ("gone-pkg", None),
            ("revived-pkg", Some("stale reason")),
            ("unchecked-pkg", Some("kept")),
            ("live-pkg", None),
        ]);
        let checked = s(&["gone-pkg", "revived-pkg", "live-pkg"]);
        let candidates = vec![PruneCandidate {
            package: "gone-pkg".to_string(),
            reason: PruneReason::ProjectGone,
        }];
        let changed = apply_unshipped_marks(&mut inventory, &checked, &candidates);
        assert_eq!(changed, 2); // gone-pkg set, revived-pkg cleared
        assert!(inventory.find_package("gone-pkg").unwrap().is_unshipped());
        assert!(
            !inventory
                .find_package("revived-pkg")
                .unwrap()
                .is_unshipped()
        );
        // Outside the scan scope: untouched.
        assert_eq!(
            inventory.find_package("unchecked-pkg").unwrap().unshipped,
            Some("kept".to_string())
        );
        assert!(!inventory.find_package("live-pkg").unwrap().is_unshipped());
    }

    #[test]
    fn apply_marks_idempotent() {
        let mut inventory = inv(&[("gone-pkg", Some("dist-git project gone (404)"))]);
        let checked = s(&["gone-pkg"]);
        let candidates = vec![PruneCandidate {
            package: "gone-pkg".to_string(),
            reason: PruneReason::ProjectGone,
        }];
        assert_eq!(
            apply_unshipped_marks(&mut inventory, &checked, &candidates),
            0
        );
    }

    #[test]
    fn describe_reasons() {
        assert!(PruneReason::ProjectGone.describe().contains("404"));
        assert!(
            PruneReason::NoActiveBranch
                .describe()
                .contains("active release")
        );
        let r = PruneReason::RetiredEverywhere(s(&["rawhide", "epel9"]));
        assert!(r.describe().contains("rawhide, epel9"), "{}", r.describe());
    }
}
