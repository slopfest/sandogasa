// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dependency closure resolution.
//!
//! Discovers the transitive set of source packages that must be built
//! on a target branch in order to satisfy the BuildRequires of a set
//! of requested packages.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use dashmap::DashMap;
use rayon::prelude::*;
use serde::Serialize;

/// A BuildRequires entry that is missing on the target branch.
#[derive(Debug, Clone, Serialize)]
pub struct MissingDep {
    /// The raw BuildRequires string (e.g. "pkgconfig(libsystemd) >= 250").
    pub dep: String,
    /// The source package that provides this on the source branch.
    pub provided_by: String,
}

/// Resolution result for a single source package.
#[derive(Debug, Serialize)]
pub struct ClosureEntry {
    /// BuildRequires missing on the target branch.
    pub missing_deps: Vec<MissingDep>,
}

/// The full dependency closure.
#[derive(Debug, Serialize)]
pub struct Closure {
    pub source_branch: String,
    pub target_branch: String,
    pub requested: Vec<String>,
    pub closure: BTreeMap<String, ClosureEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl Closure {
    /// Build an edge map suitable for DAG algorithms.
    ///
    /// Each package maps to the set of packages it depends on
    /// (the `provided_by` values from its missing deps).
    pub fn to_edges(&self) -> BTreeMap<String, BTreeSet<String>> {
        self.closure
            .iter()
            .map(|(pkg, entry)| {
                let deps: BTreeSet<String> = entry
                    .missing_deps
                    .iter()
                    .map(|d| d.provided_by.clone())
                    .filter(|p| self.closure.contains_key(p))
                    .collect();
                (pkg.clone(), deps)
            })
            .collect()
    }
}

/// Trait abstracting fedrq operations for testability.
pub trait DepResolver: Send + Sync {
    /// Validate that the source and target configurations are usable.
    /// Called once before resolution begins. Return an error message
    /// if the configuration is invalid (e.g. bad fedrq branch/repo).
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }

    /// Return the BuildRequires of a source package.
    /// Called with the source branch.
    fn buildrequires(&self, srpm: &str) -> Result<Vec<String>, String>;

    /// Resolve a dependency name to source package(s) on the source branch.
    fn resolve_source(&self, dep: &str) -> Result<Vec<String>, String>;

    /// Resolve a dependency name to source package(s) on the target branch.
    fn resolve_target(&self, dep: &str) -> Result<Vec<String>, String>;

    /// Check whether a source package exists on the source branch.
    fn src_exists(&self, srpm: &str) -> Result<bool, String>;

    /// Return the Requires of all subpackages of a source package
    /// (queried on the source branch).
    fn subpkg_requires(&self, srpm: &str) -> Result<Vec<String>, String>;
}

/// Options for controlling the resolution process.
#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    /// Maximum recursion depth (0 = no limit).
    pub max_depth: usize,
    /// Print progress to stderr.
    pub verbose: bool,
    /// Source packages to exclude from the closure entirely.
    /// Their deps are treated as satisfied on the target.
    pub exclude: BTreeSet<String>,
    /// Source packages to exclude from installability expansion.
    pub exclude_install: BTreeSet<String>,
    /// When true, auto-exclude default packages (e.g. glibc)
    /// from installability checks.
    pub auto_exclude: bool,
}

/// Thread-safe dependency resolution cache.
///
/// Shared between `resolve_closure_with_options` and
/// `check_installability` so that repeated queries for the same
/// dependency string are not sent to fedrq twice.
pub struct ResolveCache {
    target: DashMap<String, Option<String>>,
    source: DashMap<String, Option<String>>,
}

impl ResolveCache {
    pub fn new() -> Self {
        Self {
            target: DashMap::new(),
            source: DashMap::new(),
        }
    }
}

/// A subpackage Requires that cannot be satisfied on the target.
#[derive(Debug, Clone, Serialize)]
pub struct UnsatisfiedRequires {
    /// The raw Requires string.
    pub dep: String,
    /// The source package that provides this on the source branch,
    /// or `None` if it cannot be resolved at all.
    pub provided_by: Option<String>,
}

/// Installability check result for a single source package.
#[derive(Debug, Serialize)]
pub struct InstallabilityEntry {
    /// Subpackage Requires that are not satisfiable.
    pub unsatisfied: Vec<UnsatisfiedRequires>,
}

/// Result of the installability check across the closure.
#[derive(Debug, Serialize)]
pub struct InstallabilityReport {
    /// Packages with installability issues (only those with problems).
    pub issues: BTreeMap<String, InstallabilityEntry>,
    /// Source packages that need to be added to the closure.
    pub additional_packages: BTreeSet<String>,
}

/// Real resolver backed by `sandogasa_fedrq::Fedrq`.
pub struct FedrqResolver {
    pub source: sandogasa_fedrq::Fedrq,
    /// Source RPM queries when source uses a Koji repo.
    /// `@koji:` repos only index binary RPMs, so BuildRequires and
    /// subpackage Requires queries need `@koji-src:` instead.
    pub source_src: Option<sandogasa_fedrq::Fedrq>,
    pub target: sandogasa_fedrq::Fedrq,
}

/// Resolve the full transitive closure of missing build dependencies.
#[cfg(test)]
pub fn resolve_closure(
    resolver: &dyn DepResolver,
    packages: &[String],
    source_branch: &str,
    target_branch: &str,
) -> Result<Closure, String> {
    resolve_closure_with_options(
        resolver,
        packages,
        source_branch,
        target_branch,
        &ResolveOptions::default(),
    )
}

