// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `derive` subcommand.
//!
//! The derived dependency inventory is fully derivable: it equals
//! "reachable from the keeps" ∩ "packages you own", minus the keeps
//! themselves — and a saved `deps --graph` graph holds the whole
//! closure. So a keep-set edit never needs a fresh walk to update it:
//! recompute the view offline, in milliseconds, with `reason` chains
//! taken from witness edges. The report says what would be added and
//! removed relative to the file's current content; `--apply` replaces
//! the file's packages wholesale — an idempotent recompute rather
//! than per-edit bookkeeping.
//!
//! The closure inventory — the walk's output, `deps -o` — is the
//! other half of the same view: what a walk of the keeps collects,
//! replayed over the graph with the walk's own rules (base-distro
//! providers end the walk, only `--from` repos are collected), minus
//! the keeps and — where a derived inventory exists to hold them —
//! the owned packages. [`closure`] recomputes it, so `keep` and
//! `reconcile` share one definition of what the file holds instead of
//! each appending what its own walk happened to collect.
//!
//! The view is only as true as the last full walk: distro drift
//! (renamed sources, changed Requires, new providers) is invisible
//! offline, and an owned package that was never a fixpoint root has
//! no BuildRequires edges recorded. The periodic `deps` walk stays
//! the calibration.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::deps::DepsGraph;

/// The recomputed view, diffed against the file's current content.
#[derive(Debug, Default, Serialize)]
pub struct DeriveReport {
    /// The full derived set: owned, reachable, not a keep.
    pub derived: Vec<Derived>,
    /// In the derived set but not in the output file yet.
    pub added: Vec<String>,
    /// In the output file but no longer derivable.
    pub removed: Vec<String>,
    /// Closure view only: in the file, yet the graph reaches none of
    /// them — kept, since the graph is known to lack edges for
    /// providers a walk could not attribute (see TODO.md), and a full
    /// walk settles them either way.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unreached: Vec<String>,
}

/// One derived package with its witness reason.
#[derive(Debug, Serialize)]
pub struct Derived {
    pub name: String,
    pub reason: Option<String>,
}

/// Recompute the derived inventory from the graph: reachable from
/// `keeps`, owned per `owned`, not itself a keep. `current` is the
/// output file's present content, for the added/removed diff.
pub fn derive(
    graph: &DepsGraph,
    keeps: &BTreeSet<String>,
    owned: &BTreeSet<String>,
    current: &BTreeSet<String>,
) -> DeriveReport {
    let reachable = graph.reachable(keeps);
    view(graph, &reachable, keeps, current, |name| {
        owned.contains(name)
    })
}

/// Recompute the closure inventory from the graph: what a walk of
/// `keeps` collects — providers from the `from` repos, the walk ending
/// at `base_prefixes` — minus the keeps, and minus `owned_elsewhere`
/// when the closure has a derived inventory that holds the owned ones
/// (an external closure without one keeps them here).
pub fn closure(
    graph: &DepsGraph,
    keeps: &BTreeSet<String>,
    owned_elsewhere: Option<&BTreeSet<String>>,
    current: &BTreeSet<String>,
    from: &BTreeSet<String>,
    base_prefixes: &[String],
) -> DeriveReport {
    let (reached, collected) = graph.collectable(keeps, from, base_prefixes);
    let misplaced =
        |name: &str| keeps.contains(name) || owned_elsewhere.is_some_and(|o| o.contains(name));
    let mut report = view(graph, &reached, keeps, current, |name| {
        collected.contains(name) && !misplaced(name)
    });
    // A keep or an owned package belongs elsewhere and goes; an entry
    // the graph simply does not reach is kept and reported — the
    // graph, not the file, is what is incomplete there.
    let (removed, unreached): (Vec<String>, Vec<String>) = std::mem::take(&mut report.removed)
        .into_iter()
        .partition(|name| misplaced(name));
    report.removed = removed;
    report.unreached = unreached;
    report
}

/// Write a closure view: the file's entries stay (their reasons too,
/// and the unreached ones), `removed` go, `added` come in with their
/// witness reasons.
pub fn apply_merge(path: &str, meta_from: &str, report: &DeriveReport) -> Result<(), String> {
    let mut inventory = match std::path::Path::new(path).exists() {
        true => sandogasa_inventory::load(path)?,
        false => sandogasa_inventory::Inventory {
            inventory: sandogasa_inventory::load(meta_from)?.inventory,
            package: Vec::new(),
        },
    };
    inventory
        .package
        .retain(|p| !report.removed.contains(&p.name));
    let have: BTreeSet<String> = inventory.package.iter().map(|p| p.name.clone()).collect();
    for d in report.derived.iter().filter(|d| !have.contains(&d.name)) {
        inventory.package.push(sandogasa_inventory::Package {
            name: d.name.clone(),
            reason: d.reason.clone(),
            ..Default::default()
        });
    }
    sandogasa_inventory::save(&inventory, path)?;
    Ok(())
}

