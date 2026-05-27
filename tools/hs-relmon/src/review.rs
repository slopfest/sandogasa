// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `review` subcommand — interactive karma-style review of
//! builds sitting in Hyperscale `-testing` tags.
//!
//! Modeled on `fedora-easy-karma`: walk each build awaiting
//! review and prompt for a verdict. Here the verdict drives Koji
//! tag operations directly:
//!
//! - `+1` promotes — tag the build into the sibling `-release`
//!   tag and untag it from `-testing` (a released build
//!   shouldn't linger in testing, matching the prune-tags
//!   promotion-dedup rule).
//! - `-1` rejects — untag from `-testing`.
//! - `0` / skip — leave the build where it is.
//!
//! Target resolution (the unit of review is a `(testing-tag,
//! build)` pair, since EL9 and EL10 are distinct artifacts):
//!
//! - no target → every latest-per-package build in each scanned
//!   `-testing` tag.
//! - a package name → that package's latest build in each
//!   `-testing` tag containing it.
//! - an NVR → that exact build, wherever it's tagged for testing.

use std::cmp::Ordering;
use std::io::{BufRead, Write};

use sandogasa_koji::{
    build_info_with_changelog, list_tagged, list_tagged_nvrs, parse_nvr, parse_nvr_name, tag_build,
    untag_build,
};

use crate::prune_tags::{self, KOJI_PROFILE};
use crate::rpmvercmp::compare_evr;

/// Default cap on changelog lines shown for a brand-new package
/// (one with nothing yet in the release tag to diff against).
pub const DEFAULT_CHANGELOG_LINES: usize = 20;

/// One build to review, with the tag it's in and the release tag
/// a promotion would move it to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewItem {
    pub testing_tag: String,
    pub release_tag: String,
    pub nvr: String,
}

/// A reviewer's verdict for one build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Karma {
    /// Tag into release and untag from testing.
    Promote,
    /// Untag from testing.
    Reject,
    /// Leave the build untouched.
    Skip,
    /// Stop reviewing.
    Quit,
}

/// How the positional argument was interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// No argument — review everything in testing.
    All,
    /// A source package name.
    Package(String),
    /// A specific build NVR.
    Nvr(String),
}

/// The sibling `-release` tag for a `-testing` tag. If `tag`
/// doesn't end in `-testing` it's returned unchanged (callers
/// only pass testing tags).
pub fn release_tag_for(testing_tag: &str) -> String {
    format!("{}-release", testing_tag.trim_end_matches("-testing"))
}

/// Classify the positional argument. An NVR is recognised by the
/// `.el` dist marker every CBS build carries in its release
/// (e.g. `1.hs.el10`, `1.hs+fb.el9_z`); package names don't
/// contain `.el`, so it's a reliable discriminator.
pub fn classify_target(arg: Option<&str>) -> Target {
    match arg {
        None => Target::All,
        Some(s) if s.contains(".el") && parse_nvr(s).is_some() => Target::Nvr(s.to_string()),
        Some(s) => Target::Package(s.to_string()),
    }
}

/// Parse a karma prompt response. Accepts `+1`/`1`, `-1`,
/// `0`/`s`/empty (skip), `q` (quit). Unrecognised input returns
/// `None` so the caller can re-prompt.
pub fn parse_karma(input: &str) -> Option<Karma> {
    match input.trim() {
        "+1" | "1" => Some(Karma::Promote),
        "-1" => Some(Karma::Reject),
        "0" | "s" | "S" | "" => Some(Karma::Skip),
        "q" | "Q" => Some(Karma::Quit),
        _ => None,
    }
}

/// Build review items from `latest`-per-package NVRs in a testing
/// tag, optionally filtered to a single package. Pure helper for
/// the All / Package paths.
pub fn items_from_latest(
    testing_tag: &str,
    nvrs: &[String],
    package: Option<&str>,
) -> Vec<ReviewItem> {
    let release_tag = release_tag_for(testing_tag);
    nvrs.iter()
        .filter(|nvr| match package {
            Some(p) => parse_nvr_name(nvr) == Some(p),
            None => true,
        })
        .map(|nvr| ReviewItem {
            testing_tag: testing_tag.to_string(),
            release_tag: release_tag.clone(),
            nvr: nvr.clone(),
        })
        .collect()
}

