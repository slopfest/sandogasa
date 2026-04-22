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

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

/// Build the CBS Koji tag for a CentOS release. Accepts "c10s",
/// "c9s", etc. The tag format is
/// `proposed_updates<N>s-packages-main-release` where N is the
/// major version digit.
pub fn proposed_updates_tag(release: &str) -> Result<String, String> {
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
    Ok(format!("proposed_updates{digits}s-packages-main-release"))
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
            "  {release}: {} package(s), {added} new to inventory",
            package_names.len(),
        );
    }

    if let Err(e) = sandogasa_inventory::save(&inventory, &args.output) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    eprintln!(
        "Wrote {}: {} package(s) total, {total_added} new across {} release(s)",
        args.output,
        inventory.package.len(),
        tags.len(),
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

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
