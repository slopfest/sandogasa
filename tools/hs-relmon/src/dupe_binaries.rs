// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `dupe-binaries` subcommand — find binary RPMs shipped by more
//! than one source package within a single Hyperscale tag.
//!
//! Hyperscale overrides stock CentOS packages and occasionally
//! moves where a given binary RPM is built from — e.g. splitting
//! `ynl` out of `ethtool` into its own source package. Mid-move,
//! two source packages can end up shipping the same binary RPM in
//! the same `-release`/`-testing` tag; whichever the depsolver
//! picks is undefined, and the redundant source should be retired.
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
//! This is detect-and-report only (always read-only, no Koji auth
//! needed); acting on a collision — archiving the redundant
//! project, untagging its build — is left to `prune-archived` and
//! the GitLab tooling.

use serde::Serialize;
use std::collections::BTreeMap;

use crate::cbs::TaggedBinary;
use crate::prune_tags::candidate_tags;

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

/// Collisions found in one tag.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TagCollisions {
    pub tag: String,
    pub collisions: Vec<Collision>,
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

/// Scan every managed candidate tag for collisions. The fetch is
/// injected so tests can supply canned tag contents; in production
/// it is `cbs::Client::list_tagged_binaries`. Tags that error out
/// (don't exist on this hub, transport hiccup) are skipped.
pub fn scan<F>(repositories: &[String], fetch: F, verbose: bool) -> Vec<TagCollisions>
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
            out.push(TagCollisions { tag, collisions });
        }
    }
    out
}

/// Total number of colliding binary RPMs across all tags.
pub fn total_collisions(results: &[TagCollisions]) -> usize {
    results.iter().map(|t| t.collisions.len()).sum()
}

/// Render the scan results for human review.
pub fn render(results: &[TagCollisions]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bin(name: &str, arch: &str, source: &str, nvr: &str) -> TaggedBinary {
        TaggedBinary {
            name: name.to_string(),
            arch: arch.to_string(),
            source: source.to_string(),
            source_nvr: nvr.to_string(),
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
            bin("ynl", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9"),
            bin("ynl", "aarch64", "ethtool", "ethtool-7.0-1.hs.el9"),
            bin("ynl", "x86_64", "ynl", "ynl-1.0-1.hs.el9"),
            bin("ethtool", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9"),
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
            bin("ethtool", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9"),
            bin("ethtool", "aarch64", "ethtool", "ethtool-7.0-1.hs.el9"),
        ];
        assert!(find_collisions(&bins).is_empty());
    }

    #[test]
    fn find_collisions_skips_debug_rpms() {
        // Both sources also ship a -debuginfo of the colliding
        // binary; only the base binary is reported.
        let bins = vec![
            bin("ynl", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9"),
            bin("ynl-debuginfo", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9"),
            bin("ynl", "x86_64", "ynl", "ynl-1.0-1.hs.el9"),
            bin("ynl-debuginfo", "x86_64", "ynl", "ynl-1.0-1.hs.el9"),
        ];
        let collisions = find_collisions(&bins);
        assert_eq!(collisions.len(), 1);
        assert_eq!(collisions[0].binary, "ynl");
    }

    #[test]
    fn find_collisions_empty_when_no_overlap() {
        let bins = vec![
            bin("ethtool", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9"),
            bin("socat", "x86_64", "socat", "socat-1.8-1.hs.el9"),
        ];
        assert!(find_collisions(&bins).is_empty());
    }

    #[test]
    fn scan_groups_collisions_by_tag_and_skips_clean_and_errored() {
        let fetch = |tag: &str| -> Result<Vec<TaggedBinary>, Box<dyn std::error::Error>> {
            match tag {
                "hyperscale9s-packages-main-release" => Ok(vec![
                    bin("ynl", "x86_64", "ethtool", "ethtool-7.0-1.hs.el9"),
                    bin("ynl", "x86_64", "ynl", "ynl-1.0-1.hs.el9"),
                ]),
                // A clean tag: no collisions.
                "hyperscale10s-packages-main-release" => {
                    Ok(vec![bin("foo", "x86_64", "foo", "foo-1-1.hs.el10")])
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
    }

    #[test]
    fn render_empty_is_clear() {
        assert_eq!(render(&[]), "No duplicate binary RPMs found.\n");
    }

    #[test]
    fn render_lists_tag_binary_and_sources() {
        let results = vec![TagCollisions {
            tag: "hyperscale9s-packages-main-release".into(),
            collisions: vec![Collision {
                binary: "ynl".into(),
                sources: vec![
                    SourceBuild {
                        source: "ethtool".into(),
                        nvr: "ethtool-7.0-1.hs.el9".into(),
                    },
                    SourceBuild {
                        source: "ynl".into(),
                        nvr: "ynl-1.0-1.hs.el9".into(),
                    },
                ],
            }],
        }];
        let rendered = render(&results);
        assert!(rendered.contains("Found 1 duplicate binary RPM(s) across 1 tag(s)"));
        assert!(rendered.contains("hyperscale9s-packages-main-release"));
        assert!(rendered.contains("ynl shipped by 2 sources"));
        assert!(rendered.contains("ethtool (ethtool-7.0-1.hs.el9)"));
        assert!(rendered.contains("ynl (ynl-1.0-1.hs.el9)"));
    }
}
