// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Debian `debian/changelog` parsing and the deterministic edits
//! dbranch makes: resolving the merge conflict that appears when the
//! Debian branch is merged into a PPA branch, computing the
//! `<debver>~<codename>+<N>` rebuild version, and normalizing the
//! top stanza into a clean "Rebuild for <codename>" entry.
//!
//! These are pure string transforms so they can be unit-tested
//! against real `damo` changelog shapes without a git tree.

/// The header fields of a changelog stanza —
/// `package (version) distribution; metadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub package: String,
    pub version: String,
    pub distribution: String,
    /// Everything after the `;` (e.g. `urgency=medium`).
    pub metadata: String,
}

/// Parse a stanza header line, or `None` if the line isn't one.
pub fn parse_header(line: &str) -> Option<Header> {
    let line = line.trim_end();
    // A header has no leading whitespace.
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let open = line.find(" (")?;
    let package = line[..open].trim().to_string();
    let rest = &line[open + 2..];
    let close = rest.find(')')?;
    let version = rest[..close].to_string();
    let after = rest[close + 1..].trim_start();
    let semi = after.find(';')?;
    let distribution = after[..semi].trim().to_string();
    let metadata = after[semi + 1..].trim().to_string();
    if package.is_empty() || version.is_empty() || distribution.is_empty() {
        return None;
    }
    Some(Header {
        package,
        version,
        distribution,
        metadata,
    })
}

/// Parse every stanza header in a changelog, in file order
/// (newest first).
pub fn stanza_headers(changelog: &str) -> Vec<Header> {
    changelog.lines().filter_map(parse_header).collect()
}

/// Resolve a git-conflicted `debian/changelog` into merged text.
///
/// Returns `None` when the text contains no conflict markers (a clean
/// merge needs no fixup). For each conflict hunk the **incoming**
/// side (`theirs` — the Debian branch's new entries) is placed
/// *above* the **local** side (`ours` — the existing rebuild entry),
/// which is the version-descending order Debian changelogs use and
/// exactly what `dpkg-mergechangelogs` produces for this case.
pub fn resolve_conflict(text: &str) -> Option<String> {
    if !text.contains("<<<<<<<") {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if line.starts_with("<<<<<<<") {
            let mut ours: Vec<&str> = Vec::new();
            for l in lines.by_ref() {
                if l.starts_with("=======") {
                    break;
                }
                ours.push(l);
            }
            let mut theirs: Vec<&str> = Vec::new();
            for l in lines.by_ref() {
                if l.starts_with(">>>>>>>") {
                    break;
                }
                theirs.push(l);
            }
            // Incoming Debian entries on top, then the local rebuild,
            // separated by exactly one blank line. Depending on where
            // git drew the hunk boundary, the stanza-separating blank may
            // be missing from (or duplicated across) the two sides — so
            // trim the junction and re-insert a single blank, or the two
            // stanzas would run together (footer line immediately
            // followed by the next header).
            while theirs.last().is_some_and(|l| l.trim().is_empty()) {
                theirs.pop();
            }
            while ours.first().is_some_and(|l| l.trim().is_empty()) {
                ours.remove(0);
            }
            out.extend_from_slice(&theirs);
            if !theirs.is_empty() && !ours.is_empty() {
                out.push("");
            }
            out.extend_from_slice(&ours);
        } else {
            out.push(line);
        }
    }
    let mut resolved = out.join("\n");
    if text.ends_with('\n') {
        resolved.push('\n');
    }
    Some(resolved)
}

