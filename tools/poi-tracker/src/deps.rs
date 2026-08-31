// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `deps` subcommand.
//!
//! Walk the transitive *runtime* dependency graph of the inventory's
//! packages against a fedrq branch/repo stack, classify every provider
//! by the repository it comes from, and collect the providers pulled
//! from the repos of interest — "given this inventory of packages we
//! maintain in Hyperscale, which packages in EPEL does it depend on".
//! The collected set can be written back out as an inventory, so it
//! composes with the rest of the inventory tooling: culling a personal
//! inventory is then a set difference against inventories like these.
//!
//! "Capability" throughout is RPM's term for the strings in
//! `Provides:`/`Requires:` — package names, versioned expressions
//! (`python3dist(zstandard)`), sonames (`libfoo.so.1()(64bit)`), file
//! paths. Requires name capabilities, not packages; resolution finds
//! the package whose Provides (or file list) satisfies one.
//!
//! The walk is breadth-first over *binary* packages, one batched fedrq
//! invocation per wave: a provider's own Requires arrive in the same
//! query that discovered it, so the whole closure costs roughly one
//! fedrq spawn per dependency-graph level. Providers from the base
//! distro terminate a path (base is a given); everything else is
//! walked further, and collected when its repo is one of the repos of
//! interest. Build-time dependencies are out of scope for now — this
//! answers "what must stay available for these packages to keep
//! working", not "…to keep rebuilding".

use std::collections::{BTreeMap, BTreeSet};

use sandogasa_fedrq::PkgInfo;
use serde::Serialize;

