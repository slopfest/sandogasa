// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `prune-archived` subcommand — clean up CBS builds for packages
//! whose upstream GitLab repo is archived.
//!
//! Driven by the manifest's `archived = true` marker (set by
//! `poi-tracker sync-gitlab --mark-unshipped`). For each archived
//! package, every build in its hyperscale `-release` and
//! `-testing` tags is compared against the **stock** distro
//! version for that tag's channel:
//!
//! - Stream tags (`hyperscaleNs-…`) compare against CentOS Stream
//!   N (`centos_stream_N` on Repology).
//! - RHEL tags (`hyperscaleN-…`) compare against AlmaLinux N
//!   (`almalinux_N`).
//!
//! A build whose version is **not newer** than stock is safe to
//! untag — stock carries it now, so the archived build is
//! redundant. A build **newer** than stock (or for which stock has
//! no entry at all) is *ahead*: the archived repo may be its only
//! source, so it is never untagged automatically — interactively
//! the user is prompted per build, and under `--yes` it is warned
//! about and skipped.

use std::cmp::Ordering;

use sandogasa_koji::untag_build;
use sandogasa_repology as repology;
use sandogasa_rpmvercmp::rpmvercmp;

use crate::cbs::{Build, Client};
use crate::prune_tags::{KOJI_PROFILE, PruneOptions, TagBuilds, confirm, fetch_managed_tags};

/// Parse a hyperscale tag's leading version token into
/// `(major, is_stream)`: `hyperscale10s-…` → `(10, true)`,
/// `hyperscale9-…` → `(9, false)`. Returns `None` for tags that
/// don't start with `hyperscale` or lack a numeric version.
pub fn parse_tag_release(tag: &str) -> Option<(u32, bool)> {
    let rest = tag.strip_prefix("hyperscale")?;
    let token = rest.split('-').next()?;
    let (digits, is_stream) = match token.strip_suffix('s') {
        Some(d) => (d, true),
        None => (token, false),
    };
    Some((digits.parse().ok()?, is_stream))
}

/// The stock version for a tag's channel: CentOS Stream N for a
/// Stream tag, AlmaLinux N for a RHEL tag. `None` when stock has
/// no entry for the package in that release.
pub fn stock_version(
    packages: &[repology::Package],
    major: u32,
    is_stream: bool,
) -> Option<String> {
    let pkg = if is_stream {
        repology::centos_stream_release(packages, major)
    } else {
        repology::almalinux_release(packages, major)
    };
    pkg.map(|p| p.version.clone())
}

/// Per-tag decision for an archived package.
#[derive(Debug, Clone)]
pub struct ArchivedTagPlan {
    pub tag: String,
    /// Stock version this tag's builds were compared against
    /// (`None` if stock has no entry — then every build is ahead).
    pub stock: Option<String>,
    /// Builds not newer than stock — safe to untag.
    pub untag: Vec<String>,
    /// Builds newer than stock, or any build when stock is absent —
    /// the archived repo may be their only source, so they are
    /// never untagged without an explicit per-build decision.
    pub ahead: Vec<String>,
}

/// What `prune-archived` would do for one package.
#[derive(Debug, Clone)]
pub struct ArchivedPlan {
    pub package: String,
    pub tags: Vec<ArchivedTagPlan>,
}

impl ArchivedPlan {
    /// Total builds safe to untag (not newer than stock).
    pub fn total_untag(&self) -> usize {
        self.tags.iter().map(|t| t.untag.len()).sum()
    }

    /// Total builds ahead of stock (need a per-build decision).
    pub fn total_ahead(&self) -> usize {
        self.tags.iter().map(|t| t.ahead.len()).sum()
    }
}

/// Classify each build in each tag against its channel's stock
/// version. Pure: `repology_packages` is the package's Repology
/// data, `tag_builds` the per-tag CBS builds.
pub fn build_plan(
    package: &str,
    tag_builds: &[TagBuilds],
    repology_packages: &[repology::Package],
) -> ArchivedPlan {
    let mut tags: Vec<ArchivedTagPlan> = Vec::new();
    for (tag, builds) in tag_builds {
        let stock = parse_tag_release(tag)
            .and_then(|(major, is_stream)| stock_version(repology_packages, major, is_stream));

        let mut untag = Vec::new();
        let mut ahead = Vec::new();
        // Newest-first so prompts and output read naturally.
        let mut ordered: Vec<&Build> = builds.iter().collect();
        ordered.sort_by_key(|b| std::cmp::Reverse(b.build_id));
        for b in ordered {
            let newer_than_stock = match &stock {
                Some(s) => rpmvercmp(&b.version, s) == Ordering::Greater,
                None => true, // no stock entry: treat as ahead/only-source
            };
            if newer_than_stock {
                ahead.push(b.nvr.clone());
            } else {
                untag.push(b.nvr.clone());
            }
        }
        tags.push(ArchivedTagPlan {
            tag: tag.clone(),
            stock,
            untag,
            ahead,
        });
    }
    tags.sort_by(|a, b| a.tag.cmp(&b.tag));
    ArchivedPlan {
        package: package.to_string(),
        tags,
    }
}

