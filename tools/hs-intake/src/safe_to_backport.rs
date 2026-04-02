// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::compare::{self, CompareResult};
use crate::compare_buildrequires;
use crate::compare_provides;
use crate::compare_requires;
use crate::fedrq::{self, Fedrq};

/// A single Require of a reverse dependency that overlaps with the
/// backported package's Provides.
#[derive(Debug, Serialize)]
pub struct AffectedRequire {
    /// The full Requires string from the reverse dependency.
    pub required: String,
    /// Matching Provide(s) on the target branch (backport destination).
    pub target_provides: Vec<String>,
    /// Matching Provide(s) on the source branch (backport origin).
    pub source_provides: Vec<String>,
}

/// A reverse dependency whose Requires overlap with the backported
/// package's Provides.
#[derive(Debug, Serialize)]
pub struct AffectedDep {
    /// Source package name of the reverse dependency.
    pub package: String,
    /// The subset of its Requires that match our Provides.
    pub affected_requires: Vec<AffectedRequire>,
}

#[derive(Debug, Serialize)]
pub struct BackportResult {
    pub safe: bool,
    pub concerns: Vec<String>,
    pub build_requires: CompareResult,
    pub provides: CompareResult,
    pub requires: CompareResult,
    /// Reverse dependencies grouped by branch.
    pub reverse_deps: BTreeMap<String, Vec<AffectedDep>>,
}

/// Index a list of Provide/Require strings by their name (the part before
/// any ` = ` or ` >= ` operator).  Entries without an operator are keyed
/// by the full string.
fn index_by_name(entries: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in entries {
        let (name, _, _) = compare::split_entry(entry);
        map.entry(name.to_string()).or_default().push(entry.clone());
    }
    map
}

/// Evaluate whether a backport is safe given the three comparison results
/// and the reverse dependencies on checked branches.
pub fn evaluate(
    build_requires: CompareResult,
    provides: CompareResult,
    requires: CompareResult,
    reverse_deps: BTreeMap<String, Vec<AffectedDep>>,
    target_branch: &str,
) -> BackportResult {
    let mut concerns = Vec::new();

    if !build_requires.added.is_empty() {
        concerns.push(format!(
            "{} BuildRequire(s) added — may not be available on {target_branch}",
            build_requires.added.len()
        ));
    }
    if !build_requires.upgraded.is_empty() {
        concerns.push(format!(
            "{} BuildRequire(s) upgraded — {target_branch} may only have older versions",
            build_requires.upgraded.len()
        ));
    }

    if !requires.added.is_empty() {
        concerns.push(format!(
            "{} Require(s) added — may not be available on {target_branch}",
            requires.added.len()
        ));
    }
    // Solib-style requires (e.g. libc.so.6()(64bit)) are auto-generated at
    // build time and will be regenerated when the package is rebuilt on the
    // target branch, so they are not a concern.
    let non_solib_upgraded: Vec<_> = requires
        .upgraded
        .iter()
        .filter(|c| !sandogasa_depfilter::is_solib_dep(&c.name))
        .collect();
    if !non_solib_upgraded.is_empty() {
        concerns.push(format!(
            "{} Require(s) upgraded — {target_branch} may only have older versions",
            non_solib_upgraded.len()
        ));
    }

    if !provides.removed.is_empty() {
        concerns.push(format!(
            "{} Provide(s) removed — other packages on {target_branch} may depend on them",
            provides.removed.len()
        ));
    }
    if !provides.downgraded.is_empty() {
        concerns.push(format!(
            "{} Provide(s) downgraded — other packages on {target_branch} may need newer versions",
            provides.downgraded.len()
        ));
    }

    let provides_break = !provides.removed.is_empty() || !provides.downgraded.is_empty();
    if provides_break {
        for (branch, deps) in &reverse_deps {
            if !deps.is_empty() {
                let names: Vec<&str> = deps.iter().map(|d| d.package.as_str()).collect();
                concerns.push(format!(
                    "{} package(s) on {branch} depend on affected subpackages: {}",
                    deps.len(),
                    names.join(", ")
                ));
            }
        }
    }

    let safe = concerns.is_empty();

    BackportResult {
        safe,
        concerns,
        build_requires,
        provides,
        requires,
        reverse_deps,
    }
}

