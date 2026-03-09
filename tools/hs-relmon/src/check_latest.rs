// SPDX-License-Identifier: MPL-2.0

use crate::cbs::{self, HyperscaleSummary};
use crate::repology;
use serde::Serialize;

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
#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub package: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fedora_rawhide: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fedora_stable: Option<VersionWithDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub centos_stream: Option<VersionWithDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hs9: Option<HyperscaleSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hs10: Option<HyperscaleSummary>,
}

#[derive(Debug, Serialize)]
pub struct VersionWithDetail {
    pub version: String,
    pub detail: String,
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
                VersionWithDetail {
                    version: p.version.clone(),
                    detail: p.repo.clone(),
                }
            });
        }
        if distros.centos_stream {
            result.centos_stream =
                repology::latest_centos_stream(&packages).map(|p| VersionWithDetail {
                    version: p.version.clone(),
                    detail: p.repo.clone(),
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

/// A single row in the output table.
struct Row {
    distro: String,
    version: String,
    detail: String,
}

/// Collect the result into table rows.
fn result_to_rows(result: &CheckResult) -> Vec<Row> {
    let mut rows = Vec::new();

    if let Some(v) = &result.upstream {
        rows.push(Row {
            distro: "Upstream".into(),
            version: v.clone(),
            detail: String::new(),
        });
    }
    if let Some(v) = &result.fedora_rawhide {
        rows.push(Row {
            distro: "Fedora Rawhide".into(),
            version: v.clone(),
            detail: String::new(),
        });
    }
    if let Some(vd) = &result.fedora_stable {
        rows.push(Row {
            distro: "Fedora Stable".into(),
            version: vd.version.clone(),
            detail: vd.detail.clone(),
        });
    }
    if let Some(vd) = &result.centos_stream {
        rows.push(Row {
            distro: "CentOS Stream".into(),
            version: vd.version.clone(),
            detail: vd.detail.clone(),
        });
    }
    if let Some(summary) = &result.hs9 {
        hs_rows(&mut rows, "Hyperscale 9", summary);
    }
    if let Some(summary) = &result.hs10 {
        hs_rows(&mut rows, "Hyperscale 10", summary);
    }

    rows
}

fn hs_rows(rows: &mut Vec<Row>, label: &str, summary: &HyperscaleSummary) {
    match (&summary.release, &summary.testing) {
        (Some(rel), Some(test)) => {
            rows.push(Row {
                distro: format!("{label} (release)"),
                version: rel.version.clone(),
                detail: rel.nvr.clone(),
            });
            rows.push(Row {
                distro: format!("{label} (testing)"),
                version: test.version.clone(),
                detail: test.nvr.clone(),
            });
        }
        (Some(rel), None) => {
            rows.push(Row {
                distro: label.into(),
                version: rel.version.clone(),
                detail: rel.nvr.clone(),
            });
        }
        (None, Some(test)) => {
            rows.push(Row {
                distro: format!("{label} (testing)"),
                version: test.version.clone(),
                detail: test.nvr.clone(),
            });
        }
        (None, None) => {
            rows.push(Row {
                distro: label.into(),
                version: "not found".into(),
                detail: String::new(),
            });
        }
    }
}

/// Format the result as a table and print to stdout.
pub fn print_table(result: &CheckResult) {
    let rows = result_to_rows(result);
    if rows.is_empty() {
        return;
    }

    let distro_w = rows.iter().map(|r| r.distro.len()).max().unwrap_or(0);
    let version_w = rows.iter().map(|r| r.version.len()).max().unwrap_or(0);

    println!("{}", result.package);
    println!(
        "  {:<distro_w$}  {:<version_w$}  {}",
        "Distribution", "Version", "Detail"
    );
    println!(
        "  {:<distro_w$}  {:<version_w$}  {}",
        "─".repeat(distro_w),
        "─".repeat(version_w),
        "──────"
    );
    for row in &rows {
        if row.detail.is_empty() {
            println!("  {:<distro_w$}  {}", row.distro, row.version);
        } else {
            println!(
                "  {:<distro_w$}  {:<version_w$}  {}",
                row.distro, row.version, row.detail
            );
        }
    }
}

/// Format the result as JSON and print to stdout.
pub fn print_json(result: &CheckResult) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string_pretty(result)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cbs::Build;

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

    fn make_build(version: &str, nvr: &str) -> Build {
        Build {
            build_id: 1,
            name: "pkg".into(),
            version: version.into(),
            release: String::new(),
            nvr: nvr.into(),
        }
    }

    #[test]
    fn test_result_to_rows_all_fields() {
        let result = CheckResult {
            package: "ethtool".into(),
            upstream: Some("6.19".into()),
            fedora_rawhide: Some("6.19".into()),
            fedora_stable: Some(VersionWithDetail {
                version: "6.19".into(),
                detail: "fedora_43".into(),
            }),
            centos_stream: Some(VersionWithDetail {
                version: "6.15".into(),
                detail: "centos_stream_10".into(),
            }),
            hs9: Some(HyperscaleSummary {
                release: Some(make_build("6.15", "ethtool-6.15-3.hs.el9")),
                testing: None,
            }),
            hs10: None,
        };
        let rows = result_to_rows(&result);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].distro, "Upstream");
        assert_eq!(rows[0].version, "6.19");
        assert_eq!(rows[3].distro, "CentOS Stream");
        assert_eq!(rows[4].distro, "Hyperscale 9");
    }

    #[test]
    fn test_result_to_rows_hs_testing_and_release() {
        let result = CheckResult {
            package: "systemd".into(),
            upstream: None,
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(HyperscaleSummary {
                release: Some(make_build("258.5", "systemd-258.5-1.1.hs.el9")),
                testing: Some(make_build("260~rc2", "systemd-260~rc2-20260309.hs.el9")),
            }),
            hs10: None,
        };
        let rows = result_to_rows(&result);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].distro, "Hyperscale 9 (release)");
        assert_eq!(rows[0].version, "258.5");
        assert_eq!(rows[1].distro, "Hyperscale 9 (testing)");
        assert_eq!(rows[1].version, "260~rc2");
    }

    #[test]
    fn test_result_to_rows_hs_testing_only() {
        let result = CheckResult {
            package: "pkg".into(),
            upstream: None,
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(HyperscaleSummary {
                release: None,
                testing: Some(make_build("1.0", "pkg-1.0-1.hs.el9")),
            }),
            hs10: None,
        };
        let rows = result_to_rows(&result);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].distro, "Hyperscale 9 (testing)");
    }

    #[test]
    fn test_result_to_rows_hs_not_found() {
        let result = CheckResult {
            package: "pkg".into(),
            upstream: None,
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(HyperscaleSummary {
                release: None,
                testing: None,
            }),
            hs10: None,
        };
        let rows = result_to_rows(&result);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].distro, "Hyperscale 9");
        assert_eq!(rows[0].version, "not found");
    }

    #[test]
    fn test_json_serialization() {
        let result = CheckResult {
            package: "ethtool".into(),
            upstream: Some("6.19".into()),
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: None,
            hs10: None,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["package"], "ethtool");
        assert_eq!(json["upstream"], "6.19");
        // None fields should be absent
        assert!(json.get("fedora_rawhide").is_none());
        assert!(json.get("hs9").is_none());
    }
}
