// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Export to content-resolver YAML (feedback-pipeline-workload format).

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Inventory, Package};

/// Export an inventory to content-resolver YAML format.
///
/// Filters by workload if specified. When a workload key is given,
/// per-workload metadata overrides (name, description, maintainer,
/// labels) are applied from `inventory.workloads[key]`, falling back
/// to inventory-level values.
///
/// Strips private fields. Packages are sorted alphabetically.
pub fn export(inventory: &Inventory, workload_key: Option<&str>) -> String {
    let packages = inventory.packages_for_workload(workload_key);
    let (all_rpms, arch_rpms) = collect_rpms(&packages);

    // Resolve metadata: per-workload overrides → inventory defaults.
    let meta = workload_key.and_then(|k| inventory.inventory.workloads.get(k));

    let name = meta
        .and_then(|m| m.name.as_deref())
        .map(|n| n.to_string())
        .unwrap_or_else(|| match workload_key {
            Some(k) => format!("{}-{k}", inventory.inventory.name),
            None => inventory.inventory.name.clone(),
        });

    let description = meta
        .and_then(|m| m.description.as_deref())
        .unwrap_or(&inventory.inventory.description);

    let maintainer = meta
        .and_then(|m| m.maintainer.as_deref())
        .unwrap_or(&inventory.inventory.maintainer);

    let labels = meta
        .and_then(|m| m.labels.as_ref())
        .unwrap_or(&inventory.inventory.labels);

    let mut out = String::new();
    out.push_str("document: feedback-pipeline-workload\n");
    out.push_str("version: 1\n");
    out.push_str("data:\n");
    out.push_str(&format!("  name: {name}\n"));
    out.push_str(&format!("  description: {description}\n"));
    out.push_str(&format!("  maintainer: {maintainer}\n"));

    // Packages: sorted, deduplicated binary RPM names.
    if !all_rpms.is_empty() {
        out.push_str("  packages:\n");
        for rpm in &all_rpms {
            out.push_str(&format!("    - {rpm}\n"));
        }
    }

    // Architecture-specific packages.
    if !arch_rpms.is_empty() {
        out.push_str("  arch_packages:\n");
        for (arch, rpms) in &arch_rpms {
            out.push_str(&format!("    {arch}:\n"));
            for rpm in rpms {
                out.push_str(&format!("      - {rpm}\n"));
            }
        }
    }

    // Labels.
    if !labels.is_empty() {
        out.push_str("  labels:\n");
        for label in labels {
            out.push_str(&format!("    - {label}\n"));
        }
    }

    out
}