/// Build the detailed reverse-dependency list by cross-referencing each
/// reverse dep's Requires against the backported package's Provides on
/// both branches.
fn build_affected_deps(
    reverse_dep_names: &[String],
    branch_fq: &Fedrq,
    target_provides_by_name: &BTreeMap<String, Vec<String>>,
    source_provides_by_name: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<AffectedDep>, fedrq::Error> {
    let all_provide_names: BTreeSet<&str> = target_provides_by_name
        .keys()
        .chain(source_provides_by_name.keys())
        .map(|s| s.as_str())
        .collect();

    let mut affected_deps = Vec::new();
    for rdep in reverse_dep_names {
        let rdep_requires = branch_fq.subpkgs_requires(rdep)?;
        let mut seen = BTreeSet::new();
        let mut affected_requires = Vec::new();
        for req in &rdep_requires {
            let (name, _, _) = compare::split_entry(req);
            if all_provide_names.contains(name) && seen.insert(req.clone()) {
                affected_requires.push(AffectedRequire {
                    required: req.clone(),
                    target_provides: target_provides_by_name
                        .get(name)
                        .cloned()
                        .unwrap_or_default(),
                    source_provides: source_provides_by_name
                        .get(name)
                        .cloned()
                        .unwrap_or_default(),
                });
            }
        }
        if !affected_requires.is_empty() {
            affected_deps.push(AffectedDep {
                package: rdep.clone(),
                affected_requires,
            });
        }
    }
    Ok(affected_deps)
}

/// Query reverse dependencies on a single branch, dedup, exclude self,
/// and build the detailed affected-dep list.
fn check_branch_reverse_deps(
    srpm: &str,
    branch_fq: &Fedrq,
    subpkg_names: &[String],
    target_provides_by_name: &BTreeMap<String, Vec<String>>,
    source_provides_by_name: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<AffectedDep>, fedrq::Error> {
    let all_rdeps = branch_fq.whatrequires(subpkg_names)?;
    let reverse_dep_names: Vec<String> = all_rdeps
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|dep| dep != srpm)
        .collect();

    build_affected_deps(
        &reverse_dep_names,
        branch_fq,
        target_provides_by_name,
        source_provides_by_name,
    )
}

/// Run all three comparisons and evaluate whether backporting is safe.
///
/// `target_branch` is the branch to backport to (e.g. "c9s").
/// `source_branch` is the branch to take the package from (e.g. "f44").
/// `also_check` is additional branches to check for reverse dependencies.
pub fn safe_to_backport(
    srpm: &str,
    target_branch: &str,
    source_branch: &str,
    also_check: &[String],
) -> Result<BackportResult, fedrq::Error> {
    // Compare functions expect (compare_from, compare_to), i.e. (staler, fresher).
    let build_requires =
        compare_buildrequires::compare_buildrequires(srpm, target_branch, source_branch)?;
    let provides = compare_provides::compare_provides(srpm, target_branch, source_branch)?;
    let requires = compare_requires::compare_requires(srpm, target_branch, source_branch)?;

    let target_fq = Fedrq {
        branch: Some(target_branch.to_string()),
        ..Default::default()
    };
    let source_fq = Fedrq {
        branch: Some(source_branch.to_string()),
        ..Default::default()
    };

    // Use the target branch's subpackage names for all reverse-dep checks,
    // since those are the subpackages being replaced.
    let subpkg_names = target_fq.subpkgs_names(srpm)?;

    // Build Provides indexes for cross-referencing.
    let target_provides_raw = target_fq.subpkgs_provides(srpm)?;
    let source_provides_raw = source_fq.subpkgs_provides(srpm)?;
    let target_provides_by_name = index_by_name(&target_provides_raw);
    let source_provides_by_name = index_by_name(&source_provides_raw);

    // Check reverse deps on the target branch and any additional branches.
    let mut reverse_deps = BTreeMap::new();

    let target_affected = check_branch_reverse_deps(
        srpm,
        &target_fq,
        &subpkg_names,
        &target_provides_by_name,
        &source_provides_by_name,
    )?;
    reverse_deps.insert(target_branch.to_string(), target_affected);

    for branch in also_check {
        let branch_fq = Fedrq {
            branch: Some(branch.clone()),
            ..Default::default()
        };
        let affected = check_branch_reverse_deps(
            srpm,
            &branch_fq,
            &subpkg_names,
            &target_provides_by_name,
            &source_provides_by_name,
        )?;
        reverse_deps.insert(branch.clone(), affected);
    }

    Ok(evaluate(
        build_requires,
        provides,
        requires,
        reverse_deps,
        target_branch,
    ))
}

/// Format a `BackportResult` as a human-readable string.
pub fn format_result(
    result: &BackportResult,
    srpm: &str,
    target_branch: &str,
    source_branch: &str,
) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    if result.safe {
        writeln!(
            out,
            "Backporting {srpm} from {source_branch} to {target_branch}: SAFE"
        )
        .unwrap();
    } else {
        writeln!(
            out,
            "Backporting {srpm} from {source_branch} to {target_branch}: NOT SAFE"
        )
        .unwrap();
        writeln!(out).unwrap();
        writeln!(out, "Concerns:").unwrap();
        for c in &result.concerns {
            writeln!(out, "  - {c}").unwrap();
        }
    }

    let sections = [
        ("BuildRequire", &result.build_requires),
        ("Provide", &result.provides),
        ("Require", &result.requires),
    ];

    for (label, cmp) in sections {
        if cmp.added.is_empty()
            && cmp.removed.is_empty()
            && cmp.upgraded.is_empty()
            && cmp.downgraded.is_empty()
        {
            continue;
        }
        writeln!(out).unwrap();
        write!(
            out,
            "{}",
            compare::format_result(cmp, label, target_branch, source_branch, false)
        )
        .unwrap();
    }

    for (branch, deps) in &result.reverse_deps {
        if deps.is_empty() {
            continue;
        }
        writeln!(out).unwrap();
        writeln!(out, "Reverse dependencies on {branch}:").unwrap();
        for dep in deps {
            writeln!(out, "  {}:", dep.package).unwrap();
            for req in &dep.affected_requires {
                if req.target_provides == req.source_provides {
                    writeln!(
                        out,
                        "    - {} — provided by both {target_branch} and {source_branch}",
                        req.required
                    )
                    .unwrap();
                } else {
                    writeln!(out, "    - {}", req.required).unwrap();
                    for prov in &req.target_provides {
                        writeln!(out, "      {target_branch}: {prov}").unwrap();
                    }
                    if req.source_provides.is_empty() {
                        writeln!(out, "      {source_branch}: (not provided)").unwrap();
                    } else {
                        for prov in &req.source_provides {
                            writeln!(out, "      {source_branch}: {prov}").unwrap();
                        }
                    }
                }
            }
        }
    }
    out
}

