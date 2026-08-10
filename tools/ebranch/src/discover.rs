// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Proposing an update's bug list (`check-update --submit`).
//!
//! Finding which bugs belong to a big update is one of the more
//! tedious parts of submitting one, and it is mechanical: the bugs
//! are either still open against a package the update builds, or
//! named in the changelog entries the update introduces.
//!
//! Two sources, because neither alone is enough:
//!
//! - **Open bugs** against the components being built. Not scoped to
//!   the update's release: the-new-hotness files update requests
//!   against Rawhide and package reviews live under Fedora, so an
//!   EPEL update's bugs are mostly not EPEL bugs.
//! - **`rhbz#` references in the new changelog entries.** A bug fixed
//!   in Rawhide is closed when that build lands, so it is no longer
//!   open and the first source misses it — but a branch update
//!   carrying the same fix still closes it for that branch.
//!
//! A candidate is proposed only if [`crate::karma::bug_verdict`]
//! would score it +1, so what gets attached and what gets voted on
//! cannot disagree.

use std::collections::BTreeMap;

use crate::karma::{UpdateFacts, Verdict};

/// A bug the update looks like it should close, and how it was found.
pub struct Candidate {
    pub bug_id: u64,
    pub summary: String,
    /// Where it came from, shown so the user can judge the proposal.
    pub source: String,
}

