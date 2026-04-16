// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Package-of-interest inventory data model and I/O.
//!
//! Provides a TOML-based inventory format for tracking packages
//! across Fedora, EPEL, and CentOS SIGs. Supports exporting to
//! content-resolver YAML (feedback-pipeline-workload) and
//! hs-relmon manifest formats.

pub mod content_resolver;
pub mod hs_relmon;
pub mod import_json;
mod model;

pub use model::{Inventory, InventoryMeta, Package, WorkloadMeta};

/// Generate a JSON Schema for the inventory format.
pub fn json_schema() -> String {
    let schema = schemars::schema_for!(Inventory);
    serde_json::to_string_pretty(&schema).expect("schema serialization failed")
}

/// Load multiple inventories and merge them into one.
///
/// The first inventory provides the metadata (name, description,
/// etc.). Packages from subsequent inventories are merged in.
pub fn load_and_merge(paths: &[String]) -> Result<Inventory, String> {
    let mut iter = paths.iter();
    let first = iter
        .next()
        .ok_or("at least one inventory file is required")?;
    let mut inventory = load(first)?;
    for path in iter {
        let other = load(path)?;
        inventory.merge(&other);
    }
    Ok(inventory)
}

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
"#;
        let inv = parse(toml_in).unwrap();
        let toml_out = to_toml(&inv).unwrap();
        let inv2 = parse(&toml_out).unwrap();
        assert_eq!(inv.inventory.name, inv2.inventory.name);
        assert_eq!(inv.package.len(), inv2.package.len());
        assert_eq!(inv.package[0].name, inv2.package[0].name);
    }

    #[test]
    fn load_nonexistent_errors() {
        assert!(load("/tmp/nonexistent-sandogasa-inv-test.toml").is_err());
    }

    #[test]
    fn save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.toml");
        let inv = parse(
            r#"
[inventory]
name = "roundtrip"
description = "d"
maintainer = "m"

[[package]]
name = "pkg1"
"#,
        )
        .unwrap();
        save(&inv, path.to_str().unwrap()).unwrap();
        let loaded = load(path.to_str().unwrap()).unwrap();
        assert_eq!(loaded.inventory.name, "roundtrip");
        assert_eq!(loaded.package.len(), 1);
    }

    #[test]
    fn load_and_merge_multiple() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("inv1.toml");
        let p2 = dir.path().join("inv2.toml");

        std::fs::write(
            &p1,
            r#"
[inventory]
name = "first"
description = "d"
maintainer = "m"

[[package]]
name = "aaa"

[[package]]
name = "bbb"
"#,
        )
        .unwrap();

        std::fs::write(
            &p2,
            r#"
[inventory]
name = "second"
description = "d2"
maintainer = "m2"

[[package]]
name = "ccc"

[[package]]
name = "bbb"
reason = "updated"
"#,
        )
        .unwrap();

        let paths = vec![
            p1.to_str().unwrap().to_string(),
            p2.to_str().unwrap().to_string(),
        ];
        let merged = load_and_merge(&paths).unwrap();

        // Metadata from first file.
        assert_eq!(merged.inventory.name, "first");
        // 3 packages: aaa, bbb (replaced), ccc.
        assert_eq!(merged.package.len(), 3);
        // bbb should have the updated reason from second file.
        let bbb = merged.find_package("bbb").unwrap();
        assert_eq!(bbb.reason.as_deref(), Some("updated"));
    }

    #[test]
    fn parse_invalid_errors() {
        assert!(parse("this is not valid toml [[[").is_err());
    }

    #[test]
    fn parse_with_workloads() {
        let toml = r#"
[inventory]
name = "test"
description = "d"
maintainer = "m"

[inventory.workloads.hyperscale]
name = "hs-packages"

[[package]]
name = "foo"
"#;
        let inv = parse(toml).unwrap();
        assert!(inv.inventory.workloads.contains_key("hyperscale"));
        assert_eq!(
            inv.inventory.workloads["hyperscale"].name.as_deref(),
            Some("hs-packages")
        );
    }

    /// Verify the checked-in JSON Schema matches the current model.
    ///
    /// If the schema has changed (e.g. fields added/removed), this
    /// test fails. To update the checked-in file:
    ///
    /// ```sh
    /// UPDATE_SCHEMA=1 cargo test -p sandogasa-inventory schema_up_to_date
    /// ```
    ///
    /// Review the diff before committing — new required fields are a
    /// breaking change, new optional fields are a minor change.
    #[test]
    fn schema_up_to_date() {
        let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("inventory.schema.json");
        let generated = json_schema();

        if std::env::var("UPDATE_SCHEMA").is_ok() {
            std::fs::write(&schema_path, &generated).expect("failed to write schema");
            eprintln!("Updated {}", schema_path.display());
            return;
        }

        let committed = std::fs::read_to_string(&schema_path).unwrap_or_else(|_| {
            panic!(
                "Schema file not found at {}. Run:\n  \
                 UPDATE_SCHEMA=1 cargo test -p sandogasa-inventory schema_up_to_date",
                schema_path.display()
            )
        });

        if generated != committed {
            // Show first differing line for quick diagnosis.
            for (i, (a, b)) in generated.lines().zip(committed.lines()).enumerate() {
                if a != b {
                    panic!(
                        "Schema is out of date (first difference at line {}). Run:\n  \
                         UPDATE_SCHEMA=1 cargo test -p sandogasa-inventory schema_up_to_date\n\n\
                         expected: {a}\n  actual: {b}",
                        i + 1
                    );
                }
            }
            // Length difference.
            panic!(
                "Schema is out of date (line count differs). Run:\n  \
                 UPDATE_SCHEMA=1 cargo test -p sandogasa-inventory schema_up_to_date"
            );
        }
    }
}