/// Print a `BackportResult` in human-readable format.
pub fn print_result(result: &BackportResult, srpm: &str, target_branch: &str, source_branch: &str) {
    print!(
        "{}",
        format_result(result, srpm, target_branch, source_branch)
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::VersionChange;

    fn empty_result() -> CompareResult {
        CompareResult {
            added: vec![],
            removed: vec![],
            upgraded: vec![],
            downgraded: vec![],
            unchanged: vec![],
        }
    }

    #[test]
    fn index_by_name_groups_by_name() {
        let entries = vec![
            "libbpf = 2:1.5.0-3.el9".to_string(),
            "libbpf(aarch-64) = 2:1.5.0-3.el9".to_string(),
            "libbpf.so.1()(64bit)".to_string(),
        ];
        let idx = index_by_name(&entries);
        assert_eq!(idx["libbpf"], vec!["libbpf = 2:1.5.0-3.el9"]);
        assert_eq!(
            idx["libbpf(aarch-64)"],
            vec!["libbpf(aarch-64) = 2:1.5.0-3.el9"]
        );
        assert_eq!(idx["libbpf.so.1()(64bit)"], vec!["libbpf.so.1()(64bit)"]);
    }

    #[test]
    fn evaluate_all_empty_is_safe() {
        let result = evaluate(
            empty_result(),
            empty_result(),
            empty_result(),
            BTreeMap::new(),
            "c9s",
        );
        assert!(result.safe);
        assert!(result.concerns.is_empty());
    }

    #[test]
    fn evaluate_buildrequires_added_is_unsafe() {
        let mut br = empty_result();
        br.added = vec!["newdep".to_string()];
        let result = evaluate(br, empty_result(), empty_result(), BTreeMap::new(), "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 1);
        assert!(result.concerns[0].contains("1 BuildRequire(s) added"));
        assert!(result.concerns[0].contains("c9s"));
    }

    #[test]
    fn evaluate_buildrequires_upgraded_is_unsafe() {
        let mut br = empty_result();
        br.upgraded = vec![VersionChange {
            name: "gcc".to_string(),
            source_version: "12.0".to_string(),
            target_version: "14.0".to_string(),
        }];
        let result = evaluate(br, empty_result(), empty_result(), BTreeMap::new(), "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 1);
        assert!(result.concerns[0].contains("1 BuildRequire(s) upgraded"));
    }

    #[test]
    fn evaluate_requires_added_is_unsafe() {
        let mut req = empty_result();
        req.added = vec!["newlib".to_string()];
        let result = evaluate(empty_result(), empty_result(), req, BTreeMap::new(), "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 1);
        assert!(result.concerns[0].contains("1 Require(s) added"));
    }

    #[test]
    fn evaluate_requires_upgraded_is_unsafe() {
        let mut req = empty_result();
        req.upgraded = vec![VersionChange {
            name: "glibc".to_string(),
            source_version: "2.28".to_string(),
            target_version: "2.38".to_string(),
        }];
        let result = evaluate(empty_result(), empty_result(), req, BTreeMap::new(), "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 1);
        assert!(result.concerns[0].contains("1 Require(s) upgraded"));
    }

    #[test]
    fn evaluate_requires_solib_upgraded_is_safe() {
        let mut req = empty_result();
        req.upgraded = vec![VersionChange {
            name: "libc.so.6()(64bit)".to_string(),
            source_version: "GLIBC_2.28".to_string(),
            target_version: "GLIBC_2.38".to_string(),
        }];
        let result = evaluate(empty_result(), empty_result(), req, BTreeMap::new(), "c9s");
        assert!(result.safe);
        assert!(result.concerns.is_empty());
    }

    #[test]
    fn evaluate_requires_mixed_upgraded() {
        let mut req = empty_result();
        req.upgraded = vec![
            VersionChange {
                name: "libc.so.6()(64bit)".to_string(),
                source_version: "GLIBC_2.28".to_string(),
                target_version: "GLIBC_2.38".to_string(),
            },
            VersionChange {
                name: "libfoo".to_string(),
                source_version: "1.0".to_string(),
                target_version: "2.0".to_string(),
            },
        ];
        let result = evaluate(empty_result(), empty_result(), req, BTreeMap::new(), "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 1);
        assert!(result.concerns[0].contains("1 Require(s) upgraded"));
    }

    #[test]
    fn evaluate_provides_removed_is_unsafe() {
        let mut prov = empty_result();
        prov.removed = vec!["libold.so".to_string()];
        let result = evaluate(empty_result(), prov, empty_result(), BTreeMap::new(), "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 1);
        assert!(result.concerns[0].contains("1 Provide(s) removed"));
    }

    #[test]
    fn evaluate_provides_downgraded_is_unsafe() {
        let mut prov = empty_result();
        prov.downgraded = vec![VersionChange {
            name: "libfoo".to_string(),
            source_version: "3.0".to_string(),
            target_version: "2.0".to_string(),
        }];
        let result = evaluate(empty_result(), prov, empty_result(), BTreeMap::new(), "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 1);
        assert!(result.concerns[0].contains("1 Provide(s) downgraded"));
    }

    #[test]
    fn evaluate_multiple_concerns() {
        let mut br = empty_result();
        br.added = vec!["newdep".to_string()];
        let mut prov = empty_result();
        prov.removed = vec!["libold.so".to_string()];
        let mut req = empty_result();
        req.upgraded = vec![VersionChange {
            name: "glibc".to_string(),
            source_version: "2.28".to_string(),
            target_version: "2.38".to_string(),
        }];
        let result = evaluate(br, prov, req, BTreeMap::new(), "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 3);
    }

    #[test]
    fn evaluate_safe_changes_are_ignored() {
        let mut br = empty_result();
        br.removed = vec!["olddep".to_string()];
        br.downgraded = vec![VersionChange {
            name: "gcc".to_string(),
            source_version: "14.0".to_string(),
            target_version: "12.0".to_string(),
        }];
        let mut prov = empty_result();
        prov.added = vec!["libnew.so".to_string()];
        prov.upgraded = vec![VersionChange {
            name: "libfoo".to_string(),
            source_version: "1.0".to_string(),
            target_version: "2.0".to_string(),
        }];
        let mut req = empty_result();
        req.removed = vec!["libold".to_string()];
        req.downgraded = vec![VersionChange {
            name: "glibc".to_string(),
            source_version: "2.38".to_string(),
            target_version: "2.28".to_string(),
        }];
        let result = evaluate(br, prov, req, BTreeMap::new(), "c9s");
        assert!(result.safe);
        assert!(result.concerns.is_empty());
    }

    fn make_affected_dep(name: &str, reqs: Vec<(&str, Vec<&str>, Vec<&str>)>) -> AffectedDep {
        AffectedDep {
            package: name.to_string(),
            affected_requires: reqs
                .into_iter()
                .map(|(req, tgt, src)| AffectedRequire {
                    required: req.to_string(),
                    target_provides: tgt.into_iter().map(|s| s.to_string()).collect(),
                    source_provides: src.into_iter().map(|s| s.to_string()).collect(),
                })
                .collect(),
        }
    }

    fn rdeps_map(branch: &str, deps: Vec<AffectedDep>) -> BTreeMap<String, Vec<AffectedDep>> {
        BTreeMap::from([(branch.to_string(), deps)])
    }

    #[test]
    fn evaluate_reverse_deps_with_provides_removed() {
        let mut prov = empty_result();
        prov.removed = vec!["libold.so".to_string()];
        let rdeps = rdeps_map(
            "c9s",
            vec![
                make_affected_dep("bpftrace", vec![("libold.so", vec!["libold.so"], vec![])]),
                make_affected_dep("iproute", vec![("libold.so", vec!["libold.so"], vec![])]),
            ],
        );
        let result = evaluate(empty_result(), prov, empty_result(), rdeps, "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 2);
        assert!(result.concerns[1].contains("2 package(s)"));
        assert!(result.concerns[1].contains("bpftrace"));
        assert!(result.concerns[1].contains("iproute"));
    }

    #[test]
    fn evaluate_reverse_deps_with_provides_downgraded() {
        let mut prov = empty_result();
        prov.downgraded = vec![VersionChange {
            name: "libfoo".to_string(),
            source_version: "3.0".to_string(),
            target_version: "2.0".to_string(),
        }];
        let rdeps = rdeps_map(
            "c9s",
            vec![make_affected_dep(
                "systemd",
                vec![("libfoo >= 2.0", vec!["libfoo = 3.0"], vec!["libfoo = 2.0"])],
            )],
        );
        let result = evaluate(empty_result(), prov, empty_result(), rdeps, "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 2);
        assert!(result.concerns[1].contains("1 package(s)"));
        assert!(result.concerns[1].contains("systemd"));
    }

    #[test]
    fn evaluate_reverse_deps_without_provides_changes_is_safe() {
        let mut prov = empty_result();
        prov.upgraded = vec![VersionChange {
            name: "libfoo".to_string(),
            source_version: "1.0".to_string(),
            target_version: "2.0".to_string(),
        }];
        let rdeps = rdeps_map(
            "c9s",
            vec![make_affected_dep(
                "bpftrace",
                vec![("libfoo >= 1.0", vec!["libfoo = 1.0"], vec!["libfoo = 2.0"])],
            )],
        );
        let result = evaluate(empty_result(), prov, empty_result(), rdeps, "c9s");
        assert!(result.safe);
        assert!(result.concerns.is_empty());
        assert_eq!(result.reverse_deps["c9s"].len(), 1);
        assert_eq!(result.reverse_deps["c9s"][0].package, "bpftrace");
    }

    #[test]
    fn evaluate_reverse_deps_multiple_branches() {
        let mut prov = empty_result();
        prov.removed = vec!["libold.so".to_string()];
        let mut rdeps = BTreeMap::new();
        rdeps.insert(
            "c9s".to_string(),
            vec![make_affected_dep(
                "bpftrace",
                vec![("libold.so", vec!["libold.so"], vec![])],
            )],
        );
        rdeps.insert(
            "c9s-hyperscale".to_string(),
            vec![make_affected_dep(
                "systemd",
                vec![("libold.so", vec!["libold.so"], vec![])],
            )],
        );
        let result = evaluate(empty_result(), prov, empty_result(), rdeps, "c9s");
        assert!(!result.safe);
        // "Provide(s) removed" + concern for c9s + concern for c9s-hyperscale
        assert_eq!(result.concerns.len(), 3);
        assert!(result.concerns[1].contains("c9s"));
        assert!(result.concerns[1].contains("bpftrace"));
        assert!(result.concerns[2].contains("c9s-hyperscale"));
        assert!(result.concerns[2].contains("systemd"));
    }

    #[test]
    fn print_result_safe() {
        let result = BackportResult {
            safe: true,
            concerns: vec![],
            build_requires: empty_result(),
            provides: empty_result(),
            requires: empty_result(),
            reverse_deps: BTreeMap::new(),
        };
        print_result(&result, "systemd", "c9s", "rawhide");
    }

    #[test]
    fn print_result_unsafe_with_details() {
        let mut br = empty_result();
        br.added = vec!["newdep".to_string()];
        let result = BackportResult {
            safe: false,
            concerns: vec!["1 BuildRequire(s) added — may not be available on c9s".to_string()],
            build_requires: br,
            provides: empty_result(),
            requires: empty_result(),
            reverse_deps: BTreeMap::new(),
        };
        print_result(&result, "systemd", "c9s", "rawhide");
    }

    #[test]
    fn print_result_with_reverse_deps_same_provides() {
        let result = BackportResult {
            safe: true,
            concerns: vec![],
            build_requires: empty_result(),
            provides: empty_result(),
            requires: empty_result(),
            reverse_deps: rdeps_map(
                "c9s",
                vec![make_affected_dep(
                    "libabigail",
                    vec![(
                        "libbpf.so.1()(64bit)",
                        vec!["libbpf.so.1()(64bit)"],
                        vec!["libbpf.so.1()(64bit)"],
                    )],
                )],
            ),
        };
        print_result(&result, "libbpf", "c9s", "f44");
    }

    #[test]
    fn print_result_with_reverse_deps_different_provides() {
        let mut prov = empty_result();
        prov.removed = vec!["libold.so".to_string()];
        let result = BackportResult {
            safe: false,
            concerns: vec![
                "1 Provide(s) removed — other packages on c9s may depend on them".to_string(),
                "1 package(s) on c9s depend on affected subpackages: libabigail".to_string(),
            ],
            build_requires: empty_result(),
            provides: prov,
            requires: empty_result(),
            reverse_deps: rdeps_map(
                "c9s",
                vec![make_affected_dep(
                    "libabigail",
                    vec![
                        (
                            "libbpf = 2:1.5.0-3.el9",
                            vec!["libbpf = 2:1.5.0-3.el9"],
                            vec!["libbpf = 2:1.6.3-1.fc44"],
                        ),
                        ("libold.so", vec!["libold.so"], vec![]),
                    ],
                )],
            ),
        };
        print_result(&result, "libbpf", "c9s", "f44");
    }

    #[test]
    fn print_result_with_multiple_branches() {
        let mut rdeps = BTreeMap::new();
        rdeps.insert(
            "c9s".to_string(),
            vec![make_affected_dep(
                "bpftrace",
                vec![(
                    "libbpf.so.1()(64bit)",
                    vec!["libbpf.so.1()(64bit)"],
                    vec!["libbpf.so.1()(64bit)"],
                )],
            )],
        );
        rdeps.insert(
            "c9s-hyperscale".to_string(),
            vec![make_affected_dep(
                "systemd",
                vec![(
                    "libbpf.so.1()(64bit)",
                    vec!["libbpf.so.1()(64bit)"],
                    vec!["libbpf.so.1()(64bit)"],
                )],
            )],
        );
        let result = BackportResult {
            safe: true,
            concerns: vec![],
            build_requires: empty_result(),
            provides: empty_result(),
            requires: empty_result(),
            reverse_deps: rdeps,
        };
        print_result(&result, "libbpf", "c9s", "f44");
    }

    #[test]
    fn print_result_skips_empty_branch() {
        let mut rdeps = BTreeMap::new();
        rdeps.insert("c9s".to_string(), vec![]);
        rdeps.insert(
            "c9s-hyperscale".to_string(),
            vec![make_affected_dep(
                "systemd",
                vec![(
                    "libbpf.so.1()(64bit)",
                    vec!["libbpf.so.1()(64bit)"],
                    vec!["libbpf.so.1()(64bit)"],
                )],
            )],
        );
        let result = BackportResult {
            safe: true,
            concerns: vec![],
            build_requires: empty_result(),
            provides: empty_result(),
            requires: empty_result(),
            reverse_deps: rdeps,
        };
        // Should only print c9s-hyperscale section, not empty c9s.
        print_result(&result, "libbpf", "c9s", "f44");
    }
}
