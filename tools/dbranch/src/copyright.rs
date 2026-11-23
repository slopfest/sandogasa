// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Checking `debian/copyright` against the source, and the placeholders
//! the tools that write it leave behind.
//!
//! `licenserecon` compares the file against `licensecheck` and exits
//! non-zero when they disagree about a license. `cme update
//! dpkg-copyright` reconciles an existing file; `decopy` writes one
//! from scratch. Neither produces something uploadable unreviewed:
//! both leave placeholders where they could not tell, and
//! `licenserecon` passes them, because a placeholder is not a license
//! disagreement.
//!
//! Where the tools come from, and what documents them:
//!
//! - `cme` (packages cme and libconfig-model-dpkg-perl, which holds
//!   the dpkg-copyright model) and `licensecheck`, which `lrc` runs,
//!   are in the Developer's Reference appendix "Overview of Debian
//!   Maintainer Tools". Its checklist for sponsoring a new package asks
//!   for `licensecheck` to be run and `debian/copyright` checked.
//! - `lrc` (package licenserecon) and `decopy` (package decopy) are in
//!   neither Policy nor the Developer's Reference, and ship no man
//!   page. The Debian wiki's CopyrightReviewTools page lists them, with
//!   `cme` and `licensecheck`.
//! - The file itself: Debian Policy, "Copyright information" and
//!   "Machine-readable copyright information", and DEP-5.
//!
//! The stage's warnings are [`Finding`]s, so each names these sources
//! through [`crate::cite`] the way `dbranch check`'s findings do.

use crate::check::{Finding, Severity};

/// Placeholders the generators leave for a human, as the exact strings
/// they write.
///
/// `cme` writes `no-info-found` where it found no copyright holder;
/// `decopy` writes `Source: TODO`. `FIXME` is neither tool's, but it
/// is what a packager leaves themselves, and it means the same here.
const PLACEHOLDERS: [&str; 3] = ["no-info-found", "TODO", "FIXME"];

/// The placeholders present in a `debian/copyright`, each named once,
/// in the order listed.
///
/// Worth reporting separately from `licenserecon`'s verdict: a file
/// can satisfy it and still say `no-info-found`, which is a statement
/// about somebody else's copyright that nobody has checked.
pub fn placeholders(text: &str) -> Vec<&'static str> {
    PLACEHOLDERS
        .into_iter()
        .filter(|p| text.contains(p))
        .collect()
}

/// What to say about the placeholders in a file, or `None` when it has
/// none.
pub fn placeholder_warning(text: &str) -> Option<String> {
    let found = placeholders(text);
    if found.is_empty() {
        return None;
    }
    Some(format!(
        "debian/copyright still says {} — that is a claim about somebody's \
         copyright that no tool could settle, so it needs a person before upload",
        found.join(" and ")
    ))
}

/// The copyright holders a DEP-5 file names, one per entry, with the
/// years stripped.
///
/// A `Copyright:` field holds one holder per line and continues onto
/// indented lines, so the second and later holders are continuations
/// -- which is how `fennel.lua`'s author hid in plain sight on
/// fnlfmt: a grep for `^Copyright:` sees the first name only.
pub fn holders(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in copyright_lines(text) {
        let name = holder_name(line);
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// Every `Copyright:` field's value, one entry per line, continuations
/// included -- the lines both the holder and the year questions are
/// asked of.
fn copyright_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut in_field = false;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("Copyright:") {
            in_field = true;
            out.push(value);
        } else if in_field && (line.starts_with(' ') || line.starts_with('\t')) {
            out.push(line);
        } else {
            in_field = false;
        }
    }
    out
}

/// The latest year a copyright file claims, or `None` when it claims
/// an open-ended span.
///
/// `2020-present` is the form Debian copyright files use for "still
/// going", and a file saying that cannot go out of date -- so it is
/// not a year, it is an answer to the question.
pub fn latest_year(text: &str) -> Option<u32> {
    // The `Files: debian/*` stanza is the packaging's own copyright,
    // and it is current by construction -- the packager just edited
    // it. Counting it hides the question being asked: fnlfmt claimed
    // 2024 for upstream's files while that stanza said 2026, and a
    // comparison of whole-file maxima saw nothing wrong.
    let lines: Vec<&str> = text
        .split("\n\n")
        .filter(|para| {
            !para.lines().any(|l| {
                l.strip_prefix("Files:")
                    .is_some_and(|f| f.trim().starts_with("debian/"))
            })
        })
        .flat_map(copyright_lines)
        .collect();
    if lines.iter().any(|l| l.to_lowercase().contains("present")) {
        return None;
    }
    lines
        .iter()
        .flat_map(|l| {
            l.split(|c: char| !c.is_ascii_digit())
                .filter(|w| w.len() == 4)
                .filter_map(|w| w.parse::<u32>().ok())
        })
        .filter(|y| (1970..=2200).contains(y))
        .max()
}

