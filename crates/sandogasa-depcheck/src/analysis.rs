// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The check: what an update changes in its Provides, whether its own
//! packages' Requires resolve, and which reverse dependencies break.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

use rayon::prelude::*;
use sandogasa_depfilter::provide_name;

use sandogasa_cli::progress::Progress;

use crate::repo::{RepoQuery, chunked, filter_none};
use crate::report::{BreakKind, BrokenRequires, ChangedProvide, RevDepResult, UnsatisfiedDep};
use crate::rich::{dep_satisfiable, extract_capability_names, leaf_cap, provide_version};

/// Whether a reverse dependency's requirement still resolves after
/// the update. Leaf capabilities the update doesn't touch resolve by
/// definition (they resolved in stable, and the update doesn't change
/// them); capabilities the update (re)provides must be satisfied by a
/// NEW provide, with version constraints evaluated under RPM
/// semantics (an unversioned provide satisfies any constraint). When
/// some other, non-updated package also provides a touched
/// capability, this can over-report — the safe direction.
pub fn requirement_survives_update(
    dep: &str,
    changed_names: &BTreeSet<String>,
    new_by_name: &BTreeMap<String, Vec<String>>,
) -> bool {
    dep_satisfiable(dep, &|term: &str| {
        let Some((cap, constraint)) = crate::rich::parse_versioned_dep(term) else {
            // Unparseable leaf (shouldn't happen once rich-dep parens
            // are peeled): assume unaffected.
            return true;
        };
        let touched = changed_names.contains(cap) || new_by_name.contains_key(cap);
        if !touched {
            return true;
        }
        let Some(provides) = new_by_name.get(cap) else {
            // The capability was removed by the update.
            return false;
        };
        match constraint {
            None => true,
            Some((op, ver)) => provides.iter().any(|p| match provide_version(p) {
                None => true,
                Some(available) => sandogasa_rpmvercmp::constraint_satisfied(available, op, ver),
            }),
        }
    })
}

/// Whether something outside the update still satisfies `dep`.
///
/// [`requirement_survives_update`] can only see what the update
/// provides, so a capability that several packages provide at
/// different versions looks broken the moment the one being updated
/// moves past the required version. Rust compat packages are built to
/// do exactly that: on f44 `crate(webpki-roots)` is provided by
/// `rust-webpki-roots` at 1.0.9, `rust-webpki-roots0.25` at 0.25.4 and
/// `rust-webpki-roots0.26` at 0.26.11, and updating the first was
/// reported as breaking a requirement on the third.
///
/// So before calling a requirement broken, ask the repository which
/// sources provide it — constraint included, so RPM does the version
/// comparison — and discount the ones this update is replacing. A
/// query that fails answers "no": inventing a survival would hide a
/// real breakage, which is the one error this check must not make.
pub fn satisfied_outside_update(dep: &str, repo: &dyn RepoQuery, updated: &BTreeSet<&str>) -> bool {
    dep_satisfiable(dep, &|term: &str| {
        repo.resolve_to_source(term).is_ok_and(|sources| {
            sources
                .iter()
                .any(|src| src != "(none)" && !updated.contains(src.as_str()))
        })
    })
}

/// Compare old and new provides, classifying each as updated or removed
/// ([`sandogasa_depfilter::compare_provides`]).
pub fn compare_provides(old: &BTreeSet<String>, new: &BTreeSet<String>) -> Vec<ChangedProvide> {
    sandogasa_depfilter::compare_provides(old, new)
        .into_iter()
        .map(|c| ChangedProvide {
            old: c.old,
            new: c.new,
        })
        .collect()
}

/// Compute changed provides using `subpkgs_provides` on both repos.
///
/// Works for @testing and any repo where source RPM queries work.
///
/// Provides are unioned across all updated packages on *both*
/// sides before comparing (matching the koji path): an update can
/// move a capability between its packages — e.g. a version bump
/// paired with a new compat package that keeps shipping the old
/// version's provides. A per-package comparison would falsely
/// report such provides as removed.
pub fn compute_changed_provides_via_subpkgs(
    updated_packages: &[String],
    stable_fedrq: &dyn RepoQuery,
    new_fedrq: &dyn RepoQuery,
    verbose: bool,
) -> Vec<ChangedProvide> {
    let old_provides: BTreeSet<String> = chunked(
        updated_packages,
        "reading current provides",
        verbose,
        |chunk| stable_fedrq.subpkgs_provides_all(chunk),
    )
    .into_iter()
    .collect();

    let new_provides: BTreeSet<String> =
        chunked(updated_packages, "reading new provides", verbose, |chunk| {
            new_fedrq.subpkgs_provides_all(chunk)
        })
        .into_iter()
        .collect();

    compare_provides(&old_provides, &new_provides)
}

