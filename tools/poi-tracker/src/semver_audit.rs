// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `semver-audit` subcommand.
//!
//! For each maintained package, look at its pending upstream
//! release notification (the open `upstream-release-monitoring@`
//! "X is available" bug) and classify the version bump by semver
//! impact, comparing the new upstream version against the version
//! currently packaged in rawhide dist-git. This lets a maintainer
//! see at a glance which updates are safe minor/patch bumps versus
//! which are potentially breaking and need review.
//!
//! Classification lives in `sandogasa_bugclass::semver` (Cargo's
//! compatibility rule; shared with sandogasa-pkg-health's
//! `pending_update` check). A package retired on rawhide (a
//! `dead.package` marker, the signal `triage-retired` keys on) is
//! reported as "retired" since its update request is moot.
//!
//! A spec that already carries the "available" version doesn't by
//! itself mean the bug is stale: the version may be committed and
//! built but not yet released (a side tag awaiting its Bodhi
//! update, or gating). When the koji CLI is available, rawhide's
//! Koji tag chain (the `rawhide` alias tag) decides: carried →
//! "up to date (stale bug)", not carried → "committed, awaiting
//! release".

use std::collections::BTreeMap;

use sandogasa_bugclass::bugzilla::extract_new_version;
use sandogasa_bugclass::semver::{
    Bump, classify_with_status, numeric_components, version_at_least,
};
use sandogasa_bugzilla::BzClient;
use sandogasa_bugzilla::models::Bug;
use sandogasa_distgit::DistGitClient;
use sandogasa_distgit::spec::parse_version as parse_spec_version;
use sandogasa_inventory::Inventory;
use serde::Serialize;

use crate::triage_retired::{RETRY_ATTEMPTS, retry};
use crate::triage_updates::bug_search_query;

/// The dist-git branch whose spec gives the "current" version.
/// Upstream-release-monitoring bugs track the rawhide package.
const CURRENT_BRANCH: &str = "rawhide";

/// One package's pending update.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub package: String,
    /// Current packaged version (rawhide spec `Version:`), or `?`
    /// when it couldn't be read.
    pub current: String,
    /// New upstream version from the release-monitoring bug.
    pub new: String,
    pub bump: Bump,
    /// Bugzilla id of the release-monitoring bug.
    pub bug_id: u64,
}

/// Choose the bug advertising the highest new version among a
/// package's open release-monitoring bugs. Bugs whose version
/// can't be parsed sort below numeric ones; if none parse, the
/// first bug with an extractable version string wins.
fn pick_latest(bugs: &[Bug], component: &str) -> Option<(u64, String)> {
    let mut best: Option<(u64, String, Option<Vec<u64>>)> = None;
    for bug in bugs {
        let Some(version) = extract_new_version(&bug.summary, component) else {
            continue;
        };
        let parsed = numeric_components(&version);
        let better = match &best {
            None => true,
            Some((_, _, best_parsed)) => match (&parsed, best_parsed) {
                (Some(a), Some(b)) => a > b,
                (Some(_), None) => true,
                _ => false,
            },
        };
        if better {
            best = Some((bug.id, version, parsed));
        }
    }
    best.map(|(id, version, _)| (id, version))
}