/// Bug references in a changelog body, in the forms packagers
/// actually write: `rhbz#123`, `RHBZ#123`, `bz#123`, `#123` after a
/// `Resolves`/`Fixes`/`Closes` keyword, and Bugzilla URLs.
///
/// A bare `#123` without a keyword is ignored — it is as likely to be
/// a GitHub issue, and this list decides what gets closed.
pub fn changelog_bug_refs(body: &str) -> Vec<u64> {
    let mut found = Vec::new();
    let lower = body.to_ascii_lowercase();
    for (idx, _) in lower.match_indices('#') {
        let before = &lower[..idx];
        let keyed = before.ends_with("rhbz")
            || before.ends_with("bz")
            || before.ends_with("bug")
            || ends_with_resolution_keyword(before);
        if !keyed {
            continue;
        }
        let digits: String = lower[idx + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(id) = digits.parse::<u64>() {
            found.push(id);
        }
    }
    for (idx, _) in lower.match_indices("show_bug.cgi?id=") {
        let digits: String = lower[idx + "show_bug.cgi?id=".len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(id) = digits.parse::<u64>() {
            found.push(id);
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// Whether `text` ends with a resolution keyword and optional
/// punctuation, so `Resolves: #123` counts but a bare `#123` does not.
fn ends_with_resolution_keyword(text: &str) -> bool {
    let trimmed = text.trim_end_matches([' ', ':', '-']);
    ["resolves", "fixes", "closes", "resolved"]
        .iter()
        .any(|kw| trimmed.ends_with(kw))
}

/// Bug references from the changelog entries a build introduces —
/// those newer than `since`, the version-release the target already
/// has.
///
/// Scanning the whole changelog would attach bugs fixed years ago in
/// releases the target has had all along. `since` is `None` for a
/// package the target does not have yet, where every entry is new.
pub fn new_entry_bug_refs(changelog: &str, since: Option<&str>) -> Vec<u64> {
    let mut found = Vec::new();
    for (evr, body) in changelog_entries(changelog) {
        let is_new = match (since, evr) {
            (Some(since), Some(evr)) => {
                sandogasa_rpmvercmp::rpmvercmp(evr, since) == std::cmp::Ordering::Greater
            }
            // Without a version on either side there is nothing to
            // compare, so the entry is only taken for a package the
            // target does not have at all.
            _ => since.is_none(),
        };
        if is_new {
            found.extend(changelog_bug_refs(&body));
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// Split an RPM changelog into `(version-release, body)` per entry.
///
/// An entry starts with `* <date> <author> - <version-release>`; the
/// version-release is absent from the rare entry that omits it.
fn changelog_entries(changelog: &str) -> Vec<(Option<&str>, String)> {
    let mut entries: Vec<(Option<&str>, String)> = Vec::new();
    for line in changelog.lines() {
        if let Some(header) = line.strip_prefix("* ") {
            // The version-release trails the author, after the last
            // " - " — authors do not usually contain one.
            let evr = header.rsplit_once(" - ").map(|(_, evr)| evr.trim());
            entries.push((evr.filter(|e| !e.is_empty()), String::new()));
        } else if let Some((_, body)) = entries.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    entries
}

/// Propose bugs for an update: those still open against a package it
/// builds, and those named in the changelog entries it introduces.
///
/// Only bugs the vote logic would score +1 are proposed, and only
/// ones not already attached.
pub async fn candidates(
    bz: &sandogasa_bugzilla::BzClient,
    changelogs: &BTreeMap<String, String>,
    old_versions: &BTreeMap<String, Option<String>>,
    update: &UpdateFacts<'_>,
    already_listed: &[u64],
) -> Vec<Candidate> {
    let mut from_changelog: BTreeMap<u64, String> = BTreeMap::new();
    for (package, changelog) in changelogs {
        let since = old_versions.get(package).and_then(Option::as_deref);
        for id in new_entry_bug_refs(changelog, since) {
            from_changelog
                .entry(id)
                .or_insert_with(|| format!("named in {package}'s new changelog entries"));
        }
    }

    let open = open_bugs_for_components(bz, update.builds).await;
    let mut wanted: Vec<u64> = open.keys().copied().collect();
    wanted.extend(from_changelog.keys().copied());
    wanted.retain(|id| !already_listed.contains(id));
    wanted.sort_unstable();
    wanted.dedup();

    // Everything reaches the same verdict logic the vote uses, so a
    // proposed bug is one the update would vote +1 on.
    let facts = crate::karma::fetch_bugs(bz, &wanted).await;
    let mut out = Vec::new();
    for id in wanted {
        let Some(bug) = facts.get(&id) else { continue };
        if !matches!(
            crate::karma::bug_verdict(bug, update),
            Verdict::Decided { karma: 1, .. }
        ) {
            continue;
        }
        let source = from_changelog.get(&id).cloned().unwrap_or_else(|| {
            open.get(&id)
                .map(|c| format!("open against {c}"))
                .unwrap_or_else(|| "open".to_string())
        });
        out.push(Candidate {
            bug_id: id,
            summary: bug.summary.clone(),
            source,
        });
    }
    out
}

/// Open bugs against any of the update's packages, as bug ID to the
/// component it is filed against.
///
/// Deliberately not scoped to the update's release: an EPEL update's
/// update requests and package reviews are filed against Fedora.
async fn open_bugs_for_components(
    bz: &sandogasa_bugzilla::BzClient,
    builds: &[(String, String)],
) -> BTreeMap<u64, String> {
    if builds.is_empty() {
        return BTreeMap::new();
    }
    let mut query: Vec<String> = builds
        .iter()
        .map(|(pkg, _)| format!("component={}", urlencode(pkg)))
        .collect();
    query.push("bug_status=__open__".to_string());
    match bz.search(&query.join("&"), 0).await {
        Ok(bugs) => bugs
            .into_iter()
            .filter_map(|b| Some((b.id, b.component.first()?.clone())))
            .collect(),
        Err(e) => {
            eprintln!(
                "warning: could not search Bugzilla for open bugs ({e}); proposing only what the changelogs name"
            );
            BTreeMap::new()
        }
    }
}

/// Percent-encode a query value. Package names are conservative, but
/// a `+` in one would otherwise be read as a space.
fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real changelog, from `koji buildinfo --changelog`.
    const CHANGELOG: &str = "\
* Wed Aug 05 2026 Michel Lind <salimma@fedoraproject.org> - 1.0.8-1
- Update to version 1.0.8; Resolves RHBZ#2344815

* Fri Jul 17 2026 Fedora Release Engineering <releng@fedoraproject.org> - 0.6.3-2
- Rebuilt for https://fedoraproject.org/wiki/Fedora_45_Mass_Rebuild

* Tue Mar 17 2026 Michel Lind <salimma@fedoraproject.org> - 0.6.3-1
- Update to version 0.6.3; Resolves rhbz#2200001

* Tue Feb 10 2026 Fabio Valentini <decathorpe@gmail.com> - 0.4.3-1
- Update to version 0.4.3
";

    #[test]
    fn changelog_bug_refs_reads_the_forms_packagers_write() {
        assert_eq!(changelog_bug_refs("Resolves RHBZ#2344815"), vec![2344815]);
        assert_eq!(changelog_bug_refs("resolves: rhbz#123"), vec![123]);
        assert_eq!(changelog_bug_refs("- Fixes bz#456"), vec![456]);
        assert_eq!(changelog_bug_refs("Resolves: #789"), vec![789]);
        assert_eq!(
            changelog_bug_refs("see https://bugzilla.redhat.com/show_bug.cgi?id=321 for details"),
            vec![321]
        );
    }

    #[test]
    fn changelog_bug_refs_ignores_a_bare_hash_number() {
        // As likely a GitHub issue as a Bugzilla bug, and this list
        // decides what gets closed.
        assert!(changelog_bug_refs("- Backport upstream #4567").is_empty());
        assert!(changelog_bug_refs("- Rebuilt for Fedora 45 Mass Rebuild").is_empty());
    }

    #[test]
    fn new_entry_bug_refs_takes_only_entries_the_update_adds() {
        // The target already has 0.6.3-2, so only the 1.0.8-1 entry
        // is new; the bug fixed back in 0.6.3-1 is already there.
        assert_eq!(
            new_entry_bug_refs(CHANGELOG, Some("0.6.3-2")),
            vec![2344815]
        );
        // Further back, both bugs come along.
        assert_eq!(
            new_entry_bug_refs(CHANGELOG, Some("0.5.0-1")),
            vec![2200001, 2344815]
        );
        // Nothing new when the target already has the top entry.
        assert!(new_entry_bug_refs(CHANGELOG, Some("1.0.8-1")).is_empty());
    }

    #[test]
    fn new_entry_bug_refs_takes_everything_for_a_new_package() {
        // No old version means the target does not have the package,
        // so its whole history arrives with it.
        assert_eq!(new_entry_bug_refs(CHANGELOG, None), vec![2200001, 2344815]);
    }

    #[test]
    fn changelog_entries_splits_on_the_star_header() {
        let entries = changelog_entries(CHANGELOG);
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].0, Some("1.0.8-1"));
        assert!(entries[0].1.contains("RHBZ#2344815"));
        assert_eq!(entries[3].0, Some("0.4.3-1"));
    }
}
