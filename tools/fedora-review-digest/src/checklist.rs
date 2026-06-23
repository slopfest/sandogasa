// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The condensed review: a fixed per-generator checklist (each item
//! marked pass / caveat / fail), the verdict, and the post-import
//! boilerplate. Marks are *inferred* from the [`Review`] here and then
//! finalized by the user (see `main`); all rendering is pure.

use crate::review::{Generator, Issue, Review};

/// A checklist verdict for one item — maps to +1 / 0 / -1 at the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// ✅ +1 — criterion met.
    Pass,
    /// 🫤 0 — met with a caveat (needs a justification note).
    Caveat,
    /// ❌ -1 — not met.
    Fail,
}

impl Mark {
    pub fn emoji(self) -> &'static str {
        match self {
            Mark::Pass => "✅",
            Mark::Caveat => "🫤",
            Mark::Fail => "❌",
        }
    }

    /// From a +1 / 0 / -1 vote (the prompt's input).
    pub fn from_vote(v: i32) -> Option<Mark> {
        match v {
            1 => Some(Mark::Pass),
            0 => Some(Mark::Caveat),
            -1 => Some(Mark::Fail),
            _ => None,
        }
    }

    pub fn vote(self) -> i32 {
        match self {
            Mark::Pass => 1,
            Mark::Caveat => 0,
            Mark::Fail => -1,
        }
    }
}

/// One checklist line: a fixed criterion, its mark, and an optional
/// parenthetical note (expected for Caveat/Fail).
#[derive(Debug, Clone)]
pub struct Item {
    pub label: String,
    pub mark: Mark,
    pub note: Option<String>,
}

impl Item {
    fn new(label: &str, mark: Mark, note: Option<&str>) -> Item {
        Item {
            label: label.to_string(),
            mark,
            note: note.map(str::to_string),
        }
    }
}

/// The human name of a generator, for the intro line.
pub fn generator_name(g: Generator) -> &'static str {
    match g {
        Generator::Rust2Rpm => "rust2rpm",
        Generator::Pyp2Spec => "pyp2spec",
        Generator::Unknown => "an automated tool",
    }
}

/// The inferred checklist for a review. `crate_latest` is the latest
/// stable version crates.io reports (evidence for the version item;
/// `None` if not checked / unreachable). Only the Rust (rust2rpm)
/// checklist exists today; pyp2spec gets its own list later.
pub fn infer(review: &Review, crate_latest: Option<&str>) -> Vec<Item> {
    rust_checklist(review, crate_latest)
}

/// The Rust-SIG review criteria, with marks inferred from the review
/// signals. The reviewer finalizes these; the inference just preselects.
fn rust_checklist(r: &Review, crate_latest: Option<&str>) -> Vec<Item> {
    let mut items = vec![
        Item::new(
            "package contains only permissible content",
            Mark::Pass,
            None,
        ),
        builds_item(r),
        tests_item(r),
        version_item(r, crate_latest),
        license_item(r),
    ];
    // A shipped binary statically links its dependencies, so its
    // License: must fold in all of their licenses — an extra check.
    if r.spec.ships_binary {
        items.push(Item::new(
            "licenses of statically linked dependencies are correctly taken into account",
            Mark::Pass,
            None,
        ));
    }
    items.push(license_file_item(r));
    items.push(Item::new(
        "package complies with Rust Packaging Guidelines",
        Mark::Pass,
        None,
    ));
    items
}

/// Builds and installs: a binary RPM landed in `results/`. fedora-review
/// already runs the installability check itself (a failure would surface
/// as an issue we list), so we don't re-test installation here.
fn builds_item(r: &Review) -> Item {
    let label = "package builds and installs without errors on rawhide";
    if r.build_ok {
        Item::new(label, Mark::Pass, None)
    } else {
        Item::new(
            label,
            Mark::Fail,
            Some("no binary RPM in results/ — check build.log"),
        )
    }
}

/// Tests fully run (`%cargo_test`, no `--skip`) → pass; some skipped or
/// the suite disabled → caveat with an editable justification.
fn tests_item(r: &Review) -> Item {
    let label = "test suite is run and all unit tests pass";
    if !r.spec.tests_enabled {
        Item::new(label, Mark::Caveat, Some("disabled — justify"))
    } else if r.spec.tests_skipped {
        Item::new(label, Mark::Caveat, Some("not all tests run — justify"))
    } else {
        Item::new(label, Mark::Pass, None)
    }
}

