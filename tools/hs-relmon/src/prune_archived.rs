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
//!   N (`centos_stream_N` on Repology) plus EPEL N (`epel_N`).
//! - RHEL tags (`hyperscaleN-…`) compare against AlmaLinux N
//!   (`almalinux_N`) plus EPEL N.
//!
//! EPEL counts because it sits on top of either base: a package the
//! SIG carried until "stock" caught up is just as redundant once EPEL
//! ships it — wprof's el10 builds, once EPEL 10 had 0.5-3.el10_3.
//! Whichever of the two carries the newer package is the stock version.
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

use crate::cbs::Build;
use crate::prune_tags::{KOJI_PROFILE, TagBuilds};

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

/// What stock has of a package in one release: the version, and the
/// release when Repology recorded the full `version-release`
/// (`origversion`), so a SIG build of the same version can still be
/// told newer by its release.
#[derive(Debug, Clone, PartialEq)]
pub struct Stock {
    pub version: String,
    pub release: Option<String>,
}

impl Stock {
    /// From a Repology entry: `origversion` is `[epoch:]version-release`
    /// for an RPM repo.
    fn from(p: &repology::Package) -> Self {
        let release = p.origversion.as_deref().and_then(|ov| {
            let ov = ov.split_once(':').map_or(ov, |(_, rest)| rest);
            ov.rsplit_once('-').map(|(_, rel)| rel.to_string())
        });
        Self {
            version: p.version.clone(),
            release,
        }
    }

    /// `version-release`, or the version alone when stock's release is
    /// unknown.
    pub fn label(&self) -> String {
        match &self.release {
            Some(r) => format!("{}-{r}", self.version),
            None => self.version.clone(),
        }
    }
}

/// The stock version for a tag's channel: CentOS Stream N for a
/// Stream tag, AlmaLinux N for a RHEL tag, either plus EPEL N,
/// whichever is newer — of what stands in for the
/// SIG package named `source`: a stock source of the same name, or one
/// that ships a binary of that name, since a consumer needs the binary
/// `autoconf`, whoever builds it. Repology files every source it deems
/// the same project together, so CentOS Stream 9's `autoconf-latest`
/// (2.71, binaries `autoconf-latest` and `autoconf271`) sits beside its
/// `autoconf` (2.69): only the latter says whether stock's `autoconf`
/// has caught up. `None` when stock has nothing of the kind in that
/// release.
pub fn stock_version(
    packages: &[repology::Package],
    major: u32,
    is_stream: bool,
    source: &str,
) -> Option<Stock> {
    let stands_in = |p: &repology::Package| {
        // An entry without names cannot be told apart and counts.
        p.srcname.is_none() && p.binnames.is_none()
            || p.srcname.as_deref() == Some(source)
            || p.binnames.iter().flatten().any(|b| b == source)
    };
    let same_source: Vec<repology::Package> =
        packages.iter().filter(|p| stands_in(p)).cloned().collect();
    // The base distro of the channel, and EPEL on top of it: whichever
    // carries the newer package is what a system would get.
    let base = if is_stream {
        repology::centos_stream_release(&same_source, major)
    } else {
        repology::almalinux_release(&same_source, major)
    };
    let epel = repology::epel_release(&same_source, major);
    [base, epel]
        .into_iter()
        .flatten()
        .map(Stock::from)
        .max_by(|a, b| sandogasa_rpmvercmp::compare_evr(&a.label(), &b.label()))
}

/// Per-tag decision for an archived package.
#[derive(Debug, Clone)]
pub struct ArchivedTagPlan {
    pub tag: String,
    /// Stock version this tag's builds were compared against
    /// (`None` if stock has no entry — then every build is ahead).
    pub stock: Option<String>,
    /// Stock's release too, when Repology recorded it.
    pub stock_release: Option<String>,
    /// Builds not newer than stock — safe to untag.
    pub untag: Vec<String>,
    /// Builds newer than stock, or any build when stock is absent —
    /// the archived repo may be their only source, so they are
    /// never untagged without an explicit per-build decision.
    pub ahead: Vec<String>,
    /// Builds of stock's version but a newer release: stock has the
    /// upstream version, yet a system running the SIG build keeps it
    /// until stock's version moves — dnf does not downgrade, while a
    /// configuration manager told to upgrade may. Untagged only on an
    /// explicit per-build decision, like `ahead`.
    pub release_ahead: Vec<String>,
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

