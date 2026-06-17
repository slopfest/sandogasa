// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `file-conflicts` subcommand — find files shipped by more than
//! one source package across the repositories enabled together on
//! a Hyperscale host.
//!
//! [`crate::dupe_binaries`] catches two sources shipping the same
//! binary RPM *name* within one tag. But the sharper, real-world
//! breakage is a **file** conflict between differently-named RPMs
//! in *different* repos: the `kernel` source ships `/usr/bin/ynl`
//! and the `pyynl` tree inside its `python3-kernel-tools`
//! subpackage (kernel repo), while a standalone `python3-ynl`
//! (main repo) ships the same paths. Both repos are enabled on the
//! same host, so dnf hits a file conflict — yet the RPM names
//! differ and they live in separate tags, so name-and-tag matching
//! misses it entirely.
//!
//! Detection works per EL version over the set of repos enabled
//! together (default `main` + `kernel` on EL10/10s; `main` only on
//! EL9/9s, which has no kernel repo — override with
//! `--repositories`). For each repo's `-release` tag we pull the
//! binary RPMs' file lists from Koji (batched via `multicall`, so a
//! whole tag costs a handful of requests), map each real file path
//! to the set of source packages that ship it, and flag any path
//! owned by two or more distinct sources.
//!
//! Directories (shared ownership is normal), `%ghost` entries (not
//! on disk), and debug payloads under `/usr/lib/debug` /
//! `/usr/src/debug` (a separate, noisy class) are excluded. The
//! scan is read-only.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

use crate::cbs::{RpmFile, TaggedBinary};

/// EL version tokens scanned by default, matching the
/// `hyperscale<EL>-packages-…` tag prefix.
pub const EL_TOKENS: &[&str] = &["9", "9s", "10", "10s"];

/// Files shared by a set of source packages — a file conflict.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceConflict {
    /// The distinct source packages that ship the shared files,
    /// sorted.
    pub sources: Vec<String>,
    /// The shared file paths, sorted.
    pub files: Vec<String>,
}

/// File conflicts found within one EL version's enabled repo set.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ElConflicts {
    /// EL token (`9`, `9s`, `10`, `10s`).
    pub el: String,
    /// Repositories that were combined for this EL.
    pub repos: Vec<String>,
    /// One entry per distinct conflicting source set.
    pub conflicts: Vec<SourceConflict>,
}

/// The repositories enabled together for an EL by default. EL10 and
/// EL10s ship the kernel from a separate `kernel` repo alongside
/// `main`; EL9/9s have no kernel repo.
pub fn default_repos(el: &str) -> Vec<String> {
    let repos: &[&str] = match el {
        "10" | "10s" => &["main", "kernel"],
        _ => &["main"],
    };
    repos.iter().map(|s| s.to_string()).collect()
}

/// Whether a file entry can actually conflict on disk: a regular
/// file or symlink (not a directory), not a `%ghost`, and not part
/// of a debug payload (those produce build-id path noise and are a
/// separate concern).
pub fn is_real_file(f: &RpmFile) -> bool {
    if f.is_dir() || f.is_ghost() {
        return false;
    }
    !(f.path.starts_with("/usr/lib/debug/") || f.path.starts_with("/usr/src/debug/"))
}

/// Find file paths shipped by two or more distinct source packages.
/// Pure. `per_rpm` is `(source, files)` for every RPM in the
/// combined repo set — several RPMs may share a source (dedup is by
/// source name, so multiple arches / subpackages of one source
/// never self-conflict). Results are grouped by the conflicting
/// source set and sorted.
pub fn find_file_conflicts(per_rpm: &[(String, Vec<RpmFile>)]) -> Vec<SourceConflict> {
    // path -> set of distinct sources shipping it
    let mut path_sources: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (source, files) in per_rpm {
        for f in files {
            if !is_real_file(f) {
                continue;
            }
            path_sources.entry(&f.path).or_default().insert(source);
        }
    }
    // Group conflicting paths by their (sorted) source set.
    let mut by_set: BTreeMap<Vec<&str>, Vec<&str>> = BTreeMap::new();
    for (path, sources) in &path_sources {
        if sources.len() < 2 {
            continue;
        }
        let key: Vec<&str> = sources.iter().copied().collect();
        by_set.entry(key).or_default().push(path);
    }
    by_set
        .into_iter()
        .map(|(sources, mut files)| {
            files.sort_unstable();
            SourceConflict {
                sources: sources.iter().map(|s| s.to_string()).collect(),
                files: files.iter().map(|f| f.to_string()).collect(),
            }
        })
        .collect()
}

