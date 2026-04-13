// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Link Bugzilla review requests based on dependency analysis.
//!
//! Reads a TOML analysis file from `check-crate --toml`, searches
//! Bugzilla for review requests matching each missing package, and
//! updates the Blocks/Depends On fields to match the dependency graph.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;

use sandogasa_bugzilla::BzClient;

use crate::check_crate::{self, CheckCrateReport, DepStatus};

// ---- Public types ----

/// Options for the review-deps command.
pub struct CheckPkgReviewsOptions {
    pub toml_path: String,
    pub bugzilla_url: String,
    pub api_key: String,
    pub dry_run: bool,
    pub verbose: bool,
}

/// A proposed change to a Bugzilla review bug.
struct LinkChange {
    bug_id: u64,
    package: String,
    add_depends: Vec<u64>,
    remove_depends: Vec<u64>,
}

// ---- Public functions ----

/// Run the review-deps workflow.
pub fn check_pkg_reviews(opts: &CheckPkgReviewsOptions) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;
    rt.block_on(check_pkg_reviews_async(opts))
}

// ---- Private implementation ----

async fn check_pkg_reviews_async(opts: &CheckPkgReviewsOptions) -> Result<(), String> {
    let mut report = check_crate::load_report(&opts.toml_path)?;

    let bz = BzClient::new(&opts.bugzilla_url).with_api_key(opts.api_key.clone());

    // Build package name → crate name mapping and collect edges.
    let (pkg_to_crate, edges) = collect_packages_and_edges(&report);

    if pkg_to_crate.is_empty() {
        println!("No missing packages to link.");
        return Ok(());
    }

    if opts.verbose {
        eprintln!(
            "[review-deps] looking up {} review request(s)",
            pkg_to_crate.len()
        );
    }

    // Collect cached bug IDs from the TOML, and search for the rest.
    let mut bug_map: BTreeMap<String, (u64, String)> = BTreeMap::new(); // package → (id, status)
    let mut not_found: Vec<String> = Vec::new();
    let mut toml_updated = false;
    let mut needs_search: Vec<String> = Vec::new();

    // Batch-verify cached bug IDs.
    let mut cached_pkg_by_id: BTreeMap<u64, String> = BTreeMap::new();
    for pkg_name in pkg_to_crate.keys() {
        if let Some(&cached_id) = report.review_bugs.get(pkg_name) {
            cached_pkg_by_id.insert(cached_id, pkg_name.clone());
        } else {
            needs_search.push(pkg_name.clone());
        }
    }

    if !cached_pkg_by_id.is_empty() {
        let ids: Vec<u64> = cached_pkg_by_id.keys().copied().collect();
        if opts.verbose {
            eprintln!("[check-pkg-reviews] verifying {} cached bug(s)", ids.len());
        }
        match bz.bugs(&ids).await {
            Ok(bugs) => {
                let fetched: BTreeMap<u64, _> = bugs.into_iter().map(|b| (b.id, b)).collect();
                for (id, pkg_name) in &cached_pkg_by_id {
                    if let Some(bug) = fetched.get(id) {
                        if opts.verbose {
                            eprintln!(
                                "[check-pkg-reviews] cached bug {id} \
                                 for {pkg_name} ({status})",
                                status = bug.status
                            );
                        }
                        bug_map.insert(pkg_name.clone(), (bug.id, bug.status.clone()));
                    } else {
                        if opts.verbose {
                            eprintln!(
                                "[check-pkg-reviews] cached bug {id} \
                                 not found for {pkg_name}"
                            );
                        }
                        needs_search.push(pkg_name.clone());
                    }
                }
            }
            Err(e) => {
                if opts.verbose {
                    eprintln!(
                        "[check-pkg-reviews] batch fetch failed: {e}, \
                         falling back to search"
                    );
                }
                needs_search.extend(cached_pkg_by_id.into_values());
            }
        }
    }

    // Search Bugzilla for packages without cached IDs.
    for pkg_name in &needs_search {
        match find_review_bug(&bz, pkg_name).await? {
            Some((id, summary, status)) => {
                if opts.verbose {
                    eprintln!(
                        "[check-pkg-reviews] found bug {id} for \
                         {pkg_name} ({status}): {summary}"
                    );
                }
                bug_map.insert(pkg_name.clone(), (id, status));
                report.review_bugs.insert(pkg_name.clone(), id);
                toml_updated = true;
            }
            None => {
                not_found.push(pkg_name.clone());
            }
        }
    }

    // Save updated bug IDs back to the TOML file.
    if toml_updated {
        check_crate::write_toml(&report, &opts.toml_path)?;
    }

    // Fetch current depends_on for all found bugs (batch).
    let all_bug_ids: Vec<u64> = bug_map.values().map(|(id, _)| *id).collect();
    let current_depends: BTreeMap<u64, Vec<u64>> = if all_bug_ids.is_empty() {
        BTreeMap::new()
    } else {
        let bugs = bz
            .bugs(&all_bug_ids)
            .await
            .map_err(|e| format!("failed to fetch bugs: {e}"))?;
        bugs.into_iter().map(|b| (b.id, b.depends_on)).collect()
    };

    // Compute desired links and diff against current state.
    // Convert edges (crate names) to package names for lookup.
    let pkg_edges = crate_edges_to_pkg_edges(&edges, &report);
    let changes = compute_changes(&pkg_edges, &bug_map, &current_depends);

    // Print summary.
    print_summary(&changes, &bug_map, &not_found);

    if changes.is_empty() {
        println!("\nNo changes needed.");
        return Ok(());
    }

    if opts.dry_run {
        println!("\n(dry run — no changes applied)");
        return Ok(());
    }

    // Prompt for confirmation.
    print!("\nApply changes? [y/N] ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| format!("failed to read input: {e}"))?;

    if input.trim().to_lowercase() != "y" {
        println!("Aborted.");
        return Ok(());
    }

    // Apply changes.
    for change in &changes {
        if opts.verbose {
            eprintln!(
                "[review-deps] updating bug {} ({})",
                change.bug_id, change.package
            );
        }

        let mut update = serde_json::Map::new();
        let mut depends = serde_json::Map::new();

        if !change.add_depends.is_empty() {
            depends.insert("add".to_string(), serde_json::json!(change.add_depends));
        }
        if !change.remove_depends.is_empty() {
            depends.insert(
                "remove".to_string(),
                serde_json::json!(change.remove_depends),
            );
        }
        update.insert("depends_on".to_string(), serde_json::Value::Object(depends));

        bz.update(change.bug_id, &serde_json::Value::Object(update))
            .await
            .map_err(|e| format!("failed to update bug {}: {e}", change.bug_id))?;
    }

    println!("Updated {} bug(s).", changes.len());
    Ok(())
}

/// Build a package name → crate name map and collect the full
/// dependency edge map (using crate names) including the root.
fn collect_packages_and_edges(
    report: &CheckCrateReport,
) -> (BTreeMap<String, String>, BTreeMap<String, BTreeSet<String>>) {
    let mut pkg_to_crate: BTreeMap<String, String> = BTreeMap::new();

    // Root crate.
    pkg_to_crate.insert(report.package.clone(), report.crate_name.clone());

    // Direct missing deps (not unmet — those already exist in the repo).
    let mut root_deps = BTreeSet::new();
    for dep in &report.dependencies {
        if matches!(dep.status, DepStatus::Missing) {
            let pkg = format!("rust-{}", dep.dep.name);
            pkg_to_crate.insert(pkg, dep.dep.name.clone());
            root_deps.insert(dep.dep.name.clone());
        }
    }

    // Transitive missing deps.
    let mut missing_names: HashSet<String> = HashSet::new();
    missing_names.insert(report.crate_name.clone());
    for name in root_deps.iter() {
        missing_names.insert(name.clone());
    }
    for dep in &report.transitive_missing {
        // Only include truly missing deps, not unmet (wrong version).
        if dep.status == check_crate::TransitiveStatus::Missing {
            pkg_to_crate.insert(dep.package.clone(), dep.name.clone());
            missing_names.insert(dep.name.clone());
        }
    }

    // Build edges filtered to only missing crates (exclude unmet deps
    // that may have been included via --include-unmet during analysis).
    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (parent, deps) in &report.transitive_edges {
        if missing_names.contains(parent) {
            let filtered: BTreeSet<String> = deps
                .iter()
                .filter(|d| missing_names.contains(d.as_str()))
                .cloned()
                .collect();
            edges.insert(parent.clone(), filtered);
        }
    }
    edges.insert(report.crate_name.clone(), root_deps);

    // Ensure all missing crates have an edge entry.
    for crate_name in pkg_to_crate.values() {
        edges.entry(crate_name.clone()).or_default();
    }

    (pkg_to_crate, edges)
}

/// Convert crate-name edges to package-name edges using the report's
/// package name mappings.
fn crate_edges_to_pkg_edges(
    edges: &BTreeMap<String, BTreeSet<String>>,
    report: &CheckCrateReport,
) -> BTreeMap<String, BTreeSet<String>> {
    // Build crate → package name lookup.
    let mut crate_to_pkg: BTreeMap<&str, &str> = BTreeMap::new();
    crate_to_pkg.insert(&report.crate_name, &report.package);
    for dep in &report.transitive_missing {
        crate_to_pkg.insert(&dep.name, &dep.package);
    }
    for dep in &report.dependencies {
        if matches!(dep.status, DepStatus::Missing) {
            // Direct deps don't have a package field; derive it.
            // We'll use the pkg_to_crate map in the caller instead.
        }
    }

    let to_pkg = |name: &str| -> String {
        crate_to_pkg
            .get(name)
            .map(|s| (*s).to_string())
            .unwrap_or_else(|| format!("rust-{name}"))
    };

    edges
        .iter()
        .map(|(parent, deps)| (to_pkg(parent), deps.iter().map(|d| to_pkg(d)).collect()))
        .collect()
}

/// Search Bugzilla for a package review request.
///
/// Searches all statuses (open and closed). Returns `(bug_id, summary, status)`.
async fn find_review_bug(
    bz: &BzClient,
    rpm_name: &str,
) -> Result<Option<(u64, String, String)>, String> {
    let prefix = format!("Review Request: {rpm_name} - ");
    let query = format!(
        "product=Fedora&component=Package Review\
         &short_desc_type=substring\
         &short_desc={prefix}"
    );

    let bugs = bz
        .search(&query, 10)
        .await
        .map_err(|e| format!("Bugzilla search failed for {rpm_name}: {e}"))?;

    // Post-filter for exact prefix match.
    let matched: Vec<_> = bugs
        .into_iter()
        .filter(|b| b.summary.starts_with(&prefix))
        .collect();

    match matched.len() {
        0 => Ok(None),
        1 => Ok(Some((
            matched[0].id,
            matched[0].summary.clone(),
            matched[0].status.clone(),
        ))),
        _ => {
            // Prefer the latest open bug, else the latest closed one.
            let is_open = |b: &&sandogasa_bugzilla::models::Bug| b.status != "CLOSED";
            let best = matched
                .iter()
                .filter(is_open)
                .max_by_key(|b| b.id)
                .unwrap_or_else(|| matched.iter().max_by_key(|b| b.id).unwrap());
            Ok(Some((best.id, best.summary.clone(), best.status.clone())))
        }
    }
}

/// Compute link changes by diffing desired vs current depends_on.
fn compute_changes(
    pkg_edges: &BTreeMap<String, BTreeSet<String>>,
    bug_map: &BTreeMap<String, (u64, String)>,
    current_depends: &BTreeMap<u64, Vec<u64>>,
) -> Vec<LinkChange> {
    let mut changes = Vec::new();

    for (pkg_name, deps) in pkg_edges {
        let Some(&(bug_id, _)) = bug_map.get(pkg_name) else {
            continue;
        };

        // Desired: bug IDs of dependency packages that have review bugs.
        let desired: BTreeSet<u64> = deps
            .iter()
            .filter_map(|dep| bug_map.get(dep).map(|(id, _)| *id))
            .collect();

        // Current: existing depends_on bug IDs.
        let current: BTreeSet<u64> = current_depends
            .get(&bug_id)
            .map(|v| v.iter().copied().collect())
            .unwrap_or_default();

        // Only manage links to bugs we know about (review bugs in
        // our set). Don't remove links to unrelated bugs.
        let known_bugs: BTreeSet<u64> = bug_map.values().map(|(id, _)| *id).collect();

        let add: Vec<u64> = desired.difference(&current).copied().collect();
        let remove: Vec<u64> = current
            .difference(&desired)
            .copied()
            .filter(|id| known_bugs.contains(id))
            .collect();

        if !add.is_empty() || !remove.is_empty() {
            changes.push(LinkChange {
                bug_id,
                package: pkg_name.clone(),
                add_depends: add,
                remove_depends: remove,
            });
        }
    }

    changes
}

/// Print a summary of proposed changes.
fn print_summary(
    changes: &[LinkChange],
    bug_map: &BTreeMap<String, (u64, String)>,
    not_found: &[String],
) {
    // Reverse map for display.
    let id_to_pkg: BTreeMap<u64, &str> = bug_map
        .iter()
        .map(|(name, &(id, _))| (id, name.as_str()))
        .collect();

    let bug_label = |id: u64| -> String {
        match id_to_pkg.get(&id) {
            Some(name) => format!("{id} ({name})"),
            None => format!("{id}"),
        }
    };

    // Show found bugs with their status.
    println!("Review bugs found:\n");
    for (pkg, (id, status)) in bug_map {
        println!("  {pkg}: bug {id} ({status})");
    }

    if !changes.is_empty() {
        println!("\nProposed changes:\n");
        for c in changes {
            println!("  Bug {} ({}):", c.bug_id, c.package);
            for id in &c.add_depends {
                println!("    + Depends On: {}", bug_label(*id));
            }
            for id in &c.remove_depends {
                println!("    - Depends On: {}", bug_label(*id));
            }
        }
    }

    if !not_found.is_empty() {
        println!("\nReview bugs not found:");
        for name in not_found {
            println!("  - {name}");
        }
    }

    let found = bug_map.len();
    let total = found + not_found.len();
    println!(
        "\n{found}/{total} review bug(s) found, {} change(s) needed.",
        changes.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_packages_includes_root() {
        let report = CheckCrateReport {
            crate_name: "my-crate".to_string(),
            crate_version: "1.0.0".to_string(),
            package: "rust-my-crate".to_string(),
            branch: "rawhide".to_string(),
            dependencies: vec![check_crate::DepResult {
                dep: check_crate::CrateDep {
                    name: "dep-a".to_string(),
                    version_req: "^1.0".to_string(),
                    kind: "normal".to_string(),
                    optional: false,
                },
                status: DepStatus::Missing,
            }],
            transitive_missing: vec![check_crate::TransitiveDep {
                name: "dep-b".to_string(),
                package: "rust-dep-b".to_string(),
                status: check_crate::TransitiveStatus::Missing,
                version: "0.5.0".to_string(),
                version_req: "^0.5".to_string(),
                pulled_by: "dep-a".to_string(),
            }],
            transitive_build_order: vec![],
            transitive_edges: BTreeMap::from([
                ("dep-a".to_string(), BTreeSet::from(["dep-b".to_string()])),
                ("dep-b".to_string(), BTreeSet::new()),
            ]),
            review_bugs: BTreeMap::new(),
        };

        let (pkgs, edges) = collect_packages_and_edges(&report);
        assert!(pkgs.contains_key("rust-my-crate"));
        assert!(pkgs.contains_key("rust-dep-a"));
        assert!(pkgs.contains_key("rust-dep-b"));
        assert!(edges["my-crate"].contains("dep-a"));
        assert!(edges["dep-a"].contains("dep-b"));
    }

    #[test]
    fn compute_changes_adds_missing_links() {
        let edges = BTreeMap::from([
            ("rust-a".to_string(), BTreeSet::from(["rust-b".to_string()])),
            ("rust-b".to_string(), BTreeSet::new()),
        ]);
        let bug_map = BTreeMap::from([
            ("rust-a".to_string(), (100, "NEW".to_string())),
            ("rust-b".to_string(), (200, "NEW".to_string())),
        ]);
        let current = BTreeMap::from([(100, vec![]), (200, vec![])]);

        let changes = compute_changes(&edges, &bug_map, &current);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].bug_id, 100);
        assert_eq!(changes[0].add_depends, vec![200]);
        assert!(changes[0].remove_depends.is_empty());
    }

    #[test]
    fn compute_changes_no_op_when_correct() {
        let edges = BTreeMap::from([
            ("rust-a".to_string(), BTreeSet::from(["rust-b".to_string()])),
            ("rust-b".to_string(), BTreeSet::new()),
        ]);
        let bug_map = BTreeMap::from([
            ("rust-a".to_string(), (100, "NEW".to_string())),
            ("rust-b".to_string(), (200, "NEW".to_string())),
        ]);
        let current = BTreeMap::from([(100, vec![200]), (200, vec![])]);

        let changes = compute_changes(&edges, &bug_map, &current);
        assert!(changes.is_empty());
    }

    #[test]
    fn compute_changes_preserves_unrelated_links() {
        let edges = BTreeMap::from([("rust-a".to_string(), BTreeSet::new())]);
        let bug_map = BTreeMap::from([("rust-a".to_string(), (100, "NEW".to_string()))]);
        // Bug 100 depends on bug 999 which is not in our set.
        let current = BTreeMap::from([(100, vec![999])]);

        let changes = compute_changes(&edges, &bug_map, &current);
        // Should not remove 999 since it's not a bug we manage.
        assert!(changes.is_empty());
    }
}
