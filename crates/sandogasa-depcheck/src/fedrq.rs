// SPDX-License-Identifier: Apache-2.0 OR MIT

//! [`RepoQuery`] answered by fedrq, which a real run reads. It runs the
//! fedrq binary, so it is exercised by the tools' runs, not by tests.

use std::collections::{BTreeMap, BTreeSet};

use crate::repo::{RepoQuery, filter_none, paths_shipped, resolved_by};

impl RepoQuery for sandogasa_fedrq::Fedrq {
    fn repolist(&self) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::repolist(self).map_err(|e| e.to_string())
    }

    fn src_nvrs(&self, packages: &[String]) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::src_nvrs(self, packages).map_err(|e| e.to_string())
    }

    fn subpkgs_names(&self, srpm: &str) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::subpkgs_names(self, srpm).map_err(|e| e.to_string())
    }

    fn subpkgs_nvrs(&self, srpm: &str) -> Result<Vec<(String, String, String)>, String> {
        sandogasa_fedrq::Fedrq::subpkgs_nvrs(self, srpm).map_err(|e| e.to_string())
    }

    fn pkgs_source_vr(&self, names: &[&str]) -> Result<Vec<(String, String)>, String> {
        sandogasa_fedrq::Fedrq::pkgs_source_vr(self, names).map_err(|e| e.to_string())
    }

    fn subpkgs_provides(&self, srpm: &str) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::subpkgs_provides(self, srpm).map_err(|e| e.to_string())
    }

    fn pkg_provides(&self, name: &str) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::pkg_provides(self, name).map_err(|e| e.to_string())
    }

    fn provides_of_provider(&self, capability: &str) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::provides_of_provider(self, capability).map_err(|e| e.to_string())
    }

    fn pkg_requires(&self, name: &str) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::pkg_requires(self, name).map_err(|e| e.to_string())
    }

    fn subpkgs_requires(&self, srpm: &str) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::subpkgs_requires(self, srpm).map_err(|e| e.to_string())
    }

    fn src_requires(&self, srpm: &str) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::src_requires(self, srpm).map_err(|e| e.to_string())
    }

    fn whatrequires(&self, packages: &[String]) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::whatrequires(self, packages).map_err(|e| e.to_string())
    }

    fn resolve_to_source(&self, dep: &str) -> Result<Vec<String>, String> {
        sandogasa_fedrq::Fedrq::resolve_to_source(self, dep).map_err(|e| e.to_string())
    }

    // A batch that fails is asked again one at a time, so one bad
    // operand costs that batch's speed, not its answers.

    fn pkgs_requires(&self, names: &[String]) -> Vec<(String, Vec<String>)> {
        match self.pkgs_requires_info(names) {
            Ok(infos) => {
                let mut by_name: BTreeMap<String, Vec<String>> =
                    names.iter().map(|n| (n.clone(), Vec::new())).collect();
                for i in infos {
                    by_name.entry(i.name).or_default().extend(i.requires);
                }
                by_name.into_iter().collect()
            }
            Err(_) => names
                .iter()
                .map(|n| {
                    (
                        n.clone(),
                        filter_none(self.pkg_requires(n).unwrap_or_default()),
                    )
                })
                .collect(),
        }
    }

    fn pkgs_provides(&self, names: &[String]) -> Vec<String> {
        match self.pkgs_provides_info(names) {
            Ok(infos) => infos.into_iter().flat_map(|i| i.provides).collect(),
            Err(_) => names
                .iter()
                .flat_map(|n| filter_none(self.pkg_provides(n).unwrap_or_default()))
                .collect(),
        }
    }

    fn subpkgs_provides_all(&self, srpms: &[String]) -> Vec<String> {
        match self.subpkgs_provides_info(srpms) {
            Ok(infos) => infos.into_iter().flat_map(|i| i.provides).collect(),
            Err(_) => srpms
                .iter()
                .flat_map(|s| filter_none(self.subpkgs_provides(s).unwrap_or_default()))
                .collect(),
        }
    }

    fn resolvable(&self, caps: &[String]) -> BTreeSet<String> {
        let one_at_a_time = |caps: &[String]| -> BTreeSet<String> {
            caps.iter()
                .filter(|c| {
                    !filter_none(self.provides_of_provider(c).unwrap_or_default()).is_empty()
                })
                .cloned()
                .collect()
        };
        let (paths, names): (Vec<String>, Vec<String>) =
            caps.iter().cloned().partition(|c| c.starts_with('/'));
        let mut out = match self.providers_vr_info(&names) {
            Ok(providers) => resolved_by(&names, &providers),
            Err(_) => one_at_a_time(&names),
        };
        out.extend(match self.file_providers_info(&paths) {
            Ok(providers) => paths_shipped(&paths, &providers),
            Err(_) => one_at_a_time(&paths),
        });
        // What the batch could not match back is asked again on its
        // own, so fedrq's own resolution has the last word: the
        // matching above is a fast path, and a way of providing a
        // capability it does not know must not read as a missing one.
        // Few are left over, as most capabilities resolve.
        let left: Vec<String> = caps.iter().filter(|c| !out.contains(*c)).cloned().collect();
        out.extend(one_at_a_time(&left));
        out
    }

    fn sources_requires(&self, srpms: &[String]) -> Vec<(String, Vec<String>, Vec<String>)> {
        let (Ok(binaries), Ok(sources)) = (self.subpkgs_info(srpms), self.srcs_info(srpms)) else {
            return srpms
                .iter()
                .map(|s| {
                    (
                        s.clone(),
                        filter_none(self.subpkgs_requires(s).unwrap_or_default()),
                        filter_none(self.src_requires(s).unwrap_or_default()),
                    )
                })
                .collect();
        };
        let mut by_source: BTreeMap<&str, (Vec<String>, Vec<String>)> = srpms
            .iter()
            .map(|s| (s.as_str(), Default::default()))
            .collect();
        for b in &binaries {
            if let Some(entry) = b.source_name.as_deref().and_then(|s| by_source.get_mut(s)) {
                entry.0.extend(b.requires.iter().cloned());
            }
        }
        for s in &sources {
            if let Some(entry) = by_source.get_mut(s.name.as_str()) {
                entry.1.extend(s.requires.iter().cloned());
            }
        }
        by_source
            .into_iter()
            .map(|(s, (bin, src))| (s.to_string(), bin, src))
            .collect()
    }
}
