// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What `cbspkg promote` checks before a build moves (#157), through
//! sandogasa-depcheck, the check ebranch's `check-update` runs: the
//! build against its collection's newest in `-release`, looked up in
//! what the collection's users have together (the stock distribution,
//! EPEL and the collection's published `-release`), reported the same
//! way.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use sandogasa_cbs::Collection;
use sandogasa_depcheck::RepoQuery;
use sandogasa_depcheck::analysis::Analysis;
use sandogasa_depcheck::render::{Findings, Options, findings};
use sandogasa_depcheck::report::{BreakKind, ChangedProvide, PackageChange};

/// Where a check looks: what a collection's users have, and which
/// source packages are the collection's own.
pub trait Places: Sync {
    /// The stock distribution, EPEL and the collection's published
    /// `-release`, as one repository.
    fn stable(&self, collection: &Collection) -> Result<Box<dyn RepoQuery + Send + Sync>, String>;

    /// The source packages in the collection's published `-release`:
    /// the only ones a build can fail to build (FTBFS), as stock
    /// packages do not build against a SIG's repositories (and many
    /// do not build from Stream's public repositories at all).
    fn own_sources(&self, collection: &Collection) -> Result<BTreeSet<String>, String>;

    /// Whether the collection's `-release` is on the mirror: a new
    /// collection's is not until its first release, and is then left
    /// out of where the check looks.
    fn published(&self, _collection: &Collection) -> bool {
        true
    }

    /// The places, for the report: "CentOS Stream 10, EPEL 10 and
    /// hyperscale10s-packages-main-release".
    fn describe(&self, collection: &Collection) -> String {
        let stock = if collection.stream {
            format!("CentOS Stream {}", collection.major)
        } else {
            format!("AlmaLinux {} (for RHEL)", collection.major)
        };
        let release = collection.tag(sandogasa_cbs::Stage::Release);
        if self.published(collection) {
            format!("{stock}, EPEL {} and {release}", collection.major)
        } else {
            format!(
                "{stock} and EPEL {} ({release} is not on the mirror yet)",
                collection.major
            )
        }
    }
}

/// The places, through fedrq.
pub struct FedrqPlaces {
    installed: bool,
    http: reqwest::blocking::Client,
    /// Per collection's mirror URL, whether it is published; asked once.
    published: Mutex<BTreeMap<String, bool>>,
}

/// One build's check.
#[derive(Debug, Clone)]
pub struct BuildCheck {
    pub nvr: String,
    pub package: String,
    /// The build compared with: the collection's newest in `-release`,
    /// `None` when it has none yet.
    pub against: Option<String>,
    /// Where its users were looked up.
    pub looked_in: String,
    pub changed: Vec<ChangedProvide>,
    /// What the check found, or why it could not run.
    pub result: Result<Analysis, String>,
}

/// Keep FTBFS only for the collection's own packages, `own`: a stock
/// package does not build against the SIG's repositories. A reverse
/// dependency left with nothing broken is fine.
pub fn own_ftbfs_only(analysis: &mut Analysis, own: &BTreeSet<String>) {
    for (pkg, result) in analysis.reverse_deps.iter_mut() {
        if !own.contains(pkg) {
            result.issues.retain(|i| i.kind != BreakKind::Ftbfs);
        }
        if result.issues.is_empty() {
            result.status = "ok".to_string();
        }
    }
}

impl FedrqPlaces {
    pub fn new() -> Self {
        let installed =
            sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", Some("--version"))])
                .is_ok();
        let http = sandogasa_cli::http::blocking_builder(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .unwrap_or_default();
        Self {
            installed,
            http,
            published: Mutex::default(),
        }
    }

    fn check_installed(&self) -> Result<(), String> {
        if self.installed {
            Ok(())
        } else {
            Err("fedrq is not installed (sudo dnf install fedrq)".into())
        }
    }
}

impl Places for FedrqPlaces {
    fn stable(&self, c: &Collection) -> Result<Box<dyn RepoQuery + Send + Sync>, String> {
        self.check_installed()?;
        let mut enabled = vec!["@epel".to_string()];
        if self.published(c) {
            enabled.push(format!("@baseurl:{}", c.published_repo()));
        }
        Ok(Box::new(
            sandogasa_fedrq::Fedrq::for_branch(c.stock_branch()).with_enabled(enabled),
        ))
    }

