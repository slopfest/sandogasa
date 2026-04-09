// SPDX-License-Identifier: MPL-2.0

//! Directed graph algorithms for build dependency ordering.
//!
//! Operates on adjacency lists represented as `BTreeMap<String, BTreeSet<String>>`
//! where each key is a package and its value is the set of packages it depends on
//! (must be built before it).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

/// A group of packages that can be built in parallel.
#[derive(Debug, Serialize, Deserialize)]
pub struct BuildPhase {
    pub phase: usize,
    pub packages: Vec<String>,
}

/// A dependency cycle (strongly connected component with >1 node).
#[derive(Debug, Serialize)]
pub struct Cycle {
    pub packages: Vec<String>,
}

/// Compute topological layers using Kahn's algorithm.
///
/// Each layer (phase) contains packages whose dependencies are all
/// satisfied by earlier phases. Packages within a phase can be built
/// in parallel.
///
/// Returns `Ok(phases)` if the graph is acyclic, or `Err(remaining)`
/// with the package names involved in cycles if the graph has cycles.
pub fn topological_layers(
    edges: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Vec<BuildPhase>, Vec<String>> {
    // edges[A] = {B, C} means A depends on B and C (B, C must build first).
    // "in-degree" of A = number of unsatisfied deps = |edges[A] ∩ graph nodes|.
    // A node is ready when all its in-graph dependencies have been built.

    let mut remaining: BTreeSet<&str> = edges.keys().map(|s| s.as_str()).collect();
    let mut built: BTreeSet<&str> = BTreeSet::new();
    let mut phases = Vec::new();

    loop {
        // Find nodes whose in-graph dependencies are all built.
        let ready: Vec<&str> = remaining
            .iter()
            .filter(|&&node| {
                edges[node]
                    .iter()
                    .all(|dep| !remaining.contains(dep.as_str()) || built.contains(dep.as_str()))
            })
            .copied()
            .collect();

        if ready.is_empty() {
            break;
        }

        for &node in &ready {
            remaining.remove(node);
            built.insert(node);
        }

        let mut pkgs: Vec<String> = ready.iter().map(|s| s.to_string()).collect();
        pkgs.sort();
        phases.push(BuildPhase {
            phase: phases.len() + 1,
            packages: pkgs,
        });
    }

    if remaining.is_empty() {
        Ok(phases)
    } else {
        Err(remaining.iter().map(|s| s.to_string()).collect())
    }
}

/// Find all strongly connected components with more than one node
/// using Tarjan's algorithm. These represent dependency cycles.
pub fn find_cycles(edges: &BTreeMap<String, BTreeSet<String>>) -> Vec<Cycle> {
    let mut state = TarjanState {
        index_counter: 0,
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        index: HashMap::new(),
        lowlink: HashMap::new(),
        sccs: Vec::new(),
    };

    for node in edges.keys() {
        if !state.index.contains_key(node.as_str()) {
            strongconnect(node, edges, &mut state);
        }
    }

    let mut cycles: Vec<Cycle> = state
        .sccs
        .into_iter()
        .filter(|scc| scc.len() > 1)
        .map(|mut packages| {
            packages.sort();
            Cycle { packages }
        })
        .collect();
    cycles.sort_by(|a, b| a.packages.cmp(&b.packages));
    cycles
}

struct TarjanState<'a> {
    index_counter: usize,
    stack: Vec<&'a str>,
    on_stack: BTreeSet<&'a str>,
    index: HashMap<&'a str, usize>,
    lowlink: HashMap<&'a str, usize>,
    sccs: Vec<Vec<String>>,
}