/// The version item, with crates.io evidence: ✅ + `crates.io: <v>` when
/// the packaged version is the latest stable; 🫤 noting the newer
/// version otherwise; ✅ + "not checked" when crates.io wasn't queried.
fn version_item(r: &Review, latest: Option<&str>) -> Item {
    let label = "latest version of the crate is packaged";
    let packaged = &r.spec.version;
    match latest {
        Some(v) if v == packaged => Item {
            label: label.into(),
            mark: Mark::Pass,
            note: Some(format!("spec & crates.io: {v}")),
        },
        Some(v) => Item {
            label: label.into(),
            mark: Mark::Caveat,
            note: Some(format!("spec: {packaged}; crates.io: {v} — update")),
        },
        None => Item {
            label: label.into(),
            mark: Mark::Pass,
            note: Some(format!("spec: {packaged}; crates.io: not checked")),
        },
    }
}

/// The license item, reporting both the spec `License:` and the crate's
/// `Cargo.toml` `license` as evidence; a mismatch is flagged for the
/// reviewer to reconcile.
fn license_item(r: &Review) -> Item {
    let label = "license matches upstream specification and is acceptable for Fedora";
    let spec = &r.spec.license;
    match &r.cargo_license {
        Some(c) if c == spec => Item {
            label: label.into(),
            mark: Mark::Pass,
            note: Some(format!("spec & Cargo.toml: {spec}")),
        },
        // Cargo's deprecated `/` separator means OR, which rust2rpm
        // rewrites to SPDX — so `MIT/Apache-2.0` and `MIT OR Apache-2.0`
        // are the same license, not a mismatch.
        Some(c) if normalize_license(c) == normalize_license(spec) => Item {
            label: label.into(),
            mark: Mark::Pass,
            note: Some(format!("spec: {spec}; Cargo.toml: {c} (equivalent)")),
        },
        Some(c) => Item {
            label: label.into(),
            mark: Mark::Caveat,
            note: Some(format!("spec: {spec}; Cargo.toml: {c} — reconcile")),
        },
        None => Item {
            label: label.into(),
            mark: Mark::Pass,
            note: Some(format!("spec: {spec}; Cargo.toml: not found")),
        },
    }
}

/// Normalize a license expression for comparison: Cargo's deprecated `/`
/// (meaning OR) becomes ` OR `, and whitespace is collapsed — so a
/// Cargo `MIT/Apache-2.0` compares equal to a spec `MIT OR Apache-2.0`.
fn normalize_license(s: &str) -> String {
    s.replace('/', " OR ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// License file shipped by the crate and `%license`'d → pass; added
/// manually as a Source (upstream's crate lacks it) → caveat, noting an
/// in-flight upstream fix when a PR comment precedes the Source; no
/// `%license` at all → caveat.
fn license_file_item(r: &Review) -> Item {
    let label = "license file is included with %license in %files";
    if !r.spec.has_license_file {
        return Item::new(label, Mark::Caveat, Some("upstream ships no license file"));
    }
    if r.spec.license_manual {
        let note = if r.spec.license_fix_submitted {
            "included manually, fix submitted to upstream"
        } else {
            "included manually"
        };
        Item::new(label, Mark::Caveat, Some(note))
    } else {
        Item::new(label, Mark::Pass, None)
    }
}

/// Whether the package can be approved: no failed item and no
/// outstanding fedora-review MUST issue.
pub fn approved(items: &[Item], issues: &[Issue]) -> bool {
    issues.is_empty() && !items.iter().any(|i| i.mark == Mark::Fail)
}

/// Render the condensed-review block (the part between the first and
/// second `===`): intro line, any MUST issues to address, the marked
/// checklist, and the verdict.
pub fn render_review(generator: Generator, items: &[Item], issues: &[Issue]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "Package was generated with {}, simplifying the review.\n\n",
        generator_name(generator)
    ));

    if !issues.is_empty() {
        out.push_str("Issues to address before approval:\n");
        for it in issues {
            // Prefer the concrete finding (note) over the criterion text,
            // which reads oddly when negated.
            out.push_str(&format!("- {}\n", it.note.as_deref().unwrap_or(&it.text)));
        }
        out.push('\n');
    }

    for it in items {
        out.push_str(it.mark.emoji());
        out.push(' ');
        out.push_str(&it.label);
        if let Some(note) = &it.note {
            out.push_str(&format!(" ({note})"));
        }
        out.push('\n');
    }
    out.push('\n');

    if approved(items, issues) {
        out.push_str("Package APPROVED.\n");
    } else {
        out.push_str("Package not yet approved — please address the above.\n");
    }
    out
}

