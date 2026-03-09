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

/// Which distribution to compare Hyperscale builds against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackRef {
    Upstream,
    FedoraRawhide,
    FedoraStable,
    CentosStream,
}

impl TrackRef {
    /// Parse a track reference name.
    ///
    /// Valid names: `upstream`, `fedora-rawhide`, `fedora-stable`, `centos`,
    /// `centos-stream`.
    pub fn parse(input: &str) -> Result<Self, String> {
        match input.trim() {
            "upstream" => Ok(Self::Upstream),
            "fedora-rawhide" => Ok(Self::FedoraRawhide),
            "fedora-stable" => Ok(Self::FedoraStable),
            "centos" | "centos-stream" => Ok(Self::CentosStream),
            other => Err(format!("unknown track reference: {other:?}")),
        }
    }

    /// Resolve the reference version from Repology package data.
    fn resolve(&self, packages: &[repology::Package]) -> Option<String> {
        match self {
            Self::Upstream => repology::find_newest(packages).map(|p| p.version.clone()),
            Self::FedoraRawhide => {
                repology::latest_for_repo(packages, "fedora_rawhide").map(|p| p.version.clone())
            }
            Self::FedoraStable => {
                repology::latest_fedora_stable(packages).map(|p| p.version.clone())
            }
            Self::CentosStream => {
                repology::latest_centos_stream(packages).map(|p| p.version.clone())
            }
        }
    }
}

/// A Hyperscale summary with optional freshness status.
#[derive(Debug, Serialize)]
pub struct HyperscaleResult {
    #[serde(flatten)]
    pub summary: HyperscaleSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub newest_version: Option<bool>,
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
    pub hs9: Option<HyperscaleResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hs10: Option<HyperscaleResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<IssueRef>,
    /// Reference version for tracking (not included in JSON).
    #[serde(skip)]
    ref_version: Option<String>,
}

/// Reference to a GitLab issue.
#[derive(Debug, Clone, Serialize)]
pub struct IssueRef {
    pub iid: u64,
    pub url: String,
    pub status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assignees: Vec<String>,
}

