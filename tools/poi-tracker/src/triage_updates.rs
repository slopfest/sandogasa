// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `triage-updates` subcommand.
//!
//! For each package in the inventory with a resolved Bugzilla
//! priority — either an explicit `priority` field on the package
//! or a `default_priority` inherited from a workload — find that
//! component's OPEN release-monitoring bugs (those filed by
//! `upstream-release-monitoring@fedoraproject.org`) and raise
//! their `priority` field. Existing non-`unspecified` priorities
//! are left alone so a human triager who already set a value
//! isn't stomped.

use std::collections::BTreeMap;

use sandogasa_bugzilla::BzClient;
use sandogasa_bugzilla::models::Bug;
use sandogasa_inventory::{Inventory, Priority};

/// Reporter address for Fedora's release-monitoring bot.
/// Anitya / the-new-hotness opens a new bug under this account
/// every time a tracked package gets a new upstream release.
pub const RELEASE_MONITORING_REPORTER: &str = "upstream-release-monitoring@fedoraproject.org";

/// Bugzilla products release-monitoring files bugs against.
/// We query both because some EPEL packages live under
/// `Fedora EPEL`, not `Fedora`.
pub const PRODUCTS: &[&str] = &["Fedora", "Fedora EPEL"];

/// One planned `(bug_id → new_priority)` change.
#[derive(Debug, Clone)]
pub struct PriorityUpdate {
    pub bug_id: u64,
    pub component: String,
    pub summary: String,
    pub current_priority: String,
    pub target_priority: Priority,
}

/// Per-package decision after scanning Bugzilla — useful for
/// `--verbose` output even when there's nothing to do.
#[derive(Debug)]
pub enum PackageOutcome {
    /// Inventory specifies no priority for this package.
    NoPriority,
    /// Priority resolves to `unspecified` (explicit opt-out).
    OptedOut,
    /// Bugzilla returned no matching bugs.
    NoBugs,
    /// All matching bugs already carry a non-default priority.
    AllAlreadyTriaged(usize),
    /// One or more bugs queued for update.
    Updates(Vec<PriorityUpdate>),
}

/// Decide what to do for one package: which (if any) Bugzilla
/// updates are queued. Pure function over a fetched bug list so
/// it's straightforward to unit-test.
pub fn plan_package(package: &str, resolved: Option<Priority>, bugs: &[Bug]) -> PackageOutcome {
    let target = match resolved {
        None => return PackageOutcome::NoPriority,
        Some(Priority::Unspecified) => return PackageOutcome::OptedOut,
        Some(p) => p,
    };
    if bugs.is_empty() {
        return PackageOutcome::NoBugs;
    }
    let mut updates = Vec::new();
    let mut already_triaged = 0usize;
    for bug in bugs {
        if bug.priority != "unspecified" {
            already_triaged += 1;
            continue;
        }
        updates.push(PriorityUpdate {
            bug_id: bug.id,
            component: package.to_string(),
            summary: bug.summary.clone(),
            current_priority: bug.priority.clone(),
            target_priority: target,
        });
    }
    if updates.is_empty() {
        PackageOutcome::AllAlreadyTriaged(already_triaged)
    } else {
        PackageOutcome::Updates(updates)
    }
}

/// Build the Bugzilla search query for one component.
///
/// Returns a `&`-joined query string ready to pass to
/// `BzClient::search`. Filters:
/// - `component=<package>` (exact match on the component)
/// - `product=Fedora` and `product=Fedora EPEL` (multi-product)
/// - `reporter=upstream-release-monitoring@fedoraproject.org`
/// - `bug_status=__open__` (Bugzilla's open-states sentinel)
///
/// We accept the default payload rather than narrowing with
/// `include_fields=…` because the shared `Bug` model in
/// `sandogasa-bugzilla` deserializes several required fields
/// (`severity`, `resolution`, `creation_time`, …) that aren't
/// in any tight projection.
pub fn bug_search_query(component: &str) -> String {
    let mut parts: Vec<String> = vec![
        format!("component={}", urlencode(component)),
        format!("reporter={}", urlencode(RELEASE_MONITORING_REPORTER)),
        "bug_status=__open__".to_string(),
    ];
    for product in PRODUCTS {
        parts.push(format!("product={}", urlencode(product)));
    }
    parts.join("&")
}

