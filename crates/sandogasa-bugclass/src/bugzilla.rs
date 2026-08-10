// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bugzilla-specific bug classification.

use std::collections::HashSet;

use sandogasa_bugzilla::BzClient;
use sandogasa_bugzilla::models::Bug;

use crate::BugKind;

/// Tracker bug IDs used for FTBFS / FTI classification. Populated
/// by [`lookup_trackers`].
#[derive(Debug, Clone, Default)]
pub struct TrackerIds {
    pub ftbfs: HashSet<u64>,
    pub fti: HashSet<u64>,
}

/// Bugzilla product for Fedora packages.
pub const FEDORA: &str = "Fedora";

/// Bugzilla product for EPEL packages.
pub const FEDORA_EPEL: &str = "Fedora EPEL";

/// The Bugzilla product and version that a distro branch's bugs are
/// filed against, or `None` for a branch with no Bugzilla
/// equivalent.
///
/// The version is not the branch name. Fedora numbers its versions
/// bare — branch `f43` files against version `43` — while EPEL uses
/// the branch name but without any minor version, so `epel10.3`
/// files against `epel10`. Rawhide is the one case where the two
/// agree.
///
/// CentOS Stream (`c10s`) and ELN have no Bugzilla product here and
/// return `None`; a caller must skip them rather than search with a
/// version that matches nothing, which is silently indistinguishable
/// from a package having no bugs.
pub fn product_version_for_branch(branch: &str) -> Option<(&'static str, String)> {
    if branch == "rawhide" {
        return Some((FEDORA, branch.to_string()));
    }
    if let Some(rest) = branch.strip_prefix("epel") {
        // epel10.3 and friends share the epel10 product version.
        let major: String = rest.chars().take_while(char::is_ascii_digit).collect();
        return (!major.is_empty()).then(|| (FEDORA_EPEL, format!("epel{major}")));
    }
    let number = branch.strip_prefix('f').unwrap_or(branch);
    (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
        .then(|| (FEDORA, number.to_string()))
}

/// Extract the new version from a release-monitoring bug summary of
/// the form `"<component>-<version> is available"`.
///
/// The match is anchored on `component`: the summary must continue
/// with a `-` and then a digit, so a component is not taken for the
/// prefix of a longer package name — `rust-ctor` does not answer for
/// `rust-ctor-proc-macro-0.0.13`, whose remainder begins a name
/// segment rather than a version.
///
/// Anchoring is what makes this reliable, and why there is no
/// version of this that parses a summary without being told the
/// package. `rust-md-5-0.10.6` splits as `rust-md-5` + `0.10.6` or
/// `rust-md` + `5-0.10.6` with equal justification — both are real
/// Fedora package names — and only the caller's candidate settles
/// it. Splitting from the right the way [`sandogasa_koji::parse_nvr`]
/// does is no help either: a summary carries no release field, and a
/// version may itself contain a `-` (`0.5a1.dev-r2707`).
pub fn extract_new_version(summary: &str, component: &str) -> Option<String> {
    let body = summary.trim().strip_suffix(" is available")?;
    let version = body.strip_prefix(component)?.strip_prefix('-')?;
    version
        .starts_with(|c: char| c.is_ascii_digit())
        .then(|| version.to_string())
}

/// Extract the package name from a package-review bug summary of the
/// form `"Review Request: <package> - <description>"`. The package
/// name is the first word after the prefix; a summary that does not
/// start with it is not a review request.
pub fn review_request_package(summary: &str) -> Option<String> {
    const PREFIX: &str = "review request:";
    let summary = summary.trim();
    let (prefix, rest) = summary.split_at_checked(PREFIX.len())?;
    prefix.eq_ignore_ascii_case(PREFIX).then_some(())?;
    Some(rest.split_whitespace().next()?.to_string())
}

/// Classify a Bugzilla bug into a [`BugKind`].
///
/// Returns `Review` for bugs filed against the "Package Review"
/// component, regardless of content.
pub fn classify(bug: &Bug, trackers: &TrackerIds) -> BugKind {
    if bug.component.iter().any(|c| c == "Package Review") {
        return BugKind::Review;
    }

    // FTBFS / FTI: check if the bug blocks a known tracker.
    if bug.blocks.iter().any(|id| trackers.ftbfs.contains(id)) {
        return BugKind::Ftbfs;
    }
    if bug.blocks.iter().any(|id| trackers.fti.contains(id)) {
        return BugKind::Fti;
    }

    // Security: summary starts with CVE- or SecurityTracking keyword.
    if bug.summary.starts_with("CVE-")
        || bug
            .keywords
            .iter()
            .any(|k| k == "SecurityTracking" || k == "Security")
    {
        return BugKind::Security;
    }

    // Update request: FutureFeature keyword, and a summary naming
    // this component and a new version. Going through
    // `extract_new_version` rather than a bare `starts_with` anchors
    // the component the same way the version comparison does, so a
    // bug against `rust-ctor` is not labelled an update request on
    // the strength of a `rust-ctor-proc-macro-0.0.13` summary.
    if bug.keywords.iter().any(|k| k == "FutureFeature")
        && let Some(component) = bug.component.first()
        && extract_new_version(&bug.summary, component).is_some()
    {
        return BugKind::Update;
    }

    // Branch request.
    if bug.summary.to_lowercase().contains("branch") {
        return BugKind::Branch;
    }

    BugKind::Other
}

/// The FTBFS and FailsToInstall tracker bugs for one branch.
///
/// Either may be absent when the lookup failed; both are absent for
/// a branch with no trackers.
#[derive(Debug, Clone, Default)]
pub struct BranchTrackers {
    pub ftbfs: Option<u64>,
    pub fti: Option<u64>,
}

/// The Bugzilla aliases of a branch's FTBFS and FailsToInstall
/// tracker bugs, or `None` when the branch has none.
///
/// Only Fedora has these. `RAWHIDEFTBFS` and `RAWHIDEFailsToInstall`
/// follow Rawhide as it moves, so they need no version; a numbered
/// branch has its own pair. EPEL has no equivalent trackers, so an
/// EPEL branch cannot be classified this way at all — hence `None`
/// rather than a guess.
pub fn tracker_aliases_for_branch(branch: &str) -> Option<(String, String)> {
    if branch == "rawhide" {
        return Some((
            "RAWHIDEFTBFS".to_string(),
            "RAWHIDEFailsToInstall".to_string(),
        ));
    }
    let (product, _) = product_version_for_branch(branch)?;
    // Only Fedora has these; the EPEL names tracker_names can build
    // are for matching bot summaries, not for a lookup that would
    // find nothing.
    (product == FEDORA).then(|| tracker_names(branch)).flatten()
}

/// The FTBFS or FailsToInstall kind a bot-filed summary declares for
/// `branch`, or `None` when the summary is not one of those forms or
/// names a different release.
///
/// Fedora's bots file these with fixed wording, and the wording names
/// the release, so a summary can stand in for the tracker — which
/// matters for EPEL, where no trackers exist. Two forms:
///
/// - `F44FailsToInstall: <component>`, from "Fedora Fails To Install".
///   The release is in the prefix, explicit and stable, so this is as
///   reliable as the tracker.
/// - `<component>: FTBFS in Fedora rawhide/f44`, from the mass
///   rebuild. This one names *whichever Fedora version was Rawhide
///   when the bug was filed*, so the numbered token is matched and
///   the literal `rawhide` is not: a bug stamped `rawhide/f44` is
///   about f44 forever, while today's Rawhide has moved on. Matching
///   `rawhide` would keep resurrecting bugs from past cycles.
///
/// Human-filed FTBFS and FTI bugs have no fixed wording at all and
/// name no release, so nothing can be concluded from their summaries;
/// they are recognized only by the tracker they block.
///
/// The bug's `version` field is deliberately not consulted. It is not
/// reliably updated: a package that first failed when Rawhide was f42
/// gets a fresh bug for each later cycle it stays broken, and the old
/// one keeps `version: rawhide` while its summary still says
/// `rawhide/f42`. An update for f42 is what closes that bug, so the
/// summary's numbered token is right where the version field is not.
pub fn kind_from_summary(summary: &str, component: &str, branch: &str) -> Option<BugKind> {
    let (ftbfs_name, fti_name) = tracker_names(branch)?;
    let summary = summary.trim();
    if summary
        .to_ascii_uppercase()
        .starts_with(&format!("{}: ", fti_name.to_ascii_uppercase()))
        && summary.ends_with(component)
    {
        return Some(BugKind::Fti);
    }
    // `<component>: FTBFS in ...`, where the tail must name this
    // branch by its numbered or EPEL token — never by "rawhide".
    let tail = summary
        .strip_prefix(component)?
        .strip_prefix(": FTBFS in ")?
        .to_ascii_lowercase();
    let token = ftbfs_name.trim_end_matches("FTBFS").to_ascii_lowercase();
    tail.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| word == token)
        .then_some(BugKind::Ftbfs)
}