/// Drop review items whose package is in the skip list. Skip
/// wins over an explicit target, so a skipped package can't be
/// promoted even if its name or NVR is passed deliberately.
pub fn filter_skipped(items: Vec<ReviewItem>, skip: &[String]) -> Vec<ReviewItem> {
    if skip.is_empty() {
        return items;
    }
    items
        .into_iter()
        .filter(|it| match parse_nvr_name(&it.nvr) {
            Some(name) => !skip.iter().any(|s| s == name),
            None => true,
        })
        .collect()
}

/// Run the review flow.
pub fn run(
    repositories: &[String],
    target_arg: Option<&str>,
    skip: &[String],
    changelog_lines: usize,
    dry_run: bool,
    verbose: bool,
) -> Result<(), String> {
    let profile = Some(KOJI_PROFILE);
    let target = classify_target(target_arg);

    let testing_tags: Vec<String> = prune_tags::candidate_tags(repositories)
        .into_iter()
        .filter(|t| t.ends_with("-testing"))
        .collect();

    let collected = collect_items(&testing_tags, &target, profile, verbose);
    let before = collected.len();
    let items = filter_skipped(collected, skip);
    if verbose && items.len() < before {
        eprintln!(
            "[hs-relmon] skipped {} build(s) via --skip",
            before - items.len()
        );
    }

    if items.is_empty() {
        println!("Nothing in testing to review.");
        return Ok(());
    }

    if dry_run {
        println!("{} build(s) to review:", items.len());
        for item in &items {
            println!("  {} [{}]", item.nvr, item.testing_tag);
        }
        return Ok(());
    }

    let total = items.len();
    let mut promoted = 0usize;
    let mut rejected = 0usize;
    let mut skipped = 0usize;
    for (i, item) in items.iter().enumerate() {
        print_item(item, i + 1, total, profile, changelog_lines);
        let karma = match prompt_karma()? {
            Some(k) => k,
            None => {
                // EOF — stop the loop.
                eprintln!("\n(exiting)");
                break;
            }
        };
        match karma {
            Karma::Promote => {
                apply_promote(item, profile)?;
                promoted += 1;
            }
            Karma::Reject => {
                apply_reject(item, profile)?;
                rejected += 1;
            }
            Karma::Skip => {
                skipped += 1;
            }
            Karma::Quit => {
                break;
            }
        }
    }

    eprintln!("\n{promoted} promoted, {rejected} rejected, {skipped} skipped.");
    Ok(())
}

/// Gather the review items for the chosen target across the
/// scanned testing tags. Tags that error out (don't exist on
/// this hub, transient failure) are skipped with a verbose
/// warning.
fn collect_items(
    testing_tags: &[String],
    target: &Target,
    profile: Option<&str>,
    verbose: bool,
) -> Vec<ReviewItem> {
    let mut items: Vec<ReviewItem> = Vec::new();
    for tag in testing_tags {
        if verbose {
            eprintln!("[hs-relmon] scanning {tag}");
        }
        match target {
            Target::Nvr(nvr) => {
                // Exact NVR may be an older, non-latest build, so
                // check the full (non-`--latest`) tag listing.
                match list_tagged_nvrs(tag, profile) {
                    Ok(all) if all.iter().any(|n| n == nvr) => {
                        items.push(ReviewItem {
                            testing_tag: tag.clone(),
                            release_tag: release_tag_for(tag),
                            nvr: nvr.clone(),
                        });
                    }
                    Ok(_) => {}
                    Err(e) => {
                        if verbose {
                            eprintln!("[hs-relmon] {tag}: skipped ({e})");
                        }
                    }
                }
            }
            Target::All | Target::Package(_) => {
                let pkg = match target {
                    Target::Package(p) => Some(p.as_str()),
                    _ => None,
                };
                match list_tagged(tag, profile, None) {
                    Ok(builds) => {
                        let nvrs: Vec<String> = builds.into_iter().map(|b| b.nvr).collect();
                        items.extend(items_from_latest(tag, &nvrs, pkg));
                    }
                    Err(e) => {
                        if verbose {
                            eprintln!("[hs-relmon] {tag}: skipped ({e})");
                        }
                    }
                }
            }
        }
    }
    items
}

