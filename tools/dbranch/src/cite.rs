// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where each check's rule comes from.
//!
//! Every finding `dbranch check` reports names the document behind
//! its rule: a Debian Policy section, a Developer's Reference
//! section, a DEP, a man page or a manual. The Debian documents lag
//! behind practice (the Developer's Reference still recommends
//! deborphan, removed in 2024), so when dbranch and a document
//! disagree, the citation shows which one moved, and the gap becomes
//! a bug report or a documentation patch rather than silent drift.
//!
//! A check that no document backs is cited as dbranch's own
//! practice, with the reason, so those gaps are visible too. Where
//! dbranch knowingly departs from the document it cites, the
//! citation says so in `diverges`.
//!
//! The table is keyed by the check's slug. A test fails when a check
//! that can be reported has no entry, and an `#[ignore]`d test checks
//! the Policy and Developer's Reference citations against local
//! clones of both (see DEVELOPMENT.md, "Checks cite their sources").

use serde::Serialize;

/// The document a rule comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A section of the Debian Policy Manual.
    Policy {
        /// The section number Sphinx gives it.
        section: &'static str,
        /// Its heading, without reStructuredText markup.
        title: &'static str,
        /// The chapter file, which names the HTML page.
        doc: &'static str,
        /// The section's label, which is the HTML anchor.
        label: &'static str,
    },
    /// A section of the Developer's Reference.
    DevRef {
        section: &'static str,
        title: &'static str,
        doc: &'static str,
        label: &'static str,
    },
    /// A Debian Enhancement Proposal.
    Dep { number: u16, title: &'static str },
    /// A man page, optionally narrowed to where in it to look.
    Man {
        page: &'static str,
        section: u8,
        under: Option<&'static str>,
    },
    /// Any other manual, by the part that applies, with a public
    /// address when it has one.
    Manual {
        title: &'static str,
        part: &'static str,
        url: Option<&'static str>,
    },
    /// No document backs this; dbranch's own practice, and why.
    Practice { why: &'static str },
}

/// One source for a check's rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Citation {
    pub source: Source,
    /// Where dbranch knowingly departs from the document, and the bug
    /// or patch that should settle it.
    pub diverges: Option<&'static str>,
}

impl Source {
    /// The citation as a reader sees it, without the address.
    pub fn text(&self) -> String {
        match self {
            Source::Policy { section, title, .. } => {
                format!("Debian Policy {section}, {title}")
            }
            Source::DevRef { section, title, .. } => {
                format!("Developer's Reference {section}, {title}")
            }
            Source::Dep { number, title } => format!("DEP-{number}, {title}"),
            Source::Man {
                page,
                section,
                under,
            } => match under {
                Some(under) => format!("{page}({section}), under {under}"),
                None => format!("{page}({section})"),
            },
            Source::Manual { title, part, .. } => format!("{title}, {part}"),
            Source::Practice { why } => format!("dbranch's own practice: {why}"),
        }
    }

    /// Where to read it, when it can be linked.
    pub fn url(&self) -> Option<String> {
        match self {
            Source::Policy { doc, label, .. } => Some(format!(
                "https://www.debian.org/doc/debian-policy/{doc}.html#{label}"
            )),
            Source::DevRef { doc, label, .. } => Some(format!(
                "https://www.debian.org/doc/manuals/developers-reference/{doc}.html#{label}"
            )),
            Source::Dep { number, .. } => Some(format!(
                "https://dep-team.pages.debian.net/deps/dep{number}/"
            )),
            Source::Man { page, section, .. } => Some(format!(
                "https://manpages.debian.org/unstable/{page}.{section}"
            )),
            Source::Manual { url, .. } => url.map(str::to_string),
            Source::Practice { .. } => None,
        }
    }
}

/// How a citation reads in `--json`: the text and the address, so a
/// consumer need not know every kind of source.
#[derive(Serialize)]
struct CitationJson {
    cite: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diverges: Option<&'static str>,
}

