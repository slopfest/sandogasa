// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The wave loop shared by every dependency-closure walk.
//!
//! A walk is breadth-first over *nodes* (binary packages, `src:`
//! pseudo-entries, source packages — whatever the policy chooses),
//! each carrying the raw requirements it contributes. The engine owns
//! what every walk has in common and nothing else: the seen-set of
//! nodes, the seen-set of capabilities with first-requirer
//! attribution, batching a wave's unresolved capabilities into one
//! resolution call, per-node depth, wave counting, and the
//! end-of-walk hook that lets a policy grow the roots (a fixpoint) or
//! commit deferred decisions.
//!
//! Everything that differs between walks stays in the [`Policy`]:
//! how roots expand, which raw requirements count and as what
//! capabilities, what a resolved provider *means* (terminate, collect,
//! follow), and — deliberately — the order in which a wave's results
//! are absorbed, because attribution depends on it. poi-tracker
//! attributes provider-major (the first provider of a source, in the
//! resolver's order, names the reason); ebranch attributes
//! requirer-major (a package's missing deps in its own BuildRequires
//! order). Both are right for their report, and neither could be the
//! engine's default without changing the other's output.

use std::collections::{BTreeMap, BTreeSet};

/// One package in a wave.
#[derive(Debug, Clone)]
pub struct Node<M> {
    /// Identity in the seen-set: a binary name, `src:<source>`, a
    /// source name — whatever the policy walks over.
    pub key: String,
    /// Distance from the roots (roots are 1). Set by the engine.
    pub depth: usize,
    /// Raw requirements, if the resolver already delivered them with
    /// the node; `None` asks the policy to [`Policy::expand`] it.
    pub requires: Option<Vec<String>>,
    /// Whatever the policy needs to carry along (a `PkgInfo`, or
    /// nothing).
    pub meta: M,
}

/// A wave's successors: each node with the key of the node it was
/// reached from (for depth bookkeeping), or `None` for a root-like
/// arrival.
pub type NextWave<M> = Vec<(Node<M>, Option<String>)>;

/// What a walk does at each step. See the module docs for the split
/// between engine and policy.
pub trait Policy {
    /// Per-node payload.
    type Meta: Clone;
    /// One resolved result, as [`Policy::resolve`] returns them and
    /// [`Policy::absorb`] consumes them.
    type Hit;

    /// The first wave, from the roots.
    fn seed(&mut self, roots: &[String]) -> Result<Vec<Node<Self::Meta>>, String>;

    /// Nodes deeper than this are not visited (0 = unlimited).
    fn max_depth(&self) -> usize {
        0
    }

    /// A wave is about to be ingested — the nodes that survived the
    /// seen and depth filters, in wave order.
    fn begin_wave(&mut self, _nodes: &[Node<Self::Meta>]) {}

    /// Raw requirements for nodes that arrived without them, keyed by
    /// node. A node missing from the answer has none.
    fn expand(&mut self, _keys: &[String]) -> Result<BTreeMap<String, Vec<String>>, String> {
        Ok(BTreeMap::new())
    }

    /// A node is being visited for the first time.
    fn ingest(&mut self, _node: &Node<Self::Meta>) {}

    /// The capabilities one raw requirement of `node` asks the
    /// resolver for — none to skip it, several for a rich dependency's
    /// leaves. Called for every requirement, seen or not, so a policy
    /// can record requirer edges here.
    fn capabilities(&mut self, node: &Node<Self::Meta>, raw: &str) -> Vec<String>;

    /// Resolve a wave's not-yet-seen capabilities, batched. `wave` is
    /// the 1-based count of resolving waves so far.
    fn resolve(&self, wave: usize, caps: &[String]) -> Result<Vec<Self::Hit>, String>;

    /// Consume a wave's results and return the next wave — each node
    /// with the key of the node it was reached from, for depth
    /// bookkeeping. `pending` maps every capability resolved this
    /// wave to the node that first required it. Called for every
    /// ingested wave, with no hits when nothing needed resolving.
    fn absorb(
        &mut self,
        hits: Vec<Self::Hit>,
        pending: &BTreeMap<String, String>,
    ) -> Result<NextWave<Self::Meta>, String>;

