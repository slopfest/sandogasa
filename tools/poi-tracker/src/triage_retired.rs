// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `triage-retired` subcommand.
//!
//! For each package in the inventory, check whether it's retired
//! on the configured dist-git branch (a `dead.package` file
//! present on that branch). Retired packages have no live spec
//! to update, so any open release-monitoring bug filed against
//! that branch can be closed as `CANTFIX`.

use std::collections::BTreeMap;

use sandogasa_bugzilla::BzClient;
use sandogasa_bugzilla::models::Bug;
use sandogasa_distgit::DistGitClient;
use sandogasa_inventory::Inventory;

/// Anitya / the-new-hotness reporter address.
pub const RELEASE_MONITORING_REPORTER: &str = "upstream-release-monitoring@fedoraproject.org";

/// One planned bug close.
#[derive(Debug, Clone)]
pub struct BugClose {
    pub bug_id: u64,
    pub component: String,
    /// Dist-git branch whose retirement justifies the closure.
    pub branch: String,
    pub summary: String,
    pub current_status: String,
}

/// Per-package outcome from planning.
#[derive(Debug)]
pub enum PackageOutcome {
    /// The package is still live on this branch — nothing to do.
    NotRetired,
    /// Retired, but no open release-monitoring bugs for this
    /// branch.
    RetiredNoBugs,
    /// One or more bugs queued for closure.
    RetiredClose(Vec<BugClose>),
}

/// Decide what to do for one package on one branch: which (if
/// any) open bugs to close. Pure function over the dist-git check
/// + fetched bug list so it's easy to unit-test.
pub fn plan_package(package: &str, branch: &str, retired: bool, bugs: &[Bug]) -> PackageOutcome {
    if !retired {
        return PackageOutcome::NotRetired;
    }
    let opens: Vec<BugClose> = bugs
        .iter()
        .filter(|b| b.status != "CLOSED")
        .map(|b| BugClose {
            bug_id: b.id,
            component: package.to_string(),
            branch: branch.to_string(),
            summary: b.summary.clone(),
            current_status: b.status.clone(),
        })
        .collect();
    if opens.is_empty() {
        PackageOutcome::RetiredNoBugs
    } else {
        PackageOutcome::RetiredClose(opens)
    }
}

/// Build the Bugzilla product + version pair to scope the search
/// to bugs filed against the dist-git branch being retired.
///
/// Conventions: `rawhide` maps to product `Fedora` / version
/// `rawhide`; anything starting with `epel` maps to `Fedora EPEL`
/// / `<branch>`; everything else maps to `Fedora` / `<branch>`
/// (so an `f43` branch retirement still narrows correctly).
pub fn product_version_for_branch(branch: &str) -> (&'static str, String) {
    if branch.starts_with("epel") {
        ("Fedora EPEL", branch.to_string())
    } else {
        ("Fedora", branch.to_string())
    }
}

/// Build the Bugzilla search query for retired-package triage:
/// the component's open bugs against the retirement branch's
/// product/version pair. By default the search is scoped to
/// release-monitoring bugs (the Anitya / the-new-hotness bot);
/// with `all_reporters` the reporter filter is dropped so every
/// open bug on the retired branch is matched, regardless of who
/// filed it.
pub fn bug_search_query(component: &str, branch: &str, all_reporters: bool) -> String {
    let (product, version) = product_version_for_branch(branch);
    let mut parts = vec![
        format!("component={}", urlencode(component)),
        format!("product={}", urlencode(product)),
        format!("version={}", urlencode(&version)),
    ];
    if !all_reporters {
        parts.push(format!(
            "reporter={}",
            urlencode(RELEASE_MONITORING_REPORTER)
        ));
    }
    parts.push("bug_status=__open__".to_string());
    parts.join("&")
}

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

/// Print one package's planned closures as soon as they're
/// known, so a long inventory run gives live feedback instead of
/// accumulating everything to a final block.
pub fn print_package_closes(component: &str, closes: &[BugClose]) {
    println!("{component} ({} bug(s)):", closes.len());
    for c in closes {
        println!(
            "  bug {} [{}] ({}): {}",
            c.bug_id, c.current_status, c.branch, c.summary
        );
    }
}