impl Serialize for Citation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        CitationJson {
            cite: self.source.text(),
            url: self.source.url(),
            diverges: self.diverges,
        }
        .serialize(serializer)
    }
}

/// A citation with no known divergence.
const fn cite(source: Source) -> Citation {
    Citation {
        source,
        diverges: None,
    }
}

const POLICY_CHANGELOG: Citation = cite(Source::Policy {
    section: "4.4",
    title: "Debian changelog: debian/changelog",
    doc: "ch-source",
    label: "s-dpkgchangelog",
});

const POLICY_MAN_PAGES: Citation = cite(Source::Policy {
    section: "12.1",
    title: "Manual pages",
    doc: "ch-docs",
    label: "s12.1",
});

const DEVREF_TESTING: Citation = cite(Source::DevRef {
    section: "5.3",
    title: "Testing the package",
    doc: "pkgs",
    label: "sanitycheck",
});

const DEVREF_PATCHES: Citation = cite(Source::DevRef {
    section: "6.8",
    title: "Best practices for debian/patches",
    doc: "best-pkging-practices",
    label: "bpp-debian-patches",
});

const DEP3: Citation = cite(Source::Dep {
    number: 3,
    title: "Patch Tagging Guidelines",
});

const DEP8: Citation = cite(Source::Dep {
    number: 8,
    title: "automatic as-installed package testing",
});

const GBP_PQ: Citation = cite(Source::Man {
    page: "gbp-pq",
    section: 1,
    under: None,
});

const DPKG_SOURCE_QUILT: Citation = cite(Source::Man {
    page: "dpkg-source",
    section: 1,
    under: Some("Format: 3.0 (quilt)"),
});

const DPKG_SOURCE_UNAPPLY: Citation = cite(Source::Man {
    page: "dpkg-source",
    section: 1,
    under: Some("--unapply-patches"),
});

/// What the smoke test stands on: Developer's Reference 5.3 asks for
/// the installed software to be tried, and nothing more specific.
const SMOKE: &[Citation] = &[DEVREF_TESTING];

/// Every check's sources, by slug. Sorted by slug.
const TABLE: &[(&str, &[Citation])] = &[
    ("binary-does-not-run", SMOKE),
    ("binary-not-run", SMOKE),
    (
        "changelog-distribution-placeholder",
        &[cite(Source::Practice {
            why: "debcargo marks an entry still in progress with this \
                  distribution, and debcargo-conf's release.sh replaces it \
                  at release; nothing documents it beyond those scripts",
        })],
    ),
    (
        "gbp-branch-missing",
        &[
            cite(Source::Man {
                page: "gbp.conf",
                section: 5,
                under: None,
            }),
            cite(Source::Practice {
                why: "gbp falls back silently when a branch it is \
                      configured with is missing, so nothing else reports it",
            }),
        ],
    ),
    (
        "installed-copy-differs",
        &[
            DEVREF_TESTING,
            cite(Source::Practice {
                why: "a test of an installed program only says something \
                      about the package that installed it",
            }),
        ],
    ),
    ("no-program-test", &[DEP8, DEVREF_TESTING]),
    ("on-patch-queue-branch", &[GBP_PQ]),
    ("patch-does-not-apply", &[DPKG_SOURCE_QUILT, DEVREF_PATCHES]),
    (
        "patch-header-not-dep3",
        &[
            DEP3,
            DEVREF_PATCHES,
            cite(Source::Man {
                page: "git-interpret-trailers",
                section: 1,
                under: None,
            }),
        ],
    ),
    ("patch-not-exported", &[GBP_PQ, DEVREF_PATCHES]),
    ("patches-applied-in-tree", &[DPKG_SOURCE_UNAPPLY]),
    ("patches-left-applied-after-build", &[DPKG_SOURCE_UNAPPLY]),
    (
        "pc-directory-not-ignored",
        &[cite(Source::Practice {
            why: "quilt's working directory is not packaging, and git-\
                  buildpackage's own example hook ignores it the same way",
        })],
    ),
    (
        "redundant-relationship-substvar",
        &[cite(Source::Man {
            page: "debhelper-compat-upgrade-checklist",
            section: 7,
            under: Some("v14"),
        })],
    ),
    (
        "shipped-but-not-installed",
        &[
            POLICY_MAN_PAGES,
            cite(Source::Practice {
                why: "a file the source ships for installing is \
                      expected to reach a package; Policy asks this only \
                      of man pages",
            }),
        ],
    ),
    (
        "snapshot-version-released",
        &[cite(Source::Man {
            page: "gbp-dch",
            section: 1,
            under: Some("--snapshot"),
        })],
    ),
    (
        "stale-build",
        &[cite(Source::Practice {
            why: "checking a package built from older packaging \
                  answers for that packaging, not this one",
        })],
    ),
    ("stale-changelog-version", &[POLICY_CHANGELOG]),
    (
        "stale-version-mention",
        &[cite(Source::Practice {
            why: "a comment naming an older release is usually the \
                  first sign the file around it was not updated either",
        })],
    ),
    (
        "unscoped-lintian-override",
        // The manual ships with lintian, and there is no public copy
        // to link to, so the citation says where it is installed.
        &[cite(Source::Manual {
            title: "Lintian User's Manual",
            part: "Format of override files \
                   (/usr/share/doc/lintian/lintian.html)",
            url: None,
        })],
    ),
    (
        "upstream-change-on-packaging-branch",
        &[DPKG_SOURCE_QUILT, DEVREF_PATCHES, GBP_PQ],
    ),
];

