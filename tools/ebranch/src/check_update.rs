// SPDX-License-Identifier: MPL-2.0

//! Reverse-dependency breakage checker for Koji side tags and Bodhi updates.
//!
//! Given a set of updated packages (from a side tag or Bodhi update),
//! compares old vs new subpackage Provides to detect removed capabilities,
//! then finds reverse dependencies in the stable repo that would break.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use rayon::prelude::*;
use serde::Serialize;

/// Filter out fedrq's "(none)" placeholder from results.
fn filter_none(items: Vec<String>) -> Vec<String> {
    items
        .into_iter()
        .filter(|s| !s.is_empty() && s != "(none)")
        .collect()
}

/// Extract the name part of a Provide string.
///
/// E.g. "tor = 0.4.9.5-1.el9" → "tor",
///      "config(tor) = 0.4.9.5-1.el9" → "config(tor)",
///      "libthing.so.1()(64bit)" → "libthing.so.1()(64bit)"
fn provide_name(provide: &str) -> &str {
    // Split on version operators: " = ", " < ", " > ", " <= ", " >= "
    provide
        .split_once(" = ")
        .or_else(|| provide.split_once(" < "))
        .or_else(|| provide.split_once(" > "))
        .or_else(|| provide.split_once(" <= "))
        .or_else(|| provide.split_once(" >= "))
        .map(|(name, _)| name)
        .unwrap_or(provide)
}

/// Compare old and new provides, classifying each as updated or removed.
fn compare_provides(old: &BTreeSet<String>, new: &BTreeSet<String>) -> Vec<ChangedProvide> {
    // Build a map from provide name → full provide string for new provides.
    let new_by_name: BTreeMap<&str, &str> =
        new.iter().map(|p| (provide_name(p), p.as_str())).collect();

    let mut changed = Vec::new();
    for old_provide in old.difference(new) {
        let name = provide_name(old_provide);
        let new_match = new_by_name.get(name).map(|s| s.to_string());
        changed.push(ChangedProvide {
            old: old_provide.clone(),
            new: new_match,
        });
    }
    changed
}

/// Extract a testing branch from a side tag name.
///
/// E.g. "epel9-build-side-133287" → "epel9",
///      "epel10.0-build-side-12345" → "epel10.0"
fn testing_branch_from_side_tag(tag: Option<&str>) -> Option<String> {
    let tag = tag?;
    let rest = tag.strip_prefix("epel")?;
    let release_end = rest.find("-build-side-")?;
    Some(format!("epel{}", &rest[..release_end]))
}

/// What kind of input the user provided.
pub enum InputKind {
    /// A Koji side tag name (e.g. "epel9-build-side-133287").
    SideTag(String),
    /// A Bodhi update alias (e.g. "FEDORA-EPEL-2026-f9eaa11e18").
    BodhiAlias(String),
}

/// Options for the check-update command.
pub struct CheckUpdateOptions {
    pub branch: Option<String>,
    pub repo: Option<String>,
    /// Override branch for @testing queries (defaults to branch).
    pub testing_branch: Option<String>,
    pub koji_profile: Option<String>,
    pub verbose: bool,
}

/// A Provide that changed between old and new versions.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedProvide {
    /// The old Provide string (e.g. "tor = 0.4.9.5-1.el9").
    pub old: String,
    /// The new Provide string, if the name still exists.
    pub new: Option<String>,
}

impl ChangedProvide {
    /// Whether this is a version update (name still provided)
    /// vs a true removal.
    pub fn is_updated(&self) -> bool {
        self.new.is_some()
    }
}

/// A single broken Requires entry.
#[derive(Debug, Clone, Serialize)]
pub struct BrokenRequires {
    /// The Requires string that would break.
    pub dep: String,
    /// The Provide that was removed or updated.
    pub changed_provide: String,
}

/// Result of checking a single reverse dependency.
#[derive(Debug, Serialize)]
pub struct RevDepResult {
    /// "ok" or "broken".
    pub status: String,
    /// Broken Requires, if any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<BrokenRequires>,
}