/// The names Fedora uses for a branch's FTBFS and FailsToInstall
/// trackers, which are also the prefixes its bots put in summaries:
/// `F44FTBFS` / `F44FailsToInstall`, `EPEL9FTBFS` /
/// `EPEL9FailsToInstall`.
///
/// `None` for Rawhide, whose trackers are named `RAWHIDE*` and whose
/// summaries name a moving version — and for a branch with no
/// Bugzilla product at all.
fn tracker_names(branch: &str) -> Option<(String, String)> {
    let (product, version) = product_version_for_branch(branch)?;
    if version == "rawhide" {
        return None;
    }
    let stem = if product == FEDORA_EPEL {
        version.to_ascii_uppercase()
    } else {
        format!("F{version}")
    };
    Some((format!("{stem}FTBFS"), format!("{stem}FailsToInstall")))
}

/// Look up one branch's FTBFS and FailsToInstall tracker bug IDs.
///
/// Best-effort: a branch without trackers, or a failed lookup, gives
/// an empty result, and a caller must then reach no conclusion rather
/// than treat a bug as not-FTBFS.
pub async fn lookup_branch_trackers(bz: &BzClient, branch: &str) -> BranchTrackers {
    let Some((ftbfs_alias, fti_alias)) = tracker_aliases_for_branch(branch) else {
        return BranchTrackers::default();
    };
    let query = format!("alias={ftbfs_alias}&alias={fti_alias}");
    let Ok(bugs) = bz.search(&query, 0).await else {
        return BranchTrackers::default();
    };
    let mut trackers = BranchTrackers::default();
    for bug in &bugs {
        if bug.alias.iter().any(|a| a == &ftbfs_alias) {
            trackers.ftbfs = Some(bug.id);
        } else if bug.alias.iter().any(|a| a == &fti_alias) {
            trackers.fti = Some(bug.id);
        }
    }
    trackers
}

