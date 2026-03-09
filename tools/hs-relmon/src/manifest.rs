// SPDX-License-Identifier: MPL-2.0

use crate::check_latest::{Distros, TrackRef};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A manifest listing packages to check.
#[derive(Debug, Deserialize, Serialize)]
pub struct Manifest {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(rename = "package")]
    pub packages: Vec<PackageEntry>,
}

/// Default settings applied to all packages unless overridden.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Defaults {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distros: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repology_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_issue: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_url: Option<String>,
}

/// A single package entry in the manifest.
#[derive(Debug, Deserialize, Serialize)]
pub struct PackageEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distros: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repology_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_issue: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_url: Option<String>,
}

/// A package entry with all defaults resolved.
#[derive(Debug)]
pub struct ResolvedPackage {
    pub name: String,
    pub distros: Distros,
    pub track: TrackRef,
    pub repology_name: Option<String>,
    pub file_issue: bool,
    pub issue_url: Option<String>,
}

impl Manifest {
    /// Load a manifest from a TOML file.
    pub fn load(
        path: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&contents)?)
    }

    /// Add packages by name (skipping duplicates) and sort.
    pub fn add_packages(&mut self, names: &[String]) {
        let existing: std::collections::HashSet<String> = self
            .packages
            .iter()
            .map(|p| p.name.clone())
            .collect();
        for name in names {
            if !existing.contains(name) {
                self.packages.push(PackageEntry {
                    name: name.clone(),
                    distros: None,
                    track: None,
                    repology_name: None,
                    file_issue: None,
                    issue_url: None,
                });
            }
        }
        self.sort_packages();
    }

    /// Sort packages by name.
    pub fn sort_packages(&mut self) {
        self.packages.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Save the manifest to a TOML file.
    pub fn save(
        &self,
        path: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let toml_str = toml::to_string(self)?;
        let contents = format!(
            "# SPDX-License-Identifier: MPL-2.0\n\n{toml_str}"
        );
        std::fs::write(path, contents)?;
        Ok(())
    }

    /// Resolve all packages by merging per-package overrides
    /// with defaults.
    pub fn resolve(
        &self,
    ) -> Result<Vec<ResolvedPackage>, Box<dyn std::error::Error>> {
        self.packages
            .iter()
            .map(|pkg| self.resolve_one(pkg))
            .collect()
    }

    fn resolve_one(
        &self,
        pkg: &PackageEntry,
    ) -> Result<ResolvedPackage, Box<dyn std::error::Error>> {
        let distros_str = pkg
            .distros
            .as_ref()
            .or(self.defaults.distros.as_ref());
        let distros = match distros_str {
            Some(s) => Distros::parse(s).map_err(|e| {
                format!("{}: {e}", pkg.name)
            })?,
            None => Distros::all(),
        };

        let track_str = pkg
            .track
            .as_ref()
            .or(self.defaults.track.as_ref());
        let track = match track_str {
            Some(s) => TrackRef::parse(s).map_err(|e| {
                format!("{}: {e}", pkg.name)
            })?,
            None => TrackRef::Upstream,
        };

        let repology_name = pkg
            .repology_name
            .clone()
            .or_else(|| self.defaults.repology_name.clone());

        let file_issue = pkg
            .file_issue
            .or(self.defaults.file_issue)
            .unwrap_or(false);

        let issue_url = pkg
            .issue_url
            .clone()
            .or_else(|| self.defaults.issue_url.clone());

        Ok(ResolvedPackage {
            name: pkg.name.clone(),
            distros,
            track,
            repology_name,
            file_issue,
            issue_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_minimal() {
        let toml_str = r#"
[[package]]
name = "ethtool"
"#;
        let m: Manifest = toml::from_str(toml_str).unwrap();
        assert_eq!(m.packages.len(), 1);
        assert_eq!(m.packages[0].name, "ethtool");
        assert!(m.defaults.distros.is_none());
    }

    #[test]
    fn test_deserialize_with_defaults() {
        let toml_str = r#"
[defaults]
distros = "upstream,hyperscale"
track = "centos-stream"
file_issue = true

[[package]]
name = "ethtool"

[[package]]
name = "perf"
repology_name = "linux"
"#;
        let m: Manifest = toml::from_str(toml_str).unwrap();
        assert_eq!(
            m.defaults.distros.as_deref(),
            Some("upstream,hyperscale")
        );
        assert_eq!(m.defaults.file_issue, Some(true));
        assert_eq!(m.packages.len(), 2);
        assert_eq!(
            m.packages[1].repology_name.as_deref(),
            Some("linux")
        );
    }

    #[test]
    fn test_resolve_inherits_defaults() {
        let toml_str = r#"
[defaults]
distros = "upstream,hs9"
track = "centos-stream"
file_issue = true

[[package]]
name = "ethtool"
"#;
        let m: Manifest = toml::from_str(toml_str).unwrap();
        let resolved = m.resolve().unwrap();
        assert_eq!(resolved.len(), 1);
        let pkg = &resolved[0];
        assert!(pkg.distros.upstream);
        assert!(pkg.distros.hyperscale_9);
        assert!(!pkg.distros.hyperscale_10);
        assert_eq!(pkg.track, TrackRef::CentosStream);
        assert!(pkg.file_issue);
    }

    #[test]
    fn test_resolve_per_package_overrides() {
        let toml_str = r#"
[defaults]
distros = "upstream"
track = "upstream"
file_issue = false

[[package]]
name = "systemd"
distros = "upstream,fedora,hyperscale"
track = "fedora-rawhide"
file_issue = true
issue_url = "https://gitlab.com/custom/systemd"
"#;
        let m: Manifest = toml::from_str(toml_str).unwrap();
        let resolved = m.resolve().unwrap();
        let pkg = &resolved[0];
        assert!(pkg.distros.fedora_rawhide);
        assert!(pkg.distros.fedora_stable);
        assert_eq!(pkg.track, TrackRef::FedoraRawhide);
        assert!(pkg.file_issue);
        assert_eq!(
            pkg.issue_url.as_deref(),
            Some("https://gitlab.com/custom/systemd")
        );
    }

    #[test]
    fn test_resolve_hardcoded_fallbacks() {
        let toml_str = r#"
[[package]]
name = "pkg"
"#;
        let m: Manifest = toml::from_str(toml_str).unwrap();
        let resolved = m.resolve().unwrap();
        let pkg = &resolved[0];
        assert_eq!(pkg.distros, Distros::all());
        assert_eq!(pkg.track, TrackRef::Upstream);
        assert!(!pkg.file_issue);
        assert!(pkg.repology_name.is_none());
        assert!(pkg.issue_url.is_none());
    }

    #[test]
    fn test_resolve_bad_distro() {
        let toml_str = r#"
[[package]]
name = "bad"
distros = "bogus"
"#;
        let m: Manifest = toml::from_str(toml_str).unwrap();
        let err = m.resolve().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bad"));
        assert!(msg.contains("bogus"));
    }

    #[test]
    fn test_resolve_bad_track() {
        let toml_str = r#"
[[package]]
name = "bad"
track = "nope"
"#;
        let m: Manifest = toml::from_str(toml_str).unwrap();
        let err = m.resolve().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("bad"));
        assert!(msg.contains("nope"));
    }

    #[test]
    fn test_resolve_repology_name_from_defaults() {
        let toml_str = r#"
[defaults]
repology_name = "linux"

[[package]]
name = "perf"
"#;
        let m: Manifest = toml::from_str(toml_str).unwrap();
        let resolved = m.resolve().unwrap();
        assert_eq!(
            resolved[0].repology_name.as_deref(),
            Some("linux")
        );
    }

    #[test]
    fn test_resolve_repology_name_override() {
        let toml_str = r#"
[defaults]
repology_name = "default-name"

[[package]]
name = "perf"
repology_name = "linux"
"#;
        let m: Manifest = toml::from_str(toml_str).unwrap();
        let resolved = m.resolve().unwrap();
        assert_eq!(
            resolved[0].repology_name.as_deref(),
            Some("linux")
        );
    }

    #[test]
    fn test_resolve_issue_url_from_defaults() {
        let toml_str = r#"
[defaults]
file_issue = true
issue_url = "https://gitlab.com/default/project"

[[package]]
name = "pkg"
"#;
        let m: Manifest = toml::from_str(toml_str).unwrap();
        let resolved = m.resolve().unwrap();
        assert_eq!(
            resolved[0].issue_url.as_deref(),
            Some("https://gitlab.com/default/project")
        );
    }

    #[test]
    fn test_multiple_packages() {
        let toml_str = r#"
[defaults]
file_issue = true

[[package]]
name = "ethtool"

[[package]]
name = "perf"
repology_name = "linux"

[[package]]
name = "systemd"
file_issue = false
"#;
        let m: Manifest = toml::from_str(toml_str).unwrap();
        let resolved = m.resolve().unwrap();
        assert_eq!(resolved.len(), 3);
        assert!(resolved[0].file_issue);
        assert!(resolved[1].file_issue);
        assert!(!resolved[2].file_issue);
        assert_eq!(
            resolved[1].repology_name.as_deref(),
            Some("linux")
        );
    }

    #[test]
    fn test_sort_packages() {
        let toml_str = r#"
[[package]]
name = "systemd"

[[package]]
name = "ethtool"

[[package]]
name = "perf"
"#;
        let mut m: Manifest = toml::from_str(toml_str).unwrap();
        m.sort_packages();
        assert_eq!(m.packages[0].name, "ethtool");
        assert_eq!(m.packages[1].name, "perf");
        assert_eq!(m.packages[2].name, "systemd");
    }

    #[test]
    fn test_add_packages() {
        let toml_str = r#"
[[package]]
name = "ethtool"

[[package]]
name = "systemd"
"#;
        let mut m: Manifest = toml::from_str(toml_str).unwrap();
        m.add_packages(&[
            "perf".into(),
            "bpftrace".into(),
        ]);
        assert_eq!(m.packages.len(), 4);
        assert_eq!(m.packages[0].name, "bpftrace");
        assert_eq!(m.packages[1].name, "ethtool");
        assert_eq!(m.packages[2].name, "perf");
        assert_eq!(m.packages[3].name, "systemd");
    }

    #[test]
    fn test_add_packages_skips_duplicates() {
        let toml_str = r#"
[[package]]
name = "ethtool"
"#;
        let mut m: Manifest = toml::from_str(toml_str).unwrap();
        m.add_packages(&["ethtool".into(), "perf".into()]);
        assert_eq!(m.packages.len(), 2);
        assert_eq!(m.packages[0].name, "ethtool");
        assert_eq!(m.packages[1].name, "perf");
    }

    #[test]
    fn test_add_packages_preserves_existing_fields() {
        let toml_str = r#"
[[package]]
name = "perf"
repology_name = "linux"
"#;
        let mut m: Manifest = toml::from_str(toml_str).unwrap();
        m.add_packages(&["ethtool".into()]);
        assert_eq!(m.packages.len(), 2);
        assert_eq!(m.packages[0].name, "ethtool");
        assert_eq!(m.packages[1].name, "perf");
        assert_eq!(
            m.packages[1].repology_name.as_deref(),
            Some("linux")
        );
    }

    #[test]
    fn test_save_and_reload() {
        let toml_str = r#"
[defaults]
file_issue = true

[[package]]
name = "systemd"

[[package]]
name = "ethtool"
"#;
        let mut m: Manifest = toml::from_str(toml_str).unwrap();
        m.add_packages(&["perf".into()]);

        let dir = std::env::temp_dir().join("hs-relmon-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test-save.toml");
        m.save(&path).unwrap();

        let reloaded = Manifest::load(&path).unwrap();
        assert_eq!(reloaded.packages.len(), 3);
        assert_eq!(reloaded.packages[0].name, "ethtool");
        assert_eq!(reloaded.packages[1].name, "perf");
        assert_eq!(reloaded.packages[2].name, "systemd");
        assert_eq!(reloaded.defaults.file_issue, Some(true));

        // Verify SPDX header
        let contents =
            std::fs::read_to_string(&path).unwrap();
        assert!(contents.starts_with(
            "# SPDX-License-Identifier: MPL-2.0"
        ));

        std::fs::remove_file(&path).ok();
    }
}
