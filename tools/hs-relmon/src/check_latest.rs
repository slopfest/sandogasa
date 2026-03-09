// SPDX-License-Identifier: MPL-2.0

use crate::cbs::{self, HyperscaleSummary};
use crate::repology;

/// Which distribution sources to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distros {
    pub upstream: bool,
    pub fedora_rawhide: bool,
    pub fedora_stable: bool,
    pub centos_stream: bool,
    pub hyperscale_9: bool,
    pub hyperscale_10: bool,
}

impl Distros {
    /// All sources enabled (the default).
    pub fn all() -> Self {
        Self {
            upstream: true,
            fedora_rawhide: true,
            fedora_stable: true,
            centos_stream: true,
            hyperscale_9: true,
            hyperscale_10: true,
        }
    }

    /// Parse a comma-separated list of distro names.
    ///
    /// Valid names: `upstream`, `fedora` (rawhide + stable), `fedora-rawhide`,
    /// `fedora-stable`, `centos`, `hyperscale` (9 + 10), `hs9`, `hs10`.
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut d = Self {
            upstream: false,
            fedora_rawhide: false,
            fedora_stable: false,
            centos_stream: false,
            hyperscale_9: false,
            hyperscale_10: false,
        };
        for token in input.split(',') {
            match token.trim() {
                "upstream" => d.upstream = true,
                "fedora" => {
                    d.fedora_rawhide = true;
                    d.fedora_stable = true;
                }
                "fedora-rawhide" => d.fedora_rawhide = true,
                "fedora-stable" => d.fedora_stable = true,
                "centos" | "centos-stream" => d.centos_stream = true,
                "hyperscale" | "hs" => {
                    d.hyperscale_9 = true;
                    d.hyperscale_10 = true;
                }
                "hs9" => d.hyperscale_9 = true,
                "hs10" => d.hyperscale_10 = true,
                other => return Err(format!("unknown distro: {other:?}")),
            }
        }
        Ok(d)
    }

    fn needs_repology(&self) -> bool {
        self.upstream || self.fedora_rawhide || self.fedora_stable || self.centos_stream
    }

    fn needs_cbs(&self) -> bool {
        self.hyperscale_9 || self.hyperscale_10
    }
}

/// Result of checking a single package across selected distros.
#[derive(Debug)]
pub struct CheckResult {
    pub package: String,
    pub upstream: Option<String>,
    pub fedora_rawhide: Option<String>,
    pub fedora_stable: Option<VersionWithRepo>,
    pub centos_stream: Option<VersionWithRepo>,
    pub hs9: Option<HyperscaleSummary>,
    pub hs10: Option<HyperscaleSummary>,
}

#[derive(Debug)]
pub struct VersionWithRepo {
    pub version: String,
    pub repo: String,
}

/// Run the check-latest query for a package with the given distro selection.
pub fn check(
    repology_client: &repology::Client,
    cbs_client: &cbs::Client,
    package: &str,
    distros: &Distros,
) -> Result<CheckResult, Box<dyn std::error::Error>> {
    let mut result = CheckResult {
        package: package.to_string(),
        upstream: None,
        fedora_rawhide: None,
        fedora_stable: None,
        centos_stream: None,
        hs9: None,
        hs10: None,
    };

    if distros.needs_repology() {
        let packages = repology_client.get_project(package)?;

        if distros.upstream {
            result.upstream = repology::find_newest(&packages).map(|p| p.version.clone());
        }
        if distros.fedora_rawhide {
            result.fedora_rawhide = repology::latest_for_repo(&packages, "fedora_rawhide")
                .map(|p| p.version.clone());
        }
        if distros.fedora_stable {
            result.fedora_stable = repology::latest_fedora_stable(&packages).map(|p| {
                VersionWithRepo {
                    version: p.version.clone(),
                    repo: p.repo.clone(),
                }
            });
        }
        if distros.centos_stream {
            result.centos_stream =
                repology::latest_centos_stream(&packages).map(|p| VersionWithRepo {
                    version: p.version.clone(),
                    repo: p.repo.clone(),
                });
        }
    }

    if distros.needs_cbs() {
        let builds = cbs_client
            .get_package_id(package)?
            .map(|id| cbs_client.list_builds(id))
            .transpose()?;
        let empty = Vec::new();
        let builds = builds.as_deref().unwrap_or(&empty);

        if distros.hyperscale_9 {
            result.hs9 = Some(cbs_client.hyperscale_summary(builds, 9)?);
        }
        if distros.hyperscale_10 {
            result.hs10 = Some(cbs_client.hyperscale_summary(builds, 10)?);
        }
    }

    Ok(result)
}