/// Render the post-import boilerplate (the part below the second `===`).
/// Rust-SIG tasks today; Python gets its own when added.
pub fn render_post_import(generator: Generator, upstream_name: &str) -> String {
    match generator {
        Generator::Pyp2Spec => String::new(), // TODO: python post-import tasks
        _ => rust_post_import(upstream_name),
    }
}

fn rust_post_import(crate_name: &str) -> String {
    format!(
        "Recommended post-import rust-sig tasks:

- set up package on release-monitoring.org:
  project: {crate_name}
  homepage: https://crates.io/crates/{crate_name}
  backend: crates.io
  version scheme: semantic
  version filter (*NOT* pre-release filter): alpha;beta;rc;pre
  distro: Fedora
  Package: rust-{crate_name}

- add @rust-sig with \"commit\" access as package co-maintainer
  (should happen automatically)

- set bugzilla assignee overrides to @rust-sig (optional)

- track package in koschei for all built branches
  (should happen automatically once rust-sig is co-maintainer)
"
    )
}

/// Assemble the full Bugzilla comment from the (optional) free-form
/// reviewer comment, the review block, and the post-import block, joined
/// by `===` separators exactly as the rust-sig convention uses them.
pub fn assemble(comment: Option<&str>, review_block: &str, post_import: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = comment {
        let c = c.trim();
        if !c.is_empty() {
            parts.push(c.to_string());
        }
    }
    parts.push(review_block.trim_end().to_string());
    let post = post_import.trim_end();
    if !post.is_empty() {
        parts.push(post.to_string());
    }
    parts.join("\n\n===\n\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::{Generator, Issue, Review, SpecInfo};

    fn review(
        tests_enabled: bool,
        has_license_file: bool,
        build_ok: bool,
        issues: Vec<Issue>,
    ) -> Review {
        Review {
            spec: SpecInfo {
                generator: Generator::Rust2Rpm,
                name: "rust-foo".to_string(),
                version: "1.0".to_string(),
                license: "MIT".to_string(),
                tests_enabled,
                tests_skipped: false,
                has_license_file,
                license_manual: false,
                license_fix_submitted: false,
                ships_binary: false,
            },
            upstream_name: "foo".to_string(),
            cargo_license: Some("MIT".to_string()),
            build_ok,
            rpmlint_clean: true,
            issues,
        }
    }

    #[test]
    fn clean_review_is_all_pass_and_approved() {
        let r = review(true, true, true, vec![]);
        let items = infer(&r, Some("1.0"));
        assert_eq!(items.len(), 7);
        assert!(items.iter().all(|i| i.mark == Mark::Pass));
        assert!(approved(&items, &r.issues));
        let block = render_review(r.spec.generator, &items, &r.issues);
        assert!(block.contains("generated with rust2rpm"));
        assert!(block.contains("✅ package complies with Rust Packaging Guidelines"));
        // Evidence notes on the version + license items.
        assert!(
            block.contains("✅ latest version of the crate is packaged (spec & crates.io: 1.0)")
        );
        assert!(block.contains("(spec & Cargo.toml: MIT)"));
        assert!(block.trim_end().ends_with("Package APPROVED."));
    }

    #[test]
    fn disabled_tests_and_no_license_become_caveats() {
        let r = review(false, false, true, vec![]);
        let items = infer(&r, Some("1.0"));
        let tests = &items[2];
        assert_eq!(tests.mark, Mark::Caveat);
        assert!(tests.note.is_some());
        let lic = &items[5];
        assert_eq!(lic.mark, Mark::Caveat);
        // Caveats alone still approve.
        assert!(approved(&items, &r.issues));
        let block = render_review(r.spec.generator, &items, &r.issues);
        assert!(
            block.contains("🫤 test suite is run and all unit tests pass (disabled — justify)")
        );
    }

    #[test]
    fn version_behind_crates_io_is_a_caveat() {
        let r = review(true, true, true, vec![]);
        let items = infer(&r, Some("2.0"));
        let ver = &items[3];
        assert_eq!(ver.mark, Mark::Caveat);
        assert_eq!(
            ver.note.as_deref(),
            Some("spec: 1.0; crates.io: 2.0 — update")
        );
        // Not blocking — caveats still approve.
        assert!(approved(&items, &r.issues));
        // No crates.io check → evidence says so.
        let items = infer(&r, None);
        assert_eq!(
            items[3].note.as_deref(),
            Some("spec: 1.0; crates.io: not checked")
        );
    }

    #[test]
    fn skipped_tests_are_a_caveat() {
        let mut r = review(true, true, true, vec![]); // enabled
        r.spec.tests_skipped = true;
        let items = infer(&r, Some("1.0"));
        assert_eq!(items[2].mark, Mark::Caveat);
        assert_eq!(
            items[2].note.as_deref(),
            Some("not all tests run — justify")
        );
    }

    #[test]
    fn manually_included_license_notes_the_fix() {
        let mut r = review(true, true, true, vec![]); // has_license_file
        r.spec.license_manual = true;
        r.spec.license_fix_submitted = true;
        let items = infer(&r, Some("1.0"));
        assert_eq!(items[5].mark, Mark::Caveat);
        assert_eq!(
            items[5].note.as_deref(),
            Some("included manually, fix submitted to upstream")
        );
        // No PR comment → no "fix submitted".
        r.spec.license_fix_submitted = false;
        let items = infer(&r, Some("1.0"));
        assert_eq!(items[5].note.as_deref(), Some("included manually"));
    }

    #[test]
    fn license_mismatch_between_spec_and_cargo_is_a_caveat() {
        let mut r = review(true, true, true, vec![]);
        r.cargo_license = Some("MIT OR Apache-2.0".to_string());
        let items = infer(&r, Some("1.0"));
        let lic = &items[4];
        assert_eq!(lic.mark, Mark::Caveat);
        assert_eq!(
            lic.note.as_deref(),
            Some("spec: MIT; Cargo.toml: MIT OR Apache-2.0 — reconcile")
        );
    }

    #[test]
    fn cargo_slash_license_matches_spdx_or() {
        let mut r = review(true, true, true, vec![]);
        r.spec.license = "MIT OR Apache-2.0".to_string();
        r.cargo_license = Some("MIT/Apache-2.0".to_string()); // deprecated Cargo form
        let items = infer(&r, Some("1.0"));
        let lic = &items[4];
        assert_eq!(lic.mark, Mark::Pass);
        assert_eq!(
            lic.note.as_deref(),
            Some("spec: MIT OR Apache-2.0; Cargo.toml: MIT/Apache-2.0 (equivalent)")
        );
    }

    #[test]
    fn binary_adds_the_static_deps_check() {
        let mut r = review(true, true, true, vec![]);
        r.spec.ships_binary = true;
        let items = infer(&r, Some("1.0"));
        assert_eq!(items.len(), 8);
        let extra = &items[5]; // right after the license-match item
        assert!(
            extra
                .label
                .starts_with("licenses of statically linked dependencies")
        );
        // A library crate has no such item.
        r.spec.ships_binary = false;
        assert_eq!(infer(&r, Some("1.0")).len(), 7);
    }

    #[test]
    fn build_failure_fails_and_blocks_approval() {
        let r = review(true, true, false, vec![]);
        let items = infer(&r, Some("1.0"));
        assert_eq!(items[1].mark, Mark::Fail);
        assert!(!approved(&items, &r.issues));
        let block = render_review(r.spec.generator, &items, &r.issues);
        assert!(block.contains("Package not yet approved"));
    }

    #[test]
    fn must_issues_block_approval_and_are_listed() {
        let issue = Issue {
            name: "CheckFileDuplicates".to_string(),
            text: "Package does not contain duplicates in %files.".to_string(),
            note: Some("warning: File listed twice: /usr/.../LICENSE.txt".to_string()),
        };
        let r = review(true, true, true, vec![issue]);
        let items = infer(&r, Some("1.0"));
        assert!(!approved(&items, &r.issues));
        let block = render_review(r.spec.generator, &items, &r.issues);
        assert!(block.contains("Issues to address before approval:"));
        // The concrete finding (note), not the criterion text.
        assert!(block.contains("- warning: File listed twice"));
    }

    #[test]
    fn post_import_substitutes_the_crate_name() {
        let p = render_post_import(Generator::Rust2Rpm, "trustfall_core");
        assert!(p.contains("project: trustfall_core"));
        assert!(p.contains("Package: rust-trustfall_core"));
        assert!(p.contains("homepage: https://crates.io/crates/trustfall_core"));
    }

    #[test]
    fn assemble_joins_blocks_with_separators() {
        let out = assemble(Some("Looks good!"), "REVIEW", "POSTIMPORT");
        assert_eq!(out, "Looks good!\n\n===\n\nREVIEW\n\n===\n\nPOSTIMPORT\n");
        // No comment → no leading separator.
        let out = assemble(None, "REVIEW", "POSTIMPORT");
        assert_eq!(out, "REVIEW\n\n===\n\nPOSTIMPORT\n");
        // Empty comment treated as none.
        let out = assemble(Some("   "), "REVIEW", "POSTIMPORT");
        assert_eq!(out, "REVIEW\n\n===\n\nPOSTIMPORT\n");
    }
}
