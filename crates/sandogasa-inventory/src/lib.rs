// SPDX-License-Identifier: MPL-2.0

//! Package-of-interest inventory data model and I/O.
//!
//! Provides a TOML-based inventory format for tracking packages
//! across Fedora, EPEL, and CentOS SIGs. Supports exporting to
//! content-resolver YAML (feedback-pipeline-workload) and
//! hs-relmon manifest formats.

mod model;

pub use model::{Inventory, InventoryMeta, Package};

/// Load an inventory from a TOML file.
pub fn load(path: &str) -> Result<Inventory, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    parse(&content)
}

/// Parse an inventory from a TOML string.
pub fn parse(content: &str) -> Result<Inventory, String> {
    toml::from_str(content).map_err(|e| format!("failed to parse inventory: {e}"))
}

/// Save an inventory to a TOML file.
pub fn save(inventory: &Inventory, path: &str) -> Result<(), String> {
    let content =
        toml::to_string_pretty(inventory).map_err(|e| format!("TOML serialization failed: {e}"))?;
    std::fs::write(path, content).map_err(|e| format!("failed to write {path}: {e}"))
}

/// Serialize an inventory to a TOML string.
pub fn to_toml(inventory: &Inventory) -> Result<String, String> {
    toml::to_string_pretty(inventory).map_err(|e| format!("TOML serialization failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal() {
        let toml = r#"
[inventory]
name = "test"
description = "test inventory"
maintainer = "tester"

[[package]]
name = "foo"
"#;
        let inv = parse(toml).unwrap();
        assert_eq!(inv.inventory.name, "test");
        assert_eq!(inv.package.len(), 1);
        assert_eq!(inv.package[0].name, "foo");
    }

    #[test]
    fn parse_full() {
        let toml = r#"
[inventory]
name = "test"
description = "test inventory"
maintainer = "tester"
labels = ["eln-extras"]
private_fields = ["poc", "team"]

[[package]]
name = "systemd"
poc = "Team <team@example.com>"
reason = "Core init"
team = "userspace"
task = "T123"
rpms = ["systemd-networkd"]
domains = ["hyperscale"]
track = "upstream"
repology_name = "systemd"
distros = "upstream,fedora"
file_issue = true

[package.arch_rpms]
x86_64 = ["systemd-boot-unsigned"]
"#;
        let inv = parse(toml).unwrap();
        assert_eq!(inv.inventory.private_fields, vec!["poc", "team"]);
        let pkg = &inv.package[0];
        assert_eq!(pkg.name, "systemd");
        assert_eq!(pkg.poc.as_deref(), Some("Team <team@example.com>"));
        assert_eq!(
            pkg.rpms.as_deref(),
            Some(&["systemd-networkd".to_string()][..])
        );
        assert_eq!(
            pkg.domains.as_deref(),
            Some(&["hyperscale".to_string()][..])
        );
        assert_eq!(pkg.track.as_deref(), Some("upstream"));
        assert!(pkg.file_issue.unwrap());
        let arch = pkg.arch_rpms.as_ref().unwrap();
        assert_eq!(arch["x86_64"], vec!["systemd-boot-unsigned"]);
    }

    #[test]
    fn round_trip() {
        let toml_in = r#"
[inventory]
name = "test"
description = "desc"
maintainer = "me"

[[package]]
name = "foo"
rpms = ["foo", "foo-libs"]
domains = ["hyperscale"]
"#;
        let inv = parse(toml_in).unwrap();
        let toml_out = to_toml(&inv).unwrap();
        let inv2 = parse(&toml_out).unwrap();
        assert_eq!(inv.inventory.name, inv2.inventory.name);
        assert_eq!(inv.package.len(), inv2.package.len());
        assert_eq!(inv.package[0].name, inv2.package[0].name);
    }
}
