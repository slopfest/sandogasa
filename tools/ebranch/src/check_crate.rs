// SPDX-License-Identifier: Apache-2.0 OR MIT

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
    pub branch: Option<String>,
    pub repo: Option<String>,
    /// Human-readable label for the branch/repo combination.
    pub label: String,
    pub verbose: bool,
    pub transitive: bool,
    pub exclude_dev: bool,
    pub include_optional: bool,
    pub include_too_old: bool,
    pub exclude: HashSet<String>,
}

/// A dependency from crates.io.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrateDep {
    pub name: String,
    pub version_req: String,
    pub kind: String,
    pub optional: bool,
}

/// Status of a dependency in the target repo.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum DepStatus {
    /// The RPM provides a version that satisfies the requirement.
    #[serde(rename = "satisfied")]
    Satisfied {
        version: String,
        /// True when satisfied by a compat package, not the latest.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        compat: bool,
    },
    /// The RPM exists but no version satisfies the requirement.
    #[serde(rename = "unmet")]
    Unmet {
        available: Vec<String>,
        need: String,
    },
    /// No RPM provides this crate.
    #[serde(rename = "missing")]
    Missing,
}

/// A dependency check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepResult {
    #[serde(flatten)]
    pub dep: CrateDep,
    #[serde(flatten)]
    pub status: DepStatus,
}

/// A transitively-discovered dependency that needs action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitiveDep {
    pub name: String,
    pub package: String,
    pub status: TransitiveStatus,
    pub version: String,
    pub version_req: String,
    pub pulled_by: String,
}

/// Why a transitive dependency needs action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitiveStatus {
    /// Not available in the target repo at all.
    Missing,
    /// Available but no version satisfies the requirement.
    Unmet,
}

/// Full report for a crate check.
#[derive(Debug, Serialize, Deserialize)]
pub struct CheckCrateReport {
    pub crate_name: String,
    pub crate_version: String,
    pub package: String,
    pub branch: String,
    pub dependencies: Vec<DepResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitive_missing: Vec<TransitiveDep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitive_build_order: Vec<dag::BuildPhase>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub transitive_edges: DepEdges,
    /// Package name → Bugzilla review bug ID, populated by check-pkg-reviews.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub review_bugs: BTreeMap<String, u64>,
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
        branch: opts.branch.clone(),
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

    let (transitive_missing, transitive_build_order, transitive_edges) = if opts.transitive {
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
        (deps, phases, edges)
    } else {
        (vec![], vec![], BTreeMap::new())
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
        crate_version: version.clone(),
        package: format!("rust-{name}"),
        branch: opts.label.clone(),
        dependencies,
        transitive_missing,
        transitive_build_order,
        transitive_edges,
        review_bugs: BTreeMap::new(),
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
    let unmet: Vec<&DepResult> = report
        .dependencies
        .iter()
        .filter(|d| matches!(d.status, DepStatus::Unmet { .. }))
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

    if !unmet.is_empty() {
        print_section_header("No matching version", &unmet);
        for d in &unmet {
            if let DepStatus::Unmet { available, need } = &d.status {
                println!(
                    "  - {} {} ({}{})",
                    d.dep.name,
                    d.dep.version_req,
                    d.dep.kind,
                    opt_label(d)
                );
                println!("    available: {}, need: {need}", available.join(", "));
            }
        }
        println!();
    }

    if !satisfied.is_empty() {
        print_section_header("Satisfied", &satisfied);
        for d in &satisfied {
            if let DepStatus::Satisfied { version, compat } = &d.status {
                let compat_label = if *compat { " (compat)" } else { "" };
                println!(
                    "  - {} {} ({}{}) — {version}{compat_label}",
                    d.dep.name,
                    d.dep.version_req,
                    d.dep.kind,
                    opt_label(d)
                );
            }
        }
        println!();
    }

    if !report.transitive_missing.is_empty() {
        println!("Transitive missing ({}):", report.transitive_missing.len());
        for d in &report.transitive_missing {
            println!("  - {} {} (via {})", d.name, d.version_req, d.pulled_by);
        }
        println!();
    }

    if !report.transitive_build_order.is_empty() {
        // Build a version lookup: crate name → version_req.
        let versions: std::collections::HashMap<&str, &str> = report
            .transitive_missing
            .iter()
            .map(|d| (d.name.as_str(), d.version_req.as_str()))
            .chain(
                report
                    .dependencies
                    .iter()
                    .filter(|d| matches!(d.status, DepStatus::Missing))
                    .map(|d| (d.dep.name.as_str(), d.dep.version_req.as_str())),
            )
            .collect();

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
                if let Some(ver) = versions.get(pkg.as_str()) {
                    println!("    - rust-{pkg} {ver}");
                } else {
                    println!("    - rust-{pkg}");
                }
            }
        }
        println!();
    }

    let n_missing = unique_crate_count(&missing);
    let n_unmet = unique_crate_count(&unmet);
    let n_satisfied = unique_crate_count(&satisfied);
    if report.transitive_missing.is_empty() {
        println!("Summary: {n_missing} missing, {n_unmet} unmet, {n_satisfied} satisfied.");
    } else {
        println!(
            "Summary: {n_missing} missing (+ {} transitive), \
             {n_unmet} unmet, {n_satisfied} satisfied.",
            report.transitive_missing.len(),
        );
    }
}