/// Full result of the check-update command.
#[derive(Debug, Serialize)]
pub struct CheckUpdateReport {
    pub input: String,
    pub branch: String,
    pub updated_packages: Vec<String>,
    /// Whether full provides comparison was performed.
    pub full_analysis: bool,
    /// Provides that changed (updated version or truly removed).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_provides: Vec<ChangedProvide>,
    pub reverse_deps: BTreeMap<String, RevDepResult>,
}

/// Detect what kind of input the user provided.
pub fn detect_input_type(input: &str) -> InputKind {
    // Strip URL prefix to get the alias.
    let candidate = if let Some(rest) = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
    {
        // Last path segment is the alias.
        rest.rsplit('/').next().unwrap_or(rest)
    } else {
        input
    };

    // Bodhi aliases look like FEDORA-2026-abc123 or FEDORA-EPEL-2026-abc123.
    if candidate.starts_with("FEDORA-") && candidate.contains("-20") {
        InputKind::BodhiAlias(candidate.to_string())
    } else {
        InputKind::SideTag(input.to_string())
    }
}

/// Extract the source package name from an NVR string.
///
/// E.g. "rust-uucore-0.0.28-2.el9" → "rust-uucore"
pub fn parse_nvr(nvr: &str) -> Option<&str> {
    let mut parts = nvr.rsplitn(3, '-');
    let _release = parts.next()?;
    let _version = parts.next()?;
    let name = parts.next()?;
    if name.is_empty() { None } else { Some(name) }
}

/// List NVRs in a Koji tag via `koji list-tagged --quiet`.
pub fn koji_list_tagged(tag: &str, profile: Option<&str>) -> Result<Vec<String>, String> {
    let mut cmd = Command::new("koji");
    if let Some(p) = profile {
        cmd.args(["--profile", p]);
    }
    cmd.args(["list-tagged", "--quiet", tag]);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run koji: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("koji list-tagged failed: {}", stderr.trim()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next())
        .map(|s| s.to_string())
        .collect())
}

