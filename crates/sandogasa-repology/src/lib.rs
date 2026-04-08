// SPDX-License-Identifier: MPL-2.0

//! Rust client for the [Repology](https://repology.org/) package version
//! tracking API.

use serde::Deserialize;

/// Package version status as reported by Repology.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Newest,
    Devel,
    Unique,
    Outdated,
    Legacy,
    Rolling,
    Noscheme,
    Incorrect,
    Untrusted,
    Ignored,
}

/// A single package entry from the Repology API.
///
/// Only `repo` and `version` are guaranteed to be present.
#[derive(Debug, Clone, Deserialize)]
pub struct Package {
    pub repo: String,
    pub version: String,
    #[serde(default)]
    pub subrepo: Option<String>,
    #[serde(default)]
    pub srcname: Option<String>,
    #[serde(default)]
    pub binname: Option<String>,
    #[serde(default)]
    pub binnames: Option<Vec<String>>,
    #[serde(default)]
    pub visiblename: Option<String>,
    #[serde(default)]
    pub origversion: Option<String>,
    #[serde(default)]
    pub status: Option<Status>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub categories: Option<Vec<String>>,
    #[serde(default)]
    pub licenses: Option<Vec<String>>,
    #[serde(default)]
    pub maintainers: Option<Vec<String>>,
}

/// Client for the Repology API.
pub struct Client {
    http: reqwest::blocking::Client,
    base_url: String,
}

impl Default for Client {
    fn default() -> Self {
        Self::with_base_url("https://repology.org/api/v1")
    }
}

impl Client {
    /// Create a new client using the default Repology API URL.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a client with a custom base URL (useful for testing).
    pub fn with_base_url(base_url: &str) -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent("sandogasa-repology/0.6.2")
            .build()
            .expect("failed to build HTTP client");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Fetch all package entries for a given project name.
    pub fn get_project(&self, name: &str) -> Result<Vec<Package>, Box<dyn std::error::Error>> {
        let url = format!("{}/project/{}", self.base_url, name);
        let packages = self.http.get(&url).send()?.json::<Vec<Package>>()?;
        Ok(packages)
    }
}

/// Return packages whose `repo` field matches the given name exactly.
pub fn filter_by_repo<'a>(packages: &'a [Package], repo: &str) -> Vec<&'a Package> {
    packages.iter().filter(|p| p.repo == repo).collect()
}

/// Find the first package with `status == Newest`.
pub fn find_newest(packages: &[Package]) -> Option<&Package> {
    packages
        .iter()
        .find(|p| p.status.as_ref() == Some(&Status::Newest))
}

/// Find the latest entry for a specific repo.
///
/// When a Repology project contains multiple source packages, picks the
/// best entry by status priority (newest > outdated > legacy), breaking
/// ties with version comparison.
pub fn latest_for_repo<'a>(packages: &'a [Package], repo: &str) -> Option<&'a Package> {
    let matches = filter_by_repo(packages, repo);
    matches
        .iter()
        .max_by(|a, b| {
            status_priority(&a.status)
                .cmp(&status_priority(&b.status))
                .then_with(|| sandogasa_rpmvercmp::rpmvercmp(&a.version, &b.version))
        })
        .copied()
}

/// Find the package from the latest stable Fedora release.
///
/// Looks for `fedora_NN` repos (excluding `fedora_rawhide`), picks the
/// highest release number, and prefers the "updates" subrepo.
pub fn latest_fedora_stable(packages: &[Package]) -> Option<&Package> {
    let max_release = packages.iter().filter_map(fedora_release_number).max()?;

    let repo = format!("fedora_{}", max_release);
    latest_for_repo(packages, &repo)
}

/// Find the package from the latest CentOS Stream release.
///
/// Looks for `centos_stream_NN` repos, picks the highest release number,
/// then returns the entry with the highest version (non-legacy).
pub fn latest_centos_stream(packages: &[Package]) -> Option<&Package> {
    let max_release = packages
        .iter()
        .filter_map(centos_stream_release_number)
        .max()?;

    let repo = format!("centos_stream_{}", max_release);
    let matches = filter_by_repo(packages, &repo);
    matches
        .iter()
        .max_by(|a, b| {
            status_priority(&a.status)
                .cmp(&status_priority(&b.status))
                .then_with(|| sandogasa_rpmvercmp::rpmvercmp(&a.version, &b.version))
        })
        .copied()
}

/// Ranking for Repology status values (higher = more preferred).
fn status_priority(status: &Option<Status>) -> u8 {
    match status.as_ref() {
        Some(Status::Newest) => 6,
        Some(Status::Devel) => 5,
        Some(Status::Unique) => 4,
        Some(Status::Rolling) => 3,
        Some(Status::Outdated) | Some(Status::Incorrect) => 2,
        Some(Status::Legacy) => 0,
        _ => 1,
    }
}

