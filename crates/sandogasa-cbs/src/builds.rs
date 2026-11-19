// SPDX-License-Identifier: Apache-2.0 OR MIT

//! CBS builds as a repository a check can read (#157): their binary
//! RPMs with each one's Provides and Requires, read from the hub, so a
//! candidate is checked before any repository holds it. Three
//! `multiCall`s cover any number of builds: the binaries, then their
//! Provides, then their Requires.

use std::collections::{BTreeMap, BTreeSet};

use sandogasa_depcheck::RepoQuery;
use sandogasa_depcheck::rich::extract_capability_names;
use sandogasa_depfilter::provide_name;
use sandogasa_kojihub::HubClient;

/// One binary RPM of a build.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Binary {
    /// `name`, without version, release or arch.
    pub name: String,
    /// `version-release`.
    pub vr: String,
    pub provides: Vec<String>,
    pub requires: Vec<String>,
}

/// What the hub says about some builds.
#[derive(Debug, Clone, Default)]
pub struct HubBuilds {
    /// Per build NVR: its source package and binaries.
    builds: BTreeMap<String, (String, Vec<Binary>)>,
    /// Per build NVR: what its RPMs provide on every architecture.
    all_provides: BTreeMap<String, BTreeSet<String>>,
    /// Builds the hub could not answer for, with why.
    pub errors: BTreeMap<String, String>,
}

/// Some builds as a repository: their sources and binaries.
#[derive(Debug, Clone, Default)]
pub struct BuildsView {
    builds: Vec<(String, Vec<Binary>)>,
}

impl HubBuilds {
    /// Read `nvrs` from `hub`: three `multiCall`s in all. A build the hub
    /// does not know, or one whose dependencies it cannot give, is put
    /// in [`HubBuilds::errors`] rather than failing the rest; a hub that
    /// cannot be read at all is an error.
    pub fn fetch(hub: &HubClient, nvrs: &[String]) -> Result<Self, String> {
        let rpms = hub.builds_rpm_nvras(nvrs).map_err(|e| e.to_string())?;
        let listed: Vec<Result<Vec<String>, String>> = rpms
            .into_iter()
            .map(|r| r.map_err(|e| e.to_string()))
            .collect();
        let all: Vec<String> = listed
            .iter()
            .flat_map(|r| r.iter().flatten().cloned())
            .collect();
        let deps = |dep_type| -> Result<Vec<Result<Vec<String>, String>>, String> {
            Ok(hub
                .rpms_dep_strings(&all, dep_type)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|r| r.map_err(|e| e.to_string()))
                .collect())
        };
        let provides = deps(1)?;
        let requires = deps(0)?;
        Ok(Self::from_parts(nvrs, listed, &provides, &requires))
    }

    /// The builds from the hub's answers: per build its binary RPMs
    /// (`name-version-release.arch`), then per RPM, in the same order,
    /// its Provides and Requires. Debug packages are left out.
    pub fn from_parts(
        nvrs: &[String],
        rpms: Vec<Result<Vec<String>, String>>,
        provides: &[Result<Vec<String>, String>],
        requires: &[Result<Vec<String>, String>],
    ) -> Self {
        let mut out = Self::default();
        let mut k = 0;
        for (nvr, listed) in nvrs.iter().zip(rpms) {
            let list = match listed {
                Ok(list) => list,
                Err(e) => {
                    out.errors.insert(nvr.clone(), e);
                    continue;
                }
            };
            let (source, _) = split_nvr(nvr);
            let mut binaries = Vec::new();
            let mut all_provides = BTreeSet::new();
            // One binary per name: the one for the architecture fedrq
            // answers for (the host's), else noarch, else the first,
            // so its Requires are judged against the same
            // architecture's packages (x86_64's glibc symbol versions
            // do not exist on aarch64).
            let rank = |rpm: &str| match rpm.rsplit_once('.').map(|(_, a)| a) {
                Some(a) if a == std::env::consts::ARCH => 0,
                Some("noarch") => 1,
                _ => 2,
            };
            let mut order: Vec<(usize, String)> = list.into_iter().enumerate().collect();
            order.sort_by_key(|(i, rpm)| (rank(rpm), *i));
            let mut answers: Vec<(String, usize)> = Vec::new();
            for (i, rpm) in order {
                answers.push((rpm, k + i));
            }
            k += answers.len();
            for (rpm, at) in answers {
                let (p, r) = (provides.get(at), requires.get(at));
                let nvr_part = rpm.rsplit_once('.').map(|(n, _)| n).unwrap_or(&rpm);
                let (name, vr) = split_nvr(nvr_part);
                if is_debug(name) {
                    continue;
                }
                // Every architecture's Provides, for what the build
                // changes: a Provide dropped on one architecture only
                // counts too.
                if let Some(Ok(p)) = p {
                    all_provides.extend(p.iter().cloned());
                }
                // The best-ranked RPM of a name came first: the others,
                // and anything else the hub says about them, are not used.
                if binaries.iter().any(|b: &Binary| b.name == name) {
                    continue;
                }
                match (p, r) {
                    (Some(Ok(p)), Some(Ok(r))) => {
                        binaries.push(Binary {
                            name: name.to_string(),
                            vr: vr.to_string(),
                            provides: p.clone(),
                            requires: r.clone(),
                        });
                    }
                    (Some(Err(e)), _) | (_, Some(Err(e))) => {
                        out.errors
                            .entry(nvr.clone())
                            .or_insert_with(|| format!("{rpm}: {e}"));
                    }
                    _ => {
                        out.errors
                            .entry(nvr.clone())
                            .or_insert_with(|| format!("{rpm}: no answer"));
                    }
                }
            }
            if !out.errors.contains_key(nvr) {
                out.builds
                    .insert(nvr.clone(), (source.to_string(), binaries));
                out.all_provides.insert(nvr.clone(), all_provides);
            }
        }
        out
    }

    /// Everything the binaries of `nvr` provide, on every architecture
    /// the hub could answer for.
    pub fn provides(&self, nvr: &str) -> BTreeSet<String> {
        self.all_provides.get(nvr).cloned().unwrap_or_default()
    }

    /// The names of the binaries of `nvr`.
    pub fn binaries(&self, nvr: &str) -> Vec<String> {
        self.builds
            .get(nvr)
            .into_iter()
            .flat_map(|(_, bins)| bins.iter().map(|b| b.name.clone()))
            .collect()
    }

    /// The builds in `nvrs` as one repository, for a check to read.
    pub fn view(&self, nvrs: &[&str]) -> BuildsView {
        BuildsView {
            builds: nvrs
                .iter()
                .filter_map(|n| self.builds.get(*n))
                .cloned()
                .collect(),
        }
    }
}

