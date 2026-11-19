// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A check's report, as Markdown for a comment (Bodhi, GitLab) and as
//! colored text for a terminal: one renderer, so every tool presents a
//! check the same way. A tool adds its own sections around these.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use sandogasa_depfilter::provide_name;

use crate::arrival::Arrival;
use crate::report::{BreakKind, ChangedProvide, PackageChange, RevDepResult, UnsatisfiedDep};

/// The headings of the sections [`to_terminal`] colors, as
/// [`findings`] writes them (followed by the count).
pub const REMOVED: &str = "Removed Provides";
pub const INSTALLABILITY: &str = "Installability issues";
pub const WOULD_BREAK: &str = "Reverse dependencies that would break";

/// In a summary, how many list items to show before collapsing the
/// rest into "… and N more".
pub const LIST_CAP: usize = 15;

/// What a report shows.
#[derive(Debug, Clone, Copy)]
pub struct Options<'a> {
    /// Every list in full, and the sections a summary leaves out.
    pub detailed: bool,
    /// How to see what a summary left out (`use --detailed`), shown
    /// after "… and N more"; empty to say nothing.
    pub more: &'a str,
}

impl Options<'_> {
    /// The hint after a capped list, with its space and parentheses.
    fn hint(&self) -> String {
        if self.more.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.more)
        }
    }
}

/// The parts of a check's result the shared sections show.
#[derive(Debug, Clone, Copy)]
pub struct Findings<'a> {
    pub updated_packages: &'a [String],
    pub changes: &'a [PackageChange],
    pub changed_provides: &'a [ChangedProvide],
    pub installability_issues: &'a [UnsatisfiedDep],
    pub reverse_deps: &'a BTreeMap<String, RevDepResult>,
}

/// Write `lines` to `o`, capping at [`LIST_CAP`] unless `opts.detailed`, with
/// a "… and N more" footer when truncated.
pub fn write_capped(o: &mut String, lines: Vec<String>, opts: &Options) {
    let cap = if opts.detailed {
        lines.len()
    } else {
        LIST_CAP.min(lines.len())
    };
    for line in &lines[..cap] {
        let _ = writeln!(o, "{line}");
    }
    if cap < lines.len() {
        let _ = writeln!(o, "- … and {} more{}", lines.len() - cap, opts.hint());
    }
}

/// The "## Packages" summary: a count line plus updated packages grouped
/// by their `old → new` version transition (biggest groups first), and
/// the newly-introduced packages. Group bodies and the new-package list
/// are capped unless `detailed`.
pub fn package_summary(
    updated_packages: &[String],
    changes: &[PackageChange],
    opts: &Options,
) -> String {
    let detailed = opts.detailed;
    let mut o = String::new();
    let total = updated_packages.len();
    let _ = writeln!(o, "## Packages\n");

    // Without per-package change data, fall back to a plain count/list.
    if changes.is_empty() {
        let _ = writeln!(o, "**{total}** package(s).");
        if total > 0 && (detailed || total <= LIST_CAP) {
            let _ = writeln!(o, "\n{}", updated_packages.join(", "));
        }
        let _ = writeln!(o);
        return o;
    }

    let new_pkgs: Vec<&str> = changes
        .iter()
        .filter(|c| c.old.is_none())
        .map(|c| c.package.as_str())
        .collect();
    let updated_n = total.saturating_sub(new_pkgs.len());
    let _ = writeln!(
        o,
        "**{total}** package(s): {updated_n} updated, {} new\n",
        new_pkgs.len()
    );

    // Group version updates by (old → new), biggest groups first.
    let mut buckets: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
    for c in changes {
        if let Some(old) = &c.old {
            buckets
                .entry((old.as_str(), c.new.as_str()))
                .or_default()
                .push(&c.package);
        }
    }
    let mut sorted: Vec<_> = buckets.into_iter().collect();
    // Biggest groups first, then alphabetically by the group's first
    // package name (each bucket's packages are already in name order
    // because `changes` is built from a sorted package set).
    sorted.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then(a.1.first().cmp(&b.1.first()))
    });
    let shown = if detailed {
        sorted.len()
    } else {
        sorted.len().min(LIST_CAP)
    };
    for ((old, new), pkgs) in sorted.iter().take(shown) {
        // Name the packages for small groups (or in detailed); just
        // count the big ones.
        if detailed || pkgs.len() <= 3 {
            let _ = writeln!(o, "- `{old}` → `{new}` ({})", pkgs.join(", "));
        } else {
            let _ = writeln!(o, "- `{old}` → `{new}` ({} packages)", pkgs.len());
        }
    }
    if sorted.len() > shown {
        let _ = writeln!(
            o,
            "- … and {} more version transition(s){}",
            sorted.len() - shown,
            opts.hint()
        );
    }

    if !new_pkgs.is_empty() {
        let cap = if detailed {
            new_pkgs.len()
        } else {
            LIST_CAP.min(new_pkgs.len())
        };
        let shown_new = new_pkgs[..cap].join(", ");
        if cap < new_pkgs.len() {
            let _ = writeln!(
                o,
                "\n**New ({}):** {shown_new}, …{}",
                new_pkgs.len(),
                opts.hint()
            );
        } else {
            let _ = writeln!(o, "\n**New ({}):** {shown_new}", new_pkgs.len());
        }
    }
    let _ = writeln!(o);
    o
}

