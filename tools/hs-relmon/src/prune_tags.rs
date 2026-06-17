// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `prune-tags` and `prune-manifest` subcommands — keep the N
//! newest builds in each hyperscale `-release` / `-testing` tag
//! and untag the rest.
//!
//! Older promoted builds accumulate in Koji because nothing
//! removes them automatically; over time they make `list-tagged`
//! results noisy and slow down downstream tooling. This walks
//! a package's hyperscale builds, groups by literal tag name,
//! and emits `koji untag-build` calls for everything past the
//! retention threshold.
//!
//! `--dry-run` previews without acting; otherwise an
//! interactive confirmation prompt mirrors `cpu-sig-tracker
//! untag`. `--yes` skips the prompt.
//!
//! For batch operation, `prune-manifest <path>` walks each
//! package in the manifest in order.

use std::cmp::Ordering;
use std::io::{BufRead, Write};

use sandogasa_koji::untag_build;

use crate::cbs::{Build, Client};
use crate::rpmvercmp::compare_evr;

/// CBS Koji profile used by the workspace. Matches
/// `cpu-sig-tracker`'s constant.
pub const KOJI_PROFILE: &str = "cbs";

/// Default number of builds to keep in each `-release` tag.
pub const DEFAULT_RELEASE_KEEP: usize = 2;
/// Default number of builds to keep in each `-testing` tag.
pub const DEFAULT_TESTING_KEEP: usize = 1;
/// Default tag repository (the segment between `-packages-`
/// and the stage suffix). `main` is the standard SIG channel;
/// users can opt into `facebook`, `experimental`, etc. via
/// `--repositories`.
pub const DEFAULT_REPOSITORY: &str = "main";

/// Hyperscale EL release tokens, matching the `hyperscale<EL>-`
/// tag prefix. The default release set for the duplicate / file
/// conflict scanners.
pub const EL_TOKENS: &[&str] = &["9", "9s", "10", "10s"];

/// The retention rules for one prune invocation.
#[derive(Debug, Clone)]
pub struct PruneOptions {
    pub release_keep: usize,
    pub testing_keep: usize,
    /// Repositories to manage (matched against the segment
    /// after `-packages-`). Other repositories are left
    /// untouched.
    pub repositories: Vec<String>,
}

impl Default for PruneOptions {
    fn default() -> Self {
        Self {
            release_keep: DEFAULT_RELEASE_KEEP,
            testing_keep: DEFAULT_TESTING_KEEP,
            repositories: vec![DEFAULT_REPOSITORY.to_string()],
        }
    }
}

/// What `prune-tags` would do for one package — a per-tag
/// breakdown of which builds will be kept and which untagged.
#[derive(Debug, Clone)]
pub struct PrunePlan {
    pub package: String,
    /// One entry per managed tag with at least one build in it.
    /// Ordered by tag name for stable output.
    pub tags: Vec<TagPlan>,
}

/// Per-tag retention decision: which builds stay tagged and
/// which get untagged. Both lists are ordered newest-first.
#[derive(Debug, Clone)]
pub struct TagPlan {
    pub tag: String,
    pub keep: Vec<String>,
    pub untag: Vec<String>,
}

impl PrunePlan {
    /// Flattened `(tag, NVR)` pairs of every planned untag, in
    /// the order they should be executed.
    pub fn untag_pairs(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for tp in &self.tags {
            for nvr in &tp.untag {
                out.push((tp.tag.clone(), nvr.clone()));
            }
        }
        out
    }

    /// Total number of `(tag, NVR)` untags across all tags.
    pub fn total_untags(&self) -> usize {
        self.tags.iter().map(|t| t.untag.len()).sum()
    }
}

/// One managed tag's contents — the literal tag name paired
/// with the builds Koji currently has tagged under it.
pub type TagBuilds = (String, Vec<Build>);

/// Enumerate the managed-tag names this run will consider.
/// We cross-multiply EL-version-suffix (`9`, `9s`, `10`, `10s`),
/// repository (from `--repositories`), and stage (`release`,
/// `testing`). Non-existent tags get skipped at query time, so
/// generating a few extras is cheap.
pub fn candidate_tags(repositories: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for el in ["9", "9s", "10", "10s"] {
        for repo in repositories {
            for stage in ["release", "testing"] {
                out.push(format!("hyperscale{el}-packages-{repo}-{stage}"));
            }
        }
    }
    out.sort();
    out
}

