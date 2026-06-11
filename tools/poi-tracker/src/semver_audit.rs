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
//! Classification uses Cargo's compatibility rule (the Rust
//! convention): a change at or before the leftmost non-zero
//! component of the current version is breaking, so `1.4 -> 1.5`
//! is non-breaking but `0.4 -> 0.5` is breaking. Versions that
//! aren't plain dotted integers (pre-releases, dates, snapshots)
//! are reported as "needs review" rather than guessed at, and a
//! package retired on rawhide (a `dead.package` marker, the signal
//! `triage-retired` keys on) is reported as "retired" since its
//! update request is moot.

use std::collections::BTreeMap;

use sandogasa_bugzilla::BzClient;
use sandogasa_bugzilla::models::Bug;
use sandogasa_distgit::DistGitClient;
use sandogasa_inventory::Inventory;
use serde::Serialize;

use crate::triage_retired::{RETRY_ATTEMPTS, retry};
use crate::triage_updates::bug_search_query;

/// The dist-git branch whose spec gives the "current" version.
/// Upstream-release-monitoring bugs track the rawhide package.
const CURRENT_BRANCH: &str = "rawhide";

/// Semver impact of a pending update.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bump {
    /// The packaged version already equals the "available" version
    /// — a stale release-monitoring bug, not a pending update.
    /// (Excluded by `--non-breaking`, since there's nothing to push.)
    UpToDate,
    /// Safe under the Cargo compatibility rule (patch, or minor
    /// when the leading component is non-zero).
    NonBreaking,
    /// Changes the version's significant (leftmost non-zero)
    /// component.
    Breaking,
    /// The package is retired on the branch (a `dead.package`
    /// marker is present, the same signal `triage-retired` uses),
    /// so the update request is invalid — there's no live package
    /// to update.
    Retired,
    /// Could not be classified: a non-numeric version, an unknown
    /// current version, or a downgrade.
    NeedsReview,
}

impl Bump {
    fn label(self) -> &'static str {
        match self {
            Bump::UpToDate => "Up to date (stale bug)",
            Bump::NonBreaking => "Non-breaking",
            Bump::Breaking => "Breaking",
            Bump::Retired => "Retired (update request invalid)",
            Bump::NeedsReview => "Needs review",
        }
    }
}

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

/// Parse a version into its dot-separated numeric components.
/// Returns `None` if any component isn't a bare non-negative
/// integer (pre-release tags, dates with letters, git snapshots,
/// unexpanded spec macros, ...).
///
/// Semver build metadata (a `+suffix`, e.g. libbpf-sys's
/// `1.7.0+v1.7.0`) must be ignored when determining precedence,
/// so it is stripped before parsing.
fn numeric_components(version: &str) -> Option<Vec<u64>> {
    let v = version.trim();
    let v = v.split('+').next().unwrap_or(v);
    if v.is_empty() {
        return None;
    }
    v.split('.').map(|c| c.parse::<u64>().ok()).collect()
}

/// Whether `candidate` is at least `target`, comparing dotted
/// numeric components (shorter versions are zero-padded). Used to
/// decide whether a build addresses a release-monitoring bug.
/// Non-numeric versions only match on exact string equality.
pub fn version_at_least(candidate: &str, target: &str) -> bool {
    match (numeric_components(candidate), numeric_components(target)) {
        (Some(c), Some(t)) => {
            let width = c.len().max(t.len());
            let pad = |v: &[u64]| -> Vec<u64> {
                (0..width).map(|i| v.get(i).copied().unwrap_or(0)).collect()
            };
            pad(&c) >= pad(&t)
        }
        _ => candidate == target,
    }
}

/// Classify a `current -> new` bump using Cargo's compatibility
/// rule: a change at or before the leftmost non-zero component of
/// `current` is breaking.
pub fn classify(current: &str, new: &str) -> Bump {
    let (Some(cur), Some(new_c)) = (numeric_components(current), numeric_components(new)) else {
        return Bump::NeedsReview;
    };
    let width = cur.len().max(new_c.len());
    let cur: Vec<u64> = (0..width)
        .map(|i| cur.get(i).copied().unwrap_or(0))
        .collect();
    let new_c: Vec<u64> = (0..width)
        .map(|i| new_c.get(i).copied().unwrap_or(0))
        .collect();
    if new_c == cur {
        // Same version — the bug is stale, nothing to update.
        return Bump::UpToDate;
    }
    if new_c < cur {
        // Downgrade — unexpected for an "is available" bug.
        return Bump::NeedsReview;
    }
    // Index of the leftmost significant (non-zero) component. An
    // all-zero current version can't anchor the rule.
    let Some(lead) = cur.iter().position(|&x| x != 0) else {
        return Bump::NeedsReview;
    };
    if (0..=lead).any(|i| cur[i] != new_c[i]) {
        Bump::Breaking
    } else {
        Bump::NonBreaking
    }
}

/// Decide a package's bump given a possibly-unreadable current
/// version. A missing current version is treated as `Retired` when
/// the branch carries a `dead.package` marker (the update request
/// is moot), otherwise `NeedsReview`.
pub fn classify_with_status(current: Option<&str>, new: &str, retired: bool) -> Bump {
    match current {
        Some(cur) => classify(cur, new),
        None if retired => Bump::Retired,
        None => Bump::NeedsReview,
    }
}

/// Extract the new version from a release-monitoring bug summary
/// of the form `"<component>-<version> is available"`.
pub fn extract_new_version(summary: &str, component: &str) -> Option<String> {
    let body = summary.trim().strip_suffix(" is available")?;
    // The component prefix is followed by a single `-`; the rest is
    // the version (which may itself contain `-`, e.g. `1.0-r2707`).
    let rest = body.strip_prefix(component)?;
    let version = rest.strip_prefix('-').unwrap_or(rest);
    (!version.is_empty()).then(|| version.to_string())
}