/// Resolve the full transitive closure with options.
pub fn resolve_closure_with_options(
    resolver: &dyn DepResolver,
    packages: &[String],
    source_branch: &str,
    target_branch: &str,
    options: &ResolveOptions,
) -> Result<Closure, String> {
    resolve_closure_with_cache(
        resolver,
        packages,
        source_branch,
        target_branch,
        options,
        &ResolveCache::new(),
    )
}

/// Resolve the full transitive closure, reusing an existing cache.
fn resolve_closure_with_cache(
    resolver: &dyn DepResolver,
    packages: &[String],
    source_branch: &str,
    target_branch: &str,
    options: &ResolveOptions,
    cache: &ResolveCache,
) -> Result<Closure, String> {
    resolver.validate()?;

    // Verify requested packages exist on the source before starting
    // the expensive BFS. A missing package would silently produce
    // an empty closure.
    {
        let missing: Vec<&str> = packages
            .par_iter()
            .filter(|pkg| !resolver.src_exists(pkg).unwrap_or(false))
            .map(|s| s.as_str())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "package(s) not found on source: {}",
                missing.join(", ")
            ));
        }
    }

    let mut closure: BTreeMap<String, ClosureEntry> = BTreeMap::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut depth: BTreeMap<String, usize> = BTreeMap::new();
    for p in packages {
        depth.insert(p.clone(), 1);
    }
    let mut warnings: Vec<String> = Vec::new();

    // Level-parallel BFS: process all packages at the same depth
    // concurrently, then collect new packages for the next level.
    let mut current_level: Vec<String> = packages.to_vec();

    while !current_level.is_empty() {
        // Filter to unvisited packages within depth limit.
        let to_process: Vec<(String, usize)> = current_level
            .iter()
            .filter(|pkg| !visited.contains(*pkg))
            .filter_map(|pkg| {
                let d = depth[pkg];
                if options.max_depth > 0 && d > options.max_depth {
                    None
                } else {
                    Some((pkg.clone(), d))
                }
            })
            .collect();

        if to_process.is_empty() {
            break;
        }

        if options.verbose {
            let names: Vec<&str> = to_process.iter().map(|(p, _)| p.as_str()).collect();
            eprintln!(
                "[level] processing {} package(s) ({} resolved so far): {}",
                to_process.len(),
                closure.len(),
                names.join(", "),
            );
        }

        // Resolve all packages at this level in parallel.
        // Each returns: (pkg, entry, new_packages, pkg_warnings)
        let results: Vec<_> = to_process
            .par_iter()
            .map(|(pkg, pkg_depth)| {
                let mut log = Vec::new();
                if options.verbose {
                    log.push(format!("[depth {pkg_depth}] resolving {pkg}",));
                }

                let build_reqs = match resolver.buildrequires(pkg) {
                    Ok(reqs) => reqs,
                    Err(e) => {
                        let warn = format!("{pkg}: failed to query BuildRequires: {e}");
                        return (
                            pkg.clone(),
                            ClosureEntry {
                                missing_deps: vec![],
                            },
                            vec![],
                            vec![warn],
                            log,
                        );
                    }
                };

                let mut missing_deps: Vec<MissingDep> = Vec::new();
                let mut seen_providers: BTreeSet<String> = BTreeSet::new();
                let mut new_packages: Vec<String> = Vec::new();

                for raw_dep in &build_reqs {
                    let dep_str = raw_dep.trim();

                    if dep_str.starts_with("rpmlib(") || dep_str.starts_with("auto(") {
                        continue;
                    }

                    let target_resolved = cache
                        .target
                        .entry(dep_str.to_string())
                        .or_insert_with(|| {
                            resolver
                                .resolve_target(dep_str)
                                .ok()
                                .and_then(|v| v.into_iter().find(|s| s != "(none)"))
                        })
                        .clone();

                    if target_resolved.is_some() {
                        continue;
                    }

                    let source_resolved = cache
                        .source
                        .entry(dep_str.to_string())
                        .or_insert_with(|| {
                            resolver
                                .resolve_source(dep_str)
                                .ok()
                                .and_then(|v| v.into_iter().find(|s| s != "(none)"))
                        })
                        .clone();

                    let Some(provider) = source_resolved else {
                        continue;
                    };

                    if provider == *pkg || options.exclude.contains(&provider) {
                        continue;
                    }

                    if seen_providers.insert(provider.clone()) {
                        missing_deps.push(MissingDep {
                            dep: raw_dep.clone(),
                            provided_by: provider.clone(),
                        });
                        new_packages.push(provider);
                    }
                }

                (
                    pkg.clone(),
                    ClosureEntry { missing_deps },
                    new_packages,
                    vec![],
                    log,
                )
            })
            .collect();

        // Collect results sequentially: update closure, queue next level.
        let mut next_level: Vec<String> = Vec::new();
        for (pkg, entry, new_pkgs, warns, log) in results {
            if options.verbose {
                for line in log {
                    eprintln!("{line}");
                }
            }
            let pkg_depth = depth[&pkg];
            visited.insert(pkg.clone());
            warnings.extend(warns);
            for new_pkg in new_pkgs {
                if !visited.contains(&new_pkg) {
                    depth.entry(new_pkg.clone()).or_insert(pkg_depth + 1);
                    next_level.push(new_pkg);
                }
            }
            closure.insert(pkg, entry);
        }
        next_level.sort();
        next_level.dedup();
        current_level = next_level;
    }

    Ok(Closure {
        source_branch: source_branch.to_string(),
        target_branch: target_branch.to_string(),
        requested: packages.to_vec(),
        closure,
        warnings,
    })
}

