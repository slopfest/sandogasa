// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reverse-dependency breakage checker for Koji side tags and Bodhi updates.
//!
//! Given a set of updated packages (from a side tag or Bodhi update),
//! compares old vs new subpackage Provides to detect removed capabilities,
//! then finds reverse dependencies in the stable repo that would break.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

use rayon::prelude::*;
use sandogasa_review::Resolution;
use serde::Serialize;

// ---- Public types ----

/// What kind of input the user provided.
pub enum InputKind {
    /// A Koji side tag name (e.g. "epel9-build-side-133287").
    SideTag(String),
    /// A Bodhi update alias (e.g. "FEDORA-EPEL-2026-f9eaa11e18").
    BodhiAlias(String),
}

/// Options for the check-update command.
pub struct CheckUpdateOptions {
    pub branch: Option<String>,
    pub repo: Option<String>,
    /// Override branch for @testing queries (defaults to branch).
    pub testing_branch: Option<String>,
    pub koji_profile: Option<String>,
    pub verbose: bool,
    /// Offer to fix stale side-tag repodata interactively (regen
    /// the repo, or confirm continuing with stale data). Off for
    /// `--json` and when stdin isn't a terminal.
    pub interactive: bool,
}

/// A Provide that changed between old and new versions.
#[derive(Debug, Clone, Serialize)]
pub struct ChangedProvide {
    /// The old Provide string (e.g. "tor = 0.4.9.5-1.el9").
    pub old: String,
    /// The new Provide string, if the name still exists.
    pub new: Option<String>,
}

impl ChangedProvide {
    /// Whether this is a version update (name still provided)
    /// vs a true removal.
    pub fn is_updated(&self) -> bool {
        self.new.is_some()
    }
}

/// A source package's version change in the update — its stable V-R
/// (`None` when the package is newly introduced) and the V-R the update
/// ships. Drives the grouped-by-version-transition summary.
#[derive(Debug, Clone, Serialize)]
pub struct PackageChange {
    pub package: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    pub new: String,
}

/// A single broken Requires entry.
#[derive(Debug, Clone, Serialize)]
pub struct BrokenRequires {
    /// The Requires string that would break.
    pub dep: String,
    /// The Provide that was removed or updated.
    pub changed_provide: String,
}

/// An unsatisfied Requires in an updated package's subpackages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnsatisfiedDep {
    /// The source package.
    pub package: String,
    /// The unsatisfied Requires string.
    pub dep: String,
    /// For a boolean/rich dep, the specific capabilities that didn't
    /// resolve (so the reader can see *which* part is the problem).
    /// Empty for a plain dep (where `dep` is itself the capability).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
}

/// A side tag repo whose metadata still serves a stale V-R for
/// a build that koji says is tagged at a newer NVR.
///
/// When present, the provides comparison may have missed changes
/// from this source, so the report's reverse-dep list is
/// incomplete. The user-side remedy is `koji regen-repo <side-tag>`.
#[derive(Debug, Clone, Serialize)]
pub struct StaleSideTag {
    /// Source package name.
    pub package: String,
    /// Expected NVR per koji (`name-version-release`).
    pub expected_nvr: String,
    /// V-R actually served by the side tag repo, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_vr: Option<String>,
}

/// Result of checking a single reverse dependency.
#[derive(Debug, Serialize)]
pub struct RevDepResult {
    /// "ok" or "broken".
    pub status: String,
    /// Broken Requires, if any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<BrokenRequires>,
}

/// Full result of the check-update command.
#[derive(Debug, Serialize)]
pub struct CheckUpdateReport {
    pub input: String,
    pub branch: String,
    /// Repository class (e.g. "@epel"), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub updated_packages: Vec<String>,
    /// Per-package version changes (old V-R → new V-R, with `old: None`
    /// for newly introduced packages), for the summary's
    /// grouped-by-version-transition view.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<PackageChange>,
    /// Whether full provides comparison was performed.
    pub full_analysis: bool,
    /// Provides that changed (updated version or truly removed).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_provides: Vec<ChangedProvide>,
    /// Requires of updated packages not satisfiable on the target.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub installability_issues: Vec<UnsatisfiedDep>,
    /// Side-tag staleness warnings. When non-empty the
    /// reverse-dep analysis is incomplete.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stale_side_tag: Vec<StaleSideTag>,
    /// Why the full Provides analysis was skipped, when it was
    /// (`full_analysis == false`). Drives a precise note instead of a
    /// generic "no side tag" message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<SkipReason>,
    pub reverse_deps: BTreeMap<String, RevDepResult>,
}

/// Why a full Provides comparison couldn't be run, so the report can
/// explain the *actual* gap rather than always blaming a missing side
/// tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkipReason {
    /// Bodhi has the update in testing (and koji has it tagged), but the
    /// `@testing` repodata fedrq reads doesn't carry the new NVR(s) yet
    /// — propagation/compose lag, or fedrq serving stale metadata.
    TestingLag { expected: Vec<String> },
    /// EPEL 10+ `@testing` isn't queryable by fedrq yet; a side tag is
    /// needed to compare Provides.
    Epel10TestingUnsupported,
    /// No comparison source at all: not in `@testing` and no side tag.
    /// `bodhi_status` is the update's status when known.
    NoSource { bodhi_status: Option<String> },
}

/// A blocking finding the reviewer can curate (keep / explain / remove)
/// before karma is cast. These are exactly the findings that drive a
/// downvote: installability problems and reverse-dependency breakage
/// (the latter grouped by the changed Provide that causes it, so one
/// decision covers every package broken by that Provide).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    /// An unsatisfied Requires in an updated package's subpackages.
    Installability(UnsatisfiedDep),
    /// Reverse-dep breakage grouped by the changed Provide behind it.
    ReverseDepBreak {
        provide: String,
        packages: Vec<String>,
    },
}

impl Finding {
    /// One-line summary shown at the keep/explain/remove prompt and in
    /// the "addressed by the reviewer" section.
    pub fn summary(&self) -> String {
        match self {
            Finding::Installability(d) if d.unresolved.is_empty() => {
                format!("{}: unsatisfied dep `{}`", d.package, d.dep)
            }
            Finding::Installability(d) => format!(
                "{}: unsatisfied dep `{}` [unresolved: {}]",
                d.package,
                d.dep,
                d.unresolved.join(", ")
            ),
            Finding::ReverseDepBreak { provide, packages } => format!(
                "`{provide}` — breaks {} package(s): {}",
                packages.len(),
                packages.join(", ")
            ),
        }
    }
}

// ---- Public functions ----

/// Detect what kind of input the user provided.
pub fn detect_input_type(input: &str) -> InputKind {
    // Strip URL prefix to get the alias.
    let candidate = if let Some(rest) = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
    {
        // Last path segment is the alias.
        rest.rsplit('/').next().unwrap_or(rest)
    } else {
        input
    };

    // Bodhi aliases look like FEDORA-2026-abc123 or FEDORA-EPEL-2026-abc123.
    if candidate.starts_with("FEDORA-") && candidate.contains("-20") {
        InputKind::BodhiAlias(candidate.to_string())
    } else {
        InputKind::SideTag(input.to_string())
    }
}

/// Extract the source package name from an NVR string.
///
/// E.g. "rust-uucore-0.0.28-2.el9" → "rust-uucore"
pub fn parse_nvr(nvr: &str) -> Option<&str> {
    sandogasa_koji::parse_nvr_name(nvr)
}

/// List NVRs in a Koji tag via `koji list-tagged --quiet`.
pub fn koji_list_tagged(tag: &str, profile: Option<&str>) -> Result<Vec<String>, String> {
    // `--latest` (via `list_tagged`) so a side tag that accumulated
    // superseded builds — e.g. 6.7.0 then 6.7.1 of the same package —
    // yields only the current NVR, not the old one. Otherwise the
    // stale-side-tag check would compare the superseded 6.7.0 against
    // the repodata's 6.7.1 and wrongly flag it.
    Ok(sandogasa_koji::list_tagged(tag, profile, None)?
        .into_iter()
        .map(|b| b.nvr)
        .collect())
}

/// List binary RPM names for a build via `koji buildinfo`.
///
/// Parses the RPMs section, returning binary package names
/// (excluding `.src.rpm` entries).
pub fn koji_build_rpms(nvr: &str, profile: Option<&str>) -> Result<Vec<String>, String> {
    sandogasa_koji::build_rpms(nvr, profile)
}