/// Look up FTBFS and FTI tracker bug IDs for the given Fedora
/// versions. Always includes the permanent Rawhide trackers.
pub async fn lookup_trackers(bz: &BzClient, versions: &[u32], verbose: bool) -> TrackerIds {
    let mut ftbfs = HashSet::new();
    let mut fti = HashSet::new();

    let mut aliases = vec![
        "RAWHIDEFTBFS".to_string(),
        "RAWHIDEFailsToInstall".to_string(),
    ];
    for ver in versions {
        aliases.push(format!("F{ver}FTBFS"));
        aliases.push(format!("F{ver}FailsToInstall"));
    }

    if verbose {
        eprintln!("[bugclass] looking up FTBFS/FTI tracker bugs");
    }

    let alias_params: Vec<String> = aliases.iter().map(|a| format!("alias={a}")).collect();
    let query = alias_params.join("&");
    if let Ok(bugs) = bz.search(&query, 0).await {
        for bug in &bugs {
            for alias in &bug.alias {
                if alias.ends_with("FTBFS") {
                    ftbfs.insert(bug.id);
                } else if alias.ends_with("FailsToInstall") {
                    fti.insert(bug.id);
                }
            }
        }
    }

    if verbose {
        eprintln!(
            "[bugclass] found {} FTBFS and {} FTI tracker(s)",
            ftbfs.len(),
            fti.len()
        );
    }

    TrackerIds { ftbfs, fti }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct a `Bug` via serde — `Bug` is `#[non_exhaustive]`,
    /// so literal construction is reserved to its own crate.
    fn make_bug(summary: &str, component: &str, keywords: &[&str], blocks: &[u64]) -> Bug {
        serde_json::from_value(serde_json::json!({
            "id": 1,
            "summary": summary,
            "status": "NEW",
            "resolution": "",
            "product": "Fedora",
            "component": [component],
            "severity": "",
            "priority": "",
            "assigned_to": "",
            "creator": "",
            "creation_time": "2026-01-01T00:00:00Z",
            "last_change_time": "2026-01-01T00:00:00Z",
            "keywords": keywords,
            "blocks": blocks,
        }))
        .unwrap()
    }

    #[test]
    fn product_version_for_branch_uses_bugzilla_spelling() {
        // Fedora's versions are bare numbers: branch f43 is version
        // 43, and querying for "f43" matches nothing at all.
        assert_eq!(
            product_version_for_branch("f43"),
            Some(("Fedora", "43".to_string()))
        );
        assert_eq!(
            product_version_for_branch("rawhide"),
            Some(("Fedora", "rawhide".to_string()))
        );
        // EPEL keeps the branch spelling, minus any minor version.
        assert_eq!(
            product_version_for_branch("epel9"),
            Some(("Fedora EPEL", "epel9".to_string()))
        );
        assert_eq!(
            product_version_for_branch("epel10.3"),
            Some(("Fedora EPEL", "epel10".to_string()))
        );
        // A bare number is accepted as already-Bugzilla-shaped.
        assert_eq!(
            product_version_for_branch("43"),
            Some(("Fedora", "43".to_string()))
        );
    }

    #[test]
    fn product_version_for_branch_rejects_branches_bugzilla_lacks() {
        // CentOS Stream and ELN have no product here. Returning a
        // guess would build a query that matches nothing, which reads
        // as "this package has no bugs".
        assert_eq!(product_version_for_branch("c10s"), None);
        assert_eq!(product_version_for_branch("eln"), None);
        assert_eq!(product_version_for_branch(""), None);
        assert_eq!(product_version_for_branch("epel"), None);
    }

    #[test]
    fn kind_from_summary_reads_the_bots_wording() {
        // Real summary from bug 2437417.
        assert_eq!(
            kind_from_summary(
                "F44FailsToInstall: gnome-shell-extension-argos",
                "gnome-shell-extension-argos",
                "f44"
            ),
            Some(BugKind::Fti)
        );
        // Real summary from bug 2433898. The mass rebuild stamps the
        // Fedora version that was Rawhide at the time.
        assert_eq!(
            kind_from_summary("cachelib: FTBFS in Fedora rawhide/f44", "cachelib", "f44"),
            Some(BugKind::Ftbfs)
        );
        // Another release's bug is not this branch's business.
        assert_eq!(
            kind_from_summary("cachelib: FTBFS in Fedora rawhide/f44", "cachelib", "f43"),
            None
        );
        assert_eq!(
            kind_from_summary(
                "F44FailsToInstall: gnome-shell-extension-argos",
                "gnome-shell-extension-argos",
                "f43"
            ),
            None
        );
    }

    #[test]
    fn kind_from_summary_will_not_match_rawhide_by_name() {
        // "rawhide/f44" is about f44 forever, but Rawhide has moved
        // on; matching the word would resurrect every past cycle's
        // bugs into today's Rawhide.
        assert_eq!(
            kind_from_summary(
                "cachelib: FTBFS in Fedora rawhide/f44",
                "cachelib",
                "rawhide"
            ),
            None
        );
    }

    #[test]
    fn kind_from_summary_ignores_human_wording() {
        // No fixed form and no release named — only the tracker can
        // classify these.
        assert_eq!(
            kind_from_summary(
                "python-pyemd fails to build with setuptools 74+",
                "python-pyemd",
                "f44"
            ),
            None
        );
        assert_eq!(
            kind_from_summary(
                "[abrt] glycin-loaders: __libc_recv(): killed by SIGSYS",
                "rust-glycin",
                "f44"
            ),
            None
        );
    }

    #[test]
    fn kind_from_summary_covers_epel_where_trackers_do_not_exist() {
        // EPEL has no trackers, so the summary is the only signal
        // available should its bots start filing these.
        assert_eq!(
            kind_from_summary("EPEL9FailsToInstall: fish", "fish", "epel9"),
            Some(BugKind::Fti)
        );
        assert_eq!(tracker_aliases_for_branch("epel9"), None);
    }

    #[test]
    fn tracker_aliases_only_exist_for_fedora() {
        assert_eq!(
            tracker_aliases_for_branch("f43"),
            Some(("F43FTBFS".to_string(), "F43FailsToInstall".to_string()))
        );
        // The rawhide aliases follow rawhide as it moves, so they
        // carry no version.
        assert_eq!(
            tracker_aliases_for_branch("rawhide"),
            Some((
                "RAWHIDEFTBFS".to_string(),
                "RAWHIDEFailsToInstall".to_string()
            ))
        );
        // EPEL has no FTBFS or FailsToInstall trackers.
        assert_eq!(tracker_aliases_for_branch("epel9"), None);
        assert_eq!(tracker_aliases_for_branch("c10s"), None);
    }

    #[test]
    fn extract_new_version_handles_real_summaries() {
        assert_eq!(
            extract_new_version(
                "transmission-remote-cli-1.7.1 is available",
                "transmission-remote-cli"
            )
            .as_deref(),
            Some("1.7.1")
        );
        // Version containing a dash is preserved after the first one.
        assert_eq!(
            extract_new_version(
                "python-peak-rules-0.5a1.dev-r2707 is available",
                "python-peak-rules"
            )
            .as_deref(),
            Some("0.5a1.dev-r2707")
        );
    }

    #[test]
    fn extract_new_version_rejects_unrecognized() {
        assert_eq!(extract_new_version("something unrelated", "foo"), None);
        assert_eq!(
            extract_new_version("otherpkg-1.0 is available", "foo"),
            None
        );
    }

    #[test]
    fn extract_new_version_handles_names_ending_in_a_digit() {
        // rust-md-5, rust-sha-1, rust-utf-8 and friends are real
        // packages: the last name segment is a digit, so the version
        // boundary cannot be found without knowing the name.
        assert_eq!(
            extract_new_version("rust-md-5-0.10.6 is available", "rust-md-5").as_deref(),
            Some("0.10.6")
        );
        assert_eq!(
            extract_new_version("rust-sha-1-0.10.1 is available", "rust-sha-1").as_deref(),
            Some("0.10.1")
        );
    }

    #[test]
    fn extract_new_version_rejects_a_longer_package_name() {
        // `rust-ctor` is a prefix of `rust-ctor-proc-macro`, but the
        // bug is about the latter: what follows the prefix is another
        // name segment, not a version.
        assert_eq!(
            extract_new_version("rust-ctor-proc-macro-0.0.13 is available", "rust-ctor"),
            None
        );
        // The package it really names still resolves.
        assert_eq!(
            extract_new_version(
                "rust-ctor-proc-macro-0.0.13 is available",
                "rust-ctor-proc-macro"
            )
            .as_deref(),
            Some("0.0.13")
        );
        // A name that merely starts the same is not a match either.
        assert_eq!(
            extract_new_version("rust-ctor2-1.0 is available", "rust-ctor"),
            None
        );
    }

    #[test]
    fn review_request_package_handles_real_summaries() {
        // Package names contain dashes themselves, so the split is on
        // whitespace, not on the description separator.
        assert_eq!(
            review_request_package(
                "Review Request: rust-linktime-proc-macro - Proc-macro helpers \
                 for linktime crates"
            )
            .as_deref(),
            Some("rust-linktime-proc-macro")
        );
        // No description, an em dash separator, and lowercased prefix.
        assert_eq!(
            review_request_package("Review Request: rust-macrotest").as_deref(),
            Some("rust-macrotest")
        );
        assert_eq!(
            review_request_package("Review request: python-foo — a thing").as_deref(),
            Some("python-foo")
        );
    }

    #[test]
    fn review_request_package_rejects_other_summaries() {
        assert_eq!(review_request_package("rust-foo-1.0 is available"), None);
        assert_eq!(review_request_package("Review Request:"), None);
        assert_eq!(review_request_package("short"), None);
    }

    #[test]
    fn classify_review() {
        let trackers = TrackerIds::default();
        let bug = make_bug("Review Request: rust-foo", "Package Review", &[], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Review);
    }

    #[test]
    fn classify_security_by_summary() {
        let trackers = TrackerIds::default();
        let bug = make_bug("CVE-2026-1234 foo: buffer overflow", "foo", &[], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Security);
    }

    #[test]
    fn classify_security_by_keyword() {
        let trackers = TrackerIds::default();
        let bug = make_bug("foo: buffer overflow", "foo", &["SecurityTracking"], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Security);
    }

    #[test]
    fn classify_update_request() {
        let trackers = TrackerIds::default();
        let bug = make_bug("fish-4.0 is available", "fish", &["FutureFeature"], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Update);
        // A name that merely ends in a digit is still this component's
        // update request.
        let bug = make_bug(
            "rust-md-5-0.10.6 is available",
            "rust-md-5",
            &["FutureFeature"],
            &[],
        );
        assert_eq!(classify(&bug, &trackers), BugKind::Update);
    }

    #[test]
    fn classify_update_request_is_anchored_on_the_component() {
        let trackers = TrackerIds::default();
        // The summary is about rust-ctor-proc-macro, so this is not
        // an update request for rust-ctor, whose name merely prefixes
        // it.
        let bug = make_bug(
            "rust-ctor-proc-macro-0.0.13 is available",
            "rust-ctor",
            &["FutureFeature"],
            &[],
        );
        assert_eq!(classify(&bug, &trackers), BugKind::Other);
    }

    #[test]
    fn classify_branch_request() {
        let trackers = TrackerIds::default();
        let bug = make_bug("Please branch rust-foo for epel10", "foo", &[], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Branch);
    }

    #[test]
    fn classify_ftbfs() {
        let trackers = TrackerIds {
            ftbfs: HashSet::from([999]),
            fti: HashSet::new(),
        };
        let bug = make_bug("foo FTBFS in rawhide", "foo", &[], &[999]);
        assert_eq!(classify(&bug, &trackers), BugKind::Ftbfs);
    }

    #[test]
    fn classify_fti() {
        let trackers = TrackerIds {
            ftbfs: HashSet::new(),
            fti: HashSet::from([888]),
        };
        let bug = make_bug("foo fails to install", "foo", &[], &[888]);
        assert_eq!(classify(&bug, &trackers), BugKind::Fti);
    }

    #[test]
    fn classify_other() {
        let trackers = TrackerIds::default();
        let bug = make_bug("foo crashes on startup", "foo", &[], &[]);
        assert_eq!(classify(&bug, &trackers), BugKind::Other);
    }
}