    /// The walk has nothing left to visit: nodes to continue with
    /// (fixpoint roots, deferred approvals), or none to finish.
    fn converged(&mut self) -> Result<Vec<Node<Self::Meta>>, String> {
        Ok(vec![])
    }
}

/// What the engine itself counted.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WalkStats {
    /// Waves that resolved at least one capability.
    pub waves: usize,
    /// Distinct nodes visited.
    pub nodes_seen: usize,
}

/// Run a walk from `roots` under `policy`.
pub fn run<P: Policy>(policy: &mut P, roots: &[String]) -> Result<WalkStats, String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut seen_caps: BTreeSet<String> = BTreeSet::new();
    let mut depth: BTreeMap<String, usize> = BTreeMap::new();
    let mut stats = WalkStats::default();

    let mut wave: Vec<Node<P::Meta>> = policy.seed(roots)?;
    for node in &mut wave {
        node.depth = 1;
        depth.entry(node.key.clone()).or_insert(1);
    }

    loop {
        // Only unvisited nodes within the depth limit are ingested;
        // the first of several same-key nodes in a wave wins.
        let max_depth = policy.max_depth();
        let mut in_wave: BTreeSet<String> = BTreeSet::new();
        let mut to_process: Vec<Node<P::Meta>> = Vec::new();
        for mut node in wave {
            node.depth = depth.get(&node.key).copied().unwrap_or(node.depth);
            if seen.contains(&node.key)
                || (max_depth > 0 && node.depth > max_depth)
                || !in_wave.insert(node.key.clone())
            {
                continue;
            }
            to_process.push(node);
        }
        if to_process.is_empty() {
            let extra = policy.converged()?;
            if extra.is_empty() {
                break;
            }
            wave = extra;
            for node in &mut wave {
                depth.entry(node.key.clone()).or_insert(1);
            }
            continue;
        }
        policy.begin_wave(&to_process);

        let need: Vec<String> = to_process
            .iter()
            .filter(|n| n.requires.is_none())
            .map(|n| n.key.clone())
            .collect();
        if !need.is_empty() {
            let mut expanded = policy.expand(&need)?;
            for node in &mut to_process {
                if node.requires.is_none() {
                    node.requires = Some(expanded.remove(&node.key).unwrap_or_default());
                }
            }
        }

        // Ingest: every node's requirements become capabilities; the
        // unseen ones queue for resolution, attributed to whoever
        // asked first.
        let mut pending: BTreeMap<String, String> = BTreeMap::new();
        for node in &to_process {
            seen.insert(node.key.clone());
            policy.ingest(node);
            for raw in node.requires.as_deref().unwrap_or_default() {
                for cap in policy.capabilities(node, raw) {
                    if seen_caps.contains(&cap) {
                        continue;
                    }
                    pending.entry(cap).or_insert_with(|| node.key.clone());
                }
            }
        }

        let hits = if pending.is_empty() {
            Vec::new()
        } else {
            stats.waves += 1;
            let caps: Vec<String> = pending.keys().cloned().collect();
            seen_caps.extend(caps.iter().cloned());
            policy.resolve(stats.waves, &caps)?
        };

        let next = policy.absorb(hits, &pending)?;
        wave = Vec::with_capacity(next.len());
        for (node, from) in next {
            let d = from
                .and_then(|f| depth.get(&f).copied())
                .map_or(1, |d| d + 1);
            depth.entry(node.key.clone()).or_insert(d);
            wave.push(node);
        }
    }

    stats.nodes_seen = seen.len();
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A toy policy over a static graph: node → requirements, and
    /// requirement → the node that provides it.
    struct Toy {
        requires: BTreeMap<&'static str, Vec<&'static str>>,
        provides: BTreeMap<&'static str, &'static str>,
        max_depth: usize,
        log: Vec<String>,
        resolved: Vec<String>,
        extra_rounds: Vec<Vec<&'static str>>,
    }

    impl Policy for Toy {
        type Meta = ();
        type Hit = (String, String);

        fn seed(&mut self, roots: &[String]) -> Result<Vec<Node<()>>, String> {
            Ok(roots
                .iter()
                .map(|r| Node {
                    key: r.clone(),
                    depth: 0,
                    requires: None,
                    meta: (),
                })
                .collect())
        }
        fn max_depth(&self) -> usize {
            self.max_depth
        }
        fn begin_wave(&mut self, nodes: &[Node<()>]) {
            let keys: Vec<&str> = nodes.iter().map(|n| n.key.as_str()).collect();
            self.log.push(format!("wave {}", keys.join(",")));
        }
        fn expand(&mut self, keys: &[String]) -> Result<BTreeMap<String, Vec<String>>, String> {
            Ok(keys
                .iter()
                .map(|k| {
                    let reqs = self
                        .requires
                        .get(k.as_str())
                        .map(|v| v.iter().map(|s| s.to_string()).collect())
                        .unwrap_or_default();
                    (k.clone(), reqs)
                })
                .collect())
        }
        fn capabilities(&mut self, _node: &Node<()>, raw: &str) -> Vec<String> {
            if raw.starts_with("rpmlib(") {
                vec![]
            } else {
                vec![raw.to_string()]
            }
        }
        fn resolve(&self, _wave: usize, caps: &[String]) -> Result<Vec<(String, String)>, String> {
            Ok(caps
                .iter()
                .filter_map(|c| {
                    self.provides
                        .get(c.as_str())
                        .map(|p| (c.clone(), p.to_string()))
                })
                .collect())
        }
        fn absorb(
            &mut self,
            hits: Vec<(String, String)>,
            pending: &BTreeMap<String, String>,
        ) -> Result<NextWave<()>, String> {
            let mut next = Vec::new();
            for (cap, provider) in hits {
                self.resolved.push(format!("{cap}<-{provider}"));
                next.push((
                    Node {
                        key: provider,
                        depth: 0,
                        requires: None,
                        meta: (),
                    },
                    pending.get(&cap).cloned(),
                ));
            }
            Ok(next)
        }
        fn converged(&mut self) -> Result<Vec<Node<()>>, String> {
            if self.extra_rounds.is_empty() {
                return Ok(vec![]);
            }
            let round = self.extra_rounds.remove(0);
            Ok(round
                .into_iter()
                .map(|k| Node {
                    key: k.to_string(),
                    depth: 0,
                    requires: None,
                    meta: (),
                })
                .collect())
        }
    }

    fn toy() -> Toy {
        Toy {
            requires: BTreeMap::from([
                ("app", vec!["liba", "rpmlib(X)"]),
                ("a", vec!["libb", "liba"]),
                ("b", vec![]),
            ]),
            provides: BTreeMap::from([("liba", "a"), ("libb", "b")]),
            max_depth: 0,
            log: vec![],
            resolved: vec![],
            extra_rounds: vec![],
        }
    }

    #[test]
    fn walks_breadth_first_resolving_each_capability_once() {
        let mut p = toy();
        let stats = run(&mut p, &["app".to_string()]).unwrap();
        assert_eq!(p.log, ["wave app", "wave a", "wave b"]);
        // liba is required again by `a` but was resolved in wave 1.
        assert_eq!(p.resolved, ["liba<-a", "libb<-b"]);
        assert_eq!(
            stats,
            WalkStats {
                waves: 2,
                nodes_seen: 3
            }
        );
    }

    #[test]
    fn depth_limit_stops_visiting_but_not_counting_what_was_reached() {
        let mut p = toy();
        p.max_depth = 2;
        let stats = run(&mut p, &["app".to_string()]).unwrap();
        // app (1) → a (2) → b (3, beyond the limit: never ingested).
        assert_eq!(p.log, ["wave app", "wave a"]);
        assert_eq!(stats.nodes_seen, 2);
    }

    #[test]
    fn converged_rounds_reuse_the_seen_sets() {
        let mut p = toy();
        // After the walk settles, a round seeds `b` again and a fresh
        // `c` that needs liba: liba is already resolved, so only c is
        // visited and no new wave counts.
        p.requires.insert("c", vec!["liba"]);
        p.extra_rounds = vec![vec!["b", "c"]];
        let stats = run(&mut p, &["app".to_string()]).unwrap();
        assert_eq!(p.log, ["wave app", "wave a", "wave b", "wave c"]);
        assert_eq!(
            stats,
            WalkStats {
                waves: 2,
                nodes_seen: 4
            }
        );
    }
}
