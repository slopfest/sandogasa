// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `dependents` subcommand.
//!
//! The keep inventories are meant to shrink over time, and this is
//! the report that says where to prune next. Over a saved
//! `deps --graph` graph, classify every package of the given
//! inventories by who needs it:
//!
//! - **leaves** — nothing in the closure requires them. A leaf that
//!   only ships `-devel` binaries (the library shape) is kept for
//!   nobody; a leaf shipping real binaries is presumably kept for its
//!   own sake. The binaries are listed so the reader can tell.
//! - **carried** — some *other package of the same inventories*
//!   requires them. If such a package is only tracked as a
//!   dependency, the curated entry is redundant: track the dependent
//!   and let the dependency walk keep this one in the derived
//!   inventory.
//! - **externally needed** — required only by closure packages
//!   outside the given inventories.
//!
//! Packages the graph has never seen are reported as unknown rather
//! than guessed at — the graph may predate them.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::deps::DepsGraph;

/// Every inventory package, classified by its dependents.
#[derive(Debug, Default, Serialize)]
pub struct DependentsReport {
    /// No dependent in the closure.
    pub leaves: Vec<Leaf>,
    /// Depended on by other packages of the same inventories.
    pub carried: Vec<Carried>,
    /// Depended on only by closure packages outside the inventories.
    pub externally_needed: Vec<Carried>,
    /// Absent from the graph (it may predate them).
    pub unknown: Vec<String>,
}

/// A package nothing in the closure requires.
#[derive(Debug, Serialize)]
pub struct Leaf {
    pub name: String,
    /// Its binary packages, as the graph saw them.
    pub binaries: Vec<String>,
    /// Every binary is `-devel` — the library-only shape.
    pub devel_only: bool,
}

/// A package something requires, and who. Carries the same shape
/// markers as a leaf: a devel-only carried package is almost always
/// tracked as a mere dependency (prune it and let the walk carry it),
/// while one shipping real binaries is likely kept on purpose — the
/// dependency edge says whom the graph *could* carry it for, not why
/// the entry exists.
#[derive(Debug, Serialize)]
pub struct Carried {
    pub name: String,
    pub dependents: Vec<String>,
    /// Its binary packages, as the graph saw them.
    pub binaries: Vec<String>,
    /// Every binary is `-devel` — the library-only shape.
    pub devel_only: bool,
}

/// Classify each of `names` by its dependents in the graph, keeping
/// input (sorted-set) order within each class.
pub fn classify(graph: &DepsGraph, names: &BTreeSet<String>) -> DependentsReport {
    let dependents = graph.dependents();
    // Source → its binaries (the `src:` pseudo-entry left out).
    let mut binaries: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (binary, source) in &graph.binary_sources {
        if !binary.starts_with("src:") {
            binaries.entry(source).or_default().push(binary);
        }
    }

    let mut report = DependentsReport::default();
    for name in names {
        let known = binaries.contains_key(name.as_str())
            || graph.binary_sources.contains_key(&format!("src:{name}"))
            || graph.roots.contains(name);
        if !known {
            report.unknown.push(name.clone());
            continue;
        }
        let deps = dependents.get(name).cloned().unwrap_or_default();
        let bins: Vec<String> = binaries
            .get(name.as_str())
            .into_iter()
            .flatten()
            .map(|b| b.to_string())
            .collect();
        let devel_only = !bins.is_empty() && bins.iter().all(|b| b.ends_with("-devel"));
        if deps.is_empty() {
            report.leaves.push(Leaf {
                devel_only,
                name: name.clone(),
                binaries: bins,
            });
            continue;
        }
        let in_set: Vec<String> = deps
            .iter()
            .filter(|d| names.contains(*d))
            .cloned()
            .collect();
        match in_set.is_empty() {
            false => report.carried.push(Carried {
                name: name.clone(),
                dependents: in_set,
                binaries: bins,
                devel_only,
            }),
            true => report.externally_needed.push(Carried {
                name: name.clone(),
                dependents: deps.into_iter().collect(),
                binaries: bins,
                devel_only,
            }),
        }
    }
    report
}

/// One carried/externally-needed line: the devel-only marker is what
/// separates "prune it, the walk will carry it" from "kept on
/// purpose, decide per package".
fn carried_line(c: &Carried) -> String {
    format!(
        "  {}{} — needed by {}\n",
        c.name,
        if c.devel_only { " [devel-only]" } else { "" },
        c.dependents.join(", ")
    )
}

