// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The condensed review: a fixed per-generator checklist (each item
//! marked pass / caveat / fail), the verdict, and the post-import
//! boilerplate. Marks are *inferred* from the [`Review`] here and then
//! finalized by the user (see `main`); all rendering is pure.

use crate::review::{Generator, Issue, LicenseFile, Review};
pub use sandogasa_review::Resolution;

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
        Generator::CabalRpm => "cabal-rpm",
        Generator::Pyp2Spec => "pyp2spec",
        Generator::Unknown => "an automated tool",
    }
}

/// The inferred checklist for a review. `latest` is the latest version
/// the registry reports — crates.io for rust2rpm, Hackage for
/// cabal-rpm — as evidence for the version item (`None` if not checked
/// / unreachable). A Python spec has its own list ([`python_checklist`]).
pub fn infer(review: &Review, latest: Option<&str>) -> Vec<Item> {
    match review.spec.generator {
        Generator::CabalRpm => haskell_checklist(review, latest),
        _ => rust_checklist(review, latest),
    }
}

/// The Haskell review criteria — the Rust list with Hackage and the
/// `.cabal` file as the evidence, no static-deps item (Haskell
/// libraries link statically by design and the guidelines do not fold
/// their licenses), and a test suite upstream never wrote noted as
/// such rather than as a caveat.
fn haskell_checklist(r: &Review, latest: Option<&str>) -> Vec<Item> {
    vec![
        Item::new(
            "package contains only permissible content",
            Mark::Pass,
            None,
        ),
        builds_item(r),
        tests_item(r),
        version_item(&r.spec.version, latest, "package", "Hackage"),
        license_item(r, ".cabal"),
        license_file_item(r),
        Item::new(
            "package complies with Haskell Packaging Guidelines",
            Mark::Pass,
            None,
        ),
    ]
}

/// What a Python checklist judges by, besides the review: from PyPI and
/// pyp2spec's output.
pub struct PythonEvidence<'a> {
    /// The packaged version in PyPI's form (`2.0rc1` for `2.0~rc1`).
    pub version: &'a str,
    /// PyPI's latest version, when it was looked up.
    pub latest: Option<&'a str>,
    /// The license PyPI's metadata gives, as pyp2spec writes it.
    pub upstream_license: Option<&'a str>,
    /// Why the spec was not compared with pyp2spec's output, if it was
    /// not: the review then rests on the reviewer reading it.
    pub not_compared: Option<&'a str>,
}

/// The Python review criteria (#161): the build, the tests, the version
/// on PyPI, the license against PyPI's metadata, the license files the
/// built package ships, and the pyproject macros pyp2spec's template
/// uses for the file list and the BuildRequires.
pub fn python_checklist(r: &Review, e: &PythonEvidence<'_>) -> Vec<Item> {
    let upstream = Review {
        upstream_license: e.upstream_license.map(str::to_string),
        ..r.clone()
    };
    let declarative = pyproject_buildsystem(&r.spec_text);
    let compared = match e.not_compared {
        None => Item::new("spec compared with pyp2spec's output", Mark::Pass, None),
        // Nothing else in the list stands in for the comparison, so it
        // blocks approval until the reviewer has read the spec.
        Some(why) => Item {
            label: "spec compared with pyp2spec's output".into(),
            mark: Mark::Fail,
            note: Some(format!("not compared: {why} — read the spec, then mark it")),
        },
    };
    vec![
        Item::new(
            "package contains only permissible content",
            Mark::Pass,
            None,
        ),
        compared,
        builds_item(r),
        python_tests_item(&r.spec_text, r.spec.tests_enabled),
        python_version_item(e.version, e.latest),
        match e.upstream_license {
            Some(_) => license_item(&upstream, "PyPI"),
            None => Item::new(
                "license matches upstream specification and is acceptable for Fedora",
                Mark::Caveat,
                Some("PyPI's metadata not checked — compare by hand"),
            ),
        },
        python_license_file_item(&r.spec_text, r.license_files.as_deref()),
        // The declarative BuildSystem: pyproject calls both macros itself.
        macro_item(
            "files are listed with %pyproject_save_files",
            (uses(&r.spec_text, "%pyproject_save_files") || declarative)
                && (uses(&r.spec_text, "%{pyproject_files}")
                    || uses(&r.spec_text, "%pyproject_files")),
            "listed by hand",
        ),
        macro_item(
            "BuildRequires are generated with %pyproject_buildrequires",
            uses(&r.spec_text, "%pyproject_buildrequires") || declarative,
            "listed by hand",
        ),
        Item::new(
            "package complies with Python Packaging Guidelines",
            Mark::Pass,
            None,
        ),
    ]
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
        version_item(&r.spec.version, crate_latest, "crate", "crates.io"),
        license_item(r, "Cargo.toml"),
    ];
    // A shipped binary statically links its dependencies, so its
    // License: must fold in all of their licenses — an extra check.
    if r.spec.ships_binary {
        items.push(static_deps_item(r));
    }
    items.push(license_file_item(r));
    items.push(Item::new(
        "package complies with Rust Packaging Guidelines",
        Mark::Pass,
        None,
    ));
    items
}

/// Whether the tests run: a line of `%check` that runs a test runner
/// passes, an import check alone is a caveat.
fn python_tests_item(spec: &str, enabled: bool) -> Item {
    let label = "test suite is run and all unit tests pass";
    if !enabled {
        return Item::new(label, Mark::Caveat, Some("disabled — justify"));
    }
    let check = section(spec, "%check");
    let first = |l: &str| l.split_whitespace().next().unwrap_or("").to_string();
    let runs = check.lines().any(|l| {
        let word = first(l);
        [
            "%pytest",
            "%{pytest}",
            "%tox",
            "%{tox}",
            "pytest",
            "py.test",
        ]
        .contains(&word.as_str())
            || l.contains("-m pytest")
            || l.contains("-m unittest")
    });
    let imports = check.lines().any(|l| {
        let word = first(l);
        ["%pyproject_check_import", "%py3_check_import"].contains(&word.as_str())
    });
    if runs {
        Item::new(label, Mark::Pass, None)
    } else if imports {
        Item::new(
            label,
            Mark::Caveat,
            Some("import check only — run the tests or justify"),
        )
    } else {
        Item::new(label, Mark::Caveat, Some("no tests run — justify"))
    }
}