/// Whether a package should be included in this run given the
/// optional `--package` (only that name), `--start-from` (skip
/// earlier names in iteration order), and `--end-with` (stop
/// after this name, inclusive) filters. `--package` is mutually
/// exclusive with the range flags at the CLI layer;
/// `--start-from` and `--end-with` compose to bound a sub-range.
pub fn should_include(
    name: &str,
    only: Option<&str>,
    start_from: Option<&str>,
    end_with: Option<&str>,
) -> bool {
    if let Some(o) = only {
        return name == o;
    }
    if let Some(s) = start_from
        && name < s
    {
        return false;
    }
    if let Some(e) = end_with
        && name > e
    {
        return false;
    }
    true
}

/// Retry an async fallible operation a few times, sleeping a
/// little longer between each attempt. Used for transient
/// network failures from Pagure / Bugzilla — the failure
/// message includes the operation label so users can see what's
/// being retried.
pub async fn retry<F, Fut, T, E>(
    label: &str,
    attempts: usize,
    mut f: F,
    verbose: bool,
) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut last: Option<E> = None;
    for attempt in 1..=attempts {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt < attempts {
                    let backoff = 1u64 << (attempt - 1).min(4); // 1, 2, 4, 8s
                    if verbose {
                        eprintln!(
                            "[poi-tracker] {label} attempt {attempt}/{attempts} failed: {e}; \
                             retrying in {backoff}s"
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                }
                last = Some(e);
            }
        }
    }
    Err(last.expect("loop ran at least once"))
}

/// Default number of attempts for transient-network retries.
pub const RETRY_ATTEMPTS: usize = 3;

/// The comment body added when closing a retired-package bug.
pub fn close_comment(package: &str, branch: &str) -> String {
    format!(
        "Package `{package}` is retired on the `{branch}` dist-git \
         branch (the `dead.package` marker is present); closing as \
         CANTFIX since there's no live package to update."
    )
}

/// One `(package, branch)` retirement check performed during a
/// run, retained so `--mark` can record the results in the
/// inventory afterwards.
#[derive(Debug, Clone)]
pub struct BranchCheck {
    pub package: String,
    pub branch: String,
    pub retired: bool,
}

/// Apply branch-check results to the inventory's `retired_on`
/// markers: a checked-and-retired branch is added, a
/// checked-and-live branch removed (so un-retirement heals the
/// marker). Branches that weren't checked this run are left
/// alone. Returns how many packages changed.
pub fn apply_retirement_marks(inventory: &mut Inventory, checks: &[BranchCheck]) -> usize {
    let mut changed = 0usize;
    for pkg in &mut inventory.package {
        let mut branches = pkg.retired_on.clone().unwrap_or_default();
        let mut touched = false;
        for check in checks.iter().filter(|c| c.package == pkg.name) {
            touched = true;
            if check.retired {
                if !branches.contains(&check.branch) {
                    branches.push(check.branch.clone());
                }
            } else {
                branches.retain(|b| b != &check.branch);
            }
        }
        if !touched {
            continue;
        }
        branches.sort();
        branches.dedup();
        let new = (!branches.is_empty()).then_some(branches);
        if new != pkg.retired_on {
            pkg.retired_on = new;
            changed += 1;
        }
    }
    changed
}

/// Summary returned from `run` so the caller can pick an exit
/// code without re-counting.
#[derive(Debug, Default)]
pub struct RunReport {
    pub packages_checked: usize,
    pub packages_retired: usize,
    pub closes_planned: usize,
    pub closes_applied: usize,
    pub failures: usize,
    /// Every retirement check performed, for `--mark`.
    pub checks: Vec<BranchCheck>,
}