/// `(claimed, found)` when a scan of the source mentions a year the
/// copyright file does not reach.
///
/// The year is the half `licenserecon` and the holder comparison both
/// miss: a file crediting the right people under the right license
/// still goes stale every January, and the release that ships it says
/// so. fnlfmt claimed 2019-2024 while its own README said 2026.
pub fn stale_years(existing: &str, generated: &str) -> Option<(u32, u32)> {
    let claimed = latest_year(existing)?;
    let found = latest_year(generated)?;
    (found > claimed).then_some((claimed, found))
}

/// One holder entry with the years and their punctuation removed:
/// `© 2019-2024 fnlfmt contributors` and `2019-2024, fnlfmt
/// contributors` both become `fnlfmt contributors`, so two files
/// written in different styles can still be compared.
fn holder_name(entry: &str) -> String {
    entry
        .split_whitespace()
        .filter(|w| !is_years(w.trim_matches(|c: char| !c.is_alphanumeric())))
        .map(|w| w.trim_start_matches(['©', ',']).trim_matches(','))
        .filter(|w| !w.is_empty() && !w.eq_ignore_ascii_case("(c)"))
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Whether a word is a year or a span of them rather than part of a
/// name: `2019`, `2019-2024`, and the `2020-present` that Debian
/// copyright files write, which is not all digits and so needs saying
/// explicitly.
fn is_years(word: &str) -> bool {
    if word.is_empty() {
        return true;
    }
    word.split('-').all(|part| {
        !part.is_empty() && (part.chars().all(|c| c.is_ascii_digit()) || part == "present")
    })
}

/// Holders the generated file names that the existing one never
/// mentions anywhere.
///
/// One-directional on purpose. The question is "whose copyright is
/// not written down", and a holder the existing file has but the scan
/// missed is the ordinary case -- a tool that cannot read a header is
/// not evidence of anything. Matched by name against the whole file
/// rather than against its holder list, because the name may be
/// recorded as an `Upstream-Contact` instead, and that counts.
pub fn unattributed(existing: &str, generated: &str) -> Vec<String> {
    let haystack = existing.to_lowercase();
    holders(generated)
        .into_iter()
        .filter(|h| !haystack.contains(&h.to_lowercase()))
        .collect()
}

/// What `cme check dpkg-copyright` printed, as a finding: it exits 0
/// for a file it rejects as well as one it accepts, so whatever it
/// says is the finding, and silence is the only pass.
pub fn syntax_finding(cme_output: &str) -> Option<Finding> {
    if cme_output.trim().is_empty() {
        return None;
    }
    Some(Finding::new(
        "copyright-syntax",
        Severity::Warning,
        "cme is unhappy with debian/copyright above; it exits 0 either way, \
         so what it printed is the finding"
            .to_string(),
    ))
}

/// `lrc` found a license debian/copyright does not declare, or the
/// reverse, with the two commands that can reconcile it.
pub fn mismatch_finding(cme: &str, decopy: &str) -> Finding {
    Finding::new(
        "copyright-license-mismatch",
        Severity::Warning,
        format!(
            "debian/copyright disagrees with the source; {cme} reconciles what \
             is there, {decopy} regenerates it from a scan"
        ),
    )
}

/// [`placeholder_warning`] as a finding.
pub fn placeholder_finding(text: &str) -> Option<Finding> {
    placeholder_warning(text)
        .map(|warning| Finding::new("copyright-placeholder", Severity::Warning, warning))
}

/// [`stale_years`] as a finding.
pub fn stale_years_finding(existing: &str, generated: &str) -> Option<Finding> {
    let (claimed, found) = stale_years(existing, generated)?;
    Some(Finding::new(
        "copyright-years-stale",
        Severity::Warning,
        format!(
            "debian/copyright claims {claimed} at the latest, and the source \
             mentions {found}. A file crediting the right people under the \
             right license still goes stale every January."
        ),
    ))
}

/// [`unattributed`] as a finding, pointing at the scan to compare
/// with (`scan`, relative to the packaging repository).
pub fn unattributed_finding(existing: &str, generated: &str, scan: &str) -> Option<Finding> {
    let missing = unattributed(existing, generated);
    if missing.is_empty() {
        return None;
    }
    Some(Finding::new(
        "copyright-holder-unattributed",
        Severity::Warning,
        format!(
            "the source names {}, which debian/copyright does not. Compare it \
             with {scan} -- that scan is a guess, junk entries and all, so read \
             it rather than copy it.",
            missing.join(" and ")
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two the generators actually write, measured: cme leaves
    /// `Copyright: no-info-found` for a file it could not attribute,
    /// decopy leaves `Source: TODO`.
    /// The real fnlfmt case, which is the reason this exists: decopy
    /// puts both holders in one Files: * paragraph, and the second is
    /// a continuation line, so anything reading `^Copyright:` alone
    /// sees Calvin Rose and misses the contributors -- or, on the
    /// committed file, sees the contributors and misses nobody.
    #[test]
    fn holders_include_the_continuation_lines() {
        let generated = "Files: *\nCopyright: Calvin Rose\n           2019-2024, fnlfmt \
                         contributors\nLicense: Expat\n";
        assert_eq!(holders(generated), ["Calvin Rose", "fnlfmt contributors"]);
        let committed = "Files: *\nCopyright: © 2019-2024 fnlfmt contributors\nLicense: Expat\n";
        assert_eq!(holders(committed), ["fnlfmt contributors"]);
    }

    /// Years and their punctuation differ between the two files and
    /// mean nothing here, so they come off before anything is
    /// compared.
    #[test]
    fn years_and_symbols_are_not_part_of_a_name() {
        assert_eq!(
            holder_name("© 2019-2024 fnlfmt contributors"),
            "fnlfmt contributors"
        );
        assert_eq!(
            holder_name("2019-2024, fnlfmt contributors"),
            "fnlfmt contributors"
        );
        assert_eq!(holder_name("  Calvin Rose  "), "Calvin Rose");
        assert_eq!(holder_name("2019-2026"), "");
        // Debian copyright files write open-ended spans this way, and
        // `2020-present` is not all digits -- damo's file is written
        // like that, and the year rode along in the name until it was.
        assert_eq!(
            holder_name("2020-present SeongJae Park <sjp38.park@gmail.com>"),
            "SeongJae Park <sjp38.park@gmail.com>"
        );
        assert_eq!(holder_name("2020-present"), "");
    }

    /// A package whose copyright is complete says nothing at all --
    /// damo, whose two holders both appear, written `2020-present`
    /// against the scan's `2020,`. A check that only ever fires
    /// cannot be told from a broken one.
    #[test]
    fn a_complete_copyright_reports_nothing() {
        let existing = "Files: *\nCopyright:\n 2020-present SeongJae Park <sjp38@x.invalid>\n\
                        License: GPL-2\n\nFiles: debian/*\nCopyright:\n 2023-present Michel \
                        Lind <m@x.invalid>\nLicense: GPL-2\n";
        let scanned = "Files: *\nCopyright: 2020, SeongJae Park <sjp38@x.invalid>\nLicense: \
                       GPL-2\n\nFiles: debian/*\nCopyright: 2023, Michel Lind <m@x.invalid>\n\
                       License: GPL-2\n";
        assert_eq!(
            holders(existing),
            [
                "SeongJae Park <sjp38@x.invalid>",
                "Michel Lind <m@x.invalid>"
            ]
        );
        assert!(unattributed(existing, scanned).is_empty());
    }

    /// What the check reports: a holder the scan found and the file
    /// never mentions. fnlfmt's answer is Calvin Rose alone -- the
    /// contributors line is worded differently in each file but the
    /// name is there, so it is not flagged.
    #[test]
    fn only_the_unmentioned_holder_is_reported() {
        let generated = "Copyright: Calvin Rose\n           2019-2024, fnlfmt contributors\n";
        let committed = "Upstream-Contact: Phil Hagelberg <phil@hagelb.org>\n\nFiles: *\n\
                         Copyright: © 2019-2024 fnlfmt contributors\n";
        assert_eq!(unattributed(committed, generated), ["Calvin Rose"]);
        // A name recorded only as Upstream-Contact still counts.
        assert!(unattributed(committed, "Copyright: 2019 Phil Hagelberg\n").is_empty());
        // Nothing new to say about a file that already says it.
        assert!(unattributed(committed, committed).is_empty());
    }

    /// The three real files this was tried against: fnlfmt before the
    /// fix (claimed 2024 while its own README said 2026), fnlfmt
    /// after, and damo, whose open-ended spans cannot go stale.
    #[test]
    fn a_later_year_in_the_source_is_what_is_reported() {
        let scan = "Files: *\nCopyright: Phil Hagelberg\n           2019-2026, Phil \
                    Hagelberg\nLicense: Expat\n";
        let stale = "Files: *\nCopyright: © 2019-2024 fnlfmt contributors\nLicense: Expat\n";
        assert_eq!(stale_years(stale, scan), Some((2024, 2026)));

        let fixed = "Files: *\nCopyright: © 2019-2026 Phil Hagelberg and contributors\n";
        assert_eq!(stale_years(fixed, scan), None);

        // An open-ended span answers the question already.
        let present = "Files: *\nCopyright:\n 2020-present SeongJae Park <s@x.invalid>\n";
        assert_eq!(latest_year(present), None);
        assert_eq!(stale_years(present, scan), None);
    }

    /// Years come from the Copyright fields alone -- a date elsewhere
    /// in the file is not a claim about copyright.
    #[test]
    fn only_the_copyright_fields_are_read_for_years() {
        let text = "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n\
                    Source: https://example.invalid/thing-2099\n\n\
                    Files: *\nCopyright: 2019 Someone\nLicense: Expat\n";
        assert_eq!(latest_year(text), Some(2019));
    }

    #[test]
    fn the_generators_placeholders_are_found() {
        assert_eq!(
            placeholders("Files: LICENSE\nCopyright: no-info-found\nLicense: Expat\n"),
            ["no-info-found"]
        );
        assert_eq!(placeholders("Source: TODO\n"), ["TODO"]);
        assert_eq!(
            placeholders("Source: TODO\nCopyright: no-info-found\n"),
            ["no-info-found", "TODO"]
        );
    }

    /// A finished file says nothing, so nothing is printed.
    #[test]
    fn a_complete_file_warns_about_nothing() {
        let text = "Format: https://www.debian.org/doc/packaging-manuals/\
                    copyright-format/1.0/\nSource: https://example.invalid/x\n\n\
                    Files: *\nCopyright: 2026 Someone\nLicense: Expat\n";
        assert!(placeholders(text).is_empty());
        assert!(placeholder_warning(text).is_none());
    }

    #[test]
    fn the_warning_names_what_it_found() {
        let w = placeholder_warning("Copyright: no-info-found\n").expect("a warning");
        assert!(w.contains("no-info-found"), "{w}");
        assert!(w.contains("needs a person before upload"), "{w}");
    }

    #[test]
    fn each_warning_is_a_finding_that_cites_its_sources() {
        let cited = |f: Finding, slug: &str| {
            assert_eq!(f.check, slug);
            assert_eq!(f.severity, Severity::Warning);
            assert!(!f.sources.is_empty(), "{slug} cites nothing");
            f.message
        };

        assert!(syntax_finding(" \n").is_none(), "silence is a pass");
        let m = cited(
            syntax_finding("Warning in 'Files:*': ...").unwrap(),
            "copyright-syntax",
        );
        assert!(m.contains("exits 0 either way"), "{m}");

        let m = cited(
            mismatch_finding("cme update dpkg-copyright", "decopy > debian/copyright"),
            "copyright-license-mismatch",
        );
        assert!(m.contains("cme update dpkg-copyright reconciles"), "{m}");
        assert!(m.contains("decopy > debian/copyright regenerates"), "{m}");

        assert!(placeholder_finding("Copyright: 2020 A\n").is_none());
        let m = cited(
            placeholder_finding("Copyright: no-info-found\n").unwrap(),
            "copyright-placeholder",
        );
        assert!(m.contains("no-info-found"), "{m}");

        let scan = "Files: *\nCopyright: 2019-2026, Phil Hagelberg\nLicense: Expat\n";
        let stale = "Files: *\nCopyright: © 2019-2024 Phil Hagelberg\nLicense: Expat\n";
        assert!(stale_years_finding(scan, scan).is_none());
        let m = cited(
            stale_years_finding(stale, scan).unwrap(),
            "copyright-years-stale",
        );
        assert!(
            m.contains("claims 2024") && m.contains("mentions 2026"),
            "{m}"
        );

        let generated = "Copyright: Calvin Rose\n";
        assert!(unattributed_finding(stale, stale, "../x").is_none());
        let m = cited(
            unattributed_finding(stale, generated, "../fnlfmt.copyright.decopy").unwrap(),
            "copyright-holder-unattributed",
        );
        assert!(m.contains("names Calvin Rose"), "{m}");
        assert!(m.contains("../fnlfmt.copyright.decopy"), "{m}");
    }

    #[test]
    fn a_copyright_finding_renders_with_its_citations() {
        let text = crate::check::render(&[placeholder_finding("Copyright: TODO\n").unwrap()]);
        assert!(
            text.starts_with("warning: debian/copyright still says TODO"),
            "{text}"
        );
        assert!(
            text.contains("see: Debian Policy 12.5, Copyright information"),
            "{text}"
        );
        assert!(
            text.contains("https://www.debian.org/doc/debian-policy/ch-docs.html#s-copyrightfile"),
            "{text}"
        );
        let text = crate::check::render(&[mismatch_finding("cme", "decopy")]);
        assert!(
            text.contains("see: Debian wiki, CopyrightReviewTools, licenserecon"),
            "{text}"
        );
        assert!(
            text.contains("https://wiki.debian.org/CopyrightReviewTools#licenserecon"),
            "{text}"
        );
    }
}