/// Whether a line of `spec` that is not a comment contains `needle`.
fn uses(spec: &str, needle: &str) -> bool {
    spec.lines()
        .any(|l| !l.trim_start().starts_with('#') && l.contains(needle))
}

/// The license files, judged against how `%pyproject_save_files` was
/// asked to treat them. The macro adds every license file the wheel
/// declares (PEP 639 `License-File`) as `%license` in its dist-info
/// directory, whatever its flag; `-l` only fails the build when there is
/// none. A license file also listed by hand in the same package is then
/// shipped twice, which rpmbuild does not flag, as the copies have
/// different paths; another subpackage listing its own copy is fine.
/// `license_files` are the `%license` files in the built RPMs (`None`
/// when there are none to read).
fn python_license_file_item(spec: &str, license_files: Option<&[LicenseFile]>) -> Item {
    let label = "license file is included with %license in %files";
    let assert_license = save_files_flags(spec).contains(&"-l");
    let Some(files) = license_files else {
        return if assert_license {
            Item::new(label, Mark::Pass, Some("%pyproject_save_files -l"))
        } else {
            Item::new(label, Mark::Pass, Some("built package not checked"))
        };
    };
    let base = |f: &LicenseFile| f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
    let (declared, by_hand): (Vec<&LicenseFile>, Vec<&LicenseFile>) =
        files.iter().partition(|f| f.path.contains(".dist-info/"));
    let mut twice: Vec<String> = by_hand
        .iter()
        .filter(|h| {
            declared
                .iter()
                .any(|d| d.package == h.package && base(d) == base(h))
        })
        .map(|h| base(h))
        .collect();
    twice.sort();
    twice.dedup();
    if !twice.is_empty() {
        let fix = if assert_license {
            "drop the %license line"
        } else {
            "use %pyproject_save_files -l and drop the %license line"
        };
        return Item {
            label: label.into(),
            mark: Mark::Caveat,
            note: Some(format!(
                "{} shipped twice, by hand and from the wheel — {fix}",
                twice.join(", ")
            )),
        };
    }
    match (declared.is_empty(), by_hand.is_empty()) {
        (true, true) => Item::new(label, Mark::Fail, Some("no license file shipped")),
        (false, _) if !assert_license => Item::new(
            label,
            Mark::Pass,
            Some("from the wheel; %pyproject_save_files -l would keep it so"),
        ),
        (false, _) => Item::new(label, Mark::Pass, None),
        (true, false) => Item::new(
            label,
            Mark::Pass,
            Some("listed by hand; the wheel declares none"),
        ),
    }
}

/// The options `%pyproject_save_files` is called with in `spec`, or
/// that `BuildOption(install):` passes it under the declarative
/// `BuildSystem: pyproject`.
fn save_files_flags(spec: &str) -> Vec<&str> {
    spec.lines()
        .filter_map(|l| {
            let t = l.trim();
            t.strip_prefix("%pyproject_save_files").or_else(|| {
                t.strip_prefix("BuildOption(install):")
                    .filter(|_| pyproject_buildsystem(spec))
            })
        })
        .flat_map(str::split_whitespace)
        .filter(|w| w.starts_with('-'))
        .collect()
}

/// The version item for PyPI: the versions compared after PEP 440
/// normalization, and a packaged pre-release newer than PyPI's latest
/// stable release (what `info.version` gives) is not an update to make;
/// one older than it is.
fn python_version_item(packaged: &str, latest: Option<&str>) -> Item {
    match latest {
        Some(l) if pep440(l) == pep440(packaged) => {
            version_item(packaged, Some(packaged), "package", "PyPI")
        }
        Some(l) if is_pre_release(packaged) && release_of(packaged) > release_of(l) => Item {
            label: "latest version of the package is packaged".into(),
            mark: Mark::Pass,
            note: Some(format!(
                "spec: {packaged}, a pre-release; PyPI's latest stable: {l}"
            )),
        },
        _ => version_item(packaged, latest, "package", "PyPI"),
    }
}

/// `version` in a PEP 440 normal form, enough to compare two spellings:
/// lowercase, `alpha`/`beta`/`c`/`pre`/`preview` as `a`/`b`/`rc`, an
/// implicit post-release (`1.0-1`) as `post`, and no separator before a
/// letter (`1.0.dev3` and `1.0dev3` are the same).
fn pep440(version: &str) -> String {
    let v = version
        .to_ascii_lowercase()
        .replace("alpha", "a")
        .replace("beta", "b")
        .replace("preview", "rc")
        .replace("pre", "rc");
    let chars: Vec<char> = v.chars().collect();
    let digit = |i: usize| chars.get(i).is_some_and(|c| c.is_ascii_digit());
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        let before_letter = chars.get(i + 1).is_some_and(|n| n.is_ascii_alphabetic());
        if matches!(c, '.' | '-' | '_') && before_letter {
            continue;
        }
        // An implicit post-release: `1.0-1` is `1.0.post1`.
        if *c == '-' && digit(i + 1) {
            out.push_str("post");
            continue;
        }
        // `c` alone is `rc`: `1.0c1`.
        if *c == 'c' && (i == 0 || chars[i - 1] != 'r') && digit(i + 1) {
            out.push_str("rc");
            continue;
        }
        out.push(*c);
    }
    out
}