/// Run the audit. Returns the entries for packages that have a
/// pending update (after pattern + non-breaking filtering).
/// `latest_tagged` is the Koji lookup used to tell a stale bug
/// from a committed-but-unreleased version; `None` (koji CLI
/// unavailable) keeps the spec-only classification.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    inventory: &Inventory,
    bz: &BzClient,
    dg: &DistGitClient,
    latest_tagged: Option<&crate::triage_updates::TagLookup>,
    filter: &crate::WalkFilterArgs,
    non_breaking_only: bool,
    batch_email: Option<&str>,
    verbose: bool,
) -> Result<Vec<AuditEntry>, String> {
    let mut entries = Vec::new();

    // Batch mode: one Bugzilla query up front instead of one per
    // package; see `triage_updates::batch_bug_query`.
    let batch_bugs = match batch_email {
        Some(email) => {
            if verbose {
                eprintln!("[poi-tracker] batch: querying bugs for {email}");
            }
            let query = crate::triage_updates::batch_bug_query(email, false);
            let bugs = retry(
                "batch bug search",
                RETRY_ATTEMPTS,
                || bz.search(&query, 0),
                verbose,
            )
            .await
            .map_err(|e| format!("Bugzilla batch search: {e}"))?;
            Some(crate::triage_updates::group_bugs_by_component(bugs))
        }
        None => None,
    };

    let mut marked_retired = 0usize;
    for pkg in &inventory.package {
        if !filter.matches(&pkg.name) {
            continue;
        }
        // No longer shipped anywhere (recorded by
        // `prune-retired`): nothing to audit.
        if pkg.is_unshipped() {
            marked_retired += 1;
            if verbose {
                eprintln!(
                    "[poi-tracker] {}: marked unshipped in the \
                     inventory; skipping",
                    pkg.name
                );
            }
            continue;
        }
        // Inventory says it's retired on rawhide (recorded by
        // `triage-retired --mark`): the update request is moot and
        // the checks below would fail anyway — skip without any
        // network traffic.
        if pkg.is_retired_on(CURRENT_BRANCH) {
            marked_retired += 1;
            if verbose {
                eprintln!(
                    "[poi-tracker] {}: marked retired on {CURRENT_BRANCH} in \
                     the inventory; skipping",
                    pkg.name
                );
            }
            continue;
        }
        if verbose {
            eprintln!("[poi-tracker] {}: checking for pending update", pkg.name);
        }
        let per_pkg;
        let bugs: &[Bug] = match &batch_bugs {
            Some(map) => map.get(&pkg.name).map(Vec::as_slice).unwrap_or(&[]),
            None => {
                let query = bug_search_query(&pkg.name);
                per_pkg = retry(
                    &format!("bug search for {}", pkg.name),
                    RETRY_ATTEMPTS,
                    || bz.search(&query, 0),
                    verbose,
                )
                .await
                .map_err(|e| format!("Bugzilla search for {}: {e}", pkg.name))?;
                &per_pkg
            }
        };

        let Some((bug_id, new)) = pick_latest(bugs, &pkg.name) else {
            // No recognizable "is available" bug — nothing pending.
            continue;
        };

        // Current rawhide version from the spec.
        let current = match dg.fetch_spec(&pkg.name, CURRENT_BRANCH).await {
            Ok(spec) => parse_spec_version(&spec),
            Err(e) => {
                if verbose {
                    eprintln!("[poi-tracker] {}: cannot read rawhide spec: {e}", pkg.name);
                }
                None
            }
        };
        // When the spec can't be read, distinguish a retired
        // package (a `dead.package` marker — the same signal
        // `triage-retired` uses, so the update request is invalid)
        // from a genuine unknown. Only probed when needed.
        let retired = current.is_none()
            && dg
                .is_retired(&pkg.name, CURRENT_BRANCH)
                .await
                .unwrap_or(false);

        let mut bump = classify_with_status(current.as_deref(), &new, retired);
        // An exact-equal version (incl. non-numeric ones classify
        // can't compare) means the package already matches — a stale
        // bug, not a pending update.
        if current.as_deref() == Some(new.as_str()) {
            bump = Bump::UpToDate;
        }
        // A matching spec only means "stale bug" when rawhide
        // actually carries a build with the version — one that's
        // only in a side tag (or still gating) is in flight, not
        // stale. The koji `rawhide` alias tag inherits the
        // current fNN, so one lookup answers it.
        if bump == Bump::UpToDate
            && let Some(lookup) = latest_tagged
        {
            if verbose {
                eprintln!(
                    "[poi-tracker] {}: verifying against the {CURRENT_BRANCH} Koji tag",
                    pkg.name
                );
            }
            match lookup(CURRENT_BRANCH, &pkg.name) {
                Ok(nvr) => {
                    let shipped = nvr
                        .as_deref()
                        .and_then(sandogasa_koji::parse_nvr)
                        .is_some_and(|(name, v, _)| name == pkg.name && version_at_least(v, &new));
                    if !shipped {
                        bump = Bump::PendingRelease;
                    }
                }
                Err(e) => {
                    eprintln!(
                        "warning: {}: Koji {CURRENT_BRANCH} query failed: {e} \
                         (cannot verify staleness)",
                        pkg.name
                    );
                }
            }
        }
        let current_str = match (&current, retired) {
            (Some(cur), _) => cur.clone(),
            (None, true) => "(retired)".to_string(),
            (None, false) => "?".to_string(),
        };

        if non_breaking_only && bump != Bump::NonBreaking {
            continue;
        }
        entries.push(AuditEntry {
            package: pkg.name.clone(),
            current: current_str,
            new,
            bump,
            bug_id,
        });
    }

    if marked_retired > 0 {
        eprintln!(
            "({marked_retired} package(s) skipped: marked retired on \
             {CURRENT_BRANCH} in the inventory)"
        );
    }
    Ok(entries)
}