/// Print the transitive dependency graph in Graphviz DOT format.
///
/// Nodes are `rust-<crate>` package names. Edges point from a
/// package to its dependencies (what must be built/reviewed first).
/// Nodes are grouped by build phase when available.
pub fn print_dot(report: &CheckCrateReport) {
    // Build a version lookup: crate name → version_req.
    let versions: std::collections::HashMap<&str, &str> = report
        .transitive_missing
        .iter()
        .map(|d| (d.name.as_str(), d.version_req.as_str()))
        .chain(
            report
                .dependencies
                .iter()
                .filter(|d| matches!(d.status, DepStatus::Missing))
                .map(|d| (d.dep.name.as_str(), d.dep.version_req.as_str())),
        )
        .collect();

    println!("digraph {{");
    println!("  rankdir=BT;");
    println!(
        "  label=\"rust-{} {} — {}\";",
        report.crate_name, report.crate_version, report.branch
    );
    println!("  labelloc=t;");
    println!("  node [shape=box, style=filled, fillcolor=lightyellow];");

    // Declare nodes with version labels.
    for (name, ver) in &versions {
        println!("  \"rust-{name}\" [label=\"rust-{name}\\n{ver}\"];");
    }

    // Group nodes by phase for visual clarity.
    if !report.transitive_build_order.is_empty() {
        for phase in &report.transitive_build_order {
            println!("  {{ rank=same;");
            for pkg in &phase.packages {
                println!("    \"rust-{pkg}\";");
            }
            println!("  }}");
        }
    }

    // Root crate as a distinct node.
    println!(
        "  \"rust-{}\" [label=\"rust-{}\\n{}\", fillcolor=lightblue];",
        report.crate_name, report.crate_name, report.crate_version
    );

    // Edges: package → dependency (dep must be built first).
    for (parent, deps) in &report.transitive_edges {
        for dep in deps {
            println!("  \"rust-{parent}\" -> \"rust-{dep}\";");
        }
    }

    // Direct missing deps connect to the root crate.
    for dr in &report.dependencies {
        if matches!(dr.status, DepStatus::Missing)
            && report.transitive_edges.contains_key(&dr.dep.name)
        {
            println!(
                "  \"rust-{}\" -> \"rust-{}\";",
                report.crate_name, dr.dep.name
            );
        }
    }

    println!("}}");
}

/// Write the report to a TOML file.
///
/// Uses serde_json as an intermediate format to avoid issues with
/// `#[serde(flatten)]` and `#[serde(tag)]` in the TOML crate.
pub fn write_toml(report: &CheckCrateReport, path: &str) -> Result<(), String> {
    let json_value: serde_json::Value =
        serde_json::to_value(report).map_err(|e| format!("serialization failed: {e}"))?;
    let content =
        toml::to_string_pretty(&json_value).map_err(|e| format!("TOML conversion failed: {e}"))?;
    std::fs::write(path, content).map_err(|e| format!("failed to write {path}: {e}"))?;
    eprintln!("Wrote analysis to {path}");
    Ok(())
}

