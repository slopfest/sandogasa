// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `deps` subcommand.
//!
//! The walk, the graph and the offline oracle live in
//! `sandogasa-closure`; this module re-exports them and adds the one
//! inventory-shaped piece: turning a walk's report into an inventory
//! whose `reason` fields record what pulled each package in, so the
//! collected set plugs straight back into the inventory tooling
//! (culling becomes a set difference).

use std::collections::BTreeMap;

pub use sandogasa_closure::*;

/// Turn a report into an inventory of the collected sources, ready
/// for `sandogasa_inventory::save`.
pub fn to_inventory(
    report: &DepsReport,
    name: &str,
    description: &str,
    maintainer: &str,
) -> sandogasa_inventory::Inventory {
    sandogasa_inventory::Inventory {
        inventory: sandogasa_inventory::InventoryMeta {
            name: name.to_string(),
            description: description.to_string(),
            maintainer: maintainer.to_string(),
            labels: Vec::new(),
            workloads: BTreeMap::new(),
            private_fields: Vec::new(),
        },
        package: report
            .collected
            .iter()
            .map(|c| sandogasa_inventory::Package {
                name: c.source.clone(),
                reason: Some(if c.via.is_empty() {
                    format!("runtime dependency ({})", c.repoid)
                } else {
                    format!(
                        "runtime dependency ({}): {} requires {}",
                        c.repoid, c.required_by, c.via
                    )
                }),
                ..Default::default()
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_carries_the_reason_chain() {
        let r = DepsReport {
            collected: vec![CollectedDep {
                source: "foo".into(),
                repoid: "epel".into(),
                required_by: "root-bin".into(),
                via: "libfoo.so.1()(64bit)".into(),
            }],
            ..Default::default()
        };
        let inv = to_inventory(&r, "work-epel9-deps", "test", "me");
        assert_eq!(inv.inventory.name, "work-epel9-deps");
        assert_eq!(inv.package[0].name, "foo");
        assert_eq!(
            inv.package[0].reason.as_deref(),
            Some("runtime dependency (epel): root-bin requires libfoo.so.1()(64bit)")
        );
    }
}
