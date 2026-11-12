// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Section numbers of a Sphinx manual, read from its sources.
//!
//! Debian Policy and the Developer's Reference are both Sphinx
//! projects whose `index.rst` lists the chapters in a `:numbered:`
//! toctree. Sphinx numbers the sections from that order and from
//! the heading levels inside each chapter, so the number a reader
//! sees ("Policy 4.4") is not written anywhere in the source. This
//! module works it out the same way, so a citation can be checked
//! against a local clone without building the HTML.
//!
//! Only the first numbered toctree is read. The appendices sit in
//! a second one whose numbering starts over, and neither manual's
//! appendices are cited here.

use std::path::Path;

/// One numbered heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The number Sphinx gives it, e.g. `4.4` or `5.6.12.1`.
    pub number: String,
    /// The chapter file it is in, without `.rst` (`ch-source`).
    pub doc: String,
    /// The `.. _label:` lines right above it, each of which is an
    /// HTML anchor for it. Several are stacked to keep an old anchor
    /// working after a rename.
    pub labels: Vec<String>,
    /// The heading text with reStructuredText's literal markup
    /// removed (``` ``debian/changelog`` ``` reads `debian/changelog`).
    pub title: String,
}

/// The chapter files the first `:numbered:` toctree in `index` lists.
///
/// An entry may be a bare document name or `Title <doc>`; only the
/// document is kept.
pub fn chapters(index: &str) -> Vec<String> {
    let mut block: Vec<&str> = Vec::new();
    let mut in_toctree = false;
    let finish = |block: &[&str]| -> Option<Vec<String>> {
        let numbered = block.iter().any(|l| l.trim() == ":numbered:");
        numbered.then(|| {
            block
                .iter()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty() && !l.starts_with(':'))
                .map(|l| match l.rsplit_once('<') {
                    Some((_, doc)) => doc.trim_end_matches('>').trim().to_string(),
                    None => l.to_string(),
                })
                .collect()
        })
    };
    for line in index.lines() {
        if line.starts_with(".. toctree::") {
            if let Some(docs) = finish(&block) {
                return docs;
            }
            block.clear();
            in_toctree = true;
            continue;
        }
        if !in_toctree {
            continue;
        }
        // The first unindented, non-blank line ends the directive.
        if !line.trim().is_empty() && !line.starts_with(' ') {
            if let Some(docs) = finish(&block) {
                return docs;
            }
            block.clear();
            in_toctree = false;
            continue;
        }
        block.push(line);
    }
    finish(&block).unwrap_or_default()
}

/// Whether a line is a heading adornment: one punctuation character
/// repeated, at least three times.
fn adornment(line: &str) -> Option<char> {
    let mut chars = line.chars();
    let first = chars.next()?;
    let usable = matches!(first, '=' | '-' | '~' | '^' | '*' | '"' | '+' | '#');
    (usable && line.len() >= 3 && chars.all(|c| c == first)).then_some(first)
}

/// The numbered headings of one chapter file, whose first top-level
/// heading is chapter `first`.
///
/// Sphinx gives each adornment style a level in the order the styles
/// first appear in the file, and an overlined style is distinct from
/// the same character used as underline only. Every top-level
/// heading in the file takes a chapter number of its own.
pub fn chapter_sections(first: usize, doc: &str, text: &str) -> Vec<Section> {
    let lines: Vec<&str> = text.lines().collect();
    let mut styles: Vec<(char, bool)> = Vec::new();
    let mut counters: Vec<usize> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    let mut found = Vec::new();
    let mut i = 0;
    while i + 1 < lines.len() {
        let (line, next) = (lines[i], lines[i + 1]);
        if let Some(name) = line
            .strip_prefix(".. _")
            .and_then(|rest| rest.trim_end().strip_suffix(':'))
        {
            labels.push(name.to_string());
            i += 1;
            continue;
        }
        if let Some(ch) = adornment(next)
            && !line.trim().is_empty()
            && adornment(line).is_none()
        {
            let overlined = i > 0 && lines[i - 1] == next;
            let style = (ch, overlined);
            let level = match styles.iter().position(|s| *s == style) {
                Some(at) => at,
                None => {
                    styles.push(style);
                    styles.len() - 1
                }
            };
            counters.truncate(level + 1);
            counters.resize(level + 1, 0);
            counters[level] += 1;
            let mut number = (first + counters[0] - 1).to_string();
            for n in &counters[1..] {
                number.push_str(&format!(".{n}"));
            }
            found.push(Section {
                number,
                doc: doc.to_string(),
                labels: std::mem::take(&mut labels),
                title: line.trim().replace("``", ""),
            });
            i += 2;
            continue;
        }
        // Prose between a label and a heading detaches the label; an
        // overline is part of the heading and does not.
        if !line.trim().is_empty() && adornment(line).is_none() {
            labels.clear();
        }
        i += 1;
    }
    found
}

