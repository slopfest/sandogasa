// SPDX-License-Identifier: MPL-2.0

//! Analyze a crates.io crate's dependencies against a target RPM repo.
//!
//! Fetches the dependency list from crates.io, then checks each dependency
//! against the target repo to determine if it is available as an RPM,
//! whether the available version satisfies the crate's version requirement,
//! or if it is missing entirely.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::dag;

// ---- Public types ----

/// Options for the check-crate command.
pub struct CheckCrateOptions {
    pub branch: String,
    pub repo: Option<String>,
    pub verbose: bool,
    pub transitive: bool,
    pub include_dev: bool,
    pub include_optional: bool,
    pub include_too_old: bool,
    pub exclude: HashSet<String>,
}

/// A dependency from crates.io.
#[derive(Debug, Clone, Serialize)]
pub struct CrateDep {
    pub name: String,
    pub version_req: String,
    pub kind: String,
    pub optional: bool,
}

/// Status of a dependency in the target repo.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status")]
pub enum DepStatus {
    /// The RPM provides a version that satisfies the requirement.
    #[serde(rename = "satisfied")]
    Satisfied { version: String },
    /// The RPM exists but its version is too old.
    #[serde(rename = "too_old")]
    TooOld { have: String, need: String },
    /// No RPM provides this crate.
    #[serde(rename = "missing")]
    Missing,
}

/// A dependency check result.
#[derive(Debug, Clone, Serialize)]
pub struct DepResult {
    #[serde(flatten)]
    pub dep: CrateDep,
    #[serde(flatten)]
    pub status: DepStatus,
}

/// A transitively-missing dependency.
#[derive(Debug, Clone, Serialize)]
pub struct TransitiveDep {
    pub name: String,
    pub version: String,
    pub version_req: String,
    pub pulled_by: String,
}

/// Full report for a crate check.
#[derive(Debug, Serialize)]
pub struct CheckCrateReport {
    pub crate_name: String,
    pub crate_version: String,
    pub branch: String,
    pub dependencies: Vec<DepResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub transitive_missing: Vec<TransitiveDep>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub transitive_build_order: Vec<dag::BuildPhase>,
}

// ---- Public functions ----

/// Run the check-crate analysis.
pub fn check_crate(
    name: &str,
    version: Option<&str>,
    opts: &CheckCrateOptions,
) -> Result<CheckCrateReport, String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;

    // Resolve version.
    let version = match version {
        Some(v) => {
            if opts.verbose {
                eprintln!("[check-crate] resolving version {v} for {name}");
            }
            rt.block_on(resolve_version(name, v))?
        }
        None => {
            if opts.verbose {
                eprintln!("[check-crate] fetching latest version for {name}");
            }
            rt.block_on(fetch_latest_version(name))?
        }
    };

    if opts.verbose {
        eprintln!("[check-crate] fetching dependencies for {name} {version}");
    }

    let deps = rt.block_on(fetch_dependencies(name, &version))?;

    if opts.verbose {
        let normal = deps.iter().filter(|d| d.kind == "normal").count();
        let build = deps.iter().filter(|d| d.kind == "build").count();
        let dev = deps.iter().filter(|d| d.kind == "dev").count();
        eprintln!(
            "[check-crate] {} dependencies ({normal} normal, \
             {build} build, {dev} dev)",
            deps.len()
        );
    }

    let fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(opts.branch.clone()),
        repo: opts.repo.clone(),
    };

    if opts.verbose {
        eprintln!("[check-crate] checking dependencies against repo");
    }

    let dependencies: Vec<DepResult> = deps
        .par_iter()
        .map(|dep| {
            let status = check_dep_in_repo(&fedrq, dep);
            DepResult {
                dep: dep.clone(),
                status,
            }
        })
        .collect();

    let (transitive_missing, transitive_build_order) = if opts.transitive {
        let (deps, edges) = expand_transitive(&rt, &fedrq, &dependencies, opts)?;
        let phases = if edges.is_empty() {
            vec![]
        } else {
            match dag::topological_layers(&edges) {
                Ok(p) => p,
                Err(_) => {
                    eprintln!(
                        "warning: transitive dependency graph has cycles; \
                         build order unavailable"
                    );
                    vec![]
                }
            }
        };
        (deps, phases)
    } else {
        (vec![], vec![])
    };

    // Filter out excluded crates from the direct dependency list.
    let dependencies = if opts.exclude.is_empty() {
        dependencies
    } else {
        dependencies
            .into_iter()
            .filter(|d| !opts.exclude.contains(&d.dep.name))
            .collect()
    };

    Ok(CheckCrateReport {
        crate_name: name.to_string(),
        crate_version: version,
        branch: opts.branch.clone(),
        dependencies,
        transitive_missing,
        transitive_build_order,
    })
}

