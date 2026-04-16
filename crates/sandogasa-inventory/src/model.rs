// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Data model for package-of-interest inventories.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Top-level inventory document.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Inventory {
    /// Inventory metadata.
    pub inventory: InventoryMeta,
    /// Packages in the inventory.
    #[serde(default)]
    pub package: Vec<Package>,
}

/// Inventory metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InventoryMeta {
    /// Inventory name (e.g. "hyperscale-packages").
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Maintainer (person or team).
    pub maintainer: String,
    /// Labels/tags for the inventory (e.g. "eln-extras").
    #[serde(default)]
    pub labels: Vec<String>,
    /// Workload definitions. Keys are workload identifiers; values
    /// carry per-workload metadata for content-resolver export.
    /// Packages without explicit workloads inherit all keys.
    #[serde(default)]
    pub workloads: BTreeMap<String, WorkloadMeta>,
    /// Field names to strip from all packages on export.
    #[serde(default)]
    pub private_fields: Vec<String>,
}

/// Per-workload metadata for content-resolver export.
///
/// All fields except `packages` are optional — omitted fields
/// fall back to the inventory-level values.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorkloadMeta {
    /// Workload name in content-resolver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Maintainer override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintainer: Option<String>,
    /// Content-resolver labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// Source RPM names belonging to this workload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
}

/// A package of interest (source RPM).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Package {
    /// Source RPM name (required).
    pub name: String,

    // --- Metadata fields (may be private) ---
    /// Point of contact ("Name <email>").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poc: Option<String>,
    /// Reason for tracking this package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Team responsible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Internal task/ticket reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,

    // --- Content-resolver fields ---
    /// Binary RPM subpackages to track. If omitted, all are assumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpms: Option<Vec<String>>,
    /// Architecture-specific RPMs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch_rpms: Option<BTreeMap<String, Vec<String>>>,

    // --- hs-relmon fields ---
    /// Which branch/repository to track (upstream, fedora-rawhide, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    /// Name in Repology if different from RPM name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repology_name: Option<String>,
    /// Comma-separated distribution list for hs-relmon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distros: Option<String>,
    /// Whether to file GitLab issues for version updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_issue: Option<bool>,
}

impl Inventory {
    /// Check if a field name is private (should be stripped on export).
    pub fn is_private(&self, field: &str) -> bool {
        self.inventory.private_fields.iter().any(|f| f == field)
    }

    /// Get packages filtered by workload. Returns all if workload is None.
    ///
    /// When a workload key is given, returns only packages listed in
    /// that workload's `packages` field.
    pub fn packages_for_workload(&self, workload: Option<&str>) -> Vec<&Package> {
        match workload {
            None => self.package.iter().collect(),
            Some(w) => {
                let pkg_names: std::collections::HashSet<&str> = self
                    .inventory
                    .workloads
                    .get(w)
                    .map(|wl| wl.packages.iter().map(|s| s.as_str()).collect())
                    .unwrap_or_default();
                self.package
                    .iter()
                    .filter(|p| pkg_names.contains(p.name.as_str()))
                    .collect()
            }
        }
    }

    /// Return the sorted list of workload identifiers.
    pub fn workload_names(&self) -> Vec<&str> {
        self.inventory
            .workloads
            .keys()
            .map(|k| k.as_str())
            .collect()
    }

    /// Return the workload keys that contain a given package.
    pub fn workloads_for_package(&self, name: &str) -> Vec<&str> {
        self.inventory
            .workloads
            .iter()
            .filter(|(_, meta)| meta.packages.iter().any(|p| p == name))
            .map(|(k, _)| k.as_str())
            .collect()
    }

    /// Add a package to a workload, creating the workload if needed.
    pub fn add_to_workload(&mut self, workload: &str, package: &str) {
        let meta = self
            .inventory
            .workloads
            .entry(workload.to_string())
            .or_default();
        if !meta.packages.iter().any(|p| p == package) {
            meta.packages.push(package.to_string());
            meta.packages.sort();
        }
    }

    /// Find a package by name.
    pub fn find_package(&self, name: &str) -> Option<&Package> {
        self.package.iter().find(|p| p.name == name)
    }

    /// Find a package by name (mutable).
    pub fn find_package_mut(&mut self, name: &str) -> Option<&mut Package> {
        self.package.iter_mut().find(|p| p.name == name)
    }