/// Bugzilla expects standard URL encoding. We could pull in
/// `percent-encoding`, but the only characters we ever encode in
/// these search queries are spaces, `@`, and `+`. Keep it tight
/// and dependency-free.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Group planned updates by component for the rendered preview.
pub fn group_by_component(updates: &[PriorityUpdate]) -> BTreeMap<String, Vec<&PriorityUpdate>> {
    let mut out: BTreeMap<String, Vec<&PriorityUpdate>> = BTreeMap::new();
    for u in updates {
        out.entry(u.component.clone()).or_default().push(u);
    }
    out
}

/// Run the whole `sync-priorities` flow.
///
/// Loads the inventories (already merged by the caller), iterates
/// every package, queries Bugzilla, plans updates, prints them,
/// optionally prompts, then applies. `dry_run = true` short-
/// circuits before any PUT.
pub async fn run(
    inventory: &Inventory,
    client: &BzClient,
    dry_run: bool,
    yes: bool,
    verbose: bool,
) -> Result<RunReport, String> {
    let mut all_updates: Vec<PriorityUpdate> = Vec::new();
    let mut packages_with_priority = 0usize;

    for pkg in &inventory.package {
        let resolved = inventory.priority_for(&pkg.name);
        match resolved {
            None => {
                if verbose {
                    eprintln!("[poi-tracker] {}: no priority configured", pkg.name);
                }
                continue;
            }
            Some(Priority::Unspecified) => {
                if verbose {
                    eprintln!("[poi-tracker] {}: priority=unspecified (opt-out)", pkg.name);
                }
                continue;
            }
            Some(_) => {
                packages_with_priority += 1;
            }
        }

        if verbose {
            eprintln!(
                "[poi-tracker] {}: searching release-monitoring bugs (target: {})",
                pkg.name,
                resolved.unwrap().as_bugzilla_str()
            );
        }
        let query = bug_search_query(&pkg.name);
        let bugs = client
            .search(&query, 0)
            .await
            .map_err(|e| format!("Bugzilla search for {}: {e}", pkg.name))?;
        match plan_package(&pkg.name, resolved, &bugs) {
            PackageOutcome::NoPriority | PackageOutcome::OptedOut => {}
            PackageOutcome::NoBugs => {
                if verbose {
                    eprintln!(
                        "[poi-tracker] {}: no open release-monitoring bugs",
                        pkg.name
                    );
                }
            }
            PackageOutcome::AllAlreadyTriaged(n) => {
                if verbose {
                    eprintln!(
                        "[poi-tracker] {}: {n} open bug(s) already triaged",
                        pkg.name
                    );
                }
            }
            PackageOutcome::Updates(updates) => {
                all_updates.extend(updates);
            }
        }
    }

    print_plan(&all_updates);

    let report = RunReport {
        packages_with_priority,
        updates_planned: all_updates.len(),
        updates_applied: 0,
        failures: 0,
    };

    if all_updates.is_empty() {
        return Ok(report);
    }
    if dry_run {
        eprintln!("\n(dry-run: not applying)");
        return Ok(report);
    }
    if !yes && !confirm(&format!("\nApply {} update(s)?", all_updates.len()))? {
        eprintln!("aborted.");
        return Ok(report);
    }

    let mut applied = 0usize;
    let mut failures = 0usize;
    for u in &all_updates {
        let body = serde_json::json!({"priority": u.target_priority.as_bugzilla_str()});
        match client.update(u.bug_id, &body).await {
            Ok(()) => {
                applied += 1;
                eprintln!(
                    "updated bug {} ({}): {} -> {}",
                    u.bug_id,
                    u.component,
                    u.current_priority,
                    u.target_priority.as_bugzilla_str()
                );
            }
            Err(e) => {
                failures += 1;
                eprintln!("error: bug {} ({}): {e}", u.bug_id, u.component);
            }
        }
    }
    Ok(RunReport {
        packages_with_priority,
        updates_planned: all_updates.len(),
        updates_applied: applied,
        failures,
    })
}

/// Summary returned from `run` so the caller can pick an exit
/// code without re-counting.
#[derive(Debug, Default)]
pub struct RunReport {
    pub packages_with_priority: usize,
    pub updates_planned: usize,
    pub updates_applied: usize,
    pub failures: usize,
}

fn print_plan(updates: &[PriorityUpdate]) {
    if updates.is_empty() {
        println!("Nothing to update.");
        return;
    }
    println!("Planned priority updates:");
    let grouped = group_by_component(updates);
    for (component, entries) in &grouped {
        println!(
            "  {component} ({} bug(s) → {}):",
            entries.len(),
            entries[0].target_priority.as_bugzilla_str()
        );
        for u in entries {
            println!(
                "    bug {} [{}]: {}",
                u.bug_id, u.current_priority, u.summary
            );
        }
    }
    println!("\nTotal: {} update(s).", updates.len());
}

