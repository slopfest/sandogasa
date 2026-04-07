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

/// A single broken Requires entry.
#[derive(Debug, Clone, Serialize)]
pub struct BrokenRequires {
    /// The Requires string that would break.
    pub dep: String,
    /// The Provide that was removed.
    pub removed_provide: String,
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
    pub removed_provides: Vec<String>,
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
            removed_provides: vec![],
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
    let side_tag_fedrq = side_tag.as_ref().map(|tag| sandogasa_fedrq::Fedrq {
        branch: Some(branch.clone()),
        repo: Some(format!("@koji:{tag}")),
    });

    if side_tag_fedrq.is_some() {
        // Full analysis: compare old vs new provides.
        if opts.verbose {
            eprintln!("[check-update] comparing old vs new provides");
        }

        let removed_provides: BTreeSet<String> = updated_packages
            .par_iter()
            .flat_map(|srpm| {
                let old_provides: BTreeSet<String> =
                    filter_none(stable_fedrq.subpkgs_provides(srpm).unwrap_or_default())
                        .into_iter()
                        .collect();

                let new_provides: BTreeSet<String> = side_tag_fedrq
                    .as_ref()
                    .and_then(|fq| fq.subpkgs_provides(srpm).ok())
                    .map(|v| filter_none(v).into_iter().collect())
                    .unwrap_or_default();

                old_provides
                    .difference(&new_provides)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .collect();

        if opts.verbose {
            eprintln!(
                "[check-update] {} removed provides found",
                removed_provides.len()
            );
        }

        if removed_provides.is_empty() {
            return Ok(CheckUpdateReport {
                input: input.to_string(),
                branch,
                updated_packages,
                full_analysis: true,
                removed_provides: vec![],
                reverse_deps: BTreeMap::new(),
            });
        }

        let removed_list: Vec<String> = removed_provides.iter().cloned().collect();

        // Find reverse deps that require any removed provide.
        if opts.verbose {
            eprintln!("[check-update] finding reverse dependencies");
        }

        let rev_dep_sources = filter_none(
            stable_fedrq
                .whatrequires(&removed_list)
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

        // For each reverse dep, find which of its Requires
        // match removed provides.
        let results: Vec<(String, RevDepResult)> = rev_deps
            .par_iter()
            .map(|pkg| {
                let requires = stable_fedrq.subpkgs_requires(pkg).unwrap_or_default();

                let issues: Vec<BrokenRequires> = requires
                    .iter()
                    .filter_map(|dep| {
                        let dep_str = dep.trim();
                        if removed_provides.contains(dep_str) {
                            Some(BrokenRequires {
                                dep: dep_str.to_string(),
                                removed_provide: dep_str.to_string(),
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
            branch,
            updated_packages,
            full_analysis: true,
            removed_provides: removed_list,
            reverse_deps,
        })
    } else {
        // No side tag: use @testing repo for new provides if
        // the update is in testing, otherwise just list reverse deps.
        let testing_branch = opts.testing_branch.clone().or_else(|| Some(branch.clone()));
        let testing_fedrq = testing_branch.map(|tb| sandogasa_fedrq::Fedrq {
            branch: Some(tb),
            repo: Some("@testing".to_string()),
        });

        // Probe whether the package exists in @testing.
        let has_testing = testing_fedrq.as_ref().is_some_and(|fq| {
            updated_packages
                .first()
                .and_then(|pkg| fq.subpkgs_names(pkg).ok())
                .map(filter_none)
                .is_some_and(|names| !names.is_empty())
        });

        if has_testing {
            // Full analysis using @testing as the new-provides source.
            if opts.verbose {
                eprintln!("[check-update] using @testing for new provides");
            }

            let testing_fq = testing_fedrq.as_ref().unwrap();

            let removed_provides: BTreeSet<String> = updated_packages
                .par_iter()
                .flat_map(|srpm| {
                    let old_provides: BTreeSet<String> =
                        filter_none(stable_fedrq.subpkgs_provides(srpm).unwrap_or_default())
                            .into_iter()
                            .collect();

                    let new_provides: BTreeSet<String> = testing_fq
                        .subpkgs_provides(srpm)
                        .ok()
                        .map(|v| filter_none(v).into_iter().collect())
                        .unwrap_or_default();

                    old_provides
                        .difference(&new_provides)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .collect();

            if opts.verbose {
                eprintln!(
                    "[check-update] {} removed provides found",
                    removed_provides.len()
                );
            }

            if removed_provides.is_empty() {
                return Ok(CheckUpdateReport {
                    input: input.to_string(),
                    branch,
                    updated_packages,
                    full_analysis: true,
                    removed_provides: vec![],
                    reverse_deps: BTreeMap::new(),
                });
            }

            let removed_list: Vec<String> = removed_provides.iter().cloned().collect();

            if opts.verbose {
                eprintln!("[check-update] finding reverse dependencies");
            }

            let rev_dep_sources = filter_none(
                stable_fedrq
                    .whatrequires(&removed_list)
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
                    "[check-update] {} reverse dependencies to \
                     check: {}",
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
                            if removed_provides.contains(dep_str) {
                                Some(BrokenRequires {
                                    dep: dep_str.to_string(),
                                    removed_provide: dep_str.to_string(),
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
                branch,
                updated_packages,
                full_analysis: true,
                removed_provides: removed_list,
                reverse_deps,
            })
        } else {
            // Package not in @testing; just list reverse deps.
            if opts.verbose {
                eprintln!(
                    "[check-update] package not found in @testing; \
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
                removed_provides: vec![],
                reverse_deps,
            })
        }
    }
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

    if report.removed_provides.is_empty() {
        println!("No removed Provides. No breakage expected.");
        return;
    }

    println!("Removed Provides ({}):", report.removed_provides.len());
    for p in &report.removed_provides {
        println!("  - {p}");
    }

    if report.reverse_deps.is_empty() {
        println!(
            "\nNo packages depend on the removed Provides. \
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
                println!("    - {} (removed: {})", issue.dep, issue.removed_provide);
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