    fn own_sources(&self, c: &Collection) -> Result<BTreeSet<String>, String> {
        self.check_installed()?;
        if !self.published(c) {
            return Ok(BTreeSet::new());
        }
        let repo = sandogasa_fedrq::Fedrq::default()
            .with_repo(Some(format!("@baseurl:{}", c.published_repo())));
        Ok(repo
            .binaries_info()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter_map(|p| p.source_name)
            .collect())
    }

    fn published(&self, c: &Collection) -> bool {
        let url = c.published_repo();
        let mut known = self.published.lock().unwrap_or_else(|e| e.into_inner());
        *known
            .entry(url.clone())
            .or_insert_with(|| is_published(&self.http, &url))
    }
}

impl BuildCheck {
    /// Whether promoting should not be the default: something would
    /// break, a Requires of the build does not resolve, or the check
    /// could not run.
    pub fn breaks(&self) -> bool {
        match &self.result {
            Err(_) => true,
            Ok(a) => {
                !a.installability_issues.is_empty()
                    || a.reverse_deps.values().any(|r| r.status != "ok")
            }
        }
    }

    /// The check in Markdown, as a comment carries it and the plan
    /// shows it (through `to_terminal`): what it was compared with and
    /// where, the shared report's findings, and every version change
    /// collapsed.
    pub fn markdown(&self) -> String {
        let mut out = format!("**Checked `{}`** ", self.nvr);
        match &self.against {
            Some(a) => out.push_str(&format!("against `{a}`, the newest in `-release`, ")),
            None => out.push_str("(nothing in `-release` to compare it with yet), "),
        }
        out.push_str(&format!("in {}.\n\n", self.looked_in));
        let analysis = match &self.result {
            Ok(a) => a,
            Err(why) => {
                out.push_str(&format!(
                    "Not checked for breakage: `{}`.\n",
                    sandogasa_cbs::check::one_line(why)
                ));
                return out;
            }
        };
        let changes = [PackageChange {
            package: self.package.clone(),
            old: self
                .against
                .as_deref()
                .map(|a| version_release(a).to_string()),
            new: version_release(&self.nvr).to_string(),
        }];
        let packages = [self.package.clone()];
        out.push_str(&findings(
            &Findings {
                updated_packages: &packages,
                changes: &changes,
                changed_provides: &self.changed,
                installability_issues: &analysis.installability_issues,
                reverse_deps: &analysis.reverse_deps,
            },
            &Options {
                detailed: false,
                more: "",
            },
            "",
        ));
        let updated: Vec<&ChangedProvide> =
            self.changed.iter().filter(|c| c.new.is_some()).collect();
        if !updated.is_empty() {
            let n = updated.len();
            out.push_str(&format!(
                "\n<details><summary>Changes {n} Provide{} to another version</summary>\n",
                if n == 1 { "" } else { "s" }
            ));
            for c in updated {
                out.push_str(&format!(
                    "\n- `{}` to `{}`",
                    c.old,
                    c.new.as_deref().unwrap_or_default()
                ));
            }
            out.push_str("\n\n</details>\n");
        }
        // One line break at the end, however the findings end, so the
        // plan and the comment add their own spacing.
        format!("{}\n", out.trim_end())
    }
}

/// `version-release` of an NVR.
fn version_release(nvr: &str) -> &str {
    let mut parts = nvr.rsplitn(3, '-');
    let release = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    let len = version.len() + 1 + release.len();
    nvr.get(nvr.len().saturating_sub(len)..).unwrap_or(nvr)
}

/// Whether the repository at `url` (with fedrq's `$basearch`) is there,
/// for this machine's architecture: its `repodata/repomd.xml` answers.
fn is_published(http: &reqwest::blocking::Client, url: &str) -> bool {
    let repomd = format!(
        "{}repodata/repomd.xml",
        url.replace("$basearch", std::env::consts::ARCH)
    );
    http.head(repomd)
        .send()
        .is_ok_and(|r| r.status().is_success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandogasa_depcheck::report::{BrokenRequires, RevDepResult, UnsatisfiedDep};
    use std::collections::BTreeMap;

    fn broken(kind: BreakKind) -> RevDepResult {
        RevDepResult {
            status: "broken".into(),
            issues: vec![BrokenRequires {
                dep: "libfoo.so.1()(64bit)".into(),
                changed_provide: "libfoo.so.1()(64bit)".into(),
                kind,
            }],
            arrival: None,
        }
    }

    #[test]
    fn ftbfs_counts_only_for_the_sigs_own_packages() {
        let mut a = Analysis {
            installability_issues: vec![],
            reverse_deps: BTreeMap::from([
                ("stock-pkg".to_string(), broken(BreakKind::Ftbfs)),
                ("sig-pkg".to_string(), broken(BreakKind::Ftbfs)),
                ("stock-user".to_string(), broken(BreakKind::Fti)),
            ]),
        };
        own_ftbfs_only(&mut a, &BTreeSet::from(["sig-pkg".to_string()]));
        assert_eq!(a.reverse_deps["stock-pkg"].status, "ok");
        assert!(a.reverse_deps["stock-pkg"].issues.is_empty());
        assert_eq!(a.reverse_deps["sig-pkg"].status, "broken");
        assert_eq!(a.reverse_deps["stock-user"].status, "broken");
    }

    fn check(result: Result<Analysis, String>) -> BuildCheck {
        BuildCheck {
            nvr: "foo-2.0-1.hs.el10".into(),
            package: "foo".into(),
            against: Some("foo-1.0-1.hs.el10".into()),
            looked_in: "CentOS Stream 10, EPEL 10 and hs-release".into(),
            changed: vec![
                ChangedProvide {
                    old: "libfoo.so.1()(64bit)".into(),
                    new: None,
                },
                ChangedProvide {
                    old: "foo = 1.0-1.hs.el10".into(),
                    new: Some("foo = 2.0-1.hs.el10".into()),
                },
            ],
            result,
        }
    }

    #[test]
    fn a_check_reads_as_the_shared_report() {
        let clean = check(Ok(Analysis::default()));
        assert!(!clean.breaks());
        let md = clean.markdown();
        assert!(
            md.starts_with(
                "**Checked `foo-2.0-1.hs.el10`** against `foo-1.0-1.hs.el10`, the newest in \
                 `-release`, in CentOS Stream 10, EPEL 10 and hs-release.\n\n## Analysis\n"
            ),
            "{md}"
        );
        assert!(md.contains("## Removed Provides (1)"), "{md}");
        assert!(md.contains("<summary>Changes 1 Provide to another version</summary>"));
        assert!(md.contains("- `foo = 1.0-1.hs.el10` to `foo = 2.0-1.hs.el10`"));

        let breaking = check(Ok(Analysis {
            installability_issues: vec![],
            reverse_deps: BTreeMap::from([("user".to_string(), broken(BreakKind::Fti))]),
        }));
        assert!(breaking.breaks());
        assert!(
            breaking
                .markdown()
                .contains("## Reverse dependencies that would break (1)")
        );

        let uninstallable = check(Ok(Analysis {
            installability_issues: vec![UnsatisfiedDep {
                package: "foo".into(),
                dep: "libbar.so.2()(64bit)".into(),
                unresolved: vec![],
            }],
            reverse_deps: BTreeMap::new(),
        }));
        assert!(uninstallable.breaks());

        let failed = check(Err("fedrq is not\ninstalled".into()));
        assert!(failed.breaks());
        assert!(
            failed
                .markdown()
                .ends_with("Not checked for breakage: `fedrq is not installed`.\n")
        );

        let mut first = check(Ok(Analysis::default()));
        first.against = None;
        first.changed.clear();
        let md = first.markdown();
        assert!(
            md.contains("(nothing in `-release` to compare it with yet)"),
            "{md}"
        );
        assert!(!md.contains("<details>"), "{md}");
    }

    #[test]
    fn places_are_described_for_the_report() {
        struct P;
        impl Places for P {
            fn stable(&self, _: &Collection) -> Result<Box<dyn RepoQuery + Send + Sync>, String> {
                Err("unused".into())
            }
            fn own_sources(&self, _: &Collection) -> Result<BTreeSet<String>, String> {
                Ok(BTreeSet::new())
            }
        }
        assert_eq!(
            P.describe(&Collection::new("hyperscale", 10, true, "main")),
            "CentOS Stream 10, EPEL 10 and hyperscale10s-packages-main-release"
        );
        assert_eq!(
            P.describe(&Collection::new("hyperscale", 9, false, "facebook")),
            "AlmaLinux 9 (for RHEL), EPEL 9 and hyperscale9-packages-facebook-release"
        );
        assert_eq!(version_release("foo-bar-2.0-1.el10"), "2.0-1.el10");

        struct Unpublished;
        impl Places for Unpublished {
            fn stable(&self, _: &Collection) -> Result<Box<dyn RepoQuery + Send + Sync>, String> {
                Err("unused".into())
            }
            fn own_sources(&self, _: &Collection) -> Result<BTreeSet<String>, String> {
                Ok(BTreeSet::new())
            }
            fn published(&self, _: &Collection) -> bool {
                false
            }
        }
        assert_eq!(
            Unpublished.describe(&Collection::new("hyperscale", 10, true, "main")),
            "CentOS Stream 10 and EPEL 10 (hyperscale10s-packages-main-release is not on the \
             mirror yet)"
        );
    }

    #[test]
    fn fedrq_places_ask_the_mirror_once_and_need_fedrq() {
        let c = Collection::new("hyperscale", 10, true, "new");
        let places = |installed| FedrqPlaces {
            installed,
            http: sandogasa_cli::http::blocking_builder("test")
                .build()
                .unwrap(),
            // Known already, so no request is made.
            published: Mutex::new(BTreeMap::from([(c.published_repo(), false)])),
        };
        let without = places(false);
        assert!(without.stable(&c).is_err());
        assert_eq!(
            without.own_sources(&c).unwrap_err(),
            "fedrq is not installed (sudo dnf install fedrq)"
        );
        // Not on the mirror: looked for nowhere, and no own packages.
        let with = places(true);
        assert!(!with.published(&c));
        assert!(with.stable(&c).is_ok());
        assert!(with.own_sources(&c).unwrap().is_empty());
        assert!(with.describe(&c).ends_with("is not on the mirror yet)"));
    }

    #[test]
    fn a_repository_is_published_when_its_repodata_answers() {
        let mut server = mockito::Server::new();
        let arch = std::env::consts::ARCH;
        server
            .mock(
                "HEAD",
                format!("/SIGs/10-stream/hs/{arch}/main/repodata/repomd.xml").as_str(),
            )
            .with_status(200)
            .create();
        server
            .mock("HEAD", mockito::Matcher::Any)
            .with_status(404)
            .create();
        let http = sandogasa_cli::http::blocking_builder("test")
            .build()
            .unwrap();
        let base = server.url();
        assert!(is_published(
            &http,
            &format!("{base}/SIGs/10-stream/hs/$basearch/main/")
        ));
        assert!(!is_published(
            &http,
            &format!("{base}/SIGs/10-stream/hs/$basearch/new/")
        ));
        assert!(!is_published(&http, "http://127.0.0.1:9/$basearch/"));
    }
}