/// Collect all binary RPMs and arch-specific RPMs from packages.
///
/// If a package has no `rpms` field, its SRPM name is used as the
/// binary RPM name (common for simple packages).
fn collect_rpms(packages: &[&Package]) -> (BTreeSet<String>, BTreeMap<String, BTreeSet<String>>) {
    let mut all_rpms = BTreeSet::new();
    let mut arch_rpms: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for pkg in packages {
        // Binary RPMs.
        if let Some(ref rpms) = pkg.rpms {
            for rpm in rpms {
                all_rpms.insert(rpm.clone());
            }
        } else {
            // Default: use SRPM name as binary RPM.
            all_rpms.insert(pkg.name.clone());
        }

        // Architecture-specific RPMs.
        if let Some(ref arch) = pkg.arch_rpms {
            for (a, rpms) in arch {
                for rpm in rpms {
                    arch_rpms.entry(a.clone()).or_default().insert(rpm.clone());
                }
            }
        }
    }

    // Remove arch-specific RPMs from the general list (they're
    // listed separately).
    for rpms in arch_rpms.values() {
        for rpm in rpms {
            all_rpms.remove(rpm);
        }
    }

    (all_rpms, arch_rpms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InventoryMeta, Package, WorkloadMeta};

    fn make_inventory() -> Inventory {
        Inventory {
            inventory: InventoryMeta {
                name: "test-packages".to_string(),
                description: "Test packages".to_string(),
                maintainer: "test-sig".to_string(),
                labels: vec!["eln-extras".to_string()],
                workloads: BTreeMap::from([
                    (
                        "hyperscale".to_string(),
                        WorkloadMeta {
                            packages: vec!["fish".to_string(), "systemd".to_string()],
                            ..Default::default()
                        },
                    ),
                    (
                        "epel".to_string(),
                        WorkloadMeta {
                            packages: vec!["neovim".to_string()],
                            ..Default::default()
                        },
                    ),
                ]),
                private_fields: vec!["poc".to_string()],
            },
            package: vec![
                Package {
                    name: "fish".to_string(),
                    poc: Some("Team <t@e.com>".to_string()),
                    reason: None,
                    team: None,
                    task: None,
                    rpms: Some(vec!["fish".to_string()]),
                    arch_rpms: None,
                    track: None,
                    repology_name: None,
                    distros: None,
                    file_issue: None,
                },
                Package {
                    name: "systemd".to_string(),
                    poc: Some("Infra <i@e.com>".to_string()),
                    reason: None,
                    team: None,
                    task: None,
                    rpms: Some(vec!["systemd-networkd".to_string()]),
                    arch_rpms: Some(BTreeMap::from([
                        (
                            "x86_64".to_string(),
                            vec!["systemd-boot-unsigned".to_string()],
                        ),
                        (
                            "aarch64".to_string(),
                            vec!["systemd-boot-unsigned".to_string()],
                        ),
                    ])),
                    track: None,
                    repology_name: None,
                    distros: None,
                    file_issue: None,
                },
                Package {
                    name: "neovim".to_string(),
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
                },
            ],
        }
    }

    #[test]
    fn export_all() {
        let inv = make_inventory();
        let yaml = export(&inv, None);
        assert!(yaml.contains("document: feedback-pipeline-workload"));
        assert!(yaml.contains("name: test-packages"));
        assert!(yaml.contains("    - fish"));
        assert!(yaml.contains("    - systemd-networkd"));
        assert!(yaml.contains("    - neovim"));
        assert!(yaml.contains("    x86_64:"));
        assert!(yaml.contains("      - systemd-boot-unsigned"));
        assert!(yaml.contains("    - eln-extras"));
        // Private fields should not appear.
        assert!(!yaml.contains("Team <t@e.com>"));
    }

    #[test]
    fn export_filtered_by_workload() {
        let inv = make_inventory();
        let yaml = export(&inv, Some("hyperscale"));
        assert!(yaml.contains("    - fish"));
        assert!(yaml.contains("    - systemd-networkd"));
        // neovim is epel-only, should not appear.
        assert!(!yaml.contains("neovim"));
        // Name auto-derived: test-packages-hyperscale.
        assert!(yaml.contains("name: test-packages-hyperscale"));
    }

    #[test]
    fn export_with_workload_meta() {
        let mut inv = make_inventory();
        let wl = inv.inventory.workloads.get_mut("hyperscale").unwrap();
        wl.name = Some("hs-packages".to_string());
        wl.description = Some("Hyperscale SIG".to_string());
        wl.labels = Some(vec!["hs-label".to_string()]);
        let yaml = export(&inv, Some("hyperscale"));
        assert!(yaml.contains("name: hs-packages"));
        assert!(yaml.contains("description: Hyperscale SIG"));
        // Maintainer falls back to inventory level.
        assert!(yaml.contains("maintainer: test-sig"));
        // Labels from workload meta, not inventory.
        assert!(yaml.contains("    - hs-label"));
        assert!(!yaml.contains("eln-extras"));
    }

    #[test]
    fn export_default_rpm_name() {
        let inv = make_inventory();
        let yaml = export(&inv, Some("epel"));
        // neovim has no rpms field, so SRPM name is used.
        assert!(yaml.contains("    - neovim"));
    }

    #[test]
    fn arch_rpms_not_in_general_list() {
        let inv = make_inventory();
        let yaml = export(&inv, Some("hyperscale"));
        // systemd-boot-unsigned is arch-specific, should not be
        // in the general packages list.
        let packages_section = yaml
            .split("  packages:\n")
            .nth(1)
            .unwrap()
            .split("  arch_packages:")
            .next()
            .unwrap();
        assert!(!packages_section.contains("systemd-boot-unsigned"));
    }
}