/// Print a human-readable report to stdout.
pub fn print_report(report: &CheckCrateReport) {
    println!(
        "Checking crate: {} {}",
        report.crate_name, report.crate_version
    );
    println!("Branch: {}\n", report.branch);

    let normal = report
        .dependencies
        .iter()
        .filter(|d| d.dep.kind == "normal")
        .count();
    let build = report
        .dependencies
        .iter()
        .filter(|d| d.dep.kind == "build")
        .count();
    let dev = report
        .dependencies
        .iter()
        .filter(|d| d.dep.kind == "dev")
        .count();
    println!("Dependencies ({normal} normal, {build} build, {dev} dev):\n");

    let missing: Vec<&DepResult> = report
        .dependencies
        .iter()
        .filter(|d| matches!(d.status, DepStatus::Missing))
        .collect();
    let too_old: Vec<&DepResult> = report
        .dependencies
        .iter()
        .filter(|d| matches!(d.status, DepStatus::TooOld { .. }))
        .collect();
    let satisfied: Vec<&DepResult> = report
        .dependencies
        .iter()
        .filter(|d| matches!(d.status, DepStatus::Satisfied { .. }))
        .collect();

    if !missing.is_empty() {
        print_section_header("Missing", &missing);
        for d in &missing {
            println!(
                "  - {} {} ({}{})",
                d.dep.name,
                d.dep.version_req,
                d.dep.kind,
                opt_label(d)
            );
        }
        println!();
    }

    if !too_old.is_empty() {
        print_section_header("Too old", &too_old);
        for d in &too_old {
            if let DepStatus::TooOld { have, need } = &d.status {
                println!(
                    "  - {} {} ({}{})",
                    d.dep.name,
                    d.dep.version_req,
                    d.dep.kind,
                    opt_label(d)
                );
                println!("    have: {have}, need: {need}");
            }
        }
        println!();
    }

    if !satisfied.is_empty() {
        print_section_header("Satisfied", &satisfied);
        for d in &satisfied {
            if let DepStatus::Satisfied { version } = &d.status {
                println!(
                    "  - {} {} ({}{}) — {version}",
                    d.dep.name,
                    d.dep.version_req,
                    d.dep.kind,
                    opt_label(d)
                );
            }
        }
        println!();
    }

    if !report.transitive_build_order.is_empty() {
        let total: usize = report
            .transitive_build_order
            .iter()
            .map(|p| p.packages.len())
            .sum();
        println!(
            "Build order ({total} package(s) in {} phase(s)):",
            report.transitive_build_order.len()
        );
        for phase in &report.transitive_build_order {
            println!("\n  Phase {}:", phase.phase);
            for pkg in &phase.packages {
                println!("    - rust-{pkg}");
            }
        }
        if !report.transitive_missing.is_empty() {
            println!(
                "\n  ({} discovered transitively)",
                report.transitive_missing.len()
            );
        }
        println!();
    } else if !report.transitive_missing.is_empty() {
        println!("Transitive missing ({}):", report.transitive_missing.len());
        for d in &report.transitive_missing {
            println!("  - {} {} (via {})", d.name, d.version_req, d.pulled_by);
        }
        println!();
    }

    let n_missing = unique_crate_count(&missing);
    let n_too_old = unique_crate_count(&too_old);
    let n_satisfied = unique_crate_count(&satisfied);
    if report.transitive_missing.is_empty() {
        println!("Summary: {n_missing} missing, {n_too_old} too old, {n_satisfied} satisfied.");
    } else {
        println!(
            "Summary: {n_missing} missing (+ {} transitive), \
             {n_too_old} too old, {n_satisfied} satisfied.",
            report.transitive_missing.len(),
        );
    }
}