/// Run the check-update analysis.
pub fn check_update(input: &str, opts: &CheckUpdateOptions) -> Result<CheckUpdateReport, String> {
    // Phase 0: Determine side tag, NVRs, branch, Bodhi release branch,
    // and Bodhi update status (None for side-tag input).
    let (side_tag, nvrs, branch, bodhi_branch, bodhi_status) = match detect_input_type(input) {
        InputKind::SideTag(tag) => {
            let branch = match opts.branch.clone() {
                Some(b) => b,
                None => {
                    let b = infer_branch_for_side_tag(&tag)?;
                    if opts.verbose {
                        eprintln!("[check-update] inferred branch {b} from side tag {tag}");
                    }
                    b
                }
            };
            let nvrs = koji_list_tagged(&tag, opts.koji_profile.as_deref())?;
            (Some(tag), nvrs, branch, None, None)
        }
        InputKind::BodhiAlias(alias) => {
            let info = fetch_bodhi_update(&alias)?;

            let branch = resolve_bodhi_branch(opts.branch.clone(), info.release_name.as_deref())?;

            // If backed by a side tag, use koji to get the full list.
            let (side_tag, nvrs) = if let Some(tag) = info.side_tag {
                let koji_nvrs = koji_list_tagged(&tag, opts.koji_profile.as_deref())?;
                (Some(tag), koji_nvrs)
            } else {
                (None, info.nvrs)
            };

            (
                side_tag,
                nvrs,
                branch,
                info.release_branch,
                Some(info.status),
            )
        }
    };

    // Extract unique source package names from NVRs.
    let updated_packages: Vec<String> = nvrs
        .iter()
        .filter_map(|nvr| parse_nvr(nvr))
        .map(|s| s.to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if opts.verbose {
        eprintln!(
            "[check-update] updated packages: {}",
            updated_packages.join(", ")
        );
    }

    if updated_packages.is_empty() {
        return Ok(CheckUpdateReport {
            input: input.to_string(),
            branch: branch.clone(),
            repo: opts.repo.clone(),
            updated_packages: vec![],
            changes: vec![],
            full_analysis: side_tag.is_some(),
            changed_provides: vec![],
            installability_issues: vec![],
            stale_side_tag: vec![],
            skip_reason: None,
            reverse_deps: BTreeMap::new(),
        });
    }

    // Set up fedrq for the stable repo.
    let stable_fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(branch.clone()),
        repo: opts.repo.clone(),
    };

    // Per-package version changes for the summary: one fedrq source
    // query gives each package's stable V-R (absent ⇒ newly introduced),
    // paired with the V-R the update ships. Cheap (a single batched
    // call) and drives the grouped-by-version-transition view.
    let stable_srcs = stable_fedrq.src_nvrs(&updated_packages).unwrap_or_default();
    let old_vr: BTreeMap<&str, String> = stable_srcs
        .iter()
        .filter_map(|nvr| sandogasa_koji::parse_nvr(nvr).map(|(n, v, r)| (n, format!("{v}-{r}"))))
        .collect();
    let new_vr: BTreeMap<&str, String> = nvrs
        .iter()
        .filter_map(|nvr| sandogasa_koji::parse_nvr(nvr).map(|(n, v, r)| (n, format!("{v}-{r}"))))
        .collect();
    let changes: Vec<PackageChange> = updated_packages
        .iter()
        .map(|p| PackageChange {
            package: p.clone(),
            old: old_vr.get(p.as_str()).cloned(),
            new: new_vr.get(p.as_str()).cloned().unwrap_or_default(),
        })
        .collect();

    // Get subpackage names for the updated source packages.
    if opts.verbose {
        eprintln!("[check-update] querying subpackage names");
    }

    let all_subpkg_names: Vec<String> = updated_packages
        .par_iter()
        .flat_map(|srpm| filter_none(stable_fedrq.subpkgs_names(srpm).unwrap_or_default()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if opts.verbose {
        eprintln!(
            "[check-update] {} subpackages: {}",
            all_subpkg_names.len(),
            all_subpkg_names.join(", "),
        );
    }

    // Determine the branch for querying new provides (@testing or side tag).
    let new_branch = opts
        .testing_branch
        .clone()
        .or_else(|| testing_branch_from_side_tag(side_tag.as_deref()))
        .or(bodhi_branch)
        .unwrap_or_else(|| branch.clone());

    let side_tag_fedrq = side_tag.as_ref().map(|tag| sandogasa_fedrq::Fedrq {
        branch: None,
        repo: Some(format!("@koji:{tag}")),
    });

    // Prefer @testing when the update has been pushed to testing,
    // since it has authoritative repo metadata (no staleness issue).
    let testing_fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(new_branch),
        repo: Some("@testing".to_string()),
    };

    // Two gates: Bodhi must say the update is in testing (when we know),
    // and @testing must actually carry one of the expected NVRs (covers
    // both side-tag input and a stale @testing metadata snapshot).
    let bodhi_in_testing = match bodhi_status.as_deref() {
        Some("testing") => true,
        Some(other) => {
            if opts.verbose {
                eprintln!(
                    "[check-update] Bodhi status is '{other}', \
                     not 'testing'; skipping @testing"
                );
            }
            false
        }
        None => true,
    };
    let has_testing = bodhi_in_testing
        && testing_has_update_nvrs(&nvrs, |src| {
            testing_fedrq.subpkgs_nvrs(src).unwrap_or_default()
        });

    if has_testing {
        if opts.verbose {
            eprintln!("[check-update] using @testing for new provides");
        }
        // Get binary RPM names for installability checking.
        let testing_bins: Vec<String> = updated_packages
            .iter()
            .flat_map(|pkg| filter_none(testing_fedrq.subpkgs_names(pkg).unwrap_or_default()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let changed =
            compute_changed_provides_via_subpkgs(&updated_packages, &stable_fedrq, &testing_fedrq);
        return run_provides_analysis(
            input,
            &branch,
            &updated_packages,
            &changes,
            &testing_bins,
            changed,
            vec![],
            &stable_fedrq,
            &testing_fedrq,
            opts,
        );
    }

    if let Some(ref side_fq) = side_tag_fedrq {
        let tag = side_tag
            .as_deref()
            .expect("side_tag_fedrq implies side_tag");
        if opts.verbose {
            eprintln!("[check-update] comparing provides via koji + side tag");
        }
        // Binary RPM names from koji (for installability checking).
        let koji_bins: Vec<String> = nvrs
            .iter()
            .flat_map(|nvr| koji_build_rpms(nvr, opts.koji_profile.as_deref()).unwrap_or_default())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        // Detect side-tag repodata that lags koji (V-R mismatch) before
        // the expensive provides comparison, so a regen can fix the data
        // the comparison runs on. One batched fedrq query maps each
        // (non-debug) binary back to its source and the V-R the repodata
        // serves; we then compare against the V-R koji expects per build.
        // -debuginfo/-debugsource live in a separate debug repo, so
        // exclude them — they'd always look missing and false-flag.
        let real_bins: Vec<&str> = koji_bins
            .iter()
            .filter(|b| !is_debug_rpm(b))
            .map(String::as_str)
            .collect();
        let side_repo_vrs =
            || source_vr_map(side_fq.pkgs_source_vr(&real_bins).unwrap_or_default());

        let mut stale_side_tag = check_side_tag_staleness(&nvrs, &side_repo_vrs(), opts.verbose);

        if !stale_side_tag.is_empty() && opts.interactive {
            stale_side_tag = resolve_stale_side_tag(
                stale_side_tag,
                tag,
                opts.koji_profile.as_deref(),
                || check_side_tag_staleness(&nvrs, &side_repo_vrs(), opts.verbose),
                opts.verbose,
            )?;
        }

        let changed = compute_changed_provides_via_koji(
            &nvrs,
            &updated_packages,
            &stable_fedrq,
            side_fq,
            opts.koji_profile.as_deref(),
            opts.verbose,
        );

        return run_provides_analysis(
            input,
            &branch,
            &updated_packages,
            &changes,
            &koji_bins,
            changed,
            stale_side_tag,
            &stable_fedrq,
            side_fq,
            opts,
        );
    }

    // No side tag and not usable via @testing: just list reverse deps.
    // Pin down *why* so the report can say something accurate.
    let skip_reason = if branch.starts_with("epel10") {
        // fedrq can't query EPEL 10+ @testing yet (no metalink); the
        // only way to compare Provides is a side tag.
        SkipReason::Epel10TestingUnsupported
    } else if bodhi_status.as_deref() == Some("testing") {
        // Bodhi pushed it, but @testing (as fedrq sees it) lacks the NVR.
        SkipReason::TestingLag {
            expected: nvrs.clone(),
        }
    } else {
        SkipReason::NoSource {
            bodhi_status: bodhi_status.clone(),
        }
    };
    if opts.verbose {
        eprintln!("[check-update] skipping provides analysis: {skip_reason:?}");
    }

    let rev_dep_sources = filter_none(
        stable_fedrq
            .whatrequires(&all_subpkg_names)
            .map_err(|e| format!("whatrequires failed: {e}"))?,
    );

    let updated_set: BTreeSet<&str> = updated_packages.iter().map(|s| s.as_str()).collect();
    let rev_deps: Vec<String> = rev_dep_sources
        .into_iter()
        .filter(|pkg| !updated_set.contains(pkg.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let reverse_deps: BTreeMap<String, RevDepResult> = rev_deps
        .into_iter()
        .map(|pkg| {
            (
                pkg,
                RevDepResult {
                    status: "unknown".to_string(),
                    issues: vec![],
                },
            )
        })
        .collect();

    Ok(CheckUpdateReport {
        input: input.to_string(),
        branch,
        repo: opts.repo.clone(),
        updated_packages,
        changes,
        full_analysis: false,
        changed_provides: vec![],
        installability_issues: vec![],
        stale_side_tag: vec![],
        skip_reason: Some(skip_reason),
        reverse_deps,
    })
}

/// Word-wrap `text` to `width` columns and prefix every line with
/// `prefix` (e.g. `"> "` for a Markdown blockquote). Collapses runs of
/// whitespace; never splits a word. Keeps notes readable both in the
/// terminal and as a wrapped blockquote in a Bodhi comment.
fn wrap_prefixed(text: &str, prefix: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && prefix.len() + line.len() + 1 + word.len() > width {
            out.push_str(prefix);
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push_str(prefix);
        out.push_str(&line);
    }
    out
}

/// The "> **Note:** …" block shown when a full Provides comparison
/// couldn't run, tailored to [`SkipReason`] so it explains the actual
/// gap rather than always blaming a missing side tag. The body is plain
/// prose; [`wrap_prefixed`] handles the blockquote prefix and wrapping.
fn skip_note(report: &CheckUpdateReport) -> String {
    let body = match &report.skip_reason {
        Some(SkipReason::TestingLag { expected }) => {
            let what = if expected.is_empty() {
                "the update".to_string()
            } else {
                format!("`{}`", expected.join("`, `"))
            };
            format!(
                "**Note:** Provides weren't compared — the `@testing` \
                 metadata didn't return {what} (likely transient mirror \
                 propagation, or a stale local metadata cache). Retry \
                 shortly, or pass `--refresh`. Reverse dependencies are \
                 listed below for manual review."
            )
        }
        Some(SkipReason::Epel10TestingUnsupported) => format!(
            "**Note:** fedrq can't query the EPEL 10 `@testing` repo yet, \
             so Provides can't be compared from the update alone — build a \
             side tag and pass it (`-b {} <side-tag>`). Reverse \
             dependencies are listed below for manual review.",
            report.branch
        ),
        Some(SkipReason::NoSource { bodhi_status }) => {
            let status = match bodhi_status.as_deref() {
                Some(s) => format!(" (Bodhi status: {s})"),
                None => String::new(),
            };
            format!(
                "**Note:** no side tag, and the update isn't in \
                 `@testing`{status}, so Provides can't be compared. \
                 Reverse dependencies are listed below for manual review."
            )
        }
        None => "**Note:** cannot compare Provides. Reverse dependencies \
                 are listed below for manual review."
            .to_string(),
    };
    wrap_prefixed(&body, "> ", 76)
}

/// In non-detailed output, how many list items to show before
/// collapsing the rest into "… and N more".
const LIST_CAP: usize = 15;

/// Extract the blocking findings a reviewer curates before karma is cast:
/// every installability issue, plus reverse-dependency breakage grouped by
/// the changed Provide that causes it (one finding per Provide, listing the
/// packages it breaks). Informational findings are not included.
pub fn blocking_findings(report: &CheckUpdateReport) -> Vec<Finding> {
    let mut findings: Vec<Finding> = report
        .installability_issues
        .iter()
        .cloned()
        .map(Finding::Installability)
        .collect();

    let mut by_provide: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (pkg, result) in &report.reverse_deps {
        if result.status != "broken" {
            continue;
        }
        for issue in &result.issues {
            by_provide
                .entry(issue.changed_provide.clone())
                .or_default()
                .push(pkg.clone());
        }
    }
    for (provide, mut packages) in by_provide {
        packages.sort();
        packages.dedup();
        findings.push(Finding::ReverseDepBreak { provide, packages });
    }
    findings
}

/// Apply the reviewer's keep/explain/remove decisions to a report.
///
/// `Removed` and `Explained` findings are stripped from the curated
/// report's counts (so neither downvotes when `derive_karma` runs on it):
/// installability issues are dropped, and reverse-dep breakages from the
/// resolved Provide are cleared (a package left with no remaining breakage
/// flips back to `ok`). `Explained` findings are also returned with their
/// justification for the "addressed by the reviewer" comment section.
/// `Keep` leaves the finding in place.
pub fn apply_resolutions(
    mut report: CheckUpdateReport,
    decisions: Vec<(Finding, Resolution)>,
) -> (CheckUpdateReport, Vec<(Finding, String)>) {
    let mut addressed = Vec::new();
    for (finding, resolution) in decisions {
        match resolution {
            Resolution::Keep => continue,
            Resolution::Explained(why) => addressed.push((finding.clone(), why)),
            Resolution::Removed => {}
        }
        match finding {
            Finding::Installability(d) => {
                report
                    .installability_issues
                    .retain(|x| !(x.package == d.package && x.dep == d.dep));
            }
            Finding::ReverseDepBreak { provide, .. } => {
                for result in report.reverse_deps.values_mut() {
                    if result.status != "broken" {
                        continue;
                    }
                    result.issues.retain(|i| i.changed_provide != provide);
                    if result.issues.is_empty() {
                        result.status = "ok".to_string();
                    }
                }
            }
        }
    }
    (report, addressed)
}

/// Render the "Issues addressed by the reviewer" section appended to the
/// posted comment, listing each explained finding with its justification.
/// Empty string when nothing was explained.
pub fn render_addressed(addressed: &[(Finding, String)]) -> String {
    if addressed.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## Issues addressed by the reviewer\n\n");
    for (finding, why) in addressed {
        out.push_str(&format!("- {} — {why}\n", finding.summary()));
    }
    out
}

/// Print a human-readable report to stdout.
pub fn print_report(report: &CheckUpdateReport, detailed: bool) {
    print!("{}", render_report(report, detailed));
}

/// Render the report as Markdown — the same text `print_report` writes
/// to stdout, reusable as a Bodhi comment body. With `detailed` off,
/// the report shows counts plus the actionable problems only (so a
/// 330-package update stays readable); with it on, every list is shown.
pub fn render_report(report: &CheckUpdateReport, detailed: bool) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(o, "# Checking update: {}\n", report.input);
    match &report.repo {
        Some(repo) => {
            let _ = writeln!(o, "**Branch:** {} ({})\n", report.branch, repo);
        }
        None => {
            let _ = writeln!(o, "**Branch:** {}\n", report.branch);
        }
    }

    o.push_str(&package_summary(report, detailed));

    if !report.stale_side_tag.is_empty() {
        let _ = writeln!(
            o,
            "> **Warning:** side tag repodata is stale for {} \
             source package(s); the reverse-dep analysis below \
             may be incomplete. Run `koji regen-repo` on the side \
             tag, then rerun this check with `--refresh` to drop \
             the cached metadata.",
            report.stale_side_tag.len()
        );
        for w in &report.stale_side_tag {
            match &w.actual_vr {
                Some(actual) => {
                    let _ = writeln!(
                        o,
                        "> - **{}**: expected `{}`, found `{}` in repodata",
                        w.package, w.expected_nvr, actual
                    );
                }
                None => {
                    let _ = writeln!(
                        o,
                        "> - **{}**: expected `{}`, no matching binary RPMs in repodata",
                        w.package, w.expected_nvr
                    );
                }
            }
        }
        let _ = writeln!(o);
    }

    if !report.full_analysis {
        // Provides couldn't be compared — explain the actual reason.
        let _ = writeln!(o, "{}\n", skip_note(report));
        let n = report.reverse_deps.len();
        if n == 0 {
            let _ = writeln!(o, "No reverse dependencies found.");
        } else if detailed {
            let _ = writeln!(o, "## Reverse dependencies ({n})\n");
            for pkg in report.reverse_deps.keys() {
                let _ = writeln!(o, "- {pkg}");
            }
        } else {
            let _ = writeln!(o, "**Reverse dependencies:** {n} (use --detailed to list).");
        }
        return o;
    }

    let updated: Vec<&ChangedProvide> = report
        .changed_provides
        .iter()
        .filter(|c| c.is_updated())
        .collect();
    let removed: Vec<&ChangedProvide> = report
        .changed_provides
        .iter()
        .filter(|c| !c.is_updated())
        .collect();
    let broken: Vec<(&String, &RevDepResult)> = report
        .reverse_deps
        .iter()
        .filter(|(_, r)| r.status != "ok")
        .collect();

    // Analysis counts (always).
    let _ = writeln!(o, "## Analysis\n");
    let _ = writeln!(
        o,
        "- **Changed Provides:** {} ({} updated, {} removed)",
        report.changed_provides.len(),
        updated.len(),
        removed.len(),
    );
    let _ = writeln!(
        o,
        "- **Installability issues:** {}",
        report.installability_issues.len()
    );
    if !report.reverse_deps.is_empty() {
        let _ = writeln!(
            o,
            "- **Reverse dependencies:** {} checked, {} would break",
            report.reverse_deps.len(),
            broken.len(),
        );
    }
    let _ = writeln!(o);

    let clean = removed.is_empty() && report.installability_issues.is_empty() && broken.is_empty();
    if clean && !detailed {
        let _ = writeln!(o, "No breakage expected.");
        return o;
    }

    // Removed Provides — the breakage signal; always shown (capped).
    if !removed.is_empty() {
        let _ = writeln!(o, "## Removed Provides ({})\n", removed.len());
        let lines: Vec<String> = removed.iter().map(|c| format!("- `{}`", c.old)).collect();
        write_capped(&mut o, lines, detailed);
        let _ = writeln!(o);
    }
    // Installability issues — actionable; always shown (capped).
    if !report.installability_issues.is_empty() {
        let _ = writeln!(
            o,
            "## Installability issues ({})\n",
            report.installability_issues.len()
        );
        let lines: Vec<String> = report
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
        write_capped(&mut o, lines, detailed);
        let _ = writeln!(o);
    }
    // Reverse deps that would break — always shown (capped).
    if !broken.is_empty() {
        let _ = writeln!(
            o,
            "## Reverse dependencies that would break ({})\n",
            broken.len()
        );
        for (pkg, result) in &broken {
            let _ = writeln!(o, "- **{pkg}**");
            for issue in &result.issues {
                let _ = writeln!(
                    o,
                    "  - `{}` (changed: `{}`)",
                    issue.dep, issue.changed_provide
                );
            }
        }
        let _ = writeln!(o);
    }

    // Detailed-only sections: the full updated-Provides list and the OK
    // reverse deps (the bulky, non-actionable parts).
    if detailed && !updated.is_empty() {
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
    if detailed {
        let ok: Vec<&String> = report
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

/// Write `lines` to `o`, capping at [`LIST_CAP`] unless `detailed`, with
/// a "… and N more" footer when truncated.
fn write_capped(o: &mut String, lines: Vec<String>, detailed: bool) {
    use std::fmt::Write as _;
    let cap = if detailed {
        lines.len()
    } else {
        LIST_CAP.min(lines.len())
    };
    for line in &lines[..cap] {
        let _ = writeln!(o, "{line}");
    }
    if cap < lines.len() {
        let _ = writeln!(o, "- … and {} more (use --detailed)", lines.len() - cap);
    }
}

/// The "## Packages" summary: a count line plus updated packages grouped
/// by their `old → new` version transition (biggest groups first), and
/// the newly-introduced packages. Group bodies and the new-package list
/// are capped unless `detailed`.
fn package_summary(report: &CheckUpdateReport, detailed: bool) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let total = report.updated_packages.len();
    let _ = writeln!(o, "## Packages\n");

    // Without per-package change data, fall back to a plain count/list.
    if report.changes.is_empty() {
        let _ = writeln!(o, "**{total}** package(s).");
        if total > 0 && (detailed || total <= LIST_CAP) {
            let _ = writeln!(o, "\n{}", report.updated_packages.join(", "));
        }
        let _ = writeln!(o);
        return o;
    }

    let new_pkgs: Vec<&str> = report
        .changes
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
    for c in &report.changes {
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
    // because `report.changes` is built from a sorted package set).
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
            "- … and {} more version transition(s) (use --detailed)",
            sorted.len() - shown
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
                "\n**New ({}):** {shown_new}, … (use --detailed)",
                new_pkgs.len()
            );
        } else {
            let _ = writeln!(o, "\n**New ({}):** {shown_new}", new_pkgs.len());
        }
    }
    let _ = writeln!(o);
    o
}

// ---- Private helpers ----

/// Filter out fedrq's "(none)" placeholder from results.
fn filter_none(items: Vec<String>) -> Vec<String> {
    items
        .into_iter()
        .filter(|s| !s.is_empty() && s != "(none)")
        .collect()
}

/// Strip "grouping" parens from a token's ends while keeping parens
/// that belong to a capability name. A trailing `)` is dropped only
/// when the token has more `)` than `(` (so it closes an outer rich-dep
/// group), and likewise a leading `(`; thus `sound-theme-freedesktop)`
/// → `sound-theme-freedesktop`, but `crate(foo)` and
/// `libc.so.6(GLIBC_2.34)(64bit)` are left intact.
fn strip_grouping_parens(token: &str) -> &str {
    let mut s = token;
    loop {
        let opens = s.matches('(').count();
        let closes = s.matches(')').count();
        if closes > opens && s.ends_with(')') {
            s = &s[..s.len() - 1];
        } else if opens > closes && s.starts_with('(') {
            s = &s[1..];
        } else {
            break;
        }
    }
    s
}

/// The bare capability name of a leaf term: its first whitespace token
/// (dropping any version constraint like `foo >= 1.0`) with grouping
/// parens stripped.
fn leaf_cap(term: &str) -> &str {
    strip_grouping_parens(term.split_whitespace().next().unwrap_or(""))
}

/// Byte range of the boolean operator `op` (a standalone whitespace-
/// delimited token) at paren depth 0 in `expr`, if any.
fn find_top_op(expr: &str, op: &str) -> Option<(usize, usize)> {
    let bytes = expr.as_bytes();
    let mut depth = 0i32;
    for (idx, &c) in bytes.iter().enumerate() {
        match c {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b' ' if depth == 0 => {
                let ws = idx + 1;
                let we = expr[ws..].find(' ').map(|p| ws + p).unwrap_or(expr.len());
                if &expr[ws..we] == op {
                    return Some((ws, we));
                }
            }
            _ => {}
        }
    }
    None
}

/// Split `expr` on the first top-level `op` token into `(before, after)`.
fn split_top<'a>(expr: &'a str, op: &str) -> Option<(&'a str, &'a str)> {
    find_top_op(expr, op).map(|(s, e)| (expr[..s].trim(), expr[e..].trim()))
}

/// Split `expr` on every top-level `op` token (≥2 parts means the
/// operator is present at the top level).
fn split_all_top<'a>(expr: &'a str, op: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut rest = expr;
    while let Some((before, after)) = split_top(rest, op) {
        parts.push(before);
        rest = after;
    }
    parts.push(rest.trim());
    parts
}

/// Whether an RPM dependency is satisfiable, given `resolves(cap)` for
/// a bare capability name. Evaluates boolean/rich deps with their real
/// semantics (a depsolver-free approximation): `A if B` requires A only
/// when B resolves; `A unless B` requires A only when B does *not*;
/// `or` needs any term; `and`/`with` need all; `without` ignores the
/// excluded term. A plain term is satisfiable when its capability
/// resolves.
fn dep_satisfiable(dep: &str, resolves: &impl Fn(&str) -> bool) -> bool {
    let dep = dep.trim();
    match dep.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        // Boolean/rich dep (fully parenthesized): evaluate the inside.
        Some(inner) => eval_boolean(inner.trim(), resolves),
        None => {
            let cap = leaf_cap(dep);
            cap.is_empty() || resolves(cap)
        }
    }
}

fn eval_boolean(expr: &str, resolves: &impl Fn(&str) -> bool) -> bool {
    // `A if B [else C]` — conditional.
    if let Some((cons, rest)) = split_top(expr, "if") {
        if let Some((cond, alt)) = split_top(rest, "else") {
            return if eval_boolean(cond, resolves) {
                eval_boolean(cons, resolves)
            } else {
                eval_boolean(alt, resolves)
            };
        }
        // No `else`: the requirement applies only when the condition holds.
        return !eval_boolean(rest, resolves) || eval_boolean(cons, resolves);
    }
    // `A unless B` — inverse conditional.
    if let Some((cons, cond)) = split_top(expr, "unless") {
        return eval_boolean(cond, resolves) || eval_boolean(cons, resolves);
    }
    // `A or B …` — any term.
    let ors = split_all_top(expr, "or");
    if ors.len() > 1 {
        return ors.iter().any(|t| eval_boolean(t, resolves));
    }
    // `A and B …` / `A with B …` — all terms.
    for op in ["and", "with"] {
        let terms = split_all_top(expr, op);
        if terms.len() > 1 {
            return terms.iter().all(|t| eval_boolean(t, resolves));
        }
    }
    // `A without B` — only A matters.
    if let Some((a, _b)) = split_top(expr, "without") {
        return eval_boolean(a, resolves);
    }
    // Nested group or a plain leaf.
    let expr = expr.trim();
    match expr.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        Some(inner) => eval_boolean(inner.trim(), resolves),
        None => {
            let cap = leaf_cap(expr);
            cap.is_empty() || resolves(cap)
        }
    }
}

