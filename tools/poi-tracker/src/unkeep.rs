// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `unkeep` subcommand.
//!
//! Stop keeping a package without re-walking the world: over the
//! graph a `deps --graph` run persisted, recompute what the remaining
//! keeps reach and report what the removal frees — each freed package
//! named with its former requirers. With `--apply`, the unkept
//! packages leave the keep inventories and the freed ones leave the
//! derived (`--deps`) inventories, so the next `kondo` run offers all
//! of them as candidates. Zero fedrq calls; the periodic full walk is
//! the graph's refresh.
//!
//! Reachability follows *every* provider the walk recorded for a
//! capability, so a package stays reached while any alternative
//! satisfier chain still holds — conservative in the direction that
//! never frees too much. Derived-inventory packages are conditional
//! roots: their `src:` (BuildRequires) edges count only while the
//! package itself is still reached, mirroring the forward fixpoint,
//! so a freed crate's test-only build dependencies cascade out with
//! it.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::deps::DepsGraph;

/// What removing the keeps would change, before anything is edited.
#[derive(Debug, Default, Serialize)]
pub struct UnkeepReport {
    /// Name → keep inventories it was found in (empty: it was not a
    /// keep, and the removal frees nothing through it).
    pub removed: BTreeMap<String, Vec<String>>,
    /// Unkept packages the remaining closure still reaches, with the
    /// requirers that reach them — culling these would be undone by
    /// the next rescue pass.
    pub still_reached: BTreeMap<String, Vec<String>>,
    /// Still-reached packages that were keeps: provably in the
    /// closure, so they belong in the derived inventory instead —
    /// name → the `reason` chain the graph justifies, ready to file
    /// without a fresh walk.
    pub moved: BTreeMap<String, String>,
    /// Packages no longer reachable from the remaining keeps.
    pub freed: Vec<Freed>,
}

/// One package the removal frees.
#[derive(Debug, Serialize)]
pub struct Freed {
    pub name: String,
    /// Who needed it while the unkept packages were still roots.
    pub former_requirers: Vec<String>,
    /// Derived inventories holding it (what `--apply` edits).
    pub in_deps_files: Vec<String>,
}

/// Work out what unkeeping `names` frees. `keeps` are the keep
/// inventories' packages by file; `deps_files` the derived
/// inventories' packages by file. Nothing is edited here.
pub fn plan(
    graph: &DepsGraph,
    names: &[String],
    keeps: &BTreeMap<String, BTreeSet<String>>,
    deps_files: &BTreeMap<String, BTreeSet<String>>,
) -> UnkeepReport {
    let unkept: BTreeSet<&str> = names.iter().map(String::as_str).collect();
    let all_keeps: BTreeSet<String> = keeps.values().flatten().cloned().collect();
    let remaining: BTreeSet<String> = all_keeps
        .iter()
        .filter(|k| !unkept.contains(k.as_str()))
        .cloned()
        .collect();

    let before = graph.reachable(&all_keeps);
    let after = graph.reachable(&remaining);
    let dependents = graph.dependents();
    let dependents_of = |name: &str| -> Vec<String> {
        dependents
            .get(name)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect()
    };

    let mut report = UnkeepReport::default();
    for name in names {
        report.removed.insert(
            name.clone(),
            keeps
                .iter()
                .filter(|(_, pkgs)| pkgs.contains(name))
                .map(|(file, _)| file.clone())
                .collect(),
        );
        if after.contains(name) {
            report
                .still_reached
                .insert(name.clone(), dependents_of(name));
            if report.removed[name].is_empty() {
                continue;
            }
            if let Some(reason) = graph.witness_reason(name, &after) {
                report.moved.insert(name.clone(), reason);
            }
        }
    }
    for name in before.difference(&after) {
        report.freed.push(Freed {
            name: name.clone(),
            former_requirers: dependents_of(name),
            in_deps_files: deps_files
                .iter()
                .filter(|(_, pkgs)| pkgs.contains(name))
                .map(|(file, _)| file.clone())
                .collect(),
        });
    }
    report
}