/// Render the plan for human review.
pub fn render_plan(plan: &ArchivedPlan) -> String {
    let untag = plan.total_untag();
    let ahead = plan.total_ahead();
    if untag == 0 && ahead == 0 {
        return format!("{}: no hyperscale builds tagged.\n", plan.package);
    }
    let mut out = format!(
        "{}: {untag} build(s) at/behind stock to untag, {ahead} ahead of stock\n",
        plan.package
    );
    for tp in &plan.tags {
        if tp.untag.is_empty() && tp.ahead.is_empty() {
            continue;
        }
        let stock = tp.stock.as_deref().unwrap_or("(none in stock)");
        out.push_str(&format!("  {} [stock {stock}]\n", tp.tag));
        for nvr in &tp.untag {
            out.push_str(&format!("    untag (<= stock): {nvr}\n"));
        }
        for nvr in &tp.ahead {
            out.push_str(&format!("    ahead of stock:   {nvr}\n"));
        }
    }
    out
}

/// Outcome of applying a plan.
#[derive(Debug, Default, Clone, Copy)]
pub struct ApplyOutcome {
    pub untagged: usize,
    pub skipped_ahead: usize,
    pub errors: usize,
}

/// Apply a plan. The safe (≤ stock) untags are gated by one batch
/// confirmation per package (skipped under `assume_yes`); declining
/// it skips the whole package, ahead builds included. Ahead-of-stock
/// builds are then resolved individually: under `assume_yes` each is
/// warned about and skipped, otherwise prompted per build (default
/// no). Ahead builds are never untagged without an explicit yes.
pub fn apply_plan(plan: &ArchivedPlan, assume_yes: bool, verbose: bool) -> ApplyOutcome {
    let mut out = ApplyOutcome::default();
    let safe_total = plan.total_untag();

    // One batch confirmation for the redundant (≤ stock) builds.
    // Declining means "leave this package alone" — including its
    // ahead-of-stock builds.
    if safe_total > 0 && !assume_yes {
        let approved = confirm(&format!(
            "{}: untag {safe_total} build(s) at or behind stock?",
            plan.package
        ))
        .unwrap_or(false);
        if !approved {
            return out;
        }
    }
    for tp in &plan.tags {
        for nvr in &tp.untag {
            match do_untag(&tp.tag, nvr, verbose) {
                Ok(()) => out.untagged += 1,
                Err(()) => out.errors += 1,
            }
        }
        for nvr in &tp.ahead {
            let stock = tp.stock.as_deref().unwrap_or("(none in stock)");
            if assume_yes {
                eprintln!(
                    "skipping {nvr} in {}: newer than stock {stock} \
                     (--yes never untags ahead-of-stock builds)",
                    tp.tag
                );
                out.skipped_ahead += 1;
                continue;
            }
            let approved = confirm(&format!(
                "{nvr} in {} is newer than stock {stock}; untag anyway?",
                tp.tag
            ))
            .unwrap_or(false);
            if approved {
                match do_untag(&tp.tag, nvr, verbose) {
                    Ok(()) => out.untagged += 1,
                    Err(()) => out.errors += 1,
                }
            } else {
                out.skipped_ahead += 1;
            }
        }
    }
    out
}

/// Untag one build, logging the result.
fn do_untag(tag: &str, nvr: &str, verbose: bool) -> Result<(), ()> {
    if verbose {
        eprintln!("[hs-relmon] koji untag-build {tag} {nvr}");
    }
    match untag_build(tag, nvr, Some(KOJI_PROFILE)) {
        Ok(()) => {
            eprintln!("untagged {nvr} from {tag}");
            Ok(())
        }
        Err(e) => {
            eprintln!("error: untag-build {tag} {nvr}: {e}");
            Err(())
        }
    }
}

