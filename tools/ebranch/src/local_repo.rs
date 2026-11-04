// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Local mock results as a layer over a resolver (`resolve
//! --local-repo`, issue #59).
//!
//! When no Fedora release has a package in a state that builds for the
//! target, the packager builds it locally with changes, and a closure
//! read from the source branch describes the wrong build. A
//! `mock --chain --localrepo` results directory holds the build that
//! will actually be branched: its SRPM carries the BuildRequires as
//! evaluated for the target, and its binaries the Provides other
//! packages will see. The packages stay in the closure (they still need
//! branching); only where their dependencies are read from changes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use sandogasa_fedrq::PkgInfo;

use crate::resolve::DepResolver;

/// What a local results directory says about the packages built in it.
pub(crate) struct LocalRepo {
    /// Source package → the BuildRequires of its chosen build's SRPM.
    pub buildrequires: BTreeMap<String, Vec<String>>,
    /// The binaries of the chosen builds.
    pub binaries: Vec<PkgInfo>,
}

/// A resolver that answers from local results for the packages built
/// there and from `inner` for everything else.
pub(crate) struct LocalOverlay<'a> {
    pub inner: &'a dyn DepResolver,
    pub local: LocalRepo,
}

impl LocalRepo {
    /// Read a results directory: each package's newest successful build
    /// (newest at all when none passed), its SRPM's BuildRequires with
    /// `rpm -qp`, and its binaries from the directory's repodata with
    /// fedrq.
    pub fn load(dir: &Path, verbose: bool) -> Result<Self, String> {
        sandogasa_cli::require_tools(&[("rpm", "sudo dnf install rpm", Some("--version"))])?;
        if !dir.join("repodata").is_dir() {
            return Err(format!(
                "{} has no repodata: mock --localrepo writes it after each build, \
                 or run createrepo_c on the directory",
                dir.display()
            ));
        }
        let chosen = chosen_builds(dir)?;
        let mut buildrequires = BTreeMap::new();
        for (name, (vr, build_dir)) in &chosen {
            let srpm = srpm_in(build_dir)
                .ok_or_else(|| format!("{name}-{vr}: no source RPM in {}", build_dir.display()))?;
            if verbose {
                eprintln!("[resolve] local build {name}-{vr}");
            }
            buildrequires.insert(name.clone(), srpm_buildrequires(&srpm)?);
        }
        let fedrq = sandogasa_fedrq::Fedrq {
            branch: None,
            repo: Some(format!("@baseurl:file://{}", dir.display())),
        };
        let binaries = fedrq
            .binaries_info()
            .map_err(|e| format!("reading {}: {e}", dir.display()))?
            .into_iter()
            .filter(|b| is_chosen(b, &chosen))
            .collect();
        Ok(Self {
            buildrequires,
            binaries,
        })
    }

    /// The source packages built here.
    pub fn packages(&self) -> BTreeSet<String> {
        self.buildrequires.keys().cloned().collect()
    }

    /// The local source packages whose binaries satisfy `dep`.
    fn providers(&self, dep: &str) -> Vec<String> {
        let found: BTreeSet<String> = self
            .binaries
            .iter()
            .filter(|b| b.satisfies(dep))
            .filter_map(|b| b.source_name.clone())
            .collect();
        found.into_iter().collect()
    }

    /// The runtime Requires of a local package's binaries.
    fn requires(&self, srpm: &str) -> Vec<String> {
        let all: BTreeSet<String> = self
            .binaries
            .iter()
            .filter(|b| b.source_name.as_deref() == Some(srpm))
            .flat_map(|b| b.requires.iter().cloned())
            .filter(|r| !r.starts_with("rpmlib("))
            .collect();
        all.into_iter().collect()
    }
}

