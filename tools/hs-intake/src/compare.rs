// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::rpmvercmp::compare_evr;

/// An entry whose version changed between branches.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct VersionChange {
    pub name: String,
    pub source_version: String,
    pub target_version: String,
}

/// Result of comparing entries between two branches.
#[derive(Debug, Serialize)]
pub struct CompareResult {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub upgraded: Vec<VersionChange>,
    pub downgraded: Vec<VersionChange>,
    pub unchanged: Vec<String>,
}

/// Split an RPM dependency/provide string into (name, operator, version).
/// Recognizes ` = ` and ` >= ` as separators.
/// Returns `None` for operator and version if neither separator is found.
pub fn split_entry(entry: &str) -> (&str, Option<&str>, Option<&str>) {
    // Try ` >= ` first since ` = ` would also match the `= ` portion of ` >= `.
    if let Some((name, version)) = entry.split_once(" >= ") {
        return (name, Some(">="), Some(version));
    }
    if let Some((name, version)) = entry.split_once(" = ") {
        return (name, Some("="), Some(version));
    }
    (entry, None, None)
}

/// Split a solib-style entry like `libc.so.6(GLIBC_2.28)(64bit)` into
/// (name_pattern, version_symbol), e.g. `("libc.so.6()(64bit)", "GLIBC_2.28")`.
/// Returns `None` if the entry has no non-empty first parenthesized group.
fn split_solib(entry: &str) -> Option<(String, &str)> {
    let open = entry.find('(')?;
    // Only treat as solib if the prefix contains ".so." (e.g. libc.so.6).
    // This avoids matching non-solib entries like pkgconfig(dracut).
    if !entry[..open].contains(".so.") {
        return None;
    }
    let close = entry[open..].find(')')? + open;
    let content = &entry[open + 1..close];
    if content.is_empty() {
        return None;
    }
    let name = format!("{}(){}", &entry[..open], &entry[close + 1..]);
    Some((name, content))
}

/// Filter out entries whose name (the part before any operator) matches
/// one of the given subpackage names.  This removes self-dependencies
/// when comparing Requires or BuildRequires.
pub fn filter_self_deps(entries: Vec<String>, subpkg_names: &BTreeSet<String>) -> Vec<String> {
    entries
        .into_iter()
        .filter(|entry| {
            let (name, _, _) = split_entry(entry);
            !subpkg_names.contains(name)
        })
        .collect()
}