/// The analysis of an update as Markdown: the counts, then what the
/// update drops, which of its own Requires do not resolve, and the
/// reverse dependencies that would break, each list capped unless
/// `opts.detailed`; with it, also the Provides at another version and
/// the reverse dependencies that are fine. `nothing` is what to say when
/// no package was examined at all, which must not read as a clean bill.
pub fn findings(f: &Findings, opts: &Options, nothing: &str) -> String {
    let mut o = String::new();
    let updated: Vec<&ChangedProvide> = f
        .changed_provides
        .iter()
        .filter(|c| c.is_updated())
        .collect();
    let removed: Vec<&ChangedProvide> = f
        .changed_provides
        .iter()
        .filter(|c| !c.is_updated())
        .collect();
    let broken: Vec<(&String, &RevDepResult)> = f
        .reverse_deps
        .iter()
        .filter(|(_, r)| r.status != "ok")
        .collect();

    // Analysis counts (always).
    let _ = writeln!(o, "## Analysis\n");
    let _ = writeln!(
        o,
        "- **Changed Provides:** {} ({} updated, {} removed)",
        f.changed_provides.len(),
        updated.len(),
        removed.len(),
    );
    let _ = writeln!(
        o,
        "- **Installability issues:** {}",
        f.installability_issues.len()
    );
    if !f.reverse_deps.is_empty() {
        // Split the damage by kind: a package fails to install (FTI)
        // when a binary Requires breaks, and fails to build (FTBFS)
        // when a BuildRequires does.
        let fti = broken
            .iter()
            .filter(|(_, r)| r.issues.iter().any(|i| i.kind == BreakKind::Fti))
            .count();
        let ftbfs = broken
            .iter()
            .filter(|(_, r)| r.issues.iter().any(|i| i.kind == BreakKind::Ftbfs))
            .count();
        let breakdown = if broken.is_empty() {
            String::new()
        } else {
            format!(" ({fti} FTI, {ftbfs} FTBFS)")
        };
        let _ = writeln!(
            o,
            "- **Reverse dependencies:** {} checked, {} would break{breakdown}",
            f.reverse_deps.len(),
            broken.len(),
        );
    }
    let _ = writeln!(o);

    // An update with no packages was not examined, so there is no
    // verdict to give: a mistyped side tag, or one whose builds are not
    // tagged yet, would otherwise read as a clean bill.
    let examined_nothing = f.updated_packages.is_empty()
        && f.changes.is_empty()
        && f.changed_provides.is_empty()
        && f.installability_issues.is_empty()
        && f.reverse_deps.is_empty();
    if examined_nothing {
        let _ = writeln!(o, "{nothing}");
        return o;
    }

    let clean = removed.is_empty() && f.installability_issues.is_empty() && broken.is_empty();
    if clean && !opts.detailed {
        let _ = writeln!(o, "No breakage expected.");
        return o;
    }

    // Removed Provides — the breakage signal; always shown (capped).
    if !removed.is_empty() {
        let _ = writeln!(o, "## {REMOVED} ({})\n", removed.len());
        let lines: Vec<String> = removed.iter().map(|c| format!("- `{}`", c.old)).collect();
        write_capped(&mut o, lines, opts);
        let _ = writeln!(o);
    }
    // Installability issues — actionable; always shown (capped).
    if !f.installability_issues.is_empty() {
        let _ = writeln!(
            o,
            "## {INSTALLABILITY} ({})\n",
            f.installability_issues.len()
        );
        let lines: Vec<String> = f
            .installability_issues
            .iter()
            .map(|i| {
                if i.unresolved.is_empty() {
                    format!("- **{}**: `{}`", i.package, i.dep)
                } else {
                    format!(
                        "- **{}**: `{}` (unresolved: {})",
                        i.package,
                        i.dep,
                        i.unresolved.join(", ")
                    )
                }
            })
            .collect();
        write_capped(&mut o, lines, opts);
        let _ = writeln!(o);
    }
    // Reverse deps that would break — always shown, in full.
    if !broken.is_empty() {
        let _ = writeln!(o, "## {WOULD_BREAK} ({})\n", broken.len());
        for (pkg, result) in &broken {
            match result.arrival.as_ref().map(|a| a.arrival) {
                Some(Arrival::NewPackage) => {
                    let _ = writeln!(o, "- **{pkg}** (new since the side tag opened)");
                }
                Some(Arrival::Rebuilt) => {
                    let _ = writeln!(o, "- **{pkg}** (rebuilt since the side tag opened)");
                }
                Some(Arrival::PreExisting) => {
                    let _ = writeln!(o, "- **{pkg}** (already there when the side tag opened)");
                }
                None => {
                    let _ = writeln!(o, "- **{pkg}**");
                }
            }
            for issue in &result.issues {
                let _ = writeln!(
                    o,
                    "  - [{}] `{}` (changed: `{}`)",
                    issue.kind.label(),
                    issue.dep,
                    issue.changed_provide
                );
            }
        }
        let _ = writeln!(o);
    }

    // Detailed-only sections: the full updated-Provides list and the OK
    // reverse deps (the bulky, non-actionable parts).
    if opts.detailed && !updated.is_empty() {
        let _ = writeln!(o, "## Updated Provides ({})\n", updated.len());
        for c in &updated {
            let name = provide_name(&c.old);
            let old_ver = c
                .old
                .strip_prefix(name)
                .unwrap_or("")
                .trim()
                .trim_start_matches("= ");
            let new_ver = c
                .new
                .as_deref()
                .and_then(|n| n.strip_prefix(name))
                .unwrap_or("")
                .trim()
                .trim_start_matches("= ");
            let _ = writeln!(o, "- `{name}` ({old_ver} → {new_ver})");
        }
        let _ = writeln!(o);
    }
    if opts.detailed {
        let ok: Vec<&String> = f
            .reverse_deps
            .iter()
            .filter(|(_, r)| r.status == "ok")
            .map(|(p, _)| p)
            .collect();
        if !ok.is_empty() {
            let _ = writeln!(o, "## Reverse dependencies OK ({})\n", ok.len());
            for pkg in ok {
                let _ = writeln!(o, "- {pkg}");
            }
            let _ = writeln!(o);
        }
    }

    if clean {
        let _ = writeln!(o, "No breakage expected.");
    }
    o
}