fn confirm(prompt: &str) -> Result<bool, String> {
    use std::io::{BufRead, Write};
    eprint!("{prompt} [y/N]: ");
    std::io::stderr().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(line.trim().eq_ignore_ascii_case("y"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct a `Bug` via serde so the test doesn't need a
    /// direct chrono dep (the `creation_time` field deserializes
    /// from a string).
    fn make_bug(id: u64, priority: &str, summary: &str) -> Bug {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "summary": summary,
            "status": "NEW",
            "resolution": "",
            "product": "Fedora",
            "component": ["python-django"],
            "severity": "unspecified",
            "priority": priority,
            "assigned_to": "nobody@fedoraproject.org",
            "creator": RELEASE_MONITORING_REPORTER,
            "creation_time": "2026-05-01T00:00:00Z",
            "last_change_time": "2026-05-01T00:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn plan_no_resolved_priority_is_no_priority() {
        let outcome = plan_package("any", None, &[make_bug(1, "unspecified", "x")]);
        assert!(matches!(outcome, PackageOutcome::NoPriority));
    }

    #[test]
    fn plan_explicit_unspecified_is_opt_out() {
        let outcome = plan_package(
            "any",
            Some(Priority::Unspecified),
            &[make_bug(1, "unspecified", "x")],
        );
        assert!(matches!(outcome, PackageOutcome::OptedOut));
    }

    #[test]
    fn plan_no_bugs_returns_no_bugs() {
        let outcome = plan_package("any", Some(Priority::High), &[]);
        assert!(matches!(outcome, PackageOutcome::NoBugs));
    }

    #[test]
    fn plan_updates_only_unspecified_bugs() {
        let bugs = vec![
            make_bug(1, "unspecified", "django 5.1.3 is available"),
            make_bug(2, "low", "django 5.1.2 is available"),
            make_bug(3, "unspecified", "django 5.0.9 is available"),
            make_bug(4, "urgent", "django 4.2.16 is available"),
        ];
        let outcome = plan_package("python-django", Some(Priority::High), &bugs);
        match outcome {
            PackageOutcome::Updates(updates) => {
                assert_eq!(updates.len(), 2);
                let ids: Vec<u64> = updates.iter().map(|u| u.bug_id).collect();
                assert_eq!(ids, vec![1, 3]);
                assert!(updates.iter().all(|u| u.target_priority == Priority::High));
            }
            other => panic!("expected Updates, got {other:?}"),
        }
    }

    #[test]
    fn plan_all_already_triaged() {
        let bugs = vec![make_bug(1, "low", "x"), make_bug(2, "medium", "y")];
        let outcome = plan_package("any", Some(Priority::High), &bugs);
        match outcome {
            PackageOutcome::AllAlreadyTriaged(n) => assert_eq!(n, 2),
            other => panic!("expected AllAlreadyTriaged, got {other:?}"),
        }
    }

    #[test]
    fn bug_search_query_includes_required_filters() {
        let q = bug_search_query("python-django");
        assert!(q.contains("component=python-django"));
        assert!(q.contains("bug_status=__open__"));
        assert!(q.contains("product=Fedora"));
        assert!(q.contains("product=Fedora%20EPEL"));
        assert!(q.contains("reporter=upstream-release-monitoring%40fedoraproject.org"));
    }

    #[test]
    fn group_by_component_groups_and_orders() {
        let updates = vec![
            PriorityUpdate {
                bug_id: 1,
                component: "python-django".into(),
                summary: "a".into(),
                current_priority: "unspecified".into(),
                target_priority: Priority::High,
            },
            PriorityUpdate {
                bug_id: 2,
                component: "ansible".into(),
                summary: "b".into(),
                current_priority: "unspecified".into(),
                target_priority: Priority::Medium,
            },
            PriorityUpdate {
                bug_id: 3,
                component: "python-django".into(),
                summary: "c".into(),
                current_priority: "unspecified".into(),
                target_priority: Priority::High,
            },
        ];
        let grouped = group_by_component(&updates);
        // BTreeMap iteration order is alphabetical.
        let keys: Vec<&String> = grouped.keys().collect();
        assert_eq!(keys, vec!["ansible", "python-django"]);
        assert_eq!(grouped["python-django"].len(), 2);
        assert_eq!(grouped["ansible"].len(), 1);
    }
}
