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

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use sandogasa_koji::untag_build;

use crate::cbs::{Build, Client, hyperscale_builds};

/// CBS Koji profile used by the workspace. Matches
/// `cpu-sig-tracker`'s constant.
const KOJI_PROFILE: &str = "cbs";

/// Default number of builds to keep in each `-release` tag.
pub const DEFAULT_RELEASE_KEEP: usize = 2;
/// Default number of builds to keep in each `-testing` tag.
pub const DEFAULT_TESTING_KEEP: usize = 1;
/// Default tag repository (the segment between `-packages-`
/// and the stage suffix). `main` is the standard SIG channel;
/// users can opt into `facebook`, `experimental`, etc. via
/// `--repositories`.
pub const DEFAULT_REPOSITORY: &str = "main";

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

/// One hyperscale build paired with the tags it carries. The
/// extra type alias keeps the public APIs readable and quiets
/// clippy's `type_complexity` lint.
pub type BuildWithTags = (Build, Vec<String>);

/// Build the untag plan for one package: group its hyperscale
/// builds by managed tag, sort by `build_id` descending, and
/// split each tag into keep (N newest) + untag (the rest).
///
/// Tags outside our repository filter or outside `-release`/
/// `-testing` are skipped entirely — `-candidate` and other
/// repositories are not touched here.
pub fn build_plan(
    package: &str,
    builds_with_tags: &[BuildWithTags],
    opts: &PruneOptions,
) -> PrunePlan {
    let mut groups: BTreeMap<String, Vec<(i64, String)>> = BTreeMap::new();
    for (build, tags) in builds_with_tags {
        for tag in tags {
            if !is_managed_tag(tag, &opts.repositories) {
                continue;
            }
            groups
                .entry(tag.clone())
                .or_default()
                .push((build.build_id, build.nvr.clone()));
        }
    }

    let mut tag_plans: Vec<TagPlan> = Vec::new();
    for (tag, mut entries) in groups {
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        let keep_count = if tag.ends_with("-release") {
            opts.release_keep
        } else {
            opts.testing_keep
        };
        let nvrs: Vec<String> = entries.into_iter().map(|(_, n)| n).collect();
        let (keep, untag) = if nvrs.len() <= keep_count {
            (nvrs, Vec::new())
        } else {
            let (k, u) = nvrs.split_at(keep_count);
            (k.to_vec(), u.to_vec())
        };
        tag_plans.push(TagPlan { tag, keep, untag });
    }
    PrunePlan {
        package: package.to_string(),
        tags: tag_plans,
    }
}

/// Is `tag` a hyperscale `-release` or `-testing` tag with a
/// repository in our allow-list? The repository is the segment
/// after the last `-packages-`.
fn is_managed_tag(tag: &str, repositories: &[String]) -> bool {
    if !tag.starts_with("hyperscale") {
        return false;
    }
    let Some(repo) = extract_repository(tag) else {
        return false;
    };
    repositories.iter().any(|r| r == repo)
}

fn extract_repository(tag: &str) -> Option<&str> {
    let root = tag
        .strip_suffix("-release")
        .or_else(|| tag.strip_suffix("-testing"))?;
    let (_, repo) = root.rsplit_once("-packages-")?;
    Some(repo)
}