/// Scan each EL version's enabled repo set for cross-source file
/// conflicts. The fetchers are injected so tests can supply canned
/// data; in production they are `cbs::Client::list_tagged_binaries`
/// and `cbs::Client::list_rpm_files_multi`.
///
/// `repo_override`, when set, replaces the per-EL defaults for
/// every EL. Tags that error out (absent on this hub, e.g. a
/// `kernel` repo on EL9) are skipped.
pub fn scan<R, F>(
    repo_override: Option<&[String]>,
    els: &[&str],
    fetch_rpms: R,
    fetch_files: F,
    verbose: bool,
) -> Vec<ElConflicts>
where
    R: Fn(&str) -> Result<Vec<TaggedBinary>, Box<dyn std::error::Error>>,
    F: Fn(&[i64]) -> Result<Vec<(i64, Vec<RpmFile>)>, Box<dyn std::error::Error>>,
{
    let mut out = Vec::new();
    for el in els {
        let repos: Vec<String> = match repo_override {
            Some(o) => o.to_vec(),
            None => default_repos(el),
        };
        // rpm_id -> source across every enabled repo's release tag.
        let mut rid_source: BTreeMap<i64, String> = BTreeMap::new();
        for repo in &repos {
            let tag = format!("hyperscale{el}-packages-{repo}-release");
            if verbose {
                eprintln!("[hs-relmon] scanning {tag}");
            }
            match fetch_rpms(&tag) {
                Ok(bins) => {
                    for b in bins {
                        rid_source.insert(b.rpm_id, b.source);
                    }
                }
                Err(e) => {
                    if verbose {
                        eprintln!("[hs-relmon] {tag}: skipped ({e})");
                    }
                }
            }
        }
        if rid_source.is_empty() {
            continue;
        }
        let ids: Vec<i64> = rid_source.keys().copied().collect();
        if verbose {
            eprintln!(
                "[hs-relmon] EL{el}: fetching file lists for {} RPM(s)",
                ids.len()
            );
        }
        let files = match fetch_files(&ids) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("EL{el}: file list fetch failed: {e}");
                continue;
            }
        };
        let per_rpm: Vec<(String, Vec<RpmFile>)> = files
            .into_iter()
            .filter_map(|(rid, fs)| rid_source.get(&rid).map(|s| (s.clone(), fs)))
            .collect();
        let conflicts = find_file_conflicts(&per_rpm);
        if verbose {
            eprintln!(
                "[hs-relmon] EL{el}: {} conflicting source set(s)",
                conflicts.len()
            );
        }
        if !conflicts.is_empty() {
            out.push(ElConflicts {
                el: el.to_string(),
                repos,
                conflicts,
            });
        }
    }
    out
}

/// Total number of conflicting file paths across all EL versions.
pub fn total_conflicting_files(results: &[ElConflicts]) -> usize {
    results
        .iter()
        .flat_map(|e| &e.conflicts)
        .map(|c| c.files.len())
        .sum()
}