/// Source of batched package facts — [`sandogasa_fedrq::Fedrq`] in
/// production, canned fixtures in tests.
pub trait PkgQuery {
    /// Binary subpackages of the given source packages, with their
    /// Requires, source attribution and providing repo.
    fn subpkgs(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, String>;
    /// Providers of the given capabilities — the candidates dnf would
    /// pick — with their own Requires, Provides, source and repo.
    fn providers(&self, deps: &[String]) -> Result<Vec<PkgInfo>, String>;
}

impl PkgQuery for sandogasa_fedrq::Fedrq {
    fn subpkgs(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, String> {
        self.subpkgs_info(srpms).map_err(|e| e.to_string())
    }
    fn providers(&self, deps: &[String]) -> Result<Vec<PkgInfo>, String> {
        self.providers_info(deps).map_err(|e| e.to_string())
    }
}

/// One collected dependency: a source package the inventory
/// transitively requires at runtime, pulled from a repo of interest.
#[derive(Debug, Clone, Serialize)]
pub struct CollectedDep {
    /// Source package name.
    pub source: String,
    /// Repository it came from (e.g. `epel`).
    pub repoid: String,
    /// Binary that first pulled it in — empty when the capability
    /// could not be tied back (a file dependency, say).
    pub required_by: String,
    /// The capability that pulled it in; empty when unattributable.
    pub via: String,
}

/// The walk's result.
#[derive(Debug, Default, Serialize)]
pub struct DepsReport {
    /// Source packages the walk started from.
    pub roots: usize,
    /// Dependency-graph levels walked (= batched resolution queries).
    pub waves: usize,
    /// Distinct binary packages whose Requires were examined.
    pub binaries_walked: usize,
    /// Collected dependencies, sorted by source name.
    pub collected: Vec<CollectedDep>,
    /// Capabilities no provider could be tied to — either genuinely
    /// unresolvable in the stack or beyond the local matcher (file
    /// dependencies without their provider's file list, rich deps).
    pub unmatched: Vec<String>,
    /// Wall-clock seconds the walk took, resolution queries
    /// included.
    pub elapsed_secs: f64,
    /// Non-fatal oddities worth a human glance.
    pub warnings: Vec<String>,
}

/// The dependency's name token: everything up to the first space, so
/// `python3-foo >= 1.2` and the Provides `python3-foo = 1.5-1.el9`
/// meet at `python3-foo`. Solib capabilities carry no spaces and pass
/// through whole.
fn dep_name(dep: &str) -> &str {
    dep.split_whitespace().next().unwrap_or(dep)
}

/// Whether provider `p` satisfies `req`, by exact Provides match,
/// name-token match, or its own package name.
fn provider_matches(p: &PkgInfo, req: &str) -> bool {
    let name = dep_name(req);
    p.name == name
        || p.provides
            .iter()
            .any(|pr| pr == req || dep_name(pr) == name)
}

/// Whether `dep` should be resolved at all. RPM-internal and
/// auto-generated capabilities are noise; symbol-version deps always
/// ride alongside a bare soname dep that resolves to the same
/// provider; rich (boolean) deps are beyond `-P` resolution and are
/// skipped with a warning by the caller.
fn wants_resolution(dep: &str) -> bool {
    !sandogasa_depfilter::is_rpm_internal_dep(dep)
        && !sandogasa_depfilter::is_solib_symbol_dep(dep)
        && !dep.starts_with('(')
}

/// Walk the runtime closure of `roots` (source package names).
///
/// `from` — repo ids whose providers are collected. `base_prefixes` —
/// repo id prefixes treated as the base distro: their providers
/// satisfy a dependency silently and are not walked further. Empty
/// prefixes are ignored — `starts_with("")` holds for every repoid,
/// so a stray `--base-repo=` would otherwise classify *everything*
/// as base and return an empty report. Roots are never collected,
/// whatever repo they resolve from.
pub fn walk(
    query: &impl PkgQuery,
    roots: &[String],
    from: &BTreeSet<String>,
    base_prefixes: &[String],
    verbose: bool,
) -> Result<DepsReport, String> {
    let root_sources: BTreeSet<&str> = roots.iter().map(String::as_str).collect();
    let base_prefixes: Vec<&str> = base_prefixes
        .iter()
        .filter(|b| !b.is_empty())
        .map(String::as_str)
        .collect();
    let mut report = DepsReport {
        roots: roots.len(),
        ..Default::default()
    };
    let mut seen_bins: BTreeSet<String> = BTreeSet::new();
    let mut seen_reqs: BTreeSet<String> = BTreeSet::new();
    let mut rich_warned: BTreeSet<String> = BTreeSet::new();
    let mut collected: BTreeMap<String, CollectedDep> = BTreeMap::new();

    let started = std::time::Instant::now();
    if verbose {
        eprintln!("[deps] expanding {} root source(s)", roots.len());
    }
    let mut wave = query.subpkgs(roots)?;
    if verbose {
        eprintln!(
            "[deps] {} binaries in {:.1}s",
            wave.len(),
            started.elapsed().as_secs_f64()
        );
    }

    loop {
        // Ingest this wave's packages: collect the capabilities their
        // Requires add, remembering who asked first.
        let mut pending: BTreeMap<String, String> = BTreeMap::new();
        for pkg in &wave {
            if !seen_bins.insert(pkg.name.clone()) {
                continue;
            }
            for dep in &pkg.requires {
                if dep.starts_with('(') && rich_warned.insert(dep.clone()) {
                    report
                        .warnings
                        .push(format!("rich dependency skipped: {dep} ({})", pkg.name));
                    continue;
                }
                if !wants_resolution(dep) || seen_reqs.contains(dep) {
                    continue;
                }
                pending
                    .entry(dep.clone())
                    .or_insert_with(|| pkg.name.clone());
            }
        }
        if pending.is_empty() {
            break;
        }
        report.waves += 1;
        let reqs: Vec<String> = pending.keys().cloned().collect();
        seen_reqs.extend(reqs.iter().cloned());
        if verbose {
            eprintln!(
                "[deps] wave {}: resolving {} new capabilit{}",
                report.waves,
                reqs.len(),
                if reqs.len() == 1 { "y" } else { "ies" }
            );
        }
        let wave_started = std::time::Instant::now();
        let providers = query.providers(&reqs)?;
        if verbose {
            eprintln!(
                "[deps] wave {}: {} provider(s) in {:.1}s",
                report.waves,
                providers.len(),
                wave_started.elapsed().as_secs_f64()
            );
        }

        let mut matched: BTreeSet<&str> = BTreeSet::new();
        let mut next: Vec<PkgInfo> = Vec::new();
        for p in providers {
            // Which of this wave's capabilities did this provider
            // satisfy? Attribution is best-effort: the first match
            // names the requirer in the report.
            let mine: Vec<&str> = reqs
                .iter()
                .map(String::as_str)
                .filter(|r| provider_matches(&p, r))
                .collect();
            matched.extend(mine.iter().copied());

            if base_prefixes.iter().any(|b| p.repoid.starts_with(b)) {
                continue; // satisfied by the base distro
            }
            if from.contains(&p.repoid)
                && let Some(src) = &p.source_name
                && !root_sources.contains(src.as_str())
            {
                let (via, required_by) = mine
                    .first()
                    .map(|r| (r.to_string(), pending[*r].clone()))
                    .unwrap_or_default();
                collected.entry(src.clone()).or_insert(CollectedDep {
                    source: src.clone(),
                    repoid: p.repoid.clone(),
                    required_by,
                    via,
                });
            }
            if !seen_bins.contains(&p.name) {
                next.push(p);
            }
        }
        report.unmatched.extend(
            reqs.iter()
                .filter(|r| !matched.contains(r.as_str()))
                .cloned(),
        );
        wave = next;
    }

    report.binaries_walked = seen_bins.len();
    report.collected = collected.into_values().collect();
    report.elapsed_secs = started.elapsed().as_secs_f64();
    Ok(report)
}

/// Turn a report into an inventory of the collected sources, ready
/// for `sandogasa_inventory::save`.
pub fn to_inventory(
    report: &DepsReport,
    name: &str,
    description: &str,
    maintainer: &str,
) -> sandogasa_inventory::Inventory {
    sandogasa_inventory::Inventory {
        inventory: sandogasa_inventory::InventoryMeta {
            name: name.to_string(),
            description: description.to_string(),
            maintainer: maintainer.to_string(),
            labels: Vec::new(),
            workloads: BTreeMap::new(),
            private_fields: Vec::new(),
        },
        package: report
            .collected
            .iter()
            .map(|c| sandogasa_inventory::Package {
                name: c.source.clone(),
                reason: Some(if c.via.is_empty() {
                    format!("runtime dependency ({})", c.repoid)
                } else {
                    format!(
                        "runtime dependency ({}): {} requires {}",
                        c.repoid, c.required_by, c.via
                    )
                }),
                ..Default::default()
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canned query: `PkgInfo` can only be built through serde (the
    /// struct is `#[non_exhaustive]`), which doubles as a check that
    /// fixtures stay in fedrq's actual output shape.
    struct Canned {
        subpkgs: Vec<PkgInfo>,
        providers: Vec<PkgInfo>,
    }

    fn pkg(v: serde_json::Value) -> PkgInfo {
        serde_json::from_value(v).unwrap()
    }

    impl PkgQuery for Canned {
        fn subpkgs(&self, _srpms: &[String]) -> Result<Vec<PkgInfo>, String> {
            Ok(self.subpkgs.clone())
        }
        fn providers(&self, deps: &[String]) -> Result<Vec<PkgInfo>, String> {
            Ok(self
                .providers
                .iter()
                .filter(|p| deps.iter().any(|d| provider_matches(p, d)))
                .cloned()
                .collect())
        }
    }

    fn from_epel() -> BTreeSet<String> {
        ["epel".to_string()].into()
    }
    fn base() -> Vec<String> {
        vec!["fedrq-centos-stream-".to_string()]
    }

    #[test]
    fn collects_transitively_with_attribution() {
        // root's binary requires libfoo -> provided by foo-libs
        // (epel, source foo), whose requires pull bar (epel).
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "root-bin", "source_name": "root",
                "repoid": "centos-hyperscale",
                "requires": ["libfoo.so.1()(64bit)", "rpmlib(X)"],
            }))],
            providers: vec![
                pkg(serde_json::json!({
                    "name": "foo-libs", "source_name": "foo", "repoid": "epel",
                    "provides": ["libfoo.so.1()(64bit)", "foo-libs = 1.0"],
                    "requires": ["bar"],
                })),
                pkg(serde_json::json!({
                    "name": "bar", "source_name": "bar", "repoid": "epel",
                    "provides": ["bar = 2.0"],
                })),
            ],
        };
        let r = walk(&q, &["root".to_string()], &from_epel(), &base(), false).unwrap();
        assert_eq!(r.waves, 2);
        let names: Vec<&str> = r.collected.iter().map(|c| c.source.as_str()).collect();
        assert_eq!(names, ["bar", "foo"]);
        let foo = &r.collected[1];
        assert_eq!(foo.required_by, "root-bin");
        assert_eq!(foo.via, "libfoo.so.1()(64bit)");
        assert!(r.unmatched.is_empty());
    }