    /// Total builds ahead of stock, by version or by release alone
    /// (each needs a per-build decision).
    pub fn total_ahead(&self) -> usize {
        self.total_version_ahead() + self.total_release_ahead()
    }

    /// Builds whose version is newer than stock's.
    pub fn total_version_ahead(&self) -> usize {
        self.tags.iter().map(|t| t.ahead.len()).sum()
    }

    /// Builds of stock's version with a newer release.
    pub fn total_release_ahead(&self) -> usize {
        self.tags.iter().map(|t| t.release_ahead.len()).sum()
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
        let stock = parse_tag_release(tag).and_then(|(major, is_stream)| {
            stock_version(repology_packages, major, is_stream, package)
        });

        let mut untag = Vec::new();
        let mut ahead = Vec::new();
        let mut release_ahead = Vec::new();
        // Newest-first so prompts and output read naturally.
        let mut ordered: Vec<&Build> = builds.iter().collect();
        ordered.sort_by_key(|b| std::cmp::Reverse(b.build_id));
        for b in ordered {
            match &stock {
                // No stock entry: treat as ahead/only-source.
                None => ahead.push(b.nvr.clone()),
                Some(s) => match rpmvercmp(&b.version, &s.version) {
                    Ordering::Greater => ahead.push(b.nvr.clone()),
                    Ordering::Equal
                        if s.release
                            .as_deref()
                            .is_some_and(|r| rpmvercmp(&b.release, r) == Ordering::Greater) =>
                    {
                        release_ahead.push(b.nvr.clone())
                    }
                    _ => untag.push(b.nvr.clone()),
                },
            }
        }
        tags.push(ArchivedTagPlan {
            tag: tag.clone(),
            stock: stock.as_ref().map(|s| s.version.clone()),
            stock_release: stock.and_then(|s| s.release),
            untag,
            ahead,
            release_ahead,
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
        "{}: {untag} build(s) at/behind stock to untag, {ahead} ahead of stock{}\n",
        plan.package,
        match plan.total_release_ahead() {
            0 => String::new(),
            n => format!(" ({n} by release alone)"),
        }
    );
    for tp in &plan.tags {
        if tp.untag.is_empty() && tp.ahead.is_empty() && tp.release_ahead.is_empty() {
            continue;
        }
        out.push_str(&format!("  {} [stock {}]\n", tp.tag, tp.stock_label()));
        for nvr in &tp.untag {
            out.push_str(&format!("    untag (<= stock): {nvr}\n"));
        }
        for nvr in &tp.ahead {
            out.push_str(&format!("    ahead of stock:   {nvr}\n"));
        }
        for nvr in &tp.release_ahead {
            out.push_str(&format!("    newer release:    {nvr}\n"));
        }
    }
    out
}

impl ArchivedTagPlan {
    /// `version-release` of stock, the version alone, or a note that
    /// stock has nothing.
    pub fn stock_label(&self) -> String {
        match (&self.stock, &self.stock_release) {
            (Some(v), Some(r)) => format!("{v}-{r}"),
            (Some(v), None) => v.clone(),
            (None, _) => "(none in stock)".to_string(),
        }
    }
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
        let approved = sandogasa_cli::confirm(
            &format!(
                "{}: untag {safe_total} build(s) at or behind stock?",
                plan.package
            ),
            false,
        )
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
        let stock = tp.stock_label();
        let ahead = tp.ahead.iter().map(|nvr| {
            (
                nvr,
                format!("{nvr} in {} is newer than stock {stock}", tp.tag),
            )
        });
        let release_ahead = tp.release_ahead.iter().map(|nvr| {
            (
                nvr,
                format!(
                    "{nvr} in {} has stock's version but a newer release than stock {stock}: a \
                     system running it keeps it until stock's version moves (dnf does not \
                     downgrade; a configuration manager told to upgrade would)",
                    tp.tag
                ),
            )
        });
        for (nvr, why) in ahead.chain(release_ahead) {
            if assume_yes {
                eprintln!("skipping {nvr}: {why} (--yes never untags ahead-of-stock builds)");
                out.skipped_ahead += 1;
                continue;
            }
            let approved =
                sandogasa_cli::confirm(&format!("{why}; untag anyway?"), false).unwrap_or(false);
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
pub(crate) fn do_untag(tag: &str, nvr: &str, verbose: bool) -> Result<(), ()> {
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

/// Classify a package's managed-tag builds (from a [`TagIndex`] or
/// [`fetch_managed_tags`]) against stock. The Repology lookup is
/// injected so callers can supply a real client or a test stub; it is
/// skipped when there is nothing to compare.
pub fn plan_for_package<F>(
    package: &str,
    tag_builds: &[TagBuilds],
    fetch_repology: F,
) -> Result<ArchivedPlan, Box<dyn std::error::Error>>
where
    F: Fn(&str) -> Result<Vec<repology::Package>, Box<dyn std::error::Error>>,
{
    let repology_packages = if tag_builds.is_empty() {
        Vec::new()
    } else {
        fetch_repology(package)?
    };
    Ok(build_plan(package, tag_builds, &repology_packages))
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
        rpkg_src(repo, version, None, &[])
    }

    fn rpkg_src(
        repo: &str,
        version: &str,
        srcname: Option<&str>,
        binnames: &[&str],
    ) -> repology::Package {
        let srcname = srcname
            .map(|s| format!(r#","srcname":"{s}""#))
            .unwrap_or_default();
        let binnames = if binnames.is_empty() {
            String::new()
        } else {
            let list: Vec<String> = binnames.iter().map(|b| format!(r#""{b}""#)).collect();
            format!(r#","binnames":[{}]"#, list.join(","))
        };
        serde_json::from_str(&format!(
            r#"{{"repo":"{repo}","version":"{version}","status":"outdated"{srcname}{binnames}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn epel_is_stock_too_and_the_newer_of_the_two_wins() {
        // wprof: CentOS Stream 10 has nothing, EPEL 10 has 0.5-3.el10_3.
        let pkgs = vec![serde_json::from_str::<repology::Package>(
            r#"{"repo":"epel_10","srcname":"wprof","version":"0.5","origversion":"0.5-3.el10_3","status":"outdated"}"#,
        )
        .unwrap()];
        let stock = stock_version(&pkgs, 10, true, "wprof").unwrap();
        assert_eq!(
            (stock.version.as_str(), stock.release.as_deref()),
            ("0.5", Some("3.el10_3"))
        );
        assert!(
            stock_version(&pkgs, 9, true, "wprof").is_none(),
            "EPEL 9 has none"
        );
        // Both carry it: the newer one is stock.
        let pkgs = vec![
            rpkg_src("centos_stream_9", "1.0", Some("x"), &[]),
            rpkg_src("epel_9", "1.2", Some("x"), &[]),
            rpkg_src("almalinux_9", "1.1", Some("x"), &[]),
        ];
        assert_eq!(stock_version(&pkgs, 9, true, "x").unwrap().version, "1.2");
        assert_eq!(stock_version(&pkgs, 9, false, "x").unwrap().version, "1.2");
        let pkgs = vec![
            rpkg_src("centos_stream_9", "2.0", Some("x"), &[]),
            rpkg_src("epel_9", "1.2", Some("x"), &[]),
        ];
        assert_eq!(stock_version(&pkgs, 9, true, "x").unwrap().version, "2.0");
    }

    #[test]
    fn stock_version_is_what_ships_the_package_by_name() {
        // CentOS Stream 9 ships autoconf 2.69 and autoconf-latest 2.71
        // (binaries autoconf-latest, autoconf271); Repology files both
        // under the autoconf project. Neither the source nor a binary of
        // the latter is named autoconf, so it does not stand in.
        let pkgs = vec![
            rpkg_src("centos_stream_9", "2.69", Some("autoconf"), &["autoconf"]),
            rpkg_src(
                "centos_stream_9",
                "2.71",
                Some("autoconf-latest"),
                &["autoconf-latest", "autoconf271"],
            ),
        ];
        assert_eq!(
            stock_version(&pkgs, 9, true, "autoconf")
                .map(|s| s.version)
                .as_deref(),
            Some("2.69")
        );
        assert_eq!(
            stock_version(&pkgs, 9, true, "autoconf-latest")
                .map(|s| s.version)
                .as_deref(),
            Some("2.71")
        );
        assert!(stock_version(&pkgs, 9, true, "other").is_none());
        // A source of another name that ships the binary does stand in:
        // the consumer needs the binary, whoever builds it.
        let renamed = vec![rpkg_src(
            "centos_stream_9",
            "2.71",
            Some("autoconf-latest"),
            &["autoconf", "autoconf271"],
        )];
        assert_eq!(
            stock_version(&renamed, 9, true, "autoconf")
                .map(|s| s.version)
                .as_deref(),
            Some("2.71")
        );
        // An entry without any names cannot be told apart and counts.
        let bare = vec![rpkg("centos_stream_9", "1.0")];
        assert_eq!(
            stock_version(&bare, 9, true, "anything")
                .map(|s| s.version)
                .as_deref(),
            Some("1.0")
        );
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
        assert_eq!(
            stock_version(&pkgs, 9, true, "socat")
                .map(|s| s.version)
                .as_deref(),
            Some("1.7.4.1")
        );
        assert_eq!(
            stock_version(&pkgs, 9, false, "socat")
                .map(|s| s.version)
                .as_deref(),
            Some("1.7.0")
        );
    }

    #[test]
    fn same_version_newer_release_is_ahead_by_release_alone() {
        // gdal: stock c9s has 3.10.3-3.el9, the SIG 3.10.3-103.hs.el9.
        let tag_builds = vec![(
            "hyperscale9s-packages-main-release".to_string(),
            vec![build(7000, "gdal-3.10.3-103.hs.el9")],
        )];
        let stock = vec![serde_json::from_str::<repology::Package>(
            r#"{"repo":"centos_stream_9","srcname":"gdal","version":"3.10.3","origversion":"3.10.3-3.el9","status":"outdated"}"#,
        )
        .unwrap()];
        let plan = build_plan("gdal", &tag_builds, &stock);
        assert_eq!(plan.total_untag(), 0);
        assert_eq!(plan.total_version_ahead(), 0);
        assert_eq!(plan.total_release_ahead(), 1);
        assert_eq!(plan.total_ahead(), 1, "blocks like a version ahead");
        assert_eq!(plan.tags[0].stock_release.as_deref(), Some("3.el9"));
        let md = render_plan(&plan);
        assert!(md.contains("[stock 3.10.3-3.el9]"), "{md}");
        assert!(
            md.contains("newer release:    gdal-3.10.3-103.hs.el9"),
            "{md}"
        );
        assert!(md.contains("1 ahead of stock (1 by release alone)"), "{md}");
        // An epoch in origversion does not confuse the release parse, and
        // the same release is not ahead.
        let stock = vec![serde_json::from_str::<repology::Package>(
            r#"{"repo":"centos_stream_9","srcname":"gdal","version":"3.10.3","origversion":"1:3.10.3-103.hs.el9","status":"outdated"}"#,
        )
        .unwrap()];
        let plan = build_plan("gdal", &tag_builds, &stock);
        assert_eq!(plan.total_untag(), 1);
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