/// Delete `names` from the inventory at `path`, preserving everything
/// else. Returns how many were removed.
pub fn remove_from_inventory(path: &str, names: &BTreeSet<&str>) -> Result<usize, String> {
    let mut inventory = sandogasa_inventory::load(path)?;
    let before = inventory.package.len();
    inventory
        .package
        .retain(|p| !names.contains(p.name.as_str()));
    let removed = before - inventory.package.len();
    if removed > 0 {
        sandogasa_inventory::save(&inventory, path)?;
    }
    Ok(removed)
}

/// The human-readable plan.
pub fn format_report(report: &UnkeepReport, applied: bool) -> String {
    let mut out = String::new();
    for (name, files) in &report.removed {
        match files.is_empty() {
            true => out.push_str(&format!("{name}: not in any keep inventory\n")),
            false => out.push_str(&format!(
                "{name}: {} from {}\n",
                if applied { "removed" } else { "would remove" },
                files.join(", ")
            )),
        }
    }
    for (name, requirers) in &report.still_reached {
        match report.moved.contains_key(name) {
            true => out.push_str(&format!(
                "{name}: stays in the closure (via {}) — {} the derived inventory\n",
                requirers.join(", "),
                if applied { "moved to" } else { "would move to" },
            )),
            false => out.push_str(&format!(
                "warning: the remaining keeps still reach {name} (via {}) — \
                 a cull would be rescued right back\n",
                requirers.join(", ")
            )),
        }
    }
    match report.freed.is_empty() {
        true => out.push_str("nothing else falls out of the closure\n"),
        false => {
            out.push_str(&format!("freed ({} package(s)):\n", report.freed.len()));
            for f in &report.freed {
                out.push_str(&format!(
                    "  {} — was needed by {}{}\n",
                    f.name,
                    f.former_requirers.join(", "),
                    match f.in_deps_files.is_empty() {
                        true => String::new(),
                        false => format!(
                            " [{} {}]",
                            if applied { "removed from" } else { "in" },
                            f.in_deps_files.join(", ")
                        ),
                    }
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deps::GraphProvider;

    fn provider(binary: &str, source: &str) -> GraphProvider {
        GraphProvider {
            binary: binary.to_string(),
            source: source.to_string(),
            repoid: "rawhide".to_string(),
        }
    }

    /// keeps: app-a, app-x. app-x needs crate-c (owned, in the deps
    /// inventory); crate-c's BuildRequires need tool-t.
    fn graph() -> DepsGraph {
        let mut g = DepsGraph {
            roots: vec!["app-a".into(), "app-x".into(), "crate-c".into()],
            ..Default::default()
        };
        for (b, s) in [
            ("app-a", "app-a"),
            ("app-x", "app-x"),
            ("crate-c-devel", "crate-c"),
            ("src:crate-c", "crate-c"),
            ("tool-t", "tool-t"),
            ("liba", "lib-l"),
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
        edge(&mut g, "liba.so", "app-a", provider("liba", "lib-l"));
        edge(
            &mut g,
            "crate(c)",
            "app-x",
            provider("crate-c-devel", "crate-c"),
        );
        edge(
            &mut g,
            "tool-t",
            "src:crate-c",
            provider("tool-t", "tool-t"),
        );
        g
    }

    fn keeps() -> BTreeMap<String, BTreeSet<String>> {
        BTreeMap::from([(
            "essential.toml".to_string(),
            ["app-a".to_string(), "app-x".to_string()].into(),
        )])
    }

    fn deps_files() -> BTreeMap<String, BTreeSet<String>> {
        BTreeMap::from([(
            "essential-deps.toml".to_string(),
            ["crate-c".to_string()].into(),
        )])
    }

    #[test]
    fn unkeeping_frees_the_chain_including_build_deps() {
        let report = plan(&graph(), &["app-x".to_string()], &keeps(), &deps_files());
        assert_eq!(report.removed["app-x"], ["essential.toml"]);
        assert!(report.still_reached.is_empty());
        let freed: Vec<&str> = report.freed.iter().map(|f| f.name.as_str()).collect();
        // crate-c falls, and with it the build-only tool-t — but
        // app-a's lib-l stands.
        assert_eq!(freed, ["app-x", "crate-c", "tool-t"]);
        let crate_c = &report.freed[1];
        assert_eq!(crate_c.former_requirers, ["app-x"]);
        assert_eq!(crate_c.in_deps_files, ["essential-deps.toml"]);
    }

    #[test]
    fn a_still_reached_package_is_a_warning_not_a_free() {
        let mut g = graph();
        // app-a also needs crate-c at runtime.
        g.requirers
            .get_mut("crate(c)")
            .unwrap()
            .insert("app-a".to_string());
        let report = plan(&g, &["app-x".to_string()], &keeps(), &deps_files());
        let freed: Vec<&str> = report.freed.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(freed, ["app-x"]);
        assert!(!report.still_reached.contains_key("app-x"));
    }

    #[test]
    fn unkeeping_a_package_the_closure_reaches_warns() {
        let mut g = graph();
        // app-a depends on app-x itself.
        g.binary_sources
            .insert("app-x-libs".to_string(), "app-x".to_string());
        let p = provider("app-x-libs", "app-x");
        g.requirers
            .entry("libx.so".to_string())
            .or_default()
            .insert("app-a".to_string());
        g.providers
            .entry("libx.so".to_string())
            .or_default()
            .insert(p);
        let report = plan(&g, &["app-x".to_string()], &keeps(), &deps_files());
        assert_eq!(report.still_reached["app-x"], ["app-a"]);
        assert!(report.freed.is_empty());
    }

    #[test]
    fn a_still_reached_keep_moves_to_the_derived_inventory() {
        // lib-l is a curated keep, but app-a demonstrably needs it:
        // unkeeping it is a move, not a cull.
        let keeps = BTreeMap::from([(
            "essential.toml".to_string(),
            [
                "app-a".to_string(),
                "app-x".to_string(),
                "lib-l".to_string(),
            ]
            .into(),
        )]);
        let report = plan(&graph(), &["lib-l".to_string()], &keeps, &deps_files());
        assert_eq!(report.still_reached["lib-l"], ["app-a"]);
        assert_eq!(
            report.moved["lib-l"],
            "dependency (rawhide): app-a requires liba.so"
        );
        assert!(report.freed.is_empty());
        let text = format_report(&report, false);
        assert!(text.contains("lib-l: stays in the closure (via app-a) — would move to"));

        // A still-reached package that was never a keep is warned
        // about, not moved — there is nothing to move it from.
        let no_lib = BTreeMap::from([(
            "essential.toml".to_string(),
            ["app-a".to_string(), "app-x".to_string()].into(),
        )]);
        let report = plan(&graph(), &["lib-l".to_string()], &no_lib, &deps_files());
        assert!(report.moved.is_empty());
        assert!(
            format_report(&report, false)
                .contains("warning: the remaining keeps still reach lib-l")
        );
    }

    #[test]
    fn a_non_keep_is_reported_as_such_and_frees_nothing() {
        let report = plan(&graph(), &["neovim".to_string()], &keeps(), &deps_files());
        assert!(report.removed["neovim"].is_empty());
        assert!(report.freed.is_empty());
        assert!(report.still_reached.is_empty());
        let text = format_report(&report, false);
        assert!(text.contains("neovim: not in any keep inventory"));
    }

    #[test]
    fn remove_from_inventory_edits_only_the_named() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keeps.toml");
        let path = path.to_str().unwrap();
        std::fs::write(
            path,
            "[inventory]\nname = \"t\"\ndescription = \"t\"\nmaintainer = \"t\"\n\n\
             [[package]]\nname = \"app-a\"\n\n[[package]]\nname = \"app-x\"\n",
        )
        .unwrap();
        assert_eq!(remove_from_inventory(path, &["app-x"].into()).unwrap(), 1);
        let left = sandogasa_inventory::load(path).unwrap();
        let names: Vec<&str> = left.package.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["app-a"]);
    }
}