impl DepResolver for LocalOverlay<'_> {
    fn validate(&self) -> Result<(), String> {
        self.inner.validate()
    }

    fn buildrequires(&self, srpm: &str) -> Result<Vec<String>, String> {
        match self.local.buildrequires.get(srpm) {
            Some(brs) => Ok(brs.clone()),
            None => self.inner.buildrequires(srpm),
        }
    }

    fn resolve_source(&self, dep: &str) -> Result<Vec<String>, String> {
        let local = self.local.providers(dep);
        if local.is_empty() {
            self.inner.resolve_source(dep)
        } else {
            Ok(local)
        }
    }

    fn resolve_target(&self, dep: &str) -> Result<Vec<String>, String> {
        self.inner.resolve_target(dep)
    }

    fn src_exists(&self, srpm: &str) -> Result<bool, String> {
        Ok(self.local.buildrequires.contains_key(srpm) || self.inner.src_exists(srpm)?)
    }

    fn subpkg_requires(&self, srpm: &str) -> Result<Vec<String>, String> {
        if self.local.buildrequires.contains_key(srpm) {
            Ok(self.local.requires(srpm))
        } else {
            self.inner.subpkg_requires(srpm)
        }
    }

    fn resolve_base_vr(&self, dep: &str) -> Result<Vec<(String, String)>, String> {
        self.inner.resolve_base_vr(dep)
    }

    fn base_src_exists(&self, srpm: &str) -> Result<bool, String> {
        self.inner.base_src_exists(srpm)
    }

    // The batched forms split each batch, so what is not local still
    // reaches the inner resolver as one batch rather than one query
    // per item.

    fn buildrequires_many(
        &self,
        srpms: &[String],
    ) -> BTreeMap<String, Result<Vec<String>, String>> {
        let (local, rest): (Vec<String>, Vec<String>) = srpms
            .iter()
            .cloned()
            .partition(|s| self.local.buildrequires.contains_key(s));
        let mut out = self.inner.buildrequires_many(&rest);
        for s in local {
            out.insert(s.clone(), Ok(self.local.buildrequires[&s].clone()));
        }
        out
    }

    fn resolve_source_many(
        &self,
        deps: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, String> {
        let mut out = BTreeMap::new();
        let mut rest = Vec::new();
        for dep in deps {
            let local = self.local.providers(dep);
            if local.is_empty() {
                rest.push(dep.clone());
            } else {
                out.insert(dep.clone(), local);
            }
        }
        out.extend(self.inner.resolve_source_many(&rest)?);
        Ok(out)
    }

    fn resolve_target_many(
        &self,
        deps: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, String> {
        self.inner.resolve_target_many(deps)
    }

    fn resolve_base_vr_many(
        &self,
        caps: &[String],
    ) -> Result<BTreeMap<String, Vec<(String, String)>>, String> {
        self.inner.resolve_base_vr_many(caps)
    }

    fn subpkg_requires_many(
        &self,
        srpms: &[String],
    ) -> BTreeMap<String, Result<Vec<String>, String>> {
        let (local, rest): (Vec<String>, Vec<String>) = srpms
            .iter()
            .cloned()
            .partition(|s| self.local.buildrequires.contains_key(s));
        let mut out = self.inner.subpkg_requires_many(&rest);
        for s in local {
            let requires = self.local.requires(&s);
            out.insert(s, Ok(requires));
        }
        out
    }
}

/// Package → the build to read: its newest successful one by RPM
/// version comparison, or its newest when none passed (a failed build
/// still states the BuildRequires it was attempted with). Directories
/// not named by an NVR, such as `repodata`, are skipped.
fn chosen_builds(dir: &Path) -> Result<BTreeMap<String, (String, PathBuf)>, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    let mut builds: BTreeMap<String, Vec<(String, bool, PathBuf)>> = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(nvr) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((name, vr)) = crate::rebuild_plan::split_nvr(nvr) else {
            continue;
        };
        if srpm_in(&path).is_none() {
            continue;
        }
        let passed = path.join("success").exists();
        builds
            .entry(name.to_string())
            .or_default()
            .push((vr.to_string(), passed, path.clone()));
    }
    Ok(builds
        .into_iter()
        .filter_map(|(name, mut list)| {
            // Successful builds sort after failed ones, then by version.
            list.sort_by(|a, b| {
                a.1.cmp(&b.1)
                    .then_with(|| sandogasa_rpmvercmp::compare_evr(&a.0, &b.0))
            });
            let (vr, _, path) = list.pop()?;
            Some((name, (vr, path)))
        })
        .collect())
}

