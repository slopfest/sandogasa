// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use serde::Serialize;

use crate::compare::{self, CompareResult};
use crate::compare_buildrequires;
use crate::compare_provides;
use crate::compare_requires;
use crate::fedrq;

#[derive(Debug, Serialize)]
pub struct BackportResult {
    pub safe: bool,
    pub concerns: Vec<String>,
    pub build_requires: CompareResult,
    pub provides: CompareResult,
    pub requires: CompareResult,
}

/// Evaluate whether a backport is safe given the three comparison results.
pub fn evaluate(
    build_requires: CompareResult,
    provides: CompareResult,
    requires: CompareResult,
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
    if !requires.upgraded.is_empty() {
        concerns.push(format!(
            "{} Require(s) upgraded — {target_branch} may only have older versions",
            requires.upgraded.len()
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

    let safe = concerns.is_empty();

    BackportResult { safe, concerns, build_requires, provides, requires }
}

/// Run all three comparisons and evaluate whether backporting is safe.
///
/// `target_branch` is the branch to backport to (e.g. "c9s").
/// `source_branch` is the branch to take the package from (e.g. "f44").
pub fn safe_to_backport(
    srpm: &str,
    target_branch: &str,
    source_branch: &str,
) -> Result<BackportResult, fedrq::Error> {
    // Compare functions expect (compare_from, compare_to), i.e. (staler, fresher).
    let build_requires =
        compare_buildrequires::compare_buildrequires(srpm, target_branch, source_branch)?;
    let provides = compare_provides::compare_provides(srpm, target_branch, source_branch)?;
    let requires = compare_requires::compare_requires(srpm, target_branch, source_branch)?;

    Ok(evaluate(build_requires, provides, requires, target_branch))
}

/// Print a `BackportResult` in human-readable format.
pub fn print_result(
    result: &BackportResult,
    srpm: &str,
    target_branch: &str,
    source_branch: &str,
) {
    if result.safe {
        println!(
            "Backporting {srpm} from {source_branch} to {target_branch}: SAFE"
        );
    } else {
        println!(
            "Backporting {srpm} from {source_branch} to {target_branch}: NOT SAFE"
        );
        println!();
        println!("Concerns:");
        for c in &result.concerns {
            println!("  - {c}");
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
        println!();
        compare::print_result(cmp, label, target_branch, source_branch);
    }
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
        }
    }

    #[test]
    fn evaluate_all_empty_is_safe() {
        let result = evaluate(empty_result(), empty_result(), empty_result(), "c9s");
        assert!(result.safe);
        assert!(result.concerns.is_empty());
    }

    #[test]
    fn evaluate_buildrequires_added_is_unsafe() {
        let mut br = empty_result();
        br.added = vec!["newdep".to_string()];
        let result = evaluate(br, empty_result(), empty_result(), "c9s");
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
        let result = evaluate(br, empty_result(), empty_result(), "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 1);
        assert!(result.concerns[0].contains("1 BuildRequire(s) upgraded"));
    }

    #[test]
    fn evaluate_requires_added_is_unsafe() {
        let mut req = empty_result();
        req.added = vec!["newlib".to_string()];
        let result = evaluate(empty_result(), empty_result(), req, "c9s");
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
        let result = evaluate(empty_result(), empty_result(), req, "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 1);
        assert!(result.concerns[0].contains("1 Require(s) upgraded"));
    }

    #[test]
    fn evaluate_provides_removed_is_unsafe() {
        let mut prov = empty_result();
        prov.removed = vec!["libold.so".to_string()];
        let result = evaluate(empty_result(), prov, empty_result(), "c9s");
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
        let result = evaluate(empty_result(), prov, empty_result(), "c9s");
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
        let result = evaluate(br, prov, req, "c9s");
        assert!(!result.safe);
        assert_eq!(result.concerns.len(), 3);
    }

    #[test]
    fn evaluate_safe_changes_are_ignored() {
        // removed BuildRequires, downgraded BuildRequires,
        // removed Requires, downgraded Requires,
        // added Provides, upgraded Provides — all fine.
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
        let result = evaluate(br, prov, req, "c9s");
        assert!(result.safe);
        assert!(result.concerns.is_empty());
    }

    #[test]
    fn print_result_safe() {
        let result = BackportResult {
            safe: true,
            concerns: vec![],
            build_requires: empty_result(),
            provides: empty_result(),
            requires: empty_result(),
        };
        // target=c9s, source=rawhide → "Backporting systemd from rawhide to c9s"
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
        };
        // target=c9s, source=rawhide
        print_result(&result, "systemd", "c9s", "rawhide");
    }
}
