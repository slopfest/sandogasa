// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Link Bugzilla review requests based on dependency analysis.
//!
//! Reads a TOML analysis file from `check-crate --toml`, searches
//! Bugzilla for review requests matching each missing package, and
//! updates the Blocks/Depends On fields to match the dependency graph.

use std::collections::{BTreeMap, BTreeSet, HashSet};

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

    let bz = BzClient::new(&opts.bugzilla_url)
        .with_api_key(opts.api_key.clone())
        .map_err(|e| e.to_string())?;

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
        // Only the status is read (#123).
        match bz.bugs_with(&ids, &[]).await {
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
    for (pkg_name, found) in find_review_bugs(&bz, &needs_search).await {
        match found? {
            Some(bug) => {
                let (id, status) = (bug.id, bug.status.clone());
                if opts.verbose {
                    eprintln!(
                        "[check-pkg-reviews] found bug {id} for \
                         {pkg_name} ({status}): {}",
                        bug.summary
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
            .bugs_with(&all_bug_ids, &["depends_on"])
            .await
            .map_err(|e| format!("failed to fetch bugs: {e}"))?;
        bugs.into_iter().map(|b| (b.id, b.depends_on)).collect()
    };

    // Compute desired links and diff against current state.
    // Convert edges (crate names) to package names for lookup.
    let pkg_edges = crate_edges_to_pkg_edges(&edges, &report);
    let changes = compute_changes(&pkg_edges, &bug_map, &current_depends);

    // Print summary.
    print!("{}", summary(&changes, &bug_map, &not_found));

    if changes.is_empty() {
        println!("\nNo changes needed.");
        return Ok(());
    }

    if opts.dry_run {
        println!("\n(dry run — no changes applied)");
        return Ok(());
    }

    // Prompt for confirmation.
    if !sandogasa_cli::confirm("\nApply changes?", false)
        .map_err(|e| format!("failed to read input: {e}"))?
    {
        println!("Aborted.");
        return Ok(());
    }

    apply_changes(&bz, &changes, opts.verbose).await?;
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
    // Direct missing deps have no package field; to_pkg derives the
    // `rust-<crate>` name as its fallback.

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

/// A query-string value with the characters that would end or change
/// it escaped; Bugzilla's search is otherwise sent as written.
fn query_value(text: &str) -> String {
    text.replace('%', "%25")
        .replace('&', "%26")
        .replace('+', "%2B")
        .replace('#', "%23")
}

/// The review requests for `rpm_name` whose summary contains `text`.
/// Every match is read, not a first page: Red Hat's Bugzilla answers
/// oldest first, so a cap drops the newest reviews, re-reviews among
/// them (#129). Each candidate is confirmed with
/// [`sandogasa_bugclass::bugzilla::review_request_package`], which is
/// exact where a substring is not.
async fn review_requests(
    bz: &BzClient,
    rpm_name: &str,
    text: &str,
) -> Result<Vec<sandogasa_bugzilla::models::Bug>, String> {
    let query = format!(
        "product=Fedora&component=Package Review\
         &short_desc_type=substring&short_desc={}",
        query_value(text)
    );
    // The summary to match and, for check-wip, the fedora-review flag
    // (`wip::is_approved`); nothing else of the default set (#123).
    let bugs = bz
        .search_with(&query, 0, &["flags"])
        .await
        .map_err(|e| format!("Bugzilla search failed for {rpm_name}: {e}"))?;
    Ok(bugs
        .into_iter()
        .filter(|b| {
            sandogasa_bugclass::bugzilla::review_request_package(&b.summary).as_deref()
                == Some(rpm_name)
        })
        .collect())
}

/// Search Bugzilla for a package review request, open or closed.
///
/// Most summaries read `Review Request: <name> - <summary>`, and that
/// text matches only the package's own reviews. Without the ` - ` it
/// also matches every package whose name starts with this one
/// (rust-gix's review is the 52nd match of the gix-* reviews), so that
/// search is the fallback, for a summary with another separator.
pub async fn find_review_bug(
    bz: &BzClient,
    rpm_name: &str,
) -> Result<Option<sandogasa_bugzilla::models::Bug>, String> {
    let mut matched =
        review_requests(bz, rpm_name, &format!("Review Request: {rpm_name} - ")).await?;
    if matched.is_empty() {
        matched = review_requests(bz, rpm_name, &format!("Review Request: {rpm_name}")).await?;
    }

    // Prefer the latest open bug, else the latest closed one. A
    // durable record would want the user to pick between candidates;
    // here the link is cheap to correct, so the newest open one wins.
    let is_open = |b: &&sandogasa_bugzilla::models::Bug| b.status != "CLOSED";
    Ok(matched
        .iter()
        .filter(is_open)
        .max_by_key(|b| b.id)
        .or_else(|| matched.iter().max_by_key(|b| b.id))
        .cloned())
}

/// [`find_review_bug`] for each of `names`, a few at a time, in the
/// order given.
pub async fn find_review_bugs(
    bz: &BzClient,
    names: &[String],
) -> Vec<(
    String,
    Result<Option<sandogasa_bugzilla::models::Bug>, String>,
)> {
    const PARALLEL: usize = 4;
    let mut out = Vec::with_capacity(names.len());
    for wave in names.chunks(PARALLEL) {
        let mut set = tokio::task::JoinSet::new();
        for (i, name) in wave.iter().enumerate() {
            let bz = bz.clone();
            let name = name.clone();
            set.spawn(async move {
                let found = find_review_bug(&bz, &name).await;
                (i, name, found)
            });
        }
        let mut done = Vec::with_capacity(wave.len());
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(r) => done.push(r),
                Err(e) => panic!("review search task failed: {e}"),
            }
        }
        done.sort_by_key(|(i, _, _)| *i);
        out.extend(done.into_iter().map(|(_, name, found)| (name, found)));
    }
    out
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

/// Write each change's Depends On to its review bug, stopping at the
/// first that cannot be confirmed.
async fn apply_changes(bz: &BzClient, changes: &[LinkChange], verbose: bool) -> Result<(), String> {
    for change in changes {
        if verbose {
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

        let out = bz
            .update_verified(change.bug_id, &serde_json::Value::Object(update), 3)
            .await;
        if let Some(note) = out.note() {
            eprintln!("note: bug {}: {note}", change.bug_id);
        }
        if !out.complete() {
            return Err(format!(
                "failed to update bug {}: {}",
                change.bug_id,
                out.last_error.unwrap_or_default()
            ));
        }
    }

    Ok(())
}

/// The summary of review bugs found and changes proposed.
fn summary(
    changes: &[LinkChange],
    bug_map: &BTreeMap<String, (u64, String)>,
    not_found: &[String],
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
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
    let _ = writeln!(out, "Review bugs found:\n");
    for (pkg, (id, status)) in bug_map {
        let _ = writeln!(out, "  {pkg}: bug {id} ({status})");
    }

    if !changes.is_empty() {
        let _ = writeln!(out, "\nProposed changes:\n");
        for c in changes {
            let _ = writeln!(out, "  Bug {} ({}):", c.bug_id, c.package);
            for id in &c.add_depends {
                let _ = writeln!(out, "    + Depends On: {}", bug_label(*id));
            }
            for id in &c.remove_depends {
                let _ = writeln!(out, "    - Depends On: {}", bug_label(*id));
            }
        }
    }

    if !not_found.is_empty() {
        let _ = writeln!(out, "\nReview bugs not found:");
        for name in not_found {
            let _ = writeln!(out, "  - {name}");
        }
    }

    let found = bug_map.len();
    let total = found + not_found.len();
    let _ = writeln!(
        out,
        "\n{found}/{total} review bug(s) found, {} change(s) needed.",
        changes.len()
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The review search asks for the flags check-wip reads, against a
    /// mock that, like Bugzilla, sends only the fields asked for
    /// (#123), and matches the package exactly.
    #[tokio::test]
    async fn the_review_search_carries_the_review_flag() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer};
        let server = MockServer::start().await;
        let review = |id: u64, summary: &str| {
            serde_json::json!({
                "id": id, "summary": summary, "status": "POST", "resolution": "",
                "product": "Fedora", "component": ["Package Review"],
                "severity": "medium", "priority": "medium",
                "assigned_to": "r@example.com", "creator": "p@example.com",
                "creation_time": "2026-01-01T00:00:00Z",
                "last_change_time": "2026-01-01T00:00:00Z",
                "flags": [{"name": "fedora-review", "status": "+"}],
                "cc": ["x@example.com"], "depends_on": [1]
            })
        };
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with({
                let bugs: Vec<serde_json::Value> = vec![
                    review(10, "Review Request: rust-foo-bar - Foo bar"),
                    review(11, "Review Request: rust-foo - Foo"),
                ];
                move |r: &wiremock::Request| {
                    wiremock::ResponseTemplate::new(200).set_body_json(
                        sandogasa_bugzilla::test_support::answer(&bugs, r.url.query()),
                    )
                }
            })
            .mount(&server)
            .await;
        let bz = BzClient::new(&server.uri());
        let bug = find_review_bug(&bz, "rust-foo").await.unwrap().unwrap();
        assert_eq!(bug.id, 11);
        assert!(
            bug.flags
                .iter()
                .any(|f| f.name == "fedora-review" && f.status == "+"),
            "{:?}",
            bug.flags
        );
        assert!(bug.cc.is_empty(), "not asked for, so not sent");
    }

    /// A mock Package Review search that, like Bugzilla, answers only
    /// the reviews whose summary contains the `short_desc` asked for,
    /// oldest first, and records each `short_desc`.
    async fn review_server(
        summaries: &[(u64, &str, &str)],
    ) -> (
        wiremock::MockServer,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let server = wiremock::MockServer::start().await;
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut bugs: Vec<serde_json::Value> = summaries
            .iter()
            .map(|(id, status, summary)| {
                serde_json::json!({
                    "id": id, "summary": summary, "status": status, "resolution": "",
                    "product": "Fedora", "component": ["Package Review"],
                    "severity": "medium", "priority": "medium",
                    "assigned_to": "r@example.com", "creator": "p@example.com",
                    "creation_time": "2026-01-01T00:00:00Z",
                    "last_change_time": "2026-01-01T00:00:00Z", "flags": []
                })
            })
            .collect();
        bugs.sort_by_key(|b| b["id"].as_u64());
        let log = asked.clone();
        wiremock::Mock::given(wiremock::matchers::path("/rest/bug"))
            .respond_with(move |r: &wiremock::Request| {
                let text = r
                    .url
                    .query_pairs()
                    .find(|(k, _)| k == "short_desc")
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default();
                log.lock().unwrap().push(text.clone());
                let hits: Vec<_> = bugs
                    .iter()
                    .filter(|b| b["summary"].as_str().unwrap().contains(&text))
                    .cloned()
                    .collect();
                wiremock::ResponseTemplate::new(200).set_body_json(
                    sandogasa_bugzilla::test_support::answer(&hits, r.url.query()),
                )
            })
            .mount(&server)
            .await;
        (server, asked)
    }

    /// rust-gix's review came after a dozen gix-* reviews: a search
    /// capped at the first ten, oldest first, never saw it (#129).
    #[tokio::test]
    async fn a_review_after_many_with_the_name_as_a_prefix_is_found() {
        let siblings: Vec<String> = (1..=15)
            .map(|i| format!("Review Request: rust-gix-part{i} - Part"))
            .collect();
        let mut summaries: Vec<(u64, &str, &str)> = siblings
            .iter()
            .enumerate()
            .map(|(i, s)| (i as u64 + 1, "CLOSED", s.as_str()))
            .collect();
        summaries.push((52, "CLOSED", "Review Request: rust-gix - Interact with git"));
        let (server, asked) = review_server(&summaries).await;
        let bz = BzClient::new(&server.uri());
        let bug = find_review_bug(&bz, "rust-gix").await.unwrap().unwrap();
        assert_eq!(bug.id, 52);
        assert_eq!(*asked.lock().unwrap(), ["Review Request: rust-gix - "]);
    }

    /// rust-futures was reviewed twice; the newer review is the one.
    #[tokio::test]
    async fn the_newest_of_several_reviews_is_taken() {
        let (server, _) = review_server(&[
            (1509540, "CLOSED", "Review Request: rust-futures - Futures"),
            (1783471, "CLOSED", "Review Request: rust-futures - Futures"),
            (
                1900000,
                "CLOSED",
                "Review Request: rust-futures-util - Util",
            ),
        ])
        .await;
        let bz = BzClient::new(&server.uri());
        let bug = find_review_bug(&bz, "rust-futures").await.unwrap().unwrap();
        assert_eq!(bug.id, 1783471);
    }

    /// A summary with another separator is found by the broader
    /// search, still matched by exact name.
    #[tokio::test]
    async fn a_summary_without_the_dash_is_found_by_the_fallback() {
        let (server, asked) = review_server(&[
            (1, "CLOSED", "Review Request: rust-foo-bar - Foo bar"),
            (2, "NEW", "Review Request: rust-foo – Foo"),
        ])
        .await;
        let bz = BzClient::new(&server.uri());
        let bug = find_review_bug(&bz, "rust-foo").await.unwrap().unwrap();
        assert_eq!(bug.id, 2);
        assert_eq!(
            *asked.lock().unwrap(),
            ["Review Request: rust-foo - ", "Review Request: rust-foo"]
        );
        assert!(find_review_bug(&bz, "rust-baz").await.unwrap().is_none());
    }

    /// A `+` in a name reaches Bugzilla as a `+`, not a space.
    #[tokio::test]
    async fn a_name_with_a_plus_is_searched_as_written() {
        let (server, asked) = review_server(&[(3, "NEW", "Review Request: foo++ - Foo")]).await;
        let bz = BzClient::new(&server.uri());
        let bug = find_review_bug(&bz, "foo++").await.unwrap().unwrap();
        assert_eq!(bug.id, 3);
        assert_eq!(*asked.lock().unwrap(), ["Review Request: foo++ - "]);
    }

    #[tokio::test]
    async fn several_reviews_are_searched_and_answered_in_order() {
        let (server, _) = review_server(&[
            (1, "NEW", "Review Request: a - A"),
            (2, "NEW", "Review Request: c - C"),
            (3, "NEW", "Review Request: e - E"),
            (4, "NEW", "Review Request: f - F"),
        ])
        .await;
        let bz = BzClient::new(&server.uri());
        let names: Vec<String> = ["a", "b", "c", "d", "e", "f"].map(String::from).into();
        let found: Vec<(String, Option<u64>)> = find_review_bugs(&bz, &names)
            .await
            .into_iter()
            .map(|(n, r)| (n, r.unwrap().map(|b| b.id)))
            .collect();
        let want: Vec<(String, Option<u64>)> = [
            ("a", Some(1)),
            ("b", None),
            ("c", Some(2)),
            ("d", None),
            ("e", Some(3)),
            ("f", Some(4)),
        ]
        .map(|(n, id)| (n.to_string(), id))
        .into();
        assert_eq!(found, want);
    }

    /// A report for rust-my-crate, missing dep-a, which pulls in the
    /// missing dep-b and dep-c.
    fn report_with_missing_deps(review_bugs: &[(&str, u64)]) -> CheckCrateReport {
        let transitive = |name: &str| check_crate::TransitiveDep {
            name: name.to_string(),
            package: format!("rust-{name}"),
            status: check_crate::TransitiveStatus::Missing,
            version: "0.5.0".to_string(),
            version_req: "^0.5".to_string(),
            pulled_by: "dep-a".to_string(),
        };
        CheckCrateReport {
            crate_name: "my-crate".to_string(),
            crate_version: "1.0.0".to_string(),
            package: "rust-my-crate".to_string(),
            branch: "rawhide".to_string(),
            already_built: None,
            dependencies: vec![check_crate::DepResult {
                dep: check_crate::CrateDep {
                    name: "dep-a".to_string(),
                    version_req: "^1.0".to_string(),
                    kind: "normal".to_string(),
                    optional: false,
                },
                status: DepStatus::Missing,
                via: None,
            }],
            in_tree: vec![],
            transitive_missing: vec![transitive("dep-b"), transitive("dep-c")],
            transitive_staged: vec![],
            transitive_build_order: vec![],
            transitive_edges: BTreeMap::from([
                (
                    "dep-a".to_string(),
                    BTreeSet::from(["dep-b".to_string(), "dep-c".to_string()]),
                ),
                ("dep-b".to_string(), BTreeSet::new()),
                ("dep-c".to_string(), BTreeSet::new()),
            ]),
            review_bugs: review_bugs
                .iter()
                .map(|(n, id)| (n.to_string(), *id))
                .collect(),
            branch_requests: BTreeMap::new(),
            copr: None,
        }
    }

    /// A review bug as Bugzilla sends it.
    fn review_bug(id: u64, summary: &str, depends_on: &[u64]) -> serde_json::Value {
        serde_json::json!({
            "id": id, "summary": summary, "status": "NEW", "resolution": "",
            "product": "Fedora", "component": ["Package Review"],
            "severity": "medium", "priority": "medium",
            "assigned_to": "r@example.com", "creator": "p@example.com",
            "creation_time": "2026-01-01T00:00:00Z",
            "last_change_time": "2026-01-01T00:00:00Z",
            "flags": [], "depends_on": depends_on
        })
    }

    /// A mock Bugzilla answering both a search, by summary, and a read
    /// by ids.
    async fn bugzilla_with(bugs: Vec<serde_json::Value>) -> wiremock::MockServer {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/rest/bug"))
            .respond_with(move |r: &wiremock::Request| {
                let pairs: Vec<(String, String)> = r.url.query_pairs().into_owned().collect();
                let ids: Vec<u64> = pairs
                    .iter()
                    .filter(|(k, _)| k == "id")
                    .map(|(_, v)| v.parse().unwrap())
                    .collect();
                let text = pairs
                    .iter()
                    .find(|(k, _)| k == "short_desc")
                    .map(|(_, v)| v);
                let hits: Vec<_> = bugs
                    .iter()
                    .filter(|b| match text {
                        Some(t) => b["summary"].as_str().unwrap().contains(t.as_str()),
                        None => ids.contains(&b["id"].as_u64().unwrap()),
                    })
                    .cloned()
                    .collect();
                wiremock::ResponseTemplate::new(200).set_body_json(
                    sandogasa_bugzilla::test_support::answer(&hits, r.url.query()),
                )
            })
            .mount(&server)
            .await;
        server
    }

    /// A dry run finds the reviews, the cached one by id and the rest
    /// by search, records them in the report, and writes nothing.
    #[tokio::test]
    async fn a_dry_run_records_the_reviews_found_and_changes_nothing() {
        let server = bugzilla_with(vec![
            review_bug(10, "Review Request: rust-my-crate - Mine", &[99]),
            review_bug(20, "Review Request: rust-dep-a - A", &[]),
            review_bug(30, "Review Request: rust-dep-b - B", &[]),
        ])
        .await;
        // No PUT is mounted, so a write would fail the run.
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("report.toml").display().to_string();
        check_crate::write_toml(&report_with_missing_deps(&[("rust-dep-b", 30)]), &toml_path)
            .unwrap();
        let opts = CheckPkgReviewsOptions {
            toml_path: toml_path.clone(),
            bugzilla_url: server.uri(),
            api_key: "key".to_string(),
            dry_run: true,
            verbose: true,
        };
        check_pkg_reviews_async(&opts).await.unwrap();
        let saved = check_crate::load_report(&toml_path).unwrap();
        assert_eq!(
            saved.review_bugs,
            BTreeMap::from([
                ("rust-dep-a".to_string(), 20),
                ("rust-dep-b".to_string(), 30),
                ("rust-my-crate".to_string(), 10),
            ])
        );
        let puts = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.method.as_str() != "GET")
            .count();
        assert_eq!(puts, 0);
    }

    /// A cached id Bugzilla no longer has is searched for instead.
    #[tokio::test]
    async fn a_cached_review_that_is_gone_is_searched_for() {
        let server =
            bugzilla_with(vec![review_bug(20, "Review Request: rust-dep-a - A", &[])]).await;
        let dir = tempfile::tempdir().unwrap();
        let toml_path = dir.path().join("report.toml").display().to_string();
        check_crate::write_toml(&report_with_missing_deps(&[("rust-dep-a", 7)]), &toml_path)
            .unwrap();
        let opts = CheckPkgReviewsOptions {
            toml_path: toml_path.clone(),
            bugzilla_url: server.uri(),
            api_key: "key".to_string(),
            dry_run: true,
            verbose: false,
        };
        check_pkg_reviews_async(&opts).await.unwrap();
        let saved = check_crate::load_report(&toml_path).unwrap();
        assert_eq!(saved.review_bugs.get("rust-dep-a"), Some(&20));
    }

    #[test]
    fn the_summary_names_each_bug_change_and_missing_review() {
        let bug_map = BTreeMap::from([
            ("rust-a".to_string(), (1, "NEW".to_string())),
            ("rust-b".to_string(), (2, "POST".to_string())),
        ]);
        let changes = [LinkChange {
            bug_id: 1,
            package: "rust-a".to_string(),
            add_depends: vec![2],
            remove_depends: vec![9],
        }];
        let text = summary(&changes, &bug_map, &["rust-c".to_string()]);
        for want in [
            "  rust-a: bug 1 (NEW)",
            "  rust-b: bug 2 (POST)",
            "  Bug 1 (rust-a):",
            "    + Depends On: 2 (rust-b)",
            "    - Depends On: 9",
            "Review bugs not found:\n  - rust-c",
            "2/3 review bug(s) found, 1 change(s) needed.",
        ] {
            assert!(text.contains(want), "{want:?} not in:\n{text}");
        }
        let none = summary(&[], &bug_map, &[]);
        assert!(!none.contains("Proposed changes"), "{none}");
        assert!(!none.contains("not found"), "{none}");
    }

    /// Each change is written as a Depends On add and remove.
    #[tokio::test]
    async fn changes_are_written_as_depends_on_edits() {
        let server = bugzilla_with(vec![review_bug(1, "Review Request: rust-a - A", &[9])]).await;
        wiremock::Mock::given(wiremock::matchers::method("PUT"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({"bugs": []})),
            )
            .mount(&server)
            .await;
        let bz = BzClient::new(&server.uri())
            .with_api_key("key".to_string())
            .unwrap();
        let changes = [LinkChange {
            bug_id: 1,
            package: "rust-a".to_string(),
            add_depends: vec![2],
            remove_depends: vec![9],
        }];
        apply_changes(&bz, &changes, true).await.unwrap();
        let put = server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.method.as_str() == "PUT")
            .unwrap();
        assert_eq!(put.url.path(), "/rest/bug/1");
        let body: serde_json::Value = serde_json::from_slice(&put.body).unwrap();
        assert_eq!(
            body["depends_on"],
            serde_json::json!({"add": [2], "remove": [9]})
        );
    }

    /// A write that cannot be confirmed stops the run with an error.
    #[tokio::test]
    async fn a_write_that_fails_is_an_error() {
        let server = bugzilla_with(vec![review_bug(1, "Review Request: rust-a - A", &[])]).await;
        wiremock::Mock::given(wiremock::matchers::method("PUT"))
            .respond_with(wiremock::ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let bz = BzClient::new(&server.uri())
            .with_api_key("key".to_string())
            .unwrap()
            .with_retry_backoff(std::time::Duration::ZERO);
        let changes = [LinkChange {
            bug_id: 1,
            package: "rust-a".to_string(),
            add_depends: vec![2],
            remove_depends: vec![],
        }];
        let e = apply_changes(&bz, &changes, false).await.unwrap_err();
        assert!(e.starts_with("failed to update bug 1"), "{e}");
    }

    #[test]
    fn collect_packages_includes_root() {
        let report = CheckCrateReport {
            crate_name: "my-crate".to_string(),
            crate_version: "1.0.0".to_string(),
            package: "rust-my-crate".to_string(),
            branch: "rawhide".to_string(),
            already_built: None,
            dependencies: vec![check_crate::DepResult {
                dep: check_crate::CrateDep {
                    name: "dep-a".to_string(),
                    version_req: "^1.0".to_string(),
                    kind: "normal".to_string(),
                    optional: false,
                },
                status: DepStatus::Missing,
                via: None,
            }],
            in_tree: vec![],
            transitive_missing: vec![check_crate::TransitiveDep {
                name: "dep-b".to_string(),
                package: "rust-dep-b".to_string(),
                status: check_crate::TransitiveStatus::Missing,
                version: "0.5.0".to_string(),
                version_req: "^0.5".to_string(),
                pulled_by: "dep-a".to_string(),
            }],
            transitive_staged: vec![],
            transitive_build_order: vec![],
            transitive_edges: BTreeMap::from([
                ("dep-a".to_string(), BTreeSet::from(["dep-b".to_string()])),
                ("dep-b".to_string(), BTreeSet::new()),
            ]),
            review_bugs: BTreeMap::new(),
            branch_requests: BTreeMap::new(),
            copr: None,
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