/// Check that subpackages of all closure packages are installable.
///
/// For each package in the closure, queries its subpackage Requires
/// from the source branch and checks whether each is satisfiable by:
/// 1. The target repository (already available), or
/// 2. A package in the closure (will be built).
///
/// Returns issues found and any additional packages that would need
/// to be added to the closure to fix them.
#[cfg(test)]
pub fn check_installability(
    resolver: &dyn DepResolver,
    closure: &Closure,
    options: &ResolveOptions,
    skip: &BTreeSet<String>,
) -> InstallabilityReport {
    check_installability_with_cache(resolver, closure, options, skip, &ResolveCache::new())
}

/// Check installability, reusing an existing resolution cache.
fn check_installability_with_cache(
    resolver: &dyn DepResolver,
    closure: &Closure,
    options: &ResolveOptions,
    skip: &BTreeSet<String>,
    cache: &ResolveCache,
) -> InstallabilityReport {
    let closure_pkgs: BTreeSet<&String> = closure.closure.keys().collect();

    let pkgs_to_check: Vec<&String> = closure
        .closure
        .keys()
        .filter(|pkg| !skip.contains(*pkg))
        .collect();

    if options.verbose {
        let names: Vec<&str> = pkgs_to_check.iter().map(|p| p.as_str()).collect();
        eprintln!(
            "[installability] checking {} package(s): {}",
            pkgs_to_check.len(),
            names.join(", "),
        );
    }

    // Collect warnings from parallel subpkg_requires failures.
    let warn_collector: Mutex<Vec<String>> = Mutex::new(Vec::new());

    // Process all packages in parallel.
    let results: Vec<_> = pkgs_to_check
        .par_iter()
        .filter_map(|pkg| {
            let requires = match resolver.subpkg_requires(pkg) {
                Ok(r) => r,
                Err(e) => {
                    warn_collector.lock().unwrap().push(format!(
                        "warning: {pkg}: failed to query subpackage \
                         Requires: {e}"
                    ));
                    return None;
                }
            };

            let mut unsatisfied: Vec<UnsatisfiedRequires> = Vec::new();
            let mut seen_providers: BTreeSet<String> = BTreeSet::new();
            let mut additional: Vec<String> = Vec::new();

            for raw_dep in &requires {
                let dep_str = raw_dep.trim();

                if sandogasa_depfilter::is_rpm_internal_dep(dep_str) {
                    continue;
                }
                if options.auto_exclude && sandogasa_depfilter::is_solib_symbol_dep(dep_str) {
                    continue;
                }

                let target_resolved = cache
                    .target
                    .entry(dep_str.to_string())
                    .or_insert_with(|| {
                        resolver
                            .resolve_target(dep_str)
                            .ok()
                            .and_then(|v| v.into_iter().find(|s| s != "(none)"))
                    })
                    .clone();

                if target_resolved.is_some() {
                    continue;
                }

                let source_resolved = cache
                    .source
                    .entry(dep_str.to_string())
                    .or_insert_with(|| {
                        resolver
                            .resolve_source(dep_str)
                            .ok()
                            .and_then(|v| v.into_iter().find(|s| s != "(none)"))
                    })
                    .clone();

                match &source_resolved {
                    Some(provider) if provider == *pkg => {
                        continue;
                    }
                    Some(provider)
                        if closure_pkgs.contains(provider)
                            || options.exclude.contains(provider)
                            || options.exclude_install.contains(provider) =>
                    {
                        continue;
                    }
                    Some(provider) => {
                        if seen_providers.insert(provider.clone()) {
                            unsatisfied.push(UnsatisfiedRequires {
                                dep: raw_dep.clone(),
                                provided_by: Some(provider.clone()),
                            });
                            additional.push(provider.clone());
                        }
                    }
                    None => {
                        unsatisfied.push(UnsatisfiedRequires {
                            dep: raw_dep.clone(),
                            provided_by: None,
                        });
                    }
                }
            }

            if unsatisfied.is_empty() {
                None
            } else {
                Some((pkg.to_string(), unsatisfied, additional))
            }
        })
        .collect();

    // Print warnings collected from parallel section.
    for warn in warn_collector.into_inner().unwrap() {
        eprintln!("{warn}");
    }

    // Merge results.
    let mut issues: BTreeMap<String, InstallabilityEntry> = BTreeMap::new();
    let mut additional_packages: BTreeSet<String> = BTreeSet::new();
    for (pkg, unsatisfied, additional) in results {
        issues.insert(pkg, InstallabilityEntry { unsatisfied });
        additional_packages.extend(additional);
    }

    InstallabilityReport {
        issues,
        additional_packages,
    }
}

