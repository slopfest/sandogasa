// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `dupe-binaries` subcommand — find binary RPMs shipped by more
//! than one source package within a single Hyperscale tag, and
//! (with `--fix`) interactively untag the redundant build.
//!
//! Hyperscale overrides stock CentOS packages and occasionally
//! moves where a given binary RPM is built from — e.g. splitting
//! `perf` out of `kernel-tools` into its own source package.
//! Mid-move, two source packages can end up shipping the same
//! binary RPM in the same `-release`/`-testing` tag; whichever the
//! depsolver picks is undefined. The redundant source should be
//! retired.
//!
//! Detection is per-tag because a collision only matters when both
//! providers land in the *same* enabled repository: a binary in
//! `-release` from one source and in `-testing` from another are
//! never installed together. For each managed candidate tag we ask
//! Koji for the binary RPMs (latest build per source), group by
//! binary name, and flag any name produced by two or more distinct
//! sources. `-debuginfo`/`-debugsource` RPMs are excluded — a
//! collision there only mirrors the base binary's.
//!
//! `--fix` builds a per-tag plan: for each cluster of sources that
//! share a binary it recommends untagging the oldest build (the
//! likely stale leftover) but, crucially, lists the binaries that
//! would *disappear* from the tag if a given build is untagged —
//! those only that source provides. Untagging `kernel-tools` to
//! resolve a `perf` collision would also drop `cpupower`,
//! `turbostat`, … so the choice needs a human. The fix is
//! interactive (one prompt per cluster, default skip); in `--json`
//! mode or without a terminal it prints the plan and acts on
//! nothing.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};

use sandogasa_koji::untag_build;

use crate::cbs::TaggedBinary;
use crate::prune_tags::{KOJI_PROFILE, candidate_tags};

/// A source build shipping a colliding binary RPM.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceBuild {
    /// Source package name.
    pub source: String,
    /// NVR of the source build providing the binary.
    pub nvr: String,
}

/// A binary RPM produced by more than one source package in a tag.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Collision {
    /// The binary RPM name shared by multiple sources.
    pub binary: String,
    /// Distinct source builds shipping it, sorted by source name.
    pub sources: Vec<SourceBuild>,
}

/// Collisions found in one tag — the slim view used for `--json`
/// detect output.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TagCollisions {
    pub tag: String,
    pub collisions: Vec<Collision>,
}

/// One tag's full binary listing plus the collisions found in it.
/// The binaries are retained so `--fix` can compute, for each
/// candidate untag, which binaries only that source provides.
#[derive(Debug, Clone)]
pub struct TagScan {
    pub tag: String,
    pub binaries: Vec<TaggedBinary>,
    pub collisions: Vec<Collision>,
}

/// A build that could be untagged to resolve a collision, with the
/// collateral that choice carries.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UntagCandidate {
    /// Source package name.
    pub source: String,
    /// NVR to untag.
    pub nvr: String,
    /// Koji build ID (lower = older).
    pub build_id: i64,
    /// All non-debug binary RPMs this source ships in the tag.
    pub binaries: Vec<String>,
    /// Binaries only this source provides in the tag — they would
    /// disappear from the tag if it were untagged.
    pub unique: Vec<String>,
    /// Whether this is the recommended build to untag (the oldest
    /// in the cluster).
    pub recommended: bool,
}

/// A set of sources connected by shared binaries within one tag.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FixCluster {
    /// Binaries shipped by two or more sources in this cluster.
    pub shared: Vec<String>,
    /// Candidate builds, oldest first; the first is recommended.
    pub candidates: Vec<UntagCandidate>,
}

/// The fix plan for one tag.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TagFixPlan {
    pub tag: String,
    pub clusters: Vec<FixCluster>,
}

