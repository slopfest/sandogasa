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
use serde::{Deserialize, Serialize};

/// A BuildRequires entry that is missing on the target branch.
#[derive(Debug, Clone, Serialize)]
pub struct MissingDep {
    /// The raw BuildRequires string (e.g. "pkgconfig(libsystemd) >= 250").
    pub dep: String,
    /// The source package that provides this on the source branch.
    pub provided_by: String,
}

/// A dependency whose provider exists in the base distro (RHEL /
/// CentOS Stream / AlmaLinux) at a version that doesn't satisfy the
/// constraint. EPEL packages must not replace base-distro packages, so
/// these are pruned from the closure instead of becoming branch
/// requests — unless the user explicitly overrides (an alternate,
/// non-conflicting package, which needs a *new package review*, not a
/// branch request).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedByBase {
    /// The raw dep string that failed (e.g. "python3-setuptools >= 77").
    pub dep: String,
    /// Version-release the base distro offers.
    pub base_version: String,
    /// The base branch probed (e.g. "c10s", "al9").
    pub base_branch: String,
    /// Closure packages whose BuildRequires / subpackage Requires
    /// need this provider.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub required_by: BTreeSet<String>,
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
    /// Providers blocked by the base distro (keyed by the source
    /// package that would otherwise have been requested). Pruned from
    /// `closure` — see [`BlockedByBase`].
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub blocked_by_base: BTreeMap<String, BlockedByBase>,
    /// Providers the user chose to treat as alternate EPEL packages
    /// despite being in the base distro (via `--override` or the
    /// interactive prompt). These *are* in `closure`.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub overrides: BTreeSet<String>,
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

/// A persisted resolve closure plus branch-request tracking.
///
/// Written by `resolve --report` and consumed by the
/// branch-request subcommands (`file-request`, `file-requests`,
/// `escalate`). `edges` is already package-level (a package maps
/// to the packages it build-depends on), so branch requests link
/// directly along it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveReport {
    pub source_branch: String,
    pub target_branch: String,
    /// Every source package in the closure, sorted.
    pub packages: Vec<String>,
    /// Package → packages it depends on (empty-dep packages
    /// omitted to keep the file readable).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub edges: BTreeMap<String, BTreeSet<String>>,
    /// Filed branch requests, keyed by package. Populated by
    /// `file-request`/`file-requests`, consumed by `escalate`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub branch_requests: BTreeMap<String, BranchRequest>,
    /// Providers blocked by the base distro — never branch-request
    /// candidates (see [`BlockedByBase`]). Not part of `packages`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub blocked_by_base: BTreeMap<String, BlockedByBase>,
    /// Providers in `packages` that the user overrode despite being
    /// in the base distro. `file-requests` refuses these: an alternate
    /// package needs a new package review, not a branch request.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub overrides: BTreeSet<String>,
}

/// A filed branch-request bug and whether it's been escalated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchRequest {
    /// Red Hat Bugzilla number of the branch request.
    pub rhbz: u64,
    /// Whether the request has already been escalated (a
    /// `needinfo?` ping). Set by `escalate` so it never pings the
    /// same request twice.
    #[serde(default)]
    pub pinged: bool,
}

impl ResolveReport {
    /// Build a report from a resolved closure (no requests yet).
    pub fn from_closure(closure: &Closure) -> Self {
        let mut packages: Vec<String> = closure.closure.keys().cloned().collect();
        packages.sort();
        let edges: BTreeMap<String, BTreeSet<String>> = closure
            .to_edges()
            .into_iter()
            .filter(|(_, deps)| !deps.is_empty())
            .collect();
        ResolveReport {
            source_branch: closure.source_branch.clone(),
            target_branch: closure.target_branch.clone(),
            packages,
            edges,
            branch_requests: BTreeMap::new(),
            blocked_by_base: closure.blocked_by_base.clone(),
            overrides: closure.overrides.clone(),
        }
    }
}

/// Write a resolve report to a TOML file.
pub fn write_report(report: &ResolveReport, path: &str) -> Result<(), String> {
    let toml = toml::to_string_pretty(report).map_err(|e| format!("serialize report: {e}"))?;
    std::fs::write(path, toml).map_err(|e| format!("write {path}: {e}"))
}

