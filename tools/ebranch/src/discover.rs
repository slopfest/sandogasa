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
//! - **CVE trackers** among those open bugs, filed against the update's
//!   release, are judged by where the CVE is fixed — NVD, or GitHub's
//!   advisories when NVD has nothing — since their titles carry no
//!   version; a build at or past the fix closes the tracker.
//! - **`rhbz#` references in the new changelog entries.** A bug fixed
//!   in Rawhide is closed when that build lands, so it is no longer
//!   open and the first source misses it — but a branch update
//!   carrying the same fix still closes it for that branch. Only for
//!   a package the target already has: a package arriving for the
//!   first time brings every entry it ever had, and those bugs were
//!   closed by builds on another branch.
//!
//! A candidate is proposed only if [`crate::karma::bug_verdict`]
//! would score it +1, so what gets attached and what gets voted on
//! cannot disagree.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::karma::{BugFacts, UpdateFacts, Verdict};

/// A bug the update looks like it should close, and how it was found.
pub struct Candidate {
    pub bug_id: u64,
    pub summary: String,
    /// Where it came from, shown so the user can judge the proposal.
    pub source: String,
}

impl Candidate {
    /// The candidate as the proposals show it: the bug, then why.
    pub fn shown(&self) -> String {
        format!("  #{} {}\n    {}", self.bug_id, self.summary, self.source)
    }
}

/// What [`candidates`] found: the bugs to propose, and the open ones
/// left off because a build from before the update already met them.
pub struct Proposal {
    pub candidates: Vec<Candidate>,
    /// Each with why it is left off, as [`Candidate::source`] says why
    /// one is proposed.
    pub already_met: Vec<Candidate>,
}