/// Outcome of applying fix plans.
#[derive(Debug, Default, Clone, Copy)]
pub struct ApplyOutcome {
    pub untagged: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// Whether a binary RPM name is a debug artifact, whose collisions
/// only mirror the base binary's and so are filtered out.
pub fn is_debug_rpm(name: &str) -> bool {
    name.ends_with("-debuginfo") || name.ends_with("-debugsource")
}

/// Find binary RPMs shipped by two or more distinct source
/// packages. Pure: arches are deduped (the same binary built for
/// several arches by one source is not a collision) by collecting
/// distinct source names per binary name; `-debuginfo` /
/// `-debugsource` RPMs are skipped. Results are sorted by binary
/// name, and each collision's sources are sorted by source name.
pub fn find_collisions(binaries: &[TaggedBinary]) -> Vec<Collision> {
    // binary name -> (source name -> nvr)
    let mut by_name: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
    for b in binaries {
        if is_debug_rpm(&b.name) {
            continue;
        }
        by_name
            .entry(&b.name)
            .or_default()
            .insert(&b.source, &b.source_nvr);
    }
    by_name
        .into_iter()
        .filter(|(_, sources)| sources.len() >= 2)
        .map(|(binary, sources)| Collision {
            binary: binary.to_string(),
            sources: sources
                .into_iter()
                .map(|(source, nvr)| SourceBuild {
                    source: source.to_string(),
                    nvr: nvr.to_string(),
                })
                .collect(),
        })
        .collect()
}

/// Scan every managed candidate tag for collisions, retaining each
/// colliding tag's full binary listing. The fetch is injected so
/// tests can supply canned tag contents; in production it is
/// `cbs::Client::list_tagged_binaries`. Tags that error out (don't
/// exist on this hub, transport hiccup) are skipped.
pub fn scan<F>(repositories: &[String], fetch: F, verbose: bool) -> Vec<TagScan>
where
    F: Fn(&str) -> Result<Vec<TaggedBinary>, Box<dyn std::error::Error>>,
{
    let mut out = Vec::new();
    for tag in candidate_tags(repositories) {
        if verbose {
            eprintln!("[hs-relmon] scanning {tag}");
        }
        let binaries = match fetch(&tag) {
            Ok(b) if !b.is_empty() => b,
            Ok(_) => continue,
            Err(e) => {
                if verbose {
                    eprintln!("[hs-relmon] {tag}: skipped ({e})");
                }
                continue;
            }
        };
        let collisions = find_collisions(&binaries);
        if verbose {
            eprintln!(
                "[hs-relmon] {tag}: {} binary RPM(s), {} collision(s)",
                binaries.len(),
                collisions.len()
            );
        }
        if !collisions.is_empty() {
            out.push(TagScan {
                tag,
                binaries,
                collisions,
            });
        }
    }
    out
}

/// Total number of colliding binary RPMs across all tags.
pub fn total_collisions(results: &[TagScan]) -> usize {
    results.iter().map(|t| t.collisions.len()).sum()
}

/// Project scan results to the slim `TagCollisions` view for
/// `--json` detect output (drops the full binary listing).
pub fn collision_report(results: &[TagScan]) -> Vec<TagCollisions> {
    results
        .iter()
        .map(|s| TagCollisions {
            tag: s.tag.clone(),
            collisions: s.collisions.clone(),
        })
        .collect()
}

/// Render the detect results for human review.
pub fn render(results: &[TagScan]) -> String {
    if results.is_empty() {
        return "No duplicate binary RPMs found.\n".to_string();
    }
    let total = total_collisions(results);
    let mut out = format!(
        "Found {total} duplicate binary RPM(s) across {} tag(s):\n",
        results.len()
    );
    for tc in results {
        out.push_str(&format!("\n{}:\n", tc.tag));
        for c in &tc.collisions {
            out.push_str(&format!(
                "  {} shipped by {} sources:\n",
                c.binary,
                c.sources.len()
            ));
            for s in &c.sources {
                out.push_str(&format!("    {} ({})\n", s.source, s.nvr));
            }
        }
    }
    out
}

/// Build the per-tag fix plan from a scan result. Pure.
///
/// Sources are clustered by shared binaries (connected components,
/// so a three-way `A↔B↔C` overlap is one cluster). Within a
/// cluster, candidates are sorted oldest build first and the oldest
/// is marked recommended; each candidate's `unique` set is the
/// binaries only it provides *in the whole tag* — exactly what
/// vanishes if it is untagged.
pub fn build_fix_plan(scan: &TagScan) -> TagFixPlan {
    // Full per-tag maps over non-debug binaries.
    let mut provided_by: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut src_binaries: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut src_meta: BTreeMap<&str, (&str, i64)> = BTreeMap::new();
    for b in &scan.binaries {
        if is_debug_rpm(&b.name) {
            continue;
        }
        provided_by.entry(&b.name).or_default().insert(&b.source);
        src_binaries.entry(&b.source).or_default().insert(&b.name);
        src_meta
            .entry(&b.source)
            .or_insert((&b.source_nvr, b.build_id));
    }

    // Colliding sources, indexed for union-find.
    let mut colliding_sources: BTreeSet<&str> = BTreeSet::new();
    for sources in provided_by.values().filter(|s| s.len() >= 2) {
        colliding_sources.extend(sources.iter().copied());
    }
    let sources: Vec<&str> = colliding_sources.into_iter().collect();
    let index: BTreeMap<&str, usize> = sources.iter().enumerate().map(|(i, s)| (*s, i)).collect();
    let mut parent: Vec<usize> = (0..sources.len()).collect();

    // Union all sources that co-provide a colliding binary, and
    // tag each colliding binary with one of its sources for later
    // component assignment.
    for set in provided_by.values().filter(|s| s.len() >= 2) {
        let members: Vec<usize> = set.iter().map(|s| index[s]).collect();
        for pair in members.windows(2) {
            uf_union(&mut parent, pair[0], pair[1]);
        }
    }

    // Accumulate shared binaries and member sources per component.
    let mut comp_shared: BTreeMap<usize, BTreeSet<&str>> = BTreeMap::new();
    let mut comp_sources: BTreeMap<usize, BTreeSet<&str>> = BTreeMap::new();
    for (binary, set) in provided_by.iter().filter(|(_, s)| s.len() >= 2) {
        let any = *set.iter().next().unwrap();
        let root = uf_find(&mut parent, index[any]);
        comp_shared.entry(root).or_default().insert(binary);
        comp_sources
            .entry(root)
            .or_default()
            .extend(set.iter().copied());
    }

    let mut clusters: Vec<FixCluster> = comp_sources
        .into_iter()
        .map(|(root, srcs)| {
            let shared: Vec<String> = comp_shared
                .get(&root)
                .map(|s| s.iter().map(|b| b.to_string()).collect())
                .unwrap_or_default();
            let mut candidates: Vec<UntagCandidate> = srcs
                .iter()
                .map(|src| {
                    let (nvr, build_id) = src_meta[src];
                    let binaries: Vec<String> =
                        src_binaries[src].iter().map(|b| b.to_string()).collect();
                    let unique: Vec<String> = src_binaries[src]
                        .iter()
                        .filter(|b| provided_by[**b].len() == 1)
                        .map(|b| b.to_string())
                        .collect();
                    UntagCandidate {
                        source: src.to_string(),
                        nvr: nvr.to_string(),
                        build_id,
                        binaries,
                        unique,
                        recommended: false,
                    }
                })
                .collect();
            // Oldest build first; the oldest is the recommendation.
            candidates.sort_by(|a, b| {
                a.build_id
                    .cmp(&b.build_id)
                    .then_with(|| a.source.cmp(&b.source))
            });
            if let Some(first) = candidates.first_mut() {
                first.recommended = true;
            }
            FixCluster { shared, candidates }
        })
        .collect();
    // Stable cluster order by first shared binary.
    clusters.sort_by(|a, b| a.shared.first().cmp(&b.shared.first()));

    TagFixPlan {
        tag: scan.tag.clone(),
        clusters,
    }
}

/// Render an entire set of fix plans for review (no prompting).
pub fn render_fix_plans(plans: &[TagFixPlan]) -> String {
    if plans.is_empty() {
        return "No duplicate binary RPMs found.\n".to_string();
    }
    let mut out = String::new();
    for plan in plans {
        out.push_str(&format!("{}:\n", plan.tag));
        for cluster in &plan.clusters {
            out.push_str(&render_cluster(cluster));
        }
        out.push('\n');
    }
    out
}

/// Apply fix plans interactively: one prompt per cluster, choosing
/// which build (if any) to untag. Default is to skip. Returns the
/// tally of untagged / skipped / errored clusters.
pub fn apply_fix(plans: &[TagFixPlan], verbose: bool) -> ApplyOutcome {
    let mut outcome = ApplyOutcome::default();
    for plan in plans {
        for cluster in &plan.clusters {
            eprint!("\n{}:\n{}", plan.tag, render_cluster(cluster));
            match prompt_choice(cluster.candidates.len()) {
                Some(i) => {
                    let c = &cluster.candidates[i];
                    match do_untag(&plan.tag, &c.nvr, verbose) {
                        Ok(()) => outcome.untagged += 1,
                        Err(()) => outcome.errors += 1,
                    }
                }
                None => outcome.skipped += 1,
            }
        }
    }
    outcome
}

/// Render one cluster: the shared binaries, then each candidate
/// build numbered for selection, with the collateral (binaries only
/// that source provides) it would remove from the tag.
fn render_cluster(cluster: &FixCluster) -> String {
    let mut out = format!("  duplicate binaries: {}\n", join_or_none(&cluster.shared));
    for (i, c) in cluster.candidates.iter().enumerate() {
        let rec = if c.recommended {
            " [recommended, oldest]"
        } else {
            ""
        };
        out.push_str(&format!(
            "    [{}] untag {} (build {}){rec}\n",
            i + 1,
            c.nvr,
            c.build_id
        ));
        if c.unique.is_empty() {
            out.push_str("        removes nothing else — ships only duplicated binaries\n");
        } else {
            out.push_str(&format!(
                "        also removes from the tag (only provided here): {}\n",
                c.unique.join(", ")
            ));
        }
    }
    out
}

/// Comma-join a list, or `(none)` when empty.
fn join_or_none(items: &[String]) -> String {
    if items.is_empty() {
        "(none)".to_string()
    } else {
        items.join(", ")
    }
}

/// Prompt for which candidate (1-based) to untag. Blank, EOF, or an
/// out-of-range / unparseable entry means skip (`None`).
fn prompt_choice(n: usize) -> Option<usize> {
    eprint!("Untag which build? [1-{n}, Enter to skip]: ");
    std::io::stderr().flush().ok()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<usize>() {
        Ok(i) if (1..=n).contains(&i) => Some(i - 1),
        _ => None,
    }
}

/// Untag one build, logging the result.
fn do_untag(tag: &str, nvr: &str, verbose: bool) -> Result<(), ()> {
    if verbose {
        eprintln!("[hs-relmon] koji untag-build {tag} {nvr}");
    }
    match untag_build(tag, nvr, Some(KOJI_PROFILE)) {
        Ok(()) => {
            eprintln!("untagged {nvr} from {tag}");
            Ok(())
        }
        Err(e) => {
            eprintln!("error: untag-build {tag} {nvr}: {e}");
            Err(())
        }
    }
}

/// Union-find: find the representative of `x` with path halving.
fn uf_find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

/// Union-find: merge the sets containing `a` and `b`.
fn uf_union(parent: &mut [usize], a: usize, b: usize) {
    let ra = uf_find(parent, a);
    let rb = uf_find(parent, b);
    if ra != rb {
        parent[ra] = rb;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bin(name: &str, arch: &str, source: &str, nvr: &str, build_id: i64) -> TaggedBinary {
        TaggedBinary {
            rpm_id: 0, // unused by collision detection
            name: name.to_string(),
            arch: arch.to_string(),
            source: source.to_string(),
            source_nvr: nvr.to_string(),
            build_id,
        }
    }

    /// A scan result over a set of binaries, with collisions filled
    /// in — mirrors what `scan` produces for one tag.
    fn scan_of(tag: &str, binaries: Vec<TaggedBinary>) -> TagScan {
        let collisions = find_collisions(&binaries);
        TagScan {
            tag: tag.to_string(),
            binaries,
            collisions,
        }
    }

    #[test]
    fn is_debug_rpm_matches_debug_artifacts() {
        assert!(is_debug_rpm("ethtool-debuginfo"));
        assert!(is_debug_rpm("ethtool-debugsource"));
        assert!(!is_debug_rpm("ethtool"));
        assert!(!is_debug_rpm("ynl"));
    }

    #[test]
    fn find_collisions_flags_binary_from_two_sources() {
        let bins = vec![
            bin("ynl", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9", 100),
            bin("ynl", "aarch64", "ethtool", "ethtool-7.0-1.hs.el9", 100),
            bin("ynl", "x86_64", "ynl", "ynl-1.0-1.hs.el9", 200),
            bin("ethtool", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9", 100),
        ];
        let collisions = find_collisions(&bins);
        assert_eq!(collisions.len(), 1);
        let c = &collisions[0];
        assert_eq!(c.binary, "ynl");
        assert_eq!(
            c.sources,
            vec![
                SourceBuild {
                    source: "ethtool".into(),
                    nvr: "ethtool-7.0-1.hs.el9".into(),
                },
                SourceBuild {
                    source: "ynl".into(),
                    nvr: "ynl-1.0-1.hs.el9".into(),
                },
            ]
        );
    }

    #[test]
    fn find_collisions_ignores_same_source_multiple_arches() {
        let bins = vec![
            bin("ethtool", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9", 100),
            bin("ethtool", "aarch64", "ethtool", "ethtool-7.0-1.hs.el9", 100),
        ];
        assert!(find_collisions(&bins).is_empty());
    }

    #[test]
    fn find_collisions_skips_debug_rpms() {
        // Both sources also ship a -debuginfo of the colliding
        // binary; only the base binary is reported.
        let bins = vec![
            bin("ynl", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9", 100),
            bin(
                "ynl-debuginfo",
                "x86_64",
                "ethtool",
                "ethtool-7.0-1.hs.el9",
                100,
            ),
            bin("ynl", "x86_64", "ynl", "ynl-1.0-1.hs.el9", 200),
            bin("ynl-debuginfo", "x86_64", "ynl", "ynl-1.0-1.hs.el9", 200),
        ];
        let collisions = find_collisions(&bins);
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].binary, "ynl");
    }

    #[test]
    fn find_collisions_empty_when_no_overlap() {
        let bins = vec![
            bin("ethtool", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9", 100),
            bin("socat", "x86_64", "socat", "socat-1.8-1.hs.el9", 200),
        ];
        assert!(find_collisions(&bins).is_empty());
    }

    #[test]
    fn scan_groups_collisions_by_tag_and_skips_clean_and_errored() {
        let fetch = |tag: &str| -> Result<Vec<TaggedBinary>, Box<dyn std::error::Error>> {
            match tag {
                "hyperscale9s-packages-main-release" => Ok(vec![
                    bin("ynl", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9", 100),
                    bin("ynl", "x86_64", "ynl", "ynl-1.0-1.hs.el9", 200),
                ]),
                // A clean tag: no collisions.
                "hyperscale10s-packages-main-release" => {
                    Ok(vec![bin("foo", "x86_64", "foo", "foo-1-1.hs.el10", 300)])
                }
                // Everything else: empty or error (e.g. tag absent).
                t if t.contains("testing") => Err("no such tag".into()),
                _ => Ok(vec![]),
            }
        };
        let results = scan(&["main".to_string()], fetch, false);
        // Only the one tag with a real collision is reported.
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].tag, "hyperscale9s-packages-main-release");
        assert_eq!(total_collisions(&results), 1);
        assert_eq!(results[0].collisions[0].binary, "ynl");
        // Full binaries are retained for the fix path.
        assert_eq!(results[0].binaries.len(), 2);
    }

    #[test]
    fn render_empty_is_clear() {
        assert_eq!(render(&[]), "No duplicate binary RPMs found.\n");
    }

    #[test]
    fn render_lists_tag_binary_and_sources() {
        let results = vec![scan_of(
            "hyperscale9s-packages-main-release",
            vec![
                bin("ynl", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9", 100),
                bin("ynl", "x86_64", "ynl", "ynl-1.0-1.hs.el9", 200),
            ],
        )];
        let rendered = render(&results);
        assert!(rendered.contains("Found 1 duplicate binary RPM(s) across 1 tag(s)"));
        assert!(rendered.contains("hyperscale9s-packages-main-release"));
        assert!(rendered.contains("ynl shipped by 2 sources"));
        assert!(rendered.contains("ethtool (ethtool-7.0-1.hs.el9)"));
        assert!(rendered.contains("ynl (ynl-1.0-1.hs.el9)"));
    }

    #[test]
    fn collision_report_drops_binaries() {
        let results = vec![scan_of(
            "hyperscale9s-packages-main-release",
            vec![
                bin("ynl", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9", 100),
                bin("ynl", "x86_64", "ynl", "ynl-1.0-1.hs.el9", 200),
            ],
        )];
        let report = collision_report(&results);
        assert_eq!(report.len(), 1);
        assert_eq!(report[0].tag, "hyperscale9s-packages-main-release");
        assert_eq!(report[0].collisions.len(), 1);
    }

    #[test]
    fn build_fix_plan_recommends_oldest_and_lists_collateral() {
        // The perf/kernel-tools shape: old kernel-tools (build 100)
        // ships perf + libperf + cpupower; new perf (build 200)
        // ships only perf + libperf.
        let scan = scan_of(
            "hyperscale9s-packages-main-release",
            vec![
                bin(
                    "perf",
                    "x86_64",
                    "kernel-tools",
                    "kernel-tools-6.4-1.hs.el9",
                    100,
                ),
                bin(
                    "libperf",
                    "x86_64",
                    "kernel-tools",
                    "kernel-tools-6.4-1.hs.el9",
                    100,
                ),
                bin(
                    "cpupower",
                    "x86_64",
                    "kernel-tools",
                    "kernel-tools-6.4-1.hs.el9",
                    100,
                ),
                bin("perf", "x86_64", "perf", "perf-6.19-1.hs.el9", 200),
                bin("libperf", "x86_64", "perf", "perf-6.19-1.hs.el9", 200),
            ],
        );
        let plan = build_fix_plan(&scan);
        assert_eq!(plan.clusters.len(), 1);
        let cluster = &plan.clusters[0];
        assert_eq!(cluster.shared, vec!["libperf", "perf"]);
        assert_eq!(cluster.candidates.len(), 2);

        // Oldest build (kernel-tools) is first and recommended, and
        // its collateral is the binary only it provides.
        let rec = &cluster.candidates[0];
        assert_eq!(rec.source, "kernel-tools");
        assert!(rec.recommended);
        assert_eq!(rec.unique, vec!["cpupower"]);

        // The newer perf build ships only duplicated binaries.
        let other = &cluster.candidates[1];
        assert_eq!(other.source, "perf");
        assert!(!other.recommended);
        assert!(other.unique.is_empty());
    }

    #[test]
    fn build_fix_plan_three_way_overlap_is_one_cluster() {
        // a↔b share x, b↔c share y → all three in one cluster.
        let scan = scan_of(
            "hyperscale9s-packages-main-release",
            vec![
                bin("x", "x86_64", "a", "a-1-1.hs.el9", 10),
                bin("x", "x86_64", "b", "b-1-1.hs.el9", 20),
                bin("y", "x86_64", "b", "b-1-1.hs.el9", 20),
                bin("y", "x86_64", "c", "c-1-1.hs.el9", 30),
            ],
        );
        let plan = build_fix_plan(&scan);
        assert_eq!(plan.clusters.len(), 1);
        let sources: Vec<&str> = plan.clusters[0]
            .candidates
            .iter()
            .map(|c| c.source.as_str())
            .collect();
        assert_eq!(sources, vec!["a", "b", "c"]);
        assert_eq!(plan.clusters[0].shared, vec!["x", "y"]);
    }

    #[test]
    fn render_cluster_shows_collateral_and_recommendation() {
        let scan = scan_of(
            "t",
            vec![
                bin(
                    "perf",
                    "x86_64",
                    "kernel-tools",
                    "kernel-tools-6.4-1.hs.el9",
                    100,
                ),
                bin(
                    "cpupower",
                    "x86_64",
                    "kernel-tools",
                    "kernel-tools-6.4-1.hs.el9",
                    100,
                ),
                bin("perf", "x86_64", "perf", "perf-6.19-1.hs.el9", 200),
            ],
        );
        let plan = build_fix_plan(&scan);
        let rendered = render_cluster(&plan.clusters[0]);
        assert!(rendered.contains("duplicate binaries: perf"));
        assert!(rendered.contains("[1] untag kernel-tools-6.4-1.hs.el9"));
        assert!(rendered.contains("[recommended, oldest]"));
        assert!(rendered.contains("only provided here): cpupower"));
        assert!(rendered.contains("ships only duplicated binaries"));
    }
}