/// Diff two sets of RPM dependency/provide strings, detecting upgrades
/// where the name matches but the version differs.
pub fn diff(source: Vec<String>, target: Vec<String>) -> CompareResult {
    let source_set: BTreeSet<String> = source.into_iter().collect();
    let target_set: BTreeSet<String> = target.into_iter().collect();

    let raw_added: BTreeSet<&String> = target_set.difference(&source_set).collect();
    let raw_removed: BTreeSet<&String> = source_set.difference(&target_set).collect();
    let unchanged: Vec<String> = source_set.intersection(&target_set).cloned().collect();

    // Index removed entries by their name (LHS of operator) to find upgrades.
    let mut removed_by_name: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for r in &raw_removed {
        let (name, op, version) = split_entry(r);
        if let (Some(op), Some(v)) = (op, version) {
            removed_by_name.insert(name, (op, v));
        }
    }

    let mut added = Vec::new();
    let mut changed: Vec<VersionChange> = Vec::new();
    let mut changed_names: BTreeSet<&str> = BTreeSet::new();

    for a in &raw_added {
        let (name, op, version) = split_entry(a);
        match (op, version, removed_by_name.get(name)) {
            (Some(target_op), Some(target_v), Some(&(source_op, source_v)))
                if target_op == source_op =>
            {
                let prefix = if target_op == "=" { "" } else { ">= " };
                changed.push(VersionChange {
                    name: name.to_string(),
                    source_version: format!("{prefix}{source_v}"),
                    target_version: format!("{prefix}{target_v}"),
                });
                changed_names.insert(name);
            }
            _ => added.push(a.to_string()),
        }
    }

    let remaining_removed: Vec<String> = raw_removed
        .iter()
        .filter(|r| {
            let (name, _, _) = split_entry(r);
            !changed_names.contains(name)
        })
        .map(|r| r.to_string())
        .collect();

    // Second pass: match solib-style entries like libc.so.6(GLIBC_2.28)(64bit).
    let mut solib_removed_by_pattern: BTreeMap<String, (String, &str)> = BTreeMap::new();
    for r in &remaining_removed {
        if let Some((pattern, version)) = split_solib(r) {
            solib_removed_by_pattern.insert(pattern, (r.clone(), version));
        }
    }

    let mut solib_changed_originals: BTreeSet<String> = BTreeSet::new();
    let mut final_added = Vec::new();
    for a in &added {
        if let Some((pattern, target_v)) = split_solib(a) {
            if let Some((original, source_v)) = solib_removed_by_pattern.get(&pattern) {
                changed.push(VersionChange {
                    name: pattern,
                    source_version: source_v.to_string(),
                    target_version: target_v.to_string(),
                });
                solib_changed_originals.insert(original.clone());
                continue;
            }
        }
        final_added.push(a.clone());
    }

    let removed: Vec<String> = remaining_removed
        .into_iter()
        .filter(|r| !solib_changed_originals.contains(r))
        .collect();

    changed.sort_by(|a, b| a.name.cmp(&b.name));

    // Split changed into upgraded vs downgraded using RPM version comparison.
    // Strip the ">= " prefix before comparing, as it's not part of the EVR.
    let mut upgraded = Vec::new();
    let mut downgraded = Vec::new();
    for c in changed {
        let src = c.source_version.strip_prefix(">= ").unwrap_or(&c.source_version);
        let tgt = c.target_version.strip_prefix(">= ").unwrap_or(&c.target_version);
        match compare_evr(src, tgt) {
            Ordering::Less => upgraded.push(c),
            Ordering::Greater => downgraded.push(c),
            Ordering::Equal => upgraded.push(c),
        }
    }

    CompareResult { added: final_added, removed, upgraded, downgraded, unchanged }
}