/// Run the whole `triage-retired` flow.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    inventory: &Inventory,
    bz: &BzClient,
    dg: &DistGitClient,
    branches: &[String],
    all_reporters: bool,
    only_package: Option<&str>,
    start_from: Option<&str>,
    end_with: Option<&str>,
    claim: bool,
    claim_email: Option<&str>,
    dry_run: bool,
    yes: bool,
    verbose: bool,
) -> Result<RunReport, String> {
    let mut all_closes: Vec<BugClose> = Vec::new();
    let mut packages_checked = 0usize;
    let mut packages_retired = 0usize;
    let mut checks: Vec<BranchCheck> = Vec::new();

    for pkg in &inventory.package {
        if !should_include(&pkg.name, only_package, start_from, end_with) {
            continue;
        }
        packages_checked += 1;
        // Each package is checked on every requested branch; a
        // package retired on one branch but live on another only
        // gets its bugs closed for the branch(es) where it's dead.
        let mut pkg_closes: Vec<BugClose> = Vec::new();
        let mut retired_anywhere = false;
        for branch in branches {
            if verbose {
                eprintln!(
                    "[poi-tracker] {}: checking retirement on {branch}",
                    pkg.name
                );
            }
            let retired = retry(
                &format!("is_retired({}, {branch})", pkg.name),
                RETRY_ATTEMPTS,
                || dg.is_retired(&pkg.name, branch),
                verbose,
            )
            .await
            .map_err(|e| format!("dist-git is_retired for {} on {branch}: {e}", pkg.name))?;
            checks.push(BranchCheck {
                package: pkg.name.clone(),
                branch: branch.clone(),
                retired,
            });
            if !retired {
                continue;
            }
            retired_anywhere = true;

            if verbose {
                eprintln!(
                    "[poi-tracker] {}: retired on {branch}, searching open bugs",
                    pkg.name
                );
            }
            let query = bug_search_query(&pkg.name, branch, all_reporters);
            let bugs = retry(
                &format!("bug search for {} on {branch}", pkg.name),
                RETRY_ATTEMPTS,
                || bz.search(&query, 0),
                verbose,
            )
            .await
            .map_err(|e| format!("Bugzilla search for {} on {branch}: {e}", pkg.name))?;
            match plan_package(&pkg.name, branch, true, &bugs) {
                PackageOutcome::NotRetired => unreachable!("retired check passed above"),
                PackageOutcome::RetiredNoBugs => {
                    if verbose {
                        eprintln!(
                            "[poi-tracker] {}: retired on {branch} but no open bugs to close",
                            pkg.name
                        );
                    }
                }
                PackageOutcome::RetiredClose(closes) => pkg_closes.extend(closes),
            }
        }
        if retired_anywhere {
            packages_retired += 1;
        }
        if !pkg_closes.is_empty() {
            print_package_closes(&pkg.name, &pkg_closes);
            all_closes.extend(pkg_closes);
        }
    }

    if all_closes.is_empty() {
        println!("No retired packages with open release-monitoring bugs.");
    } else {
        print_tally(&all_closes);
    }

    let mut report = RunReport {
        packages_checked,
        packages_retired,
        closes_planned: all_closes.len(),
        closes_applied: 0,
        failures: 0,
        checks,
    };

    if all_closes.is_empty() {
        return Ok(report);
    }
    if dry_run {
        eprintln!("\n(dry-run: not applying)");
        return Ok(report);
    }
    // Offer to claim ownership before the main confirm so the
    // user sees one prompt-then-confirm flow. With `--claim`,
    // skip the prompt. With `-y` and no `--claim`, don't claim.
    // Without a configured email there's nothing to assign to,
    // so just skip silently.
    let want_claim = if claim {
        claim_email.is_some()
    } else if yes || claim_email.is_none() {
        false
    } else {
        confirm(&format!(
            "Also claim ownership (assigned_to = {})?",
            claim_email.unwrap()
        ))?
    };
    let active_claim_email = if want_claim {
        let e = claim_email.unwrap();
        eprintln!("claiming ownership as {e}");
        Some(e.to_string())
    } else {
        None
    };

    if !yes && !confirm(&format!("\nClose {} bug(s) as CANTFIX?", all_closes.len()))? {
        eprintln!("aborted.");
        return Ok(report);
    }

    for c in &all_closes {
        let mut body = serde_json::json!({
            "status": "CLOSED",
            "resolution": "CANTFIX",
            "comment": { "body": close_comment(&c.component, &c.branch) },
        });
        if let Some(ref email) = active_claim_email {
            body["assigned_to"] = serde_json::json!(email);
        }
        match bz.update(c.bug_id, &body).await {
            Ok(()) => {
                report.closes_applied += 1;
                eprintln!(
                    "closed bug {} ({}): {} -> CLOSED/CANTFIX",
                    c.bug_id, c.component, c.current_status
                );
            }
            Err(e) => {
                report.failures += 1;
                eprintln!("error: bug {} ({}): {e}", c.bug_id, c.component);
            }
        }
    }
    Ok(report)
}

