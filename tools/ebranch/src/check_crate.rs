// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Analyze a crates.io crate's dependencies against a target RPM repo.
//!
//! Fetches the dependency list from crates.io, then checks each dependency
//! against the target repo to determine if it is available as an RPM,
//! whether the available version satisfies the crate's version requirement,
//! or if it is missing entirely.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::dag;

// ---- Public types ----

/// Options for the check-crate command.
#[derive(Clone)]
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
    /// Bypass the on-disk crates.io cache (re-fetch and re-store).
    pub refresh: bool,
    /// Non-default features the Fedora build enables for an
    /// application root (`%cargo_generate_buildrequires -f`). Empty:
    /// read them from the package's rawhide spec when it exists.
    pub features: Vec<String>,
    /// Build without default features (`-n`).
    pub no_default_features: bool,
    /// Crates built in-tree from the root's own source (a workspace's
    /// members, `uu_*` for uutils-coreutils): not dependencies Fedora
    /// packages, but their dependencies are the workspace's. Globs,
    /// trusted as written, plus [`IN_TREE_REPOSITORY`] to also take
    /// every crate published from the root's repository. Empty:
    /// nothing is in-tree.
    pub in_tree: Vec<String>,
    /// A staging COPR (`owner/project`, `--staging-copr`) layered over
    /// the branch: what the branch does not satisfy is looked up there
    /// too, and counts as staged — built, still in flight — rather
    /// than missing.
    pub copr: Option<String>,
    /// The Fedora package name when it is not `rust-<crate>`
    /// (`coreutils` → `uutils-coreutils`): used for the spec lookup
    /// and as the report's package name.
    pub package: Option<String>,
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
        /// True when only the staging COPR (`--copr`) provides it:
        /// built, but not yet in the branch — still in flight.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        staged: bool,
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
    /// The in-tree crate this dependency was found under, when it is
    /// not the root's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// Where a crate version is already built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltIn {
    /// The branch itself provides it.
    Branch,
    /// Only the staging COPR provides it: built, not yet landed.
    Copr,
}

/// A crate built from the root's own source tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InTreeCrate {
    pub name: String,
    pub version_req: String,
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
    /// Built in the staging COPR (`--staging-copr`), not yet in the
    /// branch: nothing to build, still in flight.
    Staged,
}

/// Full report for a crate check.
#[derive(Debug, Serialize, Deserialize)]
pub struct CheckCrateReport {
    pub crate_name: String,
    pub crate_version: String,
    pub package: String,
    pub branch: String,
    /// The staging COPR layered over the branch, when one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copr: Option<String>,
    /// Where this very version of the crate is already built, if it is:
    /// the report is then about a rebuild, not a first build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub already_built: Option<BuiltIn>,
    pub dependencies: Vec<DepResult>,
    /// Workspace members: built in-tree, not packaged; their
    /// dependencies appear in `dependencies` with `via` set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub in_tree: Vec<InTreeCrate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitive_missing: Vec<TransitiveDep>,
    /// Transitive dependencies the staging COPR provides: built there,
    /// not yet in the branch, and not expanded further.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitive_staged: Vec<TransitiveDep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitive_build_order: Vec<dag::BuildPhase>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub transitive_edges: DepEdges,
    /// Package name → Bugzilla review bug ID, populated by check-pkg-reviews.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub review_bugs: BTreeMap<String, u64>,
}

impl CheckCrateReport {
    /// Build phases including the root crate as the final phase.
    ///
    /// The stored `transitive_build_order` only contains transitive
    /// deps. This appends the root crate itself so the result is a
    /// complete build sequence for koji/copr/human output.
    pub fn full_build_phases(&self) -> Vec<dag::BuildPhase> {
        let mut phases = self.transitive_build_order.clone();
        let next = phases.last().map_or(1, |p| p.phase + 1);
        phases.push(dag::BuildPhase {
            phase: next,
            packages: vec![self.crate_name.clone()],
        });
        phases
    }
}

// ---- Public functions ----

