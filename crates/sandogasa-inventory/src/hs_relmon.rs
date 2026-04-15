// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Export to hs-relmon manifest format.

use crate::model::{Inventory, Package};

/// Default values for the hs-relmon manifest.
pub struct RelmonDefaults {
    pub distros: String,
    pub track: String,
    pub file_issue: bool,
}

impl Default for RelmonDefaults {
    fn default() -> Self {
        Self {
            distros: "upstream,fedora,centos,hyperscale".to_string(),
            track: "upstream".to_string(),
            file_issue: true,
        }
    }
}

/// Export an inventory to hs-relmon manifest TOML format.
///
/// Only includes packages that have a `track` field set.
/// Filters by domain if specified.
pub fn export(inventory: &Inventory, domain: Option<&str>, defaults: &RelmonDefaults) -> String {
    let packages = inventory.packages_for_domain(domain);
    let relmon_packages: Vec<&&Package> = packages.iter().filter(|p| p.track.is_some()).collect();

    let mut out = String::new();
    out.push_str("# SPDX-License-Identifier: Apache-2.0 OR MIT\n\n");
    out.push_str("[defaults]\n");
    out.push_str(&format!("distros = \"{}\"\n", defaults.distros));
    out.push_str(&format!("track = \"{}\"\n", defaults.track));
    out.push_str(&format!("file_issue = {}\n", defaults.file_issue));

    for pkg in &relmon_packages {
        out.push('\n');
        out.push_str("[[package]]\n");
        out.push_str(&format!("name = \"{}\"\n", pkg.name));

        // Only emit fields that differ from defaults.
        if let Some(ref track) = pkg.track
            && track != &defaults.track
        {
            out.push_str(&format!("track = \"{track}\"\n"));
        }
        if let Some(ref repology_name) = pkg.repology_name {
            out.push_str(&format!("repology_name = \"{repology_name}\"\n"));
        }
        if let Some(ref distros) = pkg.distros
            && distros != &defaults.distros
        {
            out.push_str(&format!("distros = \"{distros}\"\n"));
        }
        if let Some(file_issue) = pkg.file_issue
            && file_issue != defaults.file_issue
        {
            out.push_str(&format!("file_issue = {file_issue}\n"));
        }
    }

    out
}

/// Result of a manifest merge operation.
pub struct MergeResult {
    /// The merged TOML content.
    pub content: String,
    /// Number of packages added.
    pub added: usize,
    /// Number of packages pruned.
    pub pruned: usize,
    /// Package names in the manifest but not in the inventory.
    pub stale: Vec<String>,
    /// Total packages in the result.
    pub total: usize,
}

