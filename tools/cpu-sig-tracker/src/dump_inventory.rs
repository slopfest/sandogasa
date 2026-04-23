// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `dump-inventory` subcommand.
//!
//! Enumerates packages currently tagged into
//! `<release>-proposed_updates` in Koji and writes them to a
//! sandogasa-inventory TOML file. The CentOS release (e.g. `c10s`)
//! becomes a workload in the inventory, with all discovered packages
//! listed under it.
//!
//! Safe to re-run against an existing inventory: existing packages
//! and workloads are preserved; new discoveries merge in.

use std::process::ExitCode;

use sandogasa_inventory::{Inventory, InventoryMeta, Package, WorkloadMeta};
use sandogasa_koji::{list_tagged_nvrs, parse_nvr_name};

#[derive(clap::Args)]
pub struct DumpInventoryArgs {
    /// CentOS release(s) to enumerate, e.g. "c10s" or "c9s,c10s"
    /// (CSV or repeated). The Koji tag queried per release is
    /// "proposed_updates<N>s-packages-main-release" where N is the
    /// major version digit.
    #[arg(short, long = "release", value_delimiter = ',')]
    pub releases: Vec<String>,

    /// Output file (sandogasa-inventory TOML). If it exists,
    /// merge in the newly-discovered packages; existing entries
    /// are preserved.
    #[arg(short, long)]
    pub output: String,

    /// Koji CLI profile (cbs for CentOS Build System).
    #[arg(long, default_value = "cbs")]
    pub koji_profile: String,