/// Pull a package's hyperscale builds + their tag lists from
/// CBS Koji. Calls `listTags` once per hyperscale build (one
/// XML-RPC roundtrip each) — manageable for typical packages
/// (~10s of builds) but linear in build count.
pub fn fetch_builds_with_tags(
    client: &Client,
    package: &str,
    verbose: bool,
) -> Result<Vec<BuildWithTags>, Box<dyn std::error::Error>> {
    let package_id = client
        .get_package_id(package)?
        .ok_or_else(|| format!("package '{package}' not found in CBS Koji"))?;
    let builds = client.list_builds(package_id)?;
    let mut hyperscale_set: Vec<&Build> = Vec::new();
    for el in [9u32, 10u32] {
        hyperscale_set.extend(hyperscale_builds(&builds, el));
    }
    if verbose {
        eprintln!(
            "[hs-relmon] {package}: fetching tags for {} hyperscale build(s)",
            hyperscale_set.len()
        );
    }
    let mut out = Vec::with_capacity(hyperscale_set.len());
    for build in hyperscale_set {
        let tags = client.list_tags(build.build_id)?;
        out.push((build.clone(), tags));
    }
    Ok(out)
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
    fn extract_repository_handles_release_and_testing() {
        assert_eq!(
            extract_repository("hyperscale10s-packages-main-release"),
            Some("main")
        );
        assert_eq!(
            extract_repository("hyperscale9-packages-facebook-testing"),
            Some("facebook")
        );
        assert_eq!(
            extract_repository("hyperscale10s-packages-experimental-release"),
            Some("experimental")
        );
        assert_eq!(
            extract_repository("hyperscale10s-packages-main-candidate"),
            None
        );
        assert_eq!(extract_repository("unrelated-tag"), None);
    }

    #[test]
    fn is_managed_tag_filters_by_repository() {
        let main = vec!["main".to_string()];
        let main_facebook = vec!["main".to_string(), "facebook".to_string()];

        assert!(is_managed_tag("hyperscale10s-packages-main-release", &main));
        assert!(!is_managed_tag(
            "hyperscale10s-packages-facebook-release",
            &main
        ));
        assert!(is_managed_tag(
            "hyperscale10s-packages-facebook-release",
            &main_facebook
        ));
        assert!(!is_managed_tag(
            "hyperscale10s-packages-main-candidate",
            &main
        ));
        assert!(!is_managed_tag("dist-c10s", &main));
    }

    #[test]
    fn build_plan_keeps_n_newest_per_tag() {
        let builds_with_tags = vec![
            (
                make_build(5000, "ethtool-6.18-1.hs.el10"),
                vec!["hyperscale10s-packages-main-release".to_string()],
            ),
            (
                make_build(4000, "ethtool-6.17-1.hs.el10"),
                vec!["hyperscale10s-packages-main-release".to_string()],
            ),
            (
                make_build(3000, "ethtool-6.16-1.hs.el10"),
                vec!["hyperscale10s-packages-main-release".to_string()],
            ),
            (
                make_build(2000, "ethtool-6.15-1.hs.el10"),
                vec!["hyperscale10s-packages-main-release".to_string()],
            ),
        ];
        let plan = build_plan("ethtool", &builds_with_tags, &PruneOptions::default());
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
        let builds_with_tags = vec![
            (
                make_build(5000, "ethtool-6.18-1.hs.el10"),
                vec![
                    "hyperscale10s-packages-main-release".to_string(),
                    "hyperscale10s-packages-main-testing".to_string(),
                ],
            ),
            (
                make_build(4000, "ethtool-6.17-1.hs.el10"),
                vec![
                    "hyperscale10s-packages-main-release".to_string(),
                    "hyperscale10s-packages-main-testing".to_string(),
                ],
            ),
            (
                make_build(3000, "ethtool-6.16-1.hs.el10"),
                vec![
                    "hyperscale10s-packages-main-release".to_string(),
                    "hyperscale10s-packages-main-testing".to_string(),
                ],
            ),
        ];
        let plan = build_plan("ethtool", &builds_with_tags, &PruneOptions::default());
        // release_keep=2 → 5000 + 4000 stay, 3000 untagged
        // testing_keep=1 → 5000 stays, 4000 + 3000 untagged
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
    fn build_plan_ignores_non_main_repository_by_default() {
        let builds_with_tags = vec![
            (
                make_build(5000, "ethtool-6.18-1.hs.el10"),
                vec!["hyperscale10s-packages-main-release".to_string()],
            ),
            (
                make_build(4000, "ethtool-6.17-1.hs.el10"),
                vec!["hyperscale10s-packages-facebook-release".to_string()],
            ),
            (
                make_build(3000, "ethtool-6.16-1.hs.el10"),
                vec!["hyperscale10s-packages-facebook-release".to_string()],
            ),
            (
                make_build(2000, "ethtool-6.15-1.hs.el10"),
                vec!["hyperscale10s-packages-facebook-release".to_string()],
            ),
        ];
        // Default repositories = ["main"] — facebook is
        // untouched even though it has 3 builds (over retention).
        let plan = build_plan("ethtool", &builds_with_tags, &PruneOptions::default());
        // The only main tag has 1 build (≤ retention) → 1 group
        // with 0 untags.
        assert_eq!(plan.total_untags(), 0);
    }

    #[test]
    fn build_plan_separates_el_versions_via_tag_name() {
        let builds_with_tags = vec![
            (
                make_build(5000, "ethtool-6.18-1.hs.el10"),
                vec!["hyperscale10s-packages-main-release".to_string()],
            ),
            (
                make_build(4000, "ethtool-6.17-1.hs.el10"),
                vec!["hyperscale10s-packages-main-release".to_string()],
            ),
            (
                make_build(3000, "ethtool-6.16-1.hs.el10"),
                vec!["hyperscale10s-packages-main-release".to_string()],
            ),
            // EL9 — separate tag, separate retention.
            (
                make_build(2500, "ethtool-6.16-1.hs.el9"),
                vec!["hyperscale9-packages-main-release".to_string()],
            ),
            (
                make_build(2000, "ethtool-6.15-1.hs.el9"),
                vec!["hyperscale9-packages-main-release".to_string()],
            ),
        ];
        let plan = build_plan("ethtool", &builds_with_tags, &PruneOptions::default());
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
    fn build_plan_nothing_to_do_when_under_retention() {
        let builds_with_tags = vec![(
            make_build(5000, "ethtool-6.18-1.hs.el10"),
            vec!["hyperscale10s-packages-main-release".to_string()],
        )];
        let plan = build_plan("ethtool", &builds_with_tags, &PruneOptions::default());
        assert_eq!(plan.total_untags(), 0);
        // Tag is still tracked so the render shows it as kept.
        assert_eq!(plan.tags.len(), 1);
        assert_eq!(plan.tags[0].keep, vec!["ethtool-6.18-1.hs.el10"]);
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