/// Load a resolve report from a TOML file.
pub fn load_report(path: &str) -> Result<ResolveReport, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    toml::from_str(&s).map_err(|e| format!("parse {path}: {e}"))
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

    /// Resolve a bare capability (no version constraint) to
    /// `(source, version-release)` pairs on the **base-distro** branch
    /// (RHEL / CentOS Stream / AlmaLinux). Only called when the
    /// base-distro guard is active; the default (no base configured)
    /// reports nothing in base.
    fn resolve_base_vr(&self, _dep: &str) -> Result<Vec<(String, String)>, String> {
        Ok(vec![])
    }
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
    /// Base-distro branch behind the target (e.g. `c10s` for epel10).
    /// `Some` activates the base-distro guard: deps whose provider is
    /// in the base at an unsatisfying version are blocked instead of
    /// becoming closure members / branch requests.
    pub base_branch: Option<String>,
    /// Providers pre-approved (via `--override`) as alternate EPEL
    /// packages despite existing in the base distro.
    pub overrides: BTreeSet<String>,
    /// Allow prompting on a TTY for override decisions not covered
    /// by `overrides`. Non-interactive runs treat them as declined.
    pub interactive: bool,
}

/// Thread-safe dependency resolution cache.
///
/// Shared between `resolve_closure_with_options` and
/// `check_installability` so that repeated queries for the same
/// dependency string are not sent to fedrq twice.
pub struct ResolveCache {
    target: DashMap<String, Option<String>>,
    source: DashMap<String, Option<String>>,
    /// Capability → base-distro `(source, V-R)` providers (guard).
    base_probe: DashMap<String, Vec<(String, String)>>,
    /// Provider → override decision (approved?). Shared across the
    /// resolve/installability iterations so each provider is decided
    /// (and prompted for) at most once per run.
    override_decisions: Mutex<BTreeMap<String, bool>>,
}