/// Read a `Tag:` field (e.g. `Version`, `Release`) from a spec file.
pub fn parse_spec_field(spec: &str, tag: &str) -> Option<String> {
    let prefix = format!("{tag}:");
    for line in spec.lines() {
        if let Some(rest) = line.trim_start().strip_prefix(&prefix) {
            let v = rest.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Read the `Version:` field from a spec file.
pub fn parse_spec_version(spec: &str) -> Option<String> {
    parse_spec_field(spec, "Version")
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
pub async fn run(
    inventory: &Inventory,
    bz: &BzClient,
    dg: &DistGitClient,
    patterns: &[String],
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
            let query = crate::triage_updates::batch_bug_query(email);
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

    for pkg in &inventory.package {
        if !crate::matches_any_pattern(&pkg.name, patterns) {
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
        let entries = run(
            &inventory,
            &bz,
            &dg,
            &[],
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

    #[test]
    fn classify_minor_and_patch_are_non_breaking() {
        assert_eq!(classify("1.4.2", "1.5.0"), Bump::NonBreaking);
        assert_eq!(classify("1.4.2", "1.4.3"), Bump::NonBreaking);
        assert_eq!(classify("1.4", "1.4.1"), Bump::NonBreaking);
    }

    #[test]
    fn classify_major_is_breaking() {
        assert_eq!(classify("1.4.2", "2.0.0"), Bump::Breaking);
    }

    #[test]
    fn classify_same_version_is_up_to_date() {
        // Stale bug: packaged version already matches "available".
        assert_eq!(classify("0.6.1", "0.6.1"), Bump::UpToDate);
        // Equal despite differing component count.
        assert_eq!(classify("1.4", "1.4.0"), Bump::UpToDate);
    }

    #[test]
    fn classify_zero_x_follows_cargo_rule() {
        // 0.x: the minor is the significant component.
        assert_eq!(classify("0.4.0", "0.5.0"), Bump::Breaking);
        assert_eq!(classify("0.4.2", "0.4.3"), Bump::NonBreaking);
        // 0.0.x: the patch is significant (^0.0.3 is exact).
        assert_eq!(classify("0.0.3", "0.0.4"), Bump::Breaking);
    }

    #[test]
    fn classify_non_numeric_needs_review() {
        assert_eq!(classify("1.0", "2.0rc1"), Bump::NeedsReview);
        assert_eq!(classify("5.000a", "5.000b"), Bump::NeedsReview);
        assert_eq!(classify("1.2.3", "1.2.4.dev-r1"), Bump::NeedsReview);
    }

    #[test]
    fn classify_ignores_build_metadata() {
        // Semver build metadata (`+...`) is ignored for precedence:
        // rust-libbpf-sys 1.6.2 -> 1.7.0+v1.7.0 is a plain minor bump.
        assert_eq!(classify("1.6.2", "1.7.0+v1.7.0"), Bump::NonBreaking);
        assert_eq!(classify("1.6.2+v1.6.2", "2.0.0"), Bump::Breaking);
        assert_eq!(classify("1.7.0+v1.6.0", "1.7.0+v1.7.0"), Bump::UpToDate);
        assert!(version_at_least("1.7.0+v1.7.0", "1.7.0"));
    }

    #[test]
    fn classify_downgrade_needs_review() {
        assert_eq!(classify("2.0.0", "1.9.0"), Bump::NeedsReview);
    }

    #[test]
    fn version_at_least_compares_numerically() {
        assert!(version_at_least("1.10.0", "1.9.0"));
        assert!(version_at_least("0.6.1", "0.6.1"));
        assert!(version_at_least("1.4", "1.4.0"));
        assert!(!version_at_least("1.4.0", "1.4.1"));
        // Non-numeric: exact match only.
        assert!(version_at_least("2.0rc1", "2.0rc1"));
        assert!(!version_at_least("2.0rc2", "2.0rc1"));
    }

    #[test]
    fn classify_with_status_handles_unreadable_current() {
        // Spec readable -> normal classification.
        assert_eq!(
            classify_with_status(Some("1.4.2"), "1.5.0", false),
            Bump::NonBreaking
        );
        // Unreadable + retired -> the update request is invalid.
        assert_eq!(classify_with_status(None, "0.9.0", true), Bump::Retired);
        // Unreadable + not retired -> genuinely unknown.
        assert_eq!(
            classify_with_status(None, "0.9.0", false),
            Bump::NeedsReview
        );
    }

    #[test]
    fn extract_new_version_handles_real_summaries() {
        assert_eq!(
            extract_new_version(
                "transmission-remote-cli-1.7.1 is available",
                "transmission-remote-cli"
            )
            .as_deref(),
            Some("1.7.1")
        );
        // Version containing a dash is preserved after the first one.
        assert_eq!(
            extract_new_version(
                "python-peak-rules-0.5a1.dev-r2707 is available",
                "python-peak-rules"
            )
            .as_deref(),
            Some("0.5a1.dev-r2707")
        );
    }

    #[test]
    fn extract_new_version_rejects_unrecognized() {
        assert_eq!(extract_new_version("something unrelated", "foo"), None);
        assert_eq!(
            extract_new_version("otherpkg-1.0 is available", "foo"),
            None
        );
    }

    #[test]
    fn parse_spec_version_reads_version_line() {
        let spec = "Name: foo\nVersion: 1.2.3\nRelease: 1%{?dist}\n";
        assert_eq!(parse_spec_version(spec).as_deref(), Some("1.2.3"));
    }

    #[test]
    fn parse_spec_version_absent() {
        assert_eq!(parse_spec_version("Name: foo\nRelease: 1\n"), None);
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