/// Query Koji for the builds of `package` in every candidate
/// managed tag. Tags that error out (don't exist on this hub,
/// transport hiccup, etc.) are skipped with a logged warning
/// when verbose.
///
/// Inverts the previous approach of "list every build, then ask
/// each one's tags". For heavy packages like systemd this drops
/// us from thousands of XML-RPC calls to a fixed handful (one
/// per candidate tag), and the progress is observable line-by-
/// line because each tag prints as it's queried.
pub fn fetch_managed_tags(
    client: &Client,
    package: &str,
    opts: &PruneOptions,
    verbose: bool,
) -> Vec<TagBuilds> {
    let candidates = candidate_tags(&opts.repositories);
    if verbose {
        eprintln!(
            "[hs-relmon] {package}: querying {} candidate tag(s)",
            candidates.len()
        );
    }
    let mut out: Vec<TagBuilds> = Vec::new();
    for (i, tag) in candidates.iter().enumerate() {
        if verbose {
            eprintln!(
                "[hs-relmon] {package}: [{}/{}] {tag}",
                i + 1,
                candidates.len()
            );
        }
        match client.list_tagged_package(tag, package) {
            Ok(builds) if !builds.is_empty() => {
                if verbose {
                    eprintln!("[hs-relmon] {package}: {tag}: {} build(s)", builds.len());
                }
                out.push((tag.clone(), builds));
            }
            Ok(_) => {
                if verbose {
                    eprintln!("[hs-relmon] {package}: {tag}: empty");
                }
            }
            Err(e) => {
                if verbose {
                    eprintln!("[hs-relmon] {package}: {tag}: skipped ({e})");
                }
            }
        }
    }
    out
}

/// `version-release` of a build, for version comparison via
/// `rpmvercmp::compare_evr`.
fn version_release(b: &Build) -> String {
    format!("{}-{}", b.version, b.release)
}

/// Build the untag plan from per-tag builds.
///
/// Two rules combine per tag:
///
/// 1. **Supersede dedup**: any build in a `-testing` tag whose
///    version is *not newer* than the latest build in the
///    sibling `-release` tag (same repository + EL combination)
///    is queued for untag from `-testing`. Once release has
///    caught up to or past a testing build, keeping it in
///    testing only adds noise — this covers both an exact
///    promoted build and older leftovers.
/// 2. **Retention**: among the remaining (strictly-newer-than-
///    release) builds in each tag, sort by `build_id` descending
///    and keep the first N per the stage's retention
///    (`release_keep` / `testing_keep`). Older builds are queued
///    for untag.
pub fn build_plan(package: &str, tag_builds: &[TagBuilds], opts: &PruneOptions) -> PrunePlan {
    // For each release tag, the highest version-release among its
    // builds — the version testing is measured against.
    let mut release_max_evr_by_tag: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (tag, builds) in tag_builds {
        if tag.ends_with("-release")
            && let Some(max) = builds
                .iter()
                .map(version_release)
                .max_by(|a, b| compare_evr(a, b))
        {
            release_max_evr_by_tag.insert(tag.clone(), max);
        }
    }

    let mut tag_plans: Vec<TagPlan> = Vec::new();
    for (tag, builds) in tag_builds {
        let is_testing = tag.ends_with("-testing");
        let sibling_release_max: Option<&String> = if is_testing {
            let sibling = format!("{}-release", tag.trim_end_matches("-testing"));
            release_max_evr_by_tag.get(&sibling)
        } else {
            None
        };

        // Bucket 1: testing builds superseded by release (queue
        // for untag). Bucket 2: everything else (subject to the
        // keep-N-newest rule).
        let mut superseded_untag: Vec<(i64, String)> = Vec::new();
        let mut considered: Vec<&Build> = Vec::new();
        for b in builds {
            let superseded = sibling_release_max.is_some_and(|rel_max| {
                compare_evr(&version_release(b), rel_max) != Ordering::Greater
            });
            if superseded {
                superseded_untag.push((b.build_id, b.nvr.clone()));
            } else {
                considered.push(b);
            }
        }

        // Apply retention to the non-superseded bucket.
        considered.sort_by_key(|b| std::cmp::Reverse(b.build_id));
        let keep_count = if tag.ends_with("-release") {
            opts.release_keep
        } else {
            opts.testing_keep
        };
        let (keep_builds, retention_untag_builds) = if considered.len() <= keep_count {
            (considered.as_slice(), &[][..])
        } else {
            considered.split_at(keep_count)
        };
        let keep: Vec<String> = keep_builds.iter().map(|b| b.nvr.clone()).collect();

        // Untags: supersede + retention, sorted newest-first.
        let mut untag_pairs: Vec<(i64, String)> = superseded_untag;
        untag_pairs.extend(
            retention_untag_builds
                .iter()
                .map(|b| (b.build_id, b.nvr.clone())),
        );
        untag_pairs.sort_by_key(|p| std::cmp::Reverse(p.0));
        let untag: Vec<String> = untag_pairs.into_iter().map(|(_, n)| n).collect();

        tag_plans.push(TagPlan {
            tag: tag.clone(),
            keep,
            untag,
        });
    }
    tag_plans.sort_by(|a, b| a.tag.cmp(&b.tag));
    PrunePlan {
        package: package.to_string(),
        tags: tag_plans,
    }
}