/// Print the review header for a build: position, NVR, tag, the
/// currently-released NVR for comparison, a downgrade warning if
/// the testing build is older, then the build's metadata and the
/// changelog entries newer than the released build (or, for a
/// brand-new package, the first `changelog_lines` lines).
fn print_item(
    item: &ReviewItem,
    idx: usize,
    total: usize,
    profile: Option<&str>,
    changelog_lines: usize,
) {
    println!("\n{}", "=".repeat(60));
    println!("[{idx}/{total}] {} ({})", item.nvr, item.testing_tag);

    let released_nvr = parse_nvr_name(&item.nvr)
        .and_then(|name| current_release_nvr(&item.release_tag, name, profile));

    // A build that isn't strictly newer than what's released has
    // no changes worth reviewing — warn and skip the changelog,
    // which also saves the second `koji buildinfo` call.
    let mut not_newer = false;
    match &released_nvr {
        Some(cur) => {
            if cur == &item.nvr {
                println!("currently in release: {cur} (same build)");
            } else {
                println!("currently in release: {cur}");
            }
            not_newer = testing_not_newer_than_release(&item.nvr, cur);
            if not_newer {
                println!(
                    "WARNING: this testing build is not newer than the released \
                     build — leave it (prune-tags will clean it up)"
                );
            }
        }
        None => println!("currently in release: (none)"),
    }
    println!("{}", "-".repeat(60));

    let testing_info = match build_info_with_changelog(&item.nvr, profile) {
        Ok(info) => info,
        Err(e) => {
            eprintln!("(could not fetch build info: {e})");
            println!("{}", "-".repeat(60));
            return;
        }
    };
    println!("{}", metadata_summary(&testing_info));

    if not_newer {
        // Nothing newer to show; don't fetch the released build's
        // changelog just to render an empty diff.
        println!("{}", "-".repeat(60));
        return;
    }

    // Show only what changed since the released build. For a new
    // package there's nothing to diff against, so cap the raw
    // changelog at `changelog_lines`.
    let released_info = released_nvr
        .as_deref()
        .and_then(|nvr| build_info_with_changelog(nvr, profile).ok());
    let testing_cl = changelog_section(&testing_info).unwrap_or("");
    let released_cl = released_info.as_deref().and_then(changelog_section);
    println!("\nChangelog:");
    println!(
        "{}",
        changelog_since(testing_cl, released_cl, changelog_lines)
    );
    println!("{}", "-".repeat(60));
}

/// The latest NVR of `package` currently in `release_tag`, if
/// any. Best-effort: errors and absence both map to `None`.
fn current_release_nvr(release_tag: &str, package: &str, profile: Option<&str>) -> Option<String> {
    let builds = list_tagged(release_tag, profile, None).ok()?;
    builds
        .into_iter()
        .find(|b| parse_nvr_name(&b.nvr) == Some(package))
        .map(|b| b.nvr)
}

/// The `version-release` of an NVR, for version comparison.
fn version_release(nvr: &str) -> Option<String> {
    parse_nvr(nvr).map(|(_, v, r)| format!("{v}-{r}"))
}

/// Is the testing build no newer than the released build — i.e.
/// the same version is already in release, or testing is a
/// downgrade? Used only to warn; pruning the stale testing tag
/// is prune-tags' job, not review's. Only a strictly *newer*
/// testing build is a normal promotion candidate.
pub fn testing_not_newer_than_release(testing_nvr: &str, released_nvr: &str) -> bool {
    match (version_release(testing_nvr), version_release(released_nvr)) {
        (Some(t), Some(r)) => compare_evr(&t, &r) != Ordering::Greater,
        _ => false,
    }
}