/// Run the check-crate analysis.
pub fn check_crate(
    name: &str,
    version: Option<&str>,
    opts: &CheckCrateOptions,
) -> Result<CheckCrateReport, String> {
    init_cache(opts.refresh);
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

    // Fedora builds an application with its default features and a
    // library with all of them, so a library root's optional deps are
    // as required as any other (same for every transitive crate).
    let library_root = !rt.block_on(is_application(name, &version));
    // An application's Fedora build may enable more than the
    // defaults: --features says which; otherwise the rawhide spec's
    // own %cargo_generate_buildrequires line does, when the package
    // already exists.
    let mut seeds: Vec<String> = Vec::new();
    let mut spec_all = false;
    let mut source = "defaults";
    let spec_label;
    if library_root {
        seeds.push("default".to_string());
    } else if !opts.features.is_empty() || opts.no_default_features {
        if !opts.no_default_features {
            seeds.push("default".to_string());
        }
        seeds.extend(opts.features.iter().cloned());
        source = "--features";
    } else if let Some((pkg, sf)) =
        rt.block_on(spec_features_from_rawhide(name, opts.package.as_deref()))
    {
        if !sf.no_default {
            seeds.push("default".to_string());
        }
        seeds.extend(sf.features.iter().cloned());
        spec_all = sf.all;
        spec_label = format!("{pkg}.spec (rawhide)");
        source = &spec_label;
    } else {
        seeds.push("default".to_string());
    }
    if opts.verbose {
        eprintln!(
            "[check-crate] {name} is {}",
            match (library_root, spec_all) {
                (true, _) => "a library: every feature counts, optional deps included".to_string(),
                (false, true) =>
                    format!("an application built with all features (-a, from {source})"),
                (false, false) => format!(
                    "an application: features {} (from {source})",
                    seeds.join(",")
                ),
            }
        );
    }
    let (deps, dep_features) =
        rt.block_on(fetch_dependencies_with_features(name, &version, &seeds))?;
    // Excluded crates are ignored outright — direct or transitive —
    // as if they were not dependencies: Fedora drops them.
    let (deps, ignored): (Vec<CrateDep>, Vec<CrateDep>) = deps
        .into_iter()
        .partition(|d| !opts.exclude.contains(&d.name));
    if opts.verbose && !ignored.is_empty() {
        let names: Vec<&str> = ignored.iter().map(|d| d.name.as_str()).collect();
        eprintln!("[check-crate] ignoring excluded: {}", names.join(", "));
    }

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

    // A staging COPR changes by the minute: refetch its metadata every
    // run (it is small), while the branch's stays cached as usual.
    if let Some(c) = &opts.copr
        && let Some((owner, project)) = c.split_once('/')
        && let Some(branch) = &opts.branch
    {
        match sandogasa_fedrq::expire_repo_cache(branch, &sandogasa_copr::repoid(owner, project)) {
            Ok(n) if opts.verbose => {
                eprintln!("[check-crate] expired {n} cached metadata file(s) of COPR {c}")
            }
            Err(e) => eprintln!("warning: could not expire the COPR's cached metadata: {e}"),
            _ => {}
        }
    }
    let repos = RepoStack {
        base: sandogasa_fedrq::Fedrq {
            branch: opts.branch.clone(),
            repo: opts.repo.clone(),
        },
        copr: opts.copr.as_ref().map(|c| sandogasa_fedrq::Fedrq {
            branch: opts.branch.clone(),
            repo: Some(format!("@copr:{c}")),
        }),
    };

    if opts.verbose {
        match &opts.copr {
            Some(c) => eprintln!(
                "[check-crate] checking dependencies against repo, then COPR {c} for the rest"
            ),
            None => eprintln!("[check-crate] checking dependencies against repo"),
        }
    }

    // Is this very version already built somewhere? Then the report is
    // about a rebuild — say so before listing what a build would need.
    let root = CrateDep {
        name: name.to_string(),
        version_req: format!("={version}"),
        kind: "normal".to_string(),
        optional: false,
    };
    let already_built = match repos.check(&[&root]).remove(0) {
        DepStatus::Satisfied { staged: true, .. } => Some(BuiltIn::Copr),
        DepStatus::Satisfied { .. } => Some(BuiltIn::Branch),
        _ => None,
    };
    if opts.verbose
        && let Some(where_) = already_built
    {
        eprintln!(
            "[check-crate] {name} {version} is already built: {}",
            built_label(where_, opts)
        );
    }

    // One fedrq invocation for every direct dependency.
    let statuses = repos.check(&deps.iter().collect::<Vec<_>>());
    let mut dependencies: Vec<DepResult> = deps
        .iter()
        .cloned()
        .zip(statuses)
        .map(|(dep, status)| DepResult {
            dep,
            status,
            via: None,
        })
        .collect();

    // Workspace members are built from the root's own tree, not
    // packaged: they leave the dependency lists, and their own
    // dependencies join them as the workspace's — with the features
    // the root's enabled set requests of them (`uu_ls/selinux`).
    // Sharing the root's repository is only a hint, taken on request:
    // phf's workspace publishes phf_shared and phf_macros as crates
    // Fedora packages separately, while uutils' members are built from
    // the coreutils tarball. The spec knows; crates.io does not — and
    // neither does the repo: rawhide still carries stale rust-uu_*
    // packages nobody retired, so the glob is trusted as written.
    let root_repo = in_tree_repository_rule(&opts.in_tree)
        .then(|| rt.block_on(fetch_repository(name)))
        .flatten();
    let is_in_tree = |dep: &str| -> bool {
        matches_in_tree_glob(&opts.in_tree, dep)
            || (root_repo.is_some() && rt.block_on(fetch_repository(dep)) == root_repo)
    };
    let mut in_tree: Vec<InTreeCrate> = Vec::new();
    let mut queue: VecDeque<(String, String, Vec<String>)> = VecDeque::new();
    let mut visited_members: HashSet<String> = HashSet::new();
    let mut kept: Vec<DepResult> = Vec::new();
    let mut packaged_anyway: Vec<String> = Vec::new();
    for dr in dependencies {
        if is_in_tree(&dr.dep.name) {
            if !matches!(dr.status, DepStatus::Missing) {
                packaged_anyway.push(dr.dep.name.clone());
            }
            // A member the root lists twice (normal and dev) is one crate.
            if visited_members.insert(dr.dep.name.clone()) {
                queue.push_back((
                    dr.dep.name.clone(),
                    dr.dep.version_req.clone(),
                    dep_features.get(&dr.dep.name).cloned().unwrap_or_default(),
                ));
                in_tree.push(InTreeCrate {
                    name: dr.dep.name,
                    version_req: dr.dep.version_req,
                });
            }
        } else {
            kept.push(dr);
        }
    }
    dependencies = kept;
    if opts.verbose && !packaged_anyway.is_empty() {
        packaged_anyway.sort();
        packaged_anyway.dedup();
        eprintln!(
            "[check-crate] in-tree as told, though the repo packages them on their own \
             (an over-broad glob, or packages something else still needs): {}",
            packaged_anyway.join(", ")
        );
    }
    let mut member_deps: Vec<(CrateDep, String)> = Vec::new();
    let mut seen_member_deps: HashSet<(String, String)> = HashSet::new();
    while let Some((member, req, feats)) = queue.pop_front() {
        let Ok(mversion) = rt.block_on(resolve_matching_version(&member, &req)) else {
            if opts.verbose {
                eprintln!("[check-crate] warning: no version of in-tree {member} matches {req}");
            }
            continue;
        };
        let mut mseeds = vec!["default".to_string()];
        mseeds.extend(feats);
        let Ok((mdeps, mfeats)) = rt.block_on(fetch_dependencies_with_features(
            &member, &mversion, &mseeds,
        )) else {
            continue;
        };
        // A member is built as a dependency, never tested on its own:
        // its dev dependencies are not the workspace's.
        for d in mdeps
            .into_iter()
            .filter(|d| d.kind != "dev" && should_expand(d, opts, false))
        {
            if is_in_tree(&d.name) {
                if visited_members.insert(d.name.clone()) {
                    in_tree.push(InTreeCrate {
                        name: d.name.clone(),
                        version_req: d.version_req.clone(),
                    });
                    queue.push_back((
                        d.name.clone(),
                        d.version_req.clone(),
                        mfeats.get(&d.name).cloned().unwrap_or_default(),
                    ));
                }
                continue;
            }
            if seen_member_deps.insert((d.name.clone(), d.version_req.clone())) {
                member_deps.push((d, member.clone()));
            }
        }
    }
    if !in_tree.is_empty() {
        if opts.verbose {
            let names: Vec<&str> = in_tree.iter().map(|m| m.name.as_str()).collect();
            eprintln!(
                "[check-crate] {} crate(s) built in-tree, their {} dependencies checked as the workspace's: {}",
                in_tree.len(),
                member_deps.len(),
                names.join(", ")
            );
        }
        let refs: Vec<&CrateDep> = member_deps.iter().map(|(d, _)| d).collect();
        let statuses = repos.check(&refs);
        // A dependency the root already has directly needs no second entry.
        let direct: HashSet<(String, String)> = dependencies
            .iter()
            .map(|d| (d.dep.name.clone(), d.dep.version_req.clone()))
            .collect();
        for ((dep, via), status) in member_deps.into_iter().zip(statuses) {
            if !direct.contains(&(dep.name.clone(), dep.version_req.clone())) {
                dependencies.push(DepResult {
                    dep,
                    status,
                    via: Some(via),
                });
            }
        }
    }
    let in_tree_names: HashSet<String> = in_tree.iter().map(|m| m.name.clone()).collect();

    let (transitive_missing, transitive_staged, transitive_build_order, transitive_edges) =
        if opts.transitive {
            let mut expansion_opts = opts.clone();
            expansion_opts.exclude.extend(in_tree_names.iter().cloned());
            let (deps, staged, edges) = expand_transitive(
                &rt,
                &repos,
                &dependencies,
                &expansion_opts,
                library_root || spec_all,
            )?;
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
            (deps, staged, phases, edges)
        } else {
            (vec![], vec![], vec![], BTreeMap::new())
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

    if opts.verbose {
        use std::sync::atomic::Ordering::Relaxed;
        let c = cache();
        eprintln!(
            "[check-crate] crates.io: {} response(s) from cache, {} fetched",
            c.hits.load(Relaxed),
            c.misses.load(Relaxed)
        );
    }

    Ok(CheckCrateReport {
        crate_name: name.to_string(),
        crate_version: version.clone(),
        package: opts
            .package
            .clone()
            .unwrap_or_else(|| format!("rust-{name}")),
        branch: opts.label.clone(),
        already_built,
        dependencies,
        transitive_missing,
        transitive_staged,
        transitive_build_order,
        transitive_edges,
        review_bugs: BTreeMap::new(),
        in_tree,
        copr: opts.copr.clone(),
    })
}

/// Why a package is in a generated build script, as a shell comment.
///
/// A script is read again days later, by which point the report that came
/// with it on stderr has scrolled away — so what each package is for
/// travels in the script itself.
pub fn build_reason(pkg: &str, report: &CheckCrateReport) -> Option<String> {
    let crate_name = pkg.strip_prefix("rust-").unwrap_or(pkg);
    if crate_name == report.crate_name {
        return Some(format!(
            "# {pkg}: the crate this run is about, {}",
            report.crate_version
        ));
    }
    let dep = report
        .transitive_missing
        .iter()
        .find(|d| d.name == crate_name)?;
    let why = match dep.status {
        TransitiveStatus::Missing => "not packaged".to_string(),
        TransitiveStatus::Unmet => {
            format!("packaged, but nothing satisfies {}", dep.version_req)
        }
        // Staged crates are never in transitive_missing.
        TransitiveStatus::Staged => "staged in the COPR".to_string(),
    };
    Some(format!(
        "# {pkg}: build {} for {} — {why}, pulled in by {}",
        dep.version, dep.version_req, dep.pulled_by
    ))
}

/// Print the human-readable report to stdout.
pub fn print_report(report: &CheckCrateReport) {
    print!("{}", render_report(report));
}

/// Print the human-readable report to stderr — used alongside a machine
/// output mode (`--koji`/`--copr`/`--dot`/`--toml`) so the reviewer can
/// still see what needs building, and at which versions, while the clean
/// machine output goes to stdout for piping.
pub fn eprint_report(report: &CheckCrateReport) {
    eprint!("{}", render_report(report));
}

/// Render the human-readable report as a string (what needs building,
/// with versions, and the build order).
pub fn render_report(report: &CheckCrateReport) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Checking crate: {} {}",
        report.crate_name, report.crate_version
    );
    let _ = writeln!(out, "Branch: {}", report.branch);
    if let Some(c) = &report.copr {
        let _ = writeln!(out, "Staging COPR: {c}");
    }
    match report.already_built {
        Some(BuiltIn::Branch) => {
            let _ = writeln!(
                out,
                "Already built: {} {} is in {} — this would be a rebuild",
                report.crate_name, report.crate_version, report.branch
            );
        }
        Some(BuiltIn::Copr) => {
            let _ = writeln!(
                out,
                "Already built: {} {} is in the staging COPR, not yet in {} — nothing to \
                 build there again",
                report.crate_name, report.crate_version, report.branch
            );
        }
        None => {}
    }
    let _ = writeln!(out);

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
    let _ = writeln!(
        out,
        "Dependencies ({normal} normal, {build} build, {dev} dev):\n"
    );
    if !report.in_tree.is_empty() {
        let names: Vec<&str> = report.in_tree.iter().map(|m| m.name.as_str()).collect();
        let _ = writeln!(
            out,
            "Built in-tree ({}), dependencies listed as the workspace's:\n  {}\n",
            report.in_tree.len(),
            names.join(", ")
        );
    }

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
        .filter(|d| matches!(d.status, DepStatus::Satisfied { staged: false, .. }))
        .collect();
    let staged: Vec<&DepResult> = report
        .dependencies
        .iter()
        .filter(|d| matches!(d.status, DepStatus::Satisfied { staged: true, .. }))
        .collect();

    if !missing.is_empty() {
        write_section_header(&mut out, "Missing", &missing);
        for d in &missing {
            let _ = writeln!(
                out,
                "  - {} {} ({}{})",
                d.dep.name,
                d.dep.version_req,
                d.dep.kind,
                opt_label(d)
            );
        }
        let _ = writeln!(out);
    }

    if !unmet.is_empty() {
        write_section_header(&mut out, "No matching version", &unmet);
        for d in &unmet {
            if let DepStatus::Unmet { available, need } = &d.status {
                let _ = writeln!(
                    out,
                    "  - {} {} ({}{})",
                    d.dep.name,
                    d.dep.version_req,
                    d.dep.kind,
                    opt_label(d)
                );
                let _ = writeln!(out, "    available: {}, need: {need}", available.join(", "));
            }
        }
        let _ = writeln!(out);
    }

    let n_staged = unique_crate_count(&staged) + report.transitive_staged.len();
    if n_staged > 0 {
        let _ = writeln!(out, "Staged in COPR, not yet in the branch ({n_staged}):");
        write_satisfied_lines(&mut out, &staged);
        for d in &report.transitive_staged {
            let _ = writeln!(
                out,
                "  - {} {} (via {}) — {}",
                d.name, d.version_req, d.pulled_by, d.version
            );
        }
        let _ = writeln!(out);
    }
    if !satisfied.is_empty() {
        write_section_header(&mut out, "Satisfied", &satisfied);
        write_satisfied_lines(&mut out, &satisfied);
        let _ = writeln!(out);
    }

    if !report.transitive_missing.is_empty() {
        let _ = writeln!(
            out,
            "Transitive missing ({}):",
            report.transitive_missing.len()
        );
        for d in &report.transitive_missing {
            let _ = writeln!(
                out,
                "  - {} {} (via {})",
                d.name, d.version_req, d.pulled_by
            );
        }
        let _ = writeln!(out);
    }

    if !report.transitive_build_order.is_empty() {
        let phases = report.full_build_phases();

        let versions = missing_versions(report);

        let total: usize = phases.iter().map(|p| p.packages.len()).sum();
        let _ = writeln!(
            out,
            "Build order ({total} package(s) in {} phase(s)):",
            phases.len()
        );
        for phase in &phases {
            let _ = writeln!(out, "\n  Phase {}:", phase.phase);
            for pkg in &phase.packages {
                if let Some(ver) = versions.get(pkg.as_str()) {
                    let _ = writeln!(out, "    - rust-{pkg} {ver}");
                } else {
                    let _ = writeln!(out, "    - rust-{pkg}");
                }
            }
        }
        let _ = writeln!(out);
    }

    let n_missing = unique_crate_count(&missing);
    let n_unmet = unique_crate_count(&unmet);
    let n_satisfied = unique_crate_count(&satisfied);
    let staged_note = match n_staged {
        0 => String::new(),
        n => format!(", {n} staged in COPR"),
    };
    if report.transitive_missing.is_empty() {
        let _ = writeln!(
            out,
            "Summary: {n_missing} missing, {n_unmet} unmet, {n_satisfied} satisfied{staged_note}."
        );
    } else {
        let _ = writeln!(
            out,
            "Summary: {n_missing} missing (+ {} transitive), \
             {n_unmet} unmet, {n_satisfied} satisfied{staged_note}.",
            report.transitive_missing.len(),
        );
    }
    out
}