/// `text` without Markdown's inline markup: `**` and backticks, and
/// the `<sub>` tags a footer uses.
fn plain(text: &str) -> String {
    text.replace("**", "")
        .replace('`', "")
        .replace("<sub>", "")
        .replace("</sub>", "")
}

/// A report rendered as Markdown ([`findings`], with a tool's own
/// sections around it) as a terminal shows it with `color`: headings
/// bold, warnings yellow, a blocking note bold red, what the update
/// drops yellow (red for a shared library's soname), Requires that do
/// not resolve and reverse dependencies that would break red (the
/// package bold), "No breakage expected." green, and the Markdown
/// markup left out. Without color it is the Markdown as it is, so
/// piped output and posted comments read the same.
pub fn to_terminal(markdown: &str, color: bool) -> String {
    use sandogasa_cli::style::{BOLD, GREEN, RED, YELLOW, paint};
    if !color {
        return markdown.to_string();
    }
    let bad = format!("{BOLD}{RED}");
    let mut out = String::new();
    // The section a list item belongs to, and the style of the note a
    // quoted line belongs to; both end where their block does.
    let mut section = String::new();
    let mut quote_style: Option<&str> = None;
    for line in markdown.lines() {
        if !line.starts_with("> ") {
            quote_style = None;
        }
        let shown =
            if let Some(heading) = line.strip_prefix("## ").or_else(|| line.strip_prefix("# ")) {
                section = heading.to_string();
                paint(&plain(heading), BOLD, true)
            } else if line == "</details>" {
                continue;
            } else if let Some(summary) = line
                .strip_prefix("<details><summary>")
                .and_then(|l| l.strip_suffix("</summary>"))
            {
                section.clear();
                format!("{}:", plain(summary))
            } else if let Some(quote) = line.strip_prefix("> ") {
                if quote.starts_with("**Blocking:**") {
                    quote_style = Some(&bad);
                } else if quote.starts_with("**Warning:**") {
                    quote_style = Some(YELLOW);
                }
                match quote_style {
                    Some(style) => paint(&plain(quote), style, true),
                    None => plain(quote),
                }
            } else if line == "No breakage expected." {
                paint(line, GREEN, true)
            } else if let Some(item) = line.strip_prefix("- ") {
                let text = format!("- {}", plain(item));
                let more = item.starts_with('…');
                if section.starts_with(REMOVED) && !more {
                    let style = if is_soname(&plain(item)) { RED } else { YELLOW };
                    paint(&text, style, true)
                } else if section.starts_with(INSTALLABILITY) && !more {
                    paint(&text, RED, true)
                } else if section.starts_with(WOULD_BREAK) {
                    paint(&text, &bad, true)
                } else {
                    text
                }
            } else if line.starts_with("  - ") && section.starts_with(WOULD_BREAK) {
                paint(&plain(line), RED, true)
            } else {
                // Text of a tool's own after a section ends it.
                if !line.trim().is_empty() && !line.starts_with(' ') {
                    section.clear();
                }
                plain(line)
            };
        out.push_str(&shown);
        out.push('\n');
    }
    out
}

