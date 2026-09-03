// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dependency-closure walking over fedrq, with a persisted graph and
//! an offline oracle.
//!
//! [`walk`] is a breadth-first traversal over *binary* packages, one
//! batched fedrq query per wave (split across the CPUs when a wave is
//! large): roots expand to their subpackages (and, with build
//! seeding, to a `src:` pseudo-entry carrying their BuildRequires),
//! their Requires become the wave's capabilities, the capabilities
//! resolve to providers whose own Requires seed the next wave.
//! Providers from the base distro terminate a path; the rest are
//! walked further and collected when their repo is one of the repos
//! of interest. An optional fixpoint turns newly collected packages
//! the caller owns into roots of their own.
//!
//! "Capability" throughout is RPM's term for the strings in
//! `Provides:`/`Requires:` — package names, versioned expressions
//! (`python3dist(zstandard)`), sonames, file paths. Requires name
//! capabilities, not packages.
//!
//! Every edge the walk visits is recorded in a [`DepsGraph`], which
//! answers reachability, dependents and witness-edge questions
//! offline, and backs [`GraphBackedQuery`] — a [`PkgQuery`] that
//! answers from the saved graph wherever it can and asks the real
//! backend only for the frontier. The rule of the house: roots always
//! expand live, because a package the graph met as a dependency
//! carries only the feature subpackages something requested.
//!
//! Measured shape (poi-tracker, 2026-09): fedrq costs ~0.4s per
//! capability resolved and ~1s per spawn for the repo sack, so a
//! closure is a handful of spawns and an incremental walk over a
//! saved graph is seconds where a full one is an hour.

use std::collections::{BTreeMap, BTreeSet};

use rayon::prelude::*;
pub use sandogasa_fedrq::PkgInfo;

pub mod engine;
use serde::Serialize;