/// Extract the numeric release from a `fedora_NN` repo name.
fn fedora_release_number(package: &Package) -> Option<u32> {
    package
        .repo
        .strip_prefix("fedora_")
        .and_then(|s| s.parse::<u32>().ok())
}

/// Extract the numeric release from a `centos_stream_NN` repo name.
fn centos_stream_release_number(package: &Package) -> Option<u32> {
    package
        .repo
        .strip_prefix("centos_stream_")
        .and_then(|s| s.parse::<u32>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_packages() -> Vec<Package> {
        let json = include_str!("../tests/fixtures/ethtool.json");
        serde_json::from_str(json).expect("failed to parse fixture")
    }

    #[test]
    fn deserialize_fixture() {
        let packages = fixture_packages();
        assert_eq!(packages.len(), 14);

        let arch = &packages[0];
        assert_eq!(arch.repo, "arch");
        assert_eq!(arch.version, "6.19");
        assert_eq!(arch.status, Some(Status::Newest));
        assert_eq!(arch.origversion.as_deref(), Some("2:6.19-1"));
    }

    #[test]
    fn deserialize_all_status_values() {
        let cases = [
            ("newest", Status::Newest),
            ("devel", Status::Devel),
            ("unique", Status::Unique),
            ("outdated", Status::Outdated),
            ("legacy", Status::Legacy),
            ("rolling", Status::Rolling),
            ("noscheme", Status::Noscheme),
            ("incorrect", Status::Incorrect),
            ("untrusted", Status::Untrusted),
            ("ignored", Status::Ignored),
        ];
        for (input, expected) in cases {
            let json = format!(r#"{{"repo":"test","version":"1","status":"{}"}}"#, input);
            let pkg: Package = serde_json::from_str(&json).unwrap();
            assert_eq!(pkg.status, Some(expected));
        }
    }

    #[test]
    fn deserialize_minimal_package() {
        let json = r#"{"repo":"test","version":"1.0"}"#;
        let pkg: Package = serde_json::from_str(json).unwrap();
        assert_eq!(pkg.repo, "test");
        assert_eq!(pkg.version, "1.0");
        assert!(pkg.status.is_none());
        assert!(pkg.subrepo.is_none());
        assert!(pkg.srcname.is_none());
    }

    #[test]
    fn test_filter_by_repo() {
        let packages = fixture_packages();
        let fedora_43 = filter_by_repo(&packages, "fedora_43");
        assert_eq!(fedora_43.len(), 2);
        assert!(fedora_43.iter().all(|p| p.repo == "fedora_43"));
    }

    #[test]
    fn test_filter_by_repo_no_match() {
        let packages = fixture_packages();
        let result = filter_by_repo(&packages, "nonexistent");
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_newest() {
        let packages = fixture_packages();
        let newest = find_newest(&packages).unwrap();
        assert_eq!(newest.status, Some(Status::Newest));
        assert_eq!(newest.version, "6.19");
    }

    #[test]
    fn test_find_newest_none() {
        let packages: Vec<Package> = vec![
            serde_json::from_str(r#"{"repo":"a","version":"1","status":"outdated"}"#).unwrap(),
            serde_json::from_str(r#"{"repo":"b","version":"2","status":"legacy"}"#).unwrap(),
        ];
        assert!(find_newest(&packages).is_none());
    }

    #[test]
    fn test_latest_for_repo_prefers_updates() {
        let packages = fixture_packages();
        let pkg = latest_for_repo(&packages, "fedora_43").unwrap();
        assert_eq!(pkg.subrepo.as_deref(), Some("updates"));
        assert_eq!(pkg.version, "6.19");
    }

    #[test]
    fn test_latest_for_repo_single_entry() {
        let packages = fixture_packages();
        let pkg = latest_for_repo(&packages, "fedora_rawhide").unwrap();
        assert_eq!(pkg.repo, "fedora_rawhide");
        assert_eq!(pkg.version, "6.19");
    }

    #[test]
    fn test_latest_for_repo_no_match() {
        let packages = fixture_packages();
        assert!(latest_for_repo(&packages, "nonexistent").is_none());
    }

    #[test]
    fn test_latest_for_repo_prefers_newest_status() {
        let packages: Vec<Package> = vec![
            serde_json::from_str(
                r#"{"repo":"fedora_rawhide","version":"5.7.9","status":"outdated","srcname":"usbip"}"#,
            )
            .unwrap(),
            serde_json::from_str(
                r#"{"repo":"fedora_rawhide","version":"7.0.0","status":"incorrect","srcname":"kernel"}"#,
            )
            .unwrap(),
            serde_json::from_str(
                r#"{"repo":"fedora_rawhide","version":"6.19","status":"newest","srcname":"kernel"}"#,
            )
            .unwrap(),
        ];
        let pkg = latest_for_repo(&packages, "fedora_rawhide").unwrap();
        assert_eq!(pkg.version, "6.19");
    }

    #[test]
    fn test_latest_for_repo_picks_highest_version_on_same_status() {
        let packages: Vec<Package> = vec![
            serde_json::from_str(
                r#"{"repo":"fedora_rawhide","version":"5.7.9","status":"outdated","srcname":"usbip"}"#,
            )
            .unwrap(),
            serde_json::from_str(
                r#"{"repo":"fedora_rawhide","version":"7.0.0","status":"outdated","srcname":"kernel"}"#,
            )
            .unwrap(),
        ];
        let pkg = latest_for_repo(&packages, "fedora_rawhide").unwrap();
        assert_eq!(pkg.version, "7.0.0");
    }

    #[test]
    fn test_latest_fedora_stable() {
        let packages = fixture_packages();
        let pkg = latest_fedora_stable(&packages).unwrap();
        assert_eq!(pkg.repo, "fedora_43");
        assert_eq!(pkg.subrepo.as_deref(), Some("updates"));
        assert_eq!(pkg.version, "6.19");
    }

    #[test]
    fn test_latest_fedora_stable_no_fedora() {
        let packages: Vec<Package> = vec![
            serde_json::from_str(r#"{"repo":"arch","version":"1","status":"newest"}"#).unwrap(),
            serde_json::from_str(r#"{"repo":"debian_13","version":"2","status":"outdated"}"#)
                .unwrap(),
        ];
        assert!(latest_fedora_stable(&packages).is_none());
    }

    #[test]
    fn test_fedora_release_number() {
        let pkg: Package = serde_json::from_str(r#"{"repo":"fedora_43","version":"1"}"#).unwrap();
        assert_eq!(fedora_release_number(&pkg), Some(43));

        let rawhide: Package =
            serde_json::from_str(r#"{"repo":"fedora_rawhide","version":"1"}"#).unwrap();
        assert_eq!(fedora_release_number(&rawhide), None);

        let other: Package = serde_json::from_str(r#"{"repo":"arch","version":"1"}"#).unwrap();
        assert_eq!(fedora_release_number(&other), None);
    }

    #[test]
    fn test_latest_centos_stream() {
        let packages = fixture_packages();
        let pkg = latest_centos_stream(&packages).unwrap();
        assert_eq!(pkg.repo, "centos_stream_10");
        assert_eq!(pkg.version, "6.15");
        assert_eq!(pkg.status, Some(Status::Outdated));
    }

    #[test]
    fn test_latest_centos_stream_no_centos() {
        let packages: Vec<Package> = vec![
            serde_json::from_str(r#"{"repo":"arch","version":"1","status":"newest"}"#).unwrap(),
            serde_json::from_str(r#"{"repo":"fedora_43","version":"2","status":"outdated"}"#)
                .unwrap(),
        ];
        assert!(latest_centos_stream(&packages).is_none());
    }

    #[test]
    fn test_centos_stream_release_number() {
        let pkg: Package =
            serde_json::from_str(r#"{"repo":"centos_stream_10","version":"1"}"#).unwrap();
        assert_eq!(centos_stream_release_number(&pkg), Some(10));

        let old: Package = serde_json::from_str(r#"{"repo":"centos_8","version":"1"}"#).unwrap();
        assert_eq!(centos_stream_release_number(&old), None);

        let other: Package = serde_json::from_str(r#"{"repo":"fedora_43","version":"1"}"#).unwrap();
        assert_eq!(centos_stream_release_number(&other), None);
    }

    #[test]
    fn test_status_priority_ordering() {
        assert!(status_priority(&Some(Status::Newest)) > status_priority(&Some(Status::Outdated)));
        assert!(status_priority(&Some(Status::Outdated)) > status_priority(&Some(Status::Legacy)));
        assert!(
            status_priority(&Some(Status::Outdated)) == status_priority(&Some(Status::Incorrect))
        );
        assert!(status_priority(&Some(Status::Devel)) > status_priority(&Some(Status::Outdated)));
    }

    #[test]
    fn test_client_new() {
        let client = Client::new();
        assert_eq!(client.base_url, "https://repology.org/api/v1");
    }

    #[test]
    fn test_client_with_base_url_trims_slash() {
        let client = Client::with_base_url("https://example.com/api/");
        assert_eq!(client.base_url, "https://example.com/api");
    }
}