// ---- Private helpers ----

fn opt_label(d: &DepResult) -> &str {
    if d.dep.optional { ", optional" } else { "" }
}

/// Count unique crate names in a list of dep results.
fn unique_crate_count(deps: &[&DepResult]) -> usize {
    let names: HashSet<&str> = deps.iter().map(|d| d.dep.name.as_str()).collect();
    names.len()
}

/// Print a section header with entry count and unique crate count.
fn print_section_header(label: &str, deps: &[&DepResult]) {
    let unique = unique_crate_count(deps);
    if unique == deps.len() {
        println!("{label} ({unique}):");
    } else {
        println!("{label} ({unique} crate(s), {} entries):", deps.len());
    }
}

/// Dependency edges: `edges[A] = {B, C}` means A depends on B and C.
type DepEdges = BTreeMap<String, BTreeSet<String>>;

/// Whether a dependency should be expanded in transitive mode.
fn should_expand(dep: &CrateDep, opts: &CheckCrateOptions) -> bool {
    if dep.optional && !opts.include_optional {
        return false;
    }
    match dep.kind.as_str() {
        "normal" | "build" => true,
        "dev" => opts.include_dev,
        _ => false,
    }
}

/// BFS expansion of missing dependencies.
///
/// For each missing direct dep, fetches its dependencies from crates.io,
/// checks them against the repo, and recurses into any that are also
/// missing. Returns a deduplicated list of transitively-missing crates
/// and a dependency edge map for build-order computation.
fn expand_transitive(
    rt: &tokio::runtime::Runtime,
    fedrq: &sandogasa_fedrq::Fedrq,
    direct_results: &[DepResult],
    opts: &CheckCrateOptions,
) -> Result<(Vec<TransitiveDep>, DepEdges), String> {
    let mut visited: HashSet<String> = opts.exclude.clone();
    let mut result: Vec<TransitiveDep> = Vec::new();
    // All missing crate names (direct + transitive) for edge filtering.
    let mut all_missing: HashSet<String> = HashSet::new();
    // edges[A] = {B, C} means A depends on missing crates B and C.
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Deferred edge recording: (parent_crate, Vec<missing_dep_names>).
    let mut pending_edges: Vec<(String, Vec<String>)> = Vec::new();

    let needs_rebuild = |status: &DepStatus| -> bool {
        matches!(status, DepStatus::Missing)
            || (opts.include_too_old && matches!(status, DepStatus::TooOld { .. }))
    };

    // Seed: direct deps that need (re)building and pass the kind filter.
    let mut queue: VecDeque<String> = VecDeque::new();
    for dr in direct_results {
        let excluded = visited.contains(&dr.dep.name);
        visited.insert(dr.dep.name.clone());
        if !excluded && needs_rebuild(&dr.status) && should_expand(&dr.dep, opts) {
            all_missing.insert(dr.dep.name.clone());
            queue.push_back(dr.dep.name.clone());
        }
    }

    while let Some(crate_name) = queue.pop_front() {
        if opts.verbose {
            eprintln!("[check-crate] expanding transitive deps for {crate_name}");
        }

        let version = match rt.block_on(fetch_latest_version(&crate_name)) {
            Ok(v) => v,
            Err(e) => {
                if opts.verbose {
                    eprintln!("[check-crate] warning: failed to fetch {crate_name}: {e}");
                }
                continue;
            }
        };

        let deps = match rt.block_on(fetch_dependencies(&crate_name, &version)) {
            Ok(d) => d,
            Err(e) => {
                if opts.verbose {
                    eprintln!(
                        "[check-crate] warning: failed to fetch deps \
                         for {crate_name} {version}: {e}"
                    );
                }
                continue;
            }
        };

        // Filter to relevant kinds and check against repo in parallel.
        let relevant: Vec<&CrateDep> = deps.iter().filter(|d| should_expand(d, opts)).collect();

        let results: Vec<DepResult> = relevant
            .par_iter()
            .map(|dep| {
                let status = check_dep_in_repo(fedrq, dep);
                DepResult {
                    dep: (*dep).clone(),
                    status,
                }
            })
            .collect();

        let mut rebuild_deps_of_crate: Vec<String> = Vec::new();

        for dr in &results {
            if !needs_rebuild(&dr.status) {
                continue;
            }

            // Dev deps don't create build-order edges (they're only
            // needed for %check, not the actual RPM build), but we
            // still expand them transitively below.
            if dr.dep.kind != "dev" {
                rebuild_deps_of_crate.push(dr.dep.name.clone());
            }

            if visited.contains(&dr.dep.name) {
                continue;
            }
            visited.insert(dr.dep.name.clone());

            all_missing.insert(dr.dep.name.clone());
            result.push(TransitiveDep {
                name: dr.dep.name.clone(),
                version: version.clone(),
                version_req: dr.dep.version_req.clone(),
                pulled_by: crate_name.clone(),
            });
            queue.push_back(dr.dep.name.clone());
        }

        pending_edges.push((crate_name, rebuild_deps_of_crate));
    }

    // Build final edges: only include deps that are in all_missing.
    for (parent, deps) in pending_edges {
        let dep_set: BTreeSet<String> = deps
            .into_iter()
            .filter(|d| all_missing.contains(d))
            .collect();
        edges.insert(parent, dep_set);
    }
    // Ensure all missing crates have an entry (even if no missing deps).
    for name in &all_missing {
        edges.entry(name.clone()).or_default();
    }

    if opts.verbose && !result.is_empty() {
        eprintln!(
            "[check-crate] found {} transitive missing dependencies",
            result.len()
        );
    }

    Ok((result, edges))
}

