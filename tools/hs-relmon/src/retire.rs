// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `retire` and `check-stock` — what stock catching up means for a
//! Hyperscale package, and taking one out when the answer is
//! retirement.
//!
//! Some packages the SIG carries only until CentOS Stream catches up;
//! once it has, the SIG copy is noise. Others carry packaging changes
//! of the SIG's own — patches, configuration, subpackage layout — and
//! when stock moves the action is a *rebase*, never a retirement. The
//! two are indistinguishable from CBS and Repology: a SIG build behind
//! stock looks the same either way. So the manifest declares it:
//! `divergent = true` on a package means rebase, `false` means retire,
//! unset means nobody has said, and `check-stock <manifest>` reports
//! each caught-up package's verdict accordingly, after the packages
//! still ahead of stock and before those with no build at all.
//!
//! `retire <package>` untags a package's builds from every hyperscale
//! tag (candidate and all), removes it from the manifest, and archives
//! its GitLab repo — after one confirmation, or none with `--yes`. It
//! refuses a divergent package unless `--force`. Builds in the tags of
//! a release the SIG no longer tracks (`hyperscale8s-*`, CentOS Stream
//! 8 being EOL) stay tagged — there is no stock to compare against and
//! nothing to gain by rewriting history — while the manifest entry and
//! the repo are still retired. The gate on the
//! builds is `prune-archived`'s: each is compared against the stock
//! version for its tag's channel (CentOS Stream N for a Stream tag,
//! AlmaLinux N for a RHEL tag, either plus EPEL N, via Repology); a
//! build *ahead* of stock
//! is prompted for individually and never untagged under `--yes`, and
//! if any stays tagged the package is not retired — the manifest entry
//! and the repo remain, since the SIG is still its only source.

use std::collections::BTreeMap;

use sandogasa_cli::style;
use sandogasa_repology as repology;

use crate::cbs::{Build, Client};
use crate::manifest;
use crate::prune_archived::{self, ArchivedPlan, build_plan};
use crate::prune_tags::{EL_TOKENS, TagBuilds, TagIndex};

/// The SIG's dist-git namespace; a package's repo is `<group>/<name>`.
pub const DEFAULT_GITLAB_GROUP: &str = "https://gitlab.com/CentOS/Hyperscale/rpms";

/// Group builds by the hyperscale tags they carry — `(build, its
/// tags)` pairs in, one `TagBuilds` per hyperscale tag out, tags
/// sorted. Non-hyperscale tags are ignored.
pub fn group_by_hyperscale_tag(builds: Vec<(Build, Vec<String>)>) -> Vec<TagBuilds> {
    let mut by_tag: BTreeMap<String, Vec<Build>> = BTreeMap::new();
    for (build, tags) in builds {
        for tag in tags.into_iter().filter(|t| t.starts_with("hyperscale")) {
            by_tag.entry(tag).or_default().push(build.clone());
        }
    }
    by_tag.into_iter().collect()
}

/// Every hyperscale tag a package's completed builds are in, from
/// the package's build list — all stages, candidate included, which
/// the managed-tag enumeration `prune-*` uses leaves out. Linear in
/// the package's build count, which is fine for the small packages
/// retirement is about.
pub fn hyperscale_tag_builds(
    cbs: &Client,
    package: &str,
    verbose: bool,
) -> Result<Vec<TagBuilds>, Box<dyn std::error::Error>> {
    let Some(id) = cbs.get_package_id(package)? else {
        return Ok(Vec::new());
    };
    let builds = cbs.list_builds(id)?;
    if verbose {
        eprintln!(
            "[hs-relmon] {package}: {} completed build(s), reading their tags",
            builds.len()
        );
    }
    let mut with_tags = Vec::with_capacity(builds.len());
    for b in builds {
        let tags = cbs.list_tags(b.build_id)?;
        with_tags.push((b, tags));
    }
    Ok(group_by_hyperscale_tag(with_tags))
}

/// Whether a hyperscale tag belongs to a release the SIG still tracks
/// (its EL token is one of [`EL_TOKENS`]); `hyperscale8s-*` is not.
pub fn live_release(tag: &str) -> bool {
    tag.strip_prefix("hyperscale")
        .and_then(|rest| rest.split('-').next())
        .is_some_and(|token| EL_TOKENS.contains(&token))
}