/// One-line-per-package recap printed after the loop, so the
/// reader can scan everything that's about to be closed (or that
/// was just closed) without scrolling back through the live
/// per-package blocks.
fn print_tally(closes: &[BugClose]) {
    let mut by_pkg: BTreeMap<&str, Vec<&BugClose>> = BTreeMap::new();
    for c in closes {
        by_pkg.entry(c.component.as_str()).or_default().push(c);
    }
    println!(
        "\nTotal: {} closure(s) across {} package(s):",
        closes.len(),
        by_pkg.len()
    );
    for (pkg, bugs) in &by_pkg {
        let ids: Vec<String> = bugs
            .iter()
            .map(|b| format!("rhbz#{} ({})", b.bug_id, b.branch))
            .collect();
        println!("  {pkg}: {}", ids.join(", "));
    }
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

    fn make_bug(id: u64, status: &str, summary: &str) -> Bug {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "summary": summary,
            "status": status,
            "resolution": "",
            "product": "Fedora",
            "component": ["foo"],
            "severity": "unspecified",
            "priority": "unspecified",
            "assigned_to": "nobody@fedoraproject.org",
            "creator": RELEASE_MONITORING_REPORTER,
            "creation_time": "2026-01-01T00:00:00Z",
            "last_change_time": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn plan_skips_live_packages() {
        let outcome = plan_package("foo", "rawhide", false, &[make_bug(1, "NEW", "x")]);
        assert!(matches!(outcome, PackageOutcome::NotRetired));
    }

    #[test]
    fn plan_no_bugs_when_retired_with_empty_search() {
        let outcome = plan_package("foo", "rawhide", true, &[]);
        assert!(matches!(outcome, PackageOutcome::RetiredNoBugs));
    }

    #[test]
    fn plan_closes_only_open_bugs_when_retired() {
        let bugs = vec![
            make_bug(1, "NEW", "foo 1.0 available"),
            make_bug(2, "ASSIGNED", "foo 0.9 available"),
            // The query filters to open already, but a defensive
            // check guards against a stray CLOSED slipping in.
            make_bug(3, "CLOSED", "foo 0.8 available"),
        ];
        let outcome = plan_package("foo", "epel9", true, &bugs);
        match outcome {
            PackageOutcome::RetiredClose(closes) => {
                assert_eq!(closes.len(), 2);
                let ids: Vec<u64> = closes.iter().map(|c| c.bug_id).collect();
                assert_eq!(ids, vec![1, 2]);
                // Each close is tagged with the branch it's for.
                assert!(closes.iter().all(|c| c.branch == "epel9"));
            }
            other => panic!("expected RetiredClose, got {other:?}"),
        }
    }

    #[test]
    fn product_version_picks_epel_for_epel_branches() {
        assert_eq!(
            product_version_for_branch("epel10"),
            ("Fedora EPEL", "epel10".to_string())
        );
        assert_eq!(
            product_version_for_branch("epel9"),
            ("Fedora EPEL", "epel9".to_string())
        );
    }

    #[test]
    fn product_version_picks_fedora_for_rawhide_and_fnn() {
        assert_eq!(
            product_version_for_branch("rawhide"),
            ("Fedora", "rawhide".to_string())
        );
        assert_eq!(
            product_version_for_branch("f43"),
            ("Fedora", "f43".to_string())
        );
    }

    #[test]
    fn bug_search_query_scopes_to_branch() {
        let q = bug_search_query("python-django6", "epel10", false);
        assert!(q.contains("component=python-django6"));
        assert!(q.contains("product=Fedora%20EPEL"));
        assert!(q.contains("version=epel10"));
        assert!(q.contains("bug_status=__open__"));
        assert!(q.contains("reporter=upstream-release-monitoring%40fedoraproject.org"));

        let q = bug_search_query("foo", "rawhide", false);
        assert!(q.contains("product=Fedora&"));
        assert!(q.contains("version=rawhide"));
    }

    #[test]
    fn bug_search_query_all_reporters_drops_reporter_filter() {
        let q = bug_search_query("python-django3", "epel8", true);
        assert!(q.contains("component=python-django3"));
        assert!(q.contains("product=Fedora%20EPEL"));
        assert!(q.contains("version=epel8"));
        assert!(q.contains("bug_status=__open__"));
        // No reporter scoping — every open bug on the branch matches.
        assert!(!q.contains("reporter="));
    }

    #[test]
    fn close_comment_mentions_package_and_branch() {
        let c = close_comment("python-django6", "epel10");
        assert!(c.contains("python-django6"));
        assert!(c.contains("epel10"));
        assert!(c.contains("CANTFIX"));
    }

    // ---- apply_retirement_marks ----

    fn check(package: &str, branch: &str, retired: bool) -> BranchCheck {
        BranchCheck {
            package: package.to_string(),
            branch: branch.to_string(),
            retired,
        }
    }

    fn inventory_with(packages: &[(&str, Option<Vec<&str>>)]) -> Inventory {
        let mut toml =
            String::from("[inventory]\nname = \"t\"\ndescription = \"t\"\nmaintainer = \"t\"\n");
        for (name, retired_on) in packages {
            toml.push_str(&format!("\n[[package]]\nname = \"{name}\"\n"));
            if let Some(branches) = retired_on {
                let list: Vec<String> = branches.iter().map(|b| format!("\"{b}\"")).collect();
                toml.push_str(&format!("retired_on = [{}]\n", list.join(", ")));
            }
        }
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn apply_marks_adds_retired_branches_sorted() {
        let mut inv = inventory_with(&[("foo", None)]);
        let changed = apply_retirement_marks(
            &mut inv,
            &[check("foo", "rawhide", true), check("foo", "epel8", true)],
        );
        assert_eq!(changed, 1);
        assert_eq!(
            inv.package[0].retired_on,
            Some(vec!["epel8".to_string(), "rawhide".to_string()])
        );
    }

    #[test]
    fn apply_marks_removes_unretired_branch_and_clears_empty() {
        // Un-retirement heals the marker; an emptied list drops
        // the field entirely.
        let mut inv = inventory_with(&[("foo", Some(vec!["rawhide"]))]);
        let changed = apply_retirement_marks(&mut inv, &[check("foo", "rawhide", false)]);
        assert_eq!(changed, 1);
        assert_eq!(inv.package[0].retired_on, None);
    }

    #[test]
    fn apply_marks_leaves_unchecked_branches_alone() {
        let mut inv = inventory_with(&[("foo", Some(vec!["epel8"]))]);
        let changed = apply_retirement_marks(&mut inv, &[check("foo", "rawhide", true)]);
        assert_eq!(changed, 1);
        assert_eq!(
            inv.package[0].retired_on,
            Some(vec!["epel8".to_string(), "rawhide".to_string()])
        );
    }

    #[test]
    fn apply_marks_no_change_counts_zero() {
        let mut inv = inventory_with(&[("foo", Some(vec!["rawhide"])), ("bar", None)]);
        // foo already marked; bar not checked at all.
        let changed = apply_retirement_marks(&mut inv, &[check("foo", "rawhide", true)]);
        assert_eq!(changed, 0);
        assert_eq!(inv.package[1].retired_on, None);
    }

    #[test]
    fn should_include_no_filters_keeps_everything() {
        assert!(should_include("foo", None, None, None));
        assert!(should_include("zzz", None, None, None));
    }

    #[test]
    fn should_include_only_matches_exact() {
        assert!(should_include("foo", Some("foo"), None, None));
        assert!(!should_include("foo-utils", Some("foo"), None, None));
        assert!(!should_include("foo", Some("bar"), None, None));
    }

    #[test]
    fn should_include_start_from_is_inclusive() {
        // Skip until name >= start_from; include start and onward.
        assert!(!should_include("apple", None, Some("mango"), None));
        assert!(should_include("mango", None, Some("mango"), None));
        assert!(should_include("zebra", None, Some("mango"), None));
    }

    #[test]
    fn should_include_end_with_is_inclusive() {
        // Include up to and including end_with; skip anything after.
        assert!(should_include("apple", None, None, Some("mango")));
        assert!(should_include("mango", None, None, Some("mango")));
        assert!(!should_include("zebra", None, None, Some("mango")));
    }

    #[test]
    fn should_include_range_bounds_both_inclusive() {
        // [start, end] inclusive sub-range — handy for "all rust-nu-*".
        let s = Some("rust-nu-cli");
        let e = Some("rust-nu-engine");
        assert!(!should_include("rust-itertools", None, s, e));
        assert!(should_include("rust-nu-cli", None, s, e));
        assert!(should_include("rust-nu-cmd-base", None, s, e));
        assert!(should_include("rust-nu-engine", None, s, e));
        assert!(!should_include("rust-nu-utils", None, s, e));
    }
}