impl ResolveCache {
    pub fn new() -> Self {
        Self {
            target: DashMap::new(),
            source: DashMap::new(),
            base_probe: DashMap::new(),
            override_decisions: Mutex::new(BTreeMap::new()),
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
    /// Runtime deps blocked by the base distro (declined overrides) —
    /// not added to `additional_packages`; merged into the closure's
    /// blocked map by [`resolve_with_installability`].
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub blocked_by_base: BTreeMap<String, BlockedByBase>,
}

/// Real resolver backed by `sandogasa_fedrq::Fedrq`.
pub struct FedrqResolver {
    pub source: sandogasa_fedrq::Fedrq,
    /// Source RPM queries when source uses a Koji repo.
    /// `@koji:` repos only index binary RPMs, so BuildRequires and
    /// subpackage Requires queries need `@koji-src:` instead.
    pub source_src: Option<sandogasa_fedrq::Fedrq>,
    pub target: sandogasa_fedrq::Fedrq,
    /// Base-distro repos behind an EPEL target (the base-distro
    /// guard's probe). `None` = guard inactive.
    pub base: Option<sandogasa_fedrq::Fedrq>,
}

/// The base-distro branch to probe for an EPEL-ish branch name.
///
/// - `epel9*` (and `c9s`) → **al9**: fedrq's `c9s` layers epel9 +
///   epel9-next on top of CentOS Stream 9 ("EPEL 9 is weird"), so
///   probing it would see EPEL's own packages; AlmaLinux 9 is a clean
///   RHEL 9 stand-in. UBI is not usable (incomplete package set).
/// - `epel10*` / `c10s` → **c10s** (clean base).
/// - anything else (epel8, Fedora branches) → `None` — no safe base
///   known; the guard stays off unless `--base-branch` is given.
pub fn epel_base_branch(branch: &str) -> Option<&'static str> {
    if branch.starts_with("epel10") || branch == "c10s" {
        Some("c10s")
    } else if branch.starts_with("epel9") || branch == "c9s" || branch == "al9" {
        Some("al9")
    } else {
        None
    }
}

/// Resolve the base-distro branch for the guard, if any.
///
/// Order: an explicit `--base-branch` always wins; an `epel*` target
/// branch uses the built-in mapping; a base-ish target branch paired
/// with an `@epel` target repo (the CBS pattern, e.g. `-t c10s
/// --target-repo @epel`) also maps. Anything else — Fedora targets,
/// unmapped EPEL branches like epel8 — leaves the guard inactive.
pub fn base_branch_for(
    base_flag: Option<&str>,
    target_branch: Option<&str>,
    target_repo: Option<&str>,
) -> Option<String> {
    if let Some(b) = base_flag {
        return Some(b.to_string());
    }
    let branch = target_branch?;
    if branch.starts_with("epel") {
        return epel_base_branch(branch).map(String::from);
    }
    if target_repo.is_some_and(|r| r.contains("@epel")) {
        return epel_base_branch(branch).map(String::from);
    }
    None
}

/// Split a plain dep string into its capability and optional version
/// constraint (`"python3-setuptools >= 77"` → `("python3-setuptools",
/// Some((">=", "77")))`). Returns `None` for rich (boolean) deps and
/// anything else that isn't the `name [op version]` shape — those keep
/// the pre-guard behavior.
fn parse_versioned_dep(dep: &str) -> Option<(&str, Option<(&str, &str)>)> {
    let dep = dep.trim();
    if dep.starts_with('(') {
        return None;
    }
    let parts: Vec<&str> = dep.split_whitespace().collect();
    match parts.as_slice() {
        [cap] => Some((cap, None)),
        [cap, op, ver] if matches!(*op, "<" | ">" | "<=" | ">=" | "=") => {
            Some((cap, Some((op, ver))))
        }
        _ => None,
    }
}

/// Whether an available `version-release` satisfies `op required`
/// under RPM semantics: when the required version carries no release,
/// the available release is ignored.
///
/// Note: the base probe returns V-R without the epoch, so an epoch in
/// the constraint (rare) compares against epoch 0.
fn constraint_satisfied(available_vr: &str, op: &str, required: &str) -> bool {
    use std::cmp::Ordering::*;
    let available = if required.contains('-') {
        available_vr
    } else {
        available_vr.split('-').next().unwrap_or(available_vr)
    };
    let ord = sandogasa_rpmvercmp::compare_evr(available, required);
    match op {
        "=" => ord == Equal,
        ">=" => ord != Less,
        "<=" => ord != Greater,
        ">" => ord == Greater,
        "<" => ord == Less,
        _ => false,
    }
}

/// How a would-be-missing dep relates to the base distro.
enum BaseClass {
    /// Not provided by the base at all — a normal missing dep.
    NotInBase,
    /// The base offers a version that satisfies the constraint. Can
    /// happen when the target repos don't include the base (e.g. an
    /// `@epel`-only target repo) — the dep isn't missing at all.
    SatisfiedByBase { base_vr: String },
    /// In the base, but no offered version satisfies the constraint.
    Blocked { base_vr: String },
}

/// Classify a dep that failed the target query against the base
/// distro. Only meaningful when the guard is active
/// (`options.base_branch` set); the probe is memoized per capability.
fn classify_against_base(
    resolver: &dyn DepResolver,
    cache: &ResolveCache,
    dep_str: &str,
) -> BaseClass {
    let Some((cap, constraint)) = parse_versioned_dep(dep_str) else {
        return BaseClass::NotInBase;
    };
    let providers = cache
        .base_probe
        .entry(cap.to_string())
        .or_insert_with(|| resolver.resolve_base_vr(cap).unwrap_or_default())
        .clone();
    // Best (highest) version the base offers for this capability.
    let Some(base_vr) = providers
        .into_iter()
        .map(|(_, vr)| vr)
        .max_by(|a, b| sandogasa_rpmvercmp::compare_evr(a, b))
    else {
        return BaseClass::NotInBase;
    };
    match constraint {
        None => BaseClass::SatisfiedByBase { base_vr },
        Some((op, ver)) if constraint_satisfied(&base_vr, op, ver) => {
            BaseClass::SatisfiedByBase { base_vr }
        }
        Some(_) => BaseClass::Blocked { base_vr },
    }
}

/// Interpret an override-prompt answer (default **no** — EPEL must not
/// replace base packages, so declining is the safe choice).
fn parse_yes(line: &str) -> bool {
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// A dep classified as blocked, pending an override decision at the
/// sequential merge point (prompts can't run inside the parallel BFS).
struct BlockedCandidate {
    /// Rawhide-side provider (the would-be branch-request package).
    provider: String,
    /// The raw dep string that failed.
    dep: String,
    /// Best version-release the base distro offers.
    base_vr: String,
}

/// Decide whether `provider` may be treated as an alternate EPEL
/// package: pre-approved via `--override`, previously decided this
/// run, or (interactively) asked — default no. The decision is cached
/// so each provider is prompted for at most once per run.
fn decide_override(
    options: &ResolveOptions,
    cache: &ResolveCache,
    candidate: &BlockedCandidate,
    parent: &str,
    base_branch: &str,
) -> bool {
    let mut decisions = cache.override_decisions.lock().unwrap();
    if options.overrides.contains(&candidate.provider) {
        // Recorded so the closure's final `overrides` set (derived from
        // the decisions map) includes flag-approved providers too.
        decisions.insert(candidate.provider.clone(), true);
        return true;
    }
    if let Some(&decided) = decisions.get(&candidate.provider) {
        return decided;
    }
    let approved = options.interactive && prompt_override(candidate, parent, base_branch);
    decisions.insert(candidate.provider.clone(), approved);
    approved
}

/// Ask (on stderr/stdin) whether to descend into a base-distro-blocked
/// provider as an alternate-package override.
fn prompt_override(candidate: &BlockedCandidate, parent: &str, base_branch: &str) -> bool {
    use std::io::{BufRead, Write};
    eprintln!(
        "{} is in {base_branch} (base distro; EPEL must not replace it).",
        candidate.provider
    );
    eprintln!(
        "  needed by {parent}: {} — {base_branch} has {}",
        candidate.dep, candidate.base_vr
    );
    eprint!("Descend as an alternate-package override? [y/N]: ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    parse_yes(&line)
}

/// Merge one blocked map into another, unioning `required_by` for
/// providers present in both.
fn merge_blocked(dst: &mut BTreeMap<String, BlockedByBase>, src: BTreeMap<String, BlockedByBase>) {
    for (provider, entry) in src {
        dst.entry(provider)
            .and_modify(|e| e.required_by.extend(entry.required_by.iter().cloned()))
            .or_insert(entry);
    }
}

/// Fold a blocked candidate into the blocked map (merging
/// `required_by` when several parents hit the same provider).
fn record_blocked(
    blocked: &mut BTreeMap<String, BlockedByBase>,
    candidate: BlockedCandidate,
    parent: &str,
    base_branch: &str,
) {
    blocked
        .entry(candidate.provider)
        .or_insert_with(|| BlockedByBase {
            dep: candidate.dep,
            base_version: candidate.base_vr,
            base_branch: base_branch.to_string(),
            required_by: BTreeSet::new(),
        })
        .required_by
        .insert(parent.to_string());
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
    let mut blocked_by_base: BTreeMap<String, BlockedByBase> = BTreeMap::new();

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
                            vec![],
                            vec![warn],
                            log,
                        );
                    }
                };