/// Print a `CompareResult` in human-readable format.
///
/// When `show_unchanged` is true, entries that are identical in both
/// branches are listed at the end.
pub fn print_result(
    result: &CompareResult,
    label: &str,
    source_branch: &str,
    target_branch: &str,
    show_unchanged: bool,
) {
    if result.added.is_empty()
        && result.removed.is_empty()
        && result.upgraded.is_empty()
        && result.downgraded.is_empty()
    {
        println!("No differences in {label}.");
        if show_unchanged && !result.unchanged.is_empty() {
            println!();
            println!("Unchanged:");
            for p in &result.unchanged {
                println!("  {p}");
            }
        }
        return;
    }
    let mut need_blank = false;
    for (section_label, entries) in [
        ("Upgraded", &result.upgraded),
        ("Downgraded", &result.downgraded),
    ] {
        if entries.is_empty() {
            continue;
        }
        if need_blank {
            println!();
        }
        let name_w = entries.iter().map(|u| u.name.len()).max().unwrap();
        let src_w = entries
            .iter()
            .map(|u| u.source_version.len())
            .max()
            .unwrap()
            .max(source_branch.len());
        let tgt_w = entries
            .iter()
            .map(|u| u.target_version.len())
            .max()
            .unwrap()
            .max(target_branch.len());

        let sep = format!(
            "+-{}-+-{}-+-{}-+",
            "-".repeat(name_w),
            "-".repeat(src_w),
            "-".repeat(tgt_w)
        );
        println!("{section_label} ({source_branch} -> {target_branch}):");
        println!("{sep}");
        println!(
            "| {:<name_w$} | {:<src_w$} | {:<tgt_w$} |",
            label, source_branch, target_branch
        );
        println!("{sep}");
        for u in entries {
            println!(
                "| {:<name_w$} | {:<src_w$} | {:<tgt_w$} |",
                u.name, u.source_version, u.target_version
            );
        }
        println!("{sep}");
        need_blank = true;
    }
    if !result.removed.is_empty() {
        if need_blank {
            println!();
        }
        println!("Removed (in {source_branch} but not {target_branch}):");
        for p in &result.removed {
            println!("  - {p}");
        }
        need_blank = true;
    }
    if !result.added.is_empty() {
        if need_blank {
            println!();
        }
        println!("Added (in {target_branch} but not {source_branch}):");
        for p in &result.added {
            println!("  + {p}");
        }
        need_blank = true;
    }
    if show_unchanged && !result.unchanged.is_empty() {
        if need_blank {
            println!();
        }
        println!("Unchanged:");
        for p in &result.unchanged {
            println!("  {p}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_self_deps_removes_matching_names() {
        let entries = vec![
            "libbpf = 2:1.5.0-3.el9".to_string(),
            "libbpf-devel = 2:1.5.0-3.el9".to_string(),
            "glibc >= 2.28".to_string(),
            "kernel-headers".to_string(),
            "libbpf.so.1()(64bit)".to_string(),
        ];
        let self_names: BTreeSet<String> =
            ["libbpf", "libbpf-devel"].iter().map(|s| s.to_string()).collect();
        let filtered = filter_self_deps(entries, &self_names);
        assert_eq!(
            filtered,
            vec![
                "glibc >= 2.28",
                "kernel-headers",
                "libbpf.so.1()(64bit)",
            ]
        );
    }

    #[test]
    fn filter_self_deps_empty_names() {
        let entries = vec!["foo".to_string(), "bar = 1.0".to_string()];
        let self_names: BTreeSet<String> = BTreeSet::new();
        let filtered = filter_self_deps(entries.clone(), &self_names);
        assert_eq!(filtered, entries);
    }

    #[test]
    fn split_entry_with_equals() {
        let (name, op, version) = split_entry("libbpf(aarch-64) = 2:1.5.0-3.el9");
        assert_eq!(name, "libbpf(aarch-64)");
        assert_eq!(op, Some("="));
        assert_eq!(version, Some("2:1.5.0-3.el9"));
    }

    #[test]
    fn split_entry_with_gte() {
        let (name, op, version) = split_entry("kernel-headers >= 5.14.0-647");
        assert_eq!(name, "kernel-headers");
        assert_eq!(op, Some(">="));
        assert_eq!(version, Some("5.14.0-647"));
    }

    #[test]
    fn split_entry_without_version() {
        let (name, op, version) = split_entry("/usr/sbin/halt");
        assert_eq!(name, "/usr/sbin/halt");
        assert_eq!(op, None);
        assert_eq!(version, None);
    }

    #[test]
    fn diff_empty() {
        let result = diff(vec![], vec![]);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.upgraded.is_empty());
        assert!(result.downgraded.is_empty());
    }

    #[test]
    fn diff_added_only() {
        let result = diff(vec![], vec!["foo".to_string()]);
        assert_eq!(result.added, vec!["foo"]);
        assert!(result.removed.is_empty());
        assert!(result.upgraded.is_empty());
        assert!(result.downgraded.is_empty());
    }

    #[test]
    fn diff_removed_only() {
        let result = diff(vec!["foo".to_string()], vec![]);
        assert!(result.added.is_empty());
        assert_eq!(result.removed, vec!["foo"]);
        assert!(result.upgraded.is_empty());
        assert!(result.downgraded.is_empty());
    }

    #[test]
    fn diff_detects_upgrade() {
        let source = vec!["libbpf = 1.0".to_string()];
        let target = vec!["libbpf = 2.0".to_string()];
        let result = diff(source, target);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.downgraded.is_empty());
        assert_eq!(result.upgraded.len(), 1);
        assert_eq!(result.upgraded[0].name, "libbpf");
        assert_eq!(result.upgraded[0].source_version, "1.0");
        assert_eq!(result.upgraded[0].target_version, "2.0");
    }

    #[test]
    fn diff_detects_downgrade() {
        let source = vec!["libbpf = 2.0".to_string()];
        let target = vec!["libbpf = 1.0".to_string()];
        let result = diff(source, target);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.upgraded.is_empty());
        assert_eq!(result.downgraded.len(), 1);
        assert_eq!(result.downgraded[0].name, "libbpf");
        assert_eq!(result.downgraded[0].source_version, "2.0");
        assert_eq!(result.downgraded[0].target_version, "1.0");
    }

    #[test]
    fn diff_mixed_changes() {
        let source = vec![
            "libfoo = 1.0".to_string(),
            "libold".to_string(),
        ];
        let target = vec![
            "libfoo = 2.0".to_string(),
            "libnew".to_string(),
        ];
        let result = diff(source, target);
        assert_eq!(result.added, vec!["libnew"]);
        assert_eq!(result.removed, vec!["libold"]);
        assert_eq!(result.upgraded.len(), 1);
        assert_eq!(result.upgraded[0].name, "libfoo");
        assert!(result.downgraded.is_empty());
    }

    #[test]
    fn diff_detects_gte_upgrade() {
        let source = vec!["kernel-headers >= 5.14.0-647".to_string()];
        let target = vec!["kernel-headers >= 5.16.0".to_string()];
        let result = diff(source, target);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.downgraded.is_empty());
        assert_eq!(result.upgraded.len(), 1);
        assert_eq!(result.upgraded[0].name, "kernel-headers");
        assert_eq!(result.upgraded[0].source_version, ">= 5.14.0-647");
        assert_eq!(result.upgraded[0].target_version, ">= 5.16.0");
    }

    #[test]
    fn diff_detects_gte_downgrade() {
        let source = vec!["kernel-headers >= 5.16.0".to_string()];
        let target = vec!["kernel-headers >= 5.14.0-647".to_string()];
        let result = diff(source, target);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.upgraded.is_empty());
        assert_eq!(result.downgraded.len(), 1);
        assert_eq!(result.downgraded[0].name, "kernel-headers");
    }

    #[test]
    fn split_solib_with_version() {
        let (name, version) = split_solib("libc.so.6(GLIBC_2.28)(64bit)").unwrap();
        assert_eq!(name, "libc.so.6()(64bit)");
        assert_eq!(version, "GLIBC_2.28");
    }

    #[test]
    fn split_solib_empty_parens() {
        assert!(split_solib("libfoo.so()(64bit)").is_none());
    }

    #[test]
    fn split_solib_no_parens() {
        assert!(split_solib("/usr/sbin/halt").is_none());
    }

    #[test]
    fn split_solib_non_solib_parens() {
        assert!(split_solib("pkgconfig(dracut)").is_none());
    }

    #[test]
    fn diff_detects_solib_upgrade() {
        let source = vec!["libc.so.6(GLIBC_2.28)(64bit)".to_string()];
        let target = vec!["libc.so.6(GLIBC_2.38)(64bit)".to_string()];
        let result = diff(source, target);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.downgraded.is_empty());
        assert_eq!(result.upgraded.len(), 1);
        assert_eq!(result.upgraded[0].name, "libc.so.6()(64bit)");
        assert_eq!(result.upgraded[0].source_version, "GLIBC_2.28");
        assert_eq!(result.upgraded[0].target_version, "GLIBC_2.38");
    }

    #[test]
    fn diff_detects_solib_downgrade() {
        let source = vec!["libc.so.6(GLIBC_2.38)(64bit)".to_string()];
        let target = vec!["libc.so.6(GLIBC_2.28)(64bit)".to_string()];
        let result = diff(source, target);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.upgraded.is_empty());
        assert_eq!(result.downgraded.len(), 1);
        assert_eq!(result.downgraded[0].name, "libc.so.6()(64bit)");
    }

    #[test]
    fn diff_non_solib_parens_not_matched() {
        let source = vec!["pkgconfig(dracut)".to_string()];
        let target = vec!["pkgconfig(systemd)".to_string()];
        let result = diff(source, target);
        assert_eq!(result.added, vec!["pkgconfig(systemd)"]);
        assert_eq!(result.removed, vec!["pkgconfig(dracut)"]);
        assert!(result.upgraded.is_empty());
        assert!(result.downgraded.is_empty());
    }

    #[test]
    fn diff_no_changes() {
        let entries = vec!["foo = 1.0".to_string(), "bar".to_string()];
        let result = diff(entries.clone(), entries);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.upgraded.is_empty());
        assert!(result.downgraded.is_empty());
        assert_eq!(result.unchanged, vec!["bar", "foo = 1.0"]);
    }

    #[test]
    fn diff_tracks_unchanged() {
        let source = vec![
            "libfoo = 1.0".to_string(),
            "libbar".to_string(),
            "libold".to_string(),
        ];
        let target = vec![
            "libfoo = 2.0".to_string(),
            "libbar".to_string(),
            "libnew".to_string(),
        ];
        let result = diff(source, target);
        assert_eq!(result.unchanged, vec!["libbar"]);
    }

    #[test]
    fn print_result_no_differences() {
        let result = CompareResult {
            added: vec![],
            removed: vec![],
            upgraded: vec![],
            downgraded: vec![],
            unchanged: vec![],
        };
        // Exercises the early-return "No differences" path.
        print_result(&result, "Provide", "f41", "rawhide", false);
    }

    #[test]
    fn print_result_upgraded_only() {
        let result = CompareResult {
            added: vec![],
            removed: vec![],
            upgraded: vec![VersionChange {
                name: "libbpf".to_string(),
                source_version: "1.0".to_string(),
                target_version: "2.0".to_string(),
            }],
            downgraded: vec![],
            unchanged: vec![],
        };
        print_result(&result, "Require", "c9s", "f44", false);
    }

    #[test]
    fn print_result_downgraded_only() {
        let result = CompareResult {
            added: vec![],
            removed: vec![],
            upgraded: vec![],
            downgraded: vec![VersionChange {
                name: "libbpf".to_string(),
                source_version: "2.0".to_string(),
                target_version: "1.0".to_string(),
            }],
            unchanged: vec![],
        };
        print_result(&result, "Require", "f44", "c9s", false);
    }

    #[test]
    fn print_result_added_and_removed() {
        let result = CompareResult {
            added: vec!["libnew".to_string()],
            removed: vec!["libold".to_string()],
            upgraded: vec![],
            downgraded: vec![],
            unchanged: vec![],
        };
        print_result(&result, "Provide", "f41", "rawhide", false);
    }

    #[test]
    fn print_result_all_sections() {
        let result = CompareResult {
            added: vec!["libnew".to_string()],
            removed: vec!["libold".to_string()],
            upgraded: vec![VersionChange {
                name: "libfoo".to_string(),
                source_version: "1.0".to_string(),
                target_version: "2.0".to_string(),
            }],
            downgraded: vec![VersionChange {
                name: "libbar".to_string(),
                source_version: "3.0".to_string(),
                target_version: "1.0".to_string(),
            }],
            unchanged: vec![],
        };
        // Exercises all four sections including need_blank separators.
        print_result(&result, "Require", "f41", "c9s", false);
    }

    #[test]
    fn print_result_show_unchanged_with_differences() {
        let result = CompareResult {
            added: vec!["libnew".to_string()],
            removed: vec![],
            upgraded: vec![],
            downgraded: vec![],
            unchanged: vec!["libcommon".to_string()],
        };
        print_result(&result, "Provide", "f41", "rawhide", true);
    }

    #[test]
    fn print_result_show_unchanged_no_differences() {
        let result = CompareResult {
            added: vec![],
            removed: vec![],
            upgraded: vec![],
            downgraded: vec![],
            unchanged: vec!["libcommon".to_string()],
        };
        // Exercises the "No differences" + unchanged path.
        print_result(&result, "Provide", "f41", "rawhide", true);
    }

    #[test]
    fn print_result_all_sections_with_unchanged() {
        let result = CompareResult {
            added: vec!["libnew".to_string()],
            removed: vec!["libold".to_string()],
            upgraded: vec![VersionChange {
                name: "libfoo".to_string(),
                source_version: "1.0".to_string(),
                target_version: "2.0".to_string(),
            }],
            downgraded: vec![VersionChange {
                name: "libbar".to_string(),
                source_version: "3.0".to_string(),
                target_version: "1.0".to_string(),
            }],
            unchanged: vec!["libcommon".to_string()],
        };
        // Exercises all five sections including unchanged.
        print_result(&result, "Require", "f41", "c9s", true);
    }

    #[test]
    fn print_result_show_unchanged_false_hides_unchanged() {
        let result = CompareResult {
            added: vec![],
            removed: vec![],
            upgraded: vec![],
            downgraded: vec![],
            unchanged: vec!["libcommon".to_string()],
        };
        // With show_unchanged=false, unchanged entries are not shown.
        print_result(&result, "Provide", "f41", "rawhide", false);
    }
}