/// Source of batched package facts — [`sandogasa_fedrq::Fedrq`] in
/// production, canned fixtures in tests.
pub trait PkgQuery: Sync {
    /// Binary subpackages of the given source packages, with their
    /// Requires, source attribution and providing repo.
    fn subpkgs(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, String>;
    /// The given source packages themselves, whose Requires are
    /// their BuildRequires.
    fn srcs(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, String>;
    /// Providers of the given capabilities — the candidates dnf would
    /// pick — with their own Requires, Provides, source and repo.
    fn providers(&self, deps: &[String]) -> Result<Vec<PkgInfo>, String>;
}

impl PkgQuery for sandogasa_fedrq::Fedrq {
    fn subpkgs(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, String> {
        self.subpkgs_info(srpms).map_err(|e| e.to_string())
    }
    fn srcs(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, String> {
        self.srcs_info(srpms).map_err(|e| e.to_string())
    }
    fn providers(&self, deps: &[String]) -> Result<Vec<PkgInfo>, String> {
        self.providers_info(deps).map_err(|e| e.to_string())
    }
}

/// The full dependency graph a walk traversed, for persistence:
/// where the report keeps one attribution per collected package, the
/// graph keeps every edge, which is what incremental maintenance
/// needs — removing a root means asking "does anything else still
/// reach this?", a reachability question over all edges, not the
/// first-met one.
///
/// Normalized: capabilities map to every binary that required them
/// and every provider that satisfied them; `binary_sources` maps
/// binaries (and `src:`-prefixed sources) back to source packages.
#[derive(Debug, Default, Serialize, serde::Deserialize)]
pub struct DepsGraph {
    /// Source packages the walk started from.
    pub roots: Vec<String>,
    /// Binary name → source package name.
    pub binary_sources: BTreeMap<String, String>,
    /// Capability → binaries whose Requires named it.
    pub requirers: BTreeMap<String, BTreeSet<String>>,
    /// Capability → providers that satisfied it.
    pub providers: BTreeMap<String, BTreeSet<GraphProvider>>,
}

impl DepsGraph {
    /// Dependent sources per source: for every capability, each
    /// provider's source gains every requirer's source, with
    /// self-edges dropped and `src:` pseudo-requirers mapped back to
    /// the source whose BuildRequires they carry.
    pub fn dependents(&self) -> BTreeMap<String, BTreeSet<String>> {
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (capability, providers) in &self.providers {
            for requirer in self.requirers.get(capability).into_iter().flatten() {
                let source = self.binary_sources.get(requirer).unwrap_or(requirer);
                for p in providers {
                    if &p.source != source {
                        out.entry(p.source.clone())
                            .or_default()
                            .insert(source.clone());
                    }
                }
            }
        }
        out
    }

    /// Entity → the capabilities it required, per the recorded edges:
    /// the inverse of `requirers`. For a `src:<name>` entity that is a
    /// source's BuildRequires, complete whenever the entity exists
    /// (BuildRequires are per source, not per feature).
    pub fn requires_by_entity(&self) -> BTreeMap<&str, Vec<&str>> {
        let mut out: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (cap, reqs) in &self.requirers {
            for r in reqs {
                out.entry(r.as_str()).or_default().push(cap.as_str());
            }
        }
        out
    }

    /// Sources reachable from `keeps` over the recorded edges. Every
    /// recorded provider of an active capability counts, and a
    /// root's `src:` (BuildRequires) edges activate only once the
    /// root itself is reached.
    pub fn reachable(&self, keeps: &BTreeSet<String>) -> BTreeSet<String> {
        // Source → the walk entities (binaries, `src:` pseudo-
        // entries) whose Requires it owns.
        let mut entities: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (binary, source) in &self.binary_sources {
            entities.entry(source).or_default().push(binary);
        }
        // Entity → capabilities it required.
        let mut requires: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (capability, requirers) in &self.requirers {
            for requirer in requirers {
                requires.entry(requirer).or_default().push(capability);
            }
        }
        let mut reached: BTreeSet<String> = keeps.clone();
        let mut frontier: Vec<String> = keeps.iter().cloned().collect();
        while let Some(source) = frontier.pop() {
            for entity in entities.get(source.as_str()).into_iter().flatten() {
                for capability in requires.get(entity).into_iter().flatten() {
                    for provider in self.providers.get(*capability).into_iter().flatten() {
                        if reached.insert(provider.source.clone()) {
                            frontier.push(provider.source.clone());
                        }
                    }
                }
            }
        }
        reached
    }

    /// One witness edge for `name` whose requirer is in `within` —
    /// the `reason` a walk would have written for it.
    pub fn witness_reason(&self, name: &str, within: &BTreeSet<String>) -> Option<String> {
        for (capability, providers) in &self.providers {
            let Some(p) = providers.iter().find(|p| p.source == name) else {
                continue;
            };
            for requirer in self.requirers.get(capability).into_iter().flatten() {
                let source = self.binary_sources.get(requirer).unwrap_or(requirer);
                if source != name && within.contains(source) {
                    return Some(format!(
                        "dependency ({}): {requirer} requires {capability}",
                        p.repoid
                    ));
                }
            }
        }
        None
    }
}

impl DepsGraph {
    /// Fold another walk's graph into this one: roots, binary
    /// attribution and every edge, union-wise. This is how an
    /// incremental walk (one new root) extends a stored graph instead
    /// of replacing it.
    pub fn merge(&mut self, other: DepsGraph) {
        for root in other.roots {
            if !self.roots.contains(&root) {
                self.roots.push(root);
            }
        }
        self.binary_sources.extend(other.binary_sources);
        for (cap, reqs) in other.requirers {
            self.requirers.entry(cap).or_default().extend(reqs);
        }
        for (cap, provs) in other.providers {
            self.providers.entry(cap).or_default().extend(provs);
        }
    }
}

/// A [`PkgQuery`] that answers from a saved graph wherever it can and
/// asks the real query only for what the graph has never seen.
///
/// Capabilities the graph already resolved come back as the recorded
/// providers, their Requires reconstructed from the inverted edge map
/// — so a new root's walk cascades offline through the known part of
/// the world and pays fedrq's per-capability price only at the
/// frontier. Root expansion (`subpkgs`/`srcs`) always goes live: a
/// package the graph met as a *collected* dependency carries only the
/// feature subpackages something requested, and a root must expand
/// every feature (the asymmetry that once made four crates look
/// dependent-free).
pub struct GraphBackedQuery<'a, Q: PkgQuery> {
    inner: &'a Q,
    graph: &'a DepsGraph,
    /// Entity → the capabilities it required, per the graph.
    requires_of: BTreeMap<&'a str, Vec<&'a str>>,
    /// Binary → the capabilities it provided, per the graph.
    provides_of: BTreeMap<&'a str, Vec<&'a str>>,
    /// Capabilities answered from the graph / sent to the inner query.
    pub offline: std::sync::atomic::AtomicUsize,
    pub online: std::sync::atomic::AtomicUsize,
}

impl<'a, Q: PkgQuery> GraphBackedQuery<'a, Q> {
    pub fn new(inner: &'a Q, graph: &'a DepsGraph) -> Self {
        let requires_of = graph.requires_by_entity();
        let mut provides_of: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (cap, provs) in &graph.providers {
            for p in provs {
                provides_of
                    .entry(p.binary.as_str())
                    .or_default()
                    .push(cap.as_str());
            }
        }
        Self {
            inner,
            graph,
            requires_of,
            provides_of,
            offline: Default::default(),
            online: Default::default(),
        }
    }
}

impl<Q: PkgQuery> PkgQuery for GraphBackedQuery<'_, Q> {
    fn subpkgs(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, String> {
        self.inner.subpkgs(srpms)
    }
    fn srcs(&self, srpms: &[String]) -> Result<Vec<PkgInfo>, String> {
        self.inner.srcs(srpms)
    }
    fn providers(&self, deps: &[String]) -> Result<Vec<PkgInfo>, String> {
        use std::sync::atomic::Ordering::Relaxed;
        let mut known: BTreeMap<&str, PkgInfo> = BTreeMap::new();
        let mut unknown: Vec<String> = Vec::new();
        for dep in deps {
            match self.graph.providers.get(dep).filter(|p| !p.is_empty()) {
                None => unknown.push(dep.clone()),
                Some(provs) => {
                    for gp in provs {
                        let entry = known.entry(gp.binary.as_str()).or_insert_with(|| {
                            let strings = |m: &BTreeMap<&str, Vec<&str>>| -> Vec<String> {
                                m.get(gp.binary.as_str())
                                    .map(|v| v.iter().map(|c| c.to_string()).collect())
                                    .unwrap_or_default()
                            };
                            PkgInfo::new(
                                gp.binary.clone(),
                                strings(&self.requires_of),
                                strings(&self.provides_of),
                                Some(gp.source.clone()),
                                gp.repoid.clone(),
                            )
                        });
                        if !entry.provides.iter().any(|c| c == dep) {
                            entry.provides.push(dep.clone());
                        }
                    }
                }
            }
        }
        self.offline.fetch_add(deps.len() - unknown.len(), Relaxed);
        self.online.fetch_add(unknown.len(), Relaxed);
        let mut out: Vec<PkgInfo> = known.into_values().collect();
        if !unknown.is_empty() {
            out.extend(self.inner.providers(&unknown)?);
        }
        Ok(out)
    }
}

/// One provider of a capability, as the graph records it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize)]
pub struct GraphProvider {
    pub binary: String,
    pub source: String,
    pub repoid: String,
}

/// One collected dependency: a source package the inventory
/// transitively requires at runtime, pulled from a repo of interest.
#[derive(Debug, Clone, Serialize)]
pub struct CollectedDep {
    /// Source package name.
    pub source: String,
    /// Repository it came from (e.g. `epel`).
    pub repoid: String,
    /// Binary that first pulled it in — empty when the capability
    /// could not be tied back (a file dependency, say).
    pub required_by: String,
    /// The capability that pulled it in; empty when unattributable.
    pub via: String,
}

/// The walk's result.
#[derive(Debug, Default, Serialize)]
pub struct DepsReport {
    /// Source packages the walk started from.
    pub roots: usize,
    /// Dependency-graph levels walked (= batched resolution queries).
    pub waves: usize,
    /// Distinct binary packages whose Requires were examined.
    pub binaries_walked: usize,
    /// Collected dependencies, sorted by source name.
    pub collected: Vec<CollectedDep>,
    /// Capabilities no provider could be tied to — either genuinely
    /// unresolvable in the stack or beyond the local matcher (file
    /// dependencies without their provider's file list, rich deps).
    pub unmatched: Vec<String>,
    /// Wall-clock seconds the walk took, resolution queries
    /// included.
    pub elapsed_secs: f64,
    /// Fixpoint rounds taken beyond the initial walk (0 without
    /// `--fixpoint`, or when the first walk already covered every
    /// owned package).
    pub fixpoint_rounds: usize,
    /// Non-fatal oddities worth a human glance.
    pub warnings: Vec<String>,
}

/// Whether `dep` should be resolved at all. RPM-internal and
/// auto-generated capabilities are noise; symbol-version deps always
/// ride alongside a bare soname dep that resolves to the same
/// provider; rich (boolean) deps go through [`rich_dep_leaves`]
/// first and reach here as plain leaves.
fn wants_resolution(dep: &str) -> bool {
    !sandogasa_depfilter::is_rpm_internal_dep(dep)
        && !sandogasa_depfilter::is_solib_symbol_dep(dep)
        && !dep.starts_with('(')
}

/// The leaf dependency strings of a rich (boolean) dependency, when
/// every connective in it is a conjunction (`and`, `with`) — then
/// each leaf is genuinely required and resolvable on its own. This
/// is the shape rust-packaging emits for every crate version range,
/// `(crate(x) >= 1.2 with crate(x) < 2.0~)`, so skipping it would
/// drop the whole crate graph of any shipped -devel subpackage.
///
/// `None` for expressions with a conditional or alternative
/// connective (`or`, `if`, `else`, `unless`, `without`): which
/// branch applies depends on install state, and resolving all of
/// them would overstate the closure. The caller warns and skips.
fn rich_dep_leaves(dep: &str) -> Option<Vec<String>> {
    let inner = dep.strip_prefix('(')?.strip_suffix(')')?;
    let mut leaves: Vec<String> = Vec::new();
    let mut segment = String::new();
    let mut depth: i32 = 0;
    // Split into words, tracking parenthesis depth: connectives are
    // bare words at depth 0; anything else (including versioned
    // leaves like `crate(x) >= 1.2`, whose own parentheses raise the
    // depth only mid-word) accumulates into the current segment.
    let flush = |segment: &mut String, leaves: &mut Vec<String>| -> Option<()> {
        let seg = std::mem::take(segment);
        let seg = seg.trim();
        if seg.is_empty() {
            return None; // malformed: empty operand
        }
        if seg.starts_with('(') {
            leaves.extend(rich_dep_leaves(seg)?);
        } else {
            leaves.push(seg.to_string());
        }
        Some(())
    };
    for word in inner.split(' ') {
        if depth == 0 && matches!(word, "and" | "with") {
            flush(&mut segment, &mut leaves)?;
            continue;
        }
        if depth == 0 && matches!(word, "or" | "if" | "else" | "unless" | "without") {
            return None;
        }
        depth += word.matches('(').count() as i32 - word.matches(')').count() as i32;
        if !segment.is_empty() {
            segment.push(' ');
        }
        segment.push_str(word);
    }
    flush(&mut segment, &mut leaves)?;
    Some(leaves)
}

/// Walk the runtime closure of `roots` (source package names).
///
/// `from` — repo ids whose providers are collected. `base_prefixes` —
/// repo id prefixes treated as the base distro: their providers
/// satisfy a dependency silently and are not walked further. Empty
/// prefixes are ignored — `starts_with("")` holds for every repoid,
/// so a stray `--base-repo=` would otherwise classify *everything*
/// as base and return an empty report. Roots are never collected,
/// whatever repo they resolve from.
/// What a walk should do, beyond its roots.
pub struct WalkOpts<'a> {
    /// Repo ids whose providers are collected.
    pub from: &'a BTreeSet<String>,
    /// Repo id prefixes whose providers end the walk (the base
    /// distro is a given). Empty prefixes are ignored.
    pub base_prefixes: &'a [String],
    /// Seed the roots' own BuildRequires too, attributed
    /// `src:<name>`: a kept application then justifies what it
    /// builds with.
    pub build: bool,
    /// The fixpoint: names "our own" packages (an inventory's).
    /// After the walk converges, collected packages found in this
    /// set become additional roots — they are ours to keep
    /// rebuildable, so their BuildRequires seed another round —
    /// repeating until a round adds nothing. Requires `build`.
    /// Rounds reuse the walk's seen-sets, so each costs only the
    /// genuinely new capabilities: measured against the manual
    /// three-invocation loop (~90 min), the in-process fixpoint is
    /// the first walk plus small change.
    pub fixpoint_own: Option<&'a BTreeSet<String>>,
    pub verbose: bool,
}

/// Walk the closure from `roots`: the shared [`engine`] loop under
/// the deps policy — collect providers from the repos of interest,
/// terminate at the base distro, record every edge, grow the roots
/// by fixpoint when asked. Returns the report (first attributions)
/// and the graph (all edges).
pub fn walk(
    query: &impl PkgQuery,
    roots: &[String],
    opts: &WalkOpts,
) -> Result<(DepsReport, DepsGraph), String> {
    let mut policy = DepsPolicy {
        query,
        from: opts.from,
        base_prefixes: opts
            .base_prefixes
            .iter()
            .filter(|b| !b.is_empty())
            .map(String::as_str)
            .collect(),
        build: opts.build,
        fixpoint_own: opts.fixpoint_own,
        verbose: opts.verbose,
        root_sources: roots.iter().cloned().collect(),
        fixpoint_roots: roots.iter().cloned().collect(),
        report: DepsReport {
            roots: roots.len(),
            ..Default::default()
        },
        graph: DepsGraph {
            roots: roots.to_vec(),
            ..Default::default()
        },
        collected: BTreeMap::new(),
        rich_warned: BTreeSet::new(),
        started: std::time::Instant::now(),
    };
    let stats = engine::run(&mut policy, roots)?;
    let DepsPolicy {
        mut report,
        graph,
        collected,
        started,
        ..
    } = policy;
    report.waves = stats.waves;
    report.binaries_walked = stats.nodes_seen;
    report.collected = collected.into_values().collect();
    report.elapsed_secs = started.elapsed().as_secs_f64();
    Ok((report, graph))
}

/// The deps walk's policy: binaries and `src:` entries as nodes,
/// providers followed unless the base distro satisfies them,
/// collected when their repo is one of interest.
struct DepsPolicy<'a, Q: PkgQuery> {
    query: &'a Q,
    from: &'a BTreeSet<String>,
    base_prefixes: Vec<&'a str>,
    build: bool,
    fixpoint_own: Option<&'a BTreeSet<String>>,
    verbose: bool,
    root_sources: BTreeSet<String>,
    fixpoint_roots: BTreeSet<String>,
    report: DepsReport,
    graph: DepsGraph,
    collected: BTreeMap<String, CollectedDep>,
    rich_warned: BTreeSet<String>,
    started: std::time::Instant,
}

impl<Q: PkgQuery> DepsPolicy<'_, Q> {
    fn node(p: PkgInfo) -> engine::Node<PkgInfo> {
        engine::Node {
            key: p.name.clone(),
            depth: 0,
            requires: Some(p.requires.clone()),
            meta: p,
        }
    }