/// Compute changed provides for side tags using `koji buildinfo`
/// + `fedrq pkg_provides`.
///
/// Side tags don't index source RPMs, so we get binary RPM names
/// from koji and query their provides individually.
pub fn compute_changed_provides_via_koji(
    binary_names: &[String],
    updated_packages: &[String],
    stable_fedrq: &dyn RepoQuery,
    side_tag_fedrq: &dyn RepoQuery,
    verbose: bool,
) -> Vec<ChangedProvide> {
    // Get old provides from stable repo (source-based query works here).
    let old_provides: BTreeSet<String> = chunked(
        updated_packages,
        "reading current provides",
        verbose,
        |chunk| stable_fedrq.subpkgs_provides_all(chunk),
    )
    .into_iter()
    .collect();

    // New provides: the binary RPMs koji lists for the builds
    // (`binary_names`), each one's provides read from the new repo.
    let new_provides: BTreeSet<String> =
        chunked(binary_names, "reading new provides", verbose, |chunk| {
            side_tag_fedrq.pkgs_provides(chunk)
        })
        .into_iter()
        .collect();

    compare_provides(&old_provides, &new_provides)
}

/// Check that the updated packages' subpackage Requires are
/// satisfiable on the stable repo. Returns deps that can't be
/// resolved (indicating the update would be uninstallable).
///
/// Queries provides and requires per binary RPM name (not per
/// source package) so this works on Koji repos that lack source
/// RPMs.
pub fn check_update_installability(
    updated_packages: &[String],
    binary_names: &[String],
    new_provides_full: &[String],
    new_fedrq: &dyn RepoQuery,
    stable_fedrq: &dyn RepoQuery,
    verbose: bool,
) -> Vec<UnsatisfiedDep> {
    if verbose {
        eprintln!(
            "[depcheck] checking installability of {} binary packages",
            binary_names.len()
        );
    }

    // The update's own provides satisfy each other's deps; this check
    // resolves at the capability-name level.
    let new_provides: BTreeSet<String> = new_provides_full
        .iter()
        .map(|p| provide_name(p).to_string())
        .collect();

    // Map binary RPM names back to source packages for reporting.
    let bin_to_src: BTreeMap<&str, &str> = binary_names
        .iter()
        .map(|bin| {
            let src = updated_packages
                .iter()
                .find(|src| bin.starts_with(src.as_str()) && bin[src.len()..].starts_with('-'))
                .map(|s| s.as_str())
                .unwrap_or(bin.as_str());
            (bin.as_str(), src)
        })
        .collect();

    if verbose {
        eprintln!(
            "[depcheck] {} new provides collected, checking requires",
            new_provides.len()
        );
    }

    // Memoize stable-repo capability resolution: a single capability
    // like `libstdc++.so.6` or `libQt6Core.so.6` is required by many of
    // the update's binaries, and each lookup shells out to fedrq. Cache
    // the satisfied/unsatisfied verdict per capability so we resolve it
    // once per run instead of once per requiring package.
    let cap_satisfied: Mutex<HashMap<String, bool>> = Mutex::new(HashMap::new());
    let resolves_in_stable = |cap: &str| -> bool {
        if let Some(&hit) = cap_satisfied.lock().unwrap().get(cap) {
            return hit;
        }
        let resolved = stable_fedrq.provides_of_provider(cap).unwrap_or_default();
        let satisfied = !filter_none(resolved).is_empty();
        cap_satisfied
            .lock()
            .unwrap()
            .insert(cap.to_string(), satisfied);
        satisfied
    };

    // A capability resolves if another package in the update provides
    // it, or it's available in the stable repo (memoized). Leaf terms
    // arrive with their version constraint; this check resolves at
    // the name level, so strip it first.
    let resolves = |term: &str| {
        let cap = leaf_cap(term);
        new_provides.contains(cap) || resolves_in_stable(cap)
    };

    let requires: Vec<(String, Vec<String>)> =
        chunked(binary_names, "reading requires", verbose, |chunk| {
            new_fedrq.pkgs_requires(chunk)
        });

    // Ask about every capability the requirements name at once, rather
    // than one fedrq invocation each: a large update requires a few
    // thousand. Anything the evaluation reaches that is not in this
    // set is still asked on its own, below.
    let wanted: Vec<String> = requires
        .iter()
        .flat_map(|(_, reqs)| reqs.iter())
        .filter(|dep| !sandogasa_depfilter::is_rpm_internal_dep(dep.trim()))
        .flat_map(|dep| extract_capability_names(dep))
        .filter(|cap| !cap.is_empty() && !new_provides.contains(cap.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let found: BTreeSet<String> = chunked(&wanted, "resolving requires", verbose, |chunk| {
        stable_fedrq.resolvable(chunk).into_iter().collect()
    })
    .into_iter()
    .collect();
    {
        let mut memo = cap_satisfied.lock().unwrap();
        for cap in wanted {
            let hit = found.contains(&cap);
            memo.insert(cap, hit);
        }
    }

    requires
        .into_par_iter()
        .flat_map_iter(|(name, requires)| {
            let src = bin_to_src.get(name.as_str()).copied().unwrap_or(&name);
            requires
                .into_iter()
                .filter_map(|dep| {
                    let dep = dep.trim();
                    if dep.is_empty() || sandogasa_depfilter::is_rpm_internal_dep(dep) {
                        return None;
                    }
                    // Evaluate boolean/rich deps with real semantics
                    // (`A if B` only requires A when B resolves, etc.).
                    if dep_satisfiable(dep, &resolves) {
                        return None;
                    }
                    // For a boolean dep, name the capabilities that
                    // actually failed so the reader sees which part is
                    // the problem; for a plain dep the dep is the cap.
                    let unresolved = if dep.starts_with('(') {
                        extract_capability_names(dep)
                            .into_iter()
                            .filter(|c| !resolves(c))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    Some(UnsatisfiedDep {
                        package: src.to_string(),
                        dep: dep.to_string(),
                        unresolved,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// What [`analyze`] finds: the update's own Requires that do not
/// resolve, and each reverse dependency with what of it breaks.
#[derive(Debug, Default)]
pub struct Analysis {
    pub installability_issues: Vec<UnsatisfiedDep>,
    pub reverse_deps: BTreeMap<String, RevDepResult>,
}

/// Check an update: `updated_packages` (source names) built
/// `binary_names`, readable in `new_fedrq`, and change
/// `changed_provides` against `stable_fedrq`, the repositories its
/// users have. Finds the binaries' Requires that do not resolve there,
/// and the reverse dependencies there whose Requires (FTI) or
/// BuildRequires (FTBFS) the update leaves unsatisfied.
pub fn analyze(
    updated_packages: &[String],
    binary_names: &[String],
    changed_provides: &[ChangedProvide],
    stable_fedrq: &dyn RepoQuery,
    new_fedrq: &dyn RepoQuery,
    verbose: bool,
) -> Result<Analysis, String> {
    // Collect the update's provides once (full strings, versions
    // included): the installability check resolves names against
    // them, and the reverse-dep check evaluates version constraints.
    if verbose {
        eprintln!("[depcheck] collecting provides from updated packages");
    }
    let new_provides_full: Vec<String> =
        chunked(binary_names, "collecting provides", verbose, |chunk| {
            new_fedrq.pkgs_provides(chunk)
        });
    let mut new_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for provide in &new_provides_full {
        new_by_name
            .entry(provide_name(provide).to_string())
            .or_default()
            .push(provide.clone());
    }

    let installability_issues = check_update_installability(
        updated_packages,
        binary_names,
        &new_provides_full,
        new_fedrq,
        stable_fedrq,
        verbose,
    );

    if changed_provides.is_empty() {
        return Ok(Analysis {
            installability_issues,
            reverse_deps: BTreeMap::new(),
        });
    }

    // Use old provide strings for whatrequires lookup.
    let old_provide_strings: Vec<String> = changed_provides.iter().map(|c| c.old.clone()).collect();

    if verbose {
        eprintln!("[depcheck] finding reverse dependencies");
    }

    let rev_dep_sources = filter_none(
        stable_fedrq
            .whatrequires(&old_provide_strings)
            .map_err(|e| format!("whatrequires failed: {e}"))?,
    );

    let updated_set: BTreeSet<&str> = updated_packages.iter().map(|s| s.as_str()).collect();
    let rev_deps: Vec<String> = rev_dep_sources
        .into_iter()
        .filter(|pkg| !updated_set.contains(pkg.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if verbose {
        eprintln!(
            "[depcheck] {} reverse dependencies to check: {}",
            rev_deps.len(),
            rev_deps.join(", "),
        );
    }

    let changed_names: BTreeSet<String> = changed_provides
        .iter()
        .map(|c| provide_name(&c.old).to_string())
        .collect();

    // Install-time Requires of the binaries (FTI when broken) *and*
    // the source package's BuildRequires (FTBFS when broken): a version
    // bump can FTBFS a reverse dep without touching its installed
    // binaries — e.g. a versioned BuildRequires like
    // `(crate(ctor/default) >= 0.6.0 with crate(ctor/default) < 0.7.0~)`
    // when ctor moves to 1.x. Read for all of them in batches.
    let rev_requires = chunked(
        &rev_deps,
        "reading reverse dependencies",
        verbose,
        |chunk| stable_fedrq.sources_requires(chunk),
    );

    let progress = Progress::stderr("checking reverse dependencies", rev_requires.len(), verbose);
    let results: Vec<(String, RevDepResult)> = rev_requires
        .into_par_iter()
        .map(|(pkg, binary_requires, build_requires)| {
            progress.tick(&pkg);
            let mut deps: Vec<(BreakKind, String)> = binary_requires
                .into_iter()
                .map(|dep| (BreakKind::Fti, dep))
                .collect();
            deps.extend(
                build_requires
                    .into_iter()
                    .map(|dep| (BreakKind::Ftbfs, dep)),
            );

            let mut seen: BTreeSet<(BreakKind, String)> = BTreeSet::new();
            let issues: Vec<BrokenRequires> = deps
                .into_iter()
                .filter_map(|(kind, dep)| {
                    let dep = dep.trim().to_string();
                    if dep.is_empty() || sandogasa_depfilter::is_rpm_internal_dep(&dep) {
                        return None;
                    }
                    if !seen.insert((kind, dep.clone())) {
                        return None;
                    }
                    if requirement_survives_update(&dep, &changed_names, &new_by_name) {
                        return None;
                    }
                    // The update does not satisfy it; something else
                    // in the repository may, and usually does when the
                    // capability has a compat package behind it.
                    if satisfied_outside_update(&dep, stable_fedrq, &updated_set) {
                        return None;
                    }
                    let changed: Vec<String> = extract_capability_names(&dep)
                        .into_iter()
                        .filter(|c| changed_names.contains(c) || new_by_name.contains_key(c))
                        .collect();
                    Some(BrokenRequires {
                        dep,
                        changed_provide: changed.join(", "),
                        kind,
                    })
                })
                .collect();

            let status = if issues.is_empty() { "ok" } else { "broken" };
            (
                pkg,
                RevDepResult {
                    status: status.to_string(),
                    issues,
                    arrival: None,
                },
            )
        })
        .collect();

    let reverse_deps: BTreeMap<String, RevDepResult> = results.into_iter().collect();

    Ok(Analysis {
        installability_issues,
        reverse_deps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository where one capability has several providers at
    /// different versions, as f44 has for `crate(webpki-roots)`.
    struct CompatRepo;

    impl RepoQuery for CompatRepo {
        fn repolist(&self) -> Result<Vec<String>, String> {
            Ok(vec!["fixture".to_string()])
        }
        fn src_nvrs(&self, _: &[String]) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn subpkgs_names(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn subpkgs_nvrs(&self, _: &str) -> Result<Vec<(String, String, String)>, String> {
            Ok(vec![])
        }
        fn pkgs_source_vr(&self, _: &[&str]) -> Result<Vec<(String, String)>, String> {
            Ok(vec![])
        }
        fn subpkgs_provides(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn pkg_provides(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn provides_of_provider(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn pkg_requires(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn subpkgs_requires(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn src_requires(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn whatrequires(&self, _: &[String]) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        // What `fedrq pkgs -P -S -F source_name <dep>` answers on f44.
        fn resolve_to_source(&self, dep: &str) -> Result<Vec<String>, String> {
            Ok(match dep.trim() {
                "crate(webpki-roots) = 0.26.11" => vec!["rust-webpki-roots0.26".to_string()],
                "crate(webpki-roots) = 0.25.4" => vec!["rust-webpki-roots0.25".to_string()],
                // The updated package itself, and nothing else.
                "crate(webpki-roots) = 1.0.9" => vec!["rust-webpki-roots".to_string()],
                _ => vec![],
            })
        }
    }

    #[test]
    fn a_requirement_met_by_a_compat_package_is_not_a_break() {
        // FEDORA-2026-8eeffa4c43: rust-webpki-roots goes 1.0.9 -> 1.0.10,
        // and rust-webpki-roots0.26 requires crate(webpki-roots) = 0.26.11
        // which it provides itself. The capability's *name* changed, so
        // the in-update check alone calls it broken.
        let updated: BTreeSet<&str> = ["rust-webpki-roots"].into();
        assert!(
            satisfied_outside_update("crate(webpki-roots) = 0.26.11", &CompatRepo, &updated),
            "the compat package still provides it"
        );
    }

    #[test]
    fn a_requirement_met_only_by_the_updated_package_is_still_a_break() {
        // The guard must not excuse everything: when the one source
        // providing a capability is the one being updated past it,
        // there is nothing else to fall back on.
        let updated: BTreeSet<&str> = ["rust-webpki-roots"].into();
        assert!(
            !satisfied_outside_update("crate(webpki-roots) = 1.0.9", &CompatRepo, &updated),
            "only the updated package provides this"
        );
        // And a capability nothing provides is not rescued either.
        assert!(!satisfied_outside_update(
            "crate(webpki-roots) = 9.9.9",
            &CompatRepo,
            &updated
        ));
    }

    #[test]
    fn requirement_survives_update_version_aware() {
        // The bear regression: its SRPM BuildRequires pins ctor to
        // 0.6.x with a rich constraint; the update moves ctor to 1.x.
        let bear_br = "(crate(ctor/default) >= 0.6.0 with crate(ctor/default) < 0.7.0~)";
        let changed: BTreeSet<String> = ["crate(ctor/default)".to_string()].into();
        let mut new_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
        new_by_name.insert(
            "crate(ctor/default)".to_string(),
            vec!["crate(ctor/default) = 1.0.8".to_string()],
        );
        assert!(!requirement_survives_update(
            bear_br,
            &changed,
            &new_by_name
        ));

        // A compatible bump (0.6.5) still satisfies the pin.
        new_by_name.insert(
            "crate(ctor/default)".to_string(),
            vec!["crate(ctor/default) = 0.6.5".to_string()],
        );
        assert!(requirement_survives_update(bear_br, &changed, &new_by_name));

        // A plain versioned require against the new version.
        assert!(requirement_survives_update(
            "crate(ctor/default) >= 0.6.0",
            &changed,
            &new_by_name
        ));
        assert!(!requirement_survives_update(
            "crate(ctor/default) >= 2.0.0",
            &changed,
            &new_by_name
        ));

        // A removed provide breaks even an unversioned require...
        let removed: BTreeSet<String> = ["libfoo.so.1()(64bit)".to_string()].into();
        assert!(!requirement_survives_update(
            "libfoo.so.1()(64bit)",
            &removed,
            &BTreeMap::new()
        ));
        // ...while capabilities the update never touches survive.
        assert!(requirement_survives_update(
            "libc.so.6(GLIBC_2.34)(64bit)",
            &changed,
            &new_by_name
        ));
        // An unversioned new provide satisfies any constraint.
        let mut unversioned: BTreeMap<String, Vec<String>> = BTreeMap::new();
        unversioned.insert("foo".to_string(), vec!["foo".to_string()]);
        assert!(requirement_survives_update(
            "foo >= 99",
            &changed,
            &unversioned
        ));
    }

    #[test]
    fn compare_provides_updated() {
        let old: BTreeSet<String> = ["tor = 0.4.9.5-1.el9".to_string()].into();
        let new: BTreeSet<String> = ["tor = 0.4.9.6-1.el9".to_string()].into();
        let changed = compare_provides(&old, &new);
        assert_eq!(changed.len(), 1);
        assert!(changed[0].is_updated());
        assert_eq!(changed[0].new.as_deref(), Some("tor = 0.4.9.6-1.el9"));
    }

    #[test]
    fn compare_provides_removed() {
        let old: BTreeSet<String> = ["libfoo.so.1()(64bit)".to_string()].into();
        let new: BTreeSet<String> = BTreeSet::new();
        let changed = compare_provides(&old, &new);
        assert_eq!(changed.len(), 1);
        assert!(!changed[0].is_updated());
        assert!(changed[0].new.is_none());
    }

    #[test]
    fn compare_provides_unchanged() {
        let old: BTreeSet<String> = ["bash".to_string()].into();
        let new: BTreeSet<String> = ["bash".to_string()].into();
        let changed = compare_provides(&old, &new);
        assert!(changed.is_empty());
    }

    #[test]
    fn compare_provides_credits_compat_package_in_same_update() {
        // const-oid 0.9 -> 0.10 drops crate(const-oid/std), but the
        // same update introduces the compat package rust-const-oid0.9
        // which still ships it. The callers union provides across
        // all packages in the update on both sides, so the provide
        // must come out unchanged — not "removed" (the bug behind
        // the wrong FEDORA-EPEL-2026-b89b964abe report).
        let old: BTreeSet<String> = ["crate(const-oid/std) = 0.9.6".to_string()].into();
        let new: BTreeSet<String> = [
            "crate(const-oid) = 0.10.2".to_string(),
            // From the compat package, same version string as old.
            "crate(const-oid/std) = 0.9.6".to_string(),
        ]
        .into();
        assert!(compare_provides(&old, &new).is_empty());
    }
}