/// Everything in `koji buildinfo` output before the `RPMs:`
/// listing — the useful metadata (NVR, state, builder, source
/// commit, finish time, tags) without the noisy RPM file list.
pub fn metadata_summary(info: &str) -> String {
    info.lines()
        .take_while(|l| !l.trim_start().starts_with("RPMs:"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_string()
}

/// The changelog body of `koji buildinfo --changelog` output —
/// the text after the `Changelog:` header line. `None` if the
/// output has no changelog section.
pub fn changelog_section(info: &str) -> Option<&str> {
    let marker = "\nChangelog:";
    let start = info.find(marker)?;
    let after = &info[start + marker.len()..];
    Some(after.trim_start_matches('\n').trim_end())
}

/// First `* `-prefixed entry header line in a changelog.
fn first_changelog_header(cl: &str) -> Option<&str> {
    cl.lines().find(|l| l.trim_start().starts_with("* "))
}

/// Trim a testing build's changelog to just what's new relative
/// to the released build.
///
/// - When `released_cl` carries a changelog, find its newest
///   entry header in `testing_cl` and return everything above it
///   (the entries added since release). Returns a placeholder
///   when nothing is newer.
/// - When `released_cl` is `None` (brand-new package) or has no
///   entries, return the first `max_new_lines` lines of
///   `testing_cl`, with a truncation marker if it was longer.
pub fn changelog_since(
    testing_cl: &str,
    released_cl: Option<&str>,
    max_new_lines: usize,
) -> String {
    let testing_cl = testing_cl.trim();
    if testing_cl.is_empty() {
        return "(no changelog)".to_string();
    }
    match released_cl.and_then(first_changelog_header) {
        Some(rel_header) => {
            let mut newer: Vec<&str> = Vec::new();
            for line in testing_cl.lines() {
                if line == rel_header {
                    break;
                }
                newer.push(line);
            }
            let joined = newer.join("\n");
            let joined = joined.trim();
            if joined.is_empty() {
                "(no changelog entries since the released build)".to_string()
            } else {
                joined.to_string()
            }
        }
        None => {
            let total = testing_cl.lines().count();
            let shown: Vec<&str> = testing_cl.lines().take(max_new_lines).collect();
            let mut out = shown.join("\n");
            if total > max_new_lines {
                out.push_str("\n… (changelog truncated)");
            }
            out
        }
    }
}

fn apply_promote(item: &ReviewItem, profile: Option<&str>) -> Result<(), String> {
    tag_build(&item.release_tag, &item.nvr, profile)
        .map_err(|e| format!("tag-build {} {}: {e}", item.release_tag, item.nvr))?;
    eprintln!("tagged {} into {}", item.nvr, item.release_tag);
    untag_build(&item.testing_tag, &item.nvr, profile)
        .map_err(|e| format!("untag-build {} {}: {e}", item.testing_tag, item.nvr))?;
    eprintln!("untagged {} from {}", item.nvr, item.testing_tag);
    Ok(())
}

fn apply_reject(item: &ReviewItem, profile: Option<&str>) -> Result<(), String> {
    untag_build(&item.testing_tag, &item.nvr, profile)
        .map_err(|e| format!("untag-build {} {}: {e}", item.testing_tag, item.nvr))?;
    eprintln!("untagged {} from {}", item.nvr, item.testing_tag);
    Ok(())
}

/// Prompt until a valid karma value is entered. Returns
/// `Ok(None)` on EOF (Ctrl-D on an empty line) so the caller can
/// stop cleanly.
fn prompt_karma() -> Result<Option<Karma>, String> {
    loop {
        eprint!("karma [+1=promote / -1=reject / Enter=skip / q=quit]: ");
        std::io::stderr().flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        let n = std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(None);
        }
        match parse_karma(&line) {
            Some(k) => return Ok(Some(k)),
            None => eprintln!("unrecognised input: {:?}", line.trim()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_tag_for_swaps_suffix() {
        assert_eq!(
            release_tag_for("hyperscale10s-packages-main-testing"),
            "hyperscale10s-packages-main-release"
        );
        assert_eq!(
            release_tag_for("hyperscale9-packages-facebook-testing"),
            "hyperscale9-packages-facebook-release"
        );
    }

    #[test]
    fn classify_target_distinguishes_nvr_from_package() {
        assert_eq!(classify_target(None), Target::All);
        assert_eq!(
            classify_target(Some("ethtool")),
            Target::Package("ethtool".to_string())
        );
        // Hyphenated package name that would otherwise parse as
        // an NVR — no `.el`, so it's a package.
        assert_eq!(
            classify_target(Some("intel-gpu-tools")),
            Target::Package("intel-gpu-tools".to_string())
        );
        assert_eq!(
            classify_target(Some("ethtool-6.18-1.hs.el10")),
            Target::Nvr("ethtool-6.18-1.hs.el10".to_string())
        );
        assert_eq!(
            classify_target(Some("systemd-261~devel-20260519010708.hs+fb.el9_z")),
            Target::Nvr("systemd-261~devel-20260519010708.hs+fb.el9_z".to_string())
        );
    }

    #[test]
    fn parse_karma_accepts_all_forms() {
        assert_eq!(parse_karma("+1"), Some(Karma::Promote));
        assert_eq!(parse_karma("1"), Some(Karma::Promote));
        assert_eq!(parse_karma(" 1 "), Some(Karma::Promote));
        assert_eq!(parse_karma("-1"), Some(Karma::Reject));
        assert_eq!(parse_karma("0"), Some(Karma::Skip));
        assert_eq!(parse_karma("s"), Some(Karma::Skip));
        assert_eq!(parse_karma(""), Some(Karma::Skip));
        assert_eq!(parse_karma("q"), Some(Karma::Quit));
        assert_eq!(parse_karma("maybe"), None);
    }

    #[test]
    fn items_from_latest_no_filter() {
        let nvrs = vec![
            "ethtool-6.18-1.hs.el10".to_string(),
            "perf-6.12.0-4.hs.el10".to_string(),
        ];
        let items = items_from_latest("hyperscale10s-packages-main-testing", &nvrs, None);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].nvr, "ethtool-6.18-1.hs.el10");
        assert_eq!(items[0].release_tag, "hyperscale10s-packages-main-release");
        assert_eq!(items[0].testing_tag, "hyperscale10s-packages-main-testing");
    }

    #[test]
    fn items_from_latest_filters_by_package() {
        let nvrs = vec![
            "ethtool-6.18-1.hs.el10".to_string(),
            "perf-6.12.0-4.hs.el10".to_string(),
            "intel-gpu-tools-1.28-2.hs.el10".to_string(),
        ];
        let items = items_from_latest(
            "hyperscale10s-packages-main-testing",
            &nvrs,
            Some("intel-gpu-tools"),
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].nvr, "intel-gpu-tools-1.28-2.hs.el10");
    }

    #[test]
    fn items_from_latest_package_no_match() {
        let nvrs = vec!["ethtool-6.18-1.hs.el10".to_string()];
        let items = items_from_latest(
            "hyperscale10s-packages-main-testing",
            &nvrs,
            Some("systemd"),
        );
        assert!(items.is_empty());
    }

    fn item(nvr: &str) -> ReviewItem {
        ReviewItem {
            testing_tag: "hyperscale10s-packages-main-testing".into(),
            release_tag: "hyperscale10s-packages-main-release".into(),
            nvr: nvr.into(),
        }
    }

    #[test]
    fn filter_skipped_drops_named_packages() {
        let items = vec![
            item("systemd-258.5-1.1.hs.el10"),
            item("ethtool-6.18-1.hs.el10"),
            item("kpatch-0.9.11-0.3.hs.el10"),
        ];
        let kept = filter_skipped(items, &["systemd".to_string(), "kpatch".to_string()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].nvr, "ethtool-6.18-1.hs.el10");
    }

    #[test]
    fn filter_skipped_empty_list_is_identity() {
        let items = vec![item("ethtool-6.18-1.hs.el10")];
        let kept = filter_skipped(items, &[]);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn filter_skipped_matches_whole_package_name() {
        // A skip of "fish" must not drop "fish-utils" — matching
        // is on the exact parsed package name.
        let items = vec![
            item("fish-3.7.1-2.hs.el10"),
            item("fish-utils-1.0-1.hs.el10"),
        ];
        let kept = filter_skipped(items, &["fish".to_string()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].nvr, "fish-utils-1.0-1.hs.el10");
    }

    #[test]
    fn testing_not_newer_than_release_warns_on_older_and_equal() {
        // Older testing build → warn.
        assert!(testing_not_newer_than_release(
            "dnsmasq-2.92-8.hs.el10",
            "dnsmasq-2.92rel2-9.hs.el10"
        ));
        // Same version already in release → warn.
        assert!(testing_not_newer_than_release(
            "dnsmasq-2.92-8.hs.el10",
            "dnsmasq-2.92-8.hs.el10"
        ));
        // Strictly newer testing build → no warning (normal
        // promotion candidate).
        assert!(!testing_not_newer_than_release(
            "dnsmasq-2.92rel2-9.hs.el10",
            "dnsmasq-2.92-8.hs.el10"
        ));
    }

    const SAMPLE_BUILDINFO: &str = "\
BUILD: dnsmasq-2.92rel2-9.hs.el10 [71740]
State: COMPLETE
Built by: salimma
Source: git+https://example/dnsmasq.git#abc
Finished: Wed, 13 May 2026 19:32:13 IST
Tags: hyperscale10s-packages-main-testing
RPMs:
/mnt/koji/.../dnsmasq-2.92rel2-9.hs.el10.src.rpm
/mnt/koji/.../dnsmasq-2.92rel2-9.hs.el10.x86_64.rpm
Changelog:
* Tue May 12 2026 Petr - 2.92rel2-9
- Update to 2.92rel2

* Mon Apr 20 2026 Petr - 2.92-8
- Fix DHCP reply

* Mon Feb 16 2026 Petr - 2.92-7
- asan support";

    #[test]
    fn metadata_summary_stops_before_rpms() {
        let meta = metadata_summary(SAMPLE_BUILDINFO);
        assert!(meta.contains("BUILD: dnsmasq-2.92rel2-9.hs.el10"));
        assert!(meta.contains("Built by: salimma"));
        assert!(meta.contains("Source: git+"));
        assert!(!meta.contains("RPMs:"));
        assert!(!meta.contains(".src.rpm"));
        assert!(!meta.contains("Changelog:"));
    }

    #[test]
    fn changelog_section_returns_entries() {
        let cl = changelog_section(SAMPLE_BUILDINFO).unwrap();
        assert!(cl.starts_with("* Tue May 12 2026 Petr - 2.92rel2-9"));
        assert!(cl.contains("2.92-7"));
        assert!(!cl.contains("BUILD:"));
    }

    #[test]
    fn changelog_since_shows_only_newer_entries() {
        // Released build is 2.92-8; its top changelog header is
        // "* Mon Apr 20 2026 Petr - 2.92-8". The testing build
        // adds the 2.92rel2-9 entry above it.
        let released = "\
* Mon Apr 20 2026 Petr - 2.92-8
- Fix DHCP reply

* Mon Feb 16 2026 Petr - 2.92-7
- asan support";
        let testing = changelog_section(SAMPLE_BUILDINFO).unwrap();
        let since = changelog_since(testing, Some(released), 20);
        assert!(since.contains("2.92rel2-9"));
        assert!(since.contains("Update to 2.92rel2"));
        assert!(!since.contains("2.92-8"));
        assert!(!since.contains("2.92-7"));
    }

    #[test]
    fn changelog_since_new_package_caps_lines() {
        let testing = changelog_section(SAMPLE_BUILDINFO).unwrap();
        let since = changelog_since(testing, None, 3);
        assert_eq!(since.lines().count(), 4); // 3 + truncation marker
        assert!(since.contains("… (changelog truncated)"));
    }

    #[test]
    fn changelog_since_new_package_under_cap_no_marker() {
        let testing = "* Tue May 12 2026 Petr - 1-1\n- first build";
        let since = changelog_since(testing, None, 20);
        assert!(!since.contains("truncated"));
        assert!(since.contains("first build"));
    }

    #[test]
    fn changelog_since_no_new_entries() {
        let cl = "* Mon Apr 20 2026 Petr - 2.92-8\n- Fix DHCP reply";
        let since = changelog_since(cl, Some(cl), 20);
        assert!(since.contains("no changelog entries since"));
    }
}