/// Print the transitive dependency graph in Graphviz DOT format.
///
/// Nodes are `rust-<crate>` package names. Edges point from a
/// package to its dependencies (what must be built/reviewed first).
/// Nodes are grouped by build phase when available.
pub fn print_dot(report: &CheckCrateReport) {
    let versions = missing_versions(report);

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

fn opt_label(d: &DepResult) -> String {
    let mut label = String::new();
    if d.dep.optional {
        label.push_str(", optional");
    }
    if let Some(via) = &d.via {
        label.push_str(&format!(", via {via}"));
    }
    label
}

/// Version lookup for crates that need building: crate name →
/// version_req, covering transitive-missing and direct missing deps.
fn missing_versions(report: &CheckCrateReport) -> std::collections::HashMap<&str, &str> {
    report
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
        .collect()
}

/// Count unique crate names in a list of dep results.
fn unique_crate_count(deps: &[&DepResult]) -> usize {
    let names: HashSet<&str> = deps.iter().map(|d| d.dep.name.as_str()).collect();
    names.len()
}

/// Print a section header with entry count and unique crate count.
/// One `  - name req (kind) — version` line per satisfied dependency.
fn write_satisfied_lines(out: &mut String, deps: &[&DepResult]) {
    use std::fmt::Write as _;
    for d in deps {
        if let DepStatus::Satisfied {
            version, compat, ..
        } = &d.status
        {
            let compat_label = if *compat { " (compat)" } else { "" };
            let _ = writeln!(
                out,
                "  - {} {} ({}{}) — {version}{compat_label}",
                d.dep.name,
                d.dep.version_req,
                d.dep.kind,
                opt_label(d)
            );
        }
    }
}

fn write_section_header(out: &mut String, label: &str, deps: &[&DepResult]) {
    use std::fmt::Write as _;
    let unique = unique_crate_count(deps);
    if unique == deps.len() {
        let _ = writeln!(out, "{label} ({unique}):");
    } else {
        let _ = writeln!(out, "{label} ({unique} crate(s), {} entries):", deps.len());
    }
}

/// Dependency edges: `edges[A] = {B, C}` means A depends on B and C.
pub type DepEdges = BTreeMap<String, BTreeSet<String>>;

/// Whether a dependency should be expanded in transitive mode.
/// `all_features`: the depending crate is built with every feature
/// (a library, or any crate that has to be packaged), so its optional
/// dependencies are required. Only an application root keeps its
/// optional deps out unless `--include-optional`.
fn should_expand(dep: &CrateDep, opts: &CheckCrateOptions, all_features: bool) -> bool {
    if dep.optional && !all_features && !opts.include_optional {
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
    repos: &RepoStack,
    direct_results: &[DepResult],
    opts: &CheckCrateOptions,
    library_root: bool,
) -> Result<(Vec<TransitiveDep>, Vec<TransitiveDep>, DepEdges), String> {
    let mut visited: HashSet<String> = opts.exclude.clone();
    let mut result: Vec<TransitiveDep> = Vec::new();
    // What the staging COPR provides along the way: reported, not built.
    let mut staged: Vec<TransitiveDep> = Vec::new();
    let mut staged_seen: HashSet<String> = HashSet::new();
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
        if !excluded && needs_rebuild(&dr.status) && should_expand(&dr.dep, opts, library_root) {
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

        let deps = match rt.block_on(fetch_dependencies(
            &crate_name,
            &version,
            &["default".to_string()],
        )) {
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

        // Filter to relevant kinds and check against the repo — one
        // fedrq invocation for the whole crate's dependency list.
        // A crate that must be packaged is built with every feature
        // (rust2rpm's `-a`), so all its optional deps count.
        let relevant: Vec<&CrateDep> = deps
            .iter()
            .filter(|d| should_expand(d, opts, true))
            .collect();
        let statuses = repos.check(&relevant);
        let results: Vec<DepResult> = relevant
            .iter()
            .zip(statuses)
            .map(|(dep, status)| DepResult {
                dep: (*dep).clone(),
                status,
                via: None,
            })
            .collect();

        let mut rebuild_deps_of_crate: Vec<String> = Vec::new();

        for dr in &results {
            if let DepStatus::Satisfied {
                staged: true,
                version,
                ..
            } = &dr.status
                && staged_seen.insert(dr.dep.name.clone())
            {
                staged.push(TransitiveDep {
                    name: dr.dep.name.clone(),
                    package: format!("rust-{}", dr.dep.name),
                    status: TransitiveStatus::Staged,
                    version: version.clone(),
                    version_req: dr.dep.version_req.clone(),
                    pulled_by: crate_name.clone(),
                });
            }
            if !needs_rebuild(&dr.status) {
                continue;
            }

            rebuild_deps_of_crate.push(dr.dep.name.clone());

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

    Ok((result, staged, edges))
}

/// crates.io API response for crate info.
#[derive(Deserialize)]
struct CrateInfoResponse {
    versions: Vec<CrateVersion>,
    #[serde(default, rename = "crate")]
    krate: CrateMeta,
}

#[derive(Deserialize, Default)]
struct CrateMeta {
    #[serde(default)]
    repository: Option<String>,
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
    /// `cfg(...)` expression or target triple the dependency is
    /// limited to.
    #[serde(default)]
    target: Option<String>,
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
    /// Binaries the crate ships: non-empty means an application,
    /// which Fedora builds with its default features; empty means a
    /// library, which Fedora builds with every feature.
    #[serde(default)]
    bin_names: Vec<String>,
}

/// Shared HTTP client for crates.io requests, built once.
fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        sandogasa_cli::http::builder("sandogasa-ebranch")
            .build()
            .expect("failed to create HTTP client")
    })
}

/// On-disk cache of crates.io responses, under the XDG cache dir
/// (`~/.cache/ebranch/crates-io/`). A published crate version is
/// immutable, so its dependency list and feature map are cached for
/// good; only a crate's *versions* list ages (24h). Fewer requests
/// hit crates.io and repeat runs are near-instant — the polite kind
/// of speedup. `--refresh` bypasses reads (and re-stores).
struct CratesIoCache {
    dir: Option<std::path::PathBuf>,
    refresh: bool,
    hits: std::sync::atomic::AtomicUsize,
    misses: std::sync::atomic::AtomicUsize,
}

static CACHE: std::sync::OnceLock<CratesIoCache> = std::sync::OnceLock::new();

/// Configure the cache for this run; a no-op after the first call.
pub fn init_cache(refresh: bool) {
    let _ = CACHE.set(CratesIoCache {
        dir: dirs::cache_dir().map(|d| d.join("ebranch").join("crates-io")),
        refresh,
        hits: Default::default(),
        misses: Default::default(),
    });
}

fn cache() -> &'static CratesIoCache {
    init_cache(false);
    CACHE.get().expect("cache initialized")
}

/// Versions lists age; everything about a published version does not.
const VERSIONS_TTL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

impl CratesIoCache {
    /// A cached body, if present and (when `ttl` is given) young enough.
    fn load(&self, rel: &str, ttl: Option<std::time::Duration>) -> Option<String> {
        if self.refresh {
            return None;
        }
        let path = self.dir.as_ref()?.join(rel);
        if let Some(ttl) = ttl {
            let age = std::fs::metadata(&path)
                .ok()?
                .modified()
                .ok()?
                .elapsed()
                .ok()?;
            if age > ttl {
                return None;
            }
        }
        std::fs::read_to_string(path).ok()
    }

    /// Store a body; failures are silent (a cache is a convenience).
    fn store(&self, rel: &str, body: &str) {
        let Some(dir) = &self.dir else { return };
        let path = dir.join(rel);
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_ok()
        {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, body).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

/// GET a crates.io API URL and deserialize the JSON response, through
/// the cache: `rel` is the cache path, `ttl` how long an entry may
/// serve (`None`: forever).
async fn get_json<T: serde::de::DeserializeOwned>(
    url: &str,
    rel: &str,
    ttl: Option<std::time::Duration>,
) -> Result<T, String> {
    use std::sync::atomic::Ordering::Relaxed;
    let what = format!("GET {url}");
    let c = cache();
    if let Some(body) = c.load(rel, ttl)
        && let Ok(parsed) = serde_json::from_str::<T>(&body)
    {
        c.hits.fetch_add(1, Relaxed);
        return Ok(parsed);
    }
    c.misses.fetch_add(1, Relaxed);
    let resp = client()
        .get(url)
        .send()
        .await
        .map_err(|e| format!("{what}: {e}"))?;
    let body = sandogasa_cli::http::ok(resp, &what)
        .await?
        .text()
        .await
        .map_err(|e| format!("{what}: {e}"))?;
    let parsed = serde_json::from_str::<T>(&body).map_err(|e| format!("{what}: {e}"))?;
    c.store(rel, &body);
    Ok(parsed)
}

/// The crate's info page (versions, repository) — cached a day.
async fn fetch_crate_info(name: &str) -> Result<CrateInfoResponse, String> {
    let url = format!("https://crates.io/api/v1/crates/{name}");
    get_json(&url, &format!("{name}/versions.json"), Some(VERSIONS_TTL)).await
}

/// The repository a crate is published from, normalized for
/// comparison (scheme-less, no trailing slash or `.git`, and cut at
/// the in-tree path a workspace member points into:
/// `…/coreutils/tree/main/src/uu/ls` is the coreutils repository).
async fn fetch_repository(name: &str) -> Option<String> {
    fetch_crate_info(name)
        .await
        .ok()?
        .krate
        .repository
        .map(|r| normalize_repository(&r))
}

fn normalize_repository(url: &str) -> String {
    let mut r = url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .to_ascii_lowercase();
    for marker in ["/-/tree/", "/tree/", "/blob/"] {
        if let Some(i) = r.find(marker) {
            r.truncate(i);
        }
    }
    r.trim_end_matches('/').trim_end_matches(".git").to_string()
}

/// Whether a dependency's `target` (a `cfg(...)` expression or a
/// target triple) applies to a Linux build, as
/// `%cargo_generate_buildrequires` decides it: `windows-sys` behind
/// `cfg(windows)` is not something Fedora has to package. Predicates
/// the build does not fix (architecture, pointer width) count as
/// true, so a dependency is only dropped when it cannot apply.
fn target_applies(target: Option<&str>) -> bool {
    let Some(t) = target.map(str::trim).filter(|t| !t.is_empty()) else {
        return true;
    };
    match t.strip_prefix("cfg(").and_then(|e| e.strip_suffix(')')) {
        Some(expr) => eval_cfg(expr),
        None => t.contains("linux"),
    }
}

fn eval_cfg(expr: &str) -> bool {
    let expr = expr.trim();
    for (op, all) in [("all(", true), ("any(", false)] {
        if let Some(inner) = expr.strip_prefix(op).and_then(|e| e.strip_suffix(')')) {
            let mut terms = split_top_level(inner).into_iter().map(eval_cfg);
            return if all {
                terms.all(|b| b)
            } else {
                terms.any(|b| b)
            };
        }
    }
    if let Some(inner) = expr.strip_prefix("not(").and_then(|e| e.strip_suffix(')')) {
        return !eval_cfg(inner);
    }
    let (key, value) = match expr.split_once('=') {
        Some((k, v)) => (k.trim(), Some(v.trim().trim_matches('"'))),
        None => (expr, None),
    };
    match (key, value) {
        ("unix", _) => true,
        ("windows", _) => false,
        ("target_os", Some(v)) => v == "linux",
        ("target_family", Some(v)) => v == "unix",
        ("target_env", Some(v)) => v == "gnu" || v.is_empty(),
        ("target_vendor", Some(v)) => v == "unknown",
        _ => true,
    }
}

/// Split `a, all(b, c), d` on the commas outside parentheses.
fn split_top_level(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    for (i, c) in s.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

/// The `--in-tree` entry that also takes every dependency published
/// from the root's own repository (crates.io's `repository`, cut at
/// the path a workspace member points into).
pub const IN_TREE_REPOSITORY: &str = "@repository";

/// Whether the in-tree list asks for the same-repository rule.
fn in_tree_repository_rule(list: &[String]) -> bool {
    list.iter().any(|g| g == IN_TREE_REPOSITORY)
}

/// Whether one of the in-tree globs (the sentinel aside) matches.
fn matches_in_tree_glob(list: &[String], name: &str) -> bool {
    list.iter()
        .any(|g| g != IN_TREE_REPOSITORY && glob_match(g, name))
}

/// Shell-style `*` matching, enough for `uu_*` and `*-sys`.
fn glob_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            match rest.find(part) {
                Some(pos) => rest = &rest[pos + part.len()..],
                None => return false,
            }
        }
    }
    true
}

/// Fetch all non-yanked versions of a crate from crates.io.
async fn fetch_versions(name: &str) -> Result<Vec<String>, String> {
    let resp = fetch_crate_info(name).await?;
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
#[cfg(test)]
fn resolve_default_deps(
    features: &std::collections::HashMap<String, Vec<String>>,
    all_optional_deps: &HashSet<String>,
) -> HashSet<String> {
    resolve_activated_deps(features, &["default".to_string()], all_optional_deps)
}

/// What enabling `seeds` activates.
#[derive(Debug, Default, PartialEq, Eq)]
struct Activation {
    /// Optional deps switched on.
    deps: HashSet<String>,
    /// Features requested *of* dependencies (`uu_ls/selinux` enables
    /// feature `selinux` of dep `uu_ls`) — what an in-tree member is
    /// built with.
    dep_features: BTreeMap<String, Vec<String>>,
}

/// The optional deps activated by enabling `seeds` — feature names,
/// `dep:` entries or optional dep names — followed transitively
/// through the crate's feature table.
#[cfg(test)]
fn resolve_activated_deps(
    features: &std::collections::HashMap<String, Vec<String>>,
    seeds: &[String],
    all_optional_deps: &HashSet<String>,
) -> HashSet<String> {
    resolve_activation(features, seeds, all_optional_deps).deps
}

/// [`resolve_activated_deps`], keeping the per-dependency feature
/// requests too.
fn resolve_activation(
    features: &std::collections::HashMap<String, Vec<String>>,
    seeds: &[String],
    all_optional_deps: &HashSet<String>,
) -> Activation {
    let mut activated = HashSet::new();
    let mut dep_features: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    let mut visited_features: HashSet<&str> = HashSet::new();

    // Each seed is a feature to enable: it may name a table entry
    // (`default`, `feat_acl`), an optional dep directly (Cargo's
    // implicit feature), or a `dep:` entry.
    for seed in seeds {
        queue.push_back(seed.as_str());
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
        // `foo/bar` enables feature bar of dep foo (and the dep, if
        // optional); `foo?/bar` only the feature, if foo is enabled.
        if let Some((dep_name, dep_feat)) = feat.split_once('/') {
            let (dep_name, weak) = match dep_name.strip_suffix('?') {
                Some(d) => (d, true),
                None => (dep_name, false),
            };
            if !weak && all_optional_deps.contains(dep_name) {
                activated.insert(dep_name.to_string());
            }
            dep_features
                .entry(dep_name.to_string())
                .or_default()
                .push(dep_feat.to_string());
            continue;
        }

        // A feature named after an optional dep activates it.
        if all_optional_deps.contains(feat) {
            activated.insert(feat.to_string());
        }

        // Recurse into sub-features (`dep/feature` entries included —
        // the branch above records them).
        if let Some(sub) = features.get(feat) {
            queue.extend(sub.iter().map(String::as_str));
        }
    }

    Activation {
        deps: activated,
        dep_features,
    }
}

/// Fetch version info (features) for a specific crate version.
async fn fetch_version_info(name: &str, version: &str) -> Result<VersionInfo, String> {
    let url = format!("https://crates.io/api/v1/crates/{name}/{version}");
    let resp: VersionInfoResponse =
        get_json(&url, &format!("{name}/{version}/version.json"), None).await?;
    Ok(resp.version)
}

/// Fetch version info (features) for a specific crate version.
async fn fetch_features(
    name: &str,
    version: &str,
) -> Result<std::collections::HashMap<String, Vec<String>>, String> {
    Ok(fetch_version_info(name, version).await?.features)
}

/// What a spec's `%cargo_generate_buildrequires` line enables.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SpecFeatures {
    /// `-a`: every feature.
    pub all: bool,
    /// `-n`: without default features.
    pub no_default: bool,
    /// `-f a,b`: extra features, macros expanded (a conditional
    /// `%global` takes the union of its definitions — a superset is
    /// the safe reading).
    pub features: Vec<String>,
}

/// Read the features a spec builds with from its
/// `%cargo_generate_buildrequires` line. `None` when the spec has no
/// such line (not a cargo package).
pub fn parse_spec_features(spec: &str) -> Option<SpecFeatures> {
    let mut globals: std::collections::HashMap<&str, Vec<String>> =
        std::collections::HashMap::new();
    for line in spec.lines() {
        let mut it = line.split_whitespace();
        if it.next() == Some("%global")
            && let Some(name) = it.next()
        {
            let value: Vec<String> = it
                .flat_map(|v| v.split(','))
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect();
            globals.entry(name).or_default().extend(value);
        }
    }
    let line = spec
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("%cargo_generate_buildrequires"))?;
    let mut out = SpecFeatures::default();
    let mut tokens = line.split_whitespace().skip(1).peekable();
    while let Some(tok) = tokens.next() {
        match tok {
            "-a" => out.all = true,
            "-n" => out.no_default = true,
            "-f" => {
                if let Some(list) = tokens.next() {
                    out.features.extend(expand_feature_list(list, &globals));
                }
            }
            t if t.starts_with("-f") => out.features.extend(expand_feature_list(&t[2..], &globals)),
            _ => {}
        }
    }
    out.features.sort();
    out.features.dedup();
    Some(out)
}