/// Print the audit entries grouped by bump kind.
pub fn print_report(entries: &[AuditEntry]) {
    if entries.is_empty() {
        println!("No pending updates.");
        return;
    }
    let mut by_bump: BTreeMap<&str, Vec<&AuditEntry>> = BTreeMap::new();
    for e in entries {
        by_bump.entry(e.bump.label()).or_default().push(e);
    }
    // Stable, meaningful order rather than alphabetical.
    for kind in [
        Bump::NonBreaking,
        Bump::Breaking,
        Bump::PendingRelease,
        Bump::UpToDate,
        Bump::Retired,
        Bump::NeedsReview,
    ] {
        let Some(group) = by_bump.get(kind.label()) else {
            continue;
        };
        println!("\n{} ({}):", kind.label(), group.len());
        for e in group {
            println!(
                "  {}  {} -> {}  (rhbz#{})",
                e.package, e.current, e.new, e.bug_id
            );
        }
        if kind == Bump::Retired {
            println!("  (run `poi-tracker triage-retired` to close these)");
        }
        if kind == Bump::PendingRelease {
            println!(
                "  (built but not yet in rawhide — waiting on a side \
                 tag merge, gating, or a Bodhi update; nothing to \
                 close yet)"
            );
        }
        if kind == Bump::UpToDate {
            println!(
                "  (run `poi-tracker triage-updates` to record fixed \
                 builds and close these)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn run_skips_packages_marked_retired() {
        // No servers are running: if the marked package weren't
        // skipped, the Bugzilla search would error the run.
        let inventory: sandogasa_inventory::Inventory = toml::from_str(
            "[inventory]\n\
             name = \"test\"\n\
             description = \"test\"\n\
             maintainer = \"tester\"\n\
             \n\
             [[package]]\n\
             name = \"foo\"\n\
             retired_on = [\"rawhide\"]\n",
        )
        .unwrap();
        let bz = BzClient::new("http://127.0.0.1:1");
        let dg = DistGitClient::with_base_url("http://127.0.0.1:1");
        let entries = run(
            &inventory,
            &bz,
            &dg,
            None,
            &crate::WalkFilterArgs::default(),
            false,
            None,
            false,
        )
        .await
        .unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn run_batch_mode_classifies_from_one_query() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .and(query_param("email1", "me@example.com"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [{
                    "id": 7,
                    "summary": "foo-1.2.0 is available",
                    "status": "NEW",
                    "resolution": "",
                    "product": "Fedora",
                    "component": ["foo"],
                    "severity": "unspecified",
                    "priority": "unspecified",
                    "assigned_to": "me@example.com",
                    "creator": "upstream-release-monitoring@fedoraproject.org",
                    "creation_time": "2026-05-01T00:00:00Z",
                    "last_change_time": "2026-05-01T00:00:00Z",
                }],
                "total_matches": 1
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rpms/foo/raw/rawhide/f/foo.spec"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("Name: foo\nVersion: 1.2.0\nRelease: 1%{?dist}\n"),
            )
            .mount(&server)
            .await;

        let inventory: sandogasa_inventory::Inventory = toml::from_str(
            "[inventory]\n\
             name = \"test\"\n\
             description = \"test\"\n\
             maintainer = \"tester\"\n\
             \n\
             [[package]]\n\
             name = \"foo\"\n",
        )
        .unwrap();
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        // No Koji lookup (koji CLI unavailable): the spec-only
        // classification stands.
        let entries = run(
            &inventory,
            &bz,
            &dg,
            None,
            &crate::WalkFilterArgs::default(),
            false,
            Some("me@example.com"),
            false,
        )
        .await
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].package, "foo");
        // Packaged version already matches the available one.
        assert_eq!(entries[0].bump, Bump::UpToDate);
        print_report(&entries);
    }

    /// Shared scaffolding for the Koji-verified stale checks: one
    /// open bug advertising foo-1.2.0, rawhide spec already at
    /// 1.2.0. The Koji lookup stub decides the outcome.
    async fn mount_up_to_date(server: &MockServer) -> sandogasa_inventory::Inventory {
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [{
                    "id": 7,
                    "summary": "foo-1.2.0 is available",
                    "status": "NEW",
                    "resolution": "",
                    "product": "Fedora",
                    "component": ["foo"],
                    "severity": "unspecified",
                    "priority": "unspecified",
                    "assigned_to": "me@example.com",
                    "creator": "upstream-release-monitoring@fedoraproject.org",
                    "creation_time": "2026-05-01T00:00:00Z",
                    "last_change_time": "2026-05-01T00:00:00Z",
                }],
                "total_matches": 1
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rpms/foo/raw/rawhide/f/foo.spec"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("Name: foo\nVersion: 1.2.0\nRelease: 1%{?dist}\n"),
            )
            .mount(server)
            .await;
        toml::from_str(
            "[inventory]\n\
             name = \"test\"\n\
             description = \"test\"\n\
             maintainer = \"tester\"\n\
             \n\
             [[package]]\n\
             name = \"foo\"\n",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn side_tag_only_version_is_pending_release_not_stale() {
        // The spec carries 1.2.0 but rawhide's tag chain still
        // resolves to 1.1.0 (the 1.2.0 build sits in a side tag)
        // -> committed-awaiting-release, not a stale bug.
        let server = MockServer::start().await;
        let inventory = mount_up_to_date(&server).await;
        let lookup = |tag: &str, pkg: &str| -> Result<Option<String>, String> {
            assert_eq!((tag, pkg), ("rawhide", "foo"));
            Ok(Some("foo-1.1.0-1.fc45".to_string()))
        };
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        let entries = run(
            &inventory,
            &bz,
            &dg,
            Some(&lookup),
            &crate::WalkFilterArgs::default(),
            false,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].bump, Bump::PendingRelease);
        print_report(&entries);
    }

    #[tokio::test]
    async fn tagged_version_confirms_stale_bug() {
        // Rawhide's tag chain already carries 1.2.0 -> genuinely
        // stale. A Koji error must also keep the spec verdict
        // (warn, don't reclassify).
        let server = MockServer::start().await;
        let inventory = mount_up_to_date(&server).await;
        let lookup = |_: &str, _: &str| -> Result<Option<String>, String> {
            Ok(Some("foo-1.2.0-1.fc45".to_string()))
        };
        let bz = BzClient::new(&server.uri());
        let dg = DistGitClient::with_base_url(&server.uri());
        let entries = run(
            &inventory,
            &bz,
            &dg,
            Some(&lookup),
            &crate::WalkFilterArgs::default(),
            false,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(entries[0].bump, Bump::UpToDate);

        let failing =
            |_: &str, _: &str| -> Result<Option<String>, String> { Err("koji down".into()) };
        let entries = run(
            &inventory,
            &bz,
            &dg,
            Some(&failing),
            &crate::WalkFilterArgs::default(),
            false,
            None,
            false,
        )
        .await
        .unwrap();
        assert_eq!(entries[0].bump, Bump::UpToDate);
    }

    fn bug(id: u64, summary: &str) -> Bug {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "summary": summary,
            "status": "NEW",
            "resolution": "",
            "product": "Fedora",
            "component": ["foo"],
            "severity": "unspecified",
            "priority": "unspecified",
            "assigned_to": "nobody@fedoraproject.org",
            "creator": "upstream-release-monitoring@fedoraproject.org",
            "creation_time": "2026-01-01T00:00:00Z",
            "last_change_time": "2026-01-01T00:00:00Z",
        }))
        .unwrap()
    }

    #[test]
    fn pick_latest_chooses_highest_version() {
        let bugs = vec![
            bug(1, "foo-1.2.0 is available"),
            bug(2, "foo-1.10.0 is available"),
            bug(3, "foo-1.3.0 is available"),
        ];
        assert_eq!(pick_latest(&bugs, "foo"), Some((2, "1.10.0".to_string())));
    }

    #[test]
    fn pick_latest_none_when_unparseable_summary() {
        let bugs = vec![bug(1, "wat")];
        assert_eq!(pick_latest(&bugs, "foo"), None);
    }
}