/// Resolve the dependency closure and iteratively expand it until
/// all subpackage Requires are satisfiable.
///
/// Runs `resolve_closure_with_options`, then `check_installability`.
/// Any additional packages discovered by the installability check
/// are fed back into a new resolution round. Repeats until no new
/// packages are needed (fixed point).
pub fn resolve_with_installability(
    resolver: &dyn DepResolver,
    packages: &[String],
    source_branch: &str,
    target_branch: &str,
    options: &ResolveOptions,
) -> Result<(Closure, InstallabilityReport), String> {
    let mut all_packages: BTreeSet<String> = packages.iter().cloned().collect();
    let requested: Vec<String> = packages.to_vec();
    // Packages whose installability already passed — skip on future iterations.
    let mut passed: BTreeSet<String> = BTreeSet::new();
    // Shared cache across all resolution and installability iterations.
    let cache = ResolveCache::new();

    loop {
        let pkg_list: Vec<String> = all_packages.iter().cloned().collect();

        if options.verbose {
            eprintln!(
                "[installability] resolving with {} package(s): {}",
                pkg_list.len(),
                pkg_list.join(", "),
            );
        }

        let mut closure = resolve_closure_with_cache(
            resolver,
            &pkg_list,
            source_branch,
            target_branch,
            options,
            &cache,
        )?;

        let report = check_installability_with_cache(resolver, &closure, options, &passed, &cache);

        // Record packages that passed this round.
        for pkg in closure.closure.keys() {
            if !passed.contains(pkg) && !report.issues.contains_key(pkg) {
                passed.insert(pkg.clone());
            }
        }

        if report.additional_packages.is_empty() {
            // Fixed point reached. Restore original requested list.
            closure.requested = requested;
            return Ok((closure, report));
        }

        let before = all_packages.len();
        all_packages.extend(report.additional_packages.iter().cloned());

        if all_packages.len() == before {
            // No new packages were actually added (all were already
            // in the set). This shouldn't happen given the check
            // above, but guard against infinite loops.
            closure.requested = requested;
            return Ok((closure, report));
        }

        if options.verbose {
            let new_pkgs: Vec<&String> = report
                .additional_packages
                .iter()
                .filter(|p| !closure.closure.contains_key(*p))
                .collect();
            eprintln!(
                "[installability] adding {} package(s): {}",
                new_pkgs.len(),
                new_pkgs
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
}

impl FedrqResolver {
    /// The fedrq instance to use for source RPM queries
    /// (BuildRequires, subpackage Requires).
    fn source_srpm(&self) -> &sandogasa_fedrq::Fedrq {
        self.source_src.as_ref().unwrap_or(&self.source)
    }
}

impl DepResolver for FedrqResolver {
    fn validate(&self) -> Result<(), String> {
        // Koji repos only index binary RPMs. If source_src is set,
        // we have a @koji-src: companion for SRPM queries. If not
        // and the source is a bare @koji: repo, fail early.
        if self.source_src.is_none()
            && let Some(ref repo) = self.source.repo
            && repo.starts_with("@koji:")
        {
            return Err(format!(
                "cannot use a Koji repo ({repo}) as the source: \
                 Koji repos do not index source RPMs, so \
                 BuildRequires cannot be queried. Use a branch \
                 (e.g. --source rawhide) instead"
            ));
        }

        // Probe all configured repos in parallel to catch bad configs
        // (e.g. nonexistent Koji repos) early and warm up the cache.
        let src_label = self
            .source
            .repo
            .as_deref()
            .or(self.source.branch.as_deref())
            .unwrap_or("source");
        let tgt_label = self
            .target
            .repo
            .as_deref()
            .or(self.target.branch.as_deref())
            .unwrap_or("target");

        let probe_source = || {
            self.source
                .resolve_to_source("bash")
                .map_err(|e| format!("source repo {src_label}: {e}"))
        };
        let probe_source_src = || {
            if let Some(ref src) = self.source_src {
                let label = src.repo.as_deref().unwrap_or("source-src");
                src.resolve_to_source("bash")
                    .map_err(|e| format!("source repo {label}: {e}"))
            } else {
                Ok(vec![])
            }
        };
        let probe_target = || {
            self.target
                .resolve_to_source("bash")
                .map_err(|e| format!("target repo {tgt_label}: {e}"))
        };

        let ((src, src_src), tgt) =
            rayon::join(|| rayon::join(probe_source, probe_source_src), probe_target);
        src?;
        src_src?;
        tgt?;
        Ok(())
    }

    fn buildrequires(&self, srpm: &str) -> Result<Vec<String>, String> {
        self.source_srpm()
            .src_buildrequires(srpm)
            .map_err(|e| e.to_string())
    }

    fn resolve_source(&self, dep: &str) -> Result<Vec<String>, String> {
        self.source
            .resolve_to_source(dep)
            .map_err(|e| e.to_string())
    }

    fn resolve_target(&self, dep: &str) -> Result<Vec<String>, String> {
        self.target
            .resolve_to_source(dep)
            .map_err(|e| e.to_string())
    }

    fn src_exists(&self, srpm: &str) -> Result<bool, String> {
        self.source_srpm()
            .src_exists(srpm)
            .map_err(|e| e.to_string())
    }

    fn subpkg_requires(&self, srpm: &str) -> Result<Vec<String>, String> {
        self.source_srpm()
            .subpkgs_requires(srpm)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockResolver {
        /// srpm -> BuildRequires list (source branch)
        buildrequires: BTreeMap<String, Vec<String>>,
        /// dep -> source package on source branch
        source_resolve: BTreeMap<String, String>,
        /// dep -> source package on target branch
        target_resolve: BTreeMap<String, String>,
        /// srpm -> subpackage Requires (source branch)
        subpkg_requires: BTreeMap<String, Vec<String>>,
    }

    impl MockResolver {
        fn new() -> Self {
            Self {
                buildrequires: BTreeMap::new(),
                source_resolve: BTreeMap::new(),
                target_resolve: BTreeMap::new(),
                subpkg_requires: BTreeMap::new(),
            }
        }

        fn add_buildrequires(&mut self, srpm: &str, reqs: &[&str]) {
            self.buildrequires.insert(
                srpm.to_string(),
                reqs.iter().map(|s| s.to_string()).collect(),
            );
        }

        fn add_source_resolve(&mut self, dep: &str, source: &str) {
            self.source_resolve
                .insert(dep.to_string(), source.to_string());
        }

        fn add_target_resolve(&mut self, dep: &str, source: &str) {
            self.target_resolve
                .insert(dep.to_string(), source.to_string());
        }

        fn add_subpkg_requires(&mut self, srpm: &str, reqs: &[&str]) {
            self.subpkg_requires.insert(
                srpm.to_string(),
                reqs.iter().map(|s| s.to_string()).collect(),
            );
        }
    }

    impl DepResolver for MockResolver {
        fn buildrequires(&self, srpm: &str) -> Result<Vec<String>, String> {
            self.buildrequires
                .get(srpm)
                .cloned()
                .ok_or_else(|| format!("package {srpm} not found"))
        }

        fn resolve_source(&self, dep: &str) -> Result<Vec<String>, String> {
            Ok(self
                .source_resolve
                .get(dep)
                .map(|s| vec![s.clone()])
                .unwrap_or_default())
        }

        fn resolve_target(&self, dep: &str) -> Result<Vec<String>, String> {
            Ok(self
                .target_resolve
                .get(dep)
                .map(|s| vec![s.clone()])
                .unwrap_or_default())
        }

        fn src_exists(&self, srpm: &str) -> Result<bool, String> {
            Ok(self.buildrequires.contains_key(srpm))
        }

        fn subpkg_requires(&self, srpm: &str) -> Result<Vec<String>, String> {
            Ok(self.subpkg_requires.get(srpm).cloned().unwrap_or_default())
        }
    }

    #[test]
    fn test_empty_packages() {
        let resolver = MockResolver::new();
        let closure = resolve_closure(&resolver, &[], "rawhide", "epel10").unwrap();
        assert!(closure.closure.is_empty());
        assert!(closure.warnings.is_empty());
    }

    #[test]
    fn test_all_deps_satisfied() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &["gcc", "glibc-devel"]);
        resolver.add_target_resolve("gcc", "gcc");
        resolver.add_target_resolve("glibc-devel", "glibc");

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        assert_eq!(closure.closure.len(), 1);
        assert!(closure.closure["mypkg"].missing_deps.is_empty());
    }

    #[test]
    fn test_missing_dep_discovered() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &["libfoo-devel"]);
        // libfoo-devel NOT on target
        resolver.add_source_resolve("libfoo-devel", "libfoo");
        // libfoo has no further missing deps
        resolver.add_buildrequires("libfoo", &["gcc"]);
        resolver.add_target_resolve("gcc", "gcc");

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        assert_eq!(closure.closure.len(), 2);
        assert_eq!(closure.closure["mypkg"].missing_deps.len(), 1);
        assert_eq!(
            closure.closure["mypkg"].missing_deps[0].provided_by,
            "libfoo"
        );
        assert!(closure.closure["libfoo"].missing_deps.is_empty());
    }

    #[test]
    fn test_transitive_closure() {
        let mut resolver = MockResolver::new();
        // a needs b (missing), b needs c (missing), c has no missing deps
        resolver.add_buildrequires("a", &["libb"]);
        resolver.add_source_resolve("libb", "b");
        resolver.add_buildrequires("b", &["libc-custom"]);
        resolver.add_source_resolve("libc-custom", "c");
        resolver.add_buildrequires("c", &["gcc"]);
        resolver.add_target_resolve("gcc", "gcc");

        let closure = resolve_closure(&resolver, &["a".to_string()], "rawhide", "epel10").unwrap();
        assert_eq!(closure.closure.len(), 3);
        assert_eq!(closure.closure["a"].missing_deps[0].provided_by, "b");
        assert_eq!(closure.closure["b"].missing_deps[0].provided_by, "c");
        assert!(closure.closure["c"].missing_deps.is_empty());
    }

    #[test]
    fn test_rpmlib_deps_skipped() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires(
            "mypkg",
            &["rpmlib(CompressedFileNames)", "auto(gcc)", "libfoo"],
        );
        resolver.add_target_resolve("libfoo", "libfoo");

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        assert!(closure.closure["mypkg"].missing_deps.is_empty());
    }

    #[test]
    fn test_versioned_dep_satisfied() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &["meson >= 0.60"]);
        // Target has meson that satisfies >= 0.60
        resolver.add_target_resolve("meson >= 0.60", "meson");

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        assert!(closure.closure["mypkg"].missing_deps.is_empty());
    }

    #[test]
    fn test_versioned_dep_not_satisfied() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &["meson >= 0.60"]);
        // Target does NOT satisfy >= 0.60
        // Source does provide it
        resolver.add_source_resolve("meson >= 0.60", "meson");
        resolver.add_buildrequires("meson", &[]);

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        assert_eq!(closure.closure["mypkg"].missing_deps.len(), 1);
        assert_eq!(
            closure.closure["mypkg"].missing_deps[0].provided_by,
            "meson"
        );
    }

    #[test]
    fn test_unresolvable_dep_warning() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &["nonexistent-thing"]);
        // Not resolvable on either branch

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        assert!(closure.closure["mypkg"].missing_deps.is_empty());
    }

    #[test]
    fn test_package_not_found_errors() {
        let resolver = MockResolver::new();
        let result = resolve_closure(&resolver, &["nonexistent".to_string()], "rawhide", "epel10");
        let err = result.unwrap_err();
        assert!(err.contains("nonexistent"));
        assert!(err.contains("not found on source"));
    }

    #[test]
    fn test_self_dep_filtered() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &["mypkg-devel"]);
        resolver.add_source_resolve("mypkg-devel", "mypkg");

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        assert!(closure.closure["mypkg"].missing_deps.is_empty());
    }

    #[test]
    fn test_dedup_providers() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &["libfoo", "libfoo-devel"]);
        resolver.add_source_resolve("libfoo", "foo");
        resolver.add_source_resolve("libfoo-devel", "foo");
        resolver.add_buildrequires("foo", &[]);

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        assert_eq!(closure.closure["mypkg"].missing_deps.len(), 1);
    }

    #[test]
    fn test_to_edges() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &["libb"]);
        resolver.add_source_resolve("libb", "b");
        resolver.add_buildrequires("b", &[]);

        let closure = resolve_closure(&resolver, &["a".to_string()], "rawhide", "epel10").unwrap();
        let edges = closure.to_edges();
        assert_eq!(edges["a"], BTreeSet::from(["b".to_string()]));
        assert!(edges["b"].is_empty());
    }

    // --- Installability check tests ---

    #[test]
    fn test_installability_all_satisfied() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &[]);
        resolver.add_subpkg_requires("mypkg", &["glibc", "bash"]);
        resolver.add_target_resolve("glibc", "glibc");
        resolver.add_target_resolve("bash", "bash");

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        let report = check_installability(
            &resolver,
            &closure,
            &ResolveOptions::default(),
            &BTreeSet::new(),
        );
        assert!(report.issues.is_empty());
        assert!(report.additional_packages.is_empty());
    }

    #[test]
    fn test_installability_missing_requires() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &[]);
        // Subpackage requires libwidget, not on target
        resolver.add_subpkg_requires("mypkg", &["libwidget"]);
        resolver.add_source_resolve("libwidget", "widget");

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        let report = check_installability(
            &resolver,
            &closure,
            &ResolveOptions::default(),
            &BTreeSet::new(),
        );
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues["mypkg"].unsatisfied.len(), 1);
        assert_eq!(report.issues["mypkg"].unsatisfied[0].dep, "libwidget");
        assert_eq!(
            report.issues["mypkg"].unsatisfied[0].provided_by,
            Some("widget".to_string())
        );
        assert!(report.additional_packages.contains("widget"));
    }

    #[test]
    fn test_installability_satisfied_by_closure() {
        let mut resolver = MockResolver::new();
        // a and b are both in closure; a's subpackage requires something from b
        resolver.add_buildrequires("a", &["libb-devel"]);
        resolver.add_source_resolve("libb-devel", "b");
        resolver.add_buildrequires("b", &[]);
        resolver.add_subpkg_requires("a", &["libb"]);
        resolver.add_source_resolve("libb", "b");
        resolver.add_subpkg_requires("b", &[]);

        let closure = resolve_closure(&resolver, &["a".to_string()], "rawhide", "epel10").unwrap();
        // b is in the closure (pulled in via BuildRequires)
        assert!(closure.closure.contains_key("b"));
        let report = check_installability(
            &resolver,
            &closure,
            &ResolveOptions::default(),
            &BTreeSet::new(),
        );
        assert!(report.issues.is_empty());
    }

    #[test]
    fn test_installability_self_provides_ok() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &[]);
        // Subpackage requires something provided by another subpackage
        // of the same source package
        resolver.add_subpkg_requires("mypkg", &["mypkg-libs"]);
        resolver.add_source_resolve("mypkg-libs", "mypkg");

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        let report = check_installability(
            &resolver,
            &closure,
            &ResolveOptions::default(),
            &BTreeSet::new(),
        );
        assert!(report.issues.is_empty());
    }

    #[test]
    fn test_installability_rpmlib_skipped() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &[]);
        resolver.add_subpkg_requires("mypkg", &["rpmlib(CompressedFileNames)", "config(mypkg)"]);

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        let report = check_installability(
            &resolver,
            &closure,
            &ResolveOptions::default(),
            &BTreeSet::new(),
        );
        assert!(report.issues.is_empty());
    }

    #[test]
    fn test_installability_unresolvable_dep() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &[]);
        // Subpackage requires something that can't be resolved anywhere
        resolver.add_subpkg_requires("mypkg", &["nonexistent-lib"]);

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        let report = check_installability(
            &resolver,
            &closure,
            &ResolveOptions::default(),
            &BTreeSet::new(),
        );
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues["mypkg"].unsatisfied[0].provided_by, None);
    }

    #[test]
    fn test_installability_dedup_providers() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &[]);
        // Two different requires both provided by the same source package
        resolver.add_subpkg_requires("mypkg", &["libwidget", "libwidget-data"]);
        resolver.add_source_resolve("libwidget", "widget");
        resolver.add_source_resolve("libwidget-data", "widget");

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        let report = check_installability(
            &resolver,
            &closure,
            &ResolveOptions::default(),
            &BTreeSet::new(),
        );
        assert_eq!(report.issues["mypkg"].unsatisfied.len(), 1);
        assert_eq!(report.additional_packages.len(), 1);
    }

    #[test]
    fn test_installability_no_subpkg_requires() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &[]);
        // No subpkg_requires configured -> defaults to empty vec

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        let report = check_installability(
            &resolver,
            &closure,
            &ResolveOptions::default(),
            &BTreeSet::new(),
        );
        assert!(report.issues.is_empty());
    }

    // --- Iterative installability expansion tests ---

    #[test]
    fn test_iterative_no_install_issues() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["glibc"]);
        resolver.add_target_resolve("glibc", "glibc");

        let (closure, report) = resolve_with_installability(
            &resolver,
            &["a".to_string()],
            "rawhide",
            "epel10",
            &ResolveOptions::default(),
        )
        .unwrap();
        assert_eq!(closure.closure.len(), 1);
        assert!(report.issues.is_empty());
        assert_eq!(closure.requested, vec!["a".to_string()]);
    }

    #[test]
    fn test_iterative_expands_closure() {
        // a builds fine but its subpackage requires libwidget,
        // which is provided by "widget" — not in initial closure.
        // widget itself builds fine with no install issues.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["libwidget"]);
        resolver.add_source_resolve("libwidget", "widget");
        resolver.add_buildrequires("widget", &[]);
        resolver.add_subpkg_requires("widget", &[]);

        let (closure, report) = resolve_with_installability(
            &resolver,
            &["a".to_string()],
            "rawhide",
            "epel10",
            &ResolveOptions::default(),
        )
        .unwrap();
        // widget should now be in the closure
        assert!(closure.closure.contains_key("widget"));
        assert_eq!(closure.closure.len(), 2);
        assert!(report.issues.is_empty());
        // Original requested list preserved
        assert_eq!(closure.requested, vec!["a".to_string()]);
    }

    #[test]
    fn test_iterative_transitive_expansion() {
        // a's subpackage needs libwidget (from widget).
        // widget's subpackage needs libgadget (from gadget).
        // gadget has no install issues.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["libwidget"]);
        resolver.add_source_resolve("libwidget", "widget");

        resolver.add_buildrequires("widget", &[]);
        resolver.add_subpkg_requires("widget", &["libgadget"]);
        resolver.add_source_resolve("libgadget", "gadget");

        resolver.add_buildrequires("gadget", &[]);
        resolver.add_subpkg_requires("gadget", &[]);

        let (closure, report) = resolve_with_installability(
            &resolver,
            &["a".to_string()],
            "rawhide",
            "epel10",
            &ResolveOptions::default(),
        )
        .unwrap();
        assert_eq!(closure.closure.len(), 3);
        assert!(closure.closure.contains_key("widget"));
        assert!(closure.closure.contains_key("gadget"));
        assert!(report.issues.is_empty());
    }

    #[test]
    fn test_iterative_with_unresolvable() {
        // a's subpackage needs libwidget (expandable) and
        // libmystery (unresolvable on source).
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["libwidget", "libmystery"]);
        resolver.add_source_resolve("libwidget", "widget");

        resolver.add_buildrequires("widget", &[]);
        resolver.add_subpkg_requires("widget", &[]);

        let (closure, report) = resolve_with_installability(
            &resolver,
            &["a".to_string()],
            "rawhide",
            "epel10",
            &ResolveOptions::default(),
        )
        .unwrap();
        // widget got added, but libmystery remains an issue
        assert!(closure.closure.contains_key("widget"));
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues["a"].unsatisfied.len(), 1);
        assert_eq!(report.issues["a"].unsatisfied[0].dep, "libmystery");
    }

    #[test]
    fn test_iterative_buildrequires_of_added_pkg() {
        // a's subpackage needs libwidget (from widget).
        // widget has a BuildRequires on libhelper-devel (from helper),
        // which is missing on target.
        // So the expansion should pull in both widget AND helper.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["libwidget"]);
        resolver.add_source_resolve("libwidget", "widget");

        resolver.add_buildrequires("widget", &["libhelper-devel"]);
        resolver.add_source_resolve("libhelper-devel", "helper");
        resolver.add_subpkg_requires("widget", &[]);

        resolver.add_buildrequires("helper", &[]);
        resolver.add_subpkg_requires("helper", &[]);

        let (closure, report) = resolve_with_installability(
            &resolver,
            &["a".to_string()],
            "rawhide",
            "epel10",
            &ResolveOptions::default(),
        )
        .unwrap();
        assert_eq!(closure.closure.len(), 3);
        assert!(closure.closure.contains_key("widget"));
        assert!(closure.closure.contains_key("helper"));
        assert!(report.issues.is_empty());
    }

    // --- Exclude-install tests ---

    #[test]
    fn test_exclude_install_skips_provider() {
        // a's subpackage requires libfoo, provided by "foo" on source.
        // With foo excluded, it should not be pulled into the closure
        // and a should be considered installable.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["libfoo"]);
        resolver.add_source_resolve("libfoo", "foo");

        let options = ResolveOptions {
            exclude_install: BTreeSet::from(["foo".to_string()]),
            ..Default::default()
        };
        let closure = resolve_closure(&resolver, &["a".to_string()], "rawhide", "epel10").unwrap();
        let report = check_installability(&resolver, &closure, &options, &BTreeSet::new());
        assert!(report.issues.is_empty());
        assert!(report.additional_packages.is_empty());
    }

    #[test]
    fn test_exclude_install_iterative() {
        // Same as test_iterative_expands_closure but with widget excluded.
        // Widget should NOT be pulled into the closure.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["libwidget"]);
        resolver.add_source_resolve("libwidget", "widget");

        let options = ResolveOptions {
            exclude_install: BTreeSet::from(["widget".to_string()]),
            ..Default::default()
        };
        let (closure, report) = resolve_with_installability(
            &resolver,
            &["a".to_string()],
            "rawhide",
            "epel10",
            &options,
        )
        .unwrap();
        assert_eq!(closure.closure.len(), 1);
        assert!(!closure.closure.contains_key("widget"));
        assert!(report.issues.is_empty());
    }

    #[test]
    fn test_exclude_install_partial() {
        // a needs libfoo (from foo, excluded) and libbar (from bar, not excluded).
        // Only bar should be pulled in; libfoo should be treated as satisfied.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["libfoo", "libbar"]);
        resolver.add_source_resolve("libfoo", "foo");
        resolver.add_source_resolve("libbar", "bar");
        resolver.add_buildrequires("bar", &[]);
        resolver.add_subpkg_requires("bar", &[]);

        let options = ResolveOptions {
            exclude_install: BTreeSet::from(["foo".to_string()]),
            ..Default::default()
        };
        let (closure, report) = resolve_with_installability(
            &resolver,
            &["a".to_string()],
            "rawhide",
            "epel10",
            &options,
        )
        .unwrap();
        assert!(closure.closure.contains_key("bar"));
        assert!(!closure.closure.contains_key("foo"));
        assert!(report.issues.is_empty());
    }

    // --- Auto-exclude and solib filtering tests ---

    #[test]
    fn test_solib_symbol_deps_skipped_with_auto_exclude() {
        // a's subpackage requires a solib symbol version dep.
        // With auto_exclude, these are skipped (auto-generated at build time).
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["libc.so.6(GLIBC_2.38)(64bit)"]);

        let options = ResolveOptions {
            auto_exclude: true,
            ..Default::default()
        };
        let closure = resolve_closure(&resolver, &["a".to_string()], "rawhide", "epel10").unwrap();
        let report = check_installability(&resolver, &closure, &options, &BTreeSet::new());
        assert!(report.issues.is_empty());
        assert!(report.additional_packages.is_empty());
    }

    #[test]
    fn test_solib_symbol_deps_not_skipped_without_auto_exclude() {
        // Without auto_exclude, solib symbol deps are checked normally.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["libc.so.6(GLIBC_2.38)(64bit)"]);
        resolver.add_source_resolve("libc.so.6(GLIBC_2.38)(64bit)", "glibc");

        let options = ResolveOptions::default(); // auto_exclude: false
        let closure = resolve_closure(&resolver, &["a".to_string()], "rawhide", "epel10").unwrap();
        let report = check_installability(&resolver, &closure, &options, &BTreeSet::new());
        assert!(report.issues.contains_key("a"));
        assert!(report.additional_packages.contains("glibc"));
    }

    #[test]
    fn test_soname_deps_not_skipped_with_auto_exclude() {
        // Soname deps (empty first parens) are real ABI deps and
        // must NOT be skipped even with auto_exclude.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["libbpf.so.1()(64bit)"]);
        resolver.add_source_resolve("libbpf.so.1()(64bit)", "libbpf");
        resolver.add_buildrequires("libbpf", &[]);
        resolver.add_subpkg_requires("libbpf", &[]);

        let options = ResolveOptions {
            auto_exclude: true,
            ..Default::default()
        };
        let (closure, _report) = resolve_with_installability(
            &resolver,
            &["a".to_string()],
            "rawhide",
            "epel10",
            &options,
        )
        .unwrap();
        // libbpf should be pulled into the closure (soname dep is real).
        assert!(closure.closure.contains_key("libbpf"));
    }

    #[test]
    fn test_auto_exclude_iterative_skips_symbol_deps() {
        // With auto_exclude, solib symbol deps should not pull
        // packages into the closure during iterative resolution.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_subpkg_requires("a", &["libc.so.6(GLIBC_2.38)(64bit)"]);
        resolver.add_source_resolve("libc.so.6(GLIBC_2.38)(64bit)", "glibc");

        let options = ResolveOptions {
            auto_exclude: true,
            ..Default::default()
        };
        let (closure, report) = resolve_with_installability(
            &resolver,
            &["a".to_string()],
            "rawhide",
            "epel10",
            &options,
        )
        .unwrap();
        assert_eq!(closure.closure.len(), 1);
        assert!(!closure.closure.contains_key("glibc"));
        assert!(report.issues.is_empty());
    }

    // --- Skip (already-passed) tests ---

    #[test]
    fn test_skip_already_passed_packages() {
        // Directly test that the skip set prevents re-checking.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("a", &[]);
        resolver.add_buildrequires("b", &[]);
        resolver.add_subpkg_requires("a", &["glibc"]);
        resolver.add_subpkg_requires("b", &["libwidget"]);
        resolver.add_target_resolve("glibc", "glibc");
        resolver.add_source_resolve("libwidget", "widget");

        let closure = resolve_closure(
            &resolver,
            &["a".to_string(), "b".to_string()],
            "rawhide",
            "epel10",
        )
        .unwrap();

        // Without skip: b has an issue.
        let report = check_installability(
            &resolver,
            &closure,
            &ResolveOptions::default(),
            &BTreeSet::new(),
        );
        assert!(report.issues.contains_key("b"));
        assert!(report.additional_packages.contains("widget"));

        // With a in skip set: same result (a was fine anyway),
        // but proves skipping works without error.
        let skip = BTreeSet::from(["a".to_string()]);
        let report = check_installability(&resolver, &closure, &ResolveOptions::default(), &skip);
        assert!(report.issues.contains_key("b"));
        assert!(!report.issues.contains_key("a"));
    }
}