/// Render the scan results for human review. Each source set's file
/// list is capped at `FILE_CAP` lines with a "… and N more" tail so
/// a big overlap doesn't flood the terminal; `--json` carries the
/// full list.
pub fn render(results: &[ElConflicts]) -> String {
    const FILE_CAP: usize = 25;
    if results.is_empty() {
        return "No file conflicts found.\n".to_string();
    }
    let total = total_conflicting_files(results);
    let pairs: usize = results.iter().map(|e| e.conflicts.len()).sum();
    let mut out = format!("Found {total} conflicting file(s) across {pairs} source set(s):\n");
    for el in results {
        out.push_str(&format!(
            "\nhyperscale{} (repos: {}):\n",
            el.el,
            el.repos.join(", ")
        ));
        for c in &el.conflicts {
            out.push_str(&format!(
                "  {} — {} file(s):\n",
                c.sources.join(" + "),
                c.files.len()
            ));
            for path in c.files.iter().take(FILE_CAP) {
                out.push_str(&format!("    {path}\n"));
            }
            if c.files.len() > FILE_CAP {
                out.push_str(&format!("    … and {} more\n", c.files.len() - FILE_CAP));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tb(rpm_id: i64, name: &str, source: &str) -> TaggedBinary {
        TaggedBinary {
            rpm_id,
            name: name.to_string(),
            arch: "x86_64".to_string(),
            source: source.to_string(),
            source_nvr: format!("{source}-1-1.hs.el10"),
            build_id: rpm_id,
        }
    }

    fn file(path: &str) -> RpmFile {
        RpmFile {
            path: path.to_string(),
            mode: 0o100755,
            flags: 0,
        }
    }

    fn dir(path: &str) -> RpmFile {
        RpmFile {
            path: path.to_string(),
            mode: 0o040755,
            flags: 0,
        }
    }

    fn ghost(path: &str) -> RpmFile {
        RpmFile {
            path: path.to_string(),
            mode: 0o100644,
            flags: 64,
        }
    }

    #[test]
    fn default_repos_adds_kernel_only_for_el10() {
        assert_eq!(default_repos("10s"), vec!["main", "kernel"]);
        assert_eq!(default_repos("10"), vec!["main", "kernel"]);
        assert_eq!(default_repos("9s"), vec!["main"]);
        assert_eq!(default_repos("9"), vec!["main"]);
    }

    #[test]
    fn is_real_file_filters_dirs_ghosts_and_debug() {
        assert!(is_real_file(&file("/usr/bin/ynl")));
        assert!(!is_real_file(&dir(
            "/usr/lib/python3.12/site-packages/pyynl"
        )));
        assert!(!is_real_file(&ghost("/var/log/ynl.log")));
        assert!(!is_real_file(&file(
            "/usr/lib/debug/.build-id/19/ad64.debug"
        )));
        assert!(!is_real_file(&file("/usr/src/debug/foo-1/bar.c")));
    }

    #[test]
    fn find_file_conflicts_flags_path_from_two_sources() {
        // kernel's python3-kernel-tools and a standalone python3-ynl
        // both own /usr/bin/ynl and the pyynl module file.
        let per_rpm = vec![
            (
                "kernel".to_string(),
                vec![
                    file("/usr/bin/ynl"),
                    file("/usr/lib/python3.12/site-packages/pyynl/cli.py"),
                    file("/usr/bin/cpupower"), // unique to kernel side
                    dir("/usr/lib/python3.12/site-packages/pyynl"),
                ],
            ),
            (
                "ynl".to_string(),
                vec![
                    file("/usr/bin/ynl"),
                    file("/usr/lib/python3.12/site-packages/pyynl/cli.py"),
                ],
            ),
        ];
        let conflicts = find_file_conflicts(&per_rpm);
        assert_eq!(conflicts.len(), 1);
        let c = &conflicts[0];
        assert_eq!(c.sources, vec!["kernel", "ynl"]);
        assert_eq!(
            c.files,
            vec![
                "/usr/bin/ynl",
                "/usr/lib/python3.12/site-packages/pyynl/cli.py"
            ]
        );
        // The directory and the kernel-only file are not conflicts.
        assert!(!c.files.iter().any(|f| f.contains("cpupower")));
    }

    #[test]
    fn find_file_conflicts_ignores_same_source_across_rpms() {
        // Two subpackages of one source legitimately split files.
        let per_rpm = vec![
            ("kernel".to_string(), vec![file("/usr/bin/ynl")]),
            ("kernel".to_string(), vec![file("/usr/bin/ynl")]),
        ];
        assert!(find_file_conflicts(&per_rpm).is_empty());
    }

    #[test]
    fn find_file_conflicts_empty_when_no_overlap() {
        let per_rpm = vec![
            ("a".to_string(), vec![file("/usr/bin/a")]),
            ("b".to_string(), vec![file("/usr/bin/b")]),
        ];
        assert!(find_file_conflicts(&per_rpm).is_empty());
    }

    #[test]
    fn scan_combines_repos_per_el_and_reports_conflicts() {
        let fetch_rpms = |tag: &str| -> Result<Vec<TaggedBinary>, Box<dyn std::error::Error>> {
            match tag {
                "hyperscale10s-packages-kernel-release" => {
                    Ok(vec![tb(1, "python3-kernel-tools", "kernel")])
                }
                "hyperscale10s-packages-main-release" => Ok(vec![tb(2, "python3-ynl", "ynl")]),
                // EL9 main exists but is conflict-free; kernel repo
                // doesn't exist on EL9 (errors -> skipped).
                "hyperscale9s-packages-main-release" => Ok(vec![tb(3, "foo", "foo")]),
                "hyperscale9s-packages-kernel-release" => Err("no such tag".into()),
                _ => Ok(vec![]),
            }
        };
        let fetch_files =
            |ids: &[i64]| -> Result<Vec<(i64, Vec<RpmFile>)>, Box<dyn std::error::Error>> {
                Ok(ids
                    .iter()
                    .map(|id| {
                        let files = match id {
                            1 | 2 => vec![file("/usr/bin/ynl")], // collide
                            _ => vec![file("/usr/bin/foo")],
                        };
                        (*id, files)
                    })
                    .collect())
            };
        let results = scan(None, EL_TOKENS, fetch_rpms, fetch_files, false);
        // Only EL10s has a conflict (main+kernel share /usr/bin/ynl).
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].el, "10s");
        assert_eq!(results[0].repos, vec!["main", "kernel"]);
        assert_eq!(results[0].conflicts.len(), 1);
        assert_eq!(results[0].conflicts[0].sources, vec!["kernel", "ynl"]);
        assert_eq!(results[0].conflicts[0].files, vec!["/usr/bin/ynl"]);
        assert_eq!(total_conflicting_files(&results), 1);
    }

    #[test]
    fn render_empty_and_populated() {
        assert_eq!(render(&[]), "No file conflicts found.\n");
        let results = vec![ElConflicts {
            el: "10s".to_string(),
            repos: vec!["main".to_string(), "kernel".to_string()],
            conflicts: vec![SourceConflict {
                sources: vec!["kernel".to_string(), "ynl".to_string()],
                files: vec!["/usr/bin/ynl".to_string()],
            }],
        }];
        let r = render(&results);
        assert!(r.contains("Found 1 conflicting file(s) across 1 source set(s)"));
        assert!(r.contains("hyperscale10s (repos: main, kernel)"));
        assert!(r.contains("kernel + ynl — 1 file(s)"));
        assert!(r.contains("/usr/bin/ynl"));
    }
}