/// A comma-separated feature list, with `%{name}` / `%name` macros
/// expanded from the spec's `%global` definitions.
fn expand_feature_list(
    list: &str,
    globals: &std::collections::HashMap<&str, Vec<String>>,
) -> Vec<String> {
    let macro_name = list
        .strip_prefix("%{")
        .and_then(|l| l.strip_suffix('}'))
        .or_else(|| list.strip_prefix('%'));
    match macro_name.and_then(|n| globals.get(n)) {
        Some(values) => values.clone(),
        None => list
            .split(',')
            .filter(|f| !f.is_empty())
            .map(str::to_string)
            .collect(),
    }
}

/// The features the rawhide package of `crate_name` builds with, from
/// its spec on dist-git — tried as `rust-<crate>` then `<crate>`
/// (applications are often packaged under the bare name). `None`
/// when no such package exists or its spec is not a cargo package's.
async fn spec_features_from_rawhide(
    crate_name: &str,
    package: Option<&str>,
) -> Option<(String, SpecFeatures)> {
    let candidates = match package {
        Some(p) => vec![p.to_string()],
        None => vec![format!("rust-{crate_name}"), crate_name.to_string()],
    };
    for pkg in candidates {
        let url = format!("https://src.fedoraproject.org/rpms/{pkg}/raw/rawhide/f/{pkg}.spec");
        let Ok(resp) = client().get(&url).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(text) = resp.text().await else {
            continue;
        };
        if let Some(features) = parse_spec_features(&text) {
            return Some((pkg, features));
        }
    }
    None
}