/// Extract the bare capability names referenced by a dependency string
/// (for reporting which parts of a flagged dep didn't resolve). Strips
/// version constraints, boolean operators, and grouping parens — while
/// keeping parens that are part of a name (`crate(foo)`,
/// `libc.so.6(GLIBC_2.34)(64bit)`).
fn extract_capability_names(dep: &str) -> Vec<String> {
    let dep = dep.trim();
    let dep = match dep.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        Some(inner) => inner,
        None => dep,
    };
    dep.split_whitespace()
        .filter(|token| {
            !matches!(
                *token,
                ">=" | "<=" | ">" | "<" | "=" | "with" | "or" | "and" | "if" | "unless" | "else"
            ) && !token.starts_with(|c: char| c.is_ascii_digit())
                && !token.ends_with('~')
        })
        .map(|s| strip_grouping_parens(s).to_string())
        .filter(|s| !s.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Extract the name part of a Provide string.
///
/// E.g. "tor = 0.4.9.5-1.el9" → "tor",
///      "config(tor) = 0.4.9.5-1.el9" → "config(tor)",
///      "libthing.so.1()(64bit)" → "libthing.so.1()(64bit)"
fn provide_name(provide: &str) -> &str {
    // Split on version operators: " = ", " < ", " > ", " <= ", " >= "
    provide
        .split_once(" = ")
        .or_else(|| provide.split_once(" < "))
        .or_else(|| provide.split_once(" > "))
        .or_else(|| provide.split_once(" <= "))
        .or_else(|| provide.split_once(" >= "))
        .map(|(name, _)| name)
        .unwrap_or(provide)
}

/// Compare old and new provides, classifying each as updated or removed.
fn compare_provides(old: &BTreeSet<String>, new: &BTreeSet<String>) -> Vec<ChangedProvide> {
    // Build a map from provide name → full provide string for new provides.
    let new_by_name: BTreeMap<&str, &str> =
        new.iter().map(|p| (provide_name(p), p.as_str())).collect();

    let mut changed = Vec::new();
    for old_provide in old.difference(new) {
        let name = provide_name(old_provide);
        let new_match = new_by_name.get(name).map(|s| s.to_string());
        changed.push(ChangedProvide {
            old: old_provide.clone(),
            new: new_match,
        });
    }
    changed
}

/// Check whether `@testing` actually carries the expected new
/// content for at least one of the input NVRs.
///
/// Bodhi can claim an update is being routed to testing while the
/// updates-testing repo metadata still reflects the previous V-R,
/// and side-tag inputs have no Bodhi status to consult — so we
/// confirm by querying via `lookup` directly. Returns true as soon
/// as one subpackage matches an expected `(version, release)`.
///
/// `lookup(src)` returns `(name, version, release)` tuples for all
/// subpackages of `src` in `@testing`; the caller is responsible for
/// wiring it to `Fedrq::subpkgs_nvrs` (extracted as a closure so the
/// matcher can be unit-tested without invoking fedrq).
fn testing_has_update_nvrs<F>(nvrs: &[String], lookup: F) -> bool
where
    F: Fn(&str) -> Vec<(String, String, String)>,
{
    let expected: BTreeMap<&str, (&str, &str)> = nvrs
        .iter()
        .filter_map(|nvr| sandogasa_koji::parse_nvr(nvr).map(|(n, v, r)| (n, (v, r))))
        .collect();

    expected
        .iter()
        .any(|(src, (exp_v, exp_r))| lookup(src).iter().any(|(_, v, r)| v == exp_v && r == exp_r))
}

/// True for a Fedora release branch name (`fNN`, e.g. `f43`).
fn is_fedora_branch(b: &str) -> bool {
    b.strip_prefix('f')
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|c| c.is_ascii_digit()))
}