impl IssueRef {
    /// Build an `IssueRef` from a GitLab API issue.
    ///
    /// `status` is the resolved work-item status
    /// (e.g. "To do"), falling back to `issue.state` if
    /// the GraphQL status is unavailable.
    pub fn from_gitlab_issue(
        issue: &crate::gitlab::Issue,
        status: Option<String>,
    ) -> Self {
        Self {
            iid: issue.iid,
            url: issue.web_url.clone(),
            status: status
                .unwrap_or_else(|| issue.state.clone()),
            assignees: issue
                .assignees
                .iter()
                .map(|a| a.username.clone())
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct VersionWithDetail {
    pub version: String,
    pub detail: String,
}

impl CheckResult {
    /// Whether any Hyperscale build is outdated relative to the reference.
    pub fn is_outdated(&self) -> bool {
        [&self.hs9, &self.hs10]
            .iter()
            .filter_map(|r| r.as_ref())
            .any(|r| r.newest_version == Some(false))
    }

    /// The reference version used for tracking.
    pub fn ref_version(&self) -> Option<&str> {
        self.ref_version.as_deref()
    }

    /// Whether the issue (if any) matches the given filters.
    ///
    /// Returns `false` if there is no issue. Both filters
    /// must match when provided.
    pub fn matches_issue_filter(
        &self,
        status: Option<&str>,
        assignee: Option<&str>,
    ) -> bool {
        let issue = match &self.issue {
            Some(i) => i,
            None => return false,
        };
        if let Some(s) = status {
            if issue.status != s {
                return false;
            }
        }
        if let Some(a) = assignee {
            if !issue.assignees.iter().any(|u| u == a) {
                return false;
            }
        }
        true
    }
}

/// Run the check-latest query for a package with the given distro selection.
///
/// The `track` reference determines which distribution Hyperscale builds are
/// compared against to determine freshness (newest / outdated).
pub fn check(
    repology_client: &repology::Client,
    cbs_client: &cbs::Client,
    package: &str,
    repology_name: &str,
    distros: &Distros,
    track: &TrackRef,
) -> Result<CheckResult, Box<dyn std::error::Error>> {
    let mut result = CheckResult {
        package: package.to_string(),
        upstream: None,
        fedora_rawhide: None,
        fedora_stable: None,
        centos_stream: None,
        hs9: None,
        hs10: None,
        issue: None,
        ref_version: None,
    };

    // Fetch Repology data if needed for display or for tracking reference.
    let fetch_repology = distros.needs_repology() || distros.needs_cbs();
    let packages = if fetch_repology {
        repology_client.get_project(repology_name)?
    } else {
        Vec::new()
    };

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

    let ref_version = track.resolve(&packages);
    result.ref_version = ref_version.clone();

    if distros.needs_cbs() {
        let builds = cbs_client
            .get_package_id(package)?
            .map(|id| cbs_client.list_builds(id))
            .transpose()?;
        let empty = Vec::new();
        let builds = builds.as_deref().unwrap_or(&empty);

        if distros.hyperscale_9 {
            let summary = cbs_client.hyperscale_summary(builds, 9)?;
            let newest_version = compute_newest_version(&summary, &ref_version);
            result.hs9 = Some(HyperscaleResult {
                summary,
                newest_version,
            });
        }
        if distros.hyperscale_10 {
            let summary = cbs_client.hyperscale_summary(builds, 10)?;
            let newest_version = compute_newest_version(&summary, &ref_version);
            result.hs10 = Some(HyperscaleResult {
                summary,
                newest_version,
            });
        }
    }

    Ok(result)
}

/// Determine whether the effective Hyperscale version is at least as
/// new as the reference.
///
/// Uses the release build version, falling back to testing if no release exists.
/// Returns `None` if the reference version is unknown.
fn compute_newest_version(summary: &HyperscaleSummary, ref_version: &Option<String>) -> Option<bool> {
    let ref_ver = ref_version.as_ref()?;
    let effective = summary.release.as_ref().or(summary.testing.as_ref())?;
    Some(repology::version_cmp(&effective.version, ref_ver) != std::cmp::Ordering::Less)
}

/// A single row in the output table.
struct Row {
    distro: String,
    version: String,
    detail: String,
    status: String,
}

/// Collect the result into table rows.
fn result_to_rows(result: &CheckResult) -> Vec<Row> {
    let mut rows = Vec::new();

    if let Some(v) = &result.upstream {
        rows.push(Row {
            distro: "Upstream".into(),
            version: v.clone(),
            detail: String::new(),
            status: String::new(),
        });
    }
    if let Some(v) = &result.fedora_rawhide {
        rows.push(Row {
            distro: "Fedora Rawhide".into(),
            version: v.clone(),
            detail: String::new(),
            status: String::new(),
        });
    }
    if let Some(vd) = &result.fedora_stable {
        rows.push(Row {
            distro: "Fedora Stable".into(),
            version: vd.version.clone(),
            detail: vd.detail.clone(),
            status: String::new(),
        });
    }
    if let Some(vd) = &result.centos_stream {
        rows.push(Row {
            distro: "CentOS Stream".into(),
            version: vd.version.clone(),
            detail: vd.detail.clone(),
            status: String::new(),
        });
    }
    if let Some(hs_result) = &result.hs9 {
        hs_rows(&mut rows, "Hyperscale 9", &hs_result.summary, result.ref_version.as_deref());
    }
    if let Some(hs_result) = &result.hs10 {
        hs_rows(&mut rows, "Hyperscale 10", &hs_result.summary, result.ref_version.as_deref());
    }

    rows
}

fn version_status(version: &str, ref_version: Option<&str>) -> String {
    match ref_version {
        Some(ref_ver) => {
            if repology::version_cmp(version, ref_ver) != std::cmp::Ordering::Less {
                "newest".into()
            } else {
                "outdated".into()
            }
        }
        None => String::new(),
    }
}

fn hs_rows(rows: &mut Vec<Row>, label: &str, summary: &HyperscaleSummary, ref_version: Option<&str>) {
    match (&summary.release, &summary.testing) {
        (Some(rel), Some(test)) => {
            rows.push(Row {
                distro: format!("{label} (release)"),
                version: rel.version.clone(),
                detail: rel.nvr.clone(),
                status: version_status(&rel.version, ref_version),
            });
            rows.push(Row {
                distro: format!("{label} (testing)"),
                version: test.version.clone(),
                detail: test.nvr.clone(),
                status: version_status(&test.version, ref_version),
            });
        }
        (Some(rel), None) => {
            rows.push(Row {
                distro: label.into(),
                version: rel.version.clone(),
                detail: rel.nvr.clone(),
                status: version_status(&rel.version, ref_version),
            });
        }
        (None, Some(test)) => {
            rows.push(Row {
                distro: format!("{label} (testing)"),
                version: test.version.clone(),
                detail: test.nvr.clone(),
                status: version_status(&test.version, ref_version),
            });
        }
        (None, None) => {
            rows.push(Row {
                distro: label.into(),
                version: "not found".into(),
                detail: String::new(),
                status: String::new(),
            });
        }
    }
}

/// Format the result as a table string.
pub fn format_table(result: &CheckResult) -> String {
    let mut buf = Vec::new();
    let _ = write_table(result, &mut buf);
    String::from_utf8(buf).unwrap_or_default()
}

/// Format the result as a table and print to stdout.
pub fn print_table(result: &CheckResult) {
    let _ = write_table(result, &mut std::io::stdout().lock());
}

/// Format the result as JSON and print to stdout.
pub fn print_json(result: &CheckResult) -> Result<(), Box<dyn std::error::Error>> {
    write_json(result, &mut std::io::stdout().lock())?;
    Ok(())
}

/// Format multiple results as a JSON array and print to stdout.
pub fn print_json_array(
    results: &[CheckResult],
) -> Result<(), Box<dyn std::error::Error>> {
    write_json_array(results, &mut std::io::stdout().lock())
}

fn write_json_array(
    results: &[CheckResult],
    w: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error>> {
    writeln!(w, "{}", serde_json::to_string_pretty(results)?)?;
    Ok(())
}

fn write_table(
    result: &CheckResult,
    w: &mut dyn std::io::Write,
) -> std::io::Result<()> {
    let rows = result_to_rows(result);
    if rows.is_empty() {
        return Ok(());
    }

    let distro_w = rows.iter().map(|r| r.distro.len()).max().unwrap_or(0).max("Distribution".len());
    let version_w = rows.iter().map(|r| r.version.len()).max().unwrap_or(0).max("Version".len());
    let has_status = rows.iter().any(|r| !r.status.is_empty());
    let detail_w = rows
        .iter()
        .map(|r| r.detail.len())
        .max()
        .unwrap_or(0)
        .max("Detail".len());

    writeln!(w, "{}", result.package)?;
    if has_status {
        writeln!(
            w,
            "  {:<distro_w$}  {:<version_w$}  {:<detail_w$}  {}",
            "Distribution", "Version", "Detail", "Status"
        )?;
        writeln!(
            w,
            "  {:<distro_w$}  {:<version_w$}  {:<detail_w$}  {}",
            "─".repeat(distro_w),
            "─".repeat(version_w),
            "─".repeat(detail_w),
            "──────"
        )?;
    } else {
        writeln!(
            w,
            "  {:<distro_w$}  {:<version_w$}  {}",
            "Distribution", "Version", "Detail"
        )?;
        writeln!(
            w,
            "  {:<distro_w$}  {:<version_w$}  {}",
            "─".repeat(distro_w),
            "─".repeat(version_w),
            "──────"
        )?;
    }
    for row in &rows {
        if !row.status.is_empty() {
            writeln!(
                w,
                "  {:<distro_w$}  {:<version_w$}  {:<detail_w$}  {}",
                row.distro, row.version, row.detail, row.status
            )?;
        } else if row.detail.is_empty() {
            writeln!(w, "  {:<distro_w$}  {}", row.distro, row.version)?;
        } else {
            writeln!(
                w,
                "  {:<distro_w$}  {:<version_w$}  {}",
                row.distro, row.version, row.detail
            )?;
        }
    }
    Ok(())
}

fn write_json(
    result: &CheckResult,
    w: &mut dyn std::io::Write,
) -> Result<(), Box<dyn std::error::Error>> {
    writeln!(w, "{}", serde_json::to_string_pretty(result)?)?;
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
    fn test_track_ref_parse() {
        assert_eq!(TrackRef::parse("upstream").unwrap(), TrackRef::Upstream);
        assert_eq!(
            TrackRef::parse("fedora-rawhide").unwrap(),
            TrackRef::FedoraRawhide
        );
        assert_eq!(
            TrackRef::parse("fedora-stable").unwrap(),
            TrackRef::FedoraStable
        );
        assert_eq!(
            TrackRef::parse("centos").unwrap(),
            TrackRef::CentosStream
        );
        assert_eq!(
            TrackRef::parse("centos-stream").unwrap(),
            TrackRef::CentosStream
        );
        assert!(TrackRef::parse("bogus").is_err());
    }

    #[test]
    fn test_track_ref_parse_trims_spaces() {
        assert_eq!(
            TrackRef::parse("  upstream  ").unwrap(),
            TrackRef::Upstream
        );
    }

    fn make_hs_result(summary: HyperscaleSummary) -> HyperscaleResult {
        HyperscaleResult {
            summary,
            newest_version: None,
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
            hs9: Some(make_hs_result(HyperscaleSummary {
                release: Some(make_build("6.15", "ethtool-6.15-3.hs.el9")),
                testing: None,
            })),
            hs10: None,
            issue: None,
            ref_version: None,
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
            hs9: Some(make_hs_result(HyperscaleSummary {
                release: Some(make_build("258.5", "systemd-258.5-1.1.hs.el9")),
                testing: Some(make_build("260~rc2", "systemd-260~rc2-20260309.hs.el9")),
            })),
            hs10: None,
            issue: None,
            ref_version: None,
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
            hs9: Some(make_hs_result(HyperscaleSummary {
                release: None,
                testing: Some(make_build("1.0", "pkg-1.0-1.hs.el9")),
            })),
            hs10: None,
            issue: None,
            ref_version: None,
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
            hs9: Some(make_hs_result(HyperscaleSummary {
                release: None,
                testing: None,
            })),
            hs10: None,
            issue: None,
            ref_version: None,
        };
        let rows = result_to_rows(&result);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].distro, "Hyperscale 9");
        assert_eq!(rows[0].version, "not found");
    }

    #[test]
    fn test_result_to_rows_with_tracking_outdated() {
        let result = CheckResult {
            package: "ethtool".into(),
            upstream: Some("6.19".into()),
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(make_hs_result(HyperscaleSummary {
                release: Some(make_build("6.15", "ethtool-6.15-3.hs.el9")),
                testing: None,
            })),
            hs10: None,
            issue: None,
            ref_version: Some("6.19".into()),
        };
        let rows = result_to_rows(&result);
        assert_eq!(rows[1].status, "outdated");
        assert_eq!(rows[0].status, ""); // non-HS rows have no status
    }

    #[test]
    fn test_result_to_rows_with_tracking_newest() {
        let result = CheckResult {
            package: "ethtool".into(),
            upstream: None,
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(make_hs_result(HyperscaleSummary {
                release: Some(make_build("6.15", "ethtool-6.15-3.hs.el9")),
                testing: None,
            })),
            hs10: None,
            issue: None,
            ref_version: Some("6.15".into()),
        };
        let rows = result_to_rows(&result);
        assert_eq!(rows[0].status, "newest");
    }

    #[test]
    fn test_result_to_rows_tracking_per_build() {
        // release is outdated, testing is newest
        let result = CheckResult {
            package: "systemd".into(),
            upstream: None,
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(make_hs_result(HyperscaleSummary {
                release: Some(make_build("258.5", "systemd-258.5-1.1.hs.el9")),
                testing: Some(make_build("260", "systemd-260-1.hs.el9")),
            })),
            hs10: None,
            issue: None,
            ref_version: Some("260".into()),
        };
        let rows = result_to_rows(&result);
        assert_eq!(rows[0].distro, "Hyperscale 9 (release)");
        assert_eq!(rows[0].status, "outdated");
        assert_eq!(rows[1].distro, "Hyperscale 9 (testing)");
        assert_eq!(rows[1].status, "newest");
    }

    #[test]
    fn test_compute_newest_version_matches() {
        let summary = HyperscaleSummary {
            release: Some(make_build("6.15", "ethtool-6.15-3.hs.el9")),
            testing: None,
        };
        assert_eq!(
            compute_newest_version(&summary, &Some("6.15".into())),
            Some(true)
        );
    }

    #[test]
    fn test_compute_newest_version_outdated() {
        let summary = HyperscaleSummary {
            release: Some(make_build("6.15", "ethtool-6.15-3.hs.el9")),
            testing: None,
        };
        assert_eq!(
            compute_newest_version(&summary, &Some("6.19".into())),
            Some(false)
        );
    }

    #[test]
    fn test_compute_newest_version_ahead() {
        let summary = HyperscaleSummary {
            release: Some(make_build("6.19", "pkg-6.19-1.hs.el9")),
            testing: None,
        };
        assert_eq!(
            compute_newest_version(&summary, &Some("6.18.16".into())),
            Some(true)
        );
    }

    #[test]
    fn test_compute_newest_version_no_ref() {
        let summary = HyperscaleSummary {
            release: Some(make_build("6.15", "ethtool-6.15-3.hs.el9")),
            testing: None,
        };
        assert_eq!(compute_newest_version(&summary, &None), None);
    }

    #[test]
    fn test_compute_newest_version_uses_testing_fallback() {
        let summary = HyperscaleSummary {
            release: None,
            testing: Some(make_build("6.19", "ethtool-6.19-1.hs.el9")),
        };
        assert_eq!(
            compute_newest_version(&summary, &Some("6.19".into())),
            Some(true)
        );
    }

    #[test]
    fn test_compute_newest_version_no_builds() {
        let summary = HyperscaleSummary {
            release: None,
            testing: None,
        };
        assert_eq!(
            compute_newest_version(&summary, &Some("6.19".into())),
            None
        );
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
            issue: None,
            ref_version: None,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["package"], "ethtool");
        assert_eq!(json["upstream"], "6.19");
        // None fields should be absent
        assert!(json.get("fedora_rawhide").is_none());
        assert!(json.get("hs9").is_none());
    }

    #[test]
    fn test_json_serialization_with_newest_version() {
        let result = CheckResult {
            package: "ethtool".into(),
            upstream: Some("6.19".into()),
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(HyperscaleResult {
                summary: HyperscaleSummary {
                    release: Some(make_build("6.15", "ethtool-6.15-3.hs.el9")),
                    testing: None,
                },
                newest_version: Some(false),
            }),
            hs10: None,
            issue: None,
            ref_version: Some("6.19".into()),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["hs9"]["newest_version"], false);
        assert_eq!(json["hs9"]["release"]["version"], "6.15");
        // ref_version should not appear in JSON
        assert!(json.get("ref_version").is_none());
    }

    #[test]
    fn test_is_outdated_true() {
        let result = CheckResult {
            package: "pkg".into(),
            upstream: None,
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(HyperscaleResult {
                summary: HyperscaleSummary {
                    release: Some(make_build("1.0", "pkg-1.0-1.hs.el9")),
                    testing: None,
                },
                newest_version: Some(false),
            }),
            hs10: None,
            issue: None,
            ref_version: Some("2.0".into()),
        };
        assert!(result.is_outdated());
        assert_eq!(result.ref_version(), Some("2.0"));
    }

    #[test]
    fn test_is_outdated_false_when_newest() {
        let result = CheckResult {
            package: "pkg".into(),
            upstream: None,
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(HyperscaleResult {
                summary: HyperscaleSummary {
                    release: Some(make_build("2.0", "pkg-2.0-1.hs.el9")),
                    testing: None,
                },
                newest_version: Some(true),
            }),
            hs10: None,
            issue: None,
            ref_version: Some("2.0".into()),
        };
        assert!(!result.is_outdated());
    }

    #[test]
    fn test_is_outdated_false_when_no_hs() {
        let result = CheckResult {
            package: "pkg".into(),
            upstream: Some("2.0".into()),
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: None,
            hs10: None,
            issue: None,
            ref_version: None,
        };
        assert!(!result.is_outdated());
        assert_eq!(result.ref_version(), None);
    }

    #[test]
    fn test_is_outdated_mixed_hs9_hs10() {
        let result = CheckResult {
            package: "pkg".into(),
            upstream: None,
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(HyperscaleResult {
                summary: HyperscaleSummary {
                    release: Some(make_build("2.0", "pkg-2.0-1.hs.el9")),
                    testing: None,
                },
                newest_version: Some(true),
            }),
            hs10: Some(HyperscaleResult {
                summary: HyperscaleSummary {
                    release: Some(make_build("1.0", "pkg-1.0-1.hs.el10")),
                    testing: None,
                },
                newest_version: Some(false),
            }),
            issue: None,
            ref_version: Some("2.0".into()),
        };
        assert!(result.is_outdated());
    }

    #[test]
    fn test_format_table() {
        let result = CheckResult {
            package: "ethtool".into(),
            upstream: Some("6.19".into()),
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(make_hs_result(HyperscaleSummary {
                release: Some(make_build("6.15", "ethtool-6.15-3.hs.el9")),
                testing: None,
            })),
            hs10: None,
            issue: None,
            ref_version: Some("6.19".into()),
        };
        let table = format_table(&result);
        assert!(table.contains("ethtool"));
        assert!(table.contains("Upstream"));
        assert!(table.contains("outdated"));
    }

    #[test]
    fn test_write_table_with_status() {
        let result = CheckResult {
            package: "ethtool".into(),
            upstream: Some("6.19".into()),
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(make_hs_result(HyperscaleSummary {
                release: Some(make_build("6.15", "ethtool-6.15-3.hs.el9")),
                testing: None,
            })),
            hs10: None,
            issue: None,
            ref_version: Some("6.19".into()),
        };
        let mut buf = Vec::new();
        write_table(&result, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("ethtool"));
        assert!(output.contains("Upstream"));
        assert!(output.contains("6.19"));
        assert!(output.contains("outdated"));
        assert!(output.contains("Status"));
    }

    #[test]
    fn test_write_table_without_status() {
        let result = CheckResult {
            package: "pkg".into(),
            upstream: Some("1.0".into()),
            fedora_rawhide: None,
            fedora_stable: Some(VersionWithDetail {
                version: "1.0".into(),
                detail: "fedora_43".into(),
            }),
            centos_stream: None,
            hs9: None,
            hs10: None,
            issue: None,
            ref_version: None,
        };
        let mut buf = Vec::new();
        write_table(&result, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("pkg"));
        assert!(output.contains("Upstream"));
        assert!(output.contains("fedora_43"));
        assert!(!output.contains("Status"));
    }

    #[test]
    fn test_write_table_empty() {
        let result = CheckResult {
            package: "pkg".into(),
            upstream: None,
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: None,
            hs10: None,
            issue: None,
            ref_version: None,
        };
        let mut buf = Vec::new();
        write_table(&result, &mut buf).unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn test_write_json() {
        let result = CheckResult {
            package: "ethtool".into(),
            upstream: Some("6.19".into()),
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: None,
            hs10: None,
            issue: None,
            ref_version: None,
        };
        let mut buf = Vec::new();
        write_json(&result, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(json["package"], "ethtool");
        assert_eq!(json["upstream"], "6.19");
    }

    #[test]
    fn test_write_json_with_issue() {
        let result = CheckResult {
            package: "pkg".into(),
            upstream: Some("2.0".into()),
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: None,
            hs10: None,
            issue: Some(IssueRef {
                iid: 5,
                url: "https://example.com/-/issues/5".into(),
                status: "opened".into(),
                assignees: vec!["alice".into()],
            }),
            ref_version: None,
        };
        let mut buf = Vec::new();
        write_json(&result, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&output).unwrap();
        assert_eq!(json["issue"]["iid"], 5);
        assert_eq!(json["issue"]["status"], "opened");
        assert_eq!(json["issue"]["assignees"][0], "alice");
    }

    #[test]
    fn test_write_json_array() {
        let results = vec![
            CheckResult {
                package: "a".into(),
                upstream: Some("1.0".into()),
                fedora_rawhide: None,
                fedora_stable: None,
                centos_stream: None,
                hs9: None,
                hs10: None,
                issue: None,
                ref_version: None,
            },
            CheckResult {
                package: "b".into(),
                upstream: None,
                fedora_rawhide: None,
                fedora_stable: None,
                centos_stream: None,
                hs9: None,
                hs10: None,
                issue: Some(IssueRef {
                    iid: 3,
                    url: "u".into(),
                    status: "closed".into(),
                    assignees: vec![],
                }),
                ref_version: None,
            },
        ];
        let mut buf = Vec::new();
        write_json_array(&results, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&output).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["package"], "a");
        assert!(arr[0].get("issue").is_none());
        assert_eq!(arr[1]["issue"]["iid"], 3);
        assert_eq!(arr[1]["issue"]["status"], "closed");
    }

    #[test]
    fn test_json_serialization_without_tracking() {
        let result = CheckResult {
            package: "pkg".into(),
            upstream: None,
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: Some(HyperscaleResult {
                summary: HyperscaleSummary {
                    release: Some(make_build("1.0", "pkg-1.0-1.hs.el9")),
                    testing: None,
                },
                newest_version: None,
            }),
            hs10: None,
            issue: None,
            ref_version: None,
        };
        let json = serde_json::to_value(&result).unwrap();
        // newest_version should be absent when None
        assert!(json["hs9"].get("newest_version").is_none());
    }

    #[test]
    fn test_json_serialization_with_issue() {
        let result = CheckResult {
            package: "pkg".into(),
            upstream: Some("2.0".into()),
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: None,
            hs10: None,
            issue: Some(IssueRef {
                iid: 42,
                url: "https://gitlab.com/test/pkg/-/issues/42"
                    .into(),
                status: "opened".into(),
                assignees: vec!["alice".into()],
            }),
            ref_version: None,
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["issue"]["iid"], 42);
        assert_eq!(
            json["issue"]["url"],
            "https://gitlab.com/test/pkg/-/issues/42"
        );
        assert_eq!(json["issue"]["status"], "opened");
        assert_eq!(json["issue"]["assignees"][0], "alice");
    }

    #[test]
    fn test_json_serialization_issue_no_assignees() {
        let issue_ref = IssueRef {
            iid: 1,
            url: "u".into(),
            status: "closed".into(),
            assignees: vec![],
        };
        let json = serde_json::to_value(&issue_ref).unwrap();
        assert_eq!(json["status"], "closed");
        // empty assignees should be absent
        assert!(json.get("assignees").is_none());
    }

    #[test]
    fn test_json_array_serialization() {
        let results = vec![
            CheckResult {
                package: "a".into(),
                upstream: Some("1.0".into()),
                fedora_rawhide: None,
                fedora_stable: None,
                centos_stream: None,
                hs9: None,
                hs10: None,
                issue: None,
                ref_version: None,
            },
            CheckResult {
                package: "b".into(),
                upstream: Some("2.0".into()),
                fedora_rawhide: None,
                fedora_stable: None,
                centos_stream: None,
                hs9: None,
                hs10: None,
                issue: None,
                ref_version: None,
            },
        ];
        let json = serde_json::to_value(&results).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["package"], "a");
        assert_eq!(arr[1]["package"], "b");
    }

    #[test]
    fn test_issue_ref_from_gitlab_issue_with_status() {
        use crate::gitlab;
        let issue = gitlab::Issue {
            iid: 7,
            title: "t".into(),
            description: None,
            state: "opened".into(),
            web_url: "https://example.com/issues/7".into(),
            assignees: vec![
                gitlab::Assignee {
                    username: "alice".into(),
                },
                gitlab::Assignee {
                    username: "bob".into(),
                },
            ],
        };
        let r = IssueRef::from_gitlab_issue(
            &issue,
            Some("To do".into()),
        );
        assert_eq!(r.iid, 7);
        assert_eq!(r.url, "https://example.com/issues/7");
        assert_eq!(r.status, "To do");
        assert_eq!(r.assignees, vec!["alice", "bob"]);
    }

    #[test]
    fn test_issue_ref_from_gitlab_issue_no_status() {
        use crate::gitlab;
        let issue = gitlab::Issue {
            iid: 1,
            title: "t".into(),
            description: None,
            state: "closed".into(),
            web_url: "u".into(),
            assignees: vec![],
        };
        let r = IssueRef::from_gitlab_issue(&issue, None);
        assert_eq!(r.status, "closed");
        assert!(r.assignees.is_empty());
    }

    fn make_result_with_issue(
        issue: Option<IssueRef>,
    ) -> CheckResult {
        CheckResult {
            package: "pkg".into(),
            upstream: None,
            fedora_rawhide: None,
            fedora_stable: None,
            centos_stream: None,
            hs9: None,
            hs10: None,
            issue,
            ref_version: None,
        }
    }

    #[test]
    fn test_matches_issue_filter_no_issue() {
        let r = make_result_with_issue(None);
        assert!(!r.matches_issue_filter(None, None));
        assert!(!r.matches_issue_filter(
            Some("opened"),
            None,
        ));
    }

    #[test]
    fn test_matches_issue_filter_status() {
        let r = make_result_with_issue(Some(IssueRef {
            iid: 1,
            url: "u".into(),
            status: "opened".into(),
            assignees: vec![],
        }));
        assert!(r.matches_issue_filter(
            Some("opened"),
            None,
        ));
        assert!(!r.matches_issue_filter(
            Some("closed"),
            None,
        ));
    }

    #[test]
    fn test_matches_issue_filter_assignee() {
        let r = make_result_with_issue(Some(IssueRef {
            iid: 1,
            url: "u".into(),
            status: "opened".into(),
            assignees: vec![
                "alice".into(),
                "bob".into(),
            ],
        }));
        assert!(r.matches_issue_filter(
            None,
            Some("alice"),
        ));
        assert!(r.matches_issue_filter(
            None,
            Some("bob"),
        ));
        assert!(!r.matches_issue_filter(
            None,
            Some("eve"),
        ));
    }

    #[test]
    fn test_matches_issue_filter_both() {
        let r = make_result_with_issue(Some(IssueRef {
            iid: 1,
            url: "u".into(),
            status: "opened".into(),
            assignees: vec!["alice".into()],
        }));
        assert!(r.matches_issue_filter(
            Some("opened"),
            Some("alice"),
        ));
        assert!(!r.matches_issue_filter(
            Some("closed"),
            Some("alice"),
        ));
        assert!(!r.matches_issue_filter(
            Some("opened"),
            Some("bob"),
        ));
    }

    #[test]
    fn test_matches_issue_filter_no_filters() {
        let r = make_result_with_issue(Some(IssueRef {
            iid: 1,
            url: "u".into(),
            status: "opened".into(),
            assignees: vec![],
        }));
        assert!(r.matches_issue_filter(None, None));
    }
}