/// Whether a crate version ships binaries — an application, built with
/// its default features — or is a library, built with all of them.
async fn is_application(name: &str, version: &str) -> bool {
    fetch_version_info(name, version)
        .await
        .map(|v| !v.bin_names.is_empty())
        .unwrap_or(false)
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
async fn fetch_dependencies(
    name: &str,
    version: &str,
    seeds: &[String],
) -> Result<Vec<CrateDep>, String> {
    Ok(fetch_dependencies_with_features(name, version, seeds)
        .await?
        .0)
}

/// [`fetch_dependencies`], also returning the features the enabled
/// set requests of each dependency (`dep/feature` entries).
async fn fetch_dependencies_with_features(
    name: &str,
    version: &str,
    seeds: &[String],
) -> Result<(Vec<CrateDep>, BTreeMap<String, Vec<String>>), String> {
    let url = format!("https://crates.io/api/v1/crates/{name}/{version}/dependencies");
    let resp: DepsResponse =
        get_json(&url, &format!("{name}/{version}/dependencies.json"), None).await?;

    // Resolve default features to find which optional deps are
    // activated by default (RPMs are built with default features).
    let all_optional: HashSet<String> = resp
        .dependencies
        .iter()
        .filter(|d| d.optional)
        .map(|d| d.crate_id.clone())
        .collect();

    // The feature table is needed for `dep/feature` requests even
    // when nothing is optional.
    let features = fetch_features(name, version).await.unwrap_or_default();
    let activation = resolve_activation(&features, seeds, &all_optional);

    let deps = resp
        .dependencies
        .into_iter()
        .filter(|d| target_applies(d.target.as_deref()))
        .map(|d| {
            let activated = d.optional && activation.deps.contains(&d.crate_id);
            CrateDep {
                name: d.crate_id,
                version_req: d.req,
                kind: d.kind.unwrap_or_else(|| "normal".to_string()),
                optional: d.optional && !activated,
            }
        })
        .collect();
    Ok((deps, activation.dep_features))
}

/// `in rawhide` / `in the staging COPR @…, not yet in rawhide`.
fn built_label(where_: BuiltIn, opts: &CheckCrateOptions) -> String {
    let branch = opts.branch.as_deref().unwrap_or("the branch");
    match where_ {
        BuiltIn::Branch => format!("in {branch}"),
        BuiltIn::Copr => format!(
            "in the staging COPR {}, not yet in {branch}",
            opts.copr.as_deref().unwrap_or("?")
        ),
    }
}

/// The repos a dependency is checked against: the branch, and the
/// staging COPR layered over it (`--copr`), consulted for whatever the
/// branch does not satisfy. fedrq's `@copr:` repo is standalone, so
/// the layering is two queries, which is also what attributes a hit.
struct RepoStack {
    base: sandogasa_fedrq::Fedrq,
    copr: Option<sandogasa_fedrq::Fedrq>,
}

impl RepoStack {
    fn check(&self, deps: &[&CrateDep]) -> Vec<DepStatus> {
        let mut statuses = check_deps_in_repo(&self.base, deps);
        let Some(copr) = &self.copr else {
            return statuses;
        };
        let rest: Vec<usize> = statuses
            .iter()
            .enumerate()
            .filter(|(_, s)| !matches!(s, DepStatus::Satisfied { .. }))
            .map(|(i, _)| i)
            .collect();
        if rest.is_empty() {
            return statuses;
        }
        let subset: Vec<&CrateDep> = rest.iter().map(|&i| deps[i]).collect();
        for (i, s) in rest.into_iter().zip(check_deps_in_repo(copr, &subset)) {
            if let DepStatus::Satisfied {
                version, compat, ..
            } = s
            {
                statuses[i] = DepStatus::Satisfied {
                    version,
                    compat,
                    staged: true,
                };
            }
        }
        statuses
    }
}

/// Check if a dependency is available in the target repo and if
/// the version satisfies the requirement.
#[cfg(test)]
fn check_dep_in_repo(fedrq: &sandogasa_fedrq::Fedrq, dep: &CrateDep) -> DepStatus {
    check_deps_in_repo(fedrq, &[dep]).remove(0)
}

/// Check several dependencies against the repo in one fedrq
/// invocation: the providers of every `crate(<name>)` come back
/// together, each attributed to the crates its Provides satisfy, and
/// the per-crate decision is the same as ever. A repo the query
/// cannot reach reads as "nothing provides anything", as the
/// single-crate query always did.
fn check_deps_in_repo(fedrq: &sandogasa_fedrq::Fedrq, deps: &[&CrateDep]) -> Vec<DepStatus> {
    let caps: Vec<String> = deps.iter().map(|d| format!("crate({})", d.name)).collect();
    let providers = fedrq.providers_info(&caps).unwrap_or_default();
    deps.iter()
        .zip(&caps)
        .map(|(dep, cap)| {
            let provides: Vec<String> = providers
                .iter()
                .filter(|p| p.satisfies(cap))
                .flat_map(|p| p.provides.iter().cloned())
                .collect();
            status_from_provides(dep, &provides)
        })
        .collect()
}

/// The status of one dependency given everything the repo's providers
/// of its capability declare.
fn status_from_provides(dep: &CrateDep, provides: &[String]) -> DepStatus {
    // Extract all provided versions (multiple packages may provide
    // different versions, e.g. rust-rand and rust-rand0.9).
    let versions = extract_crate_versions(provides, &dep.name);

    if versions.is_empty() {
        return DepStatus::Missing;
    }

    let Ok(req) = semver::VersionReq::parse(&dep.version_req) else {
        // Can't parse the requirement — treat as satisfied to avoid
        // false positives.
        return DepStatus::Satisfied {
            version: versions[0].clone(),
            compat: false,
            staged: false,
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
                staged: false,
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
    fn a_scripted_package_says_why_it_is_there() {
        let report = CheckCrateReport {
            crate_name: "arrow".to_string(),
            crate_version: "57.0.0".to_string(),
            package: "rust-arrow".to_string(),
            branch: "rawhide".to_string(),
            already_built: None,
            dependencies: vec![],
            transitive_staged: vec![],
            transitive_missing: vec![
                TransitiveDep {
                    name: "quick-xml".to_string(),
                    package: "rust-quick-xml".to_string(),
                    status: TransitiveStatus::Unmet,
                    version: "0.40.1".to_string(),
                    version_req: "^0.39.4".to_string(),
                    pulled_by: "arrow".to_string(),
                },
                TransitiveDep {
                    name: "brand-new".to_string(),
                    package: "rust-brand-new".to_string(),
                    status: TransitiveStatus::Missing,
                    version: "1.0.0".to_string(),
                    version_req: "^1".to_string(),
                    pulled_by: "quick-xml".to_string(),
                },
            ],
            transitive_build_order: vec![],
            transitive_edges: Default::default(),
            review_bugs: Default::default(),
            in_tree: vec![],
            copr: None,
        };

        // Packaged but too old: the requirement is the interesting part.
        let unmet = build_reason("rust-quick-xml", &report).unwrap();
        assert!(
            unmet.starts_with("# rust-quick-xml: build 0.40.1 for ^0.39.4"),
            "{unmet}"
        );
        assert!(unmet.contains("nothing satisfies ^0.39.4"), "{unmet}");
        assert!(unmet.contains("pulled in by arrow"), "{unmet}");

        // Absent entirely, and pulled in by a dependency rather than the
        // root — which is the thing a reader cannot reconstruct later.
        let missing = build_reason("rust-brand-new", &report).unwrap();
        assert!(missing.contains("not packaged"), "{missing}");
        assert!(missing.contains("pulled in by quick-xml"), "{missing}");

        // The crate the run is about is named as such, not as a dependency.
        let root = build_reason("rust-arrow", &report).unwrap();
        assert!(root.contains("the crate this run is about"), "{root}");

        // Anything else gets no comment rather than a wrong one.
        assert_eq!(build_reason("rust-unrelated", &report), None);
        // And every comment is a comment, or it would break the script.
        for line in [unmet, missing, root] {
            assert!(line.starts_with("# "), "{line}");
        }
    }

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
            refresh: false,
            features: Vec::new(),
            no_default_features: false,
            package: None,
            in_tree: Vec::new(),
            copr: None,
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
    fn cache_serves_stored_bodies_until_ttl_and_never_when_refreshing() {
        let dir = tempfile::tempdir().unwrap();
        let c = CratesIoCache {
            dir: Some(dir.path().to_path_buf()),
            refresh: false,
            hits: Default::default(),
            misses: Default::default(),
        };
        assert_eq!(c.load("foo/1.0.0/dependencies.json", None), None);
        c.store("foo/1.0.0/dependencies.json", "{\"dependencies\":[]}");
        // Immutable entries serve forever; aged ones only within TTL.
        assert_eq!(
            c.load("foo/1.0.0/dependencies.json", None).as_deref(),
            Some("{\"dependencies\":[]}")
        );
        assert!(
            c.load("foo/1.0.0/dependencies.json", Some(VERSIONS_TTL))
                .is_some()
        );
        assert!(
            c.load(
                "foo/1.0.0/dependencies.json",
                Some(std::time::Duration::ZERO)
            )
            .is_none()
        );
        // --refresh reads nothing (but still stores).
        let r = CratesIoCache {
            dir: Some(dir.path().to_path_buf()),
            refresh: true,
            hits: Default::default(),
            misses: Default::default(),
        };
        assert_eq!(r.load("foo/1.0.0/dependencies.json", None), None);
        // No cache dir: silently a no-op.
        let none = CratesIoCache {
            dir: None,
            refresh: false,
            hits: Default::default(),
            misses: Default::default(),
        };
        none.store("x", "y");
        assert_eq!(none.load("x", None), None);
    }

    #[test]
    fn batched_statuses_attribute_providers_by_capability() {
        let pkg = |v: serde_json::Value| -> sandogasa_fedrq::PkgInfo {
            serde_json::from_value(v).unwrap()
        };
        // rand is provided at 0.9 (main) and 0.8 (compat); serde only
        // at 1.0; nothing provides ureq.
        let providers = [
            pkg(
                serde_json::json!({"name": "rust-rand-devel", "source_name": "rust-rand",
                "repoid": "rawhide", "provides": ["crate(rand) = 0.9.2", "crate(rand/default) = 0.9.2"]}),
            ),
            pkg(
                serde_json::json!({"name": "rust-rand0.8-devel", "source_name": "rust-rand0.8",
                "repoid": "rawhide", "provides": ["crate(rand) = 0.8.5"]}),
            ),
            pkg(
                serde_json::json!({"name": "rust-serde-devel", "source_name": "rust-serde",
                "repoid": "rawhide", "provides": ["crate(serde) = 1.0.228"]}),
            ),
        ];
        let deps = [
            make_dep("rand", "normal", false),
            make_dep("serde", "normal", false),
            make_dep("ureq", "normal", false),
        ];
        // Reproduce check_deps_in_repo's attribution over canned providers.
        let statuses: Vec<DepStatus> = deps
            .iter()
            .map(|dep| {
                let cap = format!("crate({})", dep.name);
                let provides: Vec<String> = providers
                    .iter()
                    .filter(|p| p.satisfies(&cap))
                    .flat_map(|p| p.provides.iter().cloned())
                    .collect();
                status_from_provides(dep, &provides)
            })
            .collect();
        // make_dep asks for ^1 by default: rand has no 1.x → unmet with
        // both versions listed, serde 1.0.228 satisfied, ureq missing.
        assert!(matches!(&statuses[0], DepStatus::Unmet { available, .. } if available.len() == 2));
        assert!(
            matches!(&statuses[1], DepStatus::Satisfied { version, compat: false, .. } if version == "1.0.228")
        );
        assert!(matches!(statuses[2], DepStatus::Missing));
    }

    #[test]
    fn all_features_makes_optional_deps_required() {
        let opts = make_opts(true, false, false);
        // A library root or any transitive crate: optional counts.
        assert!(should_expand(&make_dep("foo", "normal", true), &opts, true));
        // An application root without --include-optional: it does not.
        assert!(!should_expand(
            &make_dep("foo", "normal", true),
            &opts,
            false
        ));
        // Kind filtering still applies under all-features.
        assert!(!should_expand(&make_dep("foo", "weird", true), &opts, true));
    }

    #[test]
    fn spec_features_read_the_three_shapes_fedora_uses() {
        // uutils-coreutils: -f with a conditional %global → union.
        let uutils = "%if 0%{?el9}\n%global feature_flags feat_acl,feat_os_unix,uudoc\n%else\n\
            %global feature_flags feat_acl,feat_os_unix,feat_systemd_logind,uudoc\n%endif\n\
            %cargo_generate_buildrequires -f %{feature_flags}\n%cargo_build -f %{feature_flags}\n";
        let sf = parse_spec_features(uutils).unwrap();
        assert!(!sf.all && !sf.no_default);
        assert_eq!(
            sf.features,
            ["feat_acl", "feat_os_unix", "feat_systemd_logind", "uudoc"]
        );
        // atuin: everything.
        let atuin = "%cargo_generate_buildrequires -a\n%cargo_build -a\n";
        assert!(parse_spec_features(atuin).unwrap().all);
        // rbw: defaults; nushell: -t is not a feature flag.
        assert_eq!(
            parse_spec_features("%cargo_generate_buildrequires\n").unwrap(),
            SpecFeatures::default()
        );
        assert_eq!(
            parse_spec_features("%cargo_generate_buildrequires -t\n").unwrap(),
            SpecFeatures::default()
        );
        // A literal list and -n.
        let lit = parse_spec_features("%cargo_generate_buildrequires -n -f zstd,ssl\n").unwrap();
        assert!(lit.no_default);
        assert_eq!(lit.features, ["ssl", "zstd"]);
        // Not a cargo package at all.
        assert_eq!(parse_spec_features("%build\nmake\n"), None);
    }

    #[test]
    fn enabled_features_activate_their_optional_deps() {
        let features = std::collections::HashMap::from([
            ("default".to_string(), vec!["std".to_string()]),
            ("feat_acl".to_string(), vec!["dep:exacl".to_string()]),
            ("feat_selinux".to_string(), vec!["selinux".to_string()]),
        ]);
        let optional: HashSet<String> = ["exacl", "selinux", "zstd"].map(String::from).into();
        // Defaults alone activate nothing optional here.
        assert!(resolve_default_deps(&features, &optional).is_empty());
        // The Fedora build's -f list does: via dep: and via the
        // feature-named-after-a-dep form; a seed naming an optional
        // dep directly works too (Cargo's implicit feature).
        let seeds = ["default", "feat_acl", "feat_selinux", "zstd"].map(String::from);
        let on = resolve_activated_deps(&features, &seeds, &optional);
        assert_eq!(on, ["exacl", "selinux", "zstd"].map(String::from).into());
    }

    #[test]
    fn target_cfg_is_evaluated_for_linux() {
        assert!(target_applies(None));
        assert!(target_applies(Some("cfg(unix)")));
        assert!(target_applies(Some("cfg(not(windows))")));
        assert!(target_applies(Some("cfg(target_os = \"linux\")")));
        assert!(target_applies(Some(
            "cfg(all(unix, not(target_os = \"macos\")))"
        )));
        assert!(target_applies(Some("cfg(target_arch = \"x86_64\")")));
        assert!(target_applies(Some("x86_64-unknown-linux-gnu")));
        assert!(!target_applies(Some("cfg(windows)")));
        assert!(!target_applies(Some(
            "cfg(any(windows, target_os = \"macos\"))"
        )));
        assert!(!target_applies(Some("cfg(target_env = \"musl\")")));
        assert!(!target_applies(Some("x86_64-pc-windows-msvc")));
        assert!(!target_applies(Some(
            "cfg(all(unix, target_os = \"redox\"))"
        )));
    }

    #[test]
    fn in_tree_list_separates_globs_from_the_repository_sentinel() {
        let list = ["uucore*", "@repository"].map(String::from);
        assert!(matches_in_tree_glob(&list, "uucore_procs"));
        assert!(!matches_in_tree_glob(&list, "@repository"));
        assert!(in_tree_repository_rule(&list));
        let globs_only = ["uu_*".to_string()];
        assert!(!in_tree_repository_rule(&globs_only));
        assert!(!in_tree_repository_rule(&[]));
    }

    #[test]
    fn staged_deps_render_in_their_own_section_and_load_from_old_reports() {
        let mut report = make_report();
        report.copr = Some("@rust/uutils-and-nushell".to_string());
        report.dependencies.push(DepResult {
            dep: make_dep("phf_shared", "normal", false),
            status: DepStatus::Satisfied {
                version: "0.14.0".to_string(),
                compat: false,
                staged: true,
            },
            via: None,
        });
        report.transitive_staged.push(TransitiveDep {
            name: "phf_generator".to_string(),
            package: "rust-phf_generator".to_string(),
            status: TransitiveStatus::Staged,
            version: "0.14.0".to_string(),
            version_req: "^0.14.0".to_string(),
            pulled_by: "phf_macros".to_string(),
        });
        report.already_built = Some(BuiltIn::Copr);
        let text = render_report(&report);
        assert!(text.contains("Staging COPR: @rust/uutils-and-nushell"));
        assert!(
            text.contains(
                "Already built: my-crate 1.0.0 is in the staging COPR, not yet in rawhide"
            )
        );
        assert!(text.contains("Staged in COPR, not yet in the branch (2):\n  - phf_shared"));
        assert!(text.contains("  - phf_generator ^0.14.0 (via phf_macros) — 0.14.0"));
        assert!(text.contains("2 staged in COPR."), "{text}");
        // A report saved before `staged` existed reads as not staged.
        let old: DepStatus =
            serde_json::from_str(r#"{"status":"satisfied","version":"1.0"}"#).unwrap();
        assert!(matches!(old, DepStatus::Satisfied { staged: false, .. }));
    }

    #[test]
    fn glob_match_handles_prefix_suffix_and_exact() {
        assert!(glob_match("uu_*", "uu_ls"));
        assert!(!glob_match("uu_*", "uucore"));
        assert!(glob_match("*-sys", "libz-sys"));
        assert!(glob_match("uu_*_compat", "uu_ls_compat"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "exactly"));
    }

    #[test]
    fn repository_normalization_matches_the_same_project() {
        assert_eq!(
            normalize_repository("https://github.com/uutils/coreutils/"),
            normalize_repository("http://www.github.com/uutils/coreutils.git")
        );
        // crates.io: uu_ls points into the workspace it lives in.
        assert_eq!(
            normalize_repository("https://github.com/uutils/coreutils/tree/main/src/uu/ls"),
            normalize_repository("https://github.com/uutils/coreutils")
        );
        assert_ne!(
            normalize_repository("https://github.com/uutils/coreutils"),
            normalize_repository("https://github.com/uutils/findutils")
        );
    }

    #[test]
    fn dep_feature_entries_request_features_of_members() {
        // uutils-style: the root's feat_selinux enables selinux in
        // several members; `feat_acl` enables an optional dep by name.
        let features = std::collections::HashMap::from([
            ("default".to_string(), vec!["feat_common".to_string()]),
            (
                "feat_selinux".to_string(),
                vec![
                    "uu_ls/selinux".to_string(),
                    "uu_cp/selinux".to_string(),
                    "selinux".to_string(),
                ],
            ),
            ("feat_acl".to_string(), vec!["exacl".to_string()]),
            (
                "feat_common".to_string(),
                vec!["uucore?/backup".to_string()],
            ),
        ]);
        let optional: HashSet<String> = ["selinux", "exacl", "uu_ls"].map(String::from).into();
        let seeds = ["default", "feat_selinux", "feat_acl"].map(String::from);
        let act = resolve_activation(&features, &seeds, &optional);
        assert_eq!(
            act.deps,
            ["selinux", "exacl", "uu_ls"].map(String::from).into()
        );
        assert_eq!(act.dep_features["uu_ls"], ["selinux"]);
        assert_eq!(act.dep_features["uu_cp"], ["selinux"]);
        // Weak (`?/`) requests the feature without enabling the dep.
        assert_eq!(act.dep_features["uucore"], ["backup"]);
        assert!(!act.deps.contains("uucore"));
    }

    #[test]
    fn should_expand_normal() {
        let opts = make_opts(true, false, false);
        assert!(should_expand(
            &make_dep("foo", "normal", false),
            &opts,
            false
        ));
    }

    #[test]
    fn should_expand_build() {
        let opts = make_opts(true, false, false);
        assert!(should_expand(
            &make_dep("foo", "build", false),
            &opts,
            false
        ));
    }

    #[test]
    fn should_expand_dev_included_by_default() {
        let opts = make_opts(true, false, false);
        assert!(should_expand(&make_dep("foo", "dev", false), &opts, false));
    }

    #[test]
    fn should_expand_dev_excluded_when_requested() {
        let opts = make_opts(true, true, false);
        assert!(!should_expand(&make_dep("foo", "dev", false), &opts, false));
    }

    #[test]
    fn should_expand_optional_excluded_by_default() {
        let opts = make_opts(true, false, false);
        assert!(!should_expand(
            &make_dep("foo", "normal", true),
            &opts,
            false
        ));
    }

    #[test]
    fn should_expand_optional_when_included() {
        let opts = make_opts(true, false, true);
        assert!(should_expand(
            &make_dep("foo", "normal", true),
            &opts,
            false
        ));
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
            already_built: None,
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
                        staged: false,
                    },
                    via: None,
                },
                DepResult {
                    dep: CrateDep {
                        name: "missing-dep".to_string(),
                        version_req: "^0.5".to_string(),
                        kind: "normal".to_string(),
                        optional: false,
                    },
                    status: DepStatus::Missing,
                    via: None,
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
                    via: None,
                },
            ],
            transitive_staged: vec![],
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
            in_tree: vec![],
            copr: None,
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

    fn make_report() -> CheckCrateReport {
        CheckCrateReport {
            crate_name: "my-crate".to_string(),
            crate_version: "1.0.0".to_string(),
            package: "rust-my-crate".to_string(),
            branch: "rawhide".to_string(),
            already_built: None,
            copr: None,
            dependencies: vec![
                DepResult {
                    dep: make_dep("dep-a", "normal", false),
                    status: DepStatus::Missing,
                    via: None,
                },
                DepResult {
                    dep: make_dep("dep-b", "normal", false),
                    status: DepStatus::Satisfied {
                        version: "1.0.0".to_string(),
                        compat: false,
                        staged: false,
                    },
                    via: None,
                },
            ],
            transitive_missing: vec![],
            transitive_staged: vec![],
            transitive_build_order: vec![
                dag::BuildPhase {
                    phase: 1,
                    packages: vec!["dep-a".to_string()],
                },
                dag::BuildPhase {
                    phase: 2,
                    packages: vec!["dep-c".to_string()],
                },
            ],
            transitive_edges: BTreeMap::new(),
            review_bugs: BTreeMap::new(),
            in_tree: vec![],
        }
    }

    #[test]
    fn full_build_phases_appends_root() {
        let report = make_report();
        let phases = report.full_build_phases();
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[2].phase, 3);
        assert_eq!(phases[2].packages, vec!["my-crate"]);
    }

    #[test]
    fn full_build_phases_empty_transitive() {
        let mut report = make_report();
        report.transitive_build_order.clear();
        let phases = report.full_build_phases();
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].phase, 1);
        assert_eq!(phases[0].packages, vec!["my-crate"]);
    }

    #[test]
    fn write_and_load_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.toml");
        let path_str = path.to_str().unwrap();
        let report = make_report();
        write_toml(&report, path_str).unwrap();
        let loaded = load_report(path_str).unwrap();
        assert_eq!(loaded.crate_name, "my-crate");
        assert_eq!(loaded.crate_version, "1.0.0");
        assert_eq!(loaded.dependencies.len(), 2);
    }

    #[test]
    fn check_dep_in_repo_missing() {
        let fedrq = sandogasa_fedrq::Fedrq {
            branch: Some("nonexistent-test-branch-xyz".to_string()),
            repo: None,
        };
        let dep = make_dep("nonexistent-crate-xyz", "normal", false);
        let status = check_dep_in_repo(&fedrq, &dep);
        assert!(matches!(status, DepStatus::Missing));
    }

    #[test]
    fn extract_versions_duplicate_provides() {
        let provides = vec![
            "crate(foo) = 1.0.0".to_string(),
            "crate(foo) = 1.0.0".to_string(),
        ];
        let versions = extract_crate_versions(&provides, "foo");
        assert_eq!(versions, vec!["1.0.0", "1.0.0"]);
    }

    #[test]
    fn opt_label_optional() {
        let dep = DepResult {
            dep: make_dep("foo", "normal", true),
            status: DepStatus::Missing,
            via: None,
        };
        assert_eq!(opt_label(&dep), ", optional");
    }

    #[test]
    fn opt_label_required() {
        let dep = DepResult {
            dep: make_dep("foo", "normal", false),
            status: DepStatus::Missing,
            via: None,
        };
        assert_eq!(opt_label(&dep), "");
    }

    #[test]
    fn unique_crate_count_deduplicates() {
        let deps = [
            DepResult {
                dep: make_dep("foo", "normal", false),
                status: DepStatus::Missing,
                via: None,
            },
            DepResult {
                dep: make_dep("foo", "build", false),
                status: DepStatus::Missing,
                via: None,
            },
            DepResult {
                dep: make_dep("bar", "normal", false),
                status: DepStatus::Missing,
                via: None,
            },
        ];
        let refs: Vec<&DepResult> = deps.iter().collect();
        assert_eq!(unique_crate_count(&refs), 2);
    }
}