/// Actionable error for an auto-detected EPEL branch: the `epelN` branch
/// alone can't resolve base-OS dependencies, so the reviewer must pass a
/// RHEL-compatible base branch plus the EPEL repo (the choice of base —
/// AlmaLinux, CentOS Stream, … — is theirs).
fn epel_guard_error(branch: &str) -> String {
    format!(
        "{branch} can't resolve base-OS dependencies on its own; pass a \
         base branch plus the EPEL repo, e.g. -b al9 -r @epel (epel9) or \
         -b c10s -r @epel (epel10)"
    )
}

/// The prefix of a Koji side-tag name before `-build-side-` (e.g.
/// `f43-build-side-12345` ⇒ `f43`, `epel9-build-side-1` ⇒ `epel9`).
/// Returns None when the tag doesn't match that shape.
fn branch_from_side_tag(tag: &str) -> Option<String> {
    let end = tag.find("-build-side-")?;
    let prefix = &tag[..end];
    (!prefix.is_empty()).then(|| prefix.to_string())
}

/// Infer the fedrq branch for a side tag when `--branch` is omitted.
///
/// Only Fedora side tags (`fNN-build-side-*`) map cleanly to their own
/// branch. EPEL side tags are rejected with an actionable error: the
/// `epelN` branch alone can't resolve base-OS dependencies, so a
/// RHEL-compatible base branch plus `-r @epel` is required (the choice
/// of base — AlmaLinux, CentOS Stream, … — is the user's). Any other
/// shape can't be inferred either.
fn infer_branch_for_side_tag(tag: &str) -> Result<String, String> {
    match branch_from_side_tag(tag) {
        Some(b) if is_fedora_branch(&b) => Ok(b),
        Some(b) if b.starts_with("epel") => Err(epel_guard_error(&b)),
        _ => Err(format!(
            "could not infer branch from side tag {tag}; pass --branch"
        )),
    }
}

/// Resolve the branch for a Bodhi update: an explicit `--branch` wins;
/// otherwise it's derived from the release name (e.g. "EPEL-9" → "epel9",
/// "F44" → "f44"). A *derived* EPEL branch is rejected with
/// [`epel_guard_error`] — `epelN` alone can't resolve base-OS deps — so
/// the reviewer must pass a base branch + `-r @epel`. An explicit
/// `--branch` bypasses that guard.
fn resolve_bodhi_branch(
    user: Option<String>,
    release_name: Option<&str>,
) -> Result<String, String> {
    if let Some(b) = user {
        return Ok(b);
    }
    let derived = release_name
        .map(|r| r.to_lowercase().replace('-', ""))
        .ok_or("could not determine branch from Bodhi release; use --branch")?;
    if derived.starts_with("epel") {
        return Err(epel_guard_error(&derived));
    }
    Ok(derived)
}

/// Extract a testing branch from a side tag name.
///
/// E.g. "epel9-build-side-133287" → "epel9",
///      "epel10.0-build-side-12345" → "epel10.0"
fn testing_branch_from_side_tag(tag: Option<&str>) -> Option<String> {
    let tag = tag?;
    let rest = tag.strip_prefix("epel")?;
    let release_end = rest.find("-build-side-")?;
    Some(format!("epel{}", &rest[..release_end]))
}

struct BodhiUpdateInfo {
    side_tag: Option<String>,
    nvrs: Vec<String>,
    release_name: Option<String>,
    /// Branch from the Bodhi release (e.g. "epel9", "f44").
    release_branch: Option<String>,
    /// Update status ("pending", "testing", "stable", ...).
    status: String,
}

/// Fetch a Bodhi update and extract its key fields.
fn fetch_bodhi_update(alias: &str) -> Result<BodhiUpdateInfo, String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;
    let update = rt
        .block_on(async {
            let client = sandogasa_bodhi::BodhiClient::new();
            client.update_by_alias(alias).await
        })
        .map_err(|e| format!("failed to fetch Bodhi update {alias}: {e}"))?;

    let release_name = update.release.as_ref().map(|r| r.name.clone());
    let release_branch = update.release.as_ref().and_then(|r| r.branch.clone());
    let nvrs: Vec<String> = update.builds.iter().map(|b| b.nvr.clone()).collect();
    let side_tag = update.from_tag.filter(|t| t.contains("-side-"));
    Ok(BodhiUpdateInfo {
        side_tag,
        nvrs,
        release_name,
        release_branch,
        status: update.status,
    })
}

/// Compute changed provides using `subpkgs_provides` on both repos.
///
/// Works for @testing and any repo where source RPM queries work.
///
/// Provides are unioned across all updated packages on *both*
/// sides before comparing (matching the koji path): an update can
/// move a capability between its packages — e.g. a version bump
/// paired with a new compat package that keeps shipping the old
/// version's provides. A per-package comparison would falsely
/// report such provides as removed.
fn compute_changed_provides_via_subpkgs(
    updated_packages: &[String],
    stable_fedrq: &sandogasa_fedrq::Fedrq,
    new_fedrq: &sandogasa_fedrq::Fedrq,
) -> Vec<ChangedProvide> {
    let old_provides: BTreeSet<String> = updated_packages
        .par_iter()
        .flat_map(|srpm| filter_none(stable_fedrq.subpkgs_provides(srpm).unwrap_or_default()))
        .collect();

    let new_provides: BTreeSet<String> = updated_packages
        .par_iter()
        .flat_map(|srpm| filter_none(new_fedrq.subpkgs_provides(srpm).unwrap_or_default()))
        .collect();

    compare_provides(&old_provides, &new_provides)
}

