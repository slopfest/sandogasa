// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Checking `debian/copyright` against the source, and the placeholders
//! the tools that write it leave behind.
//!
//! `licenserecon` compares the file against `licensecheck` and exits
//! non-zero when they disagree about a licence. `cme update
//! dpkg-copyright` reconciles an existing file; `decopy` writes one
//! from scratch. Neither produces something uploadable unreviewed:
//! both leave placeholders where they could not tell, and
//! `licenserecon` passes them, because a placeholder is not a licence
//! disagreement.

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

#[cfg(test)]
mod tests {
    use super::*;

    /// The two the generators actually write, measured: cme leaves
    /// `Copyright: no-info-found` for a file it could not attribute,
    /// decopy leaves `Source: TODO`.
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
}
