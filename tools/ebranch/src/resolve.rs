// SPDX-License-Identifier: MPL-2.0

//! Dependency closure resolution.
//!
//! Discovers the transitive set of source packages that must be built
//! on a target branch in order to satisfy the BuildRequires of a set
//! of requested packages.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
pub trait DepResolver {
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
}

/// Options for controlling the resolution process.
#[derive(Debug, Clone, Default)]
pub struct ResolveOptions {
    /// Maximum recursion depth (0 = no limit).
    pub max_depth: usize,
    /// Print progress to stderr.
    pub verbose: bool,
}

/// Resolve the full transitive closure of missing build dependencies.
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
    resolver.validate()?;
    let mut closure: BTreeMap<String, ClosureEntry> = BTreeMap::new();
    let mut visited: BTreeSet<String> = BTreeSet::new();
    // Track depth per package: requested packages are depth 1.
    let mut depth: BTreeMap<String, usize> = BTreeMap::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    for p in packages {
        queue.push_back(p.clone());
        depth.insert(p.clone(), 1);
    }
    let mut warnings: Vec<String> = Vec::new();

    // Cache: dep string -> Option<source package name> on each branch.
    let mut target_cache: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut source_cache: BTreeMap<String, Option<String>> = BTreeMap::new();

    while let Some(pkg) = queue.pop_front() {
        if visited.contains(&pkg) {
            continue;
        }
        let pkg_depth = depth[&pkg];
        if options.max_depth > 0 && pkg_depth > options.max_depth {
            continue;
        }
        visited.insert(pkg.clone());

        if options.verbose {
            eprintln!(
                "[depth {}] resolving {pkg} ({} queued, {} resolved)",
                pkg_depth,
                queue.len(),
                closure.len(),
            );
        }

        // Query BuildRequires on the source branch.
        let build_reqs = match resolver.buildrequires(&pkg) {
            Ok(reqs) => reqs,
            Err(e) => {
                warnings.push(format!("{pkg}: failed to query BuildRequires: {e}"));
                closure.insert(
                    pkg,
                    ClosureEntry {
                        missing_deps: vec![],
                    },
                );
                continue;
            }
        };

        let mut missing_deps: Vec<MissingDep> = Vec::new();
        // Track which source packages we've already recorded as missing
        // for this package to avoid duplicate entries.
        let mut seen_providers: BTreeSet<String> = BTreeSet::new();

        for raw_dep in &build_reqs {
            let dep_str = raw_dep.trim();

            // Skip rpmlib/auto dependencies (provided by RPM itself).
            if dep_str.starts_with("rpmlib(") || dep_str.starts_with("auto(") {
                continue;
            }

            // Check if the versioned requirement is satisfied on target.
            // We use the full dep string (including version constraints)
            // so that fedrq checks whether the available version actually
            // meets the requirement.
            let target_resolved = target_cache
                .entry(dep_str.to_string())
                .or_insert_with(|| {
                    resolver
                        .resolve_target(dep_str)
                        .ok()
                        .and_then(|v| v.into_iter().find(|s| s != "(none)"))
                })
                .clone();

            if target_resolved.is_some() {
                // Satisfied (with correct version) on target, skip.
                continue;
            }

            // Resolve on source to find which package provides it.
            // Use the full versioned string here too so we get the
            // provider that actually satisfies the constraint.
            let source_resolved = source_cache
                .entry(dep_str.to_string())
                .or_insert_with(|| {
                    resolver
                        .resolve_source(dep_str)
                        .ok()
                        .and_then(|v| v.into_iter().find(|s| s != "(none)"))
                })
                .clone();

            let Some(provider) = source_resolved else {
                // Can't resolve on source either — likely a base system dep.
                continue;
            };

            // Don't record self-dependencies.
            if provider == pkg {
                continue;
            }

            if seen_providers.insert(provider.clone()) {
                missing_deps.push(MissingDep {
                    dep: raw_dep.clone(),
                    provided_by: provider.clone(),
                });

                // Queue the provider for recursive resolution.
                if !visited.contains(&provider) {
                    depth.entry(provider.clone()).or_insert(pkg_depth + 1);
                    queue.push_back(provider);
                }
            }
        }

        closure.insert(pkg, ClosureEntry { missing_deps });
    }

    Ok(Closure {
        source_branch: source_branch.to_string(),
        target_branch: target_branch.to_string(),
        requested: packages.to_vec(),
        closure,
        warnings,
    })
}

/// Real resolver backed by `sandogasa_fedrq::Fedrq`.
pub struct FedrqResolver {
    pub source: sandogasa_fedrq::Fedrq,
    pub target: sandogasa_fedrq::Fedrq,
}

impl DepResolver for FedrqResolver {
    fn validate(&self) -> Result<(), String> {
        // Probe both branches with a no-op query to catch bad configs early.
        self.source
            .resolve_to_source("bash")
            .map_err(|e| format!("source branch config error: {e}"))?;
        self.target
            .resolve_to_source("bash")
            .map_err(|e| format!("target branch config error: {e}"))?;
        Ok(())
    }

    fn buildrequires(&self, srpm: &str) -> Result<Vec<String>, String> {
        self.source
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
    }

    impl MockResolver {
        fn new() -> Self {
            Self {
                buildrequires: BTreeMap::new(),
                source_resolve: BTreeMap::new(),
                target_resolve: BTreeMap::new(),
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
    fn test_package_not_found_warns() {
        let resolver = MockResolver::new();
        let closure =
            resolve_closure(&resolver, &["nonexistent".to_string()], "rawhide", "epel10").unwrap();
        assert_eq!(closure.warnings.len(), 1);
        assert!(closure.warnings[0].contains("nonexistent"));
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
}