    /// Add a package, replacing if one with the same name exists.
    pub fn add_package(&mut self, pkg: Package) {
        if let Some(existing) = self.find_package_mut(&pkg.name) {
            *existing = pkg;
        } else {
            self.package.push(pkg);
            self.package.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    /// Remove a package by name. Returns true if found and removed.
    pub fn remove_package(&mut self, name: &str) -> bool {
        let len = self.package.len();
        self.package.retain(|p| p.name != name);
        self.package.len() < len
    }

    /// Merge another inventory into this one.
    ///
    /// Packages from `other` are added (or replace existing ones with
    /// the same name). Metadata (name, description, etc.) is kept
    /// from the original.
    pub fn merge(&mut self, other: &Inventory) {
        for pkg in &other.package {
            self.add_package(pkg.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pkg(name: &str) -> Package {
        Package {
            name: name.to_string(),
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
        }
    }

    fn make_inventory() -> Inventory {
        let mut inv = Inventory {
            inventory: InventoryMeta {
                name: "test".to_string(),
                description: "test".to_string(),
                maintainer: "tester".to_string(),
                labels: vec![],
                workloads: BTreeMap::from([
                    (
                        "hyperscale".to_string(),
                        WorkloadMeta {
                            packages: vec!["bar".to_string(), "foo".to_string()],
                            ..Default::default()
                        },
                    ),
                    (
                        "epel".to_string(),
                        WorkloadMeta {
                            packages: vec!["foo".to_string()],
                            ..Default::default()
                        },
                    ),
                ]),
                private_fields: vec!["poc".to_string(), "team".to_string()],
            },
            package: vec![],
        };
        let mut bar = make_pkg("bar");
        bar.poc = Some("Team <t@e.com>".to_string());
        bar.team = Some("infra".to_string());
        inv.package.push(bar);

        let mut foo = make_pkg("foo");
        foo.rpms = Some(vec!["foo".to_string(), "foo-libs".to_string()]);
        foo.track = Some("upstream".to_string());
        inv.package.push(foo);

        inv
    }

    #[test]
    fn is_private() {
        let inv = make_inventory();
        assert!(inv.is_private("poc"));
        assert!(inv.is_private("team"));
        assert!(!inv.is_private("reason"));
        assert!(!inv.is_private("name"));
    }

    #[test]
    fn packages_for_workload() {
        let inv = make_inventory();
        let hs = inv.packages_for_workload(Some("hyperscale"));
        assert_eq!(hs.len(), 2);
        let epel = inv.packages_for_workload(Some("epel"));
        assert_eq!(epel.len(), 1);
        assert_eq!(epel[0].name, "foo");
        let all = inv.packages_for_workload(None);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn unlisted_package_not_in_workload() {
        let mut inv = make_inventory();
        inv.add_package(make_pkg("unlisted"));
        // Not listed in any workload's packages.
        let hs = inv.packages_for_workload(Some("hyperscale"));
        assert!(!hs.iter().any(|p| p.name == "unlisted"));
        // But shows up in unfiltered list.
        let all = inv.packages_for_workload(None);
        assert!(all.iter().any(|p| p.name == "unlisted"));
    }

    #[test]
    fn workload_names() {
        let inv = make_inventory();
        assert_eq!(inv.workload_names(), vec!["epel", "hyperscale"]);
    }

    #[test]
    fn workloads_for_package() {
        let inv = make_inventory();
        assert_eq!(inv.workloads_for_package("bar"), vec!["hyperscale"]);
        assert_eq!(inv.workloads_for_package("foo"), vec!["epel", "hyperscale"]);
        assert!(inv.workloads_for_package("nonexistent").is_empty());
    }

    #[test]
    fn add_to_workload() {
        let mut inv = make_inventory();
        inv.add_to_workload("hyperscale", "newpkg");
        assert!(
            inv.inventory.workloads["hyperscale"]
                .packages
                .contains(&"newpkg".to_string())
        );
        // Duplicate add is a no-op.
        inv.add_to_workload("hyperscale", "newpkg");
        assert_eq!(
            inv.inventory.workloads["hyperscale"]
                .packages
                .iter()
                .filter(|p| *p == "newpkg")
                .count(),
            1
        );
    }

    #[test]
    fn add_to_workload_creates_workload() {
        let mut inv = make_inventory();
        inv.add_to_workload("newwl", "pkg");
        assert!(inv.inventory.workloads.contains_key("newwl"));
        assert_eq!(inv.inventory.workloads["newwl"].packages, vec!["pkg"]);
    }

    #[test]
    fn find_package() {
        let inv = make_inventory();
        assert!(inv.find_package("foo").is_some());
        assert!(inv.find_package("nonexistent").is_none());
    }

    #[test]
    fn add_package_new() {
        let mut inv = make_inventory();
        inv.add_package(make_pkg("aaa"));
        assert_eq!(inv.package.len(), 3);
        // Should be sorted: aaa, bar, foo.
        assert_eq!(inv.package[0].name, "aaa");
    }

    #[test]
    fn add_package_replace() {
        let mut inv = make_inventory();
        let mut pkg = inv.find_package("foo").unwrap().clone();
        pkg.reason = Some("updated reason".to_string());
        inv.add_package(pkg);
        assert_eq!(inv.package.len(), 2);
        assert_eq!(
            inv.find_package("foo").unwrap().reason.as_deref(),
            Some("updated reason")
        );
    }

    #[test]
    fn remove_package() {
        let mut inv = make_inventory();
        assert!(inv.remove_package("foo"));
        assert_eq!(inv.package.len(), 1);
        assert!(!inv.remove_package("nonexistent"));
    }

    #[test]
    fn merge_inventories() {
        let mut inv1 = make_inventory();
        let inv2 = Inventory {
            inventory: InventoryMeta {
                name: "other".to_string(),
                description: "other".to_string(),
                maintainer: "other".to_string(),
                labels: vec![],
                workloads: BTreeMap::new(),
                private_fields: vec![],
            },
            package: vec![make_pkg("new-pkg")],
        };
        inv1.merge(&inv2);
        assert_eq!(inv1.package.len(), 3);
        assert!(inv1.find_package("new-pkg").is_some());
        // Metadata stays from inv1.
        assert_eq!(inv1.inventory.name, "test");
    }
}
