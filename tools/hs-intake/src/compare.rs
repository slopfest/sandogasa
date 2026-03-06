// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

/// An entry whose version changed between branches.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Upgraded {
    pub name: String,
    pub source_version: String,
    pub target_version: String,
}

/// Result of comparing entries between two branches.
#[derive(Debug, Serialize)]
pub struct CompareResult {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub upgraded: Vec<Upgraded>,
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
    let close = entry[open..].find(')')? + open;
    let content = &entry[open + 1..close];
    if content.is_empty() {
        return None;
    }
    let name = format!("{}(){}", &entry[..open], &entry[close + 1..]);
    Some((name, content))
}

/// Diff two sets of RPM dependency/provide strings, detecting upgrades
/// where the name matches but the version differs.
pub fn diff(source: Vec<String>, target: Vec<String>) -> CompareResult {
    let source_set: BTreeSet<String> = source.into_iter().collect();
    let target_set: BTreeSet<String> = target.into_iter().collect();

    let raw_added: BTreeSet<&String> = target_set.difference(&source_set).collect();
    let raw_removed: BTreeSet<&String> = source_set.difference(&target_set).collect();

    // Index removed entries by their name (LHS of operator) to find upgrades.
    let mut removed_by_name: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for r in &raw_removed {
        let (name, op, version) = split_entry(r);
        if let (Some(op), Some(v)) = (op, version) {
            removed_by_name.insert(name, (op, v));
        }
    }

    let mut added = Vec::new();
    let mut upgraded = Vec::new();
    let mut upgraded_names: BTreeSet<&str> = BTreeSet::new();

    for a in &raw_added {
        let (name, op, version) = split_entry(a);
        match (op, version, removed_by_name.get(name)) {
            (Some(target_op), Some(target_v), Some(&(source_op, source_v)))
                if target_op == source_op =>
            {
                let prefix = if target_op == "=" { "" } else { ">= " };
                upgraded.push(Upgraded {
                    name: name.to_string(),
                    source_version: format!("{prefix}{source_v}"),
                    target_version: format!("{prefix}{target_v}"),
                });
                upgraded_names.insert(name);
            }
            _ => added.push(a.to_string()),
        }
    }

    let remaining_removed: Vec<String> = raw_removed
        .iter()
        .filter(|r| {
            let (name, _, _) = split_entry(r);
            !upgraded_names.contains(name)
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

    let mut solib_upgraded_originals: BTreeSet<String> = BTreeSet::new();
    let mut final_added = Vec::new();
    for a in &added {
        if let Some((pattern, target_v)) = split_solib(a) {
            if let Some((original, source_v)) = solib_removed_by_pattern.get(&pattern) {
                upgraded.push(Upgraded {
                    name: pattern,
                    source_version: source_v.to_string(),
                    target_version: target_v.to_string(),
                });
                solib_upgraded_originals.insert(original.clone());
                continue;
            }
        }
        final_added.push(a.clone());
    }

    let removed: Vec<String> = remaining_removed
        .into_iter()
        .filter(|r| !solib_upgraded_originals.contains(r))
        .collect();

    upgraded.sort_by(|a, b| a.name.cmp(&b.name));

    CompareResult { added: final_added, removed, upgraded }
}

/// Print a `CompareResult` in human-readable format.
pub fn print_result(
    result: &CompareResult,
    label: &str,
    source_branch: &str,
    target_branch: &str,
) {
    if result.added.is_empty() && result.removed.is_empty() && result.upgraded.is_empty() {
        println!("No differences in {label}.");
        return;
    }
    let mut need_blank = false;
    if !result.upgraded.is_empty() {
        let name_w = result.upgraded.iter().map(|u| u.name.len()).max().unwrap();
        let src_w = result
            .upgraded
            .iter()
            .map(|u| u.source_version.len())
            .max()
            .unwrap()
            .max(source_branch.len());
        let tgt_w = result
            .upgraded
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
        println!("Upgraded ({source_branch} -> {target_branch}):");
        println!("{sep}");
        println!(
            "| {:<name_w$} | {:<src_w$} | {:<tgt_w$} |",
            label, source_branch, target_branch
        );
        println!("{sep}");
        for u in &result.upgraded {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn diff_added_only() {
        let result = diff(vec![], vec!["foo".to_string()]);
        assert_eq!(result.added, vec!["foo"]);
        assert!(result.removed.is_empty());
        assert!(result.upgraded.is_empty());
    }

    #[test]
    fn diff_removed_only() {
        let result = diff(vec!["foo".to_string()], vec![]);
        assert!(result.added.is_empty());
        assert_eq!(result.removed, vec!["foo"]);
        assert!(result.upgraded.is_empty());
    }

    #[test]
    fn diff_detects_upgrade() {
        let source = vec!["libbpf = 1.0".to_string()];
        let target = vec!["libbpf = 2.0".to_string()];
        let result = diff(source, target);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert_eq!(result.upgraded.len(), 1);
        assert_eq!(result.upgraded[0].name, "libbpf");
        assert_eq!(result.upgraded[0].source_version, "1.0");
        assert_eq!(result.upgraded[0].target_version, "2.0");
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
    }

    #[test]
    fn diff_detects_gte_upgrade() {
        let source = vec!["kernel-headers >= 5.14.0-647".to_string()];
        let target = vec!["kernel-headers >= 5.16.0".to_string()];
        let result = diff(source, target);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert_eq!(result.upgraded.len(), 1);
        assert_eq!(result.upgraded[0].name, "kernel-headers");
        assert_eq!(result.upgraded[0].source_version, ">= 5.14.0-647");
        assert_eq!(result.upgraded[0].target_version, ">= 5.16.0");
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
    fn diff_detects_solib_upgrade() {
        let source = vec!["libc.so.6(GLIBC_2.28)(64bit)".to_string()];
        let target = vec!["libc.so.6(GLIBC_2.38)(64bit)".to_string()];
        let result = diff(source, target);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert_eq!(result.upgraded.len(), 1);
        assert_eq!(result.upgraded[0].name, "libc.so.6()(64bit)");
        assert_eq!(result.upgraded[0].source_version, "GLIBC_2.28");
        assert_eq!(result.upgraded[0].target_version, "GLIBC_2.38");
    }

    #[test]
    fn diff_no_changes() {
        let entries = vec!["foo = 1.0".to_string(), "bar".to_string()];
        let result = diff(entries.clone(), entries);
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.upgraded.is_empty());
    }
}