/// Whether `capability` is a shared library's soname, whose loss
/// breaks the binaries built against it.
fn is_soname(capability: &str) -> bool {
    sandogasa_depfilter::is_solib_dep(capability)
        && !sandogasa_depfilter::is_solib_symbol_dep(capability)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandogasa_cli::style::{BOLD, GREEN, RED, RESET, YELLOW};

    fn change(package: &str, old: Option<&str>, new: &str) -> PackageChange {
        PackageChange {
            package: package.into(),
            old: old.map(String::from),
            new: new.into(),
        }
    }

    fn broken() -> BTreeMap<String, RevDepResult> {
        BTreeMap::from([(
            "kpat".to_string(),
            RevDepResult {
                status: "broken".into(),
                issues: vec![crate::report::BrokenRequires {
                    dep: "libfoo.so.1()(64bit)".into(),
                    changed_provide: "libfoo.so.1()(64bit)".into(),
                    kind: BreakKind::Fti,
                }],
                arrival: None,
            },
        )])
    }

    #[test]
    fn a_summary_points_at_how_to_see_the_rest_or_says_nothing() {
        let lines: Vec<String> = (0..LIST_CAP + 2).map(|i| format!("- {i}")).collect();
        let mut o = String::new();
        let opts = Options {
            detailed: false,
            more: "use --detailed",
        };
        write_capped(&mut o, lines.clone(), &opts);
        assert!(o.ends_with("- … and 2 more (use --detailed)\n"), "{o}");
        let mut o = String::new();
        write_capped(&mut o, lines.clone(), &Options { more: "", ..opts });
        assert!(o.ends_with("- … and 2 more\n"), "{o}");
        let mut o = String::new();
        write_capped(
            &mut o,
            lines,
            &Options {
                detailed: true,
                ..opts
            },
        );
        assert_eq!(o.lines().count(), LIST_CAP + 2);
    }

    #[test]
    fn findings_with_nothing_examined_say_so() {
        let empty = BTreeMap::new();
        let f = Findings {
            updated_packages: &[],
            changes: &[],
            changed_provides: &[],
            installability_issues: &[],
            reverse_deps: &empty,
        };
        let opts = Options {
            detailed: false,
            more: "",
        };
        let out = findings(&f, &opts, "Nothing to check here.");
        assert!(out.ends_with("Nothing to check here.\n"), "{out}");
    }

    #[test]
    fn a_terminal_shows_the_report_in_color_and_a_pipe_as_markdown() {
        let changes = [change("libfoo", Some("1-1"), "2-1")];
        let changed = [
            ChangedProvide {
                old: "libfoo.so.1()(64bit)".into(),
                new: None,
            },
            ChangedProvide {
                old: "foo-tools = 1-1".into(),
                new: None,
            },
        ];
        let issues = [UnsatisfiedDep {
            package: "foo".into(),
            dep: "libbar.so.2()(64bit)".into(),
            unresolved: vec![],
        }];
        let revdeps = broken();
        let f = Findings {
            updated_packages: &["libfoo".to_string()],
            changes: &changes,
            changed_provides: &changed,
            installability_issues: &issues,
            reverse_deps: &revdeps,
        };
        let opts = Options {
            detailed: false,
            more: "",
        };
        let md = format!(
            "# Checking update: x\n\n> **Warning:** stale\n> **Blocking:** in another\n\n{}{}",
            package_summary(f.updated_packages, f.changes, &opts),
            findings(&f, &opts, "")
        );
        assert_eq!(to_terminal(&md, false), md);
        let tty = to_terminal(&md, true);
        assert!(
            tty.contains(&format!("{BOLD}Checking update: x{RESET}")),
            "{tty}"
        );
        assert!(
            tty.contains(&format!("{YELLOW}Warning: stale{RESET}")),
            "{tty}"
        );
        assert!(
            tty.contains(&format!("{BOLD}{RED}Blocking: in another{RESET}")),
            "{tty}"
        );
        assert!(
            tty.contains(&format!("{RED}- libfoo.so.1()(64bit){RESET}")),
            "{tty}"
        );
        assert!(
            tty.contains(&format!("{YELLOW}- foo-tools = 1-1{RESET}")),
            "{tty}"
        );
        assert!(
            tty.contains(&format!("{RED}- foo: libbar.so.2()(64bit){RESET}")),
            "{tty}"
        );
        assert!(tty.contains(&format!("{BOLD}{RED}- kpat{RESET}")), "{tty}");
        assert!(
            tty.contains(&format!("{RED}  - [FTI] libfoo.so.1()(64bit)")),
            "{tty}"
        );
        assert!(!tty.contains('`') && !tty.contains("**"), "{tty}");

        // A blocking note's items take its color; a collapsed list a
        // tool adds after the breakage is not breakage.
        let tail = "## Reverse dependencies that would break (1)\n\n- **kpat**\n\n\
                    <details><summary>Changes 1 Provide</summary>\n\n- `a = 1` to `a = 2`\n\n\
                    </details>\n\n> **Blocking:** the target moved\n> - `foo-1` is older\n";
        let tty = to_terminal(tail, true);
        assert!(tty.contains("\n- a = 1 to a = 2\n"), "{tty}");
        assert!(
            tty.contains(&format!("{BOLD}{RED}- foo-1 is older{RESET}")),
            "{tty}"
        );
        assert!(is_soname("libfoo.so.1()(64bit)"));
        assert!(!is_soname("libfoo.so.1(LIBFOO_1.0)(64bit)"));

        // A clean report, with a cbspkg-style collapsed list and footer.
        let clean = "Nothing dropped.\n\n<details><summary>Changes 1 Provide</summary>\n\n\
                     - `a = 1` to `a = 2`\n\n</details>\n\n<sub>Checked by x</sub>\n\
                     No breakage expected.\n";
        let tty = to_terminal(clean, true);
        assert!(tty.contains("Changes 1 Provide:\n"), "{tty}");
        assert!(!tty.contains("details"), "{tty}");
        assert!(tty.contains("Checked by x\n"), "{tty}");
        assert!(
            tty.contains(&format!("{GREEN}No breakage expected.{RESET}")),
            "{tty}"
        );
    }
}