/// crates.io API response for crate info.
#[derive(Deserialize)]
struct CrateInfoResponse {
    versions: Vec<CrateVersion>,
}

#[derive(Deserialize)]
struct CrateVersion {
    num: String,
    yanked: bool,
}

/// crates.io API response for dependencies.
#[derive(Deserialize)]
struct DepsResponse {
    dependencies: Vec<RawDep>,
}

#[derive(Deserialize)]
struct RawDep {
    crate_id: String,
    req: String,
    kind: Option<String>,
    optional: bool,
}

/// Fetch all non-yanked versions of a crate from crates.io.
async fn fetch_versions(name: &str) -> Result<Vec<String>, String> {
    let url = format!("https://crates.io/api/v1/crates/{name}");
    let client = reqwest::Client::builder()
        .user_agent("sandogasa-ebranch")
        .build()
        .map_err(|e| format!("failed to create HTTP client: {e}"))?;
    let resp: CrateInfoResponse = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("failed to fetch crate info: {e}"))?
        .error_for_status()
        .map_err(|e| format!("crates.io error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("failed to parse crate info: {e}"))?;

    Ok(resp
        .versions
        .into_iter()
        .filter(|v| !v.yanked)
        .map(|v| v.num)
        .collect())
}

/// Fetch the latest non-yanked version of a crate from crates.io.
async fn fetch_latest_version(name: &str) -> Result<String, String> {
    let versions = fetch_versions(name).await?;
    versions
        .into_iter()
        .next()
        .ok_or_else(|| format!("no non-yanked versions found for {name}"))
}

/// Resolve a partial version string to the best matching version.
///
/// - `"57"` matches the highest `57.x.y`
/// - `"57.3"` matches the highest `57.3.y`
/// - `"57.3.0"` matches exactly, or falls back to resolve
async fn resolve_version(name: &str, partial: &str) -> Result<String, String> {
    let parts: Vec<&str> = partial.split('.').collect();
    let req_str = match parts.len() {
        1 => format!(
            ">={partial}.0.0, <{}.0.0",
            parts[0]
                .parse::<u64>()
                .map_err(|_| { format!("invalid version: {partial}") })?
                + 1
        ),
        2 => format!(
            ">={partial}.0, <{}.{}.0",
            parts[0],
            parts[1]
                .parse::<u64>()
                .map_err(|_| { format!("invalid version: {partial}") })?
                + 1
        ),
        3 => return Ok(partial.to_string()),
        _ => return Err(format!("invalid version: {partial}")),
    };

    let req = semver::VersionReq::parse(&req_str)
        .map_err(|e| format!("invalid version range {req_str}: {e}"))?;

    let versions = fetch_versions(name).await?;
    versions
        .into_iter()
        .filter_map(|v| {
            let parsed = semver::Version::parse(&v).ok()?;
            if req.matches(&parsed) {
                Some((v, parsed))
            } else {
                None
            }
        })
        .max_by(|(_, a), (_, b)| a.cmp(b))
        .map(|(v, _)| v)
        .ok_or_else(|| format!("no version matching {partial} found for {name}"))
}

/// Fetch the dependency list for a specific crate version.
async fn fetch_dependencies(name: &str, version: &str) -> Result<Vec<CrateDep>, String> {
    let url = format!("https://crates.io/api/v1/crates/{name}/{version}/dependencies");
    let client = reqwest::Client::builder()
        .user_agent("sandogasa-ebranch")
        .build()
        .map_err(|e| format!("failed to create HTTP client: {e}"))?;
    let resp: DepsResponse = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("failed to fetch dependencies: {e}"))?
        .error_for_status()
        .map_err(|e| format!("crates.io error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("failed to parse dependencies: {e}"))?;

    Ok(resp
        .dependencies
        .into_iter()
        .map(|d| CrateDep {
            name: d.crate_id,
            version_req: d.req,
            kind: d.kind.unwrap_or_else(|| "normal".to_string()),
            optional: d.optional,
        })
        .collect())
}

/// Check if a dependency is available in the target repo and if
/// the version satisfies the requirement.
fn check_dep_in_repo(fedrq: &sandogasa_fedrq::Fedrq, dep: &CrateDep) -> DepStatus {
    let provide_name = format!("crate({})", dep.name);

    // Query fedrq for packages that provide this crate capability.
    let provides = fedrq
        .provides_of_provider(&provide_name)
        .unwrap_or_default();

    // Parse the provided version from output like "crate(foo) = 1.2.3".
    let version = extract_crate_version(&provides, &dep.name);

    let Some(version_str) = version else {
        return DepStatus::Missing;
    };

    // Parse with semver.
    let Ok(version) = semver::Version::parse(&version_str) else {
        return DepStatus::Missing;
    };

    let Ok(req) = semver::VersionReq::parse(&dep.version_req) else {
        // Can't parse the requirement — treat as satisfied to avoid
        // false positives.
        return DepStatus::Satisfied {
            version: version_str,
        };
    };

    if req.matches(&version) {
        DepStatus::Satisfied {
            version: version_str,
        }
    } else {
        DepStatus::TooOld {
            have: version_str,
            need: dep.version_req.clone(),
        }
    }
}

/// Extract the version from fedrq provides output for a crate.
///
/// Looks for a line like `crate(foo) = 1.2.3` (without feature
/// suffix) and returns the version string.
fn extract_crate_version(provides: &[String], crate_name: &str) -> Option<String> {
    let prefix = format!("crate({crate_name}) = ");
    provides
        .iter()
        .find_map(|line| line.strip_prefix(&prefix).map(|v| v.trim().to_string()))
}

// ---- Tests ----

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_version_basic() {
        let provides = vec![
            "crate(tokio/default) = 1.51.0".to_string(),
            "crate(tokio) = 1.51.0".to_string(),
            "rust-tokio+default-devel = 1.51.0-1.el9".to_string(),
        ];
        assert_eq!(
            extract_crate_version(&provides, "tokio"),
            Some("1.51.0".to_string())
        );
    }

    #[test]
    fn extract_version_missing() {
        let provides = vec!["crate(other) = 1.0.0".to_string()];
        assert_eq!(extract_crate_version(&provides, "tokio"), None);
    }

    #[test]
    fn extract_version_empty() {
        assert_eq!(extract_crate_version(&[], "tokio"), None);
    }

    #[test]
    fn extract_version_ignores_features() {
        let provides = vec![
            "crate(tokio/fs) = 1.51.0".to_string(),
            "crate(tokio/net) = 1.51.0".to_string(),
            "crate(tokio) = 1.51.0".to_string(),
        ];
        assert_eq!(
            extract_crate_version(&provides, "tokio"),
            Some("1.51.0".to_string())
        );
    }

    #[test]
    fn check_dep_satisfied() {
        let version = semver::Version::parse("1.51.0").unwrap();
        let req = semver::VersionReq::parse("^1.0").unwrap();
        assert!(req.matches(&version));
    }

    #[test]
    fn check_dep_too_old() {
        let version = semver::Version::parse("0.4.9").unwrap();
        let req = semver::VersionReq::parse("^0.5.8").unwrap();
        assert!(!req.matches(&version));
    }

    #[test]
    fn check_dep_exact_match() {
        let version = semver::Version::parse("1.0.0").unwrap();
        let req = semver::VersionReq::parse("=1.0.0").unwrap();
        assert!(req.matches(&version));
    }

    fn make_opts(transitive: bool, include_dev: bool, include_optional: bool) -> CheckCrateOptions {
        CheckCrateOptions {
            branch: "rawhide".to_string(),
            repo: None,
            verbose: false,
            transitive,
            include_dev,
            include_optional,
            include_too_old: false,
            exclude: HashSet::new(),
        }
    }

    fn make_dep(name: &str, kind: &str, optional: bool) -> CrateDep {
        CrateDep {
            name: name.to_string(),
            version_req: "^1.0".to_string(),
            kind: kind.to_string(),
            optional,
        }
    }

    #[test]
    fn should_expand_normal() {
        let opts = make_opts(true, false, false);
        assert!(should_expand(&make_dep("foo", "normal", false), &opts));
    }

    #[test]
    fn should_expand_build() {
        let opts = make_opts(true, false, false);
        assert!(should_expand(&make_dep("foo", "build", false), &opts));
    }

    #[test]
    fn should_expand_dev_excluded_by_default() {
        let opts = make_opts(true, false, false);
        assert!(!should_expand(&make_dep("foo", "dev", false), &opts));
    }

    #[test]
    fn should_expand_dev_when_included() {
        let opts = make_opts(true, true, false);
        assert!(should_expand(&make_dep("foo", "dev", false), &opts));
    }

    #[test]
    fn should_expand_optional_excluded_by_default() {
        let opts = make_opts(true, false, false);
        assert!(!should_expand(&make_dep("foo", "normal", true), &opts));
    }

    #[test]
    fn should_expand_optional_when_included() {
        let opts = make_opts(true, false, true);
        assert!(should_expand(&make_dep("foo", "normal", true), &opts));
    }
}