/// List binary RPM names for a build via `koji buildinfo`.
///
/// Parses the RPMs section, returning binary package names
/// (excluding `.src.rpm` entries).
pub fn koji_build_rpms(nvr: &str, profile: Option<&str>) -> Result<Vec<String>, String> {
    let mut cmd = Command::new("koji");
    if let Some(p) = profile {
        cmd.args(["--profile", p]);
    }
    cmd.args(["buildinfo", nvr]);
    let output = cmd
        .output()
        .map_err(|e| format!("failed to run koji: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("koji buildinfo failed: {}", stderr.trim()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse RPMs section. Lines after "RPMs:" are paths like:
    //   /mnt/koji/.../name-ver-rel.arch.rpm\tSignatures: ...
    let mut in_rpms = false;
    let mut names = Vec::new();
    for line in stdout.lines() {
        if line.starts_with("RPMs:") {
            in_rpms = true;
            continue;
        }
        if !in_rpms {
            continue;
        }
        let path = line.split('\t').next().unwrap_or("").trim();
        if path.is_empty() {
            continue;
        }
        // Extract filename from path.
        let filename = path.rsplit('/').next().unwrap_or(path);
        // Skip source RPMs.
        if filename.ends_with(".src.rpm") {
            continue;
        }
        // Strip .arch.rpm to get the binary package name.
        // Format: name-version-release.arch.rpm
        if let Some(without_rpm) = filename.strip_suffix(".rpm") {
            // Remove .arch suffix (last dot-separated segment).
            if let Some(dot_pos) = without_rpm.rfind('.') {
                let name = &without_rpm[..dot_pos];
                // Extract just the package name (without version-release).
                if let Some(parsed) = parse_nvr(name) {
                    names.push(parsed.to_string());
                }
            }
        }
    }
    Ok(names)
}

struct BodhiUpdateInfo {
    side_tag: Option<String>,
    nvrs: Vec<String>,
    release_name: Option<String>,
}

/// Fetch a Bodhi update and extract its key fields.
fn fetch_bodhi_update(alias: &str) -> Result<BodhiUpdateInfo, String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;
    let update = rt
        .block_on(async {
            let client = sandogasa_bodhi::BodhiClient::new();
            client.update_by_alias(alias).await
        })
        .map_err(|e| format!("failed to fetch Bodhi update {alias}: {e}"))?;

    let release_name = update.release.map(|r| r.name);
    let nvrs: Vec<String> = update.builds.iter().map(|b| b.nvr.clone()).collect();
    Ok(BodhiUpdateInfo {
        side_tag: update.from_side_tag,
        nvrs,
        release_name,
    })
}

/// Compute changed provides using `subpkgs_provides` on both repos.
///
/// Works for @testing and any repo where source RPM queries work.
fn compute_changed_provides_via_subpkgs(
    updated_packages: &[String],
    stable_fedrq: &sandogasa_fedrq::Fedrq,
    new_fedrq: &sandogasa_fedrq::Fedrq,
) -> Vec<ChangedProvide> {
    updated_packages
        .par_iter()
        .flat_map(|srpm| {
            let old_provides: BTreeSet<String> =
                filter_none(stable_fedrq.subpkgs_provides(srpm).unwrap_or_default())
                    .into_iter()
                    .collect();

            let new_provides: BTreeSet<String> = new_fedrq
                .subpkgs_provides(srpm)
                .ok()
                .map(|v| filter_none(v).into_iter().collect())
                .unwrap_or_default();

            compare_provides(&old_provides, &new_provides)
        })
        .collect()
}

/// Compute changed provides for side tags using `koji buildinfo`
/// + `fedrq pkg_provides`.
///
/// Side tags don't index source RPMs, so we get binary RPM names
/// from koji and query their provides individually.
fn compute_changed_provides_via_koji(
    nvrs: &[String],
    updated_packages: &[String],
    stable_fedrq: &sandogasa_fedrq::Fedrq,
    side_tag_fedrq: &sandogasa_fedrq::Fedrq,
    koji_profile: Option<&str>,
    verbose: bool,
) -> Vec<ChangedProvide> {
    // Get old provides from stable repo (source-based query works here).
    let old_provides: BTreeSet<String> = updated_packages
        .par_iter()
        .flat_map(|srpm| filter_none(stable_fedrq.subpkgs_provides(srpm).unwrap_or_default()))
        .collect();

    // Get new provides: binary RPM names via koji, then query
    // each one's provides from the side tag repo.
    let binary_names: Vec<String> = nvrs
        .iter()
        .flat_map(|nvr| {
            koji_build_rpms(nvr, koji_profile).unwrap_or_else(|e| {
                if verbose {
                    eprintln!(
                        "[check-update] warning: \
                         koji buildinfo {nvr} failed: {e}"
                    );
                }
                vec![]
            })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if verbose {
        eprintln!(
            "[check-update] {} binary packages from koji: {}",
            binary_names.len(),
            binary_names.join(", "),
        );
    }

    let new_provides: BTreeSet<String> = binary_names
        .par_iter()
        .flat_map(|name| filter_none(side_tag_fedrq.pkg_provides(name).unwrap_or_default()))
        .collect();

    compare_provides(&old_provides, &new_provides)
}

/// Given old and new provides per source package, classify changes,
/// find affected reverse deps, and check their Requires.
fn run_provides_analysis(
    input: &str,
    branch: &str,
    updated_packages: &[String],
    changed_provides: Vec<ChangedProvide>,
    stable_fedrq: &sandogasa_fedrq::Fedrq,
    opts: &CheckUpdateOptions,
) -> Result<CheckUpdateReport, String> {
    if opts.verbose {
        let updated_count = changed_provides.iter().filter(|c| c.is_updated()).count();
        let removed_count = changed_provides.iter().filter(|c| !c.is_updated()).count();
        eprintln!(
            "[check-update] {} changed provides \
             ({updated_count} updated, {removed_count} removed)",
            changed_provides.len()
        );
    }

    if changed_provides.is_empty() {
        return Ok(CheckUpdateReport {
            input: input.to_string(),
            branch: branch.to_string(),
            updated_packages: updated_packages.to_vec(),
            full_analysis: true,
            changed_provides: vec![],
            reverse_deps: BTreeMap::new(),
        });
    }

    // Use old provide strings for whatrequires lookup.
    let old_provide_strings: Vec<String> = changed_provides.iter().map(|c| c.old.clone()).collect();
    let old_provide_set: BTreeSet<&str> = old_provide_strings.iter().map(|s| s.as_str()).collect();

    if opts.verbose {
        eprintln!("[check-update] finding reverse dependencies");
    }

    let rev_dep_sources = filter_none(
        stable_fedrq
            .whatrequires(&old_provide_strings)
            .map_err(|e| format!("whatrequires failed: {e}"))?,
    );

    let updated_set: BTreeSet<&str> = updated_packages.iter().map(|s| s.as_str()).collect();
    let rev_deps: Vec<String> = rev_dep_sources
        .into_iter()
        .filter(|pkg| !updated_set.contains(pkg.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if opts.verbose {
        eprintln!(
            "[check-update] {} reverse dependencies to check: {}",
            rev_deps.len(),
            rev_deps.join(", "),
        );
    }

    let results: Vec<(String, RevDepResult)> = rev_deps
        .par_iter()
        .map(|pkg| {
            let requires = stable_fedrq.subpkgs_requires(pkg).unwrap_or_default();

            let issues: Vec<BrokenRequires> = requires
                .iter()
                .filter_map(|dep| {
                    let dep_str = dep.trim();
                    if old_provide_set.contains(dep_str) {
                        Some(BrokenRequires {
                            dep: dep_str.to_string(),
                            changed_provide: dep_str.to_string(),
                        })
                    } else {
                        None
                    }
                })
                .collect();

            let status = if issues.is_empty() { "ok" } else { "broken" };
            (
                pkg.clone(),
                RevDepResult {
                    status: status.to_string(),
                    issues,
                },
            )
        })
        .collect();

    let reverse_deps: BTreeMap<String, RevDepResult> = results.into_iter().collect();

    Ok(CheckUpdateReport {
        input: input.to_string(),
        branch: branch.to_string(),
        updated_packages: updated_packages.to_vec(),
        full_analysis: true,
        changed_provides,
        reverse_deps,
    })
}

/// Warn if any package from koji list-tagged has no changed provides,
/// which suggests the side tag repo metadata is stale.
fn check_side_tag_staleness(nvrs: &[String], changed: &[ChangedProvide]) {
    // Collect all provide name strings that changed.
    let changed_old: BTreeSet<&str> = changed.iter().map(|c| c.old.as_str()).collect();

    // For each NVR, check if the version from the NVR appears in any
    // changed provide. If the NVR says 1.51.0 but no provide mentions
    // 1.51.0, the side tag repo is likely stale for that package.
    for nvr in nvrs {
        let Some(name) = parse_nvr(nvr) else {
            continue;
        };
        // Extract version from NVR.
        let version = nvr
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('-'))
            .and_then(|vr| vr.split_once('-'))
            .map(|(v, _)| v);
        let Some(version) = version else {
            continue;
        };

        // Check if any changed provide references this package's
        // version (from the NVR).
        let has_version_match = changed_old
            .iter()
            .any(|p| p.contains(name) || p.contains(version));

        if !has_version_match {
            eprintln!(
                "warning: {name}: side tag repo may be stale \
                 (expected version {version} from {nvr} not found \
                 in changed provides); consider running \
                 'koji regen-repo' or using @testing if available"
            );
        }
    }
}

/// Run the check-update analysis.
pub fn check_update(input: &str, opts: &CheckUpdateOptions) -> Result<CheckUpdateReport, String> {
    // Phase 0: Determine side tag, NVRs, and branch.
    let (side_tag, nvrs, branch) = match detect_input_type(input) {
        InputKind::SideTag(tag) => {
            let branch = opts
                .branch
                .clone()
                .ok_or("--branch is required for side tag input")?;
            let nvrs = koji_list_tagged(&tag, opts.koji_profile.as_deref())?;
            (Some(tag), nvrs, branch)
        }
        InputKind::BodhiAlias(alias) => {
            let info = fetch_bodhi_update(&alias)?;

            let branch = opts.branch.clone().or_else(|| {
                info.release_name
                    .as_ref()
                    .map(|r| r.to_lowercase().replace('-', ""))
            });
            let branch =
                branch.ok_or("could not determine branch from Bodhi release; use --branch")?;

            // If backed by a side tag, use koji to get the full list.
            let (side_tag, nvrs) = if let Some(tag) = info.side_tag {
                let koji_nvrs = koji_list_tagged(&tag, opts.koji_profile.as_deref())?;
                (Some(tag), koji_nvrs)
            } else {
                (None, info.nvrs)
            };

            (side_tag, nvrs, branch)
        }
    };

    // Extract unique source package names from NVRs.
    let updated_packages: Vec<String> = nvrs
        .iter()
        .filter_map(|nvr| parse_nvr(nvr))
        .map(|s| s.to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if opts.verbose {
        eprintln!(
            "[check-update] updated packages: {}",
            updated_packages.join(", ")
        );
    }

    if updated_packages.is_empty() {
        return Ok(CheckUpdateReport {
            input: input.to_string(),
            branch: branch.clone(),
            updated_packages: vec![],
            full_analysis: side_tag.is_some(),
            changed_provides: vec![],
            reverse_deps: BTreeMap::new(),
        });
    }

    // Set up fedrq for the stable repo.
    let stable_fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(branch.clone()),
        repo: opts.repo.clone(),
    };

    // Get subpackage names for the updated source packages.
    if opts.verbose {
        eprintln!("[check-update] querying subpackage names");
    }

    let all_subpkg_names: Vec<String> = updated_packages
        .par_iter()
        .flat_map(|srpm| filter_none(stable_fedrq.subpkgs_names(srpm).unwrap_or_default()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if opts.verbose {
        eprintln!(
            "[check-update] {} subpackages: {}",
            all_subpkg_names.len(),
            all_subpkg_names.join(", "),
        );
    }

    // Without a side tag we cannot compare old vs new provides,
    // so we fall back to listing all reverse deps without a
    // broken/ok judgment.
    let new_branch = opts
        .testing_branch
        .clone()
        .or_else(|| testing_branch_from_side_tag(side_tag.as_deref()))
        .unwrap_or_else(|| branch.clone());

    let side_tag_fedrq = side_tag.as_ref().map(|tag| sandogasa_fedrq::Fedrq {
        branch: None,
        repo: Some(format!("@koji:{tag}")),
    });

    // Prefer @testing when the update has been pushed to testing,
    // since it has authoritative repo metadata (no staleness issue).
    let testing_fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(new_branch),
        repo: Some("@testing".to_string()),
    };

    let has_testing = updated_packages
        .first()
        .and_then(|pkg| testing_fedrq.subpkgs_names(pkg).ok())
        .map(filter_none)
        .is_some_and(|names| !names.is_empty());

    if has_testing {
        if opts.verbose {
            eprintln!("[check-update] using @testing for new provides");
        }
        let changed =
            compute_changed_provides_via_subpkgs(&updated_packages, &stable_fedrq, &testing_fedrq);
        return run_provides_analysis(
            input,
            &branch,
            &updated_packages,
            changed,
            &stable_fedrq,
            opts,
        );
    }

    if let Some(ref side_fq) = side_tag_fedrq {
        if opts.verbose {
            eprintln!("[check-update] comparing provides via koji + side tag");
        }
        let changed = compute_changed_provides_via_koji(
            &nvrs,
            &updated_packages,
            &stable_fedrq,
            side_fq,
            opts.koji_profile.as_deref(),
            opts.verbose,
        );

        // Warn if the side tag repo appears stale.
        check_side_tag_staleness(&nvrs, &changed);

        return run_provides_analysis(
            input,
            &branch,
            &updated_packages,
            changed,
            &stable_fedrq,
            opts,
        );
    }

    // No side tag and not in @testing: just list reverse deps.
    if opts.verbose {
        eprintln!(
            "[check-update] no side tag or @testing available; \
             listing reverse deps only"
        );
    }

    let rev_dep_sources = filter_none(
        stable_fedrq
            .whatrequires(&all_subpkg_names)
            .map_err(|e| format!("whatrequires failed: {e}"))?,
    );

    let updated_set: BTreeSet<&str> = updated_packages.iter().map(|s| s.as_str()).collect();
    let rev_deps: Vec<String> = rev_dep_sources
        .into_iter()
        .filter(|pkg| !updated_set.contains(pkg.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let reverse_deps: BTreeMap<String, RevDepResult> = rev_deps
        .into_iter()
        .map(|pkg| {
            (
                pkg,
                RevDepResult {
                    status: "unknown".to_string(),
                    issues: vec![],
                },
            )
        })
        .collect();

    Ok(CheckUpdateReport {
        input: input.to_string(),
        branch,
        updated_packages,
        full_analysis: false,
        changed_provides: vec![],
        reverse_deps,
    })
}

/// Print a human-readable report to stdout.
pub fn print_report(report: &CheckUpdateReport) {
    println!("Checking update: {}", report.input);
    println!("Branch: {}", report.branch);
    println!("Updated packages: {}\n", report.updated_packages.join(", "));

    if !report.full_analysis {
        // No side tag available — informational mode.
        println!("Note: no side tag available; cannot compare Provides.");
        println!("Listing reverse dependencies for manual review.\n");
        if report.reverse_deps.is_empty() {
            println!("No reverse dependencies found.");
        } else {
            println!("Reverse dependencies:");
            for pkg in report.reverse_deps.keys() {
                println!("  - {pkg}");
            }
            println!(
                "\nTotal: {} reverse dependencies.",
                report.reverse_deps.len()
            );
        }
        return;
    }

    if report.changed_provides.is_empty() {
        println!("No changed Provides. No breakage expected.");
        return;
    }

    let updated: Vec<&ChangedProvide> = report
        .changed_provides
        .iter()
        .filter(|c| c.is_updated())
        .collect();
    let removed: Vec<&ChangedProvide> = report
        .changed_provides
        .iter()
        .filter(|c| !c.is_updated())
        .collect();

    if !updated.is_empty() {
        println!("Updated Provides ({}):", updated.len());
        for c in &updated {
            let name = provide_name(&c.old);
            let old_ver = c
                .old
                .strip_prefix(name)
                .unwrap_or("")
                .trim()
                .trim_start_matches("= ");
            let new_ver = c
                .new
                .as_deref()
                .and_then(|n| n.strip_prefix(name))
                .unwrap_or("")
                .trim()
                .trim_start_matches("= ");
            println!("  - {name} ({old_ver} -> {new_ver})");
        }
    }
    if !removed.is_empty() {
        if !updated.is_empty() {
            println!();
        }
        println!("Removed Provides ({}):", removed.len());
        for c in &removed {
            println!("  - {}", c.old);
        }
    }

    if report.reverse_deps.is_empty() {
        println!(
            "\nNo packages depend on the changed Provides. \
             No breakage expected."
        );
        return;
    }

    println!("\nReverse dependencies:");
    let mut broken_count = 0;
    for (pkg, result) in &report.reverse_deps {
        if result.status == "ok" {
            println!("  {pkg}: OK");
        } else {
            broken_count += 1;
            println!("  {pkg}: BROKEN");
            for issue in &result.issues {
                println!("    - {} (changed: {})", issue.dep, issue.changed_provide);
            }
        }
    }

    let total = report.reverse_deps.len();
    if broken_count > 0 {
        println!(
            "\nSummary: {broken_count} of {total} reverse \
             dependencies would break."
        );
    } else {
        println!("\nSummary: all {total} reverse dependencies OK.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_nvr ---

    #[test]
    fn parse_nvr_standard() {
        assert_eq!(parse_nvr("foo-1.0-1.fc42"), Some("foo"));
    }

    #[test]
    fn parse_nvr_hyphenated_name() {
        assert_eq!(parse_nvr("rust-uucore-0.0.28-2.el9"), Some("rust-uucore"));
    }

    #[test]
    fn parse_nvr_many_hyphens() {
        assert_eq!(parse_nvr("python-a-b-c-1.2.3-4.fc42"), Some("python-a-b-c"));
    }

    #[test]
    fn parse_nvr_too_short() {
        assert_eq!(parse_nvr("foo-1.0"), None);
    }

    #[test]
    fn parse_nvr_empty() {
        assert_eq!(parse_nvr(""), None);
    }

    #[test]
    fn parse_nvr_no_hyphens() {
        assert_eq!(parse_nvr("foobar"), None);
    }

    // --- provide_name ---

    #[test]
    fn provide_name_versioned() {
        assert_eq!(provide_name("tor = 0.4.9.5-1.el9"), "tor");
    }

    #[test]
    fn provide_name_with_parens() {
        assert_eq!(provide_name("config(tor) = 0.4.9.5-1.el9"), "config(tor)");
    }

    #[test]
    fn provide_name_unversioned() {
        assert_eq!(
            provide_name("libthing.so.1()(64bit)"),
            "libthing.so.1()(64bit)"
        );
    }

    #[test]
    fn provide_name_ge() {
        assert_eq!(
            provide_name("crate(foo/default) >= 1.0"),
            "crate(foo/default)"
        );
    }

    // --- compare_provides ---

    #[test]
    fn compare_provides_updated() {
        let old: BTreeSet<String> = ["tor = 0.4.9.5-1.el9".to_string()].into();
        let new: BTreeSet<String> = ["tor = 0.4.9.6-1.el9".to_string()].into();
        let changed = compare_provides(&old, &new);
        assert_eq!(changed.len(), 1);
        assert!(changed[0].is_updated());
        assert_eq!(changed[0].new.as_deref(), Some("tor = 0.4.9.6-1.el9"));
    }

    #[test]
    fn compare_provides_removed() {
        let old: BTreeSet<String> = ["libfoo.so.1()(64bit)".to_string()].into();
        let new: BTreeSet<String> = BTreeSet::new();
        let changed = compare_provides(&old, &new);
        assert_eq!(changed.len(), 1);
        assert!(!changed[0].is_updated());
        assert!(changed[0].new.is_none());
    }

    #[test]
    fn compare_provides_unchanged() {
        let old: BTreeSet<String> = ["bash".to_string()].into();
        let new: BTreeSet<String> = ["bash".to_string()].into();
        let changed = compare_provides(&old, &new);
        assert!(changed.is_empty());
    }

    // --- testing_branch_from_side_tag ---

    #[test]
    fn testing_branch_epel9() {
        assert_eq!(
            testing_branch_from_side_tag(Some("epel9-build-side-133287")),
            Some("epel9".to_string())
        );
    }

    #[test]
    fn testing_branch_epel10() {
        assert_eq!(
            testing_branch_from_side_tag(Some("epel10.0-build-side-12345")),
            Some("epel10.0".to_string())
        );
    }

    #[test]
    fn testing_branch_not_epel() {
        assert_eq!(
            testing_branch_from_side_tag(Some("f44-build-side-99999")),
            None
        );
    }

    #[test]
    fn testing_branch_none() {
        assert_eq!(testing_branch_from_side_tag(None), None);
    }

    // --- detect_input_type ---

    #[test]
    fn detect_side_tag() {
        match detect_input_type("epel9-build-side-133287") {
            InputKind::SideTag(tag) => {
                assert_eq!(tag, "epel9-build-side-133287");
            }
            _ => panic!("expected SideTag"),
        }
    }

    #[test]
    fn detect_bodhi_alias() {
        match detect_input_type("FEDORA-EPEL-2026-f9eaa11e18") {
            InputKind::BodhiAlias(alias) => {
                assert_eq!(alias, "FEDORA-EPEL-2026-f9eaa11e18");
            }
            _ => panic!("expected BodhiAlias"),
        }
    }

    #[test]
    fn detect_bodhi_alias_fedora() {
        match detect_input_type("FEDORA-2026-abc123") {
            InputKind::BodhiAlias(alias) => {
                assert_eq!(alias, "FEDORA-2026-abc123");
            }
            _ => panic!("expected BodhiAlias"),
        }
    }

    #[test]
    fn detect_bodhi_url() {
        let url = "https://bodhi.fedoraproject.org/updates/FEDORA-EPEL-2026-f9eaa11e18";
        match detect_input_type(url) {
            InputKind::BodhiAlias(alias) => {
                assert_eq!(alias, "FEDORA-EPEL-2026-f9eaa11e18");
            }
            _ => panic!("expected BodhiAlias"),
        }
    }

    #[test]
    fn detect_bodhi_url_http() {
        let url = "http://bodhi.fedoraproject.org/updates/FEDORA-2026-xyz";
        match detect_input_type(url) {
            InputKind::BodhiAlias(alias) => {
                assert_eq!(alias, "FEDORA-2026-xyz");
            }
            _ => panic!("expected BodhiAlias"),
        }
    }
}
