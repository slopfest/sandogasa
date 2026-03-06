// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::{BTreeMap, BTreeSet};

use crate::fedrq::Fedrq;

/// A Provide whose version changed between branches.
#[derive(Debug, PartialEq, Eq)]
pub struct Upgraded {
    pub name: String,
    pub source_version: String,
    pub target_version: String,
}

/// Result of comparing provides between two branches.
#[derive(Debug)]
pub struct CompareResult {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub upgraded: Vec<Upgraded>,
}

/// Split a provide string into (name, version) on the first ` = `.
/// Returns `None` for the version if there is no ` = `.
fn split_provide(provide: &str) -> (&str, Option<&str>) {
    match provide.split_once(" = ") {
        Some((name, version)) => (name, Some(version)),
        None => (provide, None),
    }
}

/// Compare the Provides of a source package between two branches.
pub fn compare_provides(
    srpm: &str,
    source_branch: &str,
    target_branch: &str,
) -> Result<CompareResult, crate::fedrq::Error> {
    let source_fq = Fedrq {
        branch: Some(source_branch.to_string()),
        ..Default::default()
    };
    let target_fq = Fedrq {
        branch: Some(target_branch.to_string()),
        ..Default::default()
    };

    let source_provides: BTreeSet<String> = source_fq.subpkgs_provides(srpm)?.into_iter().collect();
    let target_provides: BTreeSet<String> = target_fq.subpkgs_provides(srpm)?.into_iter().collect();

    let raw_added: BTreeSet<&String> = target_provides.difference(&source_provides).collect();
    let raw_removed: BTreeSet<&String> = source_provides.difference(&target_provides).collect();

    // Index removed entries by their name (LHS of ` = `) to find upgrades.
    let mut removed_by_name: BTreeMap<&str, &str> = BTreeMap::new();
    for r in &raw_removed {
        let (name, version) = split_provide(r);
        if let Some(v) = version {
            removed_by_name.insert(name, v);
        }
    }

    let mut added = Vec::new();
    let mut upgraded = Vec::new();
    let mut upgraded_names: BTreeSet<&str> = BTreeSet::new();

    for a in &raw_added {
        let (name, version) = split_provide(a);
        match (version, removed_by_name.get(name)) {
            (Some(target_v), Some(source_v)) => {
                upgraded.push(Upgraded {
                    name: name.to_string(),
                    source_version: source_v.to_string(),
                    target_version: target_v.to_string(),
                });
                upgraded_names.insert(name);
            }
            _ => added.push(a.to_string()),
        }
    }

    let removed: Vec<String> = raw_removed
        .iter()
        .filter(|r| {
            let (name, _) = split_provide(r);
            !upgraded_names.contains(name)
        })
        .map(|r| r.to_string())
        .collect();

    Ok(CompareResult { added, removed, upgraded })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_provide_with_version() {
        let (name, version) = split_provide("libbpf(aarch-64) = 2:1.5.0-3.el9");
        assert_eq!(name, "libbpf(aarch-64)");
        assert_eq!(version, Some("2:1.5.0-3.el9"));
    }

    #[test]
    fn split_provide_without_version() {
        let (name, version) = split_provide("/usr/sbin/halt");
        assert_eq!(name, "/usr/sbin/halt");
        assert_eq!(version, None);
    }

    #[test]
    fn split_provide_preserves_extra_equals() {
        let (name, version) = split_provide("foo = 1 = 2");
        assert_eq!(name, "foo");
        assert_eq!(version, Some("1 = 2"));
    }

    #[test]
    fn compare_result_empty() {
        let result = CompareResult {
            added: vec![],
            removed: vec![],
            upgraded: vec![],
        };
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.upgraded.is_empty());
    }

    #[test]
    fn compare_result_with_upgrade() {
        let result = CompareResult {
            added: vec![],
            removed: vec![],
            upgraded: vec![Upgraded {
                name: "libbpf(aarch-64)".to_string(),
                source_version: "2:1.5.0-3.el9".to_string(),
                target_version: "2:1.6.3-1.fc44".to_string(),
            }],
        };
        assert_eq!(result.upgraded.len(), 1);
        assert_eq!(result.upgraded[0].name, "libbpf(aarch-64)");
    }
}