/// Why `bug`, open against a package the update builds, was met before
/// the update: the version the target had already reached the fix a
/// CVE tracker names, or the version an update request asks for (#112).
/// Closing it is then the maintainer's, or their own update's: a
/// rebuild in someone else's side tag did not deliver it. `None` when
/// the package is new to the target, or the bug says no version.
fn met_before(bug: &BugFacts, old_versions: &BTreeMap<String, Option<String>>) -> Option<String> {
    let pkg = bug.component.as_deref()?;
    let old = old_versions.get(pkg)?.as_deref()?;
    // The upstream version alone: a request or a fix names one, and the
    // old build's release would read as more of it (24.0.0-3 is past
    // 24.0.0.1 compared whole), as an epoch would outrank any version.
    let evr = old.split_once(':').map_or(old, |(_, vr)| vr);
    let version = evr.rsplit_once('-').map_or(evr, |(v, _)| v);
    if let Some(fix) = &bug.cve_fix {
        return sandogasa_cve::is_fix(version, &fix.fixed, &fix.ranges).then(|| {
            format!(
                "{pkg} was {old} already, at or past the {} fix {}",
                fix.cve_id,
                fix.fixed_versions()
            )
        });
    }
    if bug.bundled.is_some() {
        // What the old build bundled is not known here.
        return None;
    }
    let wanted = sandogasa_bugclass::bugzilla::extract_new_version(&bug.summary, pkg)?;
    (sandogasa_rpmvercmp::rpmvercmp(version, &wanted) != Ordering::Less)
        .then(|| format!("{pkg} was {old} already"))
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
/// releases the target has had all along. `None` treats every entry
/// as new, which is why [`candidates`] does not call it that way for
/// a package the target lacks — see the note there.
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

/// Bugs named in the changelog entries an update introduces, by bug
/// id, with the package that named each.
///
/// A package the target does not have yet brings its whole history
/// with it, and every bug in that history was closed by a build on
/// the branch it came from. Anything still open is proposed by the
/// open-bug source instead, so there is nothing here to find and a
/// great deal of noise to avoid: a 31-package EPEL newpackage update
/// proposed 150 long-closed bugs this way.
fn changelog_candidates(
    changelogs: &BTreeMap<String, String>,
    old_versions: &BTreeMap<String, Option<String>>,
) -> BTreeMap<u64, String> {
    let mut found: BTreeMap<u64, String> = BTreeMap::new();
    for (package, changelog) in changelogs {
        let Some(since) = old_versions.get(package).and_then(Option::as_deref) else {
            continue;
        };
        for id in new_entry_bug_refs(changelog, Some(since)) {
            found
                .entry(id)
                .or_insert_with(|| format!("named in {package}'s new changelog entries"));
        }
    }
    found
}

/// Propose bugs for an update: those still open against a package it
/// builds, and those named in the changelog entries it introduces.
///
/// Only bugs the vote logic would score +1 are proposed, and only
/// ones not already attached. An open bug a build from before the update
/// had already met is left off and listed apart (see [`met_before`]);
/// one the new changelog entries name is the update's by its own word.
pub async fn candidates(
    bz: &sandogasa_bugzilla::BzClient,
    changelogs: &BTreeMap<String, String>,
    old_versions: &BTreeMap<String, Option<String>>,
    update: &UpdateFacts<'_>,
    already_listed: &[u64],
    verbose: bool,
) -> Proposal {
    let from_changelog = changelog_candidates(changelogs, old_versions);

    let open = open_bugs_for_components(bz, update.builds).await;
    let mut wanted: Vec<u64> = open.keys().copied().collect();
    wanted.extend(from_changelog.keys().copied());
    wanted.retain(|id| !already_listed.contains(id));
    wanted.sort_unstable();
    wanted.dedup();

    // Everything reaches the same verdict logic the vote uses, so a
    // proposed bug is one the update would vote +1 on. CVE trackers
    // need the fix looked up first; the verdict cannot read it off
    // the title.
    let mut facts = crate::karma::fetch_bugs(bz, &wanted).await;
    crate::karma::attach_cve_facts(&mut facts, update, verbose).await;
    let mut out = Vec::new();
    let mut already_met = Vec::new();
    for id in wanted {
        let Some(bug) = facts.get(&id) else { continue };
        let Verdict::Decided { karma: 1, reason } = crate::karma::bug_verdict(bug, update) else {
            continue;
        };
        if !from_changelog.contains_key(&id)
            && let Some(why) = met_before(bug, old_versions)
        {
            already_met.push(Candidate {
                bug_id: id,
                summary: bug.summary.clone(),
                source: why,
            });
            continue;
        }
        let source = from_changelog.get(&id).cloned().unwrap_or_else(|| {
            match (&bug.cve_fix, &bug.bundled, open.get(&id)) {
                (Some(fix), _, _) => format!(
                    "{} tracker, fixed in {} per {}",
                    fix.cve_id,
                    fix.fixed_versions(),
                    fix.source
                ),
                // Why the copy counts as fixed is what the user needs
                // to judge the proposal, not where the bug turned up.
                (None, Some(_), _) => reason,
                (None, None, Some(c)) => format!("open against {c}"),
                (None, None, None) => "open".to_string(),
            }
        });
        out.push(Candidate {
            bug_id: id,
            summary: bug.summary.clone(),
            source,
        });
    }
    Proposal {
        candidates: out,
        already_met,
    }
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
pub(crate) fn urlencode(value: &str) -> String {
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

    fn open_bug(summary: &str, component: &str) -> BugFacts {
        BugFacts {
            summary: summary.to_string(),
            component: Some(component.to_string()),
            ..BugFacts::default()
        }
    }

    fn olds(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(p, v)| (p.to_string(), v.map(str::to_string)))
            .collect()
    }

    #[test]
    fn an_update_request_the_old_build_met_is_theirs_to_close() {
        // #112: asterisk was only rebuilt in the libical 4 side tag,
        // and 24.0.0-3 had been built before it.
        let bug = open_bug("asterisk-24.0.0 is available", "asterisk");
        let rebuilt = olds(&[("asterisk", Some("24.0.0-3.fc46"))]);
        assert_eq!(
            met_before(&bug, &rebuilt).as_deref(),
            Some("asterisk was 24.0.0-3.fc46 already")
        );
        // An older build had not: the update is what delivers it.
        let bumped = olds(&[("asterisk", Some("23.2.0-1.fc46"))]);
        assert_eq!(met_before(&bug, &bumped), None);
        // Only the version is compared: 24.0.0-3 is not 24.0.0.1, and
        // an epoch does not outrank a newer version.
        let point = open_bug("asterisk-24.0.0.1 is available", "asterisk");
        assert_eq!(met_before(&point, &rebuilt), None);
        let epoch = olds(&[("asterisk", Some("2:23.2.0-1.fc46"))]);
        assert_eq!(met_before(&bug, &epoch), None);
        // A package new to the target has no old build at all.
        assert_eq!(met_before(&bug, &olds(&[("asterisk", None)])), None);
        assert_eq!(met_before(&bug, &olds(&[])), None);
        // A bug that names no version is not judged here.
        let ftbfs = open_bug("asterisk: FTBFS in Fedora rawhide/f45", "asterisk");
        assert_eq!(met_before(&ftbfs, &rebuilt), None);
    }

    #[test]
    fn a_cve_tracker_the_old_build_already_fixed_is_theirs_to_close() {
        let fix = |at: &str| sandogasa_cve::CveFix {
            cve_id: "CVE-2026-62292".to_string(),
            fixed: vec![sandogasa_cve::FixedVersion {
                product: "libheif".to_string(),
                version: at.to_string(),
            }],
            ranges: vec![],
            source: "GHSA-73p7-m7gg-w2jv".to_string(),
        };
        let tracker = |at: &str| BugFacts {
            cve_fix: Some(fix(at)),
            ..open_bug(
                "CVE-2026-62292 libheif: denial of service [fedora-all]",
                "libheif",
            )
        };
        let old = olds(&[("libheif", Some("1.23.5-1.fc46"))]);
        let why = met_before(&tracker("1.23.3"), &old).unwrap();
        assert!(
            why.contains("libheif was 1.23.5-1.fc46 already") && why.contains("CVE-2026-62292"),
            "{why}"
        );
        assert_eq!(met_before(&tracker("1.23.6"), &old), None);
        // A copy the build bundles: what the old build bundled is not
        // known, so it is left to the verdict.
        let bundled = BugFacts {
            bundled: Some(crate::karma::BundledCopy {
                nvr: "libheif-1.23.5-1.fc46".to_string(),
                library: "libde265".to_string(),
                version: "1.0.15".to_string(),
                fixed: true,
                what: "1.0.16".to_string(),
            }),
            ..open_bug(
                "CVE-2026-1 libheif: overflow in libde265 [fedora-all]",
                "libheif",
            )
        };
        assert_eq!(met_before(&bundled, &old), None);
    }

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

    #[test]
    fn a_package_the_target_lacks_contributes_no_changelog_bugs() {
        // Regression (issue #17): an EPEL newpackage update proposed
        // 150 bugs, every one closed years earlier by a Fedora build,
        // because a package with no previous version had its whole
        // history read as new.
        let changelogs = BTreeMap::from([("widget".to_string(), CHANGELOG.to_string())]);

        let arriving = BTreeMap::from([("widget".to_string(), None)]);
        assert!(changelog_candidates(&changelogs, &arriving).is_empty());

        // Absent from the map entirely is the same answer.
        assert!(changelog_candidates(&changelogs, &BTreeMap::new()).is_empty());

        // A package the target has keeps the source it was written for.
        let updating = BTreeMap::from([("widget".to_string(), Some("0.6.3-1".to_string()))]);
        let found = changelog_candidates(&changelogs, &updating);
        assert_eq!(
            found.get(&2344815).map(String::as_str),
            Some("named in widget's new changelog entries")
        );
        assert!(!found.contains_key(&2200001));
    }
}
