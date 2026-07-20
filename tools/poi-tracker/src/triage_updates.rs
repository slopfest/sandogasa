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
//!
//! Independently of priorities, every open release-monitoring bug
//! is also checked against Bodhi (unless `--skip-stale`): if
//! builds with the advertised version (or newer) already exist,
//! the latest addressing build per release is recorded in the
//! bug's Fixed In Version field, and the bug is closed as
//! `ERRATA` when the fix is stable in every active release the
//! package has a branch for, moved to `MODIFIED` while any
//! addressing update is still in testing, or — when only some
//! releases have the fix (commonly just rawhide, since stable
//! branches often intentionally stay behind) — offered for
//! closing interactively (`--close-stale` skips the prompt).
//!
//! Bodhi records updates per release, so a release can carry a
//! build Bodhi has no update for *in that release* — content
//! inherited at branching (the build's update belongs to an
//! older release), or answers missing while Bodhi is degraded.
//! The release's Koji stable tag chain is the authority on what
//! it actually carries, so builds Bodhi doesn't vouch for are
//! verified there (`koji list-tagged --latest --inherit
//! <stable_tag>`, so `f43-updates` also covers `f43`, and the
//! EPEL equivalents). Only a build actually tagged into the
//! release counts as shipped — a version merely committed to
//! dist-git and built into a side tag or `-candidate`/`-testing`
//! tag stays pending.

use std::collections::BTreeMap;

use sandogasa_bodhi::BodhiClient;
use sandogasa_bodhi::models::{BodhiRelease, Update};
use sandogasa_bugzilla::BzClient;
use sandogasa_bugzilla::models::Bug;
use sandogasa_distgit::DistGitClient;
use sandogasa_inventory::{Inventory, Priority};
use sandogasa_koji::parse_nvr;

use crate::semver_audit::version_at_least;
use sandogasa_bugclass::bugzilla::extract_new_version;

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

/// Build the single batch-mode Bugzilla query: every open
/// release-monitoring bug where `email` is the assignee or is
/// CC'd, across all components at once. With `any_reporter` the
/// reporter filter is dropped (triage-retired's
/// `--all-reporters`). The `email1`/`emailtype1` search-form
/// parameters are not part of the documented REST field list but
/// Red Hat Bugzilla passes them through (verified live against
/// bugzilla.redhat.com).
pub fn batch_bug_query(email: &str, any_reporter: bool) -> String {
    let mut parts: Vec<String> = vec![
        "bug_status=__open__".to_string(),
        format!("email1={}", urlencode(email)),
        "emailassigned_to1=1".to_string(),
        "emailcc1=1".to_string(),
        "emailtype1=equals".to_string(),
    ];
    if !any_reporter {
        parts.insert(
            0,
            format!("reporter={}", urlencode(RELEASE_MONITORING_REPORTER)),
        );
    }
    for product in PRODUCTS {
        parts.push(format!("product={}", urlencode(product)));
    }
    parts.join("&")
}

/// Group a batch query's results by component so the per-package
/// loop can look bugs up locally instead of querying Bugzilla per
/// package.
pub fn group_bugs_by_component(bugs: Vec<Bug>) -> BTreeMap<String, Vec<Bug>> {
    let mut map: BTreeMap<String, Vec<Bug>> = BTreeMap::new();
    for bug in bugs {
        let Some(component) = bug.component.first() else {
            continue;
        };
        map.entry(component.clone()).or_default().push(bug);
    }
    map
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

// ---- stale-bug handling (Bodhi-backed) ----

/// Signature of the Koji latest-tagged lookup injected by the
/// caller: `(tag, package)` to the latest NVR in the tag's
/// inheritance chain (`None` when the package has no build
/// there). Injectable so tests can stub Koji; callers pass
/// `None` for the whole lookup when the koji CLI is unavailable,
/// which fail-safes to "not shipped".
pub type TagLookup = dyn Fn(&str, &str) -> Result<Option<String>, String>;

/// Where an addressing build was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildSource {
    /// A Bodhi update (alias + whether it reached stable).
    Bodhi { alias: String, stable: bool },
    /// No Bodhi update for this release, but the build is tagged
    /// into the release's Koji stable tag chain — carried over
    /// from an older release (whose update predates this one),
    /// or Bodhi's answer was unavailable.
    Tagged,
}

/// The best build addressing a bug in one release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressingBuild {
    /// Build NVR (e.g. `rust-clircle-0.6.1-1.fc43`).
    pub nvr: String,
    pub source: BuildSource,
}

impl AddressingBuild {
    /// Whether the build is shipped (stable update, or tagged
    /// into the release with no in-flight Bodhi update).
    pub fn is_stable(&self) -> bool {
        match &self.source {
            BuildSource::Bodhi { stable, .. } => *stable,
            BuildSource::Tagged => true,
        }
    }
}

/// Cache of Koji tag lookups: `(stable_tag, package)` to the
/// latest NVR in the tag chain (`None` when there is no build,
/// or the lookup failed and was fail-safed to "not shipped").
type TaggedCache = BTreeMap<(String, String), Option<String>>;

/// One release's verdict for a bug: the addressing build, or
/// `None` when no update in that release carries the version.
#[derive(Debug, Clone)]
pub struct ReleaseFinding {
    /// Bodhi release name (e.g. `F43`).
    pub release: String,
    pub build: Option<AddressingBuild>,
}

/// What to do with a bug whose version is already built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleAction {
    /// Stable everywhere the package has a branch — close ERRATA.
    CloseErrata,
    /// Addressed, but at least one update is still in testing.
    Modified,
    /// Stable in some releases only (commonly just rawhide) —
    /// close only with confirmation or `--close-stale`.
    AskClose,
}

/// One planned stale-bug change.
#[derive(Debug, Clone)]
pub struct StaleBugPlan {
    pub bug_id: u64,
    pub component: String,
    pub summary: String,
    /// Version the bug advertises as available.
    pub version: String,
    pub action: StaleAction,
    /// Space-joined latest addressing NVR per release, for the
    /// bug's Fixed In Version field.
    pub fixed_in: String,
    pub findings: Vec<ReleaseFinding>,
}

