// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A repository-shaped test fixture for the resolver.
//!
//! The older test double maps a dependency string straight to the
//! source package that answers it, which cannot express how a real
//! query behaves — and three bugs lived in exactly that gap (issues
//! #15, #16 and #21). fedrq answers a *batch* with the union of the
//! providers it found, saying nothing about which dependency each came
//! back for; the caller attributes them afterwards from each package's
//! Provides, which carry versions but never file paths.
//!
//! So a test here states a small package universe — names, sources,
//! versions, Provides and files — and lets the code query it. The
//! matching below is written out independently rather than delegating
//! to [`sandogasa_fedrq::PkgInfo::satisfies`], because that function is
//! one of the things under test: a fixture that agreed with it by
//! construction would pass whatever it did.

use std::collections::{BTreeMap, BTreeSet};

use crate::resolve::DepResolver;

/// One binary package, as a repository describes it.
#[derive(Debug, Clone)]
pub struct Pkg {
    pub name: String,
    /// The source package it was built from.
    pub source: String,
    /// `version-release`.
    pub vr: String,
    /// Capabilities it declares, with the version each states.
    pub provides: Vec<(String, Option<String>)>,
    /// Paths it owns. A repository answers a file dependency from
    /// these, and never mentions them in Provides.
    pub files: Vec<String>,
    /// What it requires at install time.
    pub requires: Vec<String>,
}

impl Pkg {
    /// A package providing its own name at its own version.
    pub fn new(name: &str, source: &str, vr: &str) -> Self {
        Pkg {
            name: name.to_string(),
            source: source.to_string(),
            vr: vr.to_string(),
            provides: vec![(name.to_string(), Some(vr.to_string()))],
            files: Vec::new(),
            requires: Vec::new(),
        }
    }

    /// Add a capability, with the version it states — which is the
    /// capability's own, not the package's: freetype-devel 2.13.2
    /// provides `pkgconfig(freetype2) = 26.1.20`.
    pub fn provides(mut self, capability: &str, version: Option<&str>) -> Self {
        self.provides
            .push((capability.to_string(), version.map(str::to_string)));
        self
    }

    pub fn files(mut self, paths: &[&str]) -> Self {
        self.files.extend(paths.iter().map(|p| p.to_string()));
        self
    }

    pub fn requires(mut self, reqs: &[&str]) -> Self {
        self.requires.extend(reqs.iter().map(|r| r.to_string()));
        self
    }

    /// The record a query returns: Provides and Requires, never files.
    fn info(&self) -> sandogasa_fedrq::PkgInfo {
        let provides = self
            .provides
            .iter()
            .map(|(cap, ver)| match ver {
                Some(v) => format!("{cap} = {v}"),
                None => cap.clone(),
            })
            .collect();
        sandogasa_fedrq::PkgInfo::new(
            self.name.clone(),
            self.requires.clone(),
            provides,
            Some(self.source.clone()),
            "fixture",
        )
    }
}

/// A repository: the packages a branch offers, and the BuildRequires
/// of the sources they were built from.
#[derive(Debug, Default, Clone)]
pub struct Repo {
    packages: Vec<Pkg>,
    buildrequires: BTreeMap<String, Vec<String>>,
}

impl Repo {
    pub fn new() -> Self {
        Repo::default()
    }

    pub fn with(mut self, pkg: Pkg) -> Self {
        self.packages.push(pkg);
        self
    }

    /// Give a source package its BuildRequires. A source with none
    /// still has to be declared, or it does not exist here.
    pub fn source(mut self, srpm: &str, buildrequires: &[&str]) -> Self {
        self.buildrequires.insert(
            srpm.to_string(),
            buildrequires.iter().map(|b| b.to_string()).collect(),
        );
        self
    }

    /// The packages answering a dependency, the way a repository does:
    /// a file dependency from the file list, a capability from the
    /// Provides, and a version constraint applied to whichever
    /// capability states one.
    fn providers(&self, dep: &str) -> Vec<&Pkg> {
        self.packages.iter().filter(|p| answers(p, dep)).collect()
    }

    fn sources_for(&self, dep: &str) -> Vec<String> {
        let mut sources: Vec<String> = self
            .providers(dep)
            .into_iter()
            .map(|p| p.source.clone())
            .collect();
        sources.dedup();
        sources
    }
}