/// Format the plan for human review. Per-tag breakdown listing
/// both the builds that will stay tagged and the ones that will
/// be untagged — letting users sanity-check the retention before
/// confirming.
pub fn render_plan(plan: &PrunePlan) -> String {
    if plan.tags.is_empty() {
        return format!("{}: no managed hyperscale tags.\n", plan.package);
    }
    let total_untag = plan.total_untags();
    let mut out = if total_untag == 0 {
        format!("{}: nothing to untag.\n", plan.package)
    } else {
        format!("{}: would untag {} build(s)\n", plan.package, total_untag)
    };
    for tp in &plan.tags {
        out.push_str(&format!(
            "  {}: keep {}, untag {}\n",
            tp.tag,
            tp.keep.len(),
            tp.untag.len()
        ));
        if !tp.keep.is_empty() {
            out.push_str("    keep:\n");
            for nvr in &tp.keep {
                out.push_str(&format!("      {nvr}\n"));
            }
        }
        if !tp.untag.is_empty() {
            out.push_str("    untag:\n");
            for nvr in &tp.untag {
                out.push_str(&format!("      {nvr}\n"));
            }
        }
    }
    out
}

/// Execute every untag in the plan, logging each. Returns the
/// number of operations that failed; the caller decides whether
/// that's fatal for the whole batch.
pub fn apply_plan(plan: &PrunePlan, verbose: bool) -> usize {
    let mut errors = 0usize;
    for (tag, nvr) in plan.untag_pairs() {
        if verbose {
            eprintln!("[hs-relmon] koji untag-build {tag} {nvr}");
        }
        match untag_build(&tag, &nvr, Some(KOJI_PROFILE)) {
            Ok(()) => eprintln!("untagged {nvr} from {tag}"),
            Err(e) => {
                eprintln!("error: untag-build {tag} {nvr}: {e}");
                errors += 1;
            }
        }
    }
    errors
}

/// Interactive confirmation prompt. Reads one line from stdin
/// and treats `y` / `Y` as approval; anything else (including
/// EOF) is a rejection.
pub fn confirm(prompt: &str) -> Result<bool, Box<dyn std::error::Error>> {
    eprint!("{prompt} [y/N]: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().eq_ignore_ascii_case("y"))
}

/// Parse a comma-separated repository list. Empty entries are
/// dropped so `"main,"` works.
pub fn parse_repositories(s: &str) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .map(|p| p.to_string())
        .collect()
}