/// The sources of a check's rule, or none for a slug the table does
/// not know. A test keeps every reportable slug in the table.
pub fn for_check(check: &str) -> &'static [Citation] {
    TABLE
        .binary_search_by(|(slug, _)| slug.cmp(&check))
        .map_or(&[], |at| TABLE[at].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slugs `check.rs` can report, read from its source: the
    /// first argument of every `Finding::new` outside its tests.
    fn reported_slugs() -> Vec<String> {
        let source = include_str!("check.rs");
        let code: String = source
            .split("#[cfg(test)]\nmod tests")
            .next()
            .expect("check.rs has code")
            .lines()
            // A slug quoted in a comment is not one that is reported.
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut slugs = Vec::new();
        for (at, _) in code.match_indices("Finding::new(") {
            let rest = &code[at + "Finding::new(".len()..];
            let rest = rest.trim_start();
            if let Some(quoted) = rest.strip_prefix('"')
                && let Some(end) = quoted.find('"')
            {
                slugs.push(quoted[..end].to_string());
            }
        }
        slugs.sort();
        slugs.dedup();
        slugs
    }

    #[test]
    fn every_check_that_can_be_reported_cites_a_source() {
        let slugs = reported_slugs();
        assert!(slugs.len() >= 20, "found only {slugs:?}");
        let missing: Vec<&String> = slugs.iter().filter(|s| for_check(s).is_empty()).collect();
        assert!(missing.is_empty(), "no citation for {missing:?}");
    }

    #[test]
    fn the_table_cites_nothing_that_cannot_be_reported() {
        let slugs = reported_slugs();
        let stale: Vec<&str> = TABLE
            .iter()
            .map(|(slug, _)| *slug)
            .filter(|slug| !slugs.iter().any(|s| s == slug))
            .collect();
        assert!(stale.is_empty(), "cited but never reported: {stale:?}");
    }

    #[test]
    fn the_table_is_sorted_so_it_can_be_searched() {
        assert!(TABLE.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn an_unknown_check_has_no_sources() {
        assert!(for_check("no-such-check").is_empty());
    }

    #[test]
    fn each_kind_of_source_reads_and_links() {
        assert_eq!(
            POLICY_CHANGELOG.source.text(),
            "Debian Policy 4.4, Debian changelog: debian/changelog"
        );
        assert_eq!(
            POLICY_CHANGELOG.source.url().unwrap(),
            "https://www.debian.org/doc/debian-policy/ch-source.html#s-dpkgchangelog"
        );
        assert_eq!(
            DEVREF_PATCHES.source.url().unwrap(),
            "https://www.debian.org/doc/manuals/developers-reference/\
             best-pkging-practices.html#bpp-debian-patches"
        );
        assert_eq!(DEP3.source.text(), "DEP-3, Patch Tagging Guidelines");
        assert_eq!(
            DEP3.source.url().unwrap(),
            "https://dep-team.pages.debian.net/deps/dep3/"
        );
        assert_eq!(
            DPKG_SOURCE_UNAPPLY.source.text(),
            "dpkg-source(1), under --unapply-patches"
        );
        assert_eq!(GBP_PQ.source.text(), "gbp-pq(1)");
        assert_eq!(
            GBP_PQ.source.url().unwrap(),
            "https://manpages.debian.org/unstable/gbp-pq.1"
        );
        let manual = for_check("unscoped-lintian-override")[0].source;
        assert_eq!(
            manual.text(),
            "Lintian User's Manual, Format of override files \
             (/usr/share/doc/lintian/lintian.html)"
        );
        assert_eq!(manual.url(), None, "no link that only works locally");
        let practice = Source::Practice { why: "because" };
        assert_eq!(practice.text(), "dbranch's own practice: because");
        assert_eq!(practice.url(), None);
    }

    #[test]
    fn a_citation_serializes_as_its_text_and_address() {
        let json = serde_json::to_value(POLICY_CHANGELOG).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "cite": "Debian Policy 4.4, Debian changelog: debian/changelog",
                "url": "https://www.debian.org/doc/debian-policy/ch-source.html#s-dpkgchangelog",
            })
        );
        let diverging = Citation {
            source: Source::Practice { why: "because" },
            diverges: Some("see #1"),
        };
        let json = serde_json::to_value(diverging).unwrap();
        assert_eq!(json["diverges"], "see #1");
        assert!(json.get("url").is_none(), "{json}");
    }

    /// What a Policy citation claims: number, title, chapter, label.
    type Claim = (&'static str, &'static str, &'static str, &'static str);

    fn policy_claim(source: &Source) -> Option<Claim> {
        match *source {
            Source::Policy {
                section,
                title,
                doc,
                label,
            } => Some((section, title, doc, label)),
            _ => None,
        }
    }

    fn devref_claim(source: &Source) -> Option<Claim> {
        match *source {
            Source::DevRef {
                section,
                title,
                doc,
                label,
            } => Some((section, title, doc, label)),
            _ => None,
        }
    }

    /// The citations in `table` that `sections` no longer bears out,
    /// matched by label: a label that is gone, or one whose number,
    /// title or chapter moved.
    fn mismatches(
        table: &[(&str, &[Citation])],
        sections: &[crate::rstdoc::Section],
        claim: fn(&Source) -> Option<Claim>,
    ) -> Vec<String> {
        let mut wrong = Vec::new();
        for (slug, citations) in table {
            for citation in *citations {
                let Some((number, title, doc, label)) = claim(&citation.source) else {
                    continue;
                };
                match sections
                    .iter()
                    .find(|s| s.labels.iter().any(|l| l == label))
                {
                    None => wrong.push(format!("{slug}: no section is labelled {label}")),
                    Some(s)
                        if (s.number.as_str(), s.title.as_str(), s.doc.as_str())
                            != (number, title, doc) =>
                    {
                        wrong.push(format!(
                            "{slug}: cites {number} {title:?} in {doc}, \
                             but {label} is now {} {:?} in {}",
                            s.number, s.title, s.doc
                        ))
                    }
                    Some(_) => {}
                }
            }
        }
        wrong
    }

    /// The citations no clone can answer for, listed once each.
    fn unverified(table: &[(&str, &[Citation])]) -> Vec<String> {
        let mut texts: Vec<String> = table
            .iter()
            .flat_map(|(_, cs)| cs.iter())
            .filter(|c| policy_claim(&c.source).is_none() && devref_claim(&c.source).is_none())
            .filter(|c| !matches!(c.source, Source::Practice { .. }))
            .map(|c| c.source.text())
            .collect();
        texts.sort();
        texts.dedup();
        texts
    }

    fn section(number: &str, doc: &str, label: &str, title: &str) -> crate::rstdoc::Section {
        crate::rstdoc::Section {
            number: number.into(),
            doc: doc.into(),
            labels: vec![label.into()],
            title: title.into(),
        }
    }

    #[test]
    fn a_citation_the_manual_still_bears_out_is_not_reported() {
        let table: &[(&str, &[Citation])] = &[("c", &[POLICY_CHANGELOG])];
        let sections = [section(
            "4.4",
            "ch-source",
            "s-dpkgchangelog",
            "Debian changelog: debian/changelog",
        )];
        assert!(mismatches(table, &sections, policy_claim).is_empty());
    }

    #[test]
    fn a_renumbered_or_vanished_section_is_reported() {
        let table: &[(&str, &[Citation])] = &[("c", &[POLICY_CHANGELOG, DEVREF_PATCHES])];
        let moved = [section(
            "4.5",
            "ch-source",
            "s-dpkgchangelog",
            "Debian changelog: debian/changelog",
        )];
        let wrong = mismatches(table, &moved, policy_claim);
        assert_eq!(wrong.len(), 1, "{wrong:?}");
        assert!(wrong[0].contains("is now 4.5"), "{wrong:?}");
        // Developer's Reference citations are checked against their
        // own manual, where this label is missing.
        let wrong = mismatches(table, &moved, devref_claim);
        assert_eq!(wrong, ["c: no section is labelled bpp-debian-patches"]);
    }

    #[test]
    fn what_no_clone_can_check_is_listed_once_without_practice() {
        let listed = unverified(TABLE);
        assert!(listed.contains(&"gbp-pq(1)".to_string()), "{listed:?}");
        assert_eq!(listed.iter().filter(|t| *t == "gbp-pq(1)").count(), 1);
        assert!(
            listed
                .iter()
                .all(|t| !t.starts_with("Debian Policy") && !t.starts_with("dbranch's own")),
            "{listed:?}"
        );
    }

    /// Check one manual's citations against the clone `var` names.
    fn verify_against(var: &str, claim: fn(&Source) -> Option<Claim>) -> Vec<String> {
        let Ok(root) = std::env::var(var) else {
            return vec![format!("{var} is not set, so these were not checked")];
        };
        let sections = crate::rstdoc::sections(std::path::Path::new(&root))
            .unwrap_or_else(|e| panic!("{var}={root}: {e}"));
        mismatches(TABLE, &sections, claim)
    }

    /// The Policy and Developer's Reference citations, against local
    /// clones named by `DBRANCH_POLICY_DIR` (the clone's `policy/`)
    /// and `DBRANCH_DEVREF_DIR` (the clone's `source/`). Ignored by
    /// default: a packaging build has no such clones. Run it through
    /// `tools/dbranch/scripts/verify-citations.sh`, which also checks
    /// the clones are current.
    #[test]
    #[ignore = "needs local clones of Debian Policy and the Developer's Reference"]
    fn citations_match_the_local_documentation_clones() {
        let mut wrong = verify_against("DBRANCH_POLICY_DIR", policy_claim);
        wrong.extend(verify_against("DBRANCH_DEVREF_DIR", devref_claim));
        // What a clone cannot answer for is listed, not fetched.
        println!("not checked against a clone:");
        for text in unverified(TABLE) {
            println!("  {text}");
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }
}