fn strongconnect<'a>(
    v: &'a str,
    edges: &'a BTreeMap<String, BTreeSet<String>>,
    state: &mut TarjanState<'a>,
) {
    state.index.insert(v, state.index_counter);
    state.lowlink.insert(v, state.index_counter);
    state.index_counter += 1;
    state.stack.push(v);
    state.on_stack.insert(v);

    // edges[v] = dependencies of v, i.e. v -> dep.
    if let Some(deps) = edges.get(v) {
        for dep in deps {
            if !edges.contains_key(dep.as_str()) {
                // dep is not in our graph (external dependency), skip.
                continue;
            }
            if !state.index.contains_key(dep.as_str()) {
                strongconnect(dep, edges, state);
                let dep_low = state.lowlink[dep.as_str()];
                let v_low = state.lowlink[v];
                state.lowlink.insert(v, v_low.min(dep_low));
            } else if state.on_stack.contains(dep.as_str()) {
                let dep_idx = state.index[dep.as_str()];
                let v_low = state.lowlink[v];
                state.lowlink.insert(v, v_low.min(dep_idx));
            }
        }
    }

    if state.lowlink[v] == state.index[v] {
        let mut scc = Vec::new();
        loop {
            let w = state.stack.pop().unwrap();
            state.on_stack.remove(w);
            scc.push(w.to_string());
            if w == v {
                break;
            }
        }
        state.sccs.push(scc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edges(pairs: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        let mut m = BTreeMap::new();
        for (node, deps) in pairs {
            m.insert(
                node.to_string(),
                deps.iter().map(|d| d.to_string()).collect(),
            );
        }
        m
    }

    // --- topological_layers tests ---

    #[test]
    fn empty_graph() {
        let e = BTreeMap::new();
        let phases = topological_layers(&e).unwrap();
        assert!(phases.is_empty());
    }

    #[test]
    fn single_node_no_deps() {
        let e = edges(&[("a", &[])]);
        let phases = topological_layers(&e).unwrap();
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].packages, vec!["a"]);
    }

    #[test]
    fn linear_chain() {
        // c depends on b, b depends on a
        let e = edges(&[("a", &[]), ("b", &["a"]), ("c", &["b"])]);
        let phases = topological_layers(&e).unwrap();
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0].packages, vec!["a"]);
        assert_eq!(phases[1].packages, vec!["b"]);
        assert_eq!(phases[2].packages, vec!["c"]);
    }

    #[test]
    fn diamond() {
        // d depends on b and c, both depend on a
        let e = edges(&[("a", &[]), ("b", &["a"]), ("c", &["a"]), ("d", &["b", "c"])]);
        let phases = topological_layers(&e).unwrap();
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0].packages, vec!["a"]);
        assert_eq!(phases[1].packages, vec!["b", "c"]);
        assert_eq!(phases[2].packages, vec!["d"]);
    }

    #[test]
    fn parallel_independent() {
        let e = edges(&[("a", &[]), ("b", &[]), ("c", &[])]);
        let phases = topological_layers(&e).unwrap();
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].packages, vec!["a", "b", "c"]);
    }

    #[test]
    fn cycle_returns_err() {
        let e = edges(&[("a", &["b"]), ("b", &["a"])]);
        let err = topological_layers(&e).unwrap_err();
        assert_eq!(err.len(), 2);
    }

    #[test]
    fn deps_outside_graph_ignored() {
        // a depends on "external" which is not in the graph
        let e = edges(&[("a", &["external"]), ("b", &["a"])]);
        let phases = topological_layers(&e).unwrap();
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].packages, vec!["a"]);
        assert_eq!(phases[1].packages, vec!["b"]);
    }

    #[test]
    fn complex_dag() {
        // e depends on c,d; c depends on a,b; d depends on b
        let e = edges(&[
            ("a", &[]),
            ("b", &[]),
            ("c", &["a", "b"]),
            ("d", &["b"]),
            ("e", &["c", "d"]),
        ]);
        let phases = topological_layers(&e).unwrap();
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0].packages, vec!["a", "b"]);
        assert_eq!(phases[1].packages, vec!["c", "d"]);
        assert_eq!(phases[2].packages, vec!["e"]);
    }

    // --- find_cycles tests ---

    #[test]
    fn no_cycles_in_dag() {
        let e = edges(&[("a", &[]), ("b", &["a"]), ("c", &["b"])]);
        let cycles = find_cycles(&e);
        assert!(cycles.is_empty());
    }

    #[test]
    fn simple_cycle() {
        let e = edges(&[("a", &["b"]), ("b", &["a"])]);
        let cycles = find_cycles(&e);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].packages, vec!["a", "b"]);
    }

    #[test]
    fn triangle_cycle() {
        let e = edges(&[("a", &["b"]), ("b", &["c"]), ("c", &["a"])]);
        let cycles = find_cycles(&e);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].packages, vec!["a", "b", "c"]);
    }

    #[test]
    fn multiple_cycles() {
        let e = edges(&[("a", &["b"]), ("b", &["a"]), ("c", &["d"]), ("d", &["c"])]);
        let cycles = find_cycles(&e);
        assert_eq!(cycles.len(), 2);
    }

    #[test]
    fn cycle_with_tail() {
        // d depends on c (no cycle), c and b cycle, b depends on a (no cycle)
        let e = edges(&[("a", &[]), ("b", &["a", "c"]), ("c", &["b"]), ("d", &["c"])]);
        let cycles = find_cycles(&e);
        assert_eq!(cycles.len(), 1);
        assert_eq!(cycles[0].packages, vec!["b", "c"]);
    }

    #[test]
    fn no_cycles_empty_graph() {
        let e = BTreeMap::new();
        let cycles = find_cycles(&e);
        assert!(cycles.is_empty());
    }

    #[test]
    fn single_node_no_cycle() {
        let e = edges(&[("a", &[])]);
        let cycles = find_cycles(&e);
        assert!(cycles.is_empty());
    }
}