/// Find the best build in `updates` addressing `target_version`
/// of `package`: the highest addressing version, preferring a
/// stable update on ties.
pub fn find_addressing(
    updates: &[Update],
    package: &str,
    target_version: &str,
) -> Option<AddressingBuild> {
    let mut best: Option<(AddressingBuild, String)> = None;
    for update in updates {
        let stable = update.status == "stable";
        for build in &update.builds {
            let Some((name, version, _)) = parse_nvr(&build.nvr) else {
                continue;
            };
            if name != package || !version_at_least(version, target_version) {
                continue;
            }
            let replace = match &best {
                None => true,
                Some((cur, cur_version)) => {
                    version_at_least(version, cur_version)
                        && (version != cur_version || (stable && !cur.is_stable()))
                }
            };
            if replace {
                best = Some((
                    AddressingBuild {
                        nvr: build.nvr.clone(),
                        source: BuildSource::Bodhi {
                            alias: update.alias.clone(),
                            stable,
                        },
                    },
                    version.to_string(),
                ));
            }
        }
    }
    best.map(|(b, _)| b)
}

/// Decide the action for one bug from its per-release findings.
/// Returns `None` when nothing addresses the bug yet (it's a
/// genuine pending update).
pub fn plan_stale_bug(
    bug: &Bug,
    component: &str,
    version: &str,
    findings: Vec<ReleaseFinding>,
) -> Option<StaleBugPlan> {
    let addressed: Vec<&AddressingBuild> =
        findings.iter().filter_map(|f| f.build.as_ref()).collect();
    if addressed.is_empty() {
        return None;
    }
    let action = if addressed.iter().any(|b| !b.is_stable()) {
        StaleAction::Modified
    } else if addressed.len() == findings.len() {
        StaleAction::CloseErrata
    } else {
        StaleAction::AskClose
    };
    // Dedupe NVRs: the same build can address several releases
    // (e.g. one el10 build mass-tagged into every epel10 minor).
    let mut nvrs: Vec<&str> = Vec::new();
    for b in &addressed {
        if !nvrs.contains(&b.nvr.as_str()) {
            nvrs.push(&b.nvr);
        }
    }
    let fixed_in = nvrs.join(" ");
    // Already recorded and still mid-flight: nothing new to write.
    if action == StaleAction::Modified && bug.status == "MODIFIED" && !bug.cf_fixed_in.is_empty() {
        return None;
    }
    Some(StaleBugPlan {
        bug_id: bug.id,
        component: component.to_string(),
        summary: bug.summary.clone(),
        version: version.to_string(),
        action,
        fixed_in,
        findings,
    })
}

/// Build the Bugzilla comment for a stale-bug change.
pub fn stale_comment(plan: &StaleBugPlan) -> String {
    let mut out = format!(
        "Bodhi has builds addressing this update (version {} or \
         newer):\n",
        plan.version
    );
    for f in &plan.findings {
        match &f.build {
            Some(b) => match &b.source {
                BuildSource::Bodhi { alias, stable } => out.push_str(&format!(
                    "  {}: {} — https://bodhi.fedoraproject.org/updates/{} ({})\n",
                    f.release,
                    b.nvr,
                    alias,
                    if *stable { "stable" } else { "testing" }
                )),
                BuildSource::Tagged => out.push_str(&format!(
                    "  {}: {} (tagged into the release; no Bodhi \
                     update in this release — carried over from \
                     an earlier one)\n",
                    f.release, b.nvr
                )),
            },
            None => out.push_str(&format!("  {}: no update found\n", f.release)),
        }
    }
    out.push('\n');
    out.push_str(match plan.action {
        StaleAction::CloseErrata => {
            "The new version is in stable updates for every active \
             release this package has a branch for; closing as ERRATA."
        }
        StaleAction::Modified => {
            "Some updates are still in testing; marking this bug \
             MODIFIED until they reach stable."
        }
        StaleAction::AskClose => {
            "The releases without an update are listed above — their \
             branches are not expected to rebase; closing as ERRATA."
        }
    });
    out
}

/// Sort key for Bodhi release names ("F45" > "F43", "EPEL-10" >
/// "EPEL-9") so findings render newest-first.
fn release_rank(name: &str) -> u64 {
    name.chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .unwrap_or(0)
}