/// The human-readable report.
pub fn format_report(report: &DependentsReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "leaves — nothing in the closure needs them ({}):\n",
        report.leaves.len()
    ));
    for leaf in &report.leaves {
        out.push_str(&format!(
            "  {}{} — ships {}\n",
            leaf.name,
            if leaf.devel_only { " [devel-only]" } else { "" },
            match leaf.binaries.is_empty() {
                true => "nothing the walk saw".to_string(),
                false => leaf.binaries.join(", "),
            }
        ));
    }
    out.push_str(&format!(
        "carried — other given packages need them, the walk would \
         re-derive them ({}):\n",
        report.carried.len()
    ));
    for c in &report.carried {
        out.push_str(&carried_line(c));
    }
    if !report.externally_needed.is_empty() {
        out.push_str(&format!(
            "externally needed — only closure packages outside the given \
             inventories require them ({}):\n",
            report.externally_needed.len()
        ));
        for c in &report.externally_needed {
            out.push_str(&carried_line(c));
        }
    }
    if !report.unknown.is_empty() {
        out.push_str(&format!(
            "not in the graph ({}): {} — regenerate with deps --graph?\n",
            report.unknown.len(),
            report.unknown.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deps::GraphProvider;

    /// pandoc is a leaf shipping a real binary; rust-quiet is a
    /// devel-only leaf; rust-anyhow is carried by pandoc (in set);
    /// rust-syn is needed only by rust-quote (outside the set).
    fn graph() -> DepsGraph {
        let mut g = DepsGraph {
            roots: vec!["pandoc".into(), "rust-quiet".into(), "rust-anyhow".into()],
            ..Default::default()
        };
        for (b, s) in [
            ("pandoc", "pandoc"),
            ("rust-quiet-devel", "rust-quiet"),
            ("rust-anyhow-devel", "rust-anyhow"),
            ("rust-syn-devel", "rust-syn"),
            ("rust-quote-devel", "rust-quote"),
        ] {
            g.binary_sources.insert(b.into(), s.into());
        }
        let edge = |g: &mut DepsGraph, cap: &str, requirer: &str, prov: GraphProvider| {
            g.requirers
                .entry(cap.into())
                .or_default()
                .insert(requirer.into());
            g.providers.entry(cap.into()).or_default().insert(prov);
        };
        edge(
            &mut g,
            "crate(anyhow)",
            "pandoc",
            GraphProvider {
                binary: "rust-anyhow-devel".into(),
                source: "rust-anyhow".into(),
                repoid: "rawhide".into(),
            },
        );
        edge(
            &mut g,
            "crate(syn)",
            "rust-quote-devel",
            GraphProvider {
                binary: "rust-syn-devel".into(),
                source: "rust-syn".into(),
                repoid: "rawhide".into(),
            },
        );
        g
    }

    #[test]
    fn leaves_carried_and_external_split_correctly() {
        let names: BTreeSet<String> = ["pandoc", "rust-quiet", "rust-anyhow", "rust-syn"]
            .map(String::from)
            .into();
        let report = classify(&graph(), &names);

        let leaves: Vec<(&str, bool)> = report
            .leaves
            .iter()
            .map(|l| (l.name.as_str(), l.devel_only))
            .collect();
        assert_eq!(leaves, [("pandoc", false), ("rust-quiet", true)]);

        assert_eq!(report.carried.len(), 1);
        assert_eq!(report.carried[0].name, "rust-anyhow");
        assert_eq!(report.carried[0].dependents, ["pandoc"]);
        assert!(report.carried[0].devel_only);
        assert!(format_report(&report).contains("rust-anyhow [devel-only] — needed by pandoc"));

        assert_eq!(report.externally_needed.len(), 1);
        assert_eq!(report.externally_needed[0].name, "rust-syn");
        assert_eq!(report.externally_needed[0].dependents, ["rust-quote"]);
    }

    #[test]
    fn a_package_the_graph_never_saw_is_unknown_not_a_leaf() {
        let names: BTreeSet<String> = ["brand-new".to_string()].into();
        let report = classify(&graph(), &names);
        assert!(report.leaves.is_empty());
        assert_eq!(report.unknown, ["brand-new"]);
        assert!(format_report(&report).contains("regenerate with deps --graph?"));
    }

    #[test]
    fn build_time_dependents_count_too() {
        let mut g = graph();
        // pandoc's own BuildRequires pull rust-syn.
        g.binary_sources
            .insert("src:pandoc".to_string(), "pandoc".to_string());
        g.requirers
            .get_mut("crate(syn)")
            .unwrap()
            .insert("src:pandoc".to_string());
        let names: BTreeSet<String> = ["pandoc", "rust-syn"].map(String::from).into();
        let report = classify(&g, &names);
        assert_eq!(report.carried[0].name, "rust-syn");
        assert_eq!(report.carried[0].dependents, ["pandoc"]);
    }
}