/// Validate and normalize release-selector tokens (already split
/// from CSV / repeated flags by the arg parser), deduped in input
/// order. Each token may carry an optional `hyperscale` prefix
/// (`hyperscale10s` == `10s`). Errors on an unknown token so a typo
/// fails loudly rather than silently scanning nothing. An empty
/// input yields an empty result — the caller substitutes the
/// default release set.
pub fn normalize_releases(tokens: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for token in tokens {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let norm = trimmed.strip_prefix("hyperscale").unwrap_or(trimmed);
        if !EL_TOKENS.contains(&norm) {
            return Err(format!("unknown release '{token}' (valid: 9, 9s, 10, 10s)"));
        }
        let norm = norm.to_string();
        if !out.contains(&norm) {
            out.push(norm);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_build(build_id: i64, nvr: &str) -> Build {
        let (name, version, release) = parse_nvr(nvr);
        Build {
            build_id,
            name,
            version,
            release,
            nvr: nvr.to_string(),
        }
    }

    /// Split `name-version-release` for fixture builds.
    fn parse_nvr(nvr: &str) -> (String, String, String) {
        let parts: Vec<&str> = nvr.rsplitn(3, '-').collect();
        // rsplitn yields right-to-left: release, version, name.
        let release = parts[0].to_string();
        let version = parts[1].to_string();
        let name = parts[2].to_string();
        (name, version, release)
    }

    #[test]
    fn candidate_tags_cross_products_el_repo_stage() {
        let tags = candidate_tags(&["main".to_string()]);
        // 4 EL variants × 1 repo × 2 stages = 8 tags.
        assert_eq!(tags.len(), 8);
        assert!(tags.contains(&"hyperscale10s-packages-main-release".to_string()));
        assert!(tags.contains(&"hyperscale10s-packages-main-testing".to_string()));
        assert!(tags.contains(&"hyperscale9s-packages-main-release".to_string()));
        assert!(tags.contains(&"hyperscale9-packages-main-release".to_string()));
    }

    #[test]
    fn candidate_tags_grows_with_repositories() {
        let tags = candidate_tags(&[
            "main".to_string(),
            "facebook".to_string(),
            "experimental".to_string(),
        ]);
        // 4 EL × 3 repos × 2 stages = 24 tags.
        assert_eq!(tags.len(), 24);
        assert!(tags.contains(&"hyperscale10s-packages-facebook-release".to_string()));
        assert!(tags.contains(&"hyperscale10s-packages-experimental-testing".to_string()));
    }

    /// Helper: build a per-tag input the way `fetch_managed_tags`
    /// would after talking to Koji.
    fn tb(tag: &str, builds: Vec<Build>) -> TagBuilds {
        (tag.to_string(), builds)
    }

    #[test]
    fn build_plan_keeps_n_newest_per_tag() {
        let tag_builds = vec![tb(
            "hyperscale10s-packages-main-release",
            vec![
                make_build(5000, "ethtool-6.18-1.hs.el10"),
                make_build(4000, "ethtool-6.17-1.hs.el10"),
                make_build(3000, "ethtool-6.16-1.hs.el10"),
                make_build(2000, "ethtool-6.15-1.hs.el10"),
            ],
        )];
        let plan = build_plan("ethtool", &tag_builds, &PruneOptions::default());
        // release_keep=2 → keep 5000 + 4000, untag 3000 + 2000.
        assert_eq!(plan.tags.len(), 1);
        let tp = &plan.tags[0];
        assert_eq!(tp.tag, "hyperscale10s-packages-main-release");
        assert_eq!(
            tp.keep,
            vec!["ethtool-6.18-1.hs.el10", "ethtool-6.17-1.hs.el10"]
        );
        assert_eq!(
            tp.untag,
            vec!["ethtool-6.16-1.hs.el10", "ethtool-6.15-1.hs.el10"]
        );
    }

    #[test]
    fn build_plan_release_and_testing_have_independent_retention() {
        // Distinct NVRs in each stage so the promotion-dedup
        // rule doesn't kick in — this test isolates the
        // keep-N-newest retention.
        let release_builds = vec![
            make_build(5000, "ethtool-6.18-1.hs.el10"),
            make_build(4000, "ethtool-6.17-1.hs.el10"),
            make_build(3000, "ethtool-6.16-1.hs.el10"),
        ];
        let testing_builds = vec![
            make_build(5100, "ethtool-6.19~rc1-1.hs.el10"),
            make_build(4100, "ethtool-6.19~rc2-1.hs.el10"),
            make_build(3100, "ethtool-6.19~rc3-1.hs.el10"),
        ];
        let tag_builds = vec![
            tb("hyperscale10s-packages-main-release", release_builds),
            tb("hyperscale10s-packages-main-testing", testing_builds),
        ];
        let plan = build_plan("ethtool", &tag_builds, &PruneOptions::default());
        // release_keep=2 → 5000 + 4000 stay, 3000 untagged.
        // testing_keep=1 → 5100 stays, 4100 + 3100 untagged.
        assert_eq!(plan.total_untags(), 3);

        let release = plan
            .tags
            .iter()
            .find(|t| t.tag.ends_with("-release"))
            .unwrap();
        assert_eq!(release.keep.len(), 2);
        assert_eq!(release.untag, vec!["ethtool-6.16-1.hs.el10"]);

        let testing = plan
            .tags
            .iter()
            .find(|t| t.tag.ends_with("-testing"))
            .unwrap();
        assert_eq!(testing.keep.len(), 1);
        assert_eq!(testing.untag.len(), 2);
    }

    #[test]
    fn build_plan_separates_tags() {
        let tag_builds = vec![
            tb(
                "hyperscale10s-packages-main-release",
                vec![
                    make_build(5000, "ethtool-6.18-1.hs.el10"),
                    make_build(4000, "ethtool-6.17-1.hs.el10"),
                    make_build(3000, "ethtool-6.16-1.hs.el10"),
                ],
            ),
            tb(
                "hyperscale9-packages-main-release",
                vec![
                    make_build(2500, "ethtool-6.16-1.hs.el9"),
                    make_build(2000, "ethtool-6.15-1.hs.el9"),
                ],
            ),
        ];
        let plan = build_plan("ethtool", &tag_builds, &PruneOptions::default());
        // EL10: release_keep=2 → 5000 + 4000 stay, 3000 untagged.
        // EL9: 2 builds, both stay (under retention).
        assert_eq!(plan.total_untags(), 1);
        let el10 = plan
            .tags
            .iter()
            .find(|t| t.tag.contains("hyperscale10s"))
            .unwrap();
        assert_eq!(el10.untag, vec!["ethtool-6.16-1.hs.el10"]);
        let el9 = plan
            .tags
            .iter()
            .find(|t| t.tag.starts_with("hyperscale9-"))
            .unwrap();
        assert!(el9.untag.is_empty());
        assert_eq!(el9.keep.len(), 2);
    }

    #[test]
    fn build_plan_untags_testing_builds_that_are_in_release() {
        // Same NVR is in both -release and -testing. The
        // -release entry stays put; the -testing entry is queued
        // for untag (regardless of retention), since promoted
        // builds shouldn't linger in testing.
        let shared_build = make_build(5000, "ethtool-6.18-1.hs.el10");
        let tag_builds = vec![
            tb(
                "hyperscale10s-packages-main-release",
                vec![shared_build.clone()],
            ),
            tb(
                "hyperscale10s-packages-main-testing",
                vec![shared_build.clone()],
            ),
        ];
        let plan = build_plan("ethtool", &tag_builds, &PruneOptions::default());
        let release = plan
            .tags
            .iter()
            .find(|t| t.tag.ends_with("-release"))
            .unwrap();
        assert_eq!(release.keep, vec!["ethtool-6.18-1.hs.el10"]);
        assert!(release.untag.is_empty());
        let testing = plan
            .tags
            .iter()
            .find(|t| t.tag.ends_with("-testing"))
            .unwrap();
        // No keeps in testing — the only build there was the
        // promoted one.
        assert!(testing.keep.is_empty());
        assert_eq!(testing.untag, vec!["ethtool-6.18-1.hs.el10"]);
    }

    #[test]
    fn build_plan_keeps_unpromoted_testing_builds() {
        // Testing has v1.5 and v1.6; release has v1.5. v1.5 gets
        // untagged from testing as promoted; v1.6 stays under
        // the testing_keep=1 rule.
        let v15 = make_build(5000, "ethtool-1.5-1.hs.el10");
        let v16 = make_build(6000, "ethtool-1.6-1.hs.el10");
        let tag_builds = vec![
            tb("hyperscale10s-packages-main-release", vec![v15.clone()]),
            tb(
                "hyperscale10s-packages-main-testing",
                vec![v15.clone(), v16.clone()],
            ),
        ];
        let plan = build_plan("ethtool", &tag_builds, &PruneOptions::default());
        let testing = plan
            .tags
            .iter()
            .find(|t| t.tag.ends_with("-testing"))
            .unwrap();
        assert_eq!(testing.keep, vec!["ethtool-1.6-1.hs.el10"]);
        assert_eq!(testing.untag, vec!["ethtool-1.5-1.hs.el10"]);
    }

    #[test]
    fn build_plan_untags_testing_older_than_release() {
        // Release is at 6.18. Testing holds an older 6.17 (a
        // stale leftover) and a newer 6.19. 6.17 is superseded —
        // even though it's not the exact released build — and
        // gets untagged; 6.19 stays as a real testing candidate.
        let tag_builds = vec![
            tb(
                "hyperscale10s-packages-main-release",
                vec![make_build(5000, "ethtool-6.18-1.hs.el10")],
            ),
            tb(
                "hyperscale10s-packages-main-testing",
                vec![
                    make_build(4000, "ethtool-6.17-1.hs.el10"),
                    make_build(6000, "ethtool-6.19-1.hs.el10"),
                ],
            ),
        ];
        let plan = build_plan("ethtool", &tag_builds, &PruneOptions::default());
        let testing = plan
            .tags
            .iter()
            .find(|t| t.tag.ends_with("-testing"))
            .unwrap();
        assert_eq!(testing.keep, vec!["ethtool-6.19-1.hs.el10"]);
        assert_eq!(testing.untag, vec!["ethtool-6.17-1.hs.el10"]);
    }

    #[test]
    fn build_plan_promotion_rule_respects_repository_boundary() {
        // Build is in main-release but not facebook-release. A
        // facebook-testing copy of the same build is NOT
        // considered promoted because its sibling is
        // facebook-release.
        let shared = make_build(5000, "ethtool-6.18-1.hs.el10");
        let tag_builds = vec![
            tb("hyperscale10s-packages-main-release", vec![shared.clone()]),
            tb(
                "hyperscale10s-packages-facebook-testing",
                vec![shared.clone()],
            ),
        ];
        let opts = PruneOptions {
            repositories: vec!["main".to_string(), "facebook".to_string()],
            ..PruneOptions::default()
        };
        let plan = build_plan("ethtool", &tag_builds, &opts);
        let fb_testing = plan
            .tags
            .iter()
            .find(|t| t.tag == "hyperscale10s-packages-facebook-testing")
            .unwrap();
        assert_eq!(fb_testing.keep, vec!["ethtool-6.18-1.hs.el10"]);
        assert!(fb_testing.untag.is_empty());
    }

    #[test]
    fn build_plan_nothing_to_do_when_under_retention() {
        let tag_builds = vec![tb(
            "hyperscale10s-packages-main-release",
            vec![make_build(5000, "ethtool-6.18-1.hs.el10")],
        )];
        let plan = build_plan("ethtool", &tag_builds, &PruneOptions::default());
        assert_eq!(plan.total_untags(), 0);
        // Tag is still tracked so the render shows it as kept.
        assert_eq!(plan.tags.len(), 1);
        assert_eq!(plan.tags[0].keep, vec!["ethtool-6.18-1.hs.el10"]);
    }

    #[test]
    fn normalize_releases_validates_and_dedups() {
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(normalize_releases(&v(&["10s"])).unwrap(), vec!["10s"]);
        // The `hyperscale` prefix is stripped.
        assert_eq!(
            normalize_releases(&v(&["hyperscale10s", "9s"])).unwrap(),
            vec!["10s", "9s"]
        );
        // Dedup in input order (covers both CSV-split and repeated).
        assert_eq!(
            normalize_releases(&v(&["9", "9", "10"])).unwrap(),
            vec!["9", "10"]
        );
        // Unknown token is an error.
        assert!(normalize_releases(&v(&["11s"])).is_err());
        // Empty input is OK (caller defaults).
        assert_eq!(normalize_releases(&[]).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn parse_repositories_handles_spaces_and_trailing_commas() {
        assert_eq!(parse_repositories("main"), vec!["main"]);
        assert_eq!(
            parse_repositories("main, facebook"),
            vec!["main", "facebook"]
        );
        assert_eq!(parse_repositories("main,"), vec!["main"]);
        assert_eq!(parse_repositories(""), Vec::<String>::new());
    }

    #[test]
    fn render_plan_lists_keep_and_untag_per_tag() {
        let plan = PrunePlan {
            package: "ethtool".into(),
            tags: vec![TagPlan {
                tag: "hyperscale10s-packages-main-release".into(),
                keep: vec!["ethtool-6.18-1.hs.el10".into()],
                untag: vec!["ethtool-6.16-1.hs.el10".into()],
            }],
        };
        let rendered = render_plan(&plan);
        assert!(rendered.contains("would untag 1 build(s)"));
        assert!(rendered.contains("keep 1, untag 1"));
        assert!(rendered.contains("ethtool-6.18-1.hs.el10"));
        assert!(rendered.contains("ethtool-6.16-1.hs.el10"));
        assert!(rendered.contains("hyperscale10s-packages-main-release"));
    }

    #[test]
    fn render_plan_zero_untags_lists_kept_builds() {
        let plan = PrunePlan {
            package: "ethtool".into(),
            tags: vec![TagPlan {
                tag: "hyperscale10s-packages-main-release".into(),
                keep: vec!["ethtool-6.18-1.hs.el10".into()],
                untag: vec![],
            }],
        };
        let rendered = render_plan(&plan);
        assert!(rendered.contains("nothing to untag"));
        assert!(rendered.contains("ethtool-6.18-1.hs.el10"));
    }

    #[test]
    fn render_plan_no_managed_tags_is_distinct_from_nothing_to_untag() {
        let plan = PrunePlan {
            package: "ethtool".into(),
            tags: vec![],
        };
        assert!(render_plan(&plan).contains("no managed hyperscale tags"));
    }
}