/// The release segment of `version`, the numbers before any letter, to
/// order two versions by, without trailing zeros, so `2` and `2.0` are
/// equal: `2.0rc1` gives `[2]`, `2.1rc1` `[2, 1]`.
fn release_of(version: &str) -> Vec<u64> {
    let mut release: Vec<u64> = pep440(version)
        .split(|c: char| c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .split('.')
        .filter_map(|n| n.parse().ok())
        .collect();
    while release.last() == Some(&0) {
        release.pop();
    }
    release
}

/// Whether `version` is a PEP 440 pre-release or development release.
fn is_pre_release(version: &str) -> bool {
    let v = pep440(version);
    ["a", "b", "rc", "dev"].iter().any(|t| {
        v.match_indices(t).any(|(at, _)| {
            v[..at].ends_with(|c: char| c.is_ascii_digit())
                && v[at + t.len()..].starts_with(|c: char| c.is_ascii_digit())
        })
    })
}

/// Whether `spec` uses the declarative `BuildSystem: pyproject`, which
/// generates the BuildRequires itself.
fn pyproject_buildsystem(spec: &str) -> bool {
    spec.lines().any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("buildsystem") && v.trim() == "pyproject"
        })
    })
}

/// An item that passes when pyp2spec's macro is used, else a caveat
/// with `otherwise`.
fn macro_item(label: &str, used: bool, otherwise: &str) -> Item {
    if used {
        Item::new(label, Mark::Pass, None)
    } else {
        Item::new(label, Mark::Caveat, Some(otherwise))
    }
}

/// The body of the spec section starting with `header` (`%check`), up to
/// the next section.
fn section(spec: &str, header: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in spec.lines() {
        let t = line.trim();
        if t.starts_with('%') && is_section_header(t) {
            inside = t.split_whitespace().next() == Some(header);
            continue;
        }
        if inside && !t.starts_with('#') {
            out.push_str(t);
            out.push('\n');
        }
    }
    out
}

/// Whether `line` starts a spec section, a scriptlet or a trigger.
fn is_section_header(line: &str) -> bool {
    const SECTIONS: [&str; 15] = [
        "%description",
        "%package",
        "%prep",
        "%generate_buildrequires",
        "%conf",
        "%build",
        "%install",
        "%check",
        "%files",
        "%changelog",
        "%verifyscript",
        "%sourcelist",
        "%patchlist",
        "%clean",
        "%posttrans",
    ];
    let word = line.split_whitespace().next().unwrap_or("");
    SECTIONS.contains(&word)
        || [
            "%pre",
            "%post",
            "%trigger",
            "%filetrigger",
            "%transfiletrigger",
        ]
        .iter()
        .any(|p| word.starts_with(p))
}

/// Builds and installs. The build is ok when a binary RPM landed in
/// `results/`; installation is fedora-review's own check, which we read
/// from its issues (a `CheckPackageInstalls` failure) rather than
/// re-testing. Either failing → Fail.
fn builds_item(r: &Review) -> Item {
    let label = "package builds and installs without errors on rawhide";
    if !r.build_ok {
        Item::new(
            label,
            Mark::Fail,
            Some("no binary RPM in results/ — check build.log"),
        )
    } else if install_failed(r) {
        Item::new(
            label,
            Mark::Fail,
            Some("installation failed — see fedora-review attachment"),
        )
    } else {
        Item::new(label, Mark::Pass, None)
    }
}

/// Whether fedora-review's installability check (`CheckPackageInstalls`)
/// failed — surfaced as an issue in the review.
fn install_failed(r: &Review) -> bool {
    r.issues.iter().any(|i| i.name == "CheckPackageInstalls")
}

/// Tests fully run (`%cargo_test`, no `--skip`) → pass; some skipped or
/// the suite disabled → caveat with an editable justification. When
/// upstream's manifest shows there is no test suite to run, the caveat
/// comes with that as its note: nothing to run is still not a pass.
fn tests_item(r: &Review) -> Item {
    let label = "test suite is run and all unit tests pass";
    if !r.spec.tests_enabled {
        if r.upstream_has_tests == Some(false) {
            return Item::new(label, Mark::Caveat, Some("upstream ships no test suite"));
        }
        Item::new(label, Mark::Caveat, Some("disabled — justify"))
    } else if r.spec.tests_skipped {
        Item::new(label, Mark::Caveat, Some("not all tests run — justify"))
    } else {
        Item::new(label, Mark::Pass, None)
    }
}

/// The version item, with the registry's evidence: ✅ + `<registry>:
/// <v>` when the packaged version is the latest; 🫤 noting the newer
/// version otherwise; ✅ + "not checked" when the registry wasn't
/// queried.
fn version_item(packaged: &str, latest: Option<&str>, noun: &str, registry: &str) -> Item {
    let label = format!("latest version of the {noun} is packaged");
    match latest {
        Some(v) if v == packaged => Item {
            label,
            mark: Mark::Pass,
            note: Some(format!("spec & {registry}: {v}")),
        },
        Some(v) => Item {
            label,
            mark: Mark::Caveat,
            note: Some(format!("spec: {packaged}; {registry}: {v} — update")),
        },
        None => Item {
            label,
            mark: Mark::Pass,
            note: Some(format!("spec: {packaged}; {registry}: not checked")),
        },
    }
}

/// The license item, reporting both the spec `License:` and the
/// license upstream's `manifest` states as evidence; a mismatch is
/// flagged for the reviewer to reconcile.
fn license_item(r: &Review, manifest: &str) -> Item {
    let label = "license matches upstream specification and is acceptable for Fedora";
    let spec = &r.spec.license;
    match &r.upstream_license {
        Some(c) if c == spec => Item {
            label: label.into(),
            mark: Mark::Pass,
            note: Some(format!("spec & {manifest}: {spec}")),
        },
        // Cargo's deprecated `/` separator means OR, which rust2rpm
        // rewrites to SPDX, and a legacy Cabal identifier such as `BSD3`
        // is what cabal-rpm turned into `BSD-3-Clause` — the same
        // license spelled differently, not a mismatch.
        Some(c) if normalize_license(c) == normalize_license(spec) => Item {
            label: label.into(),
            mark: Mark::Pass,
            note: Some(format!("spec: {spec}; {manifest}: {c} (equivalent)")),
        },
        Some(c) => Item {
            label: label.into(),
            mark: Mark::Caveat,
            note: Some(format!("spec: {spec}; {manifest}: {c} — reconcile")),
        },
        None => Item {
            label: label.into(),
            mark: Mark::Pass,
            note: Some(format!("spec: {spec}; {manifest}: not found")),
        },
    }
}