/// What `retire` would do for one package.
#[derive(Debug)]
pub struct RetirePlan {
    /// Per-tag untag/ahead classification against stock, for the
    /// releases the SIG still tracks.
    pub builds: ArchivedPlan,
    /// Builds in the tags of releases no longer tracked (EOL): left
    /// as they are, `(tag, count)`.
    pub eol: Vec<(String, usize)>,
    /// Whether the manifest lists the package.
    pub in_manifest: bool,
    /// The manifest's `divergent` declaration (`None`: unset or not
    /// listed).
    pub divergent: Option<bool>,
    /// The package's GitLab repo.
    pub repo_url: String,
    /// Whether the repo is archived already (`None`: not looked up).
    pub repo_archived: Option<bool>,
}

impl RetirePlan {
    /// No build is newer than stock, so untagging loses nothing.
    pub fn eligible(&self) -> bool {
        self.builds.total_ahead() == 0
    }

    /// Declared to carry SIG changes: rebase, not retire.
    pub fn divergent(&self) -> bool {
        self.divergent == Some(true)
    }
}

/// Render a plan for review: the stock comparison, then the actions.
pub fn render_plan(plan: &RetirePlan) -> String {
    let package = &plan.builds.package;
    let mut out = String::new();
    for (tag, n) in &plan.eol {
        out.push_str(&format!(
            "{package}: {tag}: {n} build(s) stay tagged (release no longer tracked)\n"
        ));
    }
    if plan.builds.tags.is_empty() {
        out.push_str(&format!(
            "{package}: no hyperscale builds tagged{}\n",
            if plan.eol.is_empty() {
                ""
            } else {
                " for a tracked release"
            }
        ));
    } else {
        for tp in &plan.builds.tags {
            out.push_str(&format!(
                "{package}: {} [stock {}]\n",
                tp.tag,
                tp.stock_label()
            ));
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
    }
    let verdict = if plan.divergent() {
        "NOT retirable: declared divergent (carries SIG changes) — rebase instead, or --force"
    } else if plan.eligible() {
        "retirable"
    } else if plan.builds.total_version_ahead() == 0 {
        "NOT retirable: builds of stock's version but a newer release (a system running \
         them keeps them until stock's version moves)"
    } else {
        "NOT retirable: builds newer than stock"
    };
    out.push_str(&format!("{package}: {verdict}\n"));
    out.push_str(&format!(
        "    manifest: {}\n",
        if plan.in_manifest {
            "remove entry"
        } else {
            "not listed"
        }
    ));
    let repo = match plan.repo_archived {
        Some(true) => "already archived",
        Some(false) => "archive",
        None => "archive (status not checked)",
    };
    out.push_str(&format!("    {}: {repo}\n", plan.repo_url));
    out
}

/// Assemble the plan for one package: its hyperscale builds against
/// stock, its manifest entry, its repo. `fetch_repology` and
/// `repo_archived` are injected so tests can stub them.
pub fn plan_for_package<F, A>(
    cbs: &Client,
    package: &str,
    manifest: Option<&manifest::Manifest>,
    gitlab_group: &str,
    fetch_repology: F,
    repo_archived: A,
    verbose: bool,
) -> Result<RetirePlan, Box<dyn std::error::Error>>
where
    F: Fn(&str) -> Result<Vec<repology::Package>, Box<dyn std::error::Error>>,
    A: Fn(&str) -> Option<bool>,
{
    let (tag_builds, eol): (Vec<TagBuilds>, Vec<TagBuilds>) =
        hyperscale_tag_builds(cbs, package, verbose)?
            .into_iter()
            .partition(|(tag, _)| live_release(tag));
    let entry = manifest.and_then(|m| m.packages.iter().find(|p| p.name == package));
    let repology_name = entry
        .and_then(|e| e.repology_name.as_deref())
        .or(manifest.and_then(|m| m.defaults.repology_name.as_deref()))
        .unwrap_or(package);
    let repology_packages = if tag_builds.is_empty() {
        Vec::new()
    } else {
        fetch_repology(repology_name)?
    };
    let repo_url = format!("{}/{package}", gitlab_group.trim_end_matches('/'));
    Ok(RetirePlan {
        builds: build_plan(package, &tag_builds, &repology_packages),
        eol: eol.into_iter().map(|(t, b)| (t, b.len())).collect(),
        in_manifest: entry.is_some(),
        divergent: entry.and_then(|e| e.divergent),
        repo_archived: repo_archived(&repo_url),
        repo_url,
    })
}

/// The one question `retire` asks up front: what goes without further
/// ado, what will be asked about build by build, and what happens to
/// the manifest entry and the repo.
pub fn confirmation(plan: &RetirePlan) -> String {
    let package = &plan.builds.package;
    let ahead = plan.builds.total_ahead();
    format!(
        "retire {package}: untag {} build(s) at or behind stock{}, {}, {}?",
        plan.builds.total_untag(),
        if ahead > 0 {
            format!(" (then ask about {ahead} newer than stock, one by one)")
        } else {
            String::new()
        },
        if plan.in_manifest {
            "remove it from the manifest"
        } else {
            "leave the manifest (not listed)"
        },
        match plan.repo_archived {
            Some(true) => "leave the repo (archived)",
            _ => "archive the repo",
        },
    )
}

/// Outcome of retiring one package.
#[derive(Debug, Default)]
pub struct RetireOutcome {
    pub untagged: usize,
    /// Builds newer than stock left tagged — the package was not retired.
    pub left_tagged: usize,
    pub errors: usize,
    pub manifest_removed: bool,
    pub repo_archived: bool,
}

/// Retire one package per its plan. Without `assume_yes`, one
/// confirmation covers the whole retirement; builds ahead of stock
/// are then prompted for individually (default no), and under
/// `assume_yes` skipped with a warning. Any build left tagged stops
/// the retirement short: the untags done stand, but the manifest
/// entry and the repo stay.
pub fn apply_plan(
    plan: &RetirePlan,
    manifest_path: Option<&std::path::Path>,
    archive: impl Fn(&str) -> Result<(), Box<dyn std::error::Error>>,
    assume_yes: bool,
    verbose: bool,
) -> RetireOutcome {
    let package = &plan.builds.package;
    let mut out = RetireOutcome::default();
    if !assume_yes {
        let approved = sandogasa_cli::confirm(&confirmation(plan), false).unwrap_or(false);
        if !approved {
            return out;
        }
    }
    for tp in &plan.builds.tags {
        for nvr in &tp.untag {
            match prune_archived::do_untag(&tp.tag, nvr, verbose) {
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
            let approved = if assume_yes {
                eprintln!("leaving {nvr}: {why} (--yes never untags ahead-of-stock builds)");
                false
            } else {
                sandogasa_cli::confirm(&format!("{why}; untag anyway?"), false).unwrap_or(false)
            };
            if !approved {
                out.left_tagged += 1;
                continue;
            }
            match prune_archived::do_untag(&tp.tag, nvr, verbose) {
                Ok(()) => out.untagged += 1,
                Err(()) => out.errors += 1,
            }
        }
    }
    if out.left_tagged > 0 || out.errors > 0 {
        eprintln!(
            "{package}: {} build(s) still tagged; not retiring (manifest and repo untouched)",
            out.left_tagged + out.errors
        );
        return out;
    }
    if plan.in_manifest
        && let Some(path) = manifest_path
    {
        match manifest::remove_packages_from_file(path, std::slice::from_ref(package)) {
            Ok(removed) => {
                out.manifest_removed = !removed.is_empty();
                if out.manifest_removed {
                    eprintln!("removed {package} from {}", path.display());
                }
            }
            Err(e) => {
                eprintln!("error: removing {package} from {}: {e}", path.display());
                out.errors += 1;
            }
        }
    }
    if plan.repo_archived != Some(true) {
        match archive(&plan.repo_url) {
            Ok(()) => {
                out.repo_archived = true;
                eprintln!("archived {}", plan.repo_url);
            }
            Err(e) => {
                eprintln!("error: archiving {}: {e}", plan.repo_url);
                out.errors += 1;
            }
        }
    }
    out
}

/// One manifest package's standing against stock, for `check-stock`.
#[derive(Debug, PartialEq)]
pub enum Standing {
    /// Some build is newer than stock (count): the SIG is still ahead.
    Ahead(usize),
    /// Stock has the version, but some build (count) has a newer
    /// release: retiring leaves a system running it on the SIG build
    /// until stock's version moves.
    ReleaseAhead(usize),
    /// Stock has caught up (every build at or behind it) and the
    /// package is declared temporary: retire it.
    Retire,
    /// Stock has caught up and the package is declared divergent:
    /// rebase it onto stock.
    Rebase,
    /// Stock has caught up and nobody has declared which it is.
    Undeclared,
    /// No build in the managed `-release`/`-testing` tags of the
    /// repositories judged (`main` by default): gone from CBS, or
    /// carried in another repository.
    NoBuilds,
}

/// Classify a package's stock plan by its `divergent` declaration.
pub fn standing(plan: &ArchivedPlan, divergent: Option<bool>) -> Standing {
    match (plan.total_untag(), plan.total_ahead(), divergent) {
        (0, 0, _) => Standing::NoBuilds,
        (_, n, _) if n > 0 => match plan.total_version_ahead() {
            0 => Standing::ReleaseAhead(n),
            v => Standing::Ahead(v),
        },
        (_, _, Some(true)) => Standing::Rebase,
        (_, _, Some(false)) => Standing::Retire,
        (_, _, None) => Standing::Undeclared,
    }
}

/// A package's stock plan with its declaration, as `check-stock`
/// reports it.
#[derive(Debug)]
pub struct StockReport {
    pub plan: ArchivedPlan,
    pub divergent: Option<bool>,
}

impl StockReport {
    pub fn standing(&self) -> Standing {
        standing(&self.plan, self.divergent)
    }
}

/// A hyperscale tag without its boilerplate: `hyperscale10s-packages-
/// main-release` reads as `10s-main-release`.
pub fn compact_tag(tag: &str) -> String {
    tag.strip_prefix("hyperscale")
        .unwrap_or(tag)
        .replacen("-packages-", "-", 1)
}

/// The verdict word and its color, one per [`Standing`].
fn verdict(standing: &Standing) -> (&'static str, &'static str) {
    match standing {
        Standing::Ahead(_) => ("ahead", style::GREEN),
        Standing::ReleaseAhead(_) => ("release", style::BOLD),
        Standing::Retire => ("retire", style::RED),
        Standing::Rebase => ("rebase", style::YELLOW),
        Standing::Undeclared => ("declare", style::MAGENTA),
        Standing::NoBuilds => ("none", style::DIM),
    }
}

/// One line per package for the listing — verdict, package, then
/// what stock has and where — aligned so a column of verdicts scans:
///
/// ```text
/// ahead     dnsmasq                     2 build(s) newer than stock (10s-main-release 2.90; 9s-main-release 2.85)
/// retire    crun                        stock covers 2 build(s) (10s-main-testing 1.29.1; 9s-main-testing 1.29.1)
/// declare   pinentry                    stock covers 1 build(s) (10s-main-release 1.3.1)
/// none      sqlite                      no builds in the managed tags
/// ```
pub fn render_standing(report: &StockReport, color: bool) -> String {
    let plan = &report.plan;
    let stock: Vec<String> = plan
        .tags
        .iter()
        .map(|t| format!("{} {}", compact_tag(&t.tag), t.stock_label()))
        .collect();
    let stock = stock.join("; ");
    let standing = report.standing();
    let detail = match &standing {
        Standing::Ahead(n) => format!("{n} build(s) newer than stock ({stock})"),
        Standing::ReleaseAhead(n) => {
            format!("{n} build(s) of stock's version with a newer release ({stock})")
        }
        Standing::Retire | Standing::Rebase | Standing::Undeclared => {
            format!("stock covers {} build(s) ({stock})", plan.total_untag())
        }
        Standing::NoBuilds => "no builds in the managed tags".to_string(),
    };
    let (word, sgr) = verdict(&standing);
    // Pad before painting: escape codes have no width.
    format!(
        "{}  {:<26}  {detail}\n",
        style::paint(&format!("{word:<8}"), sgr, color),
        plan.package
    )
}

/// What the verdict words mean, for the foot of the listing.
pub fn legend(color: bool) -> String {
    let word = |s: &Standing| {
        let (w, sgr) = verdict(s);
        style::paint(w, sgr, color)
    };
    format!(
        "{}: the SIG is newer than stock  {}: stock has the version, the SIG's release is newer \
         (retiring leaves a system on the SIG build until stock's version moves: dnf does not \
         downgrade, a configuration manager told to upgrade would)  {}: stock has caught up, \
         the package is temporary  \
         {}: stock has caught up, the package carries SIG changes  \
         {}: stock has caught up — set `divergent = false` (retire) or `true` (rebase) on the \
         manifest entry  {}: no builds in the managed tags\n",
        word(&Standing::Ahead(0)),
        word(&Standing::ReleaseAhead(0)),
        word(&Standing::Retire),
        word(&Standing::Rebase),
        word(&Standing::Undeclared),
        word(&Standing::NoBuilds),
    )
}

/// Ask, for each undeclared caught-up package, which it is, and write
/// the answer to the manifest: `t` temporary (`divergent = false`,
/// retire when stock catches up), `d` divergent (`true`, rebase), Enter
/// or `s` to leave it undeclared; `q` stops asking. Returns how many
/// were declared. Only for a terminal: a piped run just lists.
pub fn declare_interactively(
    manifest_path: &std::path::Path,
    reports: &[StockReport],
) -> Result<usize, Box<dyn std::error::Error>> {
    use sandogasa_review::{Answer, Choice, Menu};
    const CHOICES: [Choice; 3] = [
        Choice::new('t', "temporary"),
        Choice::new('d', "divergent"),
        Choice::new('s', "skip"),
    ];
    let mut declared = 0;
    for report in reports
        .iter()
        .filter(|r| r.standing() == Standing::Undeclared)
    {
        let name = &report.plan.package;
        let answer = sandogasa_review::ask(
            &format!("{name}: stock has caught up — temporary (retire) or divergent (rebase)?"),
            &Menu {
                choices: &CHOICES,
                default: Some('s'),
                all: false,
                quit: true,
                default_arg: None,
            },
        )?;
        let divergent = match answer {
            Answer::Pick { key: 't', .. } => false,
            Answer::Pick { key: 'd', .. } => true,
            Answer::Quit => break,
            _ => continue,
        };
        if manifest::set_divergent_in_file(manifest_path, name, divergent)? {
            declared += 1;
            eprintln!(
                "{name}: divergent = {divergent} written to {}",
                manifest_path.display()
            );
        }
    }
    Ok(declared)
}

/// `check-stock`: every manifest package's standing against stock,
/// judged over the managed `-release`/`-testing` tags the index was
/// read from. Ahead of stock first (the SIG's live work), then
/// the caught-up verdicts — retire, rebase, undeclared — then the
/// packages with no build at all.
pub fn run_check_stock<F>(
    index: &TagIndex,
    manifest: &manifest::Manifest,
    skip: &[String],
    fetch_repology: F,
) -> Result<(Vec<StockReport>, usize), Box<dyn std::error::Error>>
where
    F: Fn(&str) -> Result<Vec<repology::Package>, Box<dyn std::error::Error>>,
{
    let mut reports = Vec::new();
    let mut failures = 0usize;
    for pkg in &manifest.packages {
        if skip.contains(&pkg.name) {
            continue;
        }
        let repology_name = pkg
            .repology_name
            .as_deref()
            .or(manifest.defaults.repology_name.as_deref())
            .unwrap_or(&pkg.name)
            .to_string();
        match prune_archived::plan_for_package(&pkg.name, &index.for_package(&pkg.name), |_| {
            fetch_repology(&repology_name)
        }) {
            Ok(plan) => reports.push(StockReport {
                plan,
                divergent: pkg.divergent,
            }),
            Err(e) => {
                eprintln!("{}: {e}", pkg.name);
                failures += 1;
            }
        }
    }
    let rank = |r: &StockReport| match r.standing() {
        Standing::Ahead(_) => 0,
        Standing::ReleaseAhead(_) => 1,
        Standing::Retire => 2,
        Standing::Rebase => 3,
        Standing::Undeclared => 4,
        Standing::NoBuilds => 5,
    };
    reports.sort_by_key(|r| (rank(r), r.plan.package.clone()));
    Ok((reports, failures))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prune_archived::ArchivedTagPlan;

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

    fn tag_plan(tag: &str, stock: Option<&str>, untag: &[&str], ahead: &[&str]) -> ArchivedTagPlan {
        ArchivedTagPlan {
            tag: tag.into(),
            stock: stock.map(String::from),
            stock_release: None,
            untag: untag.iter().map(|s| s.to_string()).collect(),
            ahead: ahead.iter().map(|s| s.to_string()).collect(),
            release_ahead: vec![],
        }
    }

    #[test]
    fn grouping_keeps_only_hyperscale_tags_and_sorts_them() {
        let grouped = group_by_hyperscale_tag(vec![
            (
                build(2, "crun-1.28-1.1.hs.el10"),
                vec![
                    "hyperscale10s-packages-main-testing".into(),
                    "hyperscale10s-packages-main-candidate".into(),
                ],
            ),
            (
                build(1, "crun-1.28-1.1.hs.el9"),
                vec![
                    "hyperscale9s-packages-main-candidate".into(),
                    "c9s-build".into(),
                ],
            ),
        ]);
        let tags: Vec<&str> = grouped.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            tags,
            [
                "hyperscale10s-packages-main-candidate",
                "hyperscale10s-packages-main-testing",
                "hyperscale9s-packages-main-candidate"
            ]
        );
        assert_eq!(grouped[2].1[0].nvr, "crun-1.28-1.1.hs.el9");
    }

    #[test]
    fn only_tracked_releases_are_live() {
        assert!(live_release("hyperscale9s-packages-main-release"));
        assert!(live_release("hyperscale10-packages-facebook-testing"));
        assert!(
            !live_release("hyperscale8s-packages-main-release"),
            "CentOS Stream 8 is EOL"
        );
        assert!(
            !live_release("isa10s-packages-riscv-release"),
            "not a hyperscale tag"
        );
    }

    #[test]
    fn eligible_only_when_nothing_is_ahead_of_stock() {
        let retirable = RetirePlan {
            builds: ArchivedPlan {
                package: "crun".into(),
                tags: vec![tag_plan(
                    "hyperscale9s-packages-main-testing",
                    Some("1.29.1"),
                    &["crun-1.28-1.1.hs.el9"],
                    &[],
                )],
            },
            eol: vec![],
            in_manifest: true,
            divergent: Some(false),
            repo_url: "https://gitlab.com/CentOS/Hyperscale/rpms/crun".into(),
            repo_archived: Some(false),
        };
        assert!(retirable.eligible() && !retirable.divergent());
        let md = render_plan(&retirable);
        assert!(md.contains("crun: retirable\n"), "{md}");
        assert!(
            md.contains("untag (<= stock): crun-1.28-1.1.hs.el9"),
            "{md}"
        );
        assert!(md.contains("manifest: remove entry"), "{md}");
        assert!(md.contains("rpms/crun: archive\n"), "{md}");

        let ahead = RetirePlan {
            builds: ArchivedPlan {
                package: "socat".into(),
                tags: vec![tag_plan(
                    "hyperscale9s-packages-main-release",
                    Some("1.7.4.1"),
                    &[],
                    &["socat-1.7.4.4-4.hs.el9"],
                )],
            },
            eol: vec![("hyperscale8s-packages-main-release".into(), 3)],
            in_manifest: false,
            divergent: None,
            repo_url: "https://gitlab.com/CentOS/Hyperscale/rpms/socat".into(),
            repo_archived: None,
        };
        assert!(!ahead.eligible());
        assert_eq!(
            confirmation(&ahead),
            "retire socat: untag 0 build(s) at or behind stock (then ask about 1 newer than \
             stock, one by one), leave the manifest (not listed), archive the repo?"
        );
        assert_eq!(
            confirmation(&retirable),
            "retire crun: untag 1 build(s) at or behind stock, remove it from the manifest, archive the repo?"
        );
        let md = render_plan(&ahead);
        assert!(
            md.contains(
                "hyperscale8s-packages-main-release: 3 build(s) stay tagged (release no longer tracked)"
            ),
            "{md}"
        );
        assert!(
            md.contains("NOT retirable: builds newer than stock"),
            "{md}"
        );
        assert!(md.contains("manifest: not listed"), "{md}");
        assert!(md.contains("archive (status not checked)"), "{md}");

        // Caught up, but declared to carry SIG changes: rebase.
        let divergent = RetirePlan {
            divergent: Some(true),
            ..RetirePlan {
                builds: ArchivedPlan {
                    package: "pykickstart".into(),
                    tags: vec![tag_plan(
                        "hyperscale10s-packages-main-release",
                        Some("3.52.13"),
                        &["p"],
                        &[],
                    )],
                },
                eol: vec![],
                in_manifest: true,
                divergent: None,
                repo_url: "https://gitlab.com/CentOS/Hyperscale/rpms/pykickstart".into(),
                repo_archived: Some(false),
            }
        };
        assert!(divergent.eligible() && divergent.divergent());
        assert!(render_plan(&divergent).contains("declared divergent"));
    }

    #[test]
    fn a_newer_release_of_stocks_version_stands_apart_from_ahead() {
        let mut tp = tag_plan(
            "hyperscale9s-packages-main-release",
            Some("3.10.3"),
            &[],
            &[],
        );
        tp.stock_release = Some("3.el9".into());
        tp.release_ahead = vec!["gdal-3.10.3-103.hs.el9".into()];
        let plan = ArchivedPlan {
            package: "gdal".into(),
            tags: vec![tp],
        };
        assert_eq!(standing(&plan, Some(false)), Standing::ReleaseAhead(1));
        let line = render_standing(
            &StockReport {
                plan,
                divergent: Some(false),
            },
            false,
        );
        assert!(
            line.starts_with("release   gdal")
                && line.contains("1 build(s) of stock's version with a newer release (9s-main-release 3.10.3-3.el9)"),
            "{line}"
        );
        assert!(legend(false).contains("dnf does not downgrade"));
    }

    #[test]
    fn standing_follows_stock_and_the_declaration() {
        let caught_up = ArchivedPlan {
            package: "crun".into(),
            tags: vec![
                tag_plan(
                    "hyperscale10s-packages-main-testing",
                    Some("1.29.1"),
                    &["a"],
                    &[],
                ),
                tag_plan(
                    "hyperscale9s-packages-main-testing",
                    Some("1.29.1"),
                    &["b"],
                    &[],
                ),
            ],
        };
        assert_eq!(standing(&caught_up, Some(false)), Standing::Retire);
        assert_eq!(standing(&caught_up, Some(true)), Standing::Rebase);
        assert_eq!(standing(&caught_up, None), Standing::Undeclared);
        let report = StockReport {
            plan: caught_up,
            divergent: Some(false),
        };
        assert_eq!(
            render_standing(&report, false),
            "retire    crun                        stock covers 2 build(s) (10s-main-testing 1.29.1; 9s-main-testing 1.29.1)\n"
        );
        // Painted, the verdict is padded before the escape codes so the
        // columns still line up.
        assert!(
            render_standing(&report, true)
                .starts_with("\x1b[31mretire  \x1b[0m  crun                        stock covers"),
            "{}",
            render_standing(&report, true)
        );
        let report = StockReport {
            divergent: None,
            ..report
        };
        assert!(render_standing(&report, false).starts_with("declare   crun"));
        let report = StockReport {
            divergent: Some(true),
            ..report
        };
        assert!(render_standing(&report, false).starts_with("rebase    crun"));
        assert!(legend(false).contains("set `divergent = false` (retire)"));
        assert_eq!(
            compact_tag("hyperscale10s-packages-main-release"),
            "10s-main-release"
        );
        assert_eq!(compact_tag("c9s-build"), "c9s-build");

        // Ahead of stock: the declaration does not matter yet.
        let ahead = ArchivedPlan {
            package: "socat".into(),
            tags: vec![tag_plan(
                "hyperscale9s-packages-main-release",
                None,
                &[],
                &["s"],
            )],
        };
        assert_eq!(standing(&ahead, Some(true)), Standing::Ahead(1));
        let line = render_standing(
            &StockReport {
                plan: ahead,
                divergent: None,
            },
            false,
        );
        assert!(
            line.starts_with("ahead     socat")
                && line.contains("1 build(s) newer than stock (9s-main-release (none in stock))"),
            "{line}"
        );
        let none = ArchivedPlan {
            package: "gone".into(),
            tags: vec![],
        };
        assert_eq!(standing(&none, Some(false)), Standing::NoBuilds);
    }
}