/// Run the whole `triage-updates` flow.
///
/// Loads the inventories (already merged by the caller), iterates
/// every package, queries Bugzilla, plans priority and stale-bug
/// updates, prints them, optionally prompts, then applies.
/// `dry_run = true` short-circuits before any PUT.
/// `latest_tagged` is the Koji lookup backing the tagged-build
/// fallback; `None` disables it (koji CLI unavailable).
#[allow(clippy::too_many_arguments)]
pub async fn run(
    inventory: &Inventory,
    client: &BzClient,
    dg: &DistGitClient,
    bodhi: &BodhiClient,
    latest_tagged: Option<&TagLookup>,
    filter: &crate::WalkFilterArgs,
    batch_email: Option<&str>,
    skip_stale: bool,
    close_stale: bool,
    claim: bool,
    claim_email: Option<&str>,
    dry_run: bool,
    yes: bool,
    verbose: bool,
) -> Result<RunReport, String> {
    let mut all_updates: Vec<PriorityUpdate> = Vec::new();
    let mut stale_plans: Vec<StaleBugPlan> = Vec::new();
    let mut packages_with_priority = 0usize;
    // Active Bodhi releases, fetched once on first need.
    let mut releases: Option<Vec<BodhiRelease>> = None;
    // (package, release-name) -> updates, shared across a
    // package's bugs.
    let mut updates_cache: BTreeMap<(String, String), Vec<Update>> = BTreeMap::new();
    // (stable_tag, package) -> latest tagged NVR, for the
    // tagged-build fallback.
    let mut tagged_cache = TaggedCache::new();

    // Batch mode: one Bugzilla query up front for every open
    // release-monitoring bug assigned to or CC'ing the email,
    // matched against inventory packages locally — instead of one
    // query per package.
    let batch_bugs: Option<BTreeMap<String, Vec<Bug>>> = match batch_email {
        Some(email) => {
            if verbose {
                eprintln!("[poi-tracker] batch: querying bugs for {email}");
            }
            let bugs = client
                .search(&batch_bug_query(email, false), 0)
                .await
                .map_err(|e| format!("Bugzilla batch search: {e}"))?;
            if verbose {
                eprintln!("[poi-tracker] batch: {} open bug(s) found", bugs.len());
            }
            Some(group_bugs_by_component(bugs))
        }
        None => None,
    };

    let mut marked_retired = 0usize;
    for pkg in &inventory.package {
        if !filter.matches(&pkg.name) {
            continue;
        }
        // No longer shipped anywhere (recorded by
        // `prune-retired`): nothing to triage. Its remaining bugs
        // belong to triage-retired, which still processes it.
        if pkg.is_unshipped() {
            marked_retired += 1;
            if verbose {
                eprintln!(
                    "[poi-tracker] {}: marked unshipped in the \
                     inventory; skipping (run triage-retired)",
                    pkg.name
                );
            }
            continue;
        }
        // Inventory says it's retired on rawhide (recorded by
        // `triage-retired --mark`): its release-monitoring bugs
        // belong to triage-retired, not here — skip without any
        // network traffic.
        if pkg.is_retired_on("rawhide") {
            marked_retired += 1;
            if verbose {
                eprintln!(
                    "[poi-tracker] {}: marked retired on rawhide in the \
                     inventory; skipping (run triage-retired)",
                    pkg.name
                );
            }
            continue;
        }
        let resolved = inventory.priority_for(&pkg.name);
        let target = match resolved {
            None => {
                if verbose {
                    eprintln!("[poi-tracker] {}: no priority configured", pkg.name);
                }
                None
            }
            Some(Priority::Unspecified) => {
                if verbose {
                    eprintln!("[poi-tracker] {}: priority=unspecified (opt-out)", pkg.name);
                }
                None
            }
            Some(p) => {
                packages_with_priority += 1;
                Some(p)
            }
        };
        // Without a priority to set, the search only feeds the
        // stale check — skip it entirely under --skip-stale.
        if target.is_none() && skip_stale {
            continue;
        }

        let per_pkg;
        let bugs: &[Bug] = match &batch_bugs {
            Some(map) => map.get(&pkg.name).map(Vec::as_slice).unwrap_or(&[]),
            None => {
                if verbose {
                    eprintln!(
                        "[poi-tracker] {}: searching release-monitoring bugs",
                        pkg.name
                    );
                }
                let query = bug_search_query(&pkg.name);
                per_pkg = client
                    .search(&query, 0)
                    .await
                    .map_err(|e| format!("Bugzilla search for {}: {e}", pkg.name))?;
                &per_pkg
            }
        };

        if target.is_some() {
            match plan_package(&pkg.name, resolved, bugs) {
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

        if skip_stale || bugs.is_empty() {
            continue;
        }
        plan_stale_for_package(
            &pkg.name,
            bugs,
            dg,
            bodhi,
            latest_tagged,
            &mut releases,
            &mut updates_cache,
            &mut tagged_cache,
            &mut stale_plans,
            verbose,
        )
        .await;
    }

    if marked_retired > 0 {
        eprintln!(
            "({marked_retired} package(s) skipped: marked retired on \
             rawhide in the inventory)"
        );
    }
    print_plan(&all_updates);
    print_stale_plan(&stale_plans);

    let mut report = RunReport {
        packages_with_priority,
        updates_planned: all_updates.len(),
        updates_applied: 0,
        stale_planned: stale_plans.len(),
        stale_applied: 0,
        failures: 0,
    };

    if all_updates.is_empty() && stale_plans.is_empty() {
        return Ok(report);
    }
    if dry_run {
        eprintln!("\n(dry-run: not applying)");
        return Ok(report);
    }

    // Resolve the AskClose set: --close-stale promotes them all,
    // -y without it drops them, otherwise prompt once for the lot.
    let ask_count = stale_plans
        .iter()
        .filter(|p| p.action == StaleAction::AskClose)
        .count();
    let close_partial = if ask_count == 0 || close_stale {
        close_stale
    } else if yes {
        eprintln!(
            "(skipping {ask_count} partially-addressed bug(s); pass \
             --close-stale to close them under -y)"
        );
        false
    } else {
        confirm(&format!(
            "\nClose {ask_count} bug(s) addressed only in some \
             releases as ERRATA?"
        ))?
    };
    if !close_partial {
        stale_plans.retain(|p| p.action != StaleAction::AskClose);
    }
    report.stale_planned = stale_plans.len();

    // A bug about to be closed doesn't need a priority bump.
    let closing: Vec<u64> = stale_plans
        .iter()
        .filter(|p| p.action != StaleAction::Modified)
        .map(|p| p.bug_id)
        .collect();
    all_updates.retain(|u| !closing.contains(&u.bug_id));
    report.updates_planned = all_updates.len();

    let total = all_updates.len() + stale_plans.len();
    if total == 0 {
        return Ok(report);
    }

    // Offer to claim ownership of the bugs about to be closed
    // (triaging is a benefit in itself — the person cleaning up
    // stale bugs may want the credit). MODIFIED transitions keep
    // their assignee: those bugs stay open and belong to whoever
    // owns the in-flight update.
    let active_claim_email = if closing.is_empty() {
        None
    } else {
        sandogasa_bugzilla::claim::resolve_claim(
            claim,
            yes,
            claim_email,
            &sandogasa_bugzilla::claim::close_claim_prompt(
                closing.len(),
                claim_email.unwrap_or(""),
            ),
            confirm,
        )?
    };
    if let Some(ref e) = active_claim_email {
        eprintln!("claiming ownership as {e}");
    }

    if !yes && !confirm(&format!("\nApply {total} update(s)?"))? {
        eprintln!("aborted.");
        return Ok(report);
    }

    for u in &all_updates {
        let body = serde_json::json!({"priority": u.target_priority.as_bugzilla_str()});
        match client.update(u.bug_id, &body).await {
            Ok(()) => {
                report.updates_applied += 1;
                eprintln!(
                    "updated bug {} ({}): {} -> {}",
                    u.bug_id,
                    u.component,
                    u.current_priority,
                    u.target_priority.as_bugzilla_str()
                );
            }
            Err(e) => {
                report.failures += 1;
                eprintln!("error: bug {} ({}): {e}", u.bug_id, u.component);
            }
        }
    }

    for plan in &stale_plans {
        let mut body = serde_json::json!({
            "cf_fixed_in": plan.fixed_in,
            "comment": { "body": stale_comment(plan) },
        });
        let outcome = match plan.action {
            StaleAction::Modified => {
                body["status"] = serde_json::json!("MODIFIED");
                "-> MODIFIED"
            }
            StaleAction::CloseErrata | StaleAction::AskClose => {
                body["status"] = serde_json::json!("CLOSED");
                body["resolution"] = serde_json::json!("ERRATA");
                sandogasa_bugzilla::claim::apply_claim(&mut body, active_claim_email.as_deref());
                "-> CLOSED/ERRATA"
            }
        };
        match client.update(plan.bug_id, &body).await {
            Ok(()) => {
                report.stale_applied += 1;
                eprintln!(
                    "updated bug {} ({}): {outcome} (fixed in: {})",
                    plan.bug_id, plan.component, plan.fixed_in
                );
            }
            Err(e) => {
                report.failures += 1;
                eprintln!("error: bug {} ({}): {e}", plan.bug_id, plan.component);
            }
        }
    }
    Ok(report)
}

/// Plan stale-bug actions for one package's open bugs. Network
/// failures (Bodhi, dist-git) skip the package with a warning
/// rather than failing the whole run — a missing answer must not
/// be mistaken for "no update exists".
#[allow(clippy::too_many_arguments)]
async fn plan_stale_for_package(
    package: &str,
    bugs: &[Bug],
    dg: &DistGitClient,
    bodhi: &BodhiClient,
    latest_tagged: Option<&TagLookup>,
    releases: &mut Option<Vec<BodhiRelease>>,
    updates_cache: &mut BTreeMap<(String, String), Vec<Update>>,
    tagged_cache: &mut TaggedCache,
    out: &mut Vec<StaleBugPlan>,
    verbose: bool,
) {
    let with_version: Vec<(&Bug, String)> = bugs
        .iter()
        .filter_map(|b| extract_new_version(&b.summary, package).map(|v| (b, v)))
        .collect();
    if with_version.is_empty() {
        return;
    }

    if releases.is_none() {
        match bodhi.active_releases().await {
            Ok(r) => *releases = Some(r),
            Err(e) => {
                eprintln!("warning: cannot fetch Bodhi releases: {e}");
                return;
            }
        }
    }
    let releases = releases.as_ref().unwrap();

    let branches = match dg.list_branches(package).await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("warning: {package}: cannot list dist-git branches: {e}");
            return;
        }
    };

    for (bug, version) in with_version {
        // Match the bug's product family: a Fedora bug is only
        // addressed by Fedora releases, an EPEL bug by EPEL ones.
        let prefix = match bug.product.as_str() {
            "Fedora" => "FEDORA",
            "Fedora EPEL" => "FEDORA-EPEL",
            _ => continue,
        };
        let mut relevant: Vec<&BodhiRelease> = releases
            .iter()
            .filter(|r| r.id_prefix == prefix && branches.iter().any(|b| b == &r.branch))
            .collect();
        if relevant.is_empty() {
            continue;
        }
        relevant.sort_by_key(|r| std::cmp::Reverse(release_rank(&r.name)));

        // Each release is resolved fully before moving to the
        // next: Bodhi first (it has the update alias and status),
        // then the release's Koji stable tag chain — the
        // authority on what the release carries — for builds
        // Bodhi has no update for in this release (inherited at
        // branching, or Bodhi degraded). Releases are visited
        // newest-first, so rawhide is resolved first.
        let mut findings = Vec::with_capacity(relevant.len());
        let mut failed = false;
        let mut rawhide_pending = false;
        for rel in &relevant {
            let key = (package.to_string(), rel.name.clone());
            if !updates_cache.contains_key(&key) {
                if verbose {
                    eprintln!("[poi-tracker] {package}: querying Bodhi for {}", rel.name);
                }
                match bodhi
                    .updates_for_package(package, &rel.name, &["stable", "testing"])
                    .await
                {
                    Ok(u) => {
                        updates_cache.insert(key.clone(), u);
                    }
                    Err(e) => {
                        eprintln!(
                            "warning: {package}: Bodhi query for {} failed: {e}",
                            rel.name
                        );
                        failed = true;
                        break;
                    }
                }
            }
            let mut build = find_addressing(&updates_cache[&key], package, &version);
            if build.is_none()
                && let Some(lookup) = latest_tagged
                && !rel.stable_tag.is_empty()
            {
                let tag_key = (rel.stable_tag.clone(), package.to_string());
                if !tagged_cache.contains_key(&tag_key) {
                    if verbose {
                        eprintln!(
                            "[poi-tracker] {package}: checking Koji tag {}",
                            rel.stable_tag
                        );
                    }
                    let nvr = match lookup(&rel.stable_tag, package) {
                        Ok(n) => n,
                        Err(e) => {
                            eprintln!(
                                "warning: {package}: Koji query for {} failed: {e} \
                                 (treating as not shipped)",
                                rel.stable_tag
                            );
                            None
                        }
                    };
                    tagged_cache.insert(tag_key.clone(), nvr);
                }
                if let Some(nvr) = &tagged_cache[&tag_key]
                    && let Some((name, tagged_version, _)) = parse_nvr(nvr)
                    && name == package
                    && version_at_least(tagged_version, &version)
                {
                    build = Some(AddressingBuild {
                        nvr: nvr.clone(),
                        source: BuildSource::Tagged,
                    });
                }
            }
            // Short-circuit: Fedora updates land in rawhide first
            // (a stable release may never carry a newer version
            // than rawhide), so a version absent from rawhide —
            // neither in Bodhi nor tagged into its Koji tag —
            // can't be in the stable releases either; skip
            // querying them. EPEL branches update independently
            // of each other, so no equivalent shortcut applies.
            if build.is_none() && rel.branch == "rawhide" {
                rawhide_pending = true;
                break;
            }
            findings.push(ReleaseFinding {
                release: rel.name.clone(),
                build,
            });
        }
        if failed {
            continue;
        }
        if rawhide_pending {
            if verbose {
                eprintln!(
                    "[poi-tracker] {package}: bug {} ({version}) not yet in \
                     rawhide; skipping stable-release checks",
                    bug.id
                );
            }
            continue;
        }
        if let Some(plan) = plan_stale_bug(bug, package, &version, findings) {
            out.push(plan);
        } else if verbose {
            eprintln!(
                "[poi-tracker] {package}: bug {} ({version}) still pending",
                bug.id
            );
        }
    }
}