/// The Debian base of a (possibly already rebuilt) version: strips a
/// trailing rebuild/stable suffix — either a PPA `~<codename>+<N>` or a
/// proposed-update `~deb<N>u<M>`. A Debian version may itself contain
/// `~` (e.g. `1.0~rc1-1`), so only those specific trailing shapes are
/// removed: `3.2.8-1~questing+1` → `3.2.8-1` and
/// `0~20260420-1~deb13u1` → `0~20260420-1`, while `1.0~rc1-1` is left
/// intact.
pub fn debian_base(version: &str) -> &str {
    if let Some(idx) = version.rfind('~') {
        let tail = &version[idx + 1..];
        // PPA rebuild suffix: ~<codename>+<digits>
        if let Some((name, n)) = tail.rsplit_once('+')
            && !name.is_empty()
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
        {
            return &version[..idx];
        }
        // Proposed-update suffix: ~deb<digits>u<digits>
        if let Some(rest) = tail.strip_prefix("deb")
            && let Some((maj, m)) = rest.split_once('u')
            && !maj.is_empty()
            && maj.bytes().all(|b| b.is_ascii_digit())
            && !m.is_empty()
            && m.bytes().all(|b| b.is_ascii_digit())
        {
            return &version[..idx];
        }
    }
    version
}