                let mut missing_deps: Vec<MissingDep> = Vec::new();
                let mut seen_providers: BTreeSet<String> = BTreeSet::new();
                let mut new_packages: Vec<String> = Vec::new();
                let mut blocked_candidates: Vec<BlockedCandidate> = Vec::new();

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

                    // Base-distro guard: a provider that exists in the
                    // base at an unsatisfying version must not become a
                    // branch request (EPEL can't replace base packages).
                    // Decision (override vs prune) happens sequentially.
                    if options.base_branch.is_some() {
                        match classify_against_base(resolver, cache, dep_str) {
                            BaseClass::NotInBase => {}
                            BaseClass::SatisfiedByBase { base_vr } => {
                                if options.verbose {
                                    log.push(format!("  {dep_str}: satisfied by base ({base_vr})"));
                                }
                                continue;
                            }
                            BaseClass::Blocked { base_vr } => {
                                if seen_providers.insert(provider.clone()) {
                                    blocked_candidates.push(BlockedCandidate {
                                        provider,
                                        dep: raw_dep.clone(),
                                        base_vr,
                                    });
                                }
                                continue;
                            }
                        }
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
                    blocked_candidates,
                    vec![],
                    log,
                )
            })
            .collect();

        // Collect results sequentially: update closure, queue next
        // level, and decide blocked candidates (this is the only
        // place the override prompt may fire).
        let mut next_level: Vec<String> = Vec::new();
        for (pkg, mut entry, new_pkgs, candidates, warns, log) in results {
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
            for cand in candidates {
                let base = options.base_branch.as_deref().unwrap_or_default();
                if decide_override(options, cache, &cand, &pkg, base) {
                    if !visited.contains(&cand.provider) {
                        depth.entry(cand.provider.clone()).or_insert(pkg_depth + 1);
                        next_level.push(cand.provider.clone());
                    }
                    entry.missing_deps.push(MissingDep {
                        dep: cand.dep,
                        provided_by: cand.provider,
                    });
                } else {
                    record_blocked(&mut blocked_by_base, cand, &pkg, base);
                }
            }
            closure.insert(pkg, entry);
        }
        next_level.sort();
        next_level.dedup();
        current_level = next_level;
    }

    // Providers approved as overrides (flag or prompt) that actually
    // entered the closure — recorded so the report can refuse to file
    // branch requests for them (they need a new package review).
    let overrides: BTreeSet<String> = {
        let decisions = cache.override_decisions.lock().unwrap();
        decisions
            .iter()
            .filter(|(pkg, approved)| **approved && closure.contains_key(pkg.as_str()))
            .map(|(pkg, _)| pkg.clone())
            .collect()
    };

    Ok(Closure {
        source_branch: source_branch.to_string(),
        target_branch: target_branch.to_string(),
        requested: packages.to_vec(),
        closure,
        blocked_by_base,
        overrides,
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
            let mut blocked_candidates: Vec<BlockedCandidate> = Vec::new();

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
                        // Base-distro guard (see the closure BFS): a
                        // provider in the base at an unsatisfying
                        // version must not be pulled into the closure.
                        if options.base_branch.is_some() {
                            match classify_against_base(resolver, cache, dep_str) {
                                BaseClass::NotInBase => {}
                                BaseClass::SatisfiedByBase { .. } => {
                                    continue;
                                }
                                BaseClass::Blocked { base_vr } => {
                                    if seen_providers.insert(provider.clone()) {
                                        blocked_candidates.push(BlockedCandidate {
                                            provider: provider.clone(),
                                            dep: raw_dep.clone(),
                                            base_vr,
                                        });
                                    }
                                    continue;
                                }
                            }
                        }
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

            if unsatisfied.is_empty() && blocked_candidates.is_empty() {
                None
            } else {
                Some((pkg.to_string(), unsatisfied, additional, blocked_candidates))
            }
        })
        .collect();

    // Print warnings collected from parallel section.
    for warn in warn_collector.into_inner().unwrap() {
        eprintln!("{warn}");
    }

    // Merge results; blocked candidates are decided here (the only
    // place the override prompt may fire).
    let mut issues: BTreeMap<String, InstallabilityEntry> = BTreeMap::new();
    let mut additional_packages: BTreeSet<String> = BTreeSet::new();
    let mut blocked_by_base: BTreeMap<String, BlockedByBase> = BTreeMap::new();
    for (pkg, mut unsatisfied, mut additional, candidates) in results {
        for cand in candidates {
            let base = options.base_branch.as_deref().unwrap_or_default();
            if decide_override(options, cache, &cand, &pkg, base) {
                additional.push(cand.provider.clone());
                unsatisfied.push(UnsatisfiedRequires {
                    dep: cand.dep,
                    provided_by: Some(cand.provider),
                });
            } else {
                record_blocked(&mut blocked_by_base, cand, &pkg, base);
            }
        }
        if !unsatisfied.is_empty() {
            issues.insert(pkg, InstallabilityEntry { unsatisfied });
        }
        additional_packages.extend(additional);
    }

    InstallabilityReport {
        issues,
        additional_packages,
        blocked_by_base,
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
    // Base-blocked runtime deps, accumulated across rounds (a package
    // whose only issue was a blocked dep counts as "passed" and isn't
    // re-checked, so its blocked entries must be kept here).
    let mut install_blocked: BTreeMap<String, BlockedByBase> = BTreeMap::new();

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

        // Keep base-blocked runtime deps from every round.
        for (provider, entry) in &report.blocked_by_base {
            install_blocked
                .entry(provider.clone())
                .and_modify(|e| e.required_by.extend(entry.required_by.iter().cloned()))
                .or_insert_with(|| entry.clone());
        }

        if report.additional_packages.is_empty() {
            // Fixed point reached. Restore original requested list.
            closure.requested = requested;
            merge_blocked(&mut closure.blocked_by_base, install_blocked);
            return Ok((closure, report));
        }

        let before = all_packages.len();
        all_packages.extend(report.additional_packages.iter().cloned());

        if all_packages.len() == before {
            // No new packages were actually added (all were already
            // in the set). This shouldn't happen given the check
            // above, but guard against infinite loops.
            closure.requested = requested;
            merge_blocked(&mut closure.blocked_by_base, install_blocked);
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

    fn resolve_base_vr(&self, dep: &str) -> Result<Vec<(String, String)>, String> {
        match &self.base {
            Some(base) => base.resolve_source_vr(dep).map_err(|e| e.to_string()),
            None => Ok(vec![]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_report_from_closure_drops_empty_edges() {
        let mut closure_map = BTreeMap::new();
        closure_map.insert(
            "a".to_string(),
            ClosureEntry {
                missing_deps: vec![MissingDep {
                    dep: "needs-b".into(),
                    provided_by: "b".into(),
                }],
            },
        );
        closure_map.insert(
            "b".to_string(),
            ClosureEntry {
                missing_deps: vec![],
            },
        );
        let closure = Closure {
            source_branch: "rawhide".into(),
            target_branch: "epel9".into(),
            requested: vec!["a".into()],
            closure: closure_map,
            blocked_by_base: BTreeMap::new(),
            overrides: BTreeSet::new(),
            warnings: vec![],
        };
        let report = ResolveReport::from_closure(&closure);
        assert_eq!(report.packages, vec!["a", "b"]);
        // a depends on b; b has no deps so it's omitted from edges.
        assert_eq!(report.edges.len(), 1);
        assert_eq!(
            report
                .edges
                .get("a")
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["b".to_string()]
        );
        assert!(report.branch_requests.is_empty());
    }

    #[test]
    fn resolve_report_toml_round_trip() {
        let mut edges = BTreeMap::new();
        edges.insert("a".to_string(), BTreeSet::from(["b".to_string()]));
        let mut requests = BTreeMap::new();
        requests.insert(
            "a".to_string(),
            BranchRequest {
                rhbz: 42,
                pinged: true,
            },
        );
        let mut blocked = BTreeMap::new();
        blocked.insert(
            "python-setuptools".to_string(),
            BlockedByBase {
                dep: "python3-setuptools >= 77".into(),
                base_version: "69.0.3-9.el10".into(),
                base_branch: "c10s".into(),
                required_by: BTreeSet::from(["python-django6".to_string()]),
            },
        );
        let report = ResolveReport {
            source_branch: "rawhide".into(),
            target_branch: "epel9".into(),
            packages: vec!["a".into(), "b".into()],
            edges,
            branch_requests: requests,
            blocked_by_base: blocked,
            overrides: BTreeSet::from(["a".to_string()]),
        };
        let toml = toml::to_string_pretty(&report).unwrap();
        let back: ResolveReport = toml::from_str(&toml).unwrap();
        assert_eq!(back.packages, report.packages);
        assert_eq!(back.branch_requests.get("a").unwrap().rhbz, 42);
        assert!(back.branch_requests.get("a").unwrap().pinged);
        let b = back.blocked_by_base.get("python-setuptools").unwrap();
        assert_eq!(b.base_version, "69.0.3-9.el10");
        assert!(b.required_by.contains("python-django6"));
        assert!(back.overrides.contains("a"));
    }

    #[test]
    fn resolve_report_pre_guard_toml_still_loads() {
        // A report written before the base-distro guard existed has
        // none of the new fields — it must still load.
        let old = r#"
source_branch = "rawhide"
target_branch = "epel10"
packages = ["a"]
"#;
        let back: ResolveReport = toml::from_str(old).unwrap();
        assert!(back.blocked_by_base.is_empty());
        assert!(back.overrides.is_empty());
    }

    struct MockResolver {
        /// srpm -> BuildRequires list (source branch)
        buildrequires: BTreeMap<String, Vec<String>>,
        /// dep -> source package on source branch
        source_resolve: BTreeMap<String, String>,
        /// dep -> source package on target branch
        target_resolve: BTreeMap<String, String>,
        /// srpm -> subpackage Requires (source branch)
        subpkg_requires: BTreeMap<String, Vec<String>>,
        /// bare capability -> (source, V-R) on the base-distro branch
        base_resolve: BTreeMap<String, Vec<(String, String)>>,
    }

    impl MockResolver {
        fn new() -> Self {
            Self {
                buildrequires: BTreeMap::new(),
                source_resolve: BTreeMap::new(),
                target_resolve: BTreeMap::new(),
                subpkg_requires: BTreeMap::new(),
                base_resolve: BTreeMap::new(),
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

        fn add_base_resolve(&mut self, cap: &str, source: &str, vr: &str) {
            self.base_resolve
                .entry(cap.to_string())
                .or_default()
                .push((source.to_string(), vr.to_string()));
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

        fn resolve_base_vr(&self, dep: &str) -> Result<Vec<(String, String)>, String> {
            Ok(self.base_resolve.get(dep).cloned().unwrap_or_default())
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

    // --- base-distro guard ---

    fn guard_opts(overrides: &[&str]) -> ResolveOptions {
        ResolveOptions {
            base_branch: Some("c10s".to_string()),
            overrides: overrides.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn epel_base_branch_mapping() {
        assert_eq!(epel_base_branch("epel10"), Some("c10s"));
        assert_eq!(epel_base_branch("epel10.3"), Some("c10s"));
        assert_eq!(epel_base_branch("c10s"), Some("c10s"));
        // EPEL 9 is weird: c9s layers epel9 + epel9-next, so AlmaLinux
        // stands in for RHEL 9.
        assert_eq!(epel_base_branch("epel9"), Some("al9"));
        assert_eq!(epel_base_branch("c9s"), Some("al9"));
        assert_eq!(epel_base_branch("al9"), Some("al9"));
        assert_eq!(epel_base_branch("epel8"), None);
        assert_eq!(epel_base_branch("rawhide"), None);
        assert_eq!(epel_base_branch("f45"), None);
    }

    #[test]
    fn base_branch_for_resolution_order() {
        // Explicit flag always wins.
        assert_eq!(
            base_branch_for(Some("al9"), Some("epel10"), None),
            Some("al9".to_string())
        );
        // EPEL target branch → mapped.
        assert_eq!(
            base_branch_for(None, Some("epel10"), None),
            Some("c10s".to_string())
        );
        assert_eq!(
            base_branch_for(None, Some("epel9"), None),
            Some("al9".to_string())
        );
        // Unmapped EPEL branch → guard off (caller warns).
        assert_eq!(base_branch_for(None, Some("epel8"), None), None);
        // CBS pattern: base-ish branch + @epel repo → the branch's base.
        assert_eq!(
            base_branch_for(None, Some("c10s"), Some("@epel")),
            Some("c10s".to_string())
        );
        assert_eq!(
            base_branch_for(None, Some("c9s"), Some("@epel")),
            Some("al9".to_string())
        );
        // Plain base target without @epel: SIG builds may override base
        // packages — guard off.
        assert_eq!(base_branch_for(None, Some("c10s"), None), None);
        // Fedora target → guard off.
        assert_eq!(base_branch_for(None, Some("rawhide"), None), None);
        assert_eq!(base_branch_for(None, None, Some("@koji:f45-build")), None);
    }

    #[test]
    fn parse_versioned_dep_shapes() {
        assert_eq!(
            parse_versioned_dep("python3-setuptools >= 77"),
            Some(("python3-setuptools", Some((">=", "77"))))
        );
        assert_eq!(
            parse_versioned_dep("pkgconfig(libsystemd)"),
            Some(("pkgconfig(libsystemd)", None))
        );
        assert_eq!(
            parse_versioned_dep("foo = 1.2-3"),
            Some(("foo", Some(("=", "1.2-3"))))
        );
        // Rich deps and odd shapes skip the guard.
        assert_eq!(parse_versioned_dep("(foo if bar)"), None);
        assert_eq!(parse_versioned_dep("foo bar baz qux"), None);
    }

    #[test]
    fn constraint_satisfied_rpm_semantics() {
        // Release ignored when the constraint has none.
        assert!(constraint_satisfied("77.0.3-1.el10", ">=", "77"));
        assert!(!constraint_satisfied("69.0.3-9.el10", ">=", "77"));
        assert!(constraint_satisfied("69.0.3-9.el10", "<", "77"));
        assert!(constraint_satisfied("77.0.0-1.el10", "=", "77.0.0"));
        assert!(!constraint_satisfied("77.0.1-1.el10", "=", "77.0.0"));
        // Constraint with a release compares the full V-R.
        assert!(constraint_satisfied("1.2-4.el10", ">=", "1.2-3"));
        assert!(!constraint_satisfied("1.2-2.el10", ">=", "1.2-3"));
        assert!(constraint_satisfied("2.0-1.el10", ">", "1.9"));
        assert!(constraint_satisfied("1.0-1.el10", "<=", "1.0"));
    }

    #[test]
    fn parse_yes_defaults_no() {
        assert!(parse_yes("y"));
        assert!(parse_yes("Yes"));
        assert!(!parse_yes(""));
        assert!(!parse_yes("n"));
        assert!(!parse_yes("maybe"));
    }

    #[test]
    fn guard_blocks_base_too_old() {
        // The rhbz#2482250 shape: python-django6 needs setuptools >= 77,
        // c10s has 69 — must be blocked, not a branch-request candidate.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("python-django6", &["python3-setuptools >= 77"]);
        resolver.add_source_resolve("python3-setuptools >= 77", "python-setuptools");
        resolver.add_base_resolve("python3-setuptools", "python-setuptools", "69.0.3-9.el10");

        let closure = resolve_closure_with_options(
            &resolver,
            &["python-django6".to_string()],
            "rawhide",
            "epel10",
            &guard_opts(&[]),
        )
        .unwrap();

        // Pruned: not in the closure, no edges, recorded as blocked.
        assert_eq!(closure.closure.len(), 1);
        assert!(closure.closure["python-django6"].missing_deps.is_empty());
        let blocked = closure.blocked_by_base.get("python-setuptools").unwrap();
        assert_eq!(blocked.dep, "python3-setuptools >= 77");
        assert_eq!(blocked.base_version, "69.0.3-9.el10");
        assert_eq!(blocked.base_branch, "c10s");
        assert!(blocked.required_by.contains("python-django6"));
        assert!(closure.overrides.is_empty());
    }

    #[test]
    fn guard_override_flag_descends() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("python-django6", &["python3-setuptools >= 77"]);
        resolver.add_source_resolve("python3-setuptools >= 77", "python-setuptools");
        resolver.add_base_resolve("python3-setuptools", "python-setuptools", "69.0.3-9.el10");
        resolver.add_buildrequires("python-setuptools", &[]);

        let closure = resolve_closure_with_options(
            &resolver,
            &["python-django6".to_string()],
            "rawhide",
            "epel10",
            &guard_opts(&["python-setuptools"]),
        )
        .unwrap();

        // Overridden: descended into, marked, not blocked.
        assert!(closure.closure.contains_key("python-setuptools"));
        assert_eq!(
            closure.closure["python-django6"].missing_deps[0].provided_by,
            "python-setuptools"
        );
        assert!(closure.overrides.contains("python-setuptools"));
        assert!(closure.blocked_by_base.is_empty());
    }

    #[test]
    fn guard_base_satisfying_version_skips_dep() {
        // @epel-only target repos don't see the base, so a dep the base
        // actually satisfies would otherwise be treated as missing.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &["python3-foo >= 1"]);
        resolver.add_source_resolve("python3-foo >= 1", "python-foo");
        resolver.add_base_resolve("python3-foo", "python-foo", "2.0-1.el10");

        let closure = resolve_closure_with_options(
            &resolver,
            &["mypkg".to_string()],
            "rawhide",
            "epel10",
            &guard_opts(&[]),
        )
        .unwrap();

        assert_eq!(closure.closure.len(), 1);
        assert!(closure.closure["mypkg"].missing_deps.is_empty());
        assert!(closure.blocked_by_base.is_empty());
    }

    #[test]
    fn guard_unversioned_dep_in_base_is_satisfied() {
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &["python3-foo"]);
        resolver.add_source_resolve("python3-foo", "python-foo");
        resolver.add_base_resolve("python3-foo", "python-foo", "1.0-1.el10");

        let closure = resolve_closure_with_options(
            &resolver,
            &["mypkg".to_string()],
            "rawhide",
            "epel10",
            &guard_opts(&[]),
        )
        .unwrap();

        assert!(closure.closure["mypkg"].missing_deps.is_empty());
        assert!(closure.blocked_by_base.is_empty());
    }

    #[test]
    fn guard_inactive_keeps_missing_behavior() {
        // Without a base branch the guard is off: even with the
        // capability in the base map, the dep is a plain missing dep.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &["python3-foo >= 2"]);
        resolver.add_source_resolve("python3-foo >= 2", "python-foo");
        resolver.add_base_resolve("python3-foo", "python-foo", "1.0-1.el10");
        resolver.add_buildrequires("python-foo", &[]);

        let closure =
            resolve_closure(&resolver, &["mypkg".to_string()], "rawhide", "epel10").unwrap();
        assert!(closure.closure.contains_key("python-foo"));
        assert!(closure.blocked_by_base.is_empty());
    }

    #[test]
    fn guard_rich_dep_bypasses_classification() {
        // Rich deps can't be parsed into cap+constraint; they keep the
        // pre-guard behavior (missing → descend).
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &["(python3-foo if weird)"]);
        resolver.add_source_resolve("(python3-foo if weird)", "python-foo");
        resolver.add_base_resolve("python3-foo", "python-foo", "1.0-1.el10");
        resolver.add_buildrequires("python-foo", &[]);

        let closure = resolve_closure_with_options(
            &resolver,
            &["mypkg".to_string()],
            "rawhide",
            "epel10",
            &guard_opts(&[]),
        )
        .unwrap();
        assert!(closure.closure.contains_key("python-foo"));
        assert!(closure.blocked_by_base.is_empty());
    }

    #[test]
    fn guard_blocks_runtime_dep_in_installability() {
        // A subpackage Requires hitting a base-blocked provider must
        // not expand the closure; it lands in blocked_by_base instead.
        let mut resolver = MockResolver::new();
        resolver.add_buildrequires("mypkg", &[]);
        resolver.add_subpkg_requires("mypkg", &["python3-setuptools >= 77"]);
        resolver.add_source_resolve("python3-setuptools >= 77", "python-setuptools");
        resolver.add_base_resolve("python3-setuptools", "python-setuptools", "69.0.3-9.el10");

        let (closure, report) = resolve_with_installability(
            &resolver,
            &["mypkg".to_string()],
            "rawhide",
            "epel10",
            &guard_opts(&[]),
        )
        .unwrap();

        assert_eq!(closure.closure.len(), 1);
        assert!(report.additional_packages.is_empty());
        let blocked = closure.blocked_by_base.get("python-setuptools").unwrap();
        assert!(blocked.required_by.contains("mypkg"));
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