/// Summary returned from `run` so the caller can pick an exit
/// code without re-counting.
#[derive(Debug, Default)]
pub struct RunReport {
    pub packages_with_priority: usize,
    pub updates_planned: usize,
    pub updates_applied: usize,
    pub stale_planned: usize,
    pub stale_applied: usize,
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

/// Print planned stale-bug actions, grouped by action.
fn print_stale_plan(plans: &[StaleBugPlan]) {
    if plans.is_empty() {
        return;
    }
    println!("\nBugs already addressed in Bodhi:");
    for (action, heading) in [
        (
            StaleAction::CloseErrata,
            "Close as ERRATA (stable everywhere)",
        ),
        (StaleAction::Modified, "Mark MODIFIED (still in testing)"),
        (
            StaleAction::AskClose,
            "Addressed only in some releases (close on confirm / --close-stale)",
        ),
    ] {
        let group: Vec<&StaleBugPlan> = plans.iter().filter(|p| p.action == action).collect();
        if group.is_empty() {
            continue;
        }
        println!("  {heading}:");
        for p in &group {
            println!(
                "    bug {} ({}): {} — fixed in: {}",
                p.bug_id, p.component, p.summary, p.fixed_in
            );
        }
    }
}

pub(crate) fn confirm(prompt: &str) -> Result<bool, String> {
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

    // ---- stale-bug handling ----

    fn make_update(alias: &str, status: &str, nvrs: &[&str]) -> Update {
        serde_json::from_value(serde_json::json!({
            "alias": alias,
            "status": status,
            "builds": nvrs.iter().map(|n| serde_json::json!({"nvr": n})).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    fn finding(release: &str, build: Option<(&str, &str, bool)>) -> ReleaseFinding {
        ReleaseFinding {
            release: release.to_string(),
            build: build.map(|(nvr, alias, stable)| AddressingBuild {
                nvr: nvr.to_string(),
                source: BuildSource::Bodhi {
                    alias: alias.to_string(),
                    stable,
                },
            }),
        }
    }

    #[test]
    fn find_addressing_picks_highest_matching_build() {
        let updates = vec![
            make_update("FEDORA-1", "stable", &["foo-1.2.0-1.fc43", "bar-9-1.fc43"]),
            make_update("FEDORA-2", "testing", &["foo-1.3.0-1.fc43"]),
        ];
        let best = find_addressing(&updates, "foo", "1.2.0").unwrap();
        assert_eq!(best.nvr, "foo-1.3.0-1.fc43");
        assert_eq!(
            best.source,
            BuildSource::Bodhi {
                alias: "FEDORA-2".to_string(),
                stable: false
            }
        );
        assert!(!best.is_stable());
    }

    #[test]
    fn find_addressing_prefers_stable_on_version_tie() {
        let updates = vec![
            make_update("FEDORA-T", "testing", &["foo-1.2.0-1.fc43"]),
            make_update("FEDORA-S", "stable", &["foo-1.2.0-2.fc43"]),
        ];
        let best = find_addressing(&updates, "foo", "1.2.0").unwrap();
        assert!(matches!(
            best.source,
            BuildSource::Bodhi { ref alias, stable: true } if alias == "FEDORA-S"
        ));
    }

    #[test]
    fn plan_stale_dedupes_identical_nvrs() {
        // The same build can address several releases (e.g. one
        // el10 build mass-tagged into every epel10 minor); it
        // must not repeat in Fixed In Version.
        let bug = make_bug(1, "unspecified", "foo-1.2.0 is available");
        let tagged = |rel: &str| ReleaseFinding {
            release: rel.to_string(),
            build: Some(AddressingBuild {
                nvr: "foo-1.2.0-1.el10_2".to_string(),
                source: BuildSource::Tagged,
            }),
        };
        let plan = plan_stale_bug(
            &bug,
            "foo",
            "1.2.0",
            vec![tagged("EPEL-10.3"), tagged("EPEL-10.2")],
        )
        .unwrap();
        assert_eq!(plan.action, StaleAction::CloseErrata);
        assert_eq!(plan.fixed_in, "foo-1.2.0-1.el10_2");
    }

    #[test]
    fn find_addressing_ignores_older_versions_and_other_packages() {
        let updates = vec![make_update(
            "FEDORA-1",
            "stable",
            &["foo-1.1.0-1.fc43", "foolish-2.0-1.fc43"],
        )];
        assert!(find_addressing(&updates, "foo", "1.2.0").is_none());
    }

    #[test]
    fn plan_stale_pending_when_nothing_addresses() {
        let bug = make_bug(1, "unspecified", "foo-1.2.0 is available");
        let findings = vec![finding("F45", None), finding("F43", None)];
        assert!(plan_stale_bug(&bug, "foo", "1.2.0", findings).is_none());
    }

    #[test]
    fn plan_stale_close_when_stable_everywhere() {
        let bug = make_bug(1, "unspecified", "foo-1.2.0 is available");
        let findings = vec![
            finding("F45", Some(("foo-1.2.0-1.fc45", "FEDORA-A", true))),
            finding("F43", Some(("foo-1.2.0-1.fc43", "FEDORA-B", true))),
        ];
        let plan = plan_stale_bug(&bug, "foo", "1.2.0", findings).unwrap();
        assert_eq!(plan.action, StaleAction::CloseErrata);
        assert_eq!(plan.fixed_in, "foo-1.2.0-1.fc45 foo-1.2.0-1.fc43");
    }

    #[test]
    fn plan_stale_modified_when_any_testing() {
        let bug = make_bug(1, "unspecified", "foo-1.2.0 is available");
        let findings = vec![
            finding("F45", Some(("foo-1.2.0-1.fc45", "FEDORA-A", true))),
            finding("F43", Some(("foo-1.2.0-1.fc43", "FEDORA-B", false))),
        ];
        let plan = plan_stale_bug(&bug, "foo", "1.2.0", findings).unwrap();
        assert_eq!(plan.action, StaleAction::Modified);
    }

    #[test]
    fn plan_stale_ask_when_partially_addressed_stable() {
        // Stable in rawhide only — the "ask before closing" case.
        let bug = make_bug(1, "unspecified", "foo-1.2.0 is available");
        let findings = vec![
            finding("F45", Some(("foo-1.2.0-1.fc45", "FEDORA-A", true))),
            finding("F43", None),
        ];
        let plan = plan_stale_bug(&bug, "foo", "1.2.0", findings).unwrap();
        assert_eq!(plan.action, StaleAction::AskClose);
        assert_eq!(plan.fixed_in, "foo-1.2.0-1.fc45");
    }

    #[test]
    fn plan_stale_skips_already_modified_with_fixed_in() {
        let mut bug = make_bug(1, "unspecified", "foo-1.2.0 is available");
        bug.status = "MODIFIED".to_string();
        bug.cf_fixed_in = "foo-1.2.0-1.fc43".to_string();
        let findings = vec![finding(
            "F43",
            Some(("foo-1.2.0-1.fc43", "FEDORA-B", false)),
        )];
        assert!(plan_stale_bug(&bug, "foo", "1.2.0", findings).is_none());
    }

    #[test]
    fn stale_comment_lists_releases_and_action() {
        let bug = make_bug(1, "unspecified", "foo-1.2.0 is available");
        let findings = vec![
            finding("F45", Some(("foo-1.2.0-1.fc45", "FEDORA-A", true))),
            finding("F43", None),
        ];
        let plan = plan_stale_bug(&bug, "foo", "1.2.0", findings).unwrap();
        let comment = stale_comment(&plan);
        assert!(comment.contains("F45: foo-1.2.0-1.fc45"));
        assert!(comment.contains("https://bodhi.fedoraproject.org/updates/FEDORA-A"));
        assert!(comment.contains("(stable)"));
        assert!(comment.contains("F43: no update found"));
        assert!(comment.contains("closing as ERRATA"));
    }

    #[test]
    fn release_rank_orders_names() {
        assert!(release_rank("F45") > release_rank("F43"));
        assert!(release_rank("EPEL-10") > release_rank("EPEL-9"));
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

    // ---- wiremock end-to-end (run) ----

    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_inventory(priority: Option<&str>) -> Inventory {
        let prio = priority
            .map(|p| format!("priority = \"{p}\"\n"))
            .unwrap_or_default();
        toml::from_str(&format!(
            "[inventory]\n\
             name = \"test\"\n\
             description = \"test\"\n\
             maintainer = \"tester\"\n\
             \n\
             [[package]]\n\
             name = \"foo\"\n\
             {prio}"
        ))
        .unwrap()
    }

    fn bug_json(id: u64, summary: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "summary": summary,
            "status": "NEW",
            "resolution": "",
            "product": "Fedora",
            "component": ["foo"],
            "severity": "unspecified",
            "priority": "unspecified",
            "assigned_to": "nobody@fedoraproject.org",
            "creator": RELEASE_MONITORING_REPORTER,
            "creation_time": "2026-05-01T00:00:00Z",
            "last_change_time": "2026-05-01T00:00:00Z",
        })
    }

    /// Mount the shared scaffolding: one open bug for foo-1.2.0,
    /// Bodhi releases F45 (rawhide) + F43, dist-git branches.
    async fn mount_common(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .and(query_param("component", "foo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(1, "foo-1.2.0 is available")],
                "total_matches": 1
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/releases/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "releases": [
                    {"name": "F45", "branch": "rawhide", "id_prefix": "FEDORA",
                     "state": "pending", "stable_tag": "f45"},
                    {"name": "F43", "branch": "f43", "id_prefix": "FEDORA",
                     "state": "current", "stable_tag": "f43-updates"}
                ],
                "total": 2, "page": 1, "pages": 1
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/0/rpms/foo/git/branches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "branches": ["rawhide", "f43"]
            })))
            .mount(server)
            .await;
    }

    fn updates_response(updates: serde_json::Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "updates": updates, "total": 1, "page": 1, "pages": 1
        }))
    }

    #[tokio::test]
    async fn run_closes_bug_stable_everywhere_with_tagged_fallback() {
        let server = MockServer::start().await;
        mount_common(&server).await;
        // F45: stable Bodhi update. F43: no Bodhi update in this
        // release, but the build is in the release's Koji tag
        // chain (carried over from an earlier release).
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F45"))
            .respond_with(updates_response(serde_json::json!([{
                "alias": "FEDORA-2026-aaa",
                "status": "stable",
                "builds": [{"nvr": "foo-1.2.0-1.fc45"}]
            }])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F43"))
            .respond_with(updates_response(serde_json::json!([])))
            .mount(&server)
            .await;
        let lookup = |tag: &str, pkg: &str| -> Result<Option<String>, String> {
            assert_eq!((tag, pkg), ("f43-updates", "foo"));
            Ok(Some("foo-1.2.0-1.fc43".to_string()))
        };
        // The close PUT: ERRATA + Fixed In Version from both
        // releases. The priority bump for the same bug must be
        // dropped (the bug is closing), so this is the only PUT.
        Mock::given(method("PUT"))
            .and(path("/rest/bug/1"))
            .and(body_partial_json(serde_json::json!({
                "status": "CLOSED",
                "resolution": "ERRATA",
                "cf_fixed_in": "foo-1.2.0-1.fc45 foo-1.2.0-1.fc43"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let inventory = test_inventory(Some("high"));
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        let bodhi = BodhiClient::with_base_url(&server.uri());
        let report = run(
            &inventory,
            &bz,
            &dg,
            &bodhi,
            Some(&lookup),
            &crate::WalkFilterArgs::default(),
            None,
            false,
            false,
            false,
            None,
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.stale_applied, 1);
        assert_eq!(
            report.updates_planned, 0,
            "priority bump dropped for closing bug"
        );
        assert_eq!(report.failures, 0);
    }

    #[tokio::test]
    async fn run_side_tag_only_build_stays_pending() {
        // The reported regression scenario: the new version is
        // committed to rawhide dist-git and built, but the build
        // is only in a side tag — Bodhi has no update and the
        // release's tag chain still carries the old version. The
        // bug must stay open (no PUT at all).
        let server = MockServer::start().await;
        mount_common(&server).await;
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F45"))
            .respond_with(updates_response(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/rest/bug/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(0)
            .mount(&server)
            .await;
        let lookup = |tag: &str, _pkg: &str| -> Result<Option<String>, String> {
            assert_eq!(tag, "f45", "stable releases must not be queried");
            Ok(Some("foo-1.1.0-1.fc45".to_string()))
        };

        let inventory = test_inventory(None);
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        let bodhi = BodhiClient::with_base_url(&server.uri());
        let report = run(
            &inventory,
            &bz,
            &dg,
            &bodhi,
            Some(&lookup),
            &crate::WalkFilterArgs::default(),
            None,
            false,
            false,
            false,
            None,
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.stale_planned, 0);
        assert_eq!(report.stale_applied, 0);
    }

    #[tokio::test]
    async fn run_without_koji_lookup_treats_unverified_as_pending() {
        // With no Koji lookup (koji CLI unavailable), a bug Bodhi
        // can't vouch for is left open — fail-safe, never closed
        // on unverified evidence.
        let server = MockServer::start().await;
        mount_common(&server).await;
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F45"))
            .respond_with(updates_response(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/rest/bug/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(0)
            .mount(&server)
            .await;

        let inventory = test_inventory(None);
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        let bodhi = BodhiClient::with_base_url(&server.uri());
        let report = run(
            &inventory,
            &bz,
            &dg,
            &bodhi,
            None,
            &crate::WalkFilterArgs::default(),
            None,
            false,
            false,
            false,
            None,
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.stale_planned, 0);
    }

    #[tokio::test]
    async fn run_marks_modified_when_update_in_testing() {
        let server = MockServer::start().await;
        mount_common(&server).await;
        // Both releases addressed, F43 only in testing.
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F45"))
            .respond_with(updates_response(serde_json::json!([{
                "alias": "FEDORA-2026-aaa",
                "status": "stable",
                "builds": [{"nvr": "foo-1.2.0-1.fc45"}]
            }])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F43"))
            .respond_with(updates_response(serde_json::json!([{
                "alias": "FEDORA-2026-bbb",
                "status": "testing",
                "builds": [{"nvr": "foo-1.2.0-1.fc43"}]
            }])))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/rest/bug/1"))
            .and(body_partial_json(serde_json::json!({"status": "MODIFIED"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let inventory = test_inventory(None);
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        let bodhi = BodhiClient::with_base_url(&server.uri());
        let report = run(
            &inventory,
            &bz,
            &dg,
            &bodhi,
            None,
            &crate::WalkFilterArgs::default(),
            None,
            false,
            false,
            false,
            None,
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.stale_applied, 1);
    }

    #[tokio::test]
    async fn run_claim_assigns_closed_bugs() {
        let server = MockServer::start().await;
        mount_common(&server).await;
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F45"))
            .respond_with(updates_response(serde_json::json!([{
                "alias": "FEDORA-2026-aaa",
                "status": "stable",
                "builds": [{"nvr": "foo-1.2.0-1.fc45"}]
            }])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F43"))
            .respond_with(updates_response(serde_json::json!([{
                "alias": "FEDORA-2026-bbb",
                "status": "stable",
                "builds": [{"nvr": "foo-1.2.0-1.fc43"}]
            }])))
            .mount(&server)
            .await;
        // With --claim (and a configured email), the close body
        // must also reassign the bug — even under -y.
        Mock::given(method("PUT"))
            .and(path("/rest/bug/1"))
            .and(body_partial_json(serde_json::json!({
                "status": "CLOSED",
                "resolution": "ERRATA",
                "assigned_to": "me@example.com"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let inventory = test_inventory(None);
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        let bodhi = BodhiClient::with_base_url(&server.uri());
        let report = run(
            &inventory,
            &bz,
            &dg,
            &bodhi,
            None,
            &crate::WalkFilterArgs::default(),
            None,
            false,
            false,
            true,
            Some("me@example.com"),
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.stale_applied, 1);
        assert_eq!(report.failures, 0);
    }

    #[tokio::test]
    async fn run_claim_leaves_modified_bugs_unassigned() {
        let server = MockServer::start().await;
        mount_common(&server).await;
        // Addressed but still in testing -> MODIFIED, which keeps
        // its assignee even under --claim (the bug stays open and
        // belongs to whoever owns the in-flight update).
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F45"))
            .respond_with(updates_response(serde_json::json!([{
                "alias": "FEDORA-2026-aaa",
                "status": "testing",
                "builds": [{"nvr": "foo-1.2.0-1.fc45"}]
            }])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F43"))
            .respond_with(updates_response(serde_json::json!([{
                "alias": "FEDORA-2026-bbb",
                "status": "testing",
                "builds": [{"nvr": "foo-1.2.0-1.fc43"}]
            }])))
            .mount(&server)
            .await;
        // Mounted first, so a PUT carrying assigned_to would match
        // here and fail the expect(0) on drop.
        Mock::given(method("PUT"))
            .and(path("/rest/bug/1"))
            .and(body_partial_json(
                serde_json::json!({"assigned_to": "me@example.com"}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/rest/bug/1"))
            .and(body_partial_json(serde_json::json!({"status": "MODIFIED"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let inventory = test_inventory(None);
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        let bodhi = BodhiClient::with_base_url(&server.uri());
        let report = run(
            &inventory,
            &bz,
            &dg,
            &bodhi,
            None,
            &crate::WalkFilterArgs::default(),
            None,
            false,
            false,
            true,
            Some("me@example.com"),
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.stale_applied, 1);
        assert_eq!(report.failures, 0);
    }

    #[tokio::test]
    async fn run_skips_partial_close_under_yes_without_close_stale() {
        let server = MockServer::start().await;
        mount_common(&server).await;
        // Stable in rawhide only; F43 has nothing anywhere.
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F45"))
            .respond_with(updates_response(serde_json::json!([{
                "alias": "FEDORA-2026-aaa",
                "status": "stable",
                "builds": [{"nvr": "foo-1.2.0-1.fc45"}]
            }])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F43"))
            .respond_with(updates_response(serde_json::json!([])))
            .mount(&server)
            .await;
        // F43's tag chain still carries the old build -> AskClose;
        // under -y without --close-stale nothing is written.
        let lookup = |tag: &str, _pkg: &str| -> Result<Option<String>, String> {
            assert_eq!(tag, "f43-updates");
            Ok(Some("foo-1.1.0-1.fc43".to_string()))
        };

        let inventory = test_inventory(None);
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        let bodhi = BodhiClient::with_base_url(&server.uri());
        let report = run(
            &inventory,
            &bz,
            &dg,
            &bodhi,
            Some(&lookup),
            &crate::WalkFilterArgs::default(),
            None,
            false,
            false,
            false,
            None,
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.stale_planned, 0, "AskClose dropped under -y");
        assert_eq!(report.stale_applied, 0);
    }

    #[tokio::test]
    async fn run_short_circuits_stable_checks_when_rawhide_pending() {
        let server = MockServer::start().await;
        mount_common(&server).await;
        // Rawhide (F45) has no Bodhi update and its Koji tag
        // still carries the old version -> the bug is genuinely
        // pending, and the stable release (F43) must never be
        // queried at all (neither Bodhi nor Koji).
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F45"))
            .respond_with(updates_response(serde_json::json!([])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F43"))
            .respond_with(updates_response(serde_json::json!([])))
            .expect(0)
            .mount(&server)
            .await;
        let lookup = |tag: &str, _pkg: &str| -> Result<Option<String>, String> {
            assert_eq!(tag, "f45", "F43's Koji tag must not be queried");
            Ok(Some("foo-1.1.0-1.fc45".to_string()))
        };

        let inventory = test_inventory(None);
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        let bodhi = BodhiClient::with_base_url(&server.uri());
        let report = run(
            &inventory,
            &bz,
            &dg,
            &bodhi,
            Some(&lookup),
            &crate::WalkFilterArgs::default(),
            None,
            false,
            false,
            false,
            None,
            false,
            true,
            true,
        )
        .await
        .unwrap();
        assert_eq!(report.stale_planned, 0);
        // MockServer verifies the expect(0) mocks on drop.
    }

    #[test]
    fn batch_bug_query_filters_by_email_assignee_or_cc() {
        let q = batch_bug_query("user@example.com", false);
        assert!(q.contains("reporter=upstream-release-monitoring%40fedoraproject.org"));
        assert!(q.contains("bug_status=__open__"));
        assert!(q.contains("email1=user%40example.com"));
        assert!(q.contains("emailassigned_to1=1"));
        assert!(q.contains("emailcc1=1"));
        assert!(q.contains("emailtype1=equals"));
        assert!(q.contains("product=Fedora"));
        // No per-component filter: one query covers everything.
        assert!(!q.contains("component="));
    }

    #[test]
    fn group_bugs_by_component_groups() {
        let mut a = make_bug(1, "unspecified", "foo-1.0 is available");
        a.component = vec!["foo".to_string()];
        let mut b = make_bug(2, "unspecified", "bar-2.0 is available");
        b.component = vec!["bar".to_string()];
        let mut c = make_bug(3, "unspecified", "foo-1.1 is available");
        c.component = vec!["foo".to_string()];
        let map = group_bugs_by_component(vec![a, b, c]);
        assert_eq!(map.len(), 2);
        assert_eq!(map["foo"].len(), 2);
        assert_eq!(map["bar"].len(), 1);
    }

    #[tokio::test]
    async fn run_skips_packages_marked_retired() {
        // No servers are running: if the marked package weren't
        // skipped, the Bugzilla search would error the run.
        let inventory: Inventory = toml::from_str(
            "[inventory]\n\
             name = \"test\"\n\
             description = \"test\"\n\
             maintainer = \"tester\"\n\
             \n\
             [[package]]\n\
             name = \"foo\"\n\
             priority = \"high\"\n\
             retired_on = [\"rawhide\"]\n",
        )
        .unwrap();
        let bz = BzClient::new("http://127.0.0.1:1");
        let dg = DistGitClient::with_base_url("http://127.0.0.1:1");
        let bodhi = BodhiClient::with_base_url("http://127.0.0.1:1");
        let report = run(
            &inventory,
            &bz,
            &dg,
            &bodhi,
            None,
            &crate::WalkFilterArgs::default(),
            None,
            false,
            false,
            false,
            None,
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.updates_planned, 0);
        assert_eq!(report.stale_planned, 0);
    }

    #[tokio::test]
    async fn run_batch_mode_makes_one_bugzilla_query() {
        let server = MockServer::start().await;
        // Bugs come from a single email-scoped query (expect(1));
        // the result includes a bug for a package NOT in the
        // inventory, which must be ignored by local matching.
        let mut other = bug_json(99, "other-pkg-3.0 is available");
        other["component"] = serde_json::json!(["other-pkg"]);
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .and(query_param("email1", "me@example.com"))
            .and(query_param("emailassigned_to1", "1"))
            .and(query_param("emailcc1", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [bug_json(1, "foo-1.2.0 is available"), other],
                "total_matches": 2
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/releases/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "releases": [
                    {"name": "F45", "branch": "rawhide", "id_prefix": "FEDORA",
                     "state": "pending", "stable_tag": "f45"}
                ],
                "total": 1, "page": 1, "pages": 1
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/0/rpms/foo/git/branches"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "branches": ["rawhide"]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/updates/"))
            .and(query_param("releases", "F45"))
            .respond_with(updates_response(serde_json::json!([{
                "alias": "FEDORA-2026-aaa",
                "status": "stable",
                "builds": [{"nvr": "foo-1.2.0-1.fc45"}]
            }])))
            .mount(&server)
            .await;
        // Only foo's bug is closed; a PUT for bug 99 would fail
        // the expect(1) below and bump report.failures.
        Mock::given(method("PUT"))
            .and(path("/rest/bug/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let inventory = test_inventory(None);
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        let bodhi = BodhiClient::with_base_url(&server.uri());
        let report = run(
            &inventory,
            &bz,
            &dg,
            &bodhi,
            None,
            &crate::WalkFilterArgs::default(),
            Some("me@example.com"),
            false,
            false,
            false,
            None,
            false,
            true,
            false,
        )
        .await
        .unwrap();
        assert_eq!(report.stale_applied, 1);
        assert_eq!(report.failures, 0);
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