/// Format and print a CheckResult.
pub fn print_result(result: &CheckResult) {
    println!("{}:", result.package);
    if let Some(v) = &result.upstream {
        println!("  upstream newest:  {v}");
    }
    if let Some(v) = &result.fedora_rawhide {
        println!("  fedora rawhide:   {v}");
    }
    if let Some(vr) = &result.fedora_stable {
        println!("  fedora stable:    {} ({})", vr.version, vr.repo);
    }
    if let Some(vr) = &result.centos_stream {
        println!("  centos stream:    {} ({})", vr.version, vr.repo);
    }
    if let Some(summary) = &result.hs9 {
        print_hyperscale("hs9", summary);
    }
    if let Some(summary) = &result.hs10 {
        print_hyperscale("hs10", summary);
    }
}

fn print_hyperscale(label: &str, summary: &HyperscaleSummary) {
    match (&summary.release, &summary.testing) {
        (Some(rel), Some(test)) => {
            println!("  {label} release: {} ({})", rel.version, rel.nvr);
            println!("  {label} testing: {} ({})", test.version, test.nvr);
        }
        (Some(rel), None) => {
            println!("  {label} release: {} ({})", rel.version, rel.nvr);
        }
        (None, Some(test)) => {
            println!("  {label} release: not found");
            println!("  {label} testing: {} ({})", test.version, test.nvr);
        }
        (None, None) => {
            println!("  {label}:         not found");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_distros_all() {
        let d = Distros::all();
        assert!(d.upstream);
        assert!(d.fedora_rawhide);
        assert!(d.fedora_stable);
        assert!(d.centos_stream);
        assert!(d.hyperscale_9);
        assert!(d.hyperscale_10);
    }

    #[test]
    fn test_distros_parse_single() {
        let d = Distros::parse("upstream").unwrap();
        assert!(d.upstream);
        assert!(!d.fedora_rawhide);
        assert!(!d.hyperscale_9);
    }

    #[test]
    fn test_distros_parse_fedora_expands() {
        let d = Distros::parse("fedora").unwrap();
        assert!(d.fedora_rawhide);
        assert!(d.fedora_stable);
        assert!(!d.upstream);
    }

    #[test]
    fn test_distros_parse_hyperscale_expands() {
        let d = Distros::parse("hyperscale").unwrap();
        assert!(d.hyperscale_9);
        assert!(d.hyperscale_10);
        assert!(!d.upstream);
    }

    #[test]
    fn test_distros_parse_hs_alias() {
        let d = Distros::parse("hs").unwrap();
        assert!(d.hyperscale_9);
        assert!(d.hyperscale_10);
    }

    #[test]
    fn test_distros_parse_comma_separated() {
        let d = Distros::parse("upstream,fedora-rawhide,hs10").unwrap();
        assert!(d.upstream);
        assert!(d.fedora_rawhide);
        assert!(!d.fedora_stable);
        assert!(d.hyperscale_10);
        assert!(!d.hyperscale_9);
    }

    #[test]
    fn test_distros_parse_centos_aliases() {
        let d1 = Distros::parse("centos").unwrap();
        assert!(d1.centos_stream);
        let d2 = Distros::parse("centos-stream").unwrap();
        assert!(d2.centos_stream);
    }

    #[test]
    fn test_distros_parse_with_spaces() {
        let d = Distros::parse("upstream , hs9").unwrap();
        assert!(d.upstream);
        assert!(d.hyperscale_9);
    }

    #[test]
    fn test_distros_parse_unknown() {
        let err = Distros::parse("upstream,bogus").unwrap_err();
        assert!(err.contains("bogus"));
    }

    #[test]
    fn test_needs_repology() {
        let d = Distros::parse("hs9").unwrap();
        assert!(!d.needs_repology());
        assert!(d.needs_cbs());

        let d = Distros::parse("upstream").unwrap();
        assert!(d.needs_repology());
        assert!(!d.needs_cbs());
    }
}