/// Compute the fresh rebuild version `<debver>~<codename>+<N>`.
///
/// `debver` is the Debian base of the newest stanza's version (any
/// existing rebuild suffix stripped, so running from a PPA branch
/// still yields a clean base). `N` is one past the highest existing
/// `+<N>` for that exact `<debver>~<codename>+` prefix (so re-running
/// on the same Debian version bumps the counter), or `1` when there
/// is none. Returns `None` for an empty/unparseable changelog.
pub fn rebuild_version(changelog: &str, codename: &str) -> Option<String> {
    let headers = stanza_headers(changelog);
    let debver = debian_base(&headers.first()?.version);
    let prefix = format!("{debver}~{codename}+");
    let max_n = headers
        .iter()
        .filter_map(|h| h.version.strip_prefix(&prefix))
        .filter_map(|n| n.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    Some(format!("{prefix}{}", max_n + 1))
}

/// Compute the fresh proposed-update version `<debver>~deb<major>u<M>`
/// for a Debian stable branch (e.g. `debian/trixie`, `major` = 13).
///
/// The shape mirrors [`rebuild_version`], only the suffix differs:
/// `debver` is the Debian base of the newest stanza (any rebuild/stable
/// suffix stripped, so the unstable base shows through after a merge),
/// and `M` is one past the highest existing `~deb<major>u<M>` for that
/// exact base — so a new upstream resets to `u1` while a stable-only
/// re-release of the same base bumps the counter. The `~` (not `+`)
/// makes the stable version sort *older* than the plain build, so it
/// never shadows testing/unstable on upgrade. Returns `None` for an
/// empty/unparseable changelog.
pub fn proposed_version(changelog: &str, major: u32) -> Option<String> {
    let headers = stanza_headers(changelog);
    let debver = debian_base(&headers.first()?.version);
    let prefix = format!("{debver}~deb{major}u");
    let max_m = headers
        .iter()
        .filter_map(|h| h.version.strip_prefix(&prefix))
        .filter_map(|m| m.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    Some(format!("{prefix}{}", max_m + 1))
}

/// Rewrite the top stanza into a clean rebuild entry, replacing
/// whatever `gbp dch` generated as the body (which would otherwise be
/// the whole merge delta) with a synthesized one: header
/// `<package> (<version>) <codename>; <metadata>`, a `* Rebuild for
/// <codename>` line, and — when dbranch adjusted packaging files this
/// run — a single `* Adjust <files> for <codename>` line naming them.
/// The stanza's original footer (the date/maintainer line `gbp dch`
/// finalized) is kept; lower stanzas are left untouched. Discarding the
/// gbp dch body also drops any `UNRELEASED` it may have added.
pub fn normalize_top_stanza(
    changelog: &str,
    version: &str,
    codename: &str,
    adjusted: &[String],
) -> Result<String, String> {
    let lines: Vec<&str> = changelog.lines().collect();
    let mut i = 0;
    while i < lines.len() && lines[i].trim().is_empty() {
        i += 1;
    }
    let header = lines
        .get(i)
        .and_then(|l| parse_header(l))
        .ok_or("no stanza header at top of changelog")?;
    // Footer is the first ` -- ` line; bail if a new header appears
    // first (means the top stanza has no footer).
    let mut j = i + 1;
    loop {
        let line = lines.get(j).ok_or("top stanza has no footer")?;
        if line.starts_with(" -- ") {
            break;
        }
        if parse_header(line).is_some() {
            return Err("top stanza has no footer".to_string());
        }
        j += 1;
    }

    let mut out = String::new();
    for l in &lines[..i] {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(&format!(
        "{} ({}) {}; {}\n\n  * Rebuild for {codename}\n",
        header.package, version, codename, header.metadata
    ));
    if !adjusted.is_empty() {
        out.push_str(&format!(
            "  * Adjust {} for {codename}\n",
            join_and(adjusted)
        ));
    }
    out.push('\n');
    out.push_str(lines[j]);
    out.push('\n');
    for l in &lines[j + 1..] {
        out.push_str(l);
        out.push('\n');
    }
    Ok(out)
}

/// Join items in plain English: `a`, `a and b`, `a, b, and c`.
fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_header_extracts_fields() {
        let h = parse_header("damo (3.2.8-1~questing+1) questing; urgency=medium").unwrap();
        assert_eq!(h.package, "damo");
        assert_eq!(h.version, "3.2.8-1~questing+1");
        assert_eq!(h.distribution, "questing");
        assert_eq!(h.metadata, "urgency=medium");
        // Body and footer lines are not headers.
        assert!(parse_header("  * Rebuild for questing").is_none());
        assert!(parse_header(" -- Michel Lind <m@x>  Wed, 17 Jun 2026 18:22:19 +0100").is_none());
        assert!(parse_header("").is_none());
    }

    /// The conflict git produces when master's new 3.2.8-1 entry and
    /// questing's 3.2.7-1~questing+1 rebuild entry both sit above the
    /// common 3.2.7-1 entry.
    const CONFLICTED: &str = "\
<<<<<<< HEAD
damo (3.2.7-1~questing+1) questing; urgency=medium

  * Rebuild for questing

 -- Michel Lind <m@x>  Wed, 10 Jun 2026 15:50:08 +0100

=======
damo (3.2.8-1) unstable; urgency=medium

  * New upstream version 3.2.8

 -- Michel Lind <m@x>  Wed, 17 Jun 2026 17:28:43 +0100

>>>>>>> master
damo (3.2.7-1) unstable; urgency=medium

  * New upstream version 3.2.7

 -- Michel Lind <m@x>  Wed, 10 Jun 2026 14:24:02 +0100
";

    #[test]
    fn resolve_conflict_puts_debian_entry_above_rebuild() {
        let resolved = resolve_conflict(CONFLICTED).unwrap();
        let headers = stanza_headers(&resolved);
        let versions: Vec<&str> = headers.iter().map(|h| h.version.as_str()).collect();
        // Incoming Debian entry on top, then the local rebuild, then
        // the common base — matching the damo merge commit.
        assert_eq!(versions, vec!["3.2.8-1", "3.2.7-1~questing+1", "3.2.7-1"]);
        // No conflict markers survive.
        assert!(!resolved.contains("<<<<<<<"));
        assert!(!resolved.contains("======="));
        assert!(!resolved.contains(">>>>>>>"));
        // Spacing is preserved (stanza separated by a blank line).
        assert!(resolved.contains("17:28:43 +0100\n\ndamo (3.2.7-1~questing+1)"));
    }

    #[test]
    fn resolve_conflict_none_when_clean() {
        assert!(resolve_conflict("damo (3.2.8-1) unstable; urgency=medium\n").is_none());
    }

    #[test]
    fn resolve_conflict_inserts_blank_between_stanzas() {
        // Git can draw the hunk so the incoming side ends on its footer
        // with no trailing blank and the local side starts on its
        // header — the two stanzas must not run together.
        let conflicted = "\
<<<<<<< HEAD
archlinux-keyring (0~20260420-1~deb13u1) trixie; urgency=medium

  * gbp.conf: set branch to debian/trixie

 -- M <m@x>  Mon, 08 Jun 2026 16:50:42 +0100
=======
archlinux-keyring (0~20260612-1) unstable; urgency=medium

  * New upstream version 0~20260612

 -- M <m@x>  Fri, 19 Jun 2026 15:55:25 +0100
>>>>>>> main

archlinux-keyring (0~20260420-1) unstable; urgency=medium

  * older

 -- M <m@x>  Thu, 01 Jan 2026 00:00:00 +0100
";
        let resolved = resolve_conflict(conflicted).unwrap();
        // Incoming on top, then local, then common.
        let versions: Vec<String> = stanza_headers(&resolved)
            .iter()
            .map(|h| h.version.clone())
            .collect();
        assert_eq!(
            versions,
            ["0~20260612-1", "0~20260420-1~deb13u1", "0~20260420-1"]
        );
        // A blank line separates the incoming footer from the local
        // header (the bug was a missing newline here).
        assert!(resolved.contains("15:55:25 +0100\n\narchlinux-keyring (0~20260420-1~deb13u1)"));
        // No doubled blank lines at that junction.
        assert!(!resolved.contains("15:55:25 +0100\n\n\narchlinux-keyring"));
    }

    #[test]
    fn debian_base_strips_rebuild_suffix_only() {
        assert_eq!(debian_base("3.2.8-1~questing+1"), "3.2.8-1");
        assert_eq!(debian_base("1.0~bpo12+1"), "1.0");
        // Proposed-update (stable) suffix is stripped too.
        assert_eq!(debian_base("0~20260420-1~deb13u1"), "0~20260420-1");
        assert_eq!(debian_base("1.2.3-4~deb12u2"), "1.2.3-4");
        // A plain Debian version is unchanged...
        assert_eq!(debian_base("3.2.8-1"), "3.2.8-1");
        // ...including one that legitimately contains `~`.
        assert_eq!(debian_base("1.0~rc1-1"), "1.0~rc1-1");
        // ...and one whose `~` segment only looks deb-ish.
        assert_eq!(debian_base("1.0~debug-1"), "1.0~debug-1");
        // Stacked suffix: only the last is stripped.
        assert_eq!(
            debian_base("3.2.8-1~questing+1~noble+2"),
            "3.2.8-1~questing+1"
        );
    }

    #[test]
    fn proposed_version_fresh_and_increment() {
        // After merging the Debian branch in, the top stanza is the
        // unstable version — a fresh stable build is ~deb13u1.
        let fresh = "\
archlinux-keyring (0~20260612-1) unstable; urgency=medium

  * New upstream version 0~20260612

 -- M <m@x>  Fri, 12 Jun 2026 10:00:00 +0100
";
        assert_eq!(
            proposed_version(fresh, 13).as_deref(),
            Some("0~20260612-1~deb13u1")
        );
        // A stable-only re-release (top already ~deb13u1, same base)
        // bumps to u2; debian_base sees through the existing suffix.
        let again = "\
archlinux-keyring (0~20260612-1~deb13u1) trixie; urgency=medium

  * Rebuild for trixie

 -- M <m@x>  Fri, 12 Jun 2026 12:00:00 +0100
";
        assert_eq!(
            proposed_version(again, 13).as_deref(),
            Some("0~20260612-1~deb13u2")
        );
        // A different Debian major starts fresh at u1.
        assert_eq!(
            proposed_version(again, 12).as_deref(),
            Some("0~20260612-1~deb12u1")
        );
    }

    #[test]
    fn rebuild_version_from_a_ppa_top_uses_the_debian_base() {
        // Running from a PPA branch whose top is a rebuild entry: the
        // base is still 3.2.8-1, so a noble rebuild is +1 (not a
        // doubly-suffixed mess).
        let cl = "\
damo (3.2.8-1~questing+1) questing; urgency=medium

  * Rebuild for questing

 -- M <m@x>  Wed, 17 Jun 2026 18:22:19 +0100
";
        assert_eq!(
            rebuild_version(cl, "noble").as_deref(),
            Some("3.2.8-1~noble+1")
        );
    }

    #[test]
    fn rebuild_version_fresh_is_plus_one() {
        // After resolving, the top stanza is the new Debian version.
        let resolved = resolve_conflict(CONFLICTED).unwrap();
        assert_eq!(
            rebuild_version(&resolved, "questing").as_deref(),
            Some("3.2.8-1~questing+1")
        );
    }

    #[test]
    fn rebuild_version_increments_existing_counter() {
        // A changelog where the top Debian version already has a
        // questing rebuild at +1 -> next is +2.
        let cl = "\
damo (3.2.8-1) unstable; urgency=medium

  * New upstream version 3.2.8

 -- M <m@x>  Wed, 17 Jun 2026 17:28:43 +0100

damo (3.2.8-1~questing+1) questing; urgency=medium

  * Rebuild for questing

 -- M <m@x>  Wed, 17 Jun 2026 18:22:19 +0100
";
        assert_eq!(
            rebuild_version(cl, "questing").as_deref(),
            Some("3.2.8-1~questing+2")
        );
        // A different codename starts fresh at +1.
        assert_eq!(
            rebuild_version(cl, "noble").as_deref(),
            Some("3.2.8-1~noble+1")
        );
    }

    #[test]
    fn normalize_top_stanza_rewrites_version_dist_and_body() {
        // Simulate the messy entry gbp dch --bpo leaves on top — body
        // includes the merged Debian commits, which must be discarded.
        let after_gbp = "\
damo (3.2.8-1~bpo13+1) questing; urgency=medium

  [ T ]
  * Rebuild for trixie-backports.
  * Debian change one

 -- Michel Lind <m@x>  Wed, 17 Jun 2026 18:22:19 +0100

damo (3.2.8-1) unstable; urgency=medium

  * New upstream version 3.2.8

 -- Michel Lind <m@x>  Wed, 17 Jun 2026 17:28:43 +0100
";
        let out = normalize_top_stanza(after_gbp, "3.2.8-1~questing+1", "questing", &[]).unwrap();
        let top: Vec<&str> = out.lines().take(5).collect();
        assert_eq!(top[0], "damo (3.2.8-1~questing+1) questing; urgency=medium");
        assert_eq!(top[1], "");
        assert_eq!(top[2], "  * Rebuild for questing");
        assert_eq!(top[3], "");
        // The gbp-finalized footer (date/maintainer) is preserved.
        assert_eq!(
            top[4],
            " -- Michel Lind <m@x>  Wed, 17 Jun 2026 18:22:19 +0100"
        );
        // The merged Debian commit from the gbp body is gone.
        assert!(!out.contains("Debian change one"));
        assert!(!out.contains("trixie-backports"));
        // The Debian stanza below is untouched.
        assert!(out.contains("damo (3.2.8-1) unstable; urgency=medium"));
        assert!(out.contains("  * New upstream version 3.2.8"));
    }

    #[test]
    fn normalize_top_stanza_lists_adjusted_files() {
        let after_gbp = "\
damo (3.2.8-1~bpo13+1) questing; urgency=medium

  * Rebuild for trixie-backports.

 -- M <m@x>  Wed, 17 Jun 2026 18:22:19 +0100
";
        let adjusted = vec!["gbp.conf".to_string(), "salsa-ci.yml".to_string()];
        let out =
            normalize_top_stanza(after_gbp, "3.2.8-1~questing+1", "questing", &adjusted).unwrap();
        let top: Vec<&str> = out.lines().take(5).collect();
        assert_eq!(top[0], "damo (3.2.8-1~questing+1) questing; urgency=medium");
        assert_eq!(top[1], "");
        assert_eq!(top[2], "  * Rebuild for questing");
        assert_eq!(top[3], "  * Adjust gbp.conf and salsa-ci.yml for questing");
        assert_eq!(top[4], "");
    }

    #[test]
    fn join_and_is_plain_english() {
        assert_eq!(join_and(&["a".to_string()]), "a");
        assert_eq!(join_and(&["a".to_string(), "b".to_string()]), "a and b");
        assert_eq!(
            join_and(&["a".to_string(), "b".to_string(), "c".to_string()]),
            "a, b, and c"
        );
    }
}
