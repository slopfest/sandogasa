// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Adjust a package's `debian/salsa-ci.yml` when creating a new rebuild
//! branch. dbranch injects a `RELEASE` (the suite salsa-ci builds
//! against) into the inherited `variables:` block, preserving the file's
//! other entries and comments. An Ubuntu PPA branch also gets a set of
//! backports-style relaxations; a Debian proposed-update branch builds
//! straight against the stable suite, so it gets only `RELEASE`.

/// The backports-style relaxations appended after the existing
/// variables (with their explanatory comment), for a downstream Ubuntu
/// PPA rebuild where the upstream Debian checks don't all apply. A
/// Debian proposed-update does **not** get these — it's a real stable
/// build and should face the normal checks.
const BACKPORTS_BLOCK: &[&str] = &[
    "# adjust for backports",
    r#"SALSA_CI_LINTIAN_SUPPRESS_TAGS: "bad-distribution-in-changes-file,newer-standards-version""#,
    r#"SALSA_CI_DISABLE_VERSION_BUMP: "true""#,
    "SALSA_CI_DISABLE_PIUPARTS: 1",
];

/// Inject the rebuild preset into a salsa-ci.yml's `variables:` block,
/// preserving existing entries and comments. `release` is the `RELEASE`
/// value to add (the Debian suite salsa-ci builds against — `unstable`
/// for a PPA, the codename for a proposed-update); `add_backports`
/// appends [`BACKPORTS_BLOCK`] (PPA only). Idempotent: a key already
/// present is not added again — in particular an existing `RELEASE`
/// (e.g. pinned to an older Debian suite) is left untouched. Returns the
/// new text, or `None` if the file has no `variables:` block to extend
/// (the caller warns and leaves it alone).
pub fn adjust_salsa_ci(text: &str, release: &str, add_backports: bool) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let var_idx = lines.iter().position(|l| l.trim() == "variables:")?;

    // Find the block's extent and its entries' indentation. A line
    // belongs to the block while it is blank or indented under
    // `variables:`; the first top-level line ends it.
    let mut indent = "  ".to_string();
    let mut block_end = var_idx;
    for (i, line) in lines.iter().enumerate().skip(var_idx + 1) {
        if line.trim().is_empty() {
            continue;
        }
        let line_indent = &line[..line.len() - line.trim_start().len()];
        if line_indent.is_empty() {
            break;
        }
        if block_end == var_idx {
            indent = line_indent.to_string();
        }
        block_end = i;
    }

    let block = &lines[var_idx + 1..=block_end];
    let has_release = block.iter().any(|l| l.trim_start().starts_with("RELEASE:"));
    let has_backports = block
        .iter()
        .any(|l| l.trim_start().starts_with("SALSA_CI_DISABLE_PIUPARTS"));
    // Backports only matter when requested; otherwise treat as satisfied.
    let backports_done = !add_backports || has_backports;
    if has_release && backports_done {
        return Some(text.to_string());
    }

    let release_line = format!(r#"RELEASE: "{release}""#);
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + BACKPORTS_BLOCK.len() + 1);
    for (i, line) in lines.iter().enumerate() {
        out.push((*line).to_string());
        if i == var_idx && !has_release {
            out.push(format!("{indent}{release_line}"));
        }
        if i == block_end && add_backports && !has_backports {
            for entry in BACKPORTS_BLOCK {
                out.push(format!("{indent}{entry}"));
            }
        }
    }
    let mut result = out.join("\n");
    if text.ends_with('\n') {
        result.push('\n');
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "\
---
include:
  - https://salsa.debian.org/salsa-ci-team/pipeline/raw/master/salsa-ci.yml
  - https://salsa.debian.org/salsa-ci-team/pipeline/raw/master/pipeline-jobs.yml
variables:
  # no binary package
  SALSA_CI_DISABLE_BUILD_PACKAGE_ANY: '1'
  SALSA_CI_DISABLE_CROSSBUILD_ARM64: '1'
";

    const ADJUSTED: &str = "\
---
include:
  - https://salsa.debian.org/salsa-ci-team/pipeline/raw/master/salsa-ci.yml
  - https://salsa.debian.org/salsa-ci-team/pipeline/raw/master/pipeline-jobs.yml
variables:
  RELEASE: \"unstable\"
  # no binary package
  SALSA_CI_DISABLE_BUILD_PACKAGE_ANY: '1'
  SALSA_CI_DISABLE_CROSSBUILD_ARM64: '1'
  # adjust for backports
  SALSA_CI_LINTIAN_SUPPRESS_TAGS: \"bad-distribution-in-changes-file,newer-standards-version\"
  SALSA_CI_DISABLE_VERSION_BUMP: \"true\"
  SALSA_CI_DISABLE_PIUPARTS: 1
";

    /// A Debian proposed-update: RELEASE = the codename, no backports.
    const PROPOSED: &str = "\
---
include:
  - https://salsa.debian.org/salsa-ci-team/pipeline/raw/master/salsa-ci.yml
  - https://salsa.debian.org/salsa-ci-team/pipeline/raw/master/pipeline-jobs.yml
variables:
  RELEASE: \"trixie\"
  # no binary package
  SALSA_CI_DISABLE_BUILD_PACKAGE_ANY: '1'
  SALSA_CI_DISABLE_CROSSBUILD_ARM64: '1'
";

    #[test]
    fn adjusts_base_to_expected_preset() {
        assert_eq!(
            adjust_salsa_ci(BASE, "unstable", true).as_deref(),
            Some(ADJUSTED)
        );
    }

    #[test]
    fn proposed_update_sets_codename_release_and_no_backports() {
        // A proposed-update gets RELEASE=<codename> only — no backports
        // relaxations (it's a real stable build).
        assert_eq!(
            adjust_salsa_ci(BASE, "trixie", false).as_deref(),
            Some(PROPOSED)
        );
        let out = adjust_salsa_ci(BASE, "trixie", false).unwrap();
        assert!(!out.contains("SALSA_CI_DISABLE_PIUPARTS"));
        assert!(!out.contains("adjust for backports"));
    }

    #[test]
    fn is_idempotent() {
        // Re-running over an already-adjusted file changes nothing.
        assert_eq!(
            adjust_salsa_ci(ADJUSTED, "unstable", true).as_deref(),
            Some(ADJUSTED)
        );
        assert_eq!(
            adjust_salsa_ci(PROPOSED, "trixie", false).as_deref(),
            Some(PROPOSED)
        );
    }

    #[test]
    fn none_without_variables_block() {
        assert_eq!(
            adjust_salsa_ci("---\ninclude:\n  - x\n", "unstable", true),
            None
        );
    }

    #[test]
    fn preserves_an_existing_release() {
        // A maintainer may pin RELEASE to an older Debian suite for an
        // old Ubuntu LTS; don't overwrite it (but still add backports).
        let text = "\
variables:
  RELEASE: \"bookworm\"
  SALSA_CI_DISABLE_BUILD_PACKAGE_ANY: '1'
";
        let out = adjust_salsa_ci(text, "unstable", true).unwrap();
        assert!(out.contains("RELEASE: \"bookworm\""));
        assert!(!out.contains("RELEASE: \"unstable\""));
        // The backports relaxations are still appended.
        assert!(out.contains("SALSA_CI_DISABLE_PIUPARTS: 1"));
    }

    #[test]
    fn handles_empty_variables_block() {
        let out = adjust_salsa_ci("variables:\n", "unstable", true).unwrap();
        assert!(out.contains("  RELEASE: \"unstable\""));
        assert!(out.contains("  SALSA_CI_DISABLE_PIUPARTS: 1"));
    }
}
