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
    let mut report = DeriveReport::default();
    for name in &reachable {
        if keeps.contains(name) || !owned.contains(name) {
            continue;
        }
        report.derived.push(Derived {
            name: name.clone(),
            reason: graph.witness_reason(name, &reachable),
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
    let mut out = String::new();
    out.push_str(&format!(
        "derived inventory: {} package(s) ({} to add, {} to remove{})\n",
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