/// Normalize a license expression for comparison: Cargo's deprecated `/`
/// (meaning OR) becomes ` OR `, a legacy Cabal identifier becomes its
/// SPDX family, the `-only` / `-or-later` suffix the legacy identifier
/// cannot express is dropped, and whitespace is collapsed — so a Cargo
/// `MIT/Apache-2.0` compares equal to a spec `MIT OR Apache-2.0`, and a
/// `.cabal` `GPL-3` to the `GPL-3.0-or-later` cabal-rpm writes for it.
fn normalize_license(s: &str) -> String {
    let s = crate::review::cabal_license_spdx(s).unwrap_or(s);
    s.replace('/', " OR ")
        .split_whitespace()
        .map(|w| {
            w.strip_suffix("-only")
                .or_else(|| w.strip_suffix("-or-later"))
                .unwrap_or(w)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The statically-linked-deps item for a binary crate. With evidence,
/// it cross-checks the build log's LICENSE SUMMARY against the spec's
/// folded `License:` and notes whether all are present (✅) or some are
/// missing (🫤). The full breakdown is shown to the reviewer separately
/// (see `main`). Without evidence it stays a plain ✅ to confirm manually.
fn static_deps_item(r: &Review) -> Item {
    let label = "licenses of statically linked dependencies are correctly taken into account";
    let Some(sd) = &r.static_deps else {
        return Item::new(label, Mark::Pass, None);
    };
    if sd.summary.is_empty() {
        // No build-log summary to compare against — fall back to a manual
        // check, pointing at the breakdown shown below.
        return Item::new(
            label,
            Mark::Pass,
            Some("inspect the license breakdown below (no build-log summary found)"),
        );
    }
    let missing = sd.missing();
    if missing.is_empty() {
        Item::new(
            label,
            Mark::Pass,
            Some(&format!(
                "all {} bundled-dep licenses present in the spec License:",
                sd.summary.len()
            )),
        )
    } else {
        Item {
            label: label.into(),
            mark: Mark::Caveat,
            note: Some(format!(
                "missing from the spec License:: {}",
                missing.join(", ")
            )),
        }
    }
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

/// A fedora-review issue paired with the reviewer's resolution.
#[derive(Debug, Clone)]
pub struct ReviewedIssue {
    pub issue: Issue,
    pub resolution: Resolution,
}

impl ReviewedIssue {
    /// Wrap a freshly-parsed issue as unaddressed.
    pub fn open(issue: Issue) -> ReviewedIssue {
        ReviewedIssue {
            issue,
            resolution: Resolution::Keep,
        }
    }

    /// The concrete finding to show — the note fedora-review attached,
    /// falling back to the criterion text.
    pub fn finding(&self) -> &str {
        self.issue.note.as_deref().unwrap_or(&self.issue.text)
    }
}

/// Wrap parsed issues as unaddressed, ready for the reviewer to resolve.
pub fn reviewed(issues: &[Issue]) -> Vec<ReviewedIssue> {
    issues.iter().cloned().map(ReviewedIssue::open).collect()
}

/// Whether the package can be approved: no failed checklist item and no
/// issue still open (explained or removed issues don't block).
pub fn approved(items: &[Item], issues: &[ReviewedIssue]) -> bool {
    !items.iter().any(|i| i.mark == Mark::Fail)
        && !issues.iter().any(|i| i.resolution == Resolution::Keep)
}

/// The issues to address and those the reviewer addressed (with their
/// justification), each list followed by a blank line.
fn render_issues(issues: &[ReviewedIssue]) -> String {
    let mut out = String::new();
    let open: Vec<&ReviewedIssue> = issues
        .iter()
        .filter(|i| i.resolution == Resolution::Keep)
        .collect();
    if !open.is_empty() {
        out.push_str("Issues to address before approval:\n");
        for it in &open {
            out.push_str(&format!("- {}\n", it.finding()));
        }
        out.push('\n');
    }

    let mut explained = issues.iter().filter_map(|i| match &i.resolution {
        Resolution::Explained(why) => Some((i.finding(), why.as_str())),
        Resolution::Keep | Resolution::Removed => None,
    });
    if let Some(first) = explained.next() {
        out.push_str("Issues addressed by the reviewer:\n");
        for (finding, why) in std::iter::once(first).chain(explained) {
            out.push_str(&format!("- {finding} — {why}\n"));
        }
        out.push('\n');
    }
    out
}

/// The marked checklist and the verdict.
fn render_marks(items: &[Item], issues: &[ReviewedIssue]) -> String {
    let mut out = String::new();
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

/// Render the condensed-review block (the part between the first and
/// second `===`): intro line, any open MUST issues to address, any issues
/// the reviewer addressed (with their justification), the marked
/// checklist, and the verdict.
pub fn render_review(generator: Generator, items: &[Item], issues: &[ReviewedIssue]) -> String {
    let mut out = format!(
        "Package was generated with {}, simplifying the review.\n\n",
        generator_name(generator)
    );
    out.push_str(&render_issues(issues));
    out.push_str(&render_marks(items, issues));
    out
}

/// The review block for a Python spec (#160, #161): how it differs from
/// pyp2spec's output (`compared`, from `pyp2spec::render`) in place of
/// [`render_review`]'s intro line, then the issues, the checklist and
/// the verdict.
pub fn render_python_review(compared: &str, items: &[Item], issues: &[ReviewedIssue]) -> String {
    let mut out = String::new();
    out.push_str(compared.trim_end());
    out.push_str("\n\n");
    out.push_str(&render_issues(issues));
    out.push_str(&render_marks(items, issues));
    out
}

/// Render the post-import boilerplate (the part below the second `===`).
/// Rust-SIG and Haskell-SIG tasks; Python gets its own when added.
/// `package` is the spec `Name:` (the Haskell one is `ghc-<name>` for
/// a library but the bare name for a binary).
pub fn render_post_import(generator: Generator, upstream_name: &str, package: &str) -> String {
    match generator {
        Generator::Pyp2Spec => python_post_import(upstream_name, package),
        Generator::CabalRpm => haskell_post_import(upstream_name, package),
        _ => rust_post_import(upstream_name),
    }
}

fn python_post_import(name: &str, package: &str) -> String {
    format!(
        "Recommended post-import tasks:

- set up package on release-monitoring.org:
  project: {name}
  homepage: https://pypi.org/project/{name}/
  backend: PyPI
  version scheme: RPM
  distro: Fedora
  Package: {package}

- consider adding @python-packagers-sig as package co-maintainer

- track package in koschei for all built branches
"
    )
}

fn haskell_post_import(name: &str, package: &str) -> String {
    format!(
        "Recommended post-import haskell-sig tasks:

- set up package on release-monitoring.org:
  project: {name}
  homepage: https://hackage.haskell.org/package/{name}
  backend: Hackage
  version scheme: semantic
  distro: Fedora
  Package: {package}

- add @haskell-lang-sig with \"commit\" access as package co-maintainer

- track package in koschei for all built branches
"
    )
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
    use crate::review::{Generator, Issue, Review, SpecInfo, StaticDeps};

    fn review(
        tests_enabled: bool,
        has_license_file: bool,
        build_ok: bool,
        issues: Vec<Issue>,
    ) -> Review {
        Review {
            spec_text: String::new(),
            license_files: None,
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
            upstream_license: Some("MIT".to_string()),
            upstream_has_tests: None,
            build_ok,
            rpmlint_clean: true,
            issues,
            static_deps: None,
        }
    }

    /// A Python review of `spec`, built, with these `%license` files,
    /// each as `package path` (`python3-cstq` when no package is named).
    fn python_review(spec: &str, license_files: Option<&[&str]>) -> Review {
        let mut r = review(true, true, true, vec![]);
        r.spec.generator = Generator::Pyp2Spec;
        r.spec.name = "python-cstq".into();
        r.spec.version = "0.0.20".into();
        r.spec_text = spec.to_string();
        r.license_files = license_files.map(|f| {
            f.iter()
                .map(|s| {
                    let (package, path) = s.split_once(' ').unwrap_or(("python3-cstq", s));
                    LicenseFile {
                        package: package.into(),
                        path: path.into(),
                    }
                })
                .collect()
        });
        r
    }

    const CSTQ: &str = include_str!("../tests/data/pyp2spec/cstq.submitted.spec");
    const DIST_INFO_LICENSE: &str =
        "/usr/lib/python3.15/site-packages/cstq-0.0.20.dist-info/licenses/LICENSE.txt";

    fn mark_of(items: &[Item], label_start: &str) -> (Mark, Option<String>) {
        let it = items
            .iter()
            .find(|i| i.label.starts_with(label_start))
            .unwrap_or_else(|| panic!("no item {label_start}"));
        (it.mark, it.note.clone())
    }

    #[test]
    fn a_python_review_as_cstq_passes() {
        let r = python_review(CSTQ, Some(&[DIST_INFO_LICENSE]));
        let e = PythonEvidence {
            version: "0.0.20",
            latest: Some("0.0.20"),
            upstream_license: Some("MIT"),
            not_compared: None,
        };
        let items = python_checklist(&r, &e);
        assert_eq!(items.len(), 10);
        assert!(items.iter().all(|i| i.mark == Mark::Pass), "{items:?}");
        let block = render_python_review("Changes from …", &items, &[]);
        assert!(block.starts_with("Changes from …\n\n✅ package contains only"));
        assert!(block.contains("(spec & PyPI: 0.0.20)"));
        assert!(block.contains("(spec & PyPI: MIT)"));
        assert!(block.ends_with("Package APPROVED.\n"));
    }

    #[test]
    fn python_tests_need_a_runner_not_just_an_import_check() {
        let only_import = CSTQ.replace("%pytest -v\n", "");
        let r = python_review(&only_import, None);
        assert_eq!(
            python_tests_item(&r.spec_text, true).note.as_deref(),
            Some("import check only — run the tests or justify")
        );
        let none = only_import.replace("%pyproject_check_import\n", "");
        assert_eq!(
            python_tests_item(&none, true).note.as_deref(),
            Some("no tests run — justify")
        );
        assert_eq!(python_tests_item(CSTQ, true).mark, Mark::Pass);
        assert_eq!(python_tests_item(CSTQ, false).mark, Mark::Caveat);
        // A runner named outside %check does not count.
        let elsewhere = "%build\npytest\n%check\n%pyproject_check_import\n";
        assert_eq!(python_tests_item(elsewhere, true).mark, Mark::Caveat);
    }

    #[test]
    fn a_license_file_shipped_twice_is_a_caveat() {
        let by_hand = "/usr/share/licenses/python3-cstq/LICENSE.txt";
        let item = |spec: &str, files: Option<&[&str]>| {
            let r = python_review(spec, files);
            python_license_file_item(&r.spec_text, r.license_files.as_deref())
        };
        // -l, from the wheel only: as it should be.
        let it = item(CSTQ, Some(&[DIST_INFO_LICENSE]));
        assert_eq!((it.mark, it.note), (Mark::Pass, None));
        // Listed by hand too: shipped twice.
        let it = item(CSTQ, Some(&[DIST_INFO_LICENSE, by_hand]));
        assert_eq!(it.mark, Mark::Caveat);
        assert_eq!(
            it.note.as_deref(),
            Some("LICENSE.txt shipped twice, by hand and from the wheel — drop the %license line")
        );
        // The same with -L also suggests -l.
        let no_assert = CSTQ.replace("save_files -l cstq", "save_files -L cstq");
        let it = item(&no_assert, Some(&[DIST_INFO_LICENSE, by_hand]));
        assert!(
            it.note
                .unwrap()
                .ends_with("use %pyproject_save_files -l and drop the %license line")
        );
        // -L, from the wheel only: fine, -l suggested.
        let it = item(&no_assert, Some(&[DIST_INFO_LICENSE]));
        assert_eq!(it.mark, Mark::Pass);
        assert!(it.note.unwrap().contains("-l would keep it so"));
        // By hand only: the wheel declares none.
        let it = item(&no_assert, Some(&[by_hand]));
        assert_eq!(
            (it.mark, it.note.as_deref()),
            (Mark::Pass, Some("listed by hand; the wheel declares none"))
        );
        // None at all.
        assert_eq!(item(&no_assert, Some(&[])).mark, Mark::Fail);
        // Without the built package: -l is enough evidence; else unchecked.
        assert_eq!(
            item(CSTQ, None).note.as_deref(),
            Some("%pyproject_save_files -l")
        );
        assert_eq!(
            item(&no_assert, None).note.as_deref(),
            Some("built package not checked")
        );
    }

    #[test]
    fn another_subpackages_own_license_is_not_a_duplicate() {
        let doc = "python-cstq-doc /usr/share/licenses/python-cstq-doc/LICENSE.txt";
        let r = python_review(CSTQ, Some(&[DIST_INFO_LICENSE, doc]));
        let it = python_license_file_item(&r.spec_text, r.license_files.as_deref());
        assert_eq!((it.mark, it.note), (Mark::Pass, None));
    }

    #[test]
    fn a_declarative_spec_uses_the_macros_through_its_buildsystem() {
        let declarative = "BuildSystem:    pyproject\nBuildOption(install): -l cstq\n\
                           %files -n python3-cstq -f %{pyproject_files}\n";
        assert_eq!(save_files_flags(declarative), ["-l"]);
        // BuildOption(install) means nothing without BuildSystem: pyproject.
        assert!(save_files_flags("BuildOption(install): -l x\n").is_empty());
        let r = python_review(declarative, None);
        let e = PythonEvidence {
            version: "0.0.20",
            latest: None,
            upstream_license: None,
            not_compared: None,
        };
        let items = python_checklist(&r, &e);
        assert_eq!(mark_of(&items, "files are listed").0, Mark::Pass);
        assert_eq!(mark_of(&items, "BuildRequires").0, Mark::Pass);
        assert_eq!(
            mark_of(&items, "license file").1.as_deref(),
            Some("%pyproject_save_files -l")
        );
        // PyPI's license unknown: compared by hand.
        assert_eq!(mark_of(&items, "license matches").0, Mark::Caveat);
    }

    #[test]
    fn pypi_versions_compare_in_normal_form() {
        assert_eq!(pep440("1.0dev3"), pep440("1.0.dev3"));
        assert_eq!(pep440("2.0-RC1"), "2.0rc1");
        assert_eq!(pep440("1.0.Alpha2"), "1.0a2");
        assert_eq!(pep440("1.0.post1"), "1.0post1");
        assert!(is_pre_release("2.0rc1"));
        assert!(is_pre_release("1.0.dev3"));
        assert!(!is_pre_release("1.0.post1"));
        assert!(!is_pre_release("2.0"));
        let item = |p, l| python_version_item(p, Some(l));
        assert_eq!(item("1.0dev3", "1.0.dev3").mark, Mark::Pass);
        assert_eq!(
            item("2.0rc1", "1.9").note.as_deref(),
            Some("spec: 2.0rc1, a pre-release; PyPI's latest stable: 1.9")
        );
        // A pre-release older than the stable release is behind.
        assert_eq!(item("1.0rc1", "1.0").mark, Mark::Caveat);
        assert_eq!(item("1.0rc1", "2.3").mark, Mark::Caveat);
        assert_eq!(release_of("2.0rc1"), [2]);
        assert_eq!(release_of("2.1.0rc1"), [2, 1]);
        // 2.0rc1 comes before 2, written either way.
        assert_eq!(item("2.0rc1", "2").mark, Mark::Caveat);
        // PEP 440's other spellings.
        assert_eq!(pep440("1.0c1"), pep440("1.0rc1"));
        assert_eq!(pep440("1.0-1"), pep440("1.0.post1"));
        assert_eq!(item("1.0c1", "1.0rc1").mark, Mark::Pass);
        assert_eq!(item("1.8", "1.9").mark, Mark::Caveat);
        assert_eq!(python_version_item("1.8", None).mark, Mark::Pass);
    }

    #[test]
    fn a_commented_runner_and_a_later_scriptlet_do_not_count() {
        let commented = "%check\n%pyproject_check_import\n# %pytest -- needs network\n";
        assert_eq!(python_tests_item(commented, true).mark, Mark::Caveat);
        let scriptlet = "%check\n%pyproject_check_import\n%posttrans\npytest\n";
        assert_eq!(python_tests_item(scriptlet, true).mark, Mark::Caveat);
        assert!(is_section_header("%filetriggerin -- /usr/lib"));
        assert!(is_section_header("%postun -n python3-foo"));
        assert!(!is_section_header("%pytest -v"));
    }

    #[test]
    fn a_review_without_the_comparison_is_not_approved() {
        let r = python_review(CSTQ, Some(&[DIST_INFO_LICENSE]));
        let e = PythonEvidence {
            version: "0.0.20",
            latest: None,
            upstream_license: None,
            not_compared: Some("--no-net"),
        };
        let items = python_checklist(&r, &e);
        assert_eq!(
            mark_of(&items, "spec compared"),
            (
                Mark::Fail,
                Some("not compared: --no-net — read the spec, then mark it".to_string())
            )
        );
        assert!(!approved(&items, &[]));
    }

    #[test]
    fn macros_and_runners_count_only_when_used() {
        // Unbraced %pyproject_files is the same macro.
        let unbraced = CSTQ.replace("-f %{pyproject_files}", "-f %pyproject_files");
        let items = python_checklist(
            &python_review(&unbraced, None),
            &PythonEvidence {
                version: "0.0.20",
                latest: None,
                upstream_license: Some("MIT"),
                not_compared: None,
            },
        );
        assert_eq!(mark_of(&items, "files are listed").0, Mark::Pass);
        // A commented-out macro is not used.
        let commented = CSTQ.replace("%pyproject_buildrequires", "# %pyproject_buildrequires");
        assert!(!uses(&commented, "%pyproject_buildrequires"));
        // An import check excluding a pytest plugin runs no tests.
        let plugin = "%check\n%pyproject_check_import -e '*.pytest_plugin'\n";
        assert_eq!(
            python_tests_item(plugin, true).note.as_deref(),
            Some("import check only — run the tests or justify")
        );
        let older = "%check\n%py3_check_import foo\n";
        assert_eq!(
            python_tests_item(older, true).note.as_deref(),
            Some("import check only — run the tests or justify")
        );
        let module = "%check\n%{python3} -m pytest -v\n";
        assert_eq!(python_tests_item(module, true).mark, Mark::Pass);
    }

    #[test]
    fn a_file_shipped_twice_by_two_packages_is_named_once() {
        let r = python_review(
            CSTQ,
            Some(&[
                "a /usr/lib/x/a.dist-info/licenses/LICENSE",
                "a /usr/share/licenses/a/LICENSE",
                "b /usr/lib/x/b.dist-info/licenses/COPYING",
                "b /usr/share/licenses/b/COPYING",
                "c /usr/lib/x/c.dist-info/licenses/LICENSE",
                "c /usr/share/licenses/c/LICENSE",
            ]),
        );
        let it = python_license_file_item(&r.spec_text, r.license_files.as_deref());
        assert!(
            it.note
                .unwrap()
                .starts_with("COPYING, LICENSE shipped twice")
        );
    }

    #[test]
    fn hand_written_file_lists_and_buildrequires_are_caveats() {
        let by_hand = CSTQ
            .replace("%pyproject_save_files -l cstq\n", "")
            .replace(" -f %{pyproject_files}", "")
            .replace("%pyproject_buildrequires\n", "");
        let r = python_review(&by_hand, None);
        let e = PythonEvidence {
            version: "0.0.20",
            latest: Some("0.0.21"),
            upstream_license: Some("Apache-2.0"),
            not_compared: None,
        };
        let items = python_checklist(&r, &e);
        for label in ["files are listed", "BuildRequires are generated"] {
            assert_eq!(
                mark_of(&items, label),
                (Mark::Caveat, Some("listed by hand".to_string()))
            );
        }
        assert_eq!(mark_of(&items, "latest version").0, Mark::Caveat);
        assert_eq!(mark_of(&items, "license matches").0, Mark::Caveat);
        // The declarative buildsystem generates them itself.
        assert!(pyproject_buildsystem("BuildSystem:    pyproject\n"));
        assert!(!pyproject_buildsystem("BuildSystem: meson\n"));
    }

    #[test]
    fn save_files_flags_and_sections_are_read() {
        assert_eq!(save_files_flags(CSTQ), ["-l"]);
        assert_eq!(
            save_files_flags("%pyproject_save_files -L -M\n"),
            ["-L", "-M"]
        );
        assert_eq!(
            section(CSTQ, "%check"),
            "%pyproject_check_import\n%pytest -v\n\n"
        );
        assert_eq!(section(CSTQ, "%conf"), "");
    }

    #[test]
    fn python_post_import_tasks_name_pypi() {
        let text = render_post_import(Generator::Pyp2Spec, "cstq", "python-cstq");
        assert!(text.contains("backend: PyPI"));
        assert!(text.contains("homepage: https://pypi.org/project/cstq/"));
        assert!(text.contains("Package: python-cstq"));
    }

    #[test]
    fn haskell_review_uses_hackage_and_the_cabal_file_as_evidence() {
        let mut r = review(false, true, true, vec![]);
        r.spec.generator = Generator::CabalRpm;
        r.spec.name = "ghc-shell-monad".into();
        r.spec.version = "0.6.10".into();
        r.spec.license = "BSD-3-Clause".into();
        r.upstream_name = "shell-monad".into();
        r.upstream_license = Some("BSD3".into());
        r.upstream_has_tests = Some(false);
        let items = infer(&r, Some("0.6.10"));
        assert_eq!(items.len(), 7);
        let block = render_review(r.spec.generator, &items, &[]);
        assert!(block.contains("generated with cabal-rpm"));
        // No suite to run is a caveat with the reason filled in, not a pass.
        assert!(block.contains(
            "🫤 test suite is run and all unit tests pass (upstream ships no test suite)"
        ));
        assert!(approved(&items, &[]), "a caveat does not block approval");
        assert!(
            block.contains("✅ latest version of the package is packaged (spec & Hackage: 0.6.10)")
        );
        assert!(
            block.contains("(spec: BSD-3-Clause; .cabal: BSD3 (equivalent))"),
            "{block}"
        );
        assert!(block.contains("✅ package complies with Haskell Packaging Guidelines"));
        // A legacy GPL-3 cannot say -only or -or-later, so cabal-rpm's
        // GPL-3.0-or-later is equivalent, not a mismatch.
        r.spec.license = "GPL-3.0-or-later".into();
        r.upstream_license = Some("GPL-3".into());
        assert!(infer(&r, None)[4].mark == Mark::Pass);
        // Tests disabled with a suite upstream asks for a justification.
        r.upstream_has_tests = Some(true);
        assert_eq!(
            infer(&r, None)[2].note.as_deref(),
            Some("disabled — justify")
        );
        let post = render_post_import(Generator::CabalRpm, "shell-monad", "ghc-shell-monad");
        assert!(post.contains("backend: Hackage"));
        assert!(post.contains("Package: ghc-shell-monad"));
        assert!(post.contains("@haskell-lang-sig"));
    }

    #[test]
    fn clean_review_is_all_pass_and_approved() {
        let r = review(true, true, true, vec![]);
        let items = infer(&r, Some("1.0"));
        assert_eq!(items.len(), 7);
        assert!(items.iter().all(|i| i.mark == Mark::Pass));
        assert!(approved(&items, &reviewed(&r.issues)));
        let block = render_review(r.spec.generator, &items, &reviewed(&r.issues));
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
        assert!(approved(&items, &reviewed(&r.issues)));
        let block = render_review(r.spec.generator, &items, &reviewed(&r.issues));
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
        assert!(approved(&items, &reviewed(&r.issues)));
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
        r.upstream_license = Some("MIT OR Apache-2.0".to_string());
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
        r.upstream_license = Some("MIT/Apache-2.0".to_string()); // deprecated Cargo form
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
    fn static_deps_evidence_passes_when_all_present_and_flags_missing() {
        let mut r = review(true, true, true, vec![]);
        r.spec.ships_binary = true;
        // All summary licenses are listed in the spec → ✅ with a count.
        r.static_deps = Some(StaticDeps {
            summary: vec!["MIT".into(), "Apache-2.0 OR MIT".into()],
            spec_listed: vec!["MIT".into(), "Apache-2.0 OR MIT".into()],
            spec_section: "# MIT\n# Apache-2.0 OR MIT\nLicense: MIT AND (Apache-2.0 OR MIT)".into(),
        });
        let item = &infer(&r, Some("1.0"))[5];
        assert_eq!(item.mark, Mark::Pass);
        assert_eq!(
            item.note.as_deref(),
            Some("all 2 bundled-dep licenses present in the spec License:")
        );
        // A summary license absent from the spec → 🫤 naming it.
        r.static_deps = Some(StaticDeps {
            summary: vec!["MIT".into(), "MPL-2.0".into()],
            spec_listed: vec!["MIT".into()],
            spec_section: "# MIT\nLicense: MIT".into(),
        });
        let item = &infer(&r, Some("1.0"))[5];
        assert_eq!(item.mark, Mark::Caveat);
        assert!(item.note.as_deref().unwrap().contains("MPL-2.0"));
    }

    #[test]
    fn build_failure_fails_and_blocks_approval() {
        let r = review(true, true, false, vec![]);
        let items = infer(&r, Some("1.0"));
        assert_eq!(items[1].mark, Mark::Fail);
        assert!(!approved(&items, &reviewed(&r.issues)));
        let block = render_review(r.spec.generator, &items, &reviewed(&r.issues));
        assert!(block.contains("Package not yet approved"));
    }

    #[test]
    fn install_failure_fails_the_builds_item() {
        // The build produced an RPM, but fedora-review's install check
        // failed — the "builds and installs" item must default to Fail.
        let issue = Issue {
            name: "CheckPackageInstalls".to_string(),
            text: "Package installs properly.".to_string(),
            note: Some("Installation errors (see attachment)".to_string()),
        };
        let r = review(true, true, true, vec![issue]);
        let items = infer(&r, Some("1.0"));
        assert_eq!(items[1].mark, Mark::Fail);
        assert_eq!(
            items[1].note.as_deref(),
            Some("installation failed — see fedora-review attachment")
        );
        assert!(!approved(&items, &reviewed(&r.issues)));
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
        assert!(!approved(&items, &reviewed(&r.issues)));
        let block = render_review(r.spec.generator, &items, &reviewed(&r.issues));
        assert!(block.contains("Issues to address before approval:"));
        // The concrete finding (note), not the criterion text.
        assert!(block.contains("- warning: File listed twice"));
    }

    #[test]
    fn explained_issue_unblocks_and_is_listed_separately() {
        let issue = Issue {
            name: "CheckPackageName".to_string(),
            text: "Package name conforms.".to_string(),
            note: Some("A package with this name already exists.".to_string()),
        };
        let reviewed = vec![ReviewedIssue {
            issue,
            resolution: Resolution::Explained("re-review of the retired package".to_string()),
        }];
        let r = review(true, true, true, vec![]);
        let items = infer(&r, Some("1.0"));
        // An explained issue no longer blocks (no failing item either).
        assert!(approved(&items, &reviewed));
        let block = render_review(r.spec.generator, &items, &reviewed);
        assert!(!block.contains("Issues to address before approval:"));
        assert!(block.contains("Issues addressed by the reviewer:"));
        assert!(block.contains(
            "- A package with this name already exists. — re-review of the retired package"
        ));
        assert!(block.trim_end().ends_with("Package APPROVED."));
    }

    #[test]
    fn removed_issue_is_just_absent() {
        // "Remove" is modeled as dropping the issue from the list, so an
        // empty list approves and shows no issues section.
        let r = review(true, true, true, vec![]);
        let items = infer(&r, Some("1.0"));
        let block = render_review(r.spec.generator, &items, &[]);
        assert!(approved(&items, &[]));
        assert!(!block.contains("Issues to address"));
        assert!(!block.contains("Issues addressed"));
    }

    #[test]
    fn post_import_substitutes_the_crate_name() {
        let p = render_post_import(Generator::Rust2Rpm, "trustfall_core", "rust-trustfall_core");
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
