// SPDX-License-Identifier: MPL-2.0

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
    out.push_str("# SPDX-License-Identifier: MPL-2.0\n\n");
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