/// The source RPM in a build directory.
fn srpm_in(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.to_str().is_some_and(|s| s.ends_with(".src.rpm")))
}

/// A source RPM's BuildRequires, from `rpm -qp --requires`, without
/// rpm's own `rpmlib(...)` features.
fn srpm_buildrequires(srpm: &Path) -> Result<Vec<String>, String> {
    let out = Command::new("rpm")
        .args(["-qp", "--requires"])
        .arg(srpm)
        .output()
        .map_err(|e| format!("could not run rpm: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "rpm -qp --requires {}: {}",
            srpm.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(parse_requires(&String::from_utf8_lossy(&out.stdout)))
}

/// `rpm --requires` output → the dependencies, minus `rpmlib(...)`.
fn parse_requires(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("rpmlib("))
        .map(str::to_string)
        .collect()
}

/// Whether a binary belongs to its source package's chosen build: the
/// repodata indexes every build's binaries, older ones included.
fn is_chosen(binary: &PkgInfo, chosen: &BTreeMap<String, (String, PathBuf)>) -> bool {
    let Some(source) = &binary.source_name else {
        return false;
    };
    match (chosen.get(source), binary.vr()) {
        (Some((vr, _)), Some(bvr)) => &bvr == vr,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn binary(name: &str, source: &str, vr: &str, provides: &[&str], requires: &[&str]) -> PkgInfo {
        let (version, release) = vr.split_once('-').unwrap();
        serde_json::from_value(serde_json::json!({
            "name": name,
            "source_name": source,
            "version": version,
            "release": release,
            "provides": provides,
            "requires": requires,
            "repoid": "local",
        }))
        .unwrap()
    }

    /// The source branch as fedrq would answer it: rawhide's
    /// python-sphinx-autoapi needs a newer sphinx than the target has.
    struct Rawhide;

    impl DepResolver for Rawhide {
        fn buildrequires(&self, srpm: &str) -> Result<Vec<String>, String> {
            match srpm {
                "python-sphinx-autoapi" => Ok(strs(&["python3dist(sphinx) >= 8"])),
                "python-astroid" => Ok(strs(&["python3-devel"])),
                other => Err(format!("{other} not on rawhide")),
            }
        }
        fn resolve_source(&self, dep: &str) -> Result<Vec<String>, String> {
            Ok(match dep {
                "python3dist(astroid)" => strs(&["python-astroid"]),
                "python3dist(sphinx) >= 8" => strs(&["python-sphinx"]),
                _ => vec![],
            })
        }
        fn resolve_target(&self, _dep: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn src_exists(&self, srpm: &str) -> Result<bool, String> {
            Ok(srpm != "local-only")
        }
        fn subpkg_requires(&self, _srpm: &str) -> Result<Vec<String>, String> {
            Ok(strs(&["python(abi) = 3.14"]))
        }
    }

    /// The older python-sphinx-autoapi built locally for epel10
    /// (rhbz#2563239), and an older astroid it needs.
    fn local() -> LocalRepo {
        LocalRepo {
            buildrequires: BTreeMap::from([
                (
                    "python-sphinx-autoapi".to_string(),
                    strs(&["python3-devel", "python3dist(astroid) < 4"]),
                ),
                ("python-astroid".to_string(), strs(&["python3-devel"])),
            ]),
            binaries: vec![
                binary(
                    "python3-sphinx-autoapi",
                    "python-sphinx-autoapi",
                    "3.4.0-1.el10",
                    &["python3dist(sphinx-autoapi) = 3.4"],
                    &[
                        "python3dist(astroid) < 4",
                        "rpmlib(PayloadIsZstd) <= 5.4.18-1",
                    ],
                ),
                binary(
                    "python3-astroid",
                    "python-astroid",
                    "3.3.8-1.el10",
                    &["python3dist(astroid) = 3.3.8"],
                    &[],
                ),
            ],
        }
    }

    #[test]
    fn a_local_package_takes_its_buildrequires_from_its_srpm() {
        let overlay = LocalOverlay {
            inner: &Rawhide,
            local: local(),
        };
        assert_eq!(
            overlay.buildrequires("python-sphinx-autoapi").unwrap(),
            strs(&["python3-devel", "python3dist(astroid) < 4"])
        );
        // Not built locally: rawhide's.
        assert!(overlay.buildrequires("python-sphinx").is_err());
        let many = overlay.buildrequires_many(&strs(&["python-sphinx-autoapi", "python-sphinx"]));
        assert_eq!(
            many["python-sphinx-autoapi"].as_ref().unwrap(),
            &strs(&["python3-devel", "python3dist(astroid) < 4"])
        );
        assert!(many["python-sphinx"].is_err());
    }

    #[test]
    fn a_dependency_a_local_binary_satisfies_resolves_to_it() {
        let overlay = LocalOverlay {
            inner: &Rawhide,
            local: local(),
        };
        assert_eq!(
            overlay.resolve_source("python3dist(astroid) < 4").unwrap(),
            strs(&["python-astroid"])
        );
        // Not satisfied locally: asked of the source branch.
        assert_eq!(
            overlay.resolve_source("python3dist(sphinx) >= 8").unwrap(),
            strs(&["python-sphinx"])
        );
        let many = overlay
            .resolve_source_many(&strs(&[
                "python3dist(astroid) < 4",
                "python3dist(sphinx) >= 8",
            ]))
            .unwrap();
        assert_eq!(many["python3dist(astroid) < 4"], strs(&["python-astroid"]));
        assert_eq!(many["python3dist(sphinx) >= 8"], strs(&["python-sphinx"]));
    }

    #[test]
    fn a_local_packages_runtime_requires_come_from_its_binaries() {
        let overlay = LocalOverlay {
            inner: &Rawhide,
            local: local(),
        };
        assert_eq!(
            overlay.subpkg_requires("python-sphinx-autoapi").unwrap(),
            strs(&["python3dist(astroid) < 4"])
        );
        assert_eq!(
            overlay.subpkg_requires("python-sphinx").unwrap(),
            strs(&["python(abi) = 3.14"])
        );
    }

    #[test]
    fn the_build_to_read_is_the_newest_that_passed() {
        let dir = tempfile::tempdir().unwrap();
        let build = |nvr: &str, passed: bool| {
            let d = dir.path().join(nvr);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(format!("{nvr}.src.rpm")), "").unwrap();
            std::fs::write(d.join(if passed { "success" } else { "fail" }), "").unwrap();
        };
        build("osmo-0.4.4-7.fc46", true);
        build("osmo-0.4.4-8.fc46", true);
        build("osmo-0.4.4-9.fc46", false);
        build("gnokii-0.6.31-48.fc46", false);
        std::fs::create_dir_all(dir.path().join("repodata")).unwrap();
        // Still building: no SRPM yet.
        std::fs::create_dir_all(dir.path().join("orage-4.20.3-5.fc46")).unwrap();

        let chosen = chosen_builds(dir.path()).unwrap();
        assert_eq!(chosen["osmo"].0, "0.4.4-8.fc46");
        // None passed: the newest attempt still states its BuildRequires.
        assert_eq!(chosen["gnokii"].0, "0.6.31-48.fc46");
        assert!(!chosen.contains_key("orage"));
        assert!(!chosen.contains_key("repodata"));
    }

    #[test]
    fn rpm_s_own_features_are_not_buildrequires() {
        assert_eq!(
            parse_requires(
                "autoconf\nautomake >= 1.16.4\nrpmlib(CompressedFileNames) <= 3.0.4-1\n\n"
            ),
            strs(&["autoconf", "automake >= 1.16.4"])
        );
    }

    #[test]
    fn only_the_chosen_builds_binaries_count() {
        let chosen = BTreeMap::from([(
            "osmo".to_string(),
            (
                "0.4.4-8.fc46".to_string(),
                PathBuf::from("/r/osmo-0.4.4-8.fc46"),
            ),
        )]);
        assert!(is_chosen(
            &binary("osmo", "osmo", "0.4.4-8.fc46", &[], &[]),
            &chosen
        ));
        assert!(!is_chosen(
            &binary("osmo", "osmo", "0.4.4-7.fc46", &[], &[]),
            &chosen
        ));
        assert!(!is_chosen(&binary("x", "other", "1-1", &[], &[]), &chosen));
    }
}