/// Merge inventory packages into an existing hs-relmon manifest file.
///
/// Existing entries are preserved (including fields like `issue_url`
/// that the inventory doesn't have). New packages from the inventory
/// are added with their relmon fields. When `prune` is true, entries
/// not in the inventory are removed. Entries are sorted by name.
pub fn merge_into_manifest(
    manifest_path: &str,
    inventory: &Inventory,
    domain: Option<&str>,
    defaults: &RelmonDefaults,
    prune: bool,
) -> Result<MergeResult, String> {
    use toml_edit::DocumentMut;

    let contents = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("failed to read {manifest_path}: {e}"))?;
    let mut doc: DocumentMut = contents
        .parse()
        .map_err(|e| format!("failed to parse {manifest_path}: {e}"))?;

    // Collect existing package names.
    let existing: std::collections::HashSet<String> = doc
        .get("package")
        .and_then(|i| i.as_array_of_tables())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("name").and_then(|v| v.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Build the set of inventory package names for stale detection.
    let packages = inventory.packages_for_domain(domain);
    let inv_names: std::collections::HashSet<&str> =
        packages.iter().map(|p| p.name.as_str()).collect();

    // Packages in the manifest but not in the inventory.
    let stale: Vec<String> = existing
        .iter()
        .filter(|n| !inv_names.contains(n.as_str()))
        .cloned()
        .collect();

    // New packages from the inventory not already in the manifest.
    let new_packages: Vec<&&Package> = packages
        .iter()
        .filter(|p| !existing.contains(&p.name))
        .collect();
    let added = new_packages.len();

    // Ensure the [[package]] array exists.
    if doc.get("package").is_none() {
        doc.insert(
            "package",
            toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }

    let arr = doc["package"]
        .as_array_of_tables_mut()
        .ok_or("'package' is not an array of tables")?;

    for pkg in &new_packages {
        let mut table = toml_edit::Table::new();
        table.insert("name", toml_edit::value(&pkg.name));

        if let Some(ref track) = pkg.track
            && track != &defaults.track
        {
            table.insert("track", toml_edit::value(track.as_str()));
        }
        if let Some(ref repology_name) = pkg.repology_name {
            table.insert("repology_name", toml_edit::value(repology_name.as_str()));
        }
        if let Some(ref distros) = pkg.distros
            && distros != &defaults.distros
        {
            table.insert("distros", toml_edit::value(distros.as_str()));
        }
        if let Some(file_issue) = pkg.file_issue
            && file_issue != defaults.file_issue
        {
            table.insert("file_issue", toml_edit::value(file_issue));
        }

        arr.push(table);
    }

    // Rebuild sorted, optionally pruning stale entries.
    let prune_set: std::collections::HashSet<&str> = if prune {
        stale.iter().map(|s| s.as_str()).collect()
    } else {
        std::collections::HashSet::new()
    };

    let mut entries: Vec<(String, toml_edit::Table)> = Vec::new();
    let arr = doc["package"].as_array_of_tables().unwrap();
    for table in arr.iter() {
        let name = table
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if prune_set.contains(name.as_str()) {
            continue;
        }
        let mut new_table = toml_edit::Table::new();
        for (key, item) in table.iter() {
            new_table.insert(key, item.clone());
        }
        entries.push((name, new_table));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let pruned = if prune { stale.len() } else { 0 };
    let total = entries.len();

    let mut new_arr = toml_edit::ArrayOfTables::new();
    for (_, table) in entries {
        new_arr.push(table);
    }
    doc.remove("package");
    doc.insert("package", toml_edit::Item::ArrayOfTables(new_arr));

    Ok(MergeResult {
        content: doc.to_string(),
        added,
        pruned,
        stale,
        total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InventoryMeta, Package};

    fn make_pkg(name: &str, track: Option<&str>) -> Package {
        Package {
            name: name.to_string(),
            poc: None,
            reason: None,
            team: None,
            task: None,
            rpms: None,
            arch_rpms: None,
            domains: Some(vec!["hyperscale".to_string()]),
            track: track.map(|s| s.to_string()),
            repology_name: None,
            distros: None,
            file_issue: None,
        }
    }

    fn make_inventory(packages: Vec<Package>) -> Inventory {
        Inventory {
            inventory: InventoryMeta {
                name: "test".to_string(),
                description: "test".to_string(),
                maintainer: "tester".to_string(),
                labels: vec![],
                domains: vec![],
                private_fields: vec![],
            },
            package: packages,
        }
    }

    #[test]
    fn export_only_tracked_packages() {
        let inv = make_inventory(vec![
            make_pkg("foo", Some("upstream")),
            make_pkg("bar", None),
            make_pkg("baz", Some("fedora-rawhide")),
        ]);
        let toml = export(&inv, None, &RelmonDefaults::default());
        assert!(toml.contains("name = \"foo\""));
        assert!(toml.contains("name = \"baz\""));
        assert!(!toml.contains("name = \"bar\""));
    }

    #[test]
    fn export_omits_default_values() {
        let inv = make_inventory(vec![make_pkg("foo", Some("upstream"))]);
        let defaults = RelmonDefaults::default();
        let toml = export(&inv, None, &defaults);
        // track = "upstream" is the default, should not be emitted.
        assert!(toml.contains("name = \"foo\""));
        let pkg_section = toml.split("[[package]]").nth(1).unwrap();
        assert!(!pkg_section.contains("track ="));
    }

    #[test]
    fn export_emits_non_default_values() {
        let mut pkg = make_pkg("dracut", Some("fedora-rawhide"));
        pkg.distros = Some("upstream,fedora,centos,hs9".to_string());
        pkg.file_issue = Some(false);
        let inv = make_inventory(vec![pkg]);
        let toml = export(&inv, None, &RelmonDefaults::default());
        assert!(toml.contains("track = \"fedora-rawhide\""));
        assert!(toml.contains("distros = \"upstream,fedora,centos,hs9\""));
        assert!(toml.contains("file_issue = false"));
    }

    #[test]
    fn export_emits_repology_name() {
        let mut pkg = make_pkg("perf", Some("upstream"));
        pkg.repology_name = Some("linux".to_string());
        let inv = make_inventory(vec![pkg]);
        let toml = export(&inv, None, &RelmonDefaults::default());
        assert!(toml.contains("repology_name = \"linux\""));
    }

    #[test]
    fn export_filters_by_domain() {
        let mut foo = make_pkg("foo", Some("upstream"));
        foo.domains = Some(vec!["hyperscale".to_string()]);
        let mut bar = make_pkg("bar", Some("upstream"));
        bar.domains = Some(vec!["epel".to_string()]);
        let inv = make_inventory(vec![foo, bar]);
        let toml = export(&inv, Some("hyperscale"), &RelmonDefaults::default());
        assert!(toml.contains("name = \"foo\""));
        assert!(!toml.contains("name = \"bar\""));
    }

    #[test]
    fn export_has_defaults_section() {
        let inv = make_inventory(vec![]);
        let toml = export(&inv, None, &RelmonDefaults::default());
        assert!(toml.contains("[defaults]"));
        assert!(toml.contains("distros = \"upstream,fedora,centos,hyperscale\""));
        assert!(toml.contains("track = \"upstream\""));
        assert!(toml.contains("file_issue = true"));
    }
}
