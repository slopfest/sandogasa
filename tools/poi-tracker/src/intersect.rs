// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `intersect` subcommand.
//!
//! Keep only the packages of the main inventory (`-i`) that also
//! appear in the `--with` inventories, optionally merging the result
//! into another inventory file. The motivating case: a `deps --build`
//! closure lists everything a keep-set needs — thousands of packages,
//! most of them other people's — and the durable fact is its
//! intersection with the packages *you* are on the ACL for. That
//! subset, reason chains intact, is what belongs in a curated
//! essentials inventory; the closure itself is a regenerable
//! artifact.
//!
//! Entries come from the `-i` side, so whatever metadata it carries
//! (the `reason` chains a `deps` run writes, say) survives into the
//! merge target.

use std::collections::BTreeSet;

use sandogasa_inventory::{Inventory, Package};

/// The packages of `base` whose names appear in `filter`, in `base`
/// order, entries (and their metadata) taken from `base`.
pub fn intersect(base: Inventory, filter: &BTreeSet<String>) -> Vec<Package> {
    base.package
        .into_iter()
        .filter(|p| filter.contains(&p.name))
        .collect()
}

/// Merge `packages` into the inventory at `path`, creating the file
/// (with `meta` for its header) when absent. Existing entries are
/// left untouched; the result stays sorted. Returns how many were
/// added.
pub fn merge_packages(
    path: &str,
    packages: Vec<Package>,
    meta: &sandogasa_inventory::InventoryMeta,
) -> Result<usize, String> {
    let mut inventory = if std::path::Path::new(path).exists() {
        sandogasa_inventory::load(path)?
    } else {
        Inventory {
            inventory: meta.clone(),
            package: Vec::new(),
        }
    };
    let existing: BTreeSet<String> = inventory.package.iter().map(|p| p.name.clone()).collect();
    let mut added = 0;
    for pkg in packages {
        if !existing.contains(&pkg.name) {
            inventory.package.push(pkg);
            added += 1;
        }
    }
    inventory.package.sort_by(|a, b| a.name.cmp(&b.name));
    sandogasa_inventory::save(&inventory, path)?;
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inventory(entries: &[(&str, Option<&str>)]) -> Inventory {
        Inventory {
            inventory: sandogasa_inventory::InventoryMeta {
                name: "t".into(),
                description: "t".into(),
                maintainer: "t".into(),
                labels: Vec::new(),
                workloads: Default::default(),
                private_fields: Vec::new(),
            },
            package: entries
                .iter()
                .map(|(name, reason)| Package {
                    name: name.to_string(),
                    reason: reason.map(str::to_string),
                    ..Default::default()
                })
                .collect(),
        }
    }

    #[test]
    fn intersection_keeps_base_metadata() {
        let base = inventory(&[
            (
                "rust-clap",
                Some("runtime dependency (rawhide): src:ripgrep requires crate(clap)"),
            ),
            ("glibc", Some("everything needs it")),
        ]);
        let filter: BTreeSet<String> = ["rust-clap".to_string()].into();
        let out = intersect(base, &filter);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "rust-clap");
        assert!(out[0].reason.as_deref().unwrap().contains("src:ripgrep"));
    }

    #[test]
    fn merge_accumulates_and_leaves_existing_entries_alone() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("essentials.toml");
        let dest = dest.to_str().unwrap();
        let meta = inventory(&[]).inventory;

        let first = inventory(&[("rust-clap", Some("from build deps"))]).package;
        assert_eq!(merge_packages(dest, first, &meta).unwrap(), 1);

        // Second merge: overlap keeps the original entry, new one adds.
        let second = inventory(&[("rust-clap", Some("different")), ("rust-syn", None)]).package;
        assert_eq!(merge_packages(dest, second, &meta).unwrap(), 1);

        let merged = sandogasa_inventory::load(dest).unwrap();
        let names: Vec<&str> = merged.package.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["rust-clap", "rust-syn"]);
        assert_eq!(merged.package[0].reason.as_deref(), Some("from build deps"));
    }
}
