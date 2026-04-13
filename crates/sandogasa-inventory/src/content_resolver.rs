// SPDX-License-Identifier: MPL-2.0

//! Export to content-resolver YAML (feedback-pipeline-workload format).

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{Inventory, Package};

/// Export an inventory to content-resolver YAML format.
///
/// Filters by domain if specified. Strips private fields.
/// Packages are sorted alphabetically.
pub fn export(inventory: &Inventory, domain: Option<&str>) -> String {
    let packages = inventory.packages_for_domain(domain);
    let (all_rpms, arch_rpms) = collect_rpms(&packages);

    let mut out = String::new();
    out.push_str("document: feedback-pipeline-workload\n");
    out.push_str("version: 1\n");
    out.push_str("data:\n");
    out.push_str(&format!("  name: {}\n", inventory.inventory.name));
    out.push_str(&format!(
        "  description: {}\n",
        inventory.inventory.description
    ));
    out.push_str(&format!(
        "  maintainer: {}\n",
        inventory.inventory.maintainer
    ));

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
    if !inventory.inventory.labels.is_empty() {
        out.push_str("  labels:\n");
        for label in &inventory.inventory.labels {
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
    use crate::model::{InventoryMeta, Package};

    fn make_inventory() -> Inventory {
        Inventory {
            inventory: InventoryMeta {
                name: "test-packages".to_string(),
                description: "Test packages".to_string(),
                maintainer: "test-sig".to_string(),
                labels: vec!["eln-extras".to_string()],
                domains: vec![],
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
                    domains: Some(vec!["hyperscale".to_string()]),
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
                    domains: Some(vec!["hyperscale".to_string()]),
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
                    domains: Some(vec!["epel".to_string()]),
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
    fn export_filtered_by_domain() {
        let inv = make_inventory();
        let yaml = export(&inv, Some("hyperscale"));
        assert!(yaml.contains("    - fish"));
        assert!(yaml.contains("    - systemd-networkd"));
        // neovim is epel-only, should not appear.
        assert!(!yaml.contains("neovim"));
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
