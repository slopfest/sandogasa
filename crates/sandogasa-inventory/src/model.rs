// SPDX-License-Identifier: MPL-2.0

//! Data model for package-of-interest inventories.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Top-level inventory document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inventory {
    /// Inventory metadata.
    pub inventory: InventoryMeta,
    /// Packages in the inventory.
    #[serde(default)]
    pub package: Vec<Package>,
}

/// Inventory metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Default domain(s) for packages that don't specify their own.
    #[serde(default)]
    pub domains: Vec<String>,
    /// Field names to strip from all packages on export.
    #[serde(default)]
    pub private_fields: Vec<String>,
}

/// A package of interest (source RPM).
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    // --- Domain tags ---
    /// Which workloads/SIGs this package belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domains: Option<Vec<String>>,

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

    /// Get packages filtered by domain. Returns all if domain is None.
    ///
    /// Packages without explicit domains inherit the inventory-level
    /// default domains.
    pub fn packages_for_domain(&self, domain: Option<&str>) -> Vec<&Package> {
        match domain {
            None => self.package.iter().collect(),
            Some(d) => {
                let default_match = self.inventory.domains.iter().any(|dom| dom == d);
                self.package
                    .iter()
                    .filter(|p| match &p.domains {
                        Some(domains) => domains.iter().any(|dom| dom == d),
                        None => default_match,
                    })
                    .collect()
            }
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

    fn make_inventory() -> Inventory {
        Inventory {
            inventory: InventoryMeta {
                name: "test".to_string(),
                description: "test".to_string(),
                maintainer: "tester".to_string(),
                labels: vec![],
                domains: vec![],
                private_fields: vec!["poc".to_string(), "team".to_string()],
            },
            package: vec![
                Package {
                    name: "bar".to_string(),
                    poc: Some("Team <t@e.com>".to_string()),
                    reason: None,
                    team: Some("infra".to_string()),
                    task: None,
                    rpms: None,
                    arch_rpms: None,
                    domains: Some(vec!["hyperscale".to_string()]),
                    track: None,
                    repology_name: None,
                    distros: None,
                    file_issue: None,
                },
                Package {
                    name: "foo".to_string(),
                    poc: None,
                    reason: None,
                    team: None,
                    task: None,
                    rpms: Some(vec!["foo".to_string(), "foo-libs".to_string()]),
                    arch_rpms: None,
                    domains: Some(vec!["hyperscale".to_string(), "epel".to_string()]),
                    track: Some("upstream".to_string()),
                    repology_name: None,
                    distros: None,
                    file_issue: None,
                },
            ],
        }
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
    fn packages_for_domain() {
        let inv = make_inventory();
        let hs = inv.packages_for_domain(Some("hyperscale"));
        assert_eq!(hs.len(), 2);
        let epel = inv.packages_for_domain(Some("epel"));
        assert_eq!(epel.len(), 1);
        assert_eq!(epel[0].name, "foo");
        let all = inv.packages_for_domain(None);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn packages_for_domain_inherits_default() {
        let mut inv = make_inventory();
        inv.inventory.domains = vec!["hyperscale".to_string()];
        // Add a package with no explicit domains.
        inv.add_package(Package {
            name: "nodomain".to_string(),
            poc: None,
            reason: None,
            team: None,
            task: None,
            rpms: None,
            arch_rpms: None,
            domains: None,
            track: None,
            repology_name: None,
            distros: None,
            file_issue: None,
        });
        // Should inherit "hyperscale" from inventory default.
        let hs = inv.packages_for_domain(Some("hyperscale"));
        assert!(hs.iter().any(|p| p.name == "nodomain"));
        // But not "epel".
        let epel = inv.packages_for_domain(Some("epel"));
        assert!(!epel.iter().any(|p| p.name == "nodomain"));
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
        inv.add_package(Package {
            name: "aaa".to_string(),
            poc: None,
            reason: None,
            team: None,
            task: None,
            rpms: None,
            arch_rpms: None,
            domains: None,
            track: None,
            repology_name: None,
            distros: None,
            file_issue: None,
        });
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
                domains: vec![],
                private_fields: vec![],
            },
            package: vec![Package {
                name: "new-pkg".to_string(),
                poc: None,
                reason: None,
                team: None,
                task: None,
                rpms: None,
                arch_rpms: None,
                domains: None,
                track: None,
                repology_name: None,
                distros: None,
                file_issue: None,
            }],
        };
        inv1.merge(&inv2);
        assert_eq!(inv1.package.len(), 3);
        assert!(inv1.find_package("new-pkg").is_some());
        // Metadata stays from inv1.
        assert_eq!(inv1.inventory.name, "test");
    }
}