/// Whether a package answers a dependency.
///
/// Written out rather than delegating to the matcher under test. It
/// covers what specs actually carry: a path, a plain capability with
/// an optional constraint, and the boolean forms — `with` asking one
/// package to satisfy both sides, `or` either, and `if` making the
/// requirement conditional (the condition is somebody else's to check,
/// so only the left side is matched here).
fn answers(pkg: &Pkg, dep: &str) -> bool {
    let dep = dep.trim();
    if let Some(inner) = dep.strip_prefix('(').and_then(|d| d.strip_suffix(')')) {
        if let Some((left, right)) = split_once_word(inner, "with") {
            return answers(pkg, left) && answers(pkg, right);
        }
        if let Some((left, right)) = split_once_word(inner, "or") {
            return answers(pkg, left) || answers(pkg, right);
        }
        if let Some((left, _)) = split_once_word(inner, "if") {
            return answers(pkg, left);
        }
        return answers(pkg, inner);
    }
    if dep.starts_with('/') {
        return pkg.files.iter().any(|f| f == dep);
    }
    let mut parts = dep.split_whitespace();
    let Some(cap) = parts.next() else {
        return false;
    };
    let constraint = match (parts.next(), parts.next()) {
        (Some(op), Some(want)) => Some((op, want)),
        _ => None,
    };
    pkg.provides.iter().any(|(have, version)| {
        have == cap
            && match (constraint, version) {
                (None, _) => true,
                (Some(_), None) => true,
                (Some((op, want)), Some(v)) => {
                    sandogasa_rpmvercmp::constraint_satisfied(v, op, want)
                }
            }
    })
}

/// Split a boolean expression on a keyword, respecting nothing else —
/// the fixture's expressions are one level deep.
fn split_once_word<'a>(text: &'a str, word: &str) -> Option<(&'a str, &'a str)> {
    let pattern = format!(" {word} ");
    text.find(&pattern)
        .map(|i| (&text[..i], &text[i + pattern.len()..]))
}

/// A resolver answering from repositories rather than from a table of
/// expected answers.
///
/// Batched lookups return the union the way fedrq does and leave the
/// caller to attribute it, which is what makes an attribution bug
/// visible here.
pub struct RepoResolver {
    pub source: Repo,
    pub target: Repo,
    /// The target's updates-testing, asked for what stable lacks.
    pub target_testing: Option<Repo>,
    pub base: Option<Repo>,
}

impl RepoResolver {
    pub fn new(source: Repo, target: Repo) -> Self {
        RepoResolver {
            source,
            target,
            target_testing: None,
            base: None,
        }
    }

    pub fn with_testing(mut self, testing: Repo) -> Self {
        self.target_testing = Some(testing);
        self
    }

    pub fn with_base(mut self, base: Repo) -> Self {
        self.base = Some(base);
        self
    }

    fn batched(repo: &Repo, deps: &[String]) -> Result<BTreeMap<String, Vec<String>>, String> {
        let mut union: Vec<sandogasa_fedrq::PkgInfo> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for dep in deps {
            for pkg in repo.providers(dep) {
                if seen.insert(pkg.name.clone()) {
                    union.push(pkg.info());
                }
            }
        }
        Ok(crate::resolve::attribute_providers(deps, &union))
    }
}

impl DepResolver for RepoResolver {
    fn buildrequires(&self, srpm: &str) -> Result<Vec<String>, String> {
        self.source
            .buildrequires
            .get(srpm)
            .cloned()
            .ok_or_else(|| format!("{srpm}: not found on source"))
    }

    fn resolve_source(&self, dep: &str) -> Result<Vec<String>, String> {
        Ok(self.source.sources_for(dep))
    }

    fn resolve_target(&self, dep: &str) -> Result<Vec<String>, String> {
        let found = self.target.sources_for(dep);
        if !found.is_empty() {
            return Ok(found);
        }
        Ok(self
            .target_testing
            .as_ref()
            .map(|t| t.sources_for(dep))
            .unwrap_or_default())
    }

    fn src_exists(&self, srpm: &str) -> Result<bool, String> {
        Ok(self.source.buildrequires.contains_key(srpm))
    }

    fn subpkg_requires(&self, srpm: &str) -> Result<Vec<String>, String> {
        Ok(self
            .source
            .packages
            .iter()
            .filter(|p| p.source == srpm)
            .flat_map(|p| p.requires.clone())
            .collect())
    }

    fn resolve_base_vr(&self, dep: &str) -> Result<Vec<(String, String)>, String> {
        let Some(base) = &self.base else {
            return Ok(vec![]);
        };
        // The capability's own version where it states one, as a real
        // query reports it (issue #28).
        let capability = dep.split_whitespace().next().unwrap_or(dep);
        Ok(base
            .providers(dep)
            .into_iter()
            .map(|p| {
                let version = p
                    .provides
                    .iter()
                    .find(|(cap, _)| cap == capability)
                    .and_then(|(_, v)| v.clone())
                    .unwrap_or_else(|| p.vr.clone());
                (p.source.clone(), version)
            })
            .collect())
    }

    fn resolve_source_many(
        &self,
        deps: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, String> {
        Self::batched(&self.source, deps)
    }

    fn resolve_target_many(
        &self,
        deps: &[String],
    ) -> Result<BTreeMap<String, Vec<String>>, String> {
        let mut answered = Self::batched(&self.target, deps)?;
        let Some(testing) = &self.target_testing else {
            return Ok(answered);
        };
        let unanswered: Vec<String> = answered
            .iter()
            .filter(|(_, providers)| providers.is_empty())
            .map(|(dep, _)| dep.clone())
            .collect();
        for (dep, providers) in Self::batched(testing, &unanswered)? {
            if !providers.is_empty() {
                answered.insert(dep, providers);
            }
        }
        Ok(answered)
    }
}