/// Every numbered section of the manual whose sources are in `root`.
pub fn sections(root: &Path) -> Result<Vec<Section>, String> {
    let index_path = root.join("index.rst");
    let index = std::fs::read_to_string(&index_path)
        .map_err(|e| format!("{}: {e}", index_path.display()))?;
    let mut found = Vec::new();
    let mut next_chapter = 1;
    for doc in chapters(&index) {
        let path = root.join(format!("{doc}.rst"));
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let these = chapter_sections(next_chapter, &doc, &text);
        next_chapter += these.iter().filter(|s| !s.number.contains('.')).count();
        found.extend(these);
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "\
====================
Debian Policy Manual
====================

.. toctree::
   :maxdepth: 4
   :numbered:

   ch-scope
   ch-source

.. toctree::
   :caption: Appendices
   :numbered:

   ap-pkg-scope
";

    const SOURCE: &str = "\
.. _s-source-packages:

Source packages
===============

Text.

.. _s-standardsversion:

Standards conformance
---------------------

.. _s-dpkgchangelog:

Debian changelog: ``debian/changelog``
--------------------------------------

Some prose, then a sub-section without a label.

Changelog format
~~~~~~~~~~~~~~~~

Back up a level
---------------
";

    #[test]
    fn the_first_numbered_toctree_lists_the_chapters() {
        assert_eq!(chapters(INDEX), ["ch-scope", "ch-source"]);
    }

    #[test]
    fn sections_are_numbered_by_chapter_and_adornment_level() {
        let found = chapter_sections(4, "ch-source", SOURCE);
        let got: Vec<(&str, String, &str)> = found
            .iter()
            .map(|s| (s.number.as_str(), s.labels.join(" "), s.title.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("4", "s-source-packages".into(), "Source packages"),
                ("4.1", "s-standardsversion".into(), "Standards conformance"),
                (
                    "4.2",
                    "s-dpkgchangelog".into(),
                    "Debian changelog: debian/changelog"
                ),
                ("4.2.1", String::new(), "Changelog format"),
                ("4.3", String::new(), "Back up a level"),
            ]
        );
    }

    #[test]
    fn an_overlined_title_is_a_level_of_its_own() {
        let text = "\
=====
Title
=====

Section
=======
";
        let found = chapter_sections(1, "x", text);
        let numbers: Vec<&str> = found.iter().map(|s| s.number.as_str()).collect();
        assert_eq!(numbers, ["1", "1.1"]);
    }

    #[test]
    fn a_label_does_not_carry_past_prose_to_a_later_heading() {
        let text = "\
Chapter
=======

.. _stray:

A paragraph that is not a heading.

Section
-------
";
        let found = chapter_sections(1, "x", text);
        assert!(found[1].labels.is_empty());
    }

    #[test]
    fn an_overlined_heading_keeps_the_label_above_it() {
        let text = "\
.. _s-foo:

=====
Title
=====
";
        let found = chapter_sections(1, "x", text);
        assert_eq!(found[0].labels, ["s-foo"]);
    }

    #[test]
    fn stacked_labels_all_name_the_heading() {
        let text = "\
.. _old-name:
.. _new-name:

Title
=====
";
        let found = chapter_sections(1, "x", text);
        assert_eq!(found[0].labels, ["old-name", "new-name"]);
    }

    #[test]
    fn a_second_top_level_heading_is_the_next_chapter() {
        let text = "\
One
===

Sub
---

Two
===

Sub
---
";
        let found = chapter_sections(3, "x", text);
        let numbers: Vec<&str> = found.iter().map(|s| s.number.as_str()).collect();
        assert_eq!(numbers, ["3", "3.1", "4", "4.1"]);
    }

    #[test]
    fn an_unnumbered_toctree_is_skipped_and_titled_entries_name_their_doc() {
        let index = "\
.. toctree::
   :maxdepth: 1

   front-matter

.. toctree::
   :numbered:

   ch-one
   Chapter two <ch-two>
";
        assert_eq!(chapters(index), ["ch-one", "ch-two"]);
        assert!(chapters("no toctree here\n").is_empty());
    }

    #[test]
    fn a_manual_is_read_from_its_index_and_chapter_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.rst"), INDEX).unwrap();
        std::fs::write(
            dir.path().join("ch-scope.rst"),
            "Scope\n=====\n\nMore scope\n==========\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("ch-source.rst"), SOURCE).unwrap();
        let found = sections(dir.path()).unwrap();
        // ch-scope holds two top-level headings, so ch-source is 3.
        assert_eq!(found[0].number, "1");
        assert_eq!(found[1].number, "2");
        assert_eq!(found[2].number, "3");
        assert_eq!(found[2].doc, "ch-source");
    }

    #[test]
    fn a_missing_chapter_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.rst"), INDEX).unwrap();
        let err = sections(dir.path()).unwrap_err();
        assert!(err.contains("ch-scope.rst"), "{err}");
    }
}