    #[test]
    fn base_providers_terminate_and_are_not_collected() {
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "root-bin", "source_name": "root",
                "repoid": "centos-hyperscale", "requires": ["glibc"],
            }))],
            providers: vec![pkg(serde_json::json!({
                "name": "glibc", "source_name": "glibc",
                "repoid": "fedrq-centos-stream-baseos",
                "provides": ["glibc = 2.34"],
                // would explode the walk if followed
                "requires": ["libunreachable.so.1()(64bit)"],
            }))],
        };
        let r = walk(&q, &["root".to_string()], &from_epel(), &base(), false).unwrap();
        assert!(r.collected.is_empty());
        assert_eq!(r.waves, 1);
        assert!(r.unmatched.is_empty());
    }

    #[test]
    fn roots_are_never_collected() {
        // A root's binary requires a sibling root's binary that
        // resolves from epel — still not collected: it's already
        // maintained, that's what being a root means.
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "a-bin", "source_name": "a",
                "repoid": "epel", "requires": ["b-bin"],
            }))],
            providers: vec![pkg(serde_json::json!({
                "name": "b-bin", "source_name": "b", "repoid": "epel",
                "provides": ["b-bin = 1.0"],
            }))],
        };
        let roots = ["a".to_string(), "b".to_string()];
        let r = walk(&q, &roots, &from_epel(), &base(), false).unwrap();
        assert!(r.collected.is_empty());
    }

    #[test]
    fn rich_deps_warn_and_unmatched_reqs_are_reported() {
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "root-bin", "source_name": "root",
                "repoid": "centos-hyperscale",
                "requires": ["(foo if bar)", "/usr/bin/vanisher"],
            }))],
            providers: vec![],
        };
        let r = walk(&q, &["root".to_string()], &from_epel(), &base(), false).unwrap();
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("(foo if bar)"));
        assert_eq!(r.unmatched, ["/usr/bin/vanisher"]);
    }

    #[test]
    fn empty_base_prefix_does_not_swallow_the_world() {
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "root-bin", "source_name": "root",
                "repoid": "centos-hyperscale", "requires": ["libfoo.so.1()(64bit)"],
            }))],
            providers: vec![pkg(serde_json::json!({
                "name": "foo-libs", "source_name": "foo", "repoid": "epel",
                "provides": ["libfoo.so.1()(64bit)"],
            }))],
        };
        // A stray `--base-repo=` must not turn every provider into
        // base: the empty prefix is ignored, foo is still collected.
        let base = vec![String::new(), "fedrq-centos-stream-".to_string()];
        let r = walk(&q, &["root".to_string()], &from_epel(), &base, false).unwrap();
        assert_eq!(r.collected.len(), 1);
        assert_eq!(r.collected[0].source, "foo");
    }

    #[test]
    fn versioned_requires_meet_versioned_provides() {
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "root-bin", "source_name": "root",
                "repoid": "centos-hyperscale",
                "requires": ["python3-foo >= 1.2"],
            }))],
            providers: vec![pkg(serde_json::json!({
                "name": "python3-foo", "source_name": "foo", "repoid": "epel",
                "provides": ["python3-foo = 1.5-1.el9"],
            }))],
        };
        let r = walk(&q, &["root".to_string()], &from_epel(), &base(), false).unwrap();
        assert_eq!(r.collected[0].source, "foo");
        assert_eq!(r.collected[0].via, "python3-foo >= 1.2");
        assert!(r.unmatched.is_empty());
    }

    #[test]
    fn inventory_carries_the_reason_chain() {
        let r = DepsReport {
            collected: vec![CollectedDep {
                source: "foo".into(),
                repoid: "epel".into(),
                required_by: "root-bin".into(),
                via: "libfoo.so.1()(64bit)".into(),
            }],
            ..Default::default()
        };
        let inv = to_inventory(&r, "work-epel9-deps", "test", "me");
        assert_eq!(inv.inventory.name, "work-epel9-deps");
        assert_eq!(inv.package[0].name, "foo");
        assert_eq!(
            inv.package[0].reason.as_deref(),
            Some("runtime dependency (epel): root-bin requires libfoo.so.1()(64bit)")
        );
    }
}
