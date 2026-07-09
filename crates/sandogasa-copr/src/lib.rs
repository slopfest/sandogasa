// SPDX-License-Identifier: Apache-2.0 OR MIT

//! HTTP client for [COPR](https://copr.fedorainfracloud.org/)'s
//! public `api_3` — the read-only slice `ebranch check-update` needs
//! to treat a staging COPR project as an update source.
//!
//! The [`Copr::monitor`] endpoint reports every package's latest
//! build per chroot (state + version), which yields the update's NVR
//! list for a chroot without authentication. The provides comparison
//! itself runs through fedrq's `@copr:` repo class, not this client.
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let copr = sandogasa_copr::Copr::new();
//! let packages = copr.monitor("@rust", "uutils-and-nushell")?;
//! let prefix = sandogasa_copr::chroot_prefix("epel9").unwrap();
//! for nvr in sandogasa_copr::nvrs_for_chroot(&packages, &prefix) {
//!     println!("{nvr}");
//! }
//! # Ok(())
//! # }
//! ```

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Deserialize;

/// The Fedora COPR instance.
pub const DEFAULT_BASE_URL: &str = "https://copr.fedorainfracloud.org";

/// Upper bound on any single COPR HTTP request — a hang-catcher
/// rather than a latency cap.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// A package's latest-build state in one chroot, from
/// `/api_3/monitor`.
#[derive(Debug, Clone, Deserialize)]
pub struct ChrootState {
    /// Build state, e.g. `succeeded`, `failed`, `running`.
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub build_id: Option<u64>,
    /// `V-R` of the latest build (the release usually lacks the
    /// `%{?dist}` expansion, e.g. `1.0.8-1`), possibly with an
    /// `E:` epoch prefix.
    #[serde(default)]
    pub pkg_version: Option<String>,
}

/// One package row from `/api_3/monitor`: the package name and its
/// latest build per chroot.
#[derive(Debug, Clone, Deserialize)]
pub struct PackageStatus {
    pub name: String,
    #[serde(default)]
    pub chroots: BTreeMap<String, ChrootState>,
}

#[derive(Debug, Deserialize)]
struct MonitorResponse {
    #[serde(default)]
    packages: Vec<PackageStatus>,
}

/// A COPR API client bound to one instance.
pub struct Copr {
    base_url: String,
    http: reqwest::blocking::Client,
}

impl Default for Copr {
    fn default() -> Self {
        Self::new()
    }
}

impl Copr {
    /// Client for the Fedora COPR instance.
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    /// Client for another instance (or a test server).
    pub fn with_base_url(base_url: &str) -> Self {
        sandogasa_cli::install_crypto_provider();
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("sandogasa-copr/", env!("CARGO_PKG_VERSION")))
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .expect("build reqwest client");
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
    }

    /// Every package's latest build per chroot for `owner/project`
    /// (`owner` keeps its `@` prefix for group projects). No
    /// authentication — the monitor endpoint is public.
    pub fn monitor(
        &self,
        owner: &str,
        project: &str,
    ) -> Result<Vec<PackageStatus>, Box<dyn std::error::Error>> {
        let url = format!("{}/api_3/monitor", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[("ownername", owner), ("projectname", project)])
            .send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("COPR monitor for {owner}/{project}: {status}: {text}").into());
        }
        let resp: MonitorResponse = resp.json()?;
        Ok(resp.packages)
    }
}

/// The COPR chroot-name prefix (trailing `-` included, so `epel-9-`
/// can't match an `epel-10-*` chroot) for a Fedora-ecosystem branch:
/// `f44` → `fedora-44-`, `rawhide` → `fedora-rawhide-`, `epel9` /
/// `epel10.3` → `epel-9-` / `epel-10-`, `c10s` → `centos-stream-10-`.
/// `None` for branches with no COPR chroot naming (e.g. `al9`).
pub fn chroot_prefix(branch: &str) -> Option<String> {
    if branch == "rawhide" {
        return Some("fedora-rawhide-".to_string());
    }
    if branch == "eln" {
        return Some("fedora-eln-".to_string());
    }
    if let Some(n) = branch.strip_prefix('f')
        && n.chars().all(|c| c.is_ascii_digit())
        && !n.is_empty()
    {
        return Some(format!("fedora-{n}-"));
    }
    if let Some(rest) = branch.strip_prefix("epel") {
        // epel10.3-style minor versions share the epel-10 chroots.
        let major: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !major.is_empty() {
            return Some(format!("epel-{major}-"));
        }
    }
    if let Some(n) = branch.strip_suffix('s').and_then(|b| b.strip_prefix('c'))
        && n.chars().all(|c| c.is_ascii_digit())
        && !n.is_empty()
    {
        return Some(format!("centos-stream-{n}-"));
    }
    None
}

/// NVR strings (`name-version-release`, the release as COPR reports
/// it — usually without the `%{?dist}` expansion) for every package
/// whose latest build **succeeded** in a chroot matching `prefix`.
/// Prefers the x86_64 chroot when several architectures match; any
/// epoch prefix on the version is dropped (NVRs never carry one).
pub fn nvrs_for_chroot(packages: &[PackageStatus], prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    for pkg in packages {
        let mut candidates: Vec<(&String, &ChrootState)> = pkg
            .chroots
            .iter()
            .filter(|(chroot, _)| chroot.starts_with(prefix))
            .collect();
        candidates.sort_by_key(|(chroot, _)| !chroot.ends_with("-x86_64"));
        let Some((_, state)) = candidates.first() else {
            continue;
        };
        if state.state != "succeeded" {
            continue;
        }
        let Some(version) = state.pkg_version.as_deref() else {
            continue;
        };
        let version = version.rsplit_once(':').map_or(version, |(_, v)| v);
        out.push(format!("{}-{version}", pkg.name));
    }
    out.sort();
    out
}