/// Fetch a package's managed-tag builds and classify them against
/// stock. The Repology lookup is injected so callers can supply a
/// real client or a test stub.
pub fn plan_for_package<F>(
    cbs: &Client,
    package: &str,
    opts: &PruneOptions,
    fetch_repology: F,
    verbose: bool,
) -> Result<ArchivedPlan, Box<dyn std::error::Error>>
where
    F: Fn(&str) -> Result<Vec<repology::Package>, Box<dyn std::error::Error>>,
{
    let tag_builds = fetch_managed_tags(cbs, package, opts, verbose);
    let repology_packages = if tag_builds.is_empty() {
        Vec::new()
    } else {
        fetch_repology(package)?
    };
    Ok(build_plan(package, &tag_builds, &repology_packages))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(build_id: i64, nvr: &str) -> Build {
        let parts: Vec<&str> = nvr.rsplitn(3, '-').collect();
        Build {
            build_id,
            name: parts[2].to_string(),
            version: parts[1].to_string(),
            release: parts[0].to_string(),
            nvr: nvr.to_string(),
        }
    }

    fn rpkg(repo: &str, version: &str) -> repology::Package {
        serde_json::from_str(&format!(
            r#"{{"repo":"{repo}","version":"{version}","status":"outdated"}}"#
        ))
        .unwrap()
    }

    #[test]
    fn parse_tag_release_stream_and_rhel() {
        assert_eq!(
            parse_tag_release("hyperscale10s-packages-main-release"),
            Some((10, true))
        );
        assert_eq!(
            parse_tag_release("hyperscale9-packages-facebook-release"),
            Some((9, false))
        );
        assert_eq!(parse_tag_release("not-a-hyperscale-tag"), None);
    }

    #[test]
    fn stream_tag_uses_centos_rhel_tag_uses_almalinux() {
        // Distinct el9 versions in each channel prove the mapping:
        // 9s -> centos_stream_9, 9 -> almalinux_9 (never crossed).
        let pkgs = vec![
            rpkg("centos_stream_9", "1.7.4.1"),
            rpkg("almalinux_9", "1.7.0"),
        ];
        assert_eq!(stock_version(&pkgs, 9, true).as_deref(), Some("1.7.4.1"));
        assert_eq!(stock_version(&pkgs, 9, false).as_deref(), Some("1.7.0"));
    }

    #[test]
    fn build_plan_untags_at_or_behind_stock_flags_ahead() {
        // socat: hyperscale9s has 1.7.4.4 but stock c9s is 1.7.4.1
        // (ahead -> flagged); hyperscale10s has 1.7.4.4 and stock
        // c10s caught up to 1.7.4.4 (equal -> untag).
        let tag_builds = vec![
            (
                "hyperscale9s-packages-main-release".to_string(),
                vec![build(5000, "socat-1.7.4.4-4.hs.el9")],
            ),
            (
                "hyperscale10s-packages-main-release".to_string(),
                vec![build(6000, "socat-1.7.4.4-4.hs.el10")],
            ),
        ];
        let repology_packages = vec![
            rpkg("centos_stream_9", "1.7.4.1"),
            rpkg("centos_stream_10", "1.7.4.4"),
        ];
        let plan = build_plan("socat", &tag_builds, &repology_packages);
        assert_eq!(plan.total_untag(), 1);
        assert_eq!(plan.total_ahead(), 1);

        let el9 = plan
            .tags
            .iter()
            .find(|t| t.tag.starts_with("hyperscale9s"))
            .unwrap();
        assert_eq!(el9.ahead, vec!["socat-1.7.4.4-4.hs.el9"]);
        assert!(el9.untag.is_empty());

        let el10 = plan
            .tags
            .iter()
            .find(|t| t.tag.starts_with("hyperscale10s"))
            .unwrap();
        assert_eq!(el10.untag, vec!["socat-1.7.4.4-4.hs.el10"]);
        assert!(el10.ahead.is_empty());
    }

    #[test]
    fn build_plan_no_stock_entry_is_ahead() {
        // Stock has no entry for this package/release: don't untag
        // (the archived repo is its only source).
        let tag_builds = vec![(
            "hyperscale10s-packages-main-release".to_string(),
            vec![build(7000, "wprof-0.3-1.hs.el10")],
        )];
        let plan = build_plan("wprof", &tag_builds, &[]);
        assert_eq!(plan.total_untag(), 0);
        assert_eq!(plan.tags[0].ahead, vec!["wprof-0.3-1.hs.el10"]);
        assert!(plan.tags[0].stock.is_none());
    }

    #[test]
    fn build_plan_older_than_stock_untags() {
        let tag_builds = vec![(
            "hyperscale10s-packages-main-release".to_string(),
            vec![build(8000, "foo-1.0-1.hs.el10")],
        )];
        let repology_packages = vec![rpkg("centos_stream_10", "1.2")];
        let plan = build_plan("foo", &tag_builds, &repology_packages);
        assert_eq!(plan.tags[0].untag, vec!["foo-1.0-1.hs.el10"]);
        assert!(plan.tags[0].ahead.is_empty());
    }
}