impl BuildsView {
    fn binaries(&self) -> impl Iterator<Item = (&str, &Binary)> {
        self.builds
            .iter()
            .flat_map(|(src, bins)| bins.iter().map(move |b| (src.as_str(), b)))
    }

    fn of_source<'a>(&'a self, srpm: &'a str) -> impl Iterator<Item = &'a Binary> {
        self.binaries()
            .filter(move |(s, _)| *s == srpm)
            .map(|(_, b)| b)
    }

    fn named(&self, name: &str) -> Option<&Binary> {
        self.binaries().map(|(_, b)| b).find(|b| b.name == name)
    }

    /// The binaries providing a capability `dep` names: any of them,
    /// for a rich dependency (`(a or b)`).
    fn providing<'a>(&'a self, dep: &str) -> impl Iterator<Item = (&'a str, &'a Binary)> {
        let names = extract_capability_names(dep);
        self.binaries().filter(move |(_, b)| {
            b.provides
                .iter()
                .any(|p| names.iter().any(|n| n == provide_name(p)))
        })
    }

    /// The sources whose binaries provide a capability `dep` names.
    fn providers(&self, dep: &str) -> Vec<String> {
        self.providing(dep)
            .map(|(s, _)| s.to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

impl RepoQuery for BuildsView {
    fn repolist(&self) -> Result<Vec<String>, String> {
        Ok(vec!["cbs".to_string()])
    }

    fn src_nvrs(&self, packages: &[String]) -> Result<Vec<String>, String> {
        Ok(self
            .builds
            .iter()
            .filter(|(src, _)| packages.contains(src))
            .filter_map(|(src, bins)| bins.first().map(|b| format!("{src}-{}", b.vr)))
            .collect())
    }

    fn subpkgs_names(&self, srpm: &str) -> Result<Vec<String>, String> {
        Ok(self.of_source(srpm).map(|b| b.name.clone()).collect())
    }

    fn subpkgs_nvrs(&self, srpm: &str) -> Result<Vec<(String, String, String)>, String> {
        Ok(self
            .of_source(srpm)
            .map(|b| {
                let (v, r) = b.vr.split_once('-').unwrap_or((&b.vr, ""));
                (b.name.clone(), v.to_string(), r.to_string())
            })
            .collect())
    }

    fn pkgs_source_vr(&self, names: &[&str]) -> Result<Vec<(String, String)>, String> {
        Ok(self
            .binaries()
            .filter(|(_, b)| names.contains(&b.name.as_str()))
            .map(|(s, b)| (s.to_string(), b.vr.clone()))
            .collect())
    }

    fn subpkgs_provides(&self, srpm: &str) -> Result<Vec<String>, String> {
        Ok(self
            .of_source(srpm)
            .flat_map(|b| b.provides.iter().cloned())
            .collect())
    }

    fn pkg_provides(&self, name: &str) -> Result<Vec<String>, String> {
        Ok(self
            .named(name)
            .map(|b| b.provides.clone())
            .unwrap_or_default())
    }

    fn provides_of_provider(&self, capability: &str) -> Result<Vec<String>, String> {
        Ok(self
            .providing(capability)
            .flat_map(|(_, b)| b.provides.iter().cloned())
            .collect())
    }

    fn pkg_requires(&self, name: &str) -> Result<Vec<String>, String> {
        Ok(self
            .named(name)
            .map(|b| b.requires.clone())
            .unwrap_or_default())
    }

    fn subpkgs_requires(&self, srpm: &str) -> Result<Vec<String>, String> {
        Ok(self
            .of_source(srpm)
            .flat_map(|b| b.requires.iter().cloned())
            .collect())
    }

    /// The hub gives a binary build's dependencies, not the source
    /// package's BuildRequires.
    fn src_requires(&self, _srpm: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    fn whatrequires(&self, packages: &[String]) -> Result<Vec<String>, String> {
        let names: BTreeSet<&str> = packages.iter().map(|p| provide_name(p)).collect();
        Ok(self
            .binaries()
            .filter(|(_, b)| {
                b.requires.iter().any(|r| {
                    extract_capability_names(r)
                        .iter()
                        .any(|n| names.contains(n.as_str()))
                })
            })
            .map(|(s, _)| s.to_string())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect())
    }

    fn resolve_to_source(&self, dep: &str) -> Result<Vec<String>, String> {
        Ok(self.providers(dep))
    }
}

/// `name-version-release` split into its name and `version-release`.
fn split_nvr(nvr: &str) -> (&str, &str) {
    let mut parts = nvr.rsplitn(3, '-');
    let release = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    let name = parts.next().unwrap_or(nvr);
    let vr_start = name.len() + 1;
    (
        name,
        nvr.get(vr_start..)
            .filter(|_| !version.is_empty() && !release.is_empty())
            .unwrap_or(""),
    )
}

/// Whether `name` is a debuginfo or debugsource package, whose
/// Provides only debuggers use.
fn is_debug(name: &str) -> bool {
    name.ends_with("-debuginfo") || name.ends_with("-debugsource")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// foo-2.0-1.el10 with a library, a tools package and its debuginfo,
    /// built for two arches; and a build the hub does not know.
    fn builds() -> HubBuilds {
        let nvrs = s(&["foo-2.0-1.el10", "gone-1-1.el10"]);
        let rpms = vec![
            Ok(s(&[
                "foo-libs-2.0-1.el10.x86_64",
                "foo-libs-2.0-1.el10.aarch64",
                "foo-tools-2.0-1.el10.x86_64",
                "foo-debuginfo-2.0-1.el10.x86_64",
            ])),
            Err("No such build".to_string()),
        ];
        let provides = vec![
            Ok(s(&[
                "libfoo.so.2()(64bit)",
                "foo-libs = 2.0-1.el10",
                "foo-libs(x86-64)",
            ])),
            Ok(s(&[
                "libfoo.so.2()(64bit)",
                "foo-libs = 2.0-1.el10",
                "foo-libs(aarch-64)",
            ])),
            Ok(s(&["foo-tools = 2.0-1.el10"])),
            Ok(s(&["debuginfo(build-id) = abc"])),
        ];
        let requires = vec![
            Ok(s(&["libc.so.6()(64bit)"])),
            Ok(s(&["libc.so.6()(64bit)"])),
            Ok(s(&["foo-libs = 2.0-1.el10", "(bash or zsh)"])),
            Ok(vec![]),
        ];
        HubBuilds::from_parts(&nvrs, rpms, &provides, &requires)
    }

    #[test]
    fn the_hub_answers_become_builds_and_errors() {
        let b = builds();
        assert_eq!(b.binaries("foo-2.0-1.el10"), ["foo-libs", "foo-tools"]);
        // Every architecture's Provides, though only one RPM of a name
        // is a binary.
        assert_eq!(
            b.provides("foo-2.0-1.el10"),
            s(&[
                "foo-libs = 2.0-1.el10",
                "foo-libs(aarch-64)",
                "foo-libs(x86-64)",
                "foo-tools = 2.0-1.el10",
                "libfoo.so.2()(64bit)"
            ])
            .into_iter()
            .collect::<BTreeSet<_>>()
        );
        assert_eq!(b.errors["gone-1-1.el10"], "No such build");
        assert!(b.binaries("gone-1-1.el10").is_empty());
        // An RPM whose dependencies the hub cannot give fails its build.
        let partial = HubBuilds::from_parts(
            &s(&["bar-1-1.el10"]),
            vec![Ok(s(&["bar-1-1.el10.x86_64"]))],
            &[Err("fault".to_string())],
            &[Ok(vec![])],
        );
        assert_eq!(partial.errors["bar-1-1.el10"], "bar-1-1.el10.x86_64: fault");
        let short = HubBuilds::from_parts(
            &s(&["bar-1-1.el10"]),
            vec![Ok(s(&["bar-1-1.el10.x86_64"]))],
            &[],
            &[],
        );
        assert!(short.errors["bar-1-1.el10"].ends_with("no answer"));
        // One the check does not use, for another architecture, does not.
        let host = format!("bar-1-1.el10.{}", std::env::consts::ARCH);
        let other = HubBuilds::from_parts(
            &s(&["bar-1-1.el10"]),
            vec![Ok(vec!["bar-1-1.el10.s390x".to_string(), host])],
            &[Err("fault".to_string()), Ok(s(&["bar = 1-1.el10"]))],
            &[Ok(vec![]), Ok(vec![])],
        );
        assert!(other.errors.is_empty(), "{:?}", other.errors);
        assert_eq!(other.binaries("bar-1-1.el10"), ["bar"]);
    }

    #[test]
    fn a_view_answers_a_checks_queries() {
        let b = builds();
        let v = b.view(&["foo-2.0-1.el10", "gone-1-1.el10"]);
        assert_eq!(v.repolist().unwrap(), ["cbs"]);
        assert_eq!(v.src_nvrs(&s(&["foo", "bar"])).unwrap(), ["foo-2.0-1.el10"]);
        assert_eq!(v.subpkgs_names("foo").unwrap(), ["foo-libs", "foo-tools"]);
        assert_eq!(
            v.subpkgs_nvrs("foo").unwrap()[0],
            (
                "foo-libs".to_string(),
                "2.0".to_string(),
                "1.el10".to_string()
            )
        );
        assert_eq!(
            v.pkgs_source_vr(&["foo-tools"]).unwrap(),
            [("foo".to_string(), "2.0-1.el10".to_string())]
        );
        // One foo-libs RPM, whichever the machine's architecture picks.
        assert_eq!(v.subpkgs_provides("foo").unwrap().len(), 4);
        assert_eq!(
            v.pkg_provides("foo-tools").unwrap(),
            ["foo-tools = 2.0-1.el10"]
        );
        assert!(v.pkg_provides("nope").unwrap().is_empty());
        assert_eq!(
            v.provides_of_provider("libfoo.so.2()(64bit)").unwrap()[..2],
            ["libfoo.so.2()(64bit)", "foo-libs = 2.0-1.el10"]
        );
        assert_eq!(v.pkg_requires("foo-libs").unwrap(), ["libc.so.6()(64bit)"]);
        assert_eq!(v.subpkgs_requires("foo").unwrap().len(), 3);
        assert!(v.src_requires("foo").unwrap().is_empty());
        assert_eq!(v.whatrequires(&s(&["foo-libs = 2.0"])).unwrap(), ["foo"]);
        assert_eq!(v.resolve_to_source("foo-tools >= 1").unwrap(), ["foo"]);
        assert!(v.resolve_to_source("nothing").unwrap().is_empty());
        // Every alternative of a rich dependency counts.
        assert_eq!(v.whatrequires(&s(&["zsh"])).unwrap(), ["foo"]);
        assert_eq!(
            v.resolve_to_source("(nothing or foo-tools)").unwrap(),
            ["foo"]
        );
    }

    #[test]
    fn an_nvr_splits_into_name_and_version_release() {
        assert_eq!(split_nvr("foo-bar-1.2-3.el10"), ("foo-bar", "1.2-3.el10"));
        assert_eq!(split_nvr("odd"), ("odd", ""));
    }
}