    /// Sources as `src:` nodes: their Requires are BuildRequires and
    /// flow through the same filtering and resolution, and the prefix
    /// keeps a source from shadowing its same-named main binary in
    /// the seen-set (and labels the attribution in reports).
    fn src_nodes(&self, sources: &[String]) -> Result<Vec<engine::Node<PkgInfo>>, String> {
        let mut out = Vec::new();
        for mut src in self.query.srcs(sources)? {
            src.name = format!("src:{}", src.name);
            out.push(Self::node(src));
        }
        Ok(out)
    }
}

impl<Q: PkgQuery> engine::Policy for DepsPolicy<'_, Q> {
    type Meta = PkgInfo;
    type Hit = PkgInfo;

    fn seed(&mut self, roots: &[String]) -> Result<Vec<engine::Node<PkgInfo>>, String> {
        if self.verbose {
            eprintln!("[deps] expanding {} root source(s)", roots.len());
        }
        let mut wave: Vec<engine::Node<PkgInfo>> = self
            .query
            .subpkgs(roots)?
            .into_iter()
            .map(Self::node)
            .collect();
        if self.build {
            wave.extend(self.src_nodes(roots)?);
        }
        if self.verbose {
            eprintln!(
                "[deps] {} binaries in {:.1}s{}",
                wave.len(),
                self.started.elapsed().as_secs_f64(),
                if self.build {
                    " (sources included)"
                } else {
                    ""
                },
            );
        }
        Ok(wave)
    }

    fn ingest(&mut self, node: &engine::Node<PkgInfo>) {
        if let Some(src) = &node.meta.source_name {
            self.graph
                .binary_sources
                .insert(node.key.clone(), src.clone());
        } else if let Some(root) = node.key.strip_prefix("src:") {
            self.graph
                .binary_sources
                .insert(node.key.clone(), root.to_string());
        }
    }

    fn capabilities(&mut self, node: &engine::Node<PkgInfo>, dep: &str) -> Vec<String> {
        // A rich dep made only of conjunctions contributes its leaves
        // as ordinary requirements; conditional ones are skipped with
        // a warning.
        let leaves = if dep.starts_with('(') {
            match rich_dep_leaves(dep) {
                Some(leaves) => leaves,
                None => {
                    if self.rich_warned.insert(dep.to_string()) {
                        self.report
                            .warnings
                            .push(format!("rich dependency skipped: {dep} ({})", node.key));
                    }
                    return vec![];
                }
            }
        } else {
            vec![dep.to_string()]
        };
        let mut out = Vec::new();
        for leaf in leaves {
            if !wants_resolution(&leaf) {
                continue;
            }
            // The graph keeps every requirer edge, even for a
            // capability someone already asked about — the engine's
            // pending queue only carries the unresolved.
            self.graph
                .requirers
                .entry(leaf.clone())
                .or_default()
                .insert(node.key.clone());
            out.push(leaf);
        }
        out
    }

    fn resolve(&self, wave: usize, reqs: &[String]) -> Result<Vec<PkgInfo>, String> {
        if self.verbose {
            eprintln!(
                "[deps] wave {}: resolving {} new capabilit{}",
                wave,
                reqs.len(),
                if reqs.len() == 1 { "y" } else { "ies" }
            );
        }
        let wave_started = std::time::Instant::now();
        // Resolution cost is per capability, so a large wave splits
        // across the rayon pool. The ordered collect keeps runs
        // reproducible and the collected *set* is chunk-invariant
        // (verified against a sequential run of the full Hyperscale
        // inventory: identical packages); only the `reason`
        // attribution can differ across chunkings, when a source has
        // several matching binaries and a different one is met
        // first — every such attribution is true.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let chunk_size = reqs.len().div_ceil(threads).max(32);
        let providers: Vec<PkgInfo> = reqs
            .par_chunks(chunk_size)
            .map(|chunk| self.query.providers(chunk))
            .collect::<Result<Vec<_>, String>>()?
            .into_iter()
            .flatten()
            .collect();
        if self.verbose {
            eprintln!(
                "[deps] wave {}: {} provider(s) in {:.1}s ({} batch(es))",
                wave,
                providers.len(),
                wave_started.elapsed().as_secs_f64(),
                reqs.len().div_ceil(chunk_size),
            );
        }
        Ok(providers)
    }

    fn absorb(
        &mut self,
        providers: Vec<PkgInfo>,
        pending: &BTreeMap<String, String>,
    ) -> Result<engine::NextWave<PkgInfo>, String> {
        let reqs: Vec<&str> = pending.keys().map(String::as_str).collect();
        let mut matched: BTreeSet<&str> = BTreeSet::new();
        let mut next = Vec::new();
        for p in providers {
            // Which of this wave's capabilities did this provider
            // satisfy? Attribution is best-effort: the first match
            // names the requirer in the report.
            let mine: Vec<&str> = reqs.iter().copied().filter(|r| p.satisfies(r)).collect();
            matched.extend(mine.iter().copied());
            if let Some(src) = &p.source_name {
                for req in &mine {
                    self.graph
                        .providers
                        .entry(req.to_string())
                        .or_default()
                        .insert(GraphProvider {
                            binary: p.name.clone(),
                            source: src.clone(),
                            repoid: p.repoid.clone(),
                        });
                }
            }

            if self.base_prefixes.iter().any(|b| p.repoid.starts_with(b)) {
                continue; // satisfied by the base distro
            }
            if self.from.contains(&p.repoid)
                && let Some(src) = &p.source_name
                && !self.root_sources.contains(src.as_str())
            {
                let (via, required_by) = mine
                    .first()
                    .map(|r| (r.to_string(), pending[*r].clone()))
                    .unwrap_or_default();
                self.collected.entry(src.clone()).or_insert(CollectedDep {
                    source: src.clone(),
                    repoid: p.repoid.clone(),
                    required_by,
                    via,
                });
            }
            next.push((Self::node(p), None));
        }
        self.report.unmatched.extend(
            reqs.iter()
                .filter(|r| !matched.contains(*r))
                .map(|r| r.to_string()),
        );
        Ok(next)
    }

    fn converged(&mut self) -> Result<Vec<engine::Node<PkgInfo>>, String> {
        // Converged — unless the fixpoint finds newly collected
        // packages of our own, whose BuildRequires open another round
        // against the same seen-sets.
        let Some(own) = self.fixpoint_own else {
            return Ok(vec![]);
        };
        let new_roots: Vec<String> = self
            .collected
            .keys()
            .filter(|src| own.contains(*src) && !self.fixpoint_roots.contains(*src))
            .cloned()
            .collect();
        if new_roots.is_empty() {
            return Ok(vec![]);
        }
        self.report.fixpoint_rounds += 1;
        if self.verbose {
            eprintln!(
                "[deps] fixpoint round {}: {} newly owned root(s)",
                self.report.fixpoint_rounds,
                new_roots.len(),
            );
        }
        self.fixpoint_roots.extend(new_roots.iter().cloned());
        self.graph.roots.extend(new_roots.iter().cloned());
        self.src_nodes(&new_roots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canned query: `PkgInfo` can only be built through serde (the
    /// struct is `#[non_exhaustive]`), which doubles as a check that
    /// fixtures stay in fedrq's actual output shape.
    #[derive(Default)]
    struct Canned {
        subpkgs: Vec<PkgInfo>,
        srcs: Vec<PkgInfo>,
        providers: Vec<PkgInfo>,
    }

    fn pkg(v: serde_json::Value) -> PkgInfo {
        serde_json::from_value(v).unwrap()
    }

    impl PkgQuery for Canned {
        fn subpkgs(&self, _srpms: &[String]) -> Result<Vec<PkgInfo>, String> {
            Ok(self.subpkgs.clone())
        }
        fn srcs(&self, _srpms: &[String]) -> Result<Vec<PkgInfo>, String> {
            Ok(self.srcs.clone())
        }
        fn providers(&self, deps: &[String]) -> Result<Vec<PkgInfo>, String> {
            Ok(self
                .providers
                .iter()
                .filter(|p| deps.iter().any(|d| p.satisfies(d)))
                .cloned()
                .collect())
        }
    }

    fn from_epel() -> BTreeSet<String> {
        ["epel".to_string()].into()
    }
    fn base() -> Vec<String> {
        vec!["fedrq-centos-stream-".to_string()]
    }

    #[test]
    fn collects_transitively_with_attribution() {
        // root's binary requires libfoo -> provided by foo-libs
        // (epel, source foo), whose requires pull bar (epel).
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "root-bin", "source_name": "root",
                "repoid": "centos-hyperscale",
                "requires": ["libfoo.so.1()(64bit)", "rpmlib(X)"],
            }))],
            srcs: vec![],
            providers: vec![
                pkg(serde_json::json!({
                    "name": "foo-libs", "source_name": "foo", "repoid": "epel",
                    "provides": ["libfoo.so.1()(64bit)", "foo-libs = 1.0"],
                    "requires": ["bar"],
                })),
                pkg(serde_json::json!({
                    "name": "bar", "source_name": "bar", "repoid": "epel",
                    "provides": ["bar = 2.0"],
                })),
            ],
        };
        let (r, _) = walk(
            &q,
            &["root".to_string()],
            &WalkOpts {
                from: &from_epel(),
                base_prefixes: &base(),
                build: false,
                fixpoint_own: None,
                verbose: false,
            },
        )
        .unwrap();
        assert_eq!(r.waves, 2);
        let names: Vec<&str> = r.collected.iter().map(|c| c.source.as_str()).collect();
        assert_eq!(names, ["bar", "foo"]);
        let foo = &r.collected[1];
        assert_eq!(foo.required_by, "root-bin");
        assert_eq!(foo.via, "libfoo.so.1()(64bit)");
        assert!(r.unmatched.is_empty());
    }

    #[test]
    fn base_providers_terminate_and_are_not_collected() {
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "root-bin", "source_name": "root",
                "repoid": "centos-hyperscale", "requires": ["glibc"],
            }))],
            srcs: vec![],
            providers: vec![pkg(serde_json::json!({
                "name": "glibc", "source_name": "glibc",
                "repoid": "fedrq-centos-stream-baseos",
                "provides": ["glibc = 2.34"],
                // would explode the walk if followed
                "requires": ["libunreachable.so.1()(64bit)"],
            }))],
        };
        let (r, _) = walk(
            &q,
            &["root".to_string()],
            &WalkOpts {
                from: &from_epel(),
                base_prefixes: &base(),
                build: false,
                fixpoint_own: None,
                verbose: false,
            },
        )
        .unwrap();
        assert!(r.collected.is_empty());
        assert_eq!(r.waves, 1);
        assert!(r.unmatched.is_empty());
    }

    #[test]
    fn roots_are_never_collected() {
        // A root's binary requires a sibling root's binary that
        // resolves from epel — still not collected: it's already
        // maintained, that's what being a root means.
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "a-bin", "source_name": "a",
                "repoid": "epel", "requires": ["b-bin"],
            }))],
            srcs: vec![],
            providers: vec![pkg(serde_json::json!({
                "name": "b-bin", "source_name": "b", "repoid": "epel",
                "provides": ["b-bin = 1.0"],
            }))],
        };
        let roots = ["a".to_string(), "b".to_string()];
        let (r, _) = walk(
            &q,
            &roots,
            &WalkOpts {
                from: &from_epel(),
                base_prefixes: &base(),
                build: false,
                fixpoint_own: None,
                verbose: false,
            },
        )
        .unwrap();
        assert!(r.collected.is_empty());
    }

    #[test]
    fn rich_dep_leaves_handles_the_crate_range_shape() {
        assert_eq!(
            rich_dep_leaves(
                "(crate(syn/clone-impls) >= 2.0.52 with crate(syn/clone-impls) < 3.0.0~)"
            ),
            Some(vec![
                "crate(syn/clone-impls) >= 2.0.52".to_string(),
                "crate(syn/clone-impls) < 3.0.0~".to_string(),
            ])
        );
    }

    #[test]
    fn rich_dep_leaves_recurses_into_nested_conjunctions() {
        assert_eq!(
            rich_dep_leaves("((crate(a) >= 1 with crate(a) < 2) and crate(b))"),
            Some(vec![
                "crate(a) >= 1".to_string(),
                "crate(a) < 2".to_string(),
                "crate(b)".to_string(),
            ])
        );
    }

    #[test]
    fn rich_dep_leaves_refuses_conditionals() {
        assert_eq!(rich_dep_leaves("(util-linux-core or util-linux)"), None);
        assert_eq!(
            rich_dep_leaves("(systemd-rpm-macros = 260 if rpm-build)"),
            None
        );
        assert_eq!(rich_dep_leaves("(a and (b or c))"), None);
        assert_eq!(rich_dep_leaves("(a unless b)"), None);
    }

    #[test]
    fn conjunctive_rich_deps_are_walked_not_warned() {
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "rust-app-devel", "source_name": "rust-app",
                "repoid": "centos-hyperscale",
                "requires": ["(crate(syn) >= 2.0 with crate(syn) < 3.0~)"],
            }))],
            srcs: vec![],
            providers: vec![pkg(serde_json::json!({
                "name": "rust-syn-devel", "source_name": "rust-syn",
                "repoid": "epel",
                "provides": ["crate(syn) = 2.0.52", "rust-syn-devel = 2.0.52-1.el10"],
            }))],
        };
        let (r, _) = walk(
            &q,
            &["rust-app".to_string()],
            &WalkOpts {
                from: &from_epel(),
                base_prefixes: &base(),
                build: false,
                fixpoint_own: None,
                verbose: false,
            },
        )
        .unwrap();
        assert!(r.warnings.is_empty());
        assert_eq!(r.collected[0].source, "rust-syn");
        assert_eq!(r.collected[0].via, "crate(syn) < 3.0~");
        assert!(r.unmatched.is_empty());
    }

    #[test]
    fn rich_deps_warn_and_unmatched_reqs_are_reported() {
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "root-bin", "source_name": "root",
                "repoid": "centos-hyperscale",
                "requires": ["(foo if bar)", "/usr/bin/vanisher"],
            }))],
            srcs: vec![],
            providers: vec![],
        };
        let (r, _) = walk(
            &q,
            &["root".to_string()],
            &WalkOpts {
                from: &from_epel(),
                base_prefixes: &base(),
                build: false,
                fixpoint_own: None,
                verbose: false,
            },
        )
        .unwrap();
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("(foo if bar)"));
        assert_eq!(r.unmatched, ["/usr/bin/vanisher"]);
    }

    #[test]
    fn empty_base_prefix_does_not_swallow_the_world() {
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "root-bin", "source_name": "root",
                "repoid": "centos-hyperscale", "requires": ["libfoo.so.1()(64bit)"],
            }))],
            srcs: vec![],
            providers: vec![pkg(serde_json::json!({
                "name": "foo-libs", "source_name": "foo", "repoid": "epel",
                "provides": ["libfoo.so.1()(64bit)"],
            }))],
        };
        // A stray `--base-repo=` must not turn every provider into
        // base: the empty prefix is ignored, foo is still collected.
        let base = vec![String::new(), "fedrq-centos-stream-".to_string()];
        let (r, _) = walk(
            &q,
            &["root".to_string()],
            &WalkOpts {
                from: &from_epel(),
                base_prefixes: &base,
                build: false,
                fixpoint_own: None,
                verbose: false,
            },
        )
        .unwrap();
        assert_eq!(r.collected.len(), 1);
        assert_eq!(r.collected[0].source, "foo");
    }

    #[test]
    fn versioned_requires_meet_versioned_provides() {
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "root-bin", "source_name": "root",
                "repoid": "centos-hyperscale",
                "requires": ["python3-foo >= 1.2"],
            }))],
            srcs: vec![],
            providers: vec![pkg(serde_json::json!({
                "name": "python3-foo", "source_name": "foo", "repoid": "epel",
                "provides": ["python3-foo = 1.5-1.el9"],
            }))],
        };
        let (r, _) = walk(
            &q,
            &["root".to_string()],
            &WalkOpts {
                from: &from_epel(),
                base_prefixes: &base(),
                build: false,
                fixpoint_own: None,
                verbose: false,
            },
        )
        .unwrap();
        assert_eq!(r.collected[0].source, "foo");
        assert_eq!(r.collected[0].via, "python3-foo >= 1.2");
        assert!(r.unmatched.is_empty());
    }

    #[test]
    fn build_seeds_buildrequires_attributed_to_the_source() {
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "ripgrep", "source_name": "ripgrep",
                "repoid": "rawhide", "requires": [],
            }))],
            srcs: vec![pkg(serde_json::json!({
                "name": "ripgrep", "repoid": "rawhide-source",
                "requires": ["(crate(clap) >= 4.0 with crate(clap) < 5.0~)"],
            }))],
            providers: vec![pkg(serde_json::json!({
                "name": "rust-clap-devel", "source_name": "rust-clap",
                "repoid": "rawhide",
                "provides": ["crate(clap) = 4.5.0"],
            }))],
        };
        let from: BTreeSet<String> = ["rawhide".to_string()].into();
        // Without --build the crate edge is invisible…
        let (r, _) = walk(
            &q,
            &["ripgrep".to_string()],
            &WalkOpts {
                from: &from,
                base_prefixes: &base(),
                build: false,
                fixpoint_own: None,
                verbose: false,
            },
        )
        .unwrap();
        assert!(r.collected.is_empty());
        // …with it, the app justifies its crate, labeled as build-time.
        let (r, _) = walk(
            &q,
            &["ripgrep".to_string()],
            &WalkOpts {
                from: &from,
                base_prefixes: &base(),
                build: true,
                fixpoint_own: None,
                verbose: false,
            },
        )
        .unwrap();
        assert_eq!(r.collected[0].source, "rust-clap");
        assert_eq!(r.collected[0].required_by, "src:ripgrep");
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn a_source_does_not_shadow_its_same_named_binary() {
        // Source and main binary share the name; both must be walked
        // (the src: prefix keeps them apart in the seen-set).
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "app", "source_name": "app",
                "repoid": "centos-hyperscale", "requires": ["librun.so.1()(64bit)"],
            }))],
            srcs: vec![pkg(serde_json::json!({
                "name": "app", "repoid": "centos-hyperscale-source",
                "requires": ["buildtool"],
            }))],
            providers: vec![
                pkg(serde_json::json!({
                    "name": "run-libs", "source_name": "run", "repoid": "epel",
                    "provides": ["librun.so.1()(64bit)"],
                })),
                pkg(serde_json::json!({
                    "name": "buildtool", "source_name": "buildtool", "repoid": "epel",
                    "provides": ["buildtool = 1.0"],
                })),
            ],
        };
        let (r, _) = walk(
            &q,
            &["app".to_string()],
            &WalkOpts {
                from: &from_epel(),
                base_prefixes: &base(),
                build: true,
                fixpoint_own: None,
                verbose: false,
            },
        )
        .unwrap();
        let sources: Vec<&str> = r.collected.iter().map(|c| c.source.as_str()).collect();
        assert_eq!(sources, ["buildtool", "run"]);
    }

    #[test]
    fn fixpoint_seeds_newly_owned_packages_and_converges() {
        // app's BuildRequires pull crate(x) → rust-x (ours!). The
        // fixpoint then seeds src:rust-x, whose BuildRequires pull
        // its test-only dep crate(dev) → rust-dev (not ours, no
        // further round). Without the fixpoint, rust-dev is missed:
        // test deps appear in no -devel's runtime Requires.
        let q = Canned {
            subpkgs: vec![pkg(serde_json::json!({
                "name": "app", "source_name": "app",
                "repoid": "rawhide", "requires": [],
            }))],
            srcs: vec![
                pkg(serde_json::json!({
                    "name": "app", "repoid": "rawhide-source",
                    "requires": ["crate(x)"],
                })),
                pkg(serde_json::json!({
                    "name": "rust-x", "repoid": "rawhide-source",
                    "requires": ["crate(dev)"],
                })),
            ],
            providers: vec![
                pkg(serde_json::json!({
                    "name": "rust-x-devel", "source_name": "rust-x",
                    "repoid": "rawhide", "provides": ["crate(x) = 1.0"],
                })),
                pkg(serde_json::json!({
                    "name": "rust-dev-devel", "source_name": "rust-dev",
                    "repoid": "rawhide", "provides": ["crate(dev) = 1.0"],
                })),
            ],
        };
        let from: BTreeSet<String> = ["rawhide".to_string()].into();
        let own: BTreeSet<String> = ["rust-x".to_string()].into();

        // Canned::srcs ignores its argument and returns both sources,
        // so the initial --build wave already carries src:rust-x's
        // BuildRequires — sidestep that by checking the fixpoint
        // bookkeeping AND the collected set.
        let (r, graph) = walk(
            &q,
            &["app".to_string()],
            &WalkOpts {
                from: &from,
                base_prefixes: &base(),
                build: true,
                fixpoint_own: Some(&own),
                verbose: false,
            },
        )
        .unwrap();
        let sources: Vec<&str> = r.collected.iter().map(|c| c.source.as_str()).collect();
        assert_eq!(sources, ["rust-dev", "rust-x"]);
        assert!(graph.roots.contains(&"rust-x".to_string()));

        // Convergence: rust-dev is not ours, so no further round —
        // the walk terminated rather than looping forever.
        assert!(r.fixpoint_rounds >= 1);
    }

    #[test]
    fn the_graph_keeps_every_edge_the_report_drops() {
        // Two binaries require the same capability: the report
        // attributes the collected package to the first requirer
        // only, but removal-reachability needs both edges.
        let q = Canned {
            subpkgs: vec![
                pkg(serde_json::json!({
                    "name": "app-a", "source_name": "app-a",
                    "repoid": "centos-hyperscale",
                    "requires": ["libshared.so.1()(64bit)"],
                })),
                pkg(serde_json::json!({
                    "name": "app-b", "source_name": "app-b",
                    "repoid": "centos-hyperscale",
                    "requires": ["libshared.so.1()(64bit)"],
                })),
            ],
            srcs: vec![pkg(serde_json::json!({
                "name": "app-a", "repoid": "centos-hyperscale-source",
                "requires": ["buildtool"],
            }))],
            providers: vec![
                pkg(serde_json::json!({
                    "name": "shared-libs", "source_name": "shared", "repoid": "epel",
                    "provides": ["libshared.so.1()(64bit)"],
                })),
                pkg(serde_json::json!({
                    "name": "buildtool", "source_name": "buildtool", "repoid": "epel",
                    "provides": ["buildtool = 1.0"],
                })),
            ],
        };
        let roots = ["app-a".to_string(), "app-b".to_string()];
        let (report, graph) = walk(
            &q,
            &roots,
            &WalkOpts {
                from: &from_epel(),
                base_prefixes: &base(),
                build: true,
                fixpoint_own: None,
                verbose: false,
            },
        )
        .unwrap();

        // Report: one attribution.
        let shared = report
            .collected
            .iter()
            .find(|c| c.source == "shared")
            .unwrap();
        assert_eq!(shared.required_by, "app-a");

        // Graph: both requirer edges, the provider, the src mapping.
        let cap = "libshared.so.1()(64bit)";
        let requirers: Vec<&str> = graph.requirers[cap].iter().map(String::as_str).collect();
        assert_eq!(requirers, ["app-a", "app-b"]);
        let provider = graph.providers[cap].first().unwrap();
        assert_eq!(
            (provider.source.as_str(), provider.repoid.as_str()),
            ("shared", "epel")
        );
        assert_eq!(graph.binary_sources["src:app-a"], "app-a");
        assert_eq!(graph.requirers["buildtool"].first().unwrap(), "src:app-a");
        assert_eq!(graph.roots, roots);
    }

    #[test]
    fn merge_unions_roots_attribution_and_edges() {
        let mut a = DepsGraph {
            roots: vec!["app".into()],
            ..Default::default()
        };
        a.binary_sources.insert("app".into(), "app".into());
        a.requirers
            .entry("liba.so".into())
            .or_default()
            .insert("app".into());
        a.providers
            .entry("liba.so".into())
            .or_default()
            .insert(GraphProvider {
                binary: "liba".into(),
                source: "lib-a".into(),
                repoid: "rawhide".into(),
            });
        let mut b = DepsGraph {
            roots: vec!["app".into(), "tool".into()],
            ..Default::default()
        };
        b.binary_sources.insert("tool".into(), "tool".into());
        b.requirers
            .entry("liba.so".into())
            .or_default()
            .insert("tool".into());
        b.providers
            .entry("libb.so".into())
            .or_default()
            .insert(GraphProvider {
                binary: "libb".into(),
                source: "lib-b".into(),
                repoid: "rawhide".into(),
            });
        a.merge(b);
        assert_eq!(a.roots, ["app", "tool"]);
        assert_eq!(a.binary_sources.len(), 2);
        assert_eq!(a.requirers["liba.so"].len(), 2);
        assert!(a.providers.contains_key("libb.so"));
    }

    #[test]
    fn graph_backed_query_answers_known_capabilities_offline() {
        // Graph knows liba.so → liba (source lib-a), and that liba
        // requires libc.so, itself known → glibc.
        let mut g = DepsGraph::default();
        g.binary_sources.insert("liba".into(), "lib-a".into());
        g.requirers
            .entry("libc.so".into())
            .or_default()
            .insert("liba".into());
        g.providers
            .entry("liba.so".into())
            .or_default()
            .insert(GraphProvider {
                binary: "liba".into(),
                source: "lib-a".into(),
                repoid: "rawhide".into(),
            });
        g.providers
            .entry("libc.so".into())
            .or_default()
            .insert(GraphProvider {
                binary: "glibc".into(),
                source: "glibc".into(),
                repoid: "rawhide".into(),
            });
        // The inner query knows only the genuinely new capability.
        let inner = Canned {
            subpkgs: vec![],
            srcs: vec![],
            providers: vec![pkg(serde_json::json!({
                "name": "libnew", "source_name": "lib-new", "repoid": "rawhide",
                "provides": ["libnew.so"], "requires": []
            }))],
        };
        let q = GraphBackedQuery::new(&inner, &g);
        let out = q
            .providers(&["liba.so".to_string(), "libnew.so".to_string()])
            .unwrap();
        let names: Vec<&str> = out.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["liba", "libnew"]);
        // The offline provider carries its graph-recorded Requires, so
        // the walk keeps cascading through the known world.
        assert_eq!(out[0].requires, ["libc.so"]);
        assert!(out[0].provides.iter().any(|c| c == "liba.so"));
        assert_eq!(out[0].source_name.as_deref(), Some("lib-a"));
        assert_eq!(q.offline.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(q.online.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
}