/// The distinct chroot names appearing across `packages` — used for
/// a helpful error when no chroot matches the requested branch.
pub fn available_chroots(packages: &[PackageStatus]) -> Vec<String> {
    let mut out: Vec<String> = packages
        .iter()
        .flat_map(|p| p.chroots.keys().cloned())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, chroots: &[(&str, &str, &str)]) -> PackageStatus {
        PackageStatus {
            name: name.to_string(),
            chroots: chroots
                .iter()
                .map(|(chroot, state, version)| {
                    (
                        chroot.to_string(),
                        ChrootState {
                            state: state.to_string(),
                            build_id: Some(1),
                            pkg_version: Some(version.to_string()),
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn chroot_prefix_maps_branches() {
        assert_eq!(chroot_prefix("rawhide").as_deref(), Some("fedora-rawhide-"));
        assert_eq!(chroot_prefix("f44").as_deref(), Some("fedora-44-"));
        assert_eq!(chroot_prefix("eln").as_deref(), Some("fedora-eln-"));
        assert_eq!(chroot_prefix("epel9").as_deref(), Some("epel-9-"));
        assert_eq!(chroot_prefix("epel10").as_deref(), Some("epel-10-"));
        // Minor EPEL versions share the major chroots.
        assert_eq!(chroot_prefix("epel10.3").as_deref(), Some("epel-10-"));
        assert_eq!(chroot_prefix("c10s").as_deref(), Some("centos-stream-10-"));
        // Base branches / unknowns have no COPR chroots.
        assert_eq!(chroot_prefix("al9"), None);
        assert_eq!(chroot_prefix("fedora"), None);
        assert_eq!(chroot_prefix("epel"), None);
    }

    #[test]
    fn nvrs_for_chroot_filters_and_prefers_x86_64() {
        let packages = vec![
            package(
                "rust-ctor",
                &[
                    ("fedora-rawhide-aarch64", "succeeded", "1.0.9-1"),
                    ("fedora-rawhide-x86_64", "succeeded", "1.0.8-1"),
                    ("epel-9-x86_64", "succeeded", "1.0.7-1"),
                ],
            ),
            // Latest build failed → excluded.
            package(
                "nushell",
                &[("fedora-rawhide-x86_64", "failed", "0.106.0-1")],
            ),
            // No matching chroot → excluded.
            package("uutils", &[("epel-9-x86_64", "succeeded", "0.2.2-1")]),
        ];
        assert_eq!(
            nvrs_for_chroot(&packages, "fedora-rawhide-"),
            // x86_64 wins over the (alphabetically earlier) aarch64.
            vec!["rust-ctor-1.0.8-1".to_string()]
        );
        // The epel-9- prefix can't accidentally match epel-10 chroots
        // (trailing dash), and epochs are stripped.
        let epoch = vec![package(
            "bat",
            &[("epel-9-x86_64", "succeeded", "1:0.24.0-1")],
        )];
        assert_eq!(
            nvrs_for_chroot(&epoch, "epel-9-"),
            vec!["bat-0.24.0-1".to_string()]
        );
    }

    #[test]
    fn available_chroots_dedups() {
        let packages = vec![
            package("a", &[("epel-9-x86_64", "succeeded", "1-1")]),
            package(
                "b",
                &[
                    ("epel-9-x86_64", "succeeded", "1-1"),
                    ("fedora-rawhide-x86_64", "succeeded", "1-1"),
                ],
            ),
        ];
        assert_eq!(
            available_chroots(&packages),
            vec![
                "epel-9-x86_64".to_string(),
                "fedora-rawhide-x86_64".to_string()
            ]
        );
    }

    #[test]
    fn monitor_fetches_and_parses() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api_3/monitor")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("ownername".into(), "@rust".into()),
                mockito::Matcher::UrlEncoded("projectname".into(), "uutils-and-nushell".into()),
            ]))
            .with_status(200)
            .with_body(
                r#"{"output": "ok", "message": "Project monitor request successful",
                    "packages": [
                      {"name": "rust-ctor",
                       "chroots": {
                         "fedora-rawhide-x86_64": {"state": "succeeded", "status": 1,
                                                   "build_id": 10702530,
                                                   "pkg_version": "1.0.8-1"}}}]}"#,
            )
            .create();
        let copr = Copr::with_base_url(&server.url());
        let packages = copr.monitor("@rust", "uutils-and-nushell").unwrap();
        mock.assert();
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "rust-ctor");
        assert_eq!(
            packages[0].chroots["fedora-rawhide-x86_64"]
                .pkg_version
                .as_deref(),
            Some("1.0.8-1")
        );

        // An unknown project surfaces COPR's error body.
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/api_3/monitor")
            .match_query(mockito::Matcher::Any)
            .with_status(404)
            .with_body(r#"{"error": "Project nobody/nothing does not exist"}"#)
            .create();
        let copr = Copr::with_base_url(&server.url());
        let err = copr.monitor("nobody", "nothing").unwrap_err().to_string();
        assert!(err.contains("nobody/nothing"), "{err}");
        assert!(err.contains("does not exist"), "{err}");
    }
}