    /// Drop packages from each workload that are no longer
    /// tagged in either the `-release` or `-testing` tag for
    /// that release. Orphan `[[package]]` entries (metadata
    /// blocks for packages no longer referenced by any
    /// workload) are left in place so user-entered fields
    /// aren't lost.
    #[arg(long)]
    pub prune: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

/// Build the CBS Koji tag for a CentOS release. Accepts "c10s",
/// "c9s", etc. The tag format is
/// `proposed_updates<N>s-packages-main-release` where N is the
/// major version digit.
pub fn proposed_updates_tag(release: &str) -> Result<String, String> {
    proposed_updates_tag_with_suffix(release, "release")
}

/// Build the CBS `-testing` sibling tag
/// (`proposed_updates<N>s-packages-main-testing`). Useful for
/// packages that have been built but not yet promoted to the
/// `-release` tag.
pub fn proposed_updates_testing_tag(release: &str) -> Result<String, String> {
    proposed_updates_tag_with_suffix(release, "testing")
}

fn proposed_updates_tag_with_suffix(release: &str, suffix: &str) -> Result<String, String> {
    let digits = release
        .strip_prefix('c')
        .and_then(|s| s.strip_suffix('s'))
        .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
        .ok_or_else(|| {
            format!(
                "unrecognized release '{release}': expected 'c<N>s' \
                 (e.g. c9s, c10s)"
            )
        })?;
    Ok(format!("proposed_updates{digits}s-packages-main-{suffix}"))
}

pub fn run(args: &DumpInventoryArgs) -> ExitCode {
    if args.releases.is_empty() {
        eprintln!("error: at least one --release is required");
        return ExitCode::FAILURE;
    }

    // Deduplicate while preserving first-seen order.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let releases: Vec<&str> = args
        .releases
        .iter()
        .filter(|r| seen.insert(r.as_str()))
        .map(|s| s.as_str())
        .collect();

    // Pre-validate all release strings so we fail fast before any
    // Koji calls or inventory writes.
    let tags: Vec<(String, String)> = match releases
        .iter()
        .map(|r| proposed_updates_tag(r).map(|tag| (r.to_string(), tag)))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Load existing inventory or create a fresh one.
    let mut inventory = if std::path::Path::new(&args.output).exists() {
        match sandogasa_inventory::load(&args.output) {
            Ok(inv) => inv,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        Inventory {
            inventory: InventoryMeta {
                name: "cpu-sig".to_string(),
                description: "CentOS Proposed Updates SIG packages".to_string(),
                maintainer: "CentOS Proposed Updates SIG".to_string(),
                labels: vec![],
                workloads: Default::default(),
                private_fields: vec![],
            },
            package: vec![],
        }
    };

    let mut total_added = 0usize;
    let mut total_pruned = 0usize;
    for (release, tag) in &tags {
        if args.verbose {
            eprintln!("[cpu-sig-tracker] listing tagged NVRs in {tag}");
        }
        let nvrs = match list_tagged_nvrs(tag, Some(&args.koji_profile)) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("error: koji list-tagged {tag} failed: {e}");
                return ExitCode::FAILURE;
            }
        };

        let mut package_names: Vec<String> = nvrs
            .iter()
            .filter_map(|nvr| parse_nvr_name(nvr).map(|s| s.to_string()))
            .collect();
        package_names.sort();
        package_names.dedup();

        if args.verbose {
            eprintln!(
                "[cpu-sig-tracker] {} unique package(s) in {tag}",
                package_names.len()
            );
        }

        let mut added = 0usize;
        for name in &package_names {
            if inventory.find_package(name).is_none() {
                inventory.add_package(Package {
                    name: name.clone(),
                    poc: None,
                    reason: None,
                    team: None,
                    task: None,
                    rpms: None,
                    arch_rpms: None,
                    track: None,
                    repology_name: None,
                    distros: None,
                    file_issue: None,
                });
                added += 1;
            }
        }
        total_added += added;

        // Collect the "currently tagged" set for prune: union
        // of -release and -testing. Packages in -testing but
        // not -release are still in-flight and shouldn't be
        // dropped from the workload.
        let pruned = if args.prune {
            let testing_tag = match proposed_updates_testing_tag(release) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if args.verbose {
                eprintln!("[cpu-sig-tracker] listing tagged NVRs in {testing_tag}");
            }
            let testing_nvrs = match list_tagged_nvrs(&testing_tag, Some(&args.koji_profile)) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("error: koji list-tagged {testing_tag} failed: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let tagged: std::collections::HashSet<String> = nvrs
                .iter()
                .chain(testing_nvrs.iter())
                .filter_map(|nvr| parse_nvr_name(nvr).map(|s| s.to_string()))
                .collect();
            prune_workload(&mut inventory, release, &tagged)
        } else {
            0
        };
        total_pruned += pruned;

        let workload = inventory
            .inventory
            .workloads
            .entry(release.clone())
            .or_insert_with(WorkloadMeta::default);
        for name in &package_names {
            if !workload.packages.iter().any(|p| p == name) {
                workload.packages.push(name.clone());
            }
        }
        workload.packages.sort();

        eprintln!(
            "  {release}: {} package(s), {added} new to inventory{}",
            package_names.len(),
            if args.prune {
                format!(", {pruned} pruned")
            } else {
                String::new()
            },
        );
    }

    if let Err(e) = sandogasa_inventory::save(&inventory, &args.output) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    let prune_suffix = if args.prune {
        format!(", {total_pruned} pruned")
    } else {
        String::new()
    };
    eprintln!(
        "Wrote {}: {} package(s) total, {total_added} new across {} release(s){prune_suffix}",
        args.output,
        inventory.package.len(),
        tags.len(),
    );
    ExitCode::SUCCESS
}

/// Drop workload entries for `release` whose package names
/// aren't in the `tagged` set. Returns how many were removed.
/// Logs each removal to stderr so users can see what got
/// trimmed.
fn prune_workload(
    inventory: &mut Inventory,
    release: &str,
    tagged: &std::collections::HashSet<String>,
) -> usize {
    let Some(workload) = inventory.inventory.workloads.get_mut(release) else {
        return 0;
    };
    let before = workload.packages.len();
    workload.packages.retain(|p| {
        let keep = tagged.contains(p);
        if !keep {
            eprintln!("  pruning {release}: {p} no longer tagged");
        }
        keep
    });
    before - workload.packages.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_inv_with_workload(release: &str, packages: &[&str]) -> Inventory {
        let mut inv = Inventory {
            inventory: InventoryMeta {
                name: "t".into(),
                description: "t".into(),
                maintainer: "t".into(),
                labels: vec![],
                workloads: Default::default(),
                private_fields: vec![],
            },
            package: vec![],
        };
        let meta = WorkloadMeta {
            packages: packages.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        };
        inv.inventory.workloads.insert(release.to_string(), meta);
        inv
    }

    #[test]
    fn prune_workload_drops_untagged_entries() {
        let mut inv = build_inv_with_workload("c10s", &["xz", "mutter", "PackageKit"]);
        let tagged: std::collections::HashSet<String> = ["xz", "PackageKit"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let removed = prune_workload(&mut inv, "c10s", &tagged);
        assert_eq!(removed, 1);
        assert_eq!(
            inv.inventory.workloads["c10s"].packages,
            vec!["xz".to_string(), "PackageKit".to_string()],
        );
    }

    #[test]
    fn prune_workload_keeps_all_when_everything_tagged() {
        let mut inv = build_inv_with_workload("c10s", &["xz", "PackageKit"]);
        let tagged: std::collections::HashSet<String> = ["xz", "PackageKit", "extra"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert_eq!(prune_workload(&mut inv, "c10s", &tagged), 0);
    }

    #[test]
    fn prune_workload_unknown_release_is_noop() {
        let mut inv = build_inv_with_workload("c10s", &["xz"]);
        assert_eq!(prune_workload(&mut inv, "c9s", &Default::default()), 0);
    }

    #[test]
    fn tag_for_c10s() {
        assert_eq!(
            proposed_updates_tag("c10s").unwrap(),
            "proposed_updates10s-packages-main-release"
        );
    }

    #[test]
    fn tag_for_c9s() {
        assert_eq!(
            proposed_updates_tag("c9s").unwrap(),
            "proposed_updates9s-packages-main-release"
        );
    }

    #[test]
    fn tag_rejects_missing_prefix() {
        assert!(proposed_updates_tag("10s").is_err());
    }

    #[test]
    fn tag_rejects_missing_suffix() {
        assert!(proposed_updates_tag("c10").is_err());
    }

    #[test]
    fn tag_rejects_non_digit_body() {
        assert!(proposed_updates_tag("cxs").is_err());
    }

    #[test]
    fn tag_rejects_empty_body() {
        assert!(proposed_updates_tag("cs").is_err());
    }
}
