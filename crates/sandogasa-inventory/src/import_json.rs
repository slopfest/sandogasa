// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Import from legacy poi-tracker JSON format.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::model::{Inventory, InventoryMeta, Package};

/// Legacy JSON inventory (poi-tracker Python format).
#[derive(Debug, Deserialize)]
struct JsonInventory {
    name: String,
    description: String,
    maintainer: String,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    srpm_packages: Vec<JsonSrpmPackage>,
}

/// Legacy SRPM package entry.
#[derive(Debug, Deserialize)]
struct JsonSrpmPackage {
    name: String,
    #[serde(default)]
    poc: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    rpm_packages: Vec<JsonRpmPackage>,
}

/// Legacy binary RPM entry.
#[derive(Debug, Deserialize)]
struct JsonRpmPackage {
    name: String,
    #[serde(default)]
    arches: Option<Vec<String>>,
}

/// Import a legacy JSON inventory and convert to the TOML model.
pub fn import(json_str: &str) -> Result<Inventory, String> {
    let json: JsonInventory =
        serde_json::from_str(json_str).map_err(|e| format!("failed to parse JSON: {e}"))?;

    let mut packages = Vec::new();

    for srpm in &json.srpm_packages {
        let mut rpms: Vec<String> = Vec::new();
        let mut arch_rpms: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for rpm in &srpm.rpm_packages {
            if let Some(ref arches) = rpm.arches {
                // Architecture-specific RPM.
                for arch in arches {
                    arch_rpms
                        .entry(arch.clone())
                        .or_default()
                        .push(rpm.name.clone());
                }
            } else {
                rpms.push(rpm.name.clone());
            }
        }

        packages.push(Package {
            name: srpm.name.clone(),
            poc: srpm.poc.clone(),
            reason: srpm.reason.clone(),
            team: None,
            task: None,
            rpms: if rpms.is_empty() { None } else { Some(rpms) },
            arch_rpms: if arch_rpms.is_empty() {
                None
            } else {
                Some(arch_rpms)
            },

            track: None,
            repology_name: None,
            distros: None,
            file_issue: None,
        });
    }

    packages.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Inventory {
        inventory: InventoryMeta {
            name: json.name,
            description: json.description,
            maintainer: json.maintainer,
            labels: json.labels,
            workloads: BTreeMap::new(),
            private_fields: vec![],
        },
        package: packages,
    })
}

/// Import from a JSON file.
pub fn import_file(path: &str) -> Result<Inventory, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    import(&content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_basic() {
        let json = r#"{
            "name": "test",
            "description": "test inventory",
            "maintainer": "tester",
            "labels": ["eln-extras"],
            "srpm_packages": [
                {
                    "name": "foo",
                    "poc": "Team <team@example.com>",
                    "rpm_packages": [{"name": "foo"}]
                },
                {
                    "name": "bar",
                    "reason": "Needed for baz",
                    "rpm_packages": [
                        {"name": "bar"},
                        {"name": "bar-libs"}
                    ]
                }
            ]
        }"#;
        let inv = import(json).unwrap();
        assert_eq!(inv.inventory.name, "test");
        assert_eq!(inv.inventory.labels, vec!["eln-extras"]);
        assert_eq!(inv.package.len(), 2);
        // Sorted alphabetically.
        assert_eq!(inv.package[0].name, "bar");
        assert_eq!(inv.package[1].name, "foo");
        assert_eq!(
            inv.package[1].poc.as_deref(),
            Some("Team <team@example.com>")
        );
        assert_eq!(
            inv.package[0].rpms.as_deref(),
            Some(&["bar".to_string(), "bar-libs".to_string()][..])
        );
    }

    #[test]
    fn import_arch_specific() {
        let json = r#"{
            "name": "test",
            "description": "test",
            "maintainer": "tester",
            "srpm_packages": [
                {
                    "name": "sedutil",
                    "rpm_packages": [
                        {
                            "name": "sedutil",
                            "arches": ["x86_64", "aarch64"]
                        }
                    ]
                }
            ]
        }"#;
        let inv = import(json).unwrap();
        let pkg = &inv.package[0];
        assert!(pkg.rpms.is_none());
        let arch = pkg.arch_rpms.as_ref().unwrap();
        assert!(arch["x86_64"].contains(&"sedutil".to_string()));
        assert!(arch["aarch64"].contains(&"sedutil".to_string()));
    }

    #[test]
    fn import_mixed_arch_and_noarch() {
        let json = r#"{
            "name": "test",
            "description": "test",
            "maintainer": "tester",
            "srpm_packages": [
                {
                    "name": "systemd",
                    "rpm_packages": [
                        {"name": "systemd-networkd"},
                        {
                            "name": "systemd-boot-unsigned",
                            "arches": ["x86_64", "aarch64"]
                        }
                    ]
                }
            ]
        }"#;
        let inv = import(json).unwrap();
        let pkg = &inv.package[0];
        assert_eq!(
            pkg.rpms.as_deref(),
            Some(&["systemd-networkd".to_string()][..])
        );
        let arch = pkg.arch_rpms.as_ref().unwrap();
        assert_eq!(arch["x86_64"], vec!["systemd-boot-unsigned"]);
    }

    #[test]
    fn import_empty() {
        let json = r#"{
            "name": "empty",
            "description": "empty",
            "maintainer": "nobody",
            "srpm_packages": []
        }"#;
        let inv = import(json).unwrap();
        assert!(inv.package.is_empty());
    }
}