/// The view over `reachable`, restricted by `include`, diffed against
/// `current`.
fn view(
    graph: &DepsGraph,
    reachable: &BTreeSet<String>,
    keeps: &BTreeSet<String>,
    current: &BTreeSet<String>,
    include: impl Fn(&str) -> bool,
) -> DeriveReport {
    let mut report = DeriveReport::default();
    for name in reachable {
        if keeps.contains(name) || !include(name) {
            continue;
        }
        report.derived.push(Derived {
            name: name.clone(),
            reason: graph.witness_reason(name, reachable),
        });
        if !current.contains(name) {
            report.added.push(name.clone());
        }
    }
    let derived_names: BTreeSet<&str> = report.derived.iter().map(|d| d.name.as_str()).collect();
    report.removed = current
        .iter()
        .filter(|c| !derived_names.contains(c.as_str()))
        .cloned()
        .collect();
    report
}

/// The human-readable diff-first summary.
pub fn format_report(report: &DeriveReport, applied: bool) -> String {
    format_report_of("derived inventory", report, applied)
}

/// [`format_report`] for either view, named by `what`.
pub fn format_report_of(what: &str, report: &DeriveReport, applied: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{what}: {} package(s) ({} to add, {} to remove{})\n",
        report.derived.len(),
        report.added.len(),
        report.removed.len(),
        if applied {
            "; applied"
        } else {
            "; report only"
        },
    ));
    for name in &report.added {
        out.push_str(&format!("  + {name}\n"));
    }
    for name in &report.removed {
        out.push_str(&format!("  - {name}\n"));
    }
    if !report.unreached.is_empty() {
        out.push_str(&format!(
            "  {} not reached by the graph, kept: {}\n",
            report.unreached.len(),
            report.unreached.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deps::GraphProvider;

    /// app-a (keep, owned) needs crate-c (owned); crate-c's
    /// BuildRequires need tool-t (not owned); lib-l (owned) is
    /// unreachable.
    fn graph() -> DepsGraph {
        let mut g = DepsGraph {
            roots: vec!["app-a".into(), "crate-c".into()],
            ..Default::default()
        };
        for (b, s) in [
            ("app-a", "app-a"),
            ("crate-c-devel", "crate-c"),
            ("src:crate-c", "crate-c"),
            ("tool-t", "tool-t"),
            ("liba", "lib-l"),
        ] {
            g.binary_sources.insert(b.into(), s.into());
        }
        let edge = |g: &mut DepsGraph, cap: &str, requirer: &str, binary: &str, source: &str| {
            g.requirers
                .entry(cap.into())
                .or_default()
                .insert(requirer.into());
            g.providers
                .entry(cap.into())
                .or_default()
                .insert(GraphProvider {
                    binary: binary.into(),
                    source: source.into(),
                    repoid: "rawhide".into(),
                });
        };
        edge(&mut g, "crate(c)", "app-a", "crate-c-devel", "crate-c");
        edge(&mut g, "tool-t", "src:crate-c", "tool-t", "tool-t");
        g
    }

    #[test]
    fn the_closure_view_is_reachable_non_owned_non_keeps() {
        let g = graph();
        let keeps: BTreeSet<String> = ["app-a".to_string()].into();
        let owned: BTreeSet<String> = ["app-a", "crate-c", "lib-l"]
            .into_iter()
            .map(String::from)
            .collect();
        // The file lists a stale entry nothing reaches, and a keep and
        // an owned package that belong elsewhere — all dropped.
        let current: BTreeSet<String> = ["gone", "app-a", "crate-c"]
            .into_iter()
            .map(String::from)
            .collect();
        let from: BTreeSet<String> = ["rawhide".to_string()].into();
        let report = closure(&g, &keeps, Some(&owned), &current, &from, &[]);
        let names: Vec<&str> = report.derived.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["tool-t"], "reachable, not a keep, not owned");
        assert_eq!(report.added, ["tool-t"]);
        // The keep and the owned package belong elsewhere; the entry the
        // graph does not reach is kept and named.
        assert_eq!(report.removed, ["app-a", "crate-c"]);
        assert_eq!(report.unreached, ["gone"]);
        assert!(
            report.derived[0]
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("crate-c") && r.contains("tool-t")),
            "{:?}",
            report.derived[0].reason
        );
        let dir = tempfile::tempdir().unwrap();
        let meta = dir.path().join("keeps.toml");
        std::fs::write(
            &meta,
            "[inventory]\nname = \"k\"\ndescription = \"k\"\nmaintainer = \"me\"\n",
        )
        .unwrap();
        let out = dir.path().join("closure.toml");
        std::fs::write(
            &out,
            "[inventory]\nname = \"c\"\ndescription = \"c\"\nmaintainer = \"me\"\n\n[[package]]\nname = \"gone\"\nreason = \"old\"\n\n[[package]]\nname = \"app-a\"\n\n[[package]]\nname = \"crate-c\"\n",
        )
        .unwrap();
        apply_merge(out.to_str().unwrap(), meta.to_str().unwrap(), &report).unwrap();
        let written = sandogasa_inventory::load(out.to_str().unwrap()).unwrap();
        let entries: Vec<(&str, Option<&str>)> = written
            .package
            .iter()
            .map(|p| (p.name.as_str(), p.reason.as_deref()))
            .collect();
        assert_eq!(
            entries[0],
            ("gone", Some("old")),
            "unreached stays, reason intact"
        );
        assert_eq!(entries[1].0, "tool-t");
        assert_eq!(entries.len(), 2);
        // An external closure with no derived inventory keeps the owned
        // packages the walk collected (they are its fixpoint roots).
        let report = closure(&g, &keeps, None, &current, &from, &[]);
        let names: Vec<&str> = report.derived.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["crate-c", "tool-t"]);
        // A base-distro provider ends the walk, and a provider from a
        // repo outside `from` is traversed but not collected.
        let mut g2 = g.clone();
        for (cap, requirer, binary, source, repoid) in [
            (
                "libc.so.6",
                "tool-t",
                "glibc",
                "glibc",
                "fedrq-centos-stream-baseos",
            ),
            ("libextra.so", "tool-t", "libextra", "extra", "epel"),
            ("libdeeper.so", "libextra", "libdeeper", "deeper", "rawhide"),
        ] {
            g2.binary_sources.insert(binary.into(), source.into());
            g2.requirers
                .entry(cap.into())
                .or_default()
                .insert(requirer.into());
            g2.providers
                .entry(cap.into())
                .or_default()
                .insert(GraphProvider {
                    binary: binary.into(),
                    source: source.into(),
                    repoid: repoid.into(),
                });
        }
        let base = ["fedrq-centos-stream-".to_string()];
        let report = closure(&g2, &keeps, Some(&owned), &current, &from, &base);
        let names: Vec<&str> = report.derived.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            ["deeper", "tool-t"],
            "glibc is base, extra is outside --from yet traversed"
        );
        assert!(
            format_report_of("closure inventory", &report, true)
                .starts_with("closure inventory: 2 package(s) (2 to add, 2 to remove; applied)"),
            "{}",
            format_report_of("closure inventory", &report, true)
        );
    }

    #[test]
    fn the_view_is_reachable_owned_non_keeps_with_witnesses() {
        let keeps: BTreeSet<String> = ["app-a".to_string()].into();
        let owned: BTreeSet<String> = ["app-a", "crate-c", "lib-l"].map(String::from).into();
        // The file currently holds crate-c and a stale lib-l.
        let current: BTreeSet<String> = ["crate-c", "lib-l"].map(String::from).into();
        let report = derive(&graph(), &keeps, &owned, &current);

        let names: Vec<&str> = report.derived.iter().map(|d| d.name.as_str()).collect();
        // tool-t is reachable but not owned; lib-l owned but not
        // reachable; app-a reachable+owned but a keep.
        assert_eq!(names, ["crate-c"]);
        assert_eq!(
            report.derived[0].reason.as_deref(),
            Some("dependency (rawhide): app-a requires crate(c)")
        );
        assert!(report.added.is_empty());
        assert_eq!(report.removed, ["lib-l"]);

        // Idempotence: deriving against the corrected content is a
        // no-op diff.
        let corrected: BTreeSet<String> = ["crate-c".to_string()].into();
        let again = derive(&graph(), &keeps, &owned, &corrected);
        assert!(again.added.is_empty() && again.removed.is_empty());
        let text = format_report(&again, false);
        assert!(text.contains("1 package(s) (0 to add, 0 to remove; report only)"));
    }
}