/// Compute changed provides for side tags using `koji buildinfo`
/// + `fedrq pkg_provides`.
///
/// Side tags don't index source RPMs, so we get binary RPM names
/// from koji and query their provides individually.
fn compute_changed_provides_via_koji(
    nvrs: &[String],
    updated_packages: &[String],
    stable_fedrq: &sandogasa_fedrq::Fedrq,
    side_tag_fedrq: &sandogasa_fedrq::Fedrq,
    koji_profile: Option<&str>,
    verbose: bool,
) -> Vec<ChangedProvide> {
    // Get old provides from stable repo (source-based query works here).
    let old_provides: BTreeSet<String> = updated_packages
        .par_iter()
        .flat_map(|srpm| filter_none(stable_fedrq.subpkgs_provides(srpm).unwrap_or_default()))
        .collect();

    // Get new provides: binary RPM names via koji, then query
    // each one's provides from the side tag repo.
    let binary_names: Vec<String> = nvrs
        .iter()
        .flat_map(|nvr| {
            koji_build_rpms(nvr, koji_profile).unwrap_or_else(|e| {
                if verbose {
                    eprintln!(
                        "[check-update] warning: \
                         koji buildinfo {nvr} failed: {e}"
                    );
                }
                vec![]
            })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if verbose {
        eprintln!(
            "[check-update] {} binary packages from koji: {}",
            binary_names.len(),
            binary_names.join(", "),
        );
    }

    let new_provides: BTreeSet<String> = binary_names
        .par_iter()
        .flat_map(|name| filter_none(side_tag_fedrq.pkg_provides(name).unwrap_or_default()))
        .collect();

    compare_provides(&old_provides, &new_provides)
}

/// Check that the updated packages' subpackage Requires are
/// satisfiable on the stable repo. Returns deps that can't be
/// resolved (indicating the update would be uninstallable).
///
/// Queries provides and requires per binary RPM name (not per
/// source package) so this works on Koji repos that lack source
/// RPMs.
fn check_update_installability(
    updated_packages: &[String],
    binary_names: &[String],
    new_fedrq: &sandogasa_fedrq::Fedrq,
    stable_fedrq: &sandogasa_fedrq::Fedrq,
    verbose: bool,
) -> Vec<UnsatisfiedDep> {
    if verbose {
        eprintln!(
            "[check-update] checking installability of {} binary packages",
            binary_names.len()
        );
    }

    // Collect new provides from updated packages (they satisfy each
    // other's deps). Query per binary package so it works on koji.
    if verbose {
        eprintln!("[check-update] collecting provides from updated packages");
    }
    let new_provides: BTreeSet<String> = binary_names
        .par_iter()
        .flat_map_iter(|name| {
            new_fedrq
                .pkg_provides(name)
                .unwrap_or_default()
                .into_iter()
                .map(|p| provide_name(&p).to_string())
        })
        .collect();

    // Map binary RPM names back to source packages for reporting.
    let bin_to_src: BTreeMap<&str, &str> = binary_names
        .iter()
        .map(|bin| {
            let src = updated_packages
                .iter()
                .find(|src| bin.starts_with(src.as_str()) && bin[src.len()..].starts_with('-'))
                .map(|s| s.as_str())
                .unwrap_or(bin.as_str());
            (bin.as_str(), src)
        })
        .collect();

    if verbose {
        eprintln!(
            "[check-update] {} new provides collected, checking requires",
            new_provides.len()
        );
    }

    // Memoize stable-repo capability resolution: a single capability
    // like `libstdc++.so.6` or `libQt6Core.so.6` is required by many of
    // the update's binaries, and each lookup shells out to fedrq. Cache
    // the satisfied/unsatisfied verdict per capability so we resolve it
    // once per run instead of once per requiring package.
    let cap_satisfied: Mutex<HashMap<String, bool>> = Mutex::new(HashMap::new());
    let resolves_in_stable = |cap: &str| -> bool {
        if let Some(&hit) = cap_satisfied.lock().unwrap().get(cap) {
            return hit;
        }
        let resolved = stable_fedrq.provides_of_provider(cap).unwrap_or_default();
        let satisfied = resolved.iter().any(|s| !s.is_empty() && s != "(none)");
        cap_satisfied
            .lock()
            .unwrap()
            .insert(cap.to_string(), satisfied);
        satisfied
    };

    // A capability resolves if another package in the update provides
    // it, or it's available in the stable repo (memoized).
    let resolves = |cap: &str| new_provides.contains(cap) || resolves_in_stable(cap);

    binary_names
        .par_iter()
        .flat_map_iter(|name| {
            let requires = new_fedrq.pkg_requires(name).unwrap_or_default();
            let src = bin_to_src.get(name.as_str()).copied().unwrap_or(name);
            requires
                .into_iter()
                .filter_map(|dep| {
                    let dep = dep.trim();
                    if dep.is_empty() || sandogasa_depfilter::is_rpm_internal_dep(dep) {
                        return None;
                    }
                    // Evaluate boolean/rich deps with real semantics
                    // (`A if B` only requires A when B resolves, etc.).
                    if dep_satisfiable(dep, &resolves) {
                        return None;
                    }
                    // For a boolean dep, name the capabilities that
                    // actually failed so the reader sees which part is
                    // the problem; for a plain dep the dep is the cap.
                    let unresolved = if dep.starts_with('(') {
                        extract_capability_names(dep)
                            .into_iter()
                            .filter(|c| !resolves(c))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    Some(UnsatisfiedDep {
                        package: src.to_string(),
                        dep: dep.to_string(),
                        unresolved,
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Given changed provides, find affected reverse deps and check
/// their Requires.
#[allow(clippy::too_many_arguments)]
fn run_provides_analysis(
    input: &str,
    branch: &str,
    updated_packages: &[String],
    changes: &[PackageChange],
    binary_names: &[String],
    changed_provides: Vec<ChangedProvide>,
    stale_side_tag: Vec<StaleSideTag>,
    stable_fedrq: &sandogasa_fedrq::Fedrq,
    new_fedrq: &sandogasa_fedrq::Fedrq,
    opts: &CheckUpdateOptions,
) -> Result<CheckUpdateReport, String> {
    if opts.verbose {
        let updated_count = changed_provides.iter().filter(|c| c.is_updated()).count();
        let removed_count = changed_provides.iter().filter(|c| !c.is_updated()).count();
        eprintln!(
            "[check-update] {} changed provides \
             ({updated_count} updated, {removed_count} removed)",
            changed_provides.len()
        );
    }

    let installability_issues = check_update_installability(
        updated_packages,
        binary_names,
        new_fedrq,
        stable_fedrq,
        opts.verbose,
    );

    if changed_provides.is_empty() {
        return Ok(CheckUpdateReport {
            input: input.to_string(),
            branch: branch.to_string(),
            repo: opts.repo.clone(),
            updated_packages: updated_packages.to_vec(),
            changes: changes.to_vec(),
            full_analysis: true,
            changed_provides: vec![],
            installability_issues,
            stale_side_tag,
            skip_reason: None,
            reverse_deps: BTreeMap::new(),
        });
    }

    // Use old provide strings for whatrequires lookup.
    let old_provide_strings: Vec<String> = changed_provides.iter().map(|c| c.old.clone()).collect();
    let old_provide_set: BTreeSet<&str> = old_provide_strings.iter().map(|s| s.as_str()).collect();

    if opts.verbose {
        eprintln!("[check-update] finding reverse dependencies");
    }

    let rev_dep_sources = filter_none(
        stable_fedrq
            .whatrequires(&old_provide_strings)
            .map_err(|e| format!("whatrequires failed: {e}"))?,
    );

    let updated_set: BTreeSet<&str> = updated_packages.iter().map(|s| s.as_str()).collect();
    let rev_deps: Vec<String> = rev_dep_sources
        .into_iter()
        .filter(|pkg| !updated_set.contains(pkg.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if opts.verbose {
        eprintln!(
            "[check-update] {} reverse dependencies to check: {}",
            rev_deps.len(),
            rev_deps.join(", "),
        );
    }

    let results: Vec<(String, RevDepResult)> = rev_deps
        .par_iter()
        .map(|pkg| {
            let requires = stable_fedrq.subpkgs_requires(pkg).unwrap_or_default();

            let issues: Vec<BrokenRequires> = requires
                .iter()
                .filter_map(|dep| {
                    let dep_str = dep.trim();
                    if old_provide_set.contains(dep_str) {
                        Some(BrokenRequires {
                            dep: dep_str.to_string(),
                            changed_provide: dep_str.to_string(),
                        })
                    } else {
                        None
                    }
                })
                .collect();

            let status = if issues.is_empty() { "ok" } else { "broken" };
            (
                pkg.clone(),
                RevDepResult {
                    status: status.to_string(),
                    issues,
                },
            )
        })
        .collect();

    let reverse_deps: BTreeMap<String, RevDepResult> = results.into_iter().collect();

    Ok(CheckUpdateReport {
        input: input.to_string(),
        branch: branch.to_string(),
        repo: opts.repo.clone(),
        updated_packages: updated_packages.to_vec(),
        changes: changes.to_vec(),
        full_analysis: true,
        changed_provides,
        installability_issues,
        stale_side_tag,
        skip_reason: None,
        reverse_deps,
    })
}

/// True for koji debug subpackages (`-debuginfo`/`-debugsource`),
/// which land in a separate debug repo, not the main side-tag repodata.
fn is_debug_rpm(name: &str) -> bool {
    name.ends_with("-debuginfo") || name.ends_with("-debugsource")
}

/// Group `(source, version-release)` pairs (from
/// [`Fedrq::pkgs_source_vr`]) into a `source → [V-R]` map, so a build's
/// freshness can be judged from any of its binaries.
fn source_vr_map(pairs: Vec<(String, String)>) -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (source, vr) in pairs {
        map.entry(source).or_default().push(vr);
    }
    map
}

/// Detect side-tag repos whose metadata still serves a stale V-R
/// for a build that koji has at a newer NVR.
///
/// koji list-tagged is authoritative for what builds are in the
/// side tag, but the rendered repodata can lag — leaving fedrq
/// queries returning the previous V-R inherited from the parent
/// tag. When that happens, `compute_changed_provides_via_koji`
/// compares stable's provides against the same old V-R, finds no
/// diff, and silently drops affected reverse deps.
///
/// `side_repo_vrs` maps each updated source to the V-Rs its binaries
/// actually resolve to in the side tag (built from a batched
/// `source,nevr` query). A build is fresh if any of its binaries is at
/// the expected `(version, release)`; absent ⇒ the repodata hasn't
/// picked it up; only an *older* V-R is flagged stale.
fn check_side_tag_staleness(
    nvrs: &[String],
    side_repo_vrs: &BTreeMap<String, Vec<String>>,
    verbose: bool,
) -> Vec<StaleSideTag> {
    let mut warnings = Vec::new();
    for nvr in nvrs {
        let Some((name, exp_v, exp_r)) = sandogasa_koji::parse_nvr(nvr) else {
            continue;
        };
        let expected_vr = format!("{exp_v}-{exp_r}");

        // Every V-R the repodata serves for this source's binaries.
        let empty = Vec::new();
        let vrs = side_repo_vrs.get(name).unwrap_or(&empty);

        // The expected build is present → repodata is fresh.
        if vrs.contains(&expected_vr) {
            continue;
        }

        // No binaries at all → repodata hasn't picked up the build.
        if vrs.is_empty() {
            if verbose {
                eprintln!(
                    "warning: {name}: side tag has no binary RPMs in \
                     repodata for expected {expected_vr} from {nvr}; run \
                     'koji regen-repo' on the side tag, then rerun with \
                     --refresh"
                );
            }
            warnings.push(StaleSideTag {
                package: name.to_string(),
                expected_nvr: nvr.clone(),
                actual_vr: None,
            });
            continue;
        }

        // Only *older* repodata is stale. A newer V-R than expected
        // (e.g. the side tag moved 6.7.0 → 6.7.1 while our NVR list
        // still named 6.7.0) is not a staleness problem — skip it.
        let latest = vrs
            .iter()
            .max_by(|a, b| sandogasa_rpmvercmp::compare_evr(a, b))
            .cloned()
            .unwrap_or_default();
        if sandogasa_rpmvercmp::compare_evr(&latest, &expected_vr) == std::cmp::Ordering::Less {
            if verbose {
                eprintln!(
                    "warning: {name}: side tag repodata is stale (expected \
                     {expected_vr} from {nvr}, found {latest}); run 'koji \
                     regen-repo' on the side tag, then rerun with --refresh"
                );
            }
            warnings.push(StaleSideTag {
                package: name.to_string(),
                expected_nvr: nvr.clone(),
                actual_vr: Some(latest),
            });
        }
    }
    warnings
}

/// Interpret a y/n answer line, falling back to `default_yes` on
/// empty input.
fn parse_confirm(line: &str, default_yes: bool) -> bool {
    match line.trim() {
        "" => default_yes,
        s => s.eq_ignore_ascii_case("y") || s.eq_ignore_ascii_case("yes"),
    }
}

/// Prompt on stderr and read a y/n answer from stdin.
fn confirm(prompt: &str, default_yes: bool) -> Result<bool, String> {
    use std::io::{BufRead, Write};
    let hint = if default_yes { "Y/n" } else { "y/N" };
    eprint!("{prompt} [{hint}]: ");
    std::io::stderr().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    Ok(parse_confirm(&line, default_yes))
}

/// Interactively resolve stale side-tag repodata: offer to run
/// `koji regen-repo` on the user's behalf (default yes) and
/// re-check freshness afterwards; if declined, ask whether to
/// continue with stale data (default no — abort).
///
/// Returns the (possibly now empty) staleness warnings to carry
/// into the report.
fn resolve_stale_side_tag<F>(
    stale: Vec<StaleSideTag>,
    tag: &str,
    profile: Option<&str>,
    recheck: F,
    verbose: bool,
) -> Result<Vec<StaleSideTag>, String>
where
    F: Fn() -> Vec<StaleSideTag>,
{
    eprintln!(
        "side tag repodata is stale for {} source package(s):",
        stale.len()
    );
    for w in &stale {
        match &w.actual_vr {
            Some(actual) => eprintln!(
                "  - {}: expected {}, found {} in repodata",
                w.package, w.expected_nvr, actual
            ),
            None => eprintln!(
                "  - {}: expected {}, no matching binary RPMs in repodata",
                w.package, w.expected_nvr
            ),
        }
    }
    eprintln!("without a repo regen the reverse-dep analysis may be incomplete.");

    if confirm(
        &format!("Run `koji regen-repo --wait {tag}` now? (may take several minutes)"),
        true,
    )? {
        eprintln!("waiting for koji to regenerate the {tag} repo...");
        sandogasa_koji::regen_repo(tag, profile)?;
        // Drop *both* caches: clearing only the fedrq smartcache leaves
        // libdnf5 serving the pre-regen side-tag metadata, so the
        // re-check below (and the rest of the analysis) would still see
        // stale repodata and re-warn despite the successful regen.
        sandogasa_fedrq::clear_all_caches()
            .map_err(|e| format!("failed to clear metadata caches: {e}"))?;
        if verbose {
            eprintln!("cleared fedrq + libdnf5 caches after regen");
        }
        let still_stale = recheck();
        if !still_stale.is_empty() {
            eprintln!(
                "side tag repodata is still stale for {} source \
                 package(s) after the regen; continuing with \
                 warnings in the report.",
                still_stale.len()
            );
        }
        Ok(still_stale)
    } else if confirm("Continue the check with stale data?", false)? {
        Ok(stale)
    } else {
        Err(format!(
            "aborted: side tag repodata is stale; run \
             `koji regen-repo {tag}`, then rerun this check with \
             --refresh to drop the cached metadata"
        ))
    }
}

// ---- Tests ----

#[cfg(test)]
mod tests {
    use super::*;

    // --- skip_note / render_report skipped-analysis cases ---

    fn skip_report(branch: &str, skip: SkipReason) -> CheckUpdateReport {
        CheckUpdateReport {
            input: "FEDORA-2026-test".to_string(),
            branch: branch.to_string(),
            repo: None,
            updated_packages: vec!["iptstate".to_string()],
            changes: vec![],
            full_analysis: false,
            changed_provides: vec![],
            installability_issues: vec![],
            stale_side_tag: vec![],
            skip_reason: Some(skip),
            reverse_deps: BTreeMap::new(),
        }
    }

    #[test]
    fn skip_note_testing_lag_explains_repodata_not_side_tag() {
        let r = skip_report(
            "f43",
            SkipReason::TestingLag {
                expected: vec!["iptstate-2.3.0-1.fc43".to_string()],
            },
        );
        // Flatten the wrapped blockquote so substring checks don't trip
        // on a line break landing mid-phrase.
        let md = unquote(&render_report(&r, false));
        assert!(md.contains("Provides weren't compared"));
        assert!(md.contains("`@testing`"));
        assert!(md.contains("iptstate-2.3.0-1.fc43"));
        assert!(md.contains("mirror propagation"));
        // The old, misleading wording must be gone.
        assert!(!md.contains("no side tag available"));
        // No raw line should exceed the wrap width.
        assert!(
            render_report(&r, false)
                .lines()
                .all(|l| l.chars().count() <= 76)
        );
    }

    #[test]
    fn skip_note_epel10_points_at_side_tag() {
        let r = skip_report("epel10.0", SkipReason::Epel10TestingUnsupported);
        let md = unquote(&render_report(&r, false));
        assert!(md.contains("EPEL 10"));
        assert!(md.contains("side tag"));
    }

    #[test]
    fn skip_note_no_source_reports_bodhi_status() {
        let r = skip_report(
            "f43",
            SkipReason::NoSource {
                bodhi_status: Some("pending".to_string()),
            },
        );
        let md = unquote(&render_report(&r, false));
        assert!(md.contains("isn't in"));
        assert!(md.contains("pending"));
    }

    // --- blocking_findings / apply_resolutions / render_addressed ---

    fn dep(package: &str, dep: &str) -> UnsatisfiedDep {
        UnsatisfiedDep {
            package: package.to_string(),
            dep: dep.to_string(),
            unresolved: vec![],
        }
    }

    fn broken(provide: &str) -> RevDepResult {
        RevDepResult {
            status: "broken".to_string(),
            issues: vec![BrokenRequires {
                dep: "needs".to_string(),
                changed_provide: provide.to_string(),
            }],
        }
    }

    fn report_with(
        installability: Vec<UnsatisfiedDep>,
        reverse_deps: Vec<(&str, RevDepResult)>,
    ) -> CheckUpdateReport {
        CheckUpdateReport {
            input: "FEDORA-2026-test".to_string(),
            branch: "f44".to_string(),
            repo: None,
            updated_packages: vec![],
            changes: vec![],
            full_analysis: true,
            changed_provides: vec![],
            installability_issues: installability,
            stale_side_tag: vec![],
            skip_reason: None,
            reverse_deps: reverse_deps
                .into_iter()
                .map(|(n, r)| (n.to_string(), r))
                .collect(),
        }
    }

    #[test]
    fn blocking_findings_groups_reverse_deps_by_provide() {
        let report = report_with(
            vec![dep("plasma-settings", "(a if b)")],
            vec![
                ("kpat", broken("libfoo.so.1")),
                ("kwin", broken("libfoo.so.1")),
                ("kde-cli", broken("libbar.so.2")),
            ],
        );
        let findings = blocking_findings(&report);
        // one installability + two provide groups (not three packages)
        assert_eq!(findings.len(), 3);
        let libfoo = findings
            .iter()
            .find_map(|f| match f {
                Finding::ReverseDepBreak { provide, packages } if provide == "libfoo.so.1" => {
                    Some(packages.clone())
                }
                _ => None,
            })
            .unwrap();
        assert_eq!(libfoo, vec!["kpat".to_string(), "kwin".to_string()]);
    }

    #[test]
    fn apply_resolutions_removed_installability_drops() {
        let report = report_with(vec![dep("plasma-settings", "(a if b)")], vec![]);
        let findings = blocking_findings(&report);
        let decisions = findings
            .into_iter()
            .map(|f| (f, Resolution::Removed))
            .collect();
        let (curated, addressed) = apply_resolutions(report, decisions);
        assert!(curated.installability_issues.is_empty());
        assert!(addressed.is_empty());
    }

    #[test]
    fn apply_resolutions_explained_reverse_dep_flips_to_ok_and_records() {
        let report = report_with(
            vec![],
            vec![
                ("kpat", broken("libfoo.so.1")),
                ("kwin", broken("libfoo.so.1")),
            ],
        );
        let findings = blocking_findings(&report);
        let decisions = findings
            .into_iter()
            .map(|f| (f, Resolution::Explained("rebuilt downstream".to_string())))
            .collect();
        let (curated, addressed) = apply_resolutions(report, decisions);
        assert!(curated.reverse_deps.values().all(|r| r.status == "ok"));
        assert_eq!(addressed.len(), 1);
        assert!(addressed[0].1.contains("rebuilt downstream"));
    }

    #[test]
    fn apply_resolutions_keep_leaves_breakage() {
        let report = report_with(vec![], vec![("kpat", broken("libfoo.so.1"))]);
        let findings = blocking_findings(&report);
        let decisions = findings
            .into_iter()
            .map(|f| (f, Resolution::Keep))
            .collect();
        let (curated, addressed) = apply_resolutions(report, decisions);
        assert_eq!(curated.reverse_deps["kpat"].status, "broken");
        assert!(addressed.is_empty());
    }

    #[test]
    fn apply_resolutions_partial_break_stays_broken() {
        // kpat broken by two Provides; removing one leaves it broken.
        let mut two = broken("libfoo.so.1");
        two.issues.push(BrokenRequires {
            dep: "needs2".to_string(),
            changed_provide: "libbar.so.2".to_string(),
        });
        let report = report_with(vec![], vec![("kpat", two)]);
        let decisions = vec![(
            Finding::ReverseDepBreak {
                provide: "libfoo.so.1".to_string(),
                packages: vec!["kpat".to_string()],
            },
            Resolution::Removed,
        )];
        let (curated, _) = apply_resolutions(report, decisions);
        assert_eq!(curated.reverse_deps["kpat"].status, "broken");
        assert_eq!(curated.reverse_deps["kpat"].issues.len(), 1);
        assert_eq!(
            curated.reverse_deps["kpat"].issues[0].changed_provide,
            "libbar.so.2"
        );
    }

    #[test]
    fn render_addressed_lists_explained_findings() {
        let addressed = vec![(
            Finding::Installability(dep("plasma-settings", "(a if b)")),
            "satisfied at runtime".to_string(),
        )];
        let md = render_addressed(&addressed);
        assert!(md.contains("Issues addressed by the reviewer"));
        assert!(md.contains("plasma-settings"));
        assert!(md.contains("satisfied at runtime"));
        assert!(render_addressed(&[]).is_empty());
    }

    /// Collapse a wrapped Markdown blockquote back to one line for
    /// substring assertions (drop `> ` prefixes and newlines).
    fn unquote(md: &str) -> String {
        md.replace("\n> ", " ").replace("> ", "").replace('\n', " ")
    }

    fn change(pkg: &str, old: Option<&str>, new: &str) -> PackageChange {
        PackageChange {
            package: pkg.to_string(),
            old: old.map(str::to_string),
            new: new.to_string(),
        }
    }

    #[test]
    fn package_summary_groups_by_version_transition() {
        let report = CheckUpdateReport {
            input: "FEDORA-2026-x".to_string(),
            branch: "f43".to_string(),
            repo: None,
            updated_packages: vec![
                "a".into(),
                "b".into(),
                "c".into(),
                "d".into(),
                "newpkg".into(),
            ],
            changes: vec![
                change("a", Some("6.7.0-1.fc43"), "6.7.1-1.fc43"),
                change("b", Some("6.7.0-1.fc43"), "6.7.1-1.fc43"),
                change("c", Some("6.7.0-1.fc43"), "6.7.1-1.fc43"),
                change("d", Some("6.7.0-1.fc43"), "6.7.1-1.fc43"),
                change("newpkg", None, "1.0-1.fc43"),
            ],
            full_analysis: true,
            changed_provides: vec![],
            installability_issues: vec![],
            stale_side_tag: vec![],
            skip_reason: None,
            reverse_deps: BTreeMap::new(),
        };
        let md = render_report(&report, false);
        assert!(md.contains("**5** package(s): 4 updated, 1 new"));
        // Four packages share one transition → collapsed to a count.
        assert!(md.contains("`6.7.0-1.fc43` → `6.7.1-1.fc43` (4 packages)"));
        assert!(md.contains("**New (1):** newpkg"));
        // No removed provides / installability / broken deps → clean.
        assert!(md.contains("No breakage expected."));
        // Default (non-detailed) doesn't dump the full package list.
        assert!(!md.contains("**Updated packages:**"));
    }

    #[test]
    fn package_summary_names_small_groups() {
        let report = CheckUpdateReport {
            input: "x".to_string(),
            branch: "f43".to_string(),
            repo: None,
            updated_packages: vec!["foo".into(), "bar".into()],
            changes: vec![
                change("foo", Some("1.0-1.fc43"), "1.1-1.fc43"),
                change("bar", Some("2.0-1.fc43"), "2.1-1.fc43"),
            ],
            full_analysis: true,
            changed_provides: vec![],
            installability_issues: vec![],
            stale_side_tag: vec![],
            skip_reason: None,
            reverse_deps: BTreeMap::new(),
        };
        let md = render_report(&report, false);
        // Singleton groups name the package.
        assert!(md.contains("`1.0-1.fc43` → `1.1-1.fc43` (foo)"));
        assert!(md.contains("`2.0-1.fc43` → `2.1-1.fc43` (bar)"));
        // Same-size groups are ordered alphabetically by package name,
        // not by version string (which would put foo's 1.x before bar).
        let bar = md.find("(bar)").unwrap();
        let foo = md.find("(foo)").unwrap();
        assert!(bar < foo, "expected bar before foo, got:\n{md}");
    }

    #[test]
    fn wrap_prefixed_wraps_and_prefixes() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota";
        let wrapped = wrap_prefixed(text, "> ", 20);
        // Every line is prefixed and within width.
        assert!(wrapped.lines().all(|l| l.starts_with("> ")));
        assert!(wrapped.lines().all(|l| l.chars().count() <= 20));
        // It actually wrapped (more than one line) and lost no words.
        assert!(wrapped.lines().count() > 1);
        assert_eq!(unquote(&wrapped).split_whitespace().count(), 9);
    }

    // --- parse_confirm ---

    #[test]
    fn parse_confirm_empty_uses_default() {
        assert!(parse_confirm("", true));
        assert!(parse_confirm("\n", true));
        assert!(!parse_confirm("", false));
        assert!(!parse_confirm("  \n", false));
    }

    #[test]
    fn parse_confirm_explicit_yes() {
        for answer in ["y", "Y", "yes", "YES", " y \n"] {
            assert!(parse_confirm(answer, false), "{answer:?}");
        }
    }

    #[test]
    fn parse_confirm_explicit_no() {
        for answer in ["n", "N", "no", "anything-else"] {
            assert!(!parse_confirm(answer, true), "{answer:?}");
        }
    }

    // --- parse_nvr ---

    #[test]
    fn parse_nvr_standard() {
        assert_eq!(parse_nvr("foo-1.0-1.fc42"), Some("foo"));
    }

    #[test]
    fn parse_nvr_hyphenated_name() {
        assert_eq!(parse_nvr("rust-uucore-0.0.28-2.el9"), Some("rust-uucore"));
    }

    #[test]
    fn parse_nvr_many_hyphens() {
        assert_eq!(parse_nvr("python-a-b-c-1.2.3-4.fc42"), Some("python-a-b-c"));
    }

    #[test]
    fn parse_nvr_too_short() {
        assert_eq!(parse_nvr("foo-1.0"), None);
    }

    #[test]
    fn parse_nvr_empty() {
        assert_eq!(parse_nvr(""), None);
    }

    #[test]
    fn parse_nvr_no_hyphens() {
        assert_eq!(parse_nvr("foobar"), None);
    }

    // --- provide_name ---

    #[test]
    fn provide_name_versioned() {
        assert_eq!(provide_name("tor = 0.4.9.5-1.el9"), "tor");
    }

    #[test]
    fn provide_name_with_parens() {
        assert_eq!(provide_name("config(tor) = 0.4.9.5-1.el9"), "config(tor)");
    }

    #[test]
    fn provide_name_unversioned() {
        assert_eq!(
            provide_name("libthing.so.1()(64bit)"),
            "libthing.so.1()(64bit)"
        );
    }

    #[test]
    fn provide_name_ge() {
        assert_eq!(
            provide_name("crate(foo/default) >= 1.0"),
            "crate(foo/default)"
        );
    }

    // --- dep_satisfiable (rich/boolean deps) ---

    #[test]
    fn dep_satisfiable_if_condition_present_checks_consequent() {
        // The plasma-settings case: pulseaudio IS available, so the
        // consequent (both modules) is required — and both are present.
        let dep = "((pulseaudio-module-gsettings and sound-theme-freedesktop) if pulseaudio)";
        let all = |_: &str| true;
        assert!(dep_satisfiable(dep, &all));
        // pulseaudio present, but a consequent capability missing → broken.
        let missing_module = |c: &str| c != "sound-theme-freedesktop";
        assert!(!dep_satisfiable(dep, &missing_module));
        // pulseaudio absent → the requirement doesn't apply → satisfied,
        // even though the modules aren't available.
        let only_modules_absent = |c: &str| c == "irrelevant";
        assert!(dep_satisfiable(dep, &only_modules_absent));
    }

    #[test]
    fn dep_satisfiable_or_needs_any() {
        let dep = "(foo or bar)";
        assert!(dep_satisfiable(dep, &|c: &str| c == "bar")); // one is enough
        assert!(!dep_satisfiable(dep, &|_: &str| false)); // neither
    }

    #[test]
    fn dep_satisfiable_and_needs_all() {
        let dep = "(foo and bar)";
        assert!(dep_satisfiable(dep, &|_: &str| true));
        assert!(!dep_satisfiable(dep, &|c: &str| c == "foo")); // bar missing
    }

    #[test]
    fn dep_satisfiable_unless_waives_when_condition_present() {
        let dep = "(foo unless bar)";
        // bar present → foo not required → satisfied even if foo absent.
        assert!(dep_satisfiable(dep, &|c: &str| c == "bar"));
        // bar absent → foo required.
        assert!(!dep_satisfiable(dep, &|_: &str| false));
        assert!(dep_satisfiable(dep, &|c: &str| c == "foo"));
    }

    #[test]
    fn dep_satisfiable_plain_dep_checks_capability() {
        assert!(dep_satisfiable("bash >= 5.0", &|c: &str| c == "bash"));
        assert!(!dep_satisfiable("missing-cap", &|_: &str| false));
        // A plain lib cap with parens in the name resolves as one cap.
        assert!(dep_satisfiable(
            "libc.so.6(GLIBC_2.34)(64bit)",
            &|c: &str| c == "libc.so.6(GLIBC_2.34)(64bit)"
        ));
    }

    // --- extract_capability_names ---

    #[test]
    fn extract_capability_names_strips_inner_group_close_paren() {
        // Regression: the close paren of an inner rich-dep group must
        // not stick to the capability name (the plasma-settings bug —
        // `sound-theme-freedesktop)` never resolved).
        let caps = extract_capability_names(
            "((pulseaudio-module-gsettings and sound-theme-freedesktop) if pulseaudio)",
        );
        assert!(caps.contains(&"pulseaudio-module-gsettings".to_string()));
        assert!(caps.contains(&"sound-theme-freedesktop".to_string()));
        assert!(caps.contains(&"pulseaudio".to_string()));
        assert!(!caps.iter().any(|c| c.ends_with(')') && !c.contains('(')));
    }

    #[test]
    fn extract_capability_names_keeps_paren_in_lib_caps() {
        // The trailing ')' on these caps is part of the name —
        // dropping it produces a corrupted cap that fedrq can't
        // resolve, surfacing as a false-positive installability
        // issue for every system library.
        assert_eq!(
            extract_capability_names("libc.so.6(GLIBC_2.34)(64bit)"),
            vec!["libc.so.6(GLIBC_2.34)(64bit)".to_string()]
        );
        assert_eq!(
            extract_capability_names("ld-linux-aarch64.so.1()(64bit)"),
            vec!["ld-linux-aarch64.so.1()(64bit)".to_string()]
        );
        assert_eq!(
            extract_capability_names("rtld(GNU_HASH)"),
            vec!["rtld(GNU_HASH)".to_string()]
        );
        assert_eq!(
            extract_capability_names("nginx(abi)"),
            vec!["nginx(abi)".to_string()]
        );
    }

    #[test]
    fn extract_capability_names_strips_outer_rich_dep_parens() {
        // A true rich/boolean dep wrapped in `(...)` should have
        // those wrapping parens stripped, but the inner cap
        // names with their own parens stay intact.
        let caps = extract_capability_names("(crate(foo) >= 1.0 with crate(foo) < 2.0~)");
        assert!(caps.contains(&"crate(foo)".to_string()));
        assert!(!caps.iter().any(|c| c.contains(">=") || c.contains("<")));
    }

    #[test]
    fn extract_capability_names_drops_version_operators() {
        let caps = extract_capability_names("nginx(abi) = 2:1.30.0-1.fc44");
        assert_eq!(caps, vec!["nginx(abi)".to_string()]);
    }

    // --- compare_provides ---

    #[test]
    fn compare_provides_updated() {
        let old: BTreeSet<String> = ["tor = 0.4.9.5-1.el9".to_string()].into();
        let new: BTreeSet<String> = ["tor = 0.4.9.6-1.el9".to_string()].into();
        let changed = compare_provides(&old, &new);
        assert_eq!(changed.len(), 1);
        assert!(changed[0].is_updated());
        assert_eq!(changed[0].new.as_deref(), Some("tor = 0.4.9.6-1.el9"));
    }

    #[test]
    fn compare_provides_removed() {
        let old: BTreeSet<String> = ["libfoo.so.1()(64bit)".to_string()].into();
        let new: BTreeSet<String> = BTreeSet::new();
        let changed = compare_provides(&old, &new);
        assert_eq!(changed.len(), 1);
        assert!(!changed[0].is_updated());
        assert!(changed[0].new.is_none());
    }

    #[test]
    fn compare_provides_unchanged() {
        let old: BTreeSet<String> = ["bash".to_string()].into();
        let new: BTreeSet<String> = ["bash".to_string()].into();
        let changed = compare_provides(&old, &new);
        assert!(changed.is_empty());
    }

    #[test]
    fn compare_provides_credits_compat_package_in_same_update() {
        // const-oid 0.9 -> 0.10 drops crate(const-oid/std), but the
        // same update introduces the compat package rust-const-oid0.9
        // which still ships it. The callers union provides across
        // all packages in the update on both sides, so the provide
        // must come out unchanged — not "removed" (the bug behind
        // the wrong FEDORA-EPEL-2026-b89b964abe report).
        let old: BTreeSet<String> = ["crate(const-oid/std) = 0.9.6".to_string()].into();
        let new: BTreeSet<String> = [
            "crate(const-oid) = 0.10.2".to_string(),
            // From the compat package, same version string as old.
            "crate(const-oid/std) = 0.9.6".to_string(),
        ]
        .into();
        assert!(compare_provides(&old, &new).is_empty());
    }

    // --- testing_has_update_nvrs ---

    fn nvr_row(name: &str, version: &str, release: &str) -> (String, String, String) {
        (name.to_string(), version.to_string(), release.to_string())
    }

    #[test]
    fn testing_has_update_nvrs_matches_when_vr_present() {
        let nvrs = vec!["rust-libmimalloc-sys-0.1.47-1.fc44".to_string()];
        // @testing returns subpackages at the matching V-R.
        let found = testing_has_update_nvrs(&nvrs, |_src| {
            vec![nvr_row(
                "rust-libmimalloc-sys+default-devel",
                "0.1.47",
                "1.fc44",
            )]
        });
        assert!(found);
    }

    #[test]
    fn testing_has_update_nvrs_rejects_stale_testing() {
        let nvrs = vec!["rust-libmimalloc-sys-0.1.47-1.fc44".to_string()];
        // @testing still has the previous V-R — this is the bug case.
        let found = testing_has_update_nvrs(&nvrs, |_src| {
            vec![nvr_row(
                "rust-libmimalloc-sys+default-devel",
                "0.1.44",
                "2.fc44",
            )]
        });
        assert!(!found);
    }

    #[test]
    fn testing_has_update_nvrs_any_match_wins() {
        // Multiple NVRs in the update; one matches, one doesn't.
        let nvrs = vec![
            "rust-libmimalloc-sys-0.1.47-1.fc44".to_string(),
            "rust-mimalloc-0.1.50-1.fc44".to_string(),
        ];
        let found = testing_has_update_nvrs(&nvrs, |src| match src {
            "rust-libmimalloc-sys" => {
                vec![nvr_row("rust-libmimalloc-sys-devel", "0.1.44", "2.fc44")]
            }
            "rust-mimalloc" => vec![nvr_row("rust-mimalloc-devel", "0.1.50", "1.fc44")],
            _ => vec![],
        });
        assert!(found);
    }

    #[test]
    fn testing_has_update_nvrs_empty_input_false() {
        assert!(!testing_has_update_nvrs(&[], |_| vec![]));
    }

    #[test]
    fn testing_has_update_nvrs_malformed_nvr_skipped() {
        // No valid NVRs → nothing to check → false (not a panic).
        let found = testing_has_update_nvrs(&["not-an-nvr".to_string()], |_| {
            vec![nvr_row("anything", "1", "1")]
        });
        assert!(!found);
    }

    // --- source_vr_map / check_side_tag_staleness ---

    fn vr_map(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<String>> {
        source_vr_map(
            pairs
                .iter()
                .map(|(s, vr)| (s.to_string(), vr.to_string()))
                .collect(),
        )
    }

    #[test]
    fn source_vr_map_groups_by_source() {
        let map = vr_map(&[
            ("rust-mimalloc", "0.1.50-1.fc44"),
            ("rust-mimalloc", "0.1.48-2.fc44"),
            ("python-jiter", "0.15.0-3.fc44"),
        ]);
        assert_eq!(map["rust-mimalloc"].len(), 2);
        assert_eq!(map["python-jiter"], vec!["0.15.0-3.fc44".to_string()]);
    }

    #[test]
    fn check_side_tag_staleness_matching_vr_no_warning() {
        let nvrs = vec!["rust-mimalloc-0.1.50-1.fc44".to_string()];
        let map = vr_map(&[("rust-mimalloc", "0.1.50-1.fc44")]);
        assert!(check_side_tag_staleness(&nvrs, &map, false).is_empty());
    }

    #[test]
    fn check_side_tag_staleness_one_matching_binary_is_fresh() {
        // Only one of the build's binaries needs to be at the expected
        // V-R for it to count as fresh.
        let nvrs = vec!["rust-mimalloc-0.1.50-1.fc44".to_string()];
        let map = vr_map(&[
            ("rust-mimalloc", "0.1.48-2.fc44"),
            ("rust-mimalloc", "0.1.50-1.fc44"),
        ]);
        assert!(check_side_tag_staleness(&nvrs, &map, false).is_empty());
    }

    #[test]
    fn check_side_tag_staleness_python_rename_and_debug_excluded() {
        // The python-jiter case: source python-jiter ships python3-jiter
        // (a rename). The batched source query attributes python3-jiter
        // to python-jiter at the expected V-R, so it's fresh — even
        // though the old source-name prefix match would have missed it
        // and false-flagged on python-jiter-debugsource (which never
        // reaches the side tag's main repodata and so isn't in the map).
        let nvrs = vec!["python-jiter-0.15.0-3.fc44".to_string()];
        let map = vr_map(&[("python-jiter", "0.15.0-3.fc44")]);
        assert!(check_side_tag_staleness(&nvrs, &map, false).is_empty());
    }

    #[test]
    fn check_side_tag_staleness_detects_mismatched_vr() {
        // The exact case from the f44 / FEDORA-2026-7db4114930 report:
        // koji says rust-mimalloc-0.1.50-1.fc44 but the side tag repo
        // still serves 0.1.48-2.fc44.
        let nvrs = vec!["rust-mimalloc-0.1.50-1.fc44".to_string()];
        let map = vr_map(&[("rust-mimalloc", "0.1.48-2.fc44")]);
        let warnings = check_side_tag_staleness(&nvrs, &map, false);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].package, "rust-mimalloc");
        assert_eq!(warnings[0].expected_nvr, "rust-mimalloc-0.1.50-1.fc44");
        assert_eq!(warnings[0].actual_vr.as_deref(), Some("0.1.48-2.fc44"));
    }

    #[test]
    fn check_side_tag_staleness_newer_in_repodata_is_not_stale() {
        // The FEDORA-2026-2b36efabf2 case: the NVR list named 6.7.0
        // (a superseded build still tagged), but the side tag repodata
        // serves the newer 6.7.1. Newer is not stale — no warning.
        let nvrs = vec!["aurorae-6.7.0-1.fc43".to_string()];
        let map = vr_map(&[("aurorae", "6.7.1-1.fc43")]);
        assert!(check_side_tag_staleness(&nvrs, &map, false).is_empty());
    }

    #[test]
    fn check_side_tag_staleness_detects_missing_binaries() {
        // Source absent from the side-tag map entirely.
        let nvrs = vec!["rust-mimalloc-0.1.50-1.fc44".to_string()];
        let map = BTreeMap::new();
        let warnings = check_side_tag_staleness(&nvrs, &map, false);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].package, "rust-mimalloc");
        assert!(warnings[0].actual_vr.is_none());
    }

    #[test]
    fn check_side_tag_staleness_mixed_sources() {
        // rust-libmimalloc-sys is fresh, rust-mimalloc is stale —
        // we should warn only for rust-mimalloc.
        let nvrs = vec![
            "rust-libmimalloc-sys-0.1.47-1.fc44".to_string(),
            "rust-mimalloc-0.1.50-1.fc44".to_string(),
        ];
        let map = vr_map(&[
            ("rust-libmimalloc-sys", "0.1.47-1.fc44"),
            ("rust-mimalloc", "0.1.48-2.fc44"),
        ]);
        let warnings = check_side_tag_staleness(&nvrs, &map, false);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].package, "rust-mimalloc");
    }

    // --- branch_from_side_tag ---

    #[test]
    fn branch_from_side_tag_fedora() {
        assert_eq!(
            branch_from_side_tag("f43-build-side-12345"),
            Some("f43".to_string())
        );
    }

    #[test]
    fn branch_from_side_tag_epel() {
        assert_eq!(
            branch_from_side_tag("epel9-build-side-134436"),
            Some("epel9".to_string())
        );
        assert_eq!(
            branch_from_side_tag("epel10.0-build-side-1"),
            Some("epel10.0".to_string())
        );
    }

    #[test]
    fn branch_from_side_tag_rejects_non_side_tag() {
        assert_eq!(branch_from_side_tag("f43-updates"), None);
        assert_eq!(branch_from_side_tag("rawhide"), None);
        // Empty prefix isn't a usable branch.
        assert_eq!(branch_from_side_tag("-build-side-1"), None);
    }

    #[test]
    fn is_fedora_branch_matches_fnn_only() {
        assert!(is_fedora_branch("f43"));
        assert!(is_fedora_branch("f9"));
        assert!(!is_fedora_branch("epel9"));
        assert!(!is_fedora_branch("f")); // no digits
        assert!(!is_fedora_branch("fc43")); // letters after f
        assert!(!is_fedora_branch("rawhide"));
    }

    #[test]
    fn infer_branch_for_side_tag_fedora() {
        assert_eq!(
            infer_branch_for_side_tag("f44-build-side-142334"),
            Ok("f44".to_string())
        );
    }

    #[test]
    fn infer_branch_for_side_tag_epel_errors_with_hint() {
        let err = infer_branch_for_side_tag("epel9-build-side-134436").unwrap_err();
        // Names the offending branch and tells the user what to pass.
        assert!(err.contains("epel9"));
        assert!(err.contains("-b al9 -r @epel"));
        // epel8 is rejected too (not auto-mapped).
        assert!(infer_branch_for_side_tag("epel8-build-side-1").is_err());
    }

    #[test]
    fn infer_branch_for_side_tag_unrecognized_errors() {
        let err = infer_branch_for_side_tag("not-a-side-tag").unwrap_err();
        assert!(err.contains("--branch"));
    }

    #[test]
    fn resolve_bodhi_branch_explicit_wins() {
        // An explicit --branch is honored even for an EPEL release.
        assert_eq!(
            resolve_bodhi_branch(Some("al9".to_string()), Some("EPEL-9")),
            Ok("al9".to_string())
        );
    }

    #[test]
    fn resolve_bodhi_branch_fedora_derived() {
        assert_eq!(
            resolve_bodhi_branch(None, Some("F44")),
            Ok("f44".to_string())
        );
    }

    #[test]
    fn resolve_bodhi_branch_epel_derived_is_rejected() {
        // The bug: an EPEL Bodhi update must not silently run against the
        // bare epelN branch (can't resolve base-OS deps).
        let err = resolve_bodhi_branch(None, Some("EPEL-9")).unwrap_err();
        assert!(err.contains("epel9"));
        assert!(err.contains("-b al9 -r @epel"));
    }

    #[test]
    fn resolve_bodhi_branch_missing_release_errors() {
        assert!(resolve_bodhi_branch(None, None).is_err());
    }

    // --- testing_branch_from_side_tag ---

    #[test]
    fn testing_branch_epel9() {
        assert_eq!(
            testing_branch_from_side_tag(Some("epel9-build-side-133287")),
            Some("epel9".to_string())
        );
    }

    #[test]
    fn testing_branch_epel10() {
        assert_eq!(
            testing_branch_from_side_tag(Some("epel10.0-build-side-12345")),
            Some("epel10.0".to_string())
        );
    }

    #[test]
    fn testing_branch_not_epel() {
        assert_eq!(
            testing_branch_from_side_tag(Some("f44-build-side-99999")),
            None
        );
    }

    #[test]
    fn testing_branch_none() {
        assert_eq!(testing_branch_from_side_tag(None), None);
    }

    // --- detect_input_type ---

    #[test]
    fn detect_side_tag() {
        match detect_input_type("epel9-build-side-133287") {
            InputKind::SideTag(tag) => {
                assert_eq!(tag, "epel9-build-side-133287");
            }
            _ => panic!("expected SideTag"),
        }
    }

    #[test]
    fn detect_bodhi_alias() {
        match detect_input_type("FEDORA-EPEL-2026-f9eaa11e18") {
            InputKind::BodhiAlias(alias) => {
                assert_eq!(alias, "FEDORA-EPEL-2026-f9eaa11e18");
            }
            _ => panic!("expected BodhiAlias"),
        }
    }

    #[test]
    fn detect_bodhi_alias_fedora() {
        match detect_input_type("FEDORA-2026-abc123") {
            InputKind::BodhiAlias(alias) => {
                assert_eq!(alias, "FEDORA-2026-abc123");
            }
            _ => panic!("expected BodhiAlias"),
        }
    }

    #[test]
    fn detect_bodhi_url() {
        let url = "https://bodhi.fedoraproject.org/updates/FEDORA-EPEL-2026-f9eaa11e18";
        match detect_input_type(url) {
            InputKind::BodhiAlias(alias) => {
                assert_eq!(alias, "FEDORA-EPEL-2026-f9eaa11e18");
            }
            _ => panic!("expected BodhiAlias"),
        }
    }

    #[test]
    fn detect_bodhi_url_http() {
        let url = "http://bodhi.fedoraproject.org/updates/FEDORA-2026-xyz";
        match detect_input_type(url) {
            InputKind::BodhiAlias(alias) => {
                assert_eq!(alias, "FEDORA-2026-xyz");
            }
            _ => panic!("expected BodhiAlias"),
        }
    }
}