/// Load a report from a TOML file.
#[allow(dead_code)] // used by review_deps
pub fn load_report(path: &str) -> Result<CheckCrateReport, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    let json_value: serde_json::Value =
        toml::from_str(&content).map_err(|e| format!("failed to parse TOML: {e}"))?;
    serde_json::from_value(json_value).map_err(|e| format!("failed to deserialize report: {e}"))
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
pub type DepEdges = BTreeMap<String, BTreeSet<String>>;

/// Whether a dependency should be expanded in transitive mode.
fn should_expand(dep: &CrateDep, opts: &CheckCrateOptions) -> bool {
    if dep.optional && !opts.include_optional {
        return false;
    }
    match dep.kind.as_str() {
        "normal" | "build" => true,
        "dev" => !opts.exclude_dev,
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
    // Resolved versions: crate name → latest version from crates.io.
    let mut resolved_versions: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // All missing crate names (direct + transitive) for edge filtering.
    let mut all_missing: HashSet<String> = HashSet::new();
    // edges[A] = {B, C} means A depends on missing crates B and C.
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Deferred edge recording: (parent_crate, Vec<missing_dep_names>).
    let mut pending_edges: Vec<(String, Vec<String>)> = Vec::new();

    let needs_rebuild = |status: &DepStatus| -> bool {
        matches!(status, DepStatus::Missing)
            || (opts.include_too_old && matches!(status, DepStatus::Unmet { .. }))
    };

    // Seed: direct deps that need (re)building and pass the kind filter.
    // Queue entries: (crate_name, version_req from parent).
    let mut queue: VecDeque<(String, String)> = VecDeque::new();
    for dr in direct_results {
        let excluded = visited.contains(&dr.dep.name);
        visited.insert(dr.dep.name.clone());
        if !excluded && needs_rebuild(&dr.status) && should_expand(&dr.dep, opts) {
            all_missing.insert(dr.dep.name.clone());
            queue.push_back((dr.dep.name.clone(), dr.dep.version_req.clone()));
        }
    }

    while let Some((crate_name, version_req)) = queue.pop_front() {
        if opts.verbose {
            eprintln!("[check-crate] expanding transitive deps for {crate_name}");
        }

        let version = match rt.block_on(resolve_matching_version(&crate_name, &version_req)) {
            Ok(v) => v,
            Err(e) => {
                if opts.verbose {
                    eprintln!("[check-crate] warning: failed to fetch {crate_name}: {e}");
                }
                continue;
            }
        };
        resolved_versions.insert(crate_name.clone(), version.clone());

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

            let status = if matches!(dr.status, DepStatus::Missing) {
                TransitiveStatus::Missing
            } else {
                TransitiveStatus::Unmet
            };
            all_missing.insert(dr.dep.name.clone());
            result.push(TransitiveDep {
                name: dr.dep.name.clone(),
                package: format!("rust-{}", dr.dep.name),
                status,
                version: String::new(),
                version_req: dr.dep.version_req.clone(),
                pulled_by: crate_name.clone(),
            });
            queue.push_back((dr.dep.name.clone(), dr.dep.version_req.clone()));
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

    // Fill in resolved versions for transitive deps.
    for dep in &mut result {
        if let Some(ver) = resolved_versions.get(&dep.name) {
            dep.version = ver.clone();
        }
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

/// crates.io API response for version info (features).
#[derive(Deserialize)]
struct VersionInfoResponse {
    version: VersionInfo,
}

#[derive(Deserialize)]
struct VersionInfo {
    #[serde(default)]
    features: std::collections::HashMap<String, Vec<String>>,
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

/// Resolve default features into a set of optional dep names
/// that are activated by default.
///
/// In Cargo, enabling an optional dep `foo` implicitly creates a
/// feature named `foo`. A feature can also list `dep:foo` to
/// activate a dep. We follow both forms transitively from `default`.
fn resolve_default_deps(
    features: &std::collections::HashMap<String, Vec<String>>,
    all_optional_deps: &HashSet<String>,
) -> HashSet<String> {
    let mut activated = HashSet::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    let mut visited_features: HashSet<&str> = HashSet::new();

    if let Some(defaults) = features.get("default") {
        for f in defaults {
            queue.push_back(f.as_str());
        }
    }

    while let Some(feat) = queue.pop_front() {
        if !visited_features.insert(feat) {
            continue;
        }

        // `dep:foo` syntax explicitly activates optional dep foo.
        if let Some(dep_name) = feat.strip_prefix("dep:") {
            activated.insert(dep_name.to_string());
            continue;
        }

        // A feature named after an optional dep activates it.
        if all_optional_deps.contains(feat) {
            activated.insert(feat.to_string());
        }

        // Recurse into sub-features.
        if let Some(sub) = features.get(feat) {
            for s in sub {
                // Handle "feat/subfeat" (feature of a dep) — the
                // dep part before the slash is what gets activated.
                let base = s.split('/').next().unwrap_or(s);
                queue.push_back(base);
            }
        }
    }

    activated
}

/// Fetch version info (features) for a specific crate version.
async fn fetch_features(
    name: &str,
    version: &str,
) -> Result<std::collections::HashMap<String, Vec<String>>, String> {
    let url = format!("https://crates.io/api/v1/crates/{name}/{version}");
    let client = reqwest::Client::builder()
        .user_agent("sandogasa-ebranch")
        .build()
        .map_err(|e| format!("failed to create HTTP client: {e}"))?;
    let resp: VersionInfoResponse = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("failed to fetch version info: {e}"))?
        .error_for_status()
        .map_err(|e| format!("crates.io error: {e}"))?
        .json()
        .await
        .map_err(|e| format!("failed to parse version info: {e}"))?;

    Ok(resp.version.features)
}

/// Find the highest version of a crate matching a semver requirement.
///
/// Falls back to the latest version if the requirement can't be parsed.
async fn resolve_matching_version(name: &str, version_req: &str) -> Result<String, String> {
    let versions = fetch_versions(name).await?;

    if let Ok(req) = semver::VersionReq::parse(version_req) {
        let matched = versions
            .iter()
            .filter_map(|v| {
                let parsed = semver::Version::parse(v).ok()?;
                if req.matches(&parsed) {
                    Some((v.clone(), parsed))
                } else {
                    None
                }
            })
            .max_by(|(_, a), (_, b)| a.cmp(b))
            .map(|(v, _)| v);

        if let Some(v) = matched {
            return Ok(v);
        }
    }

    // Fallback: latest non-yanked version.
    versions
        .into_iter()
        .next()
        .ok_or_else(|| format!("no versions found for {name}"))
}

/// Fetch the dependency list for a specific crate version.
///
/// Resolves default features to mark optional deps activated by
/// defaults as non-optional (since RPMs are built with defaults).
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

    // Resolve default features to find which optional deps are
    // activated by default (RPMs are built with default features).
    let all_optional: HashSet<String> = resp
        .dependencies
        .iter()
        .filter(|d| d.optional)
        .map(|d| d.crate_id.clone())
        .collect();

    let default_activated = if all_optional.is_empty() {
        HashSet::new()
    } else {
        let features = fetch_features(name, version).await.unwrap_or_default();
        resolve_default_deps(&features, &all_optional)
    };

    Ok(resp
        .dependencies
        .into_iter()
        .map(|d| {
            let activated = d.optional && default_activated.contains(&d.crate_id);
            CrateDep {
                name: d.crate_id,
                version_req: d.req,
                kind: d.kind.unwrap_or_else(|| "normal".to_string()),
                optional: d.optional && !activated,
            }
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

    // Extract all provided versions (multiple packages may provide
    // different versions, e.g. rust-rand and rust-rand0.9).
    let versions = extract_crate_versions(&provides, &dep.name);

    if versions.is_empty() {
        return DepStatus::Missing;
    }

    let Ok(req) = semver::VersionReq::parse(&dep.version_req) else {
        // Can't parse the requirement — treat as satisfied to avoid
        // false positives.
        return DepStatus::Satisfied {
            version: versions[0].clone(),
            compat: false,
        };
    };

    // Find the highest version across all providers.
    let latest = versions
        .iter()
        .filter_map(|v| semver::Version::parse(v).ok().map(|p| (v.as_str(), p)))
        .max_by(|(_, a), (_, b)| a.cmp(b))
        .map(|(s, _)| s);

    // Check if any provided version satisfies the requirement.
    for ver_str in &versions {
        if let Ok(ver) = semver::Version::parse(ver_str)
            && req.matches(&ver)
        {
            let is_compat = latest.is_some_and(|l| l != ver_str);
            return DepStatus::Satisfied {
                version: ver_str.clone(),
                compat: is_compat,
            };
        }
    }

    DepStatus::Unmet {
        available: versions,
        need: dep.version_req.clone(),
    }
}

/// Extract all versions from fedrq provides output for a crate.
///
/// Looks for lines like `crate(foo) = 1.2.3` (without feature
/// suffix) and returns all version strings.
fn extract_crate_versions(provides: &[String], crate_name: &str) -> Vec<String> {
    let prefix = format!("crate({crate_name}) = ");
    provides
        .iter()
        .filter_map(|line| line.strip_prefix(&prefix).map(|v| v.trim().to_string()))
        .collect()
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
            extract_crate_versions(&provides, "tokio"),
            vec!["1.51.0".to_string()]
        );
    }

    #[test]
    fn extract_version_missing() {
        let provides = vec!["crate(other) = 1.0.0".to_string()];
        assert!(extract_crate_versions(&provides, "tokio").is_empty());
    }

    #[test]
    fn extract_version_empty() {
        assert!(extract_crate_versions(&[], "tokio").is_empty());
    }

    #[test]
    fn extract_version_ignores_features() {
        let provides = vec![
            "crate(tokio/fs) = 1.51.0".to_string(),
            "crate(tokio/net) = 1.51.0".to_string(),
            "crate(tokio) = 1.51.0".to_string(),
        ];
        assert_eq!(
            extract_crate_versions(&provides, "tokio"),
            vec!["1.51.0".to_string()]
        );
    }

    #[test]
    fn extract_version_multiple_providers() {
        let provides = vec![
            "crate(rand) = 0.10.0".to_string(),
            "crate(rand) = 0.9.2".to_string(),
            "crate(rand) = 0.8.5".to_string(),
        ];
        assert_eq!(
            extract_crate_versions(&provides, "rand"),
            vec![
                "0.10.0".to_string(),
                "0.9.2".to_string(),
                "0.8.5".to_string()
            ]
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

    fn make_opts(transitive: bool, exclude_dev: bool, include_optional: bool) -> CheckCrateOptions {
        CheckCrateOptions {
            branch: Some("rawhide".to_string()),
            repo: None,
            label: "rawhide".to_string(),
            verbose: false,
            transitive,
            exclude_dev,
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
    fn should_expand_dev_included_by_default() {
        let opts = make_opts(true, false, false);
        assert!(should_expand(&make_dep("foo", "dev", false), &opts));
    }

    #[test]
    fn should_expand_dev_excluded_when_requested() {
        let opts = make_opts(true, true, false);
        assert!(!should_expand(&make_dep("foo", "dev", false), &opts));
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

    #[test]
    fn resolve_default_deps_basic() {
        let features = std::collections::HashMap::from([
            (
                "default".to_string(),
                vec!["write".to_string(), "parse".to_string()],
            ),
            (
                "write".to_string(),
                vec![
                    "dep:lexical-write-integer".to_string(),
                    "dep:lexical-write-float".to_string(),
                ],
            ),
            ("parse".to_string(), vec!["dep:lexical-parse".to_string()]),
        ]);
        let optional = HashSet::from([
            "lexical-write-integer".to_string(),
            "lexical-write-float".to_string(),
            "lexical-parse".to_string(),
            "serde".to_string(),
        ]);
        let activated = resolve_default_deps(&features, &optional);
        assert!(activated.contains("lexical-write-integer"));
        assert!(activated.contains("lexical-write-float"));
        assert!(activated.contains("lexical-parse"));
        assert!(!activated.contains("serde"));
    }

    #[test]
    fn resolve_default_deps_implicit_feature() {
        // Optional dep `foo` implicitly creates feature `foo`.
        let features =
            std::collections::HashMap::from([("default".to_string(), vec!["foo".to_string()])]);
        let optional = HashSet::from(["foo".to_string(), "bar".to_string()]);
        let activated = resolve_default_deps(&features, &optional);
        assert!(activated.contains("foo"));
        assert!(!activated.contains("bar"));
    }

    #[test]
    fn resolve_default_deps_no_defaults() {
        let features = std::collections::HashMap::new();
        let optional = HashSet::from(["foo".to_string()]);
        let activated = resolve_default_deps(&features, &optional);
        assert!(activated.is_empty());
    }

    #[test]
    fn toml_round_trip() {
        let report = CheckCrateReport {
            crate_name: "test-crate".to_string(),
            crate_version: "1.0.0".to_string(),
            package: "rust-test-crate".to_string(),
            branch: "rawhide".to_string(),
            dependencies: vec![
                DepResult {
                    dep: CrateDep {
                        name: "serde".to_string(),
                        version_req: "^1.0".to_string(),
                        kind: "normal".to_string(),
                        optional: false,
                    },
                    status: DepStatus::Satisfied {
                        version: "1.0.210".to_string(),
                        compat: false,
                    },
                },
                DepResult {
                    dep: CrateDep {
                        name: "missing-dep".to_string(),
                        version_req: "^0.5".to_string(),
                        kind: "normal".to_string(),
                        optional: false,
                    },
                    status: DepStatus::Missing,
                },
                DepResult {
                    dep: CrateDep {
                        name: "old-dep".to_string(),
                        version_req: "^2.0".to_string(),
                        kind: "dev".to_string(),
                        optional: false,
                    },
                    status: DepStatus::Unmet {
                        available: vec!["1.5.0".to_string()],
                        need: "^2.0".to_string(),
                    },
                },
            ],
            transitive_missing: vec![TransitiveDep {
                name: "transitive-dep".to_string(),
                package: "rust-transitive-dep".to_string(),
                status: TransitiveStatus::Missing,
                version: "0.3.0".to_string(),
                version_req: "^0.3".to_string(),
                pulled_by: "missing-dep".to_string(),
            }],
            transitive_build_order: vec![dag::BuildPhase {
                phase: 1,
                packages: vec!["transitive-dep".to_string(), "missing-dep".to_string()],
            }],
            transitive_edges: BTreeMap::from([
                (
                    "missing-dep".to_string(),
                    BTreeSet::from(["transitive-dep".to_string()]),
                ),
                ("transitive-dep".to_string(), BTreeSet::new()),
            ]),
            review_bugs: BTreeMap::new(),
        };

        // Serialize via JSON intermediate to TOML string.
        let json_value = serde_json::to_value(&report).unwrap();
        let toml_str = toml::to_string_pretty(&json_value).unwrap();

        // Deserialize back via JSON intermediate.
        let parsed_value: serde_json::Value = toml::from_str(&toml_str).unwrap();
        let parsed: CheckCrateReport = serde_json::from_value(parsed_value).unwrap();

        assert_eq!(parsed.crate_name, "test-crate");
        assert_eq!(parsed.crate_version, "1.0.0");
        assert_eq!(parsed.dependencies.len(), 3);
        assert_eq!(parsed.transitive_missing.len(), 1);
        assert_eq!(parsed.transitive_missing[0].name, "transitive-dep");
        assert_eq!(parsed.transitive_build_order.len(), 1);
        assert_eq!(parsed.transitive_edges.len(), 2);
        assert!(parsed.transitive_edges["missing-dep"].contains("transitive-dep"));
    }
}
