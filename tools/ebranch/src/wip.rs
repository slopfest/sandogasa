// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Tracking packages on their way into the distro
//! (`check-wip <ledger>`).
//!
//! A coordinated packaging effort — a stack of new crates, a version
//! bump that drags its dependencies along — moves through the same
//! sequence for every package: staged somewhere, reviewed or
//! submitted as a pull request, built for Rawhide, branched, built
//! again, shipped in an update. Which packages are where is the
//! question that decides what to do next, and answering it by hand
//! across Bugzilla, dist-git, Koji and Bodhi is the tedious part.
//!
//! The effort is tracked in a **ledger**, a TOML file that is the
//! source of truth for which packages belong to it. Not the COPR
//! they are staged in: a COPR shrinks as packages graduate out of
//! it, so a report rebuilt from one each run would lose exactly the
//! work that is finished. The ledger also holds what no service can
//! tell us — whether a package is meant to ship at all, and which
//! review bug or pull request is landing it.
//!
//! Each run reconciles rather than rebuilds. A package in the COPR
//! and not the ledger is new work; one in both has its observations
//! refreshed; one in the ledger and no longer in the COPR keeps its
//! entry and stops being called staged, because it has finished or
//! moved on. Dropping it needs `--prune`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// How a package is getting from staging into the distro.
///
/// Every package is on exactly one route, which is why this is not a
/// review bug with exceptions: an existing package being updated is
/// not a review with no bug, it is a different route entirely.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Route {
    /// Nobody has said yet.
    #[default]
    Unknown,
    /// A new package, tracked by its Review Request bug.
    Review,
    /// An update to an existing package, tracked by its dist-git
    /// pull request.
    PullRequest,
    /// A package we own: pushed and built, with nothing to track.
    Direct,
}

impl Route {
    pub fn as_str(self) -> &'static str {
        match self {
            Route::Unknown => "route not decided",
            Route::Review => "package review",
            Route::PullRequest => "pull request",
            Route::Direct => "direct",
        }
    }
}

/// What a COPR most recently said about a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Staged {
    /// The project it was seen in, as `owner/project`.
    pub copr: String,
    /// Version-release of the latest build there.
    pub version: String,
    /// Chroots whose latest build did not succeed, so a report can
    /// say "staged, but not building" rather than calling it ready.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_chroots: Vec<String>,
    /// When this was last observed, `YYYY-MM-DD`. Recorded per
    /// observation so an offline report can date what it shows
    /// instead of presenting a week-old reading as current.
    pub seen: String,
}

/// What dist-git says about a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistGit {
    /// Whether the package has a dist-git repository at all. `false`
    /// means the review has not been imported yet — a real answer,
    /// distinct from not having looked.
    pub exists: bool,
    /// Branches the repository has, when it exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branches: Vec<String>,
    /// When this was last observed, `YYYY-MM-DD`.
    pub seen: String,
}

/// The version a branch already carries, so the report can compare
/// it against what is staged rather than only saying the package
/// exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shipped {
    /// Version-release in that branch's repositories.
    pub version: String,
    /// When this was last observed, `YYYY-MM-DD`.
    pub seen: String,
}

/// One package in the effort.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    #[serde(default)]
    pub route: Route,
    /// Review Request bug, when the route is a review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_bug: Option<u64>,
    /// dist-git pull request, when the route is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<u64>,
    /// Currently staged in a COPR, and what it says. Absent once the
    /// package has left every COPR the ledger knows about — the
    /// entry stays, only this goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged: Option<Staged>,
    /// What dist-git last said. Absent means nobody has looked, which
    /// is not the same as the package not being there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distgit: Option<DistGit>,
    /// What each branch already ships, keyed by branch. Absent for a
    /// branch means the package is not in it — or that the query
    /// failed, which is why a failure records nothing rather than an
    /// empty answer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shipped: BTreeMap<String, Shipped>,
}

/// The effort: which COPRs stage it, which releases it targets, and
/// where each package stands.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    /// Schema version, so a future shape can be recognized rather
    /// than misread.
    #[serde(default = "schema_version")]
    pub schema: u32,
    /// COPRs staging this effort, as `owner/project`. Recorded so
    /// later runs need no `--copr`.
    #[serde(default)]
    pub coprs: Vec<String>,
    /// Releases this effort is for. Empty means Rawhide only, which
    /// is where every package starts.
    #[serde(default)]
    pub targets: Vec<String>,
    #[serde(default)]
    pub packages: BTreeMap<String, Package>,
}

/// Whether a branch is one the effort targets, for deciding which of
/// a package's many branches are worth printing.
fn ledger_target(ledger: &Ledger, branch: &str) -> bool {
    ledger.targets.iter().any(|t| t == branch)
}

fn schema_version() -> u32 {
    1
}

/// What a reconcile changed, for reporting it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub added: Vec<String>,
    pub refreshed: Vec<String>,
    /// No longer staged anywhere, but kept.
    pub departed: Vec<String>,
}

impl Ledger {
    /// Read a ledger, or start an empty one when the file does not
    /// exist yet — the first run creates it.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                schema: schema_version(),
                ..Self::default()
            }),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Fold a COPR's package list into the ledger.
    ///
    /// `staged` is what the COPR reports now, as
    /// `(package, version, failed chroots)`. Packages the ledger has
    /// but this COPR no longer lists lose their `staged` record and
    /// keep everything else; nothing is removed.
    pub fn reconcile(
        &mut self,
        copr: &str,
        staged: &[(String, String, Vec<String>)],
        today: &str,
    ) -> Changes {
        let mut changes = Changes::default();
        let seen: BTreeSet<&str> = staged.iter().map(|(name, _, _)| name.as_str()).collect();

        for (name, version, failed_chroots) in staged {
            let record = Staged {
                copr: copr.to_string(),
                version: version.clone(),
                failed_chroots: failed_chroots.clone(),
                seen: today.to_string(),
            };
            match self.packages.get_mut(name) {
                Some(existing) => {
                    existing.staged = Some(record);
                    changes.refreshed.push(name.clone());
                }
                None => {
                    self.packages.insert(
                        name.clone(),
                        Package {
                            staged: Some(record),
                            ..Package::default()
                        },
                    );
                    changes.added.push(name.clone());
                }
            }
        }

        // Anything this COPR used to stage and no longer does has
        // graduated or moved on. Keep the entry: losing it is the
        // failure the ledger exists to prevent.
        for (name, package) in self.packages.iter_mut() {
            if package.staged.as_ref().is_some_and(|s| s.copr == copr)
                && !seen.contains(name.as_str())
            {
                package.staged = None;
                changes.departed.push(name.clone());
            }
        }

        if !self.coprs.iter().any(|c| c == copr) {
            self.coprs.push(copr.to_string());
        }
        changes
    }

    /// Forget packages that are no longer staged anywhere. Only
    /// under `--prune`: dropping an entry silently would lose the
    /// record of finished work.
    pub fn prune(&mut self) -> Vec<String> {
        let dropped: Vec<String> = self
            .packages
            .iter()
            .filter(|(_, p)| p.staged.is_none())
            .map(|(name, _)| name.clone())
            .collect();
        self.packages.retain(|_, p| p.staged.is_some());
        dropped
    }
}

/// What `check-wip` was asked to do.
pub struct Options {
    pub ledger: std::path::PathBuf,
    /// COPRs to fold in, on top of the ones the ledger records.
    pub coprs: Vec<String>,
    pub targets: Vec<String>,
    pub offline: bool,
    pub prune: bool,
    pub json: bool,
}

/// Report the effort, refreshing from its COPRs first unless
/// `--offline`.
///
/// Refreshing writes the ledger back. That is deliberate on what
/// reads like a read-only command: the observations were expensive to
/// gather and they are facts, not decisions, so discarding them would
/// only make the next run pay again. Decisions — which route a
/// package takes, which bug is landing it — are never inferred here.
pub fn run(opts: &Options) -> Result<(), String> {
    let mut ledger = Ledger::load(&opts.ledger)?;
    for target in &opts.targets {
        if !ledger.targets.iter().any(|t| t == target) {
            ledger.targets.push(target.clone());
        }
    }

    let mut dirty = !opts.targets.is_empty();
    if !opts.offline {
        let coprs: Vec<String> = ledger
            .coprs
            .iter()
            .chain(opts.coprs.iter())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if coprs.is_empty() {
            eprintln!(
                "note: no COPR recorded in {}; pass --copr to seed it",
                opts.ledger.display()
            );
        }
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        for spec in &coprs {
            match staged_in_copr(spec) {
                Ok(staged) => {
                    let changes = ledger.reconcile(spec, &staged, &today);
                    report_changes(spec, &changes);
                }
                // One unreachable COPR must not lose the rest of the
                // report: the ledger still knows what it knew.
                Err(e) => eprintln!("warning: {spec}: {e}; keeping what the ledger has"),
            }
        }
        refresh_distgit(&mut ledger, &today);
        // Rawhide is where everything lands first, so it is always
        // worth comparing against even when the effort targets
        // branches as well.
        let mut branches = vec!["rawhide".to_string()];
        branches.extend(ledger.targets.iter().filter(|t| *t != "rawhide").cloned());
        refresh_shipped(&mut ledger, &branches, &today);
        dirty = true;
    }

    if opts.prune {
        let dropped = ledger.prune();
        if !dropped.is_empty() {
            eprintln!(
                "pruned {} package(s): {}",
                dropped.len(),
                dropped.join(", ")
            );
            dirty = true;
        }
    }

    if dirty {
        ledger.save(&opts.ledger)?;
    }

    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ledger).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", render(&ledger));
    }
    Ok(())
}

/// Ask each targeted branch what version it already ships, in one
/// batched query per branch.
///
/// This is what makes the report answer "has the staged work landed
/// yet" rather than only "does the package exist". A package absent
/// from a branch simply has no entry for it; the whole query failing
/// records nothing at all, leaving the previous reading in place.
fn refresh_shipped(ledger: &mut Ledger, branches: &[String], today: &str) {
    let names: Vec<String> = ledger.packages.keys().cloned().collect();
    if names.is_empty() {
        return;
    }
    for branch in branches {
        let fedrq = sandogasa_fedrq::Fedrq {
            branch: Some(branch.clone()),
            repo: None,
        };
        let nvrs = match fedrq.src_nvrs(&names) {
            Ok(nvrs) => nvrs,
            Err(e) => {
                eprintln!("warning: {branch}: {e}; keeping what the ledger has");
                continue;
            }
        };
        let found: BTreeMap<&str, String> = nvrs
            .iter()
            .filter_map(|nvr| {
                sandogasa_koji::parse_nvr(nvr).map(|(n, v, r)| (n, format!("{v}-{r}")))
            })
            .collect();
        for (name, package) in ledger.packages.iter_mut() {
            match found.get(name.as_str()) {
                Some(version) => {
                    package.shipped.insert(
                        branch.clone(),
                        Shipped {
                            version: version.clone(),
                            seen: today.to_string(),
                        },
                    );
                }
                // Not in this branch: drop any stale record, since
                // the query succeeded and simply did not find it.
                None => {
                    package.shipped.remove(branch);
                }
            }
        }
    }
}

/// Ask dist-git about every tracked package: whether it has a
/// repository yet, and which branches it has.
///
/// A package with no repository has not been imported — for a new
/// package that means the review has not finished. That is a real
/// answer and is recorded as one; a lookup that fails is not, and
/// leaves whatever the ledger already knew, dated as before.
fn refresh_distgit(ledger: &mut Ledger, today: &str) {
    let names: Vec<String> = ledger.packages.keys().cloned().collect();
    if names.is_empty() {
        return;
    }
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("warning: dist-git lookups skipped ({e})");
            return;
        }
    };
    let client = sandogasa_distgit::DistGitClient::new();
    let mut failed = 0usize;
    rt.block_on(async {
        for name in &names {
            match client.project_branches(name).await {
                Ok(branches) => {
                    let record = DistGit {
                        exists: branches.is_some(),
                        branches: branches.unwrap_or_default(),
                        seen: today.to_string(),
                    };
                    if let Some(package) = ledger.packages.get_mut(name) {
                        package.distgit = Some(record);
                    }
                }
                Err(_) => failed += 1,
            }
        }
    });
    if failed > 0 {
        eprintln!(
            "warning: {failed} dist-git lookup(s) failed; \
             those packages keep what the ledger had"
        );
    }
}

/// Read a COPR's packages as `(name, version-release, failed
/// chroots)`.
fn staged_in_copr(spec: &str) -> Result<Vec<(String, String, Vec<String>)>, String> {
    let (owner, project) = parse_copr(spec).ok_or_else(|| format!("not a COPR spec: {spec}"))?;
    let packages = sandogasa_copr::Copr::new()
        .monitor(&owner, &project)
        .map_err(|e| e.to_string())?;
    Ok(packages
        .into_iter()
        .map(|p| {
            // The version is whatever the chroots agree on; take the
            // first that reports one, since a package staged for
            // several chroots is the same build.
            let version = p
                .chroots
                .values()
                .find_map(|c| c.pkg_version.clone())
                .unwrap_or_else(|| "unknown".to_string());
            let failed: Vec<String> = p
                .chroots
                .iter()
                .filter(|(_, c)| !c.state.is_empty() && c.state != "succeeded")
                .map(|(chroot, _)| chroot.clone())
                .collect();
            (p.name, version, failed)
        })
        .collect())
}

/// Accept `owner/project`, `@group/project`, or a project URL.
fn parse_copr(spec: &str) -> Option<(String, String)> {
    if let Some(rest) = spec
        .strip_prefix("https://")
        .or_else(|| spec.strip_prefix("http://"))
    {
        return crate::check_update::parse_copr_url_path(rest);
    }
    crate::check_update::parse_copr_spec(spec)
}

fn report_changes(spec: &str, changes: &Changes) {
    if !changes.added.is_empty() {
        eprintln!(
            "{spec}: {} new package(s): {}",
            changes.added.len(),
            changes.added.join(", ")
        );
    }
    if !changes.departed.is_empty() {
        eprintln!(
            "{spec}: {} package(s) no longer staged: {}",
            changes.departed.len(),
            changes.departed.join(", ")
        );
    }
}

/// Where a package has reached, as the report's heading for it.
///
/// Ordered by how far along the package is, and each heading names
/// the thing standing in the way rather than the state alone — the
/// question being answered is what to do next.
fn state(package: &Package, targets: &[String]) -> &'static str {
    let missing_branch = |d: &DistGit| targets.iter().any(|t| !d.branches.iter().any(|b| b == t));
    match (&package.distgit, &package.staged) {
        // Nothing looked up yet: say so rather than implying absence.
        (None, Some(s)) if !s.failed_chroots.is_empty() => "staged, not building",
        (None, Some(_)) => "staged, dist-git not checked",
        (None, None) => "not checked",
        (Some(d), staged) => {
            if !d.exists {
                return match staged {
                    Some(s) if !s.failed_chroots.is_empty() => "staged, not building",
                    Some(_) => "staged, not yet in dist-git",
                    None => "not in dist-git",
                };
            }
            if missing_branch(d) {
                return "in dist-git, needs branching";
            }
            match (package.shipped.get("rawhide"), staged) {
                // The staged build is newer than what Rawhide has,
                // so the work has not landed there yet.
                (Some(shipped), Some(s))
                    if sandogasa_rpmvercmp::rpmvercmp(&s.version, &shipped.version)
                        == std::cmp::Ordering::Greater =>
                {
                    "in dist-git, newer build staged"
                }
                (Some(_), _) => "in rawhide",
                (None, _) => "in dist-git, not built for rawhide",
            }
        }
    }
}

/// Render the report: what is staged, what needs a decision, and
/// what has left staging.
///
/// Every line dates what it shows, so a report served from the
/// ledger without contacting anything is honest about its age rather
/// than presenting an old reading as current.
pub fn render(ledger: &Ledger) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if ledger.packages.is_empty() {
        return "No packages tracked yet. Pass --copr to seed the ledger.\n".to_string();
    }

    let _ = writeln!(
        out,
        "{} package(s) tracked in {}",
        ledger.packages.len(),
        if ledger.coprs.is_empty() {
            "no COPR".to_string()
        } else {
            ledger.coprs.join(", ")
        }
    );
    let targets = if ledger.targets.is_empty() {
        "rawhide".to_string()
    } else {
        ledger.targets.join(", ")
    };
    let _ = writeln!(out, "targets: {targets}");

    let mut group: BTreeMap<&str, Vec<(&String, &Package)>> = BTreeMap::new();
    for (name, package) in &ledger.packages {
        group
            .entry(state(package, &ledger.targets))
            .or_default()
            .push((name, package));
    }

    for (heading, packages) in &group {
        let _ = writeln!(out, "\n{heading} ({})", packages.len());
        for (name, package) in packages {
            match &package.staged {
                Some(s) => {
                    let _ = writeln!(out, "  {name} {} (as of {})", s.version, s.seen);
                    if !s.failed_chroots.is_empty() {
                        let _ = writeln!(out, "    failed in {}", s.failed_chroots.join(", "));
                    }
                }
                None => {
                    let _ = writeln!(out, "  {name}");
                }
            }
            if let Some(d) = &package.distgit {
                if d.exists {
                    let shown: Vec<&str> = d
                        .branches
                        .iter()
                        .filter(|b| *b == "rawhide" || ledger_target(ledger, b))
                        .map(String::as_str)
                        .collect();
                    let _ = writeln!(
                        out,
                        "    dist-git: {} (as of {})",
                        if shown.is_empty() {
                            format!("{} branch(es)", d.branches.len())
                        } else {
                            shown.join(", ")
                        },
                        d.seen
                    );
                } else {
                    let _ = writeln!(out, "    dist-git: no repository (as of {})", d.seen);
                }
            }
            for (branch, shipped) in &package.shipped {
                let _ = writeln!(
                    out,
                    "    {branch}: {} (as of {})",
                    shipped.version, shipped.seen
                );
            }
            if package.route != Route::Unknown {
                let via = match (package.review_bug, package.pull_request) {
                    (Some(bug), _) => format!("{} rhbz#{bug}", package.route.as_str()),
                    (_, Some(pr)) => format!("{} !{pr}", package.route.as_str()),
                    _ => package.route.as_str().to_string(),
                };
                let _ = writeln!(out, "    via {via}");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn staged(name: &str, version: &str) -> (String, String, Vec<String>) {
        (name.to_string(), version.to_string(), Vec::new())
    }

    #[test]
    fn reconcile_adds_new_packages_and_records_the_copr() {
        let mut ledger = Ledger::default();
        let changes = ledger.reconcile(
            "@rust/uutils",
            &[
                staged("rust-fundu", "2.1.1-1"),
                staged("rust-uucore", "0.7.0-1"),
            ],
            "2026-08-10",
        );
        assert_eq!(changes.added, vec!["rust-fundu", "rust-uucore"]);
        assert!(changes.departed.is_empty());
        assert_eq!(ledger.coprs, vec!["@rust/uutils"]);
        // A new entry has no route yet — that is a decision, not an
        // observation, so it cannot be inferred from the COPR.
        assert_eq!(ledger.packages["rust-fundu"].route, Route::Unknown);
    }

    #[test]
    fn reconcile_keeps_a_package_that_left_the_copr() {
        let mut ledger = Ledger::default();
        ledger.reconcile(
            "@rust/uutils",
            &[staged("rust-fundu", "2.1.1-1")],
            "2026-08-10",
        );
        // It graduated: built, branched, shipped, removed from the
        // COPR. The entry has to survive that, which is the whole
        // reason the ledger exists.
        let changes = ledger.reconcile("@rust/uutils", &[], "2026-08-11");
        assert_eq!(changes.departed, vec!["rust-fundu"]);
        assert!(ledger.packages.contains_key("rust-fundu"));
        assert!(ledger.packages["rust-fundu"].staged.is_none());
    }

    #[test]
    fn reconcile_refreshes_an_existing_package() {
        let mut ledger = Ledger::default();
        ledger.reconcile(
            "@rust/uutils",
            &[staged("rust-fundu", "2.0.1-5")],
            "2026-08-10",
        );
        let changes = ledger.reconcile(
            "@rust/uutils",
            &[staged("rust-fundu", "2.1.1-1")],
            "2026-08-11",
        );
        assert_eq!(changes.refreshed, vec!["rust-fundu"]);
        assert!(changes.added.is_empty());
        let s = ledger.packages["rust-fundu"].staged.as_ref().unwrap();
        assert_eq!(s.version, "2.1.1-1");
        assert_eq!(s.seen, "2026-08-11");
    }

    #[test]
    fn reconcile_does_not_unstage_another_coprs_package() {
        let mut ledger = Ledger::default();
        ledger.reconcile("@rust/one", &[staged("rust-a", "1-1")], "2026-08-10");
        ledger.reconcile("@rust/two", &[staged("rust-b", "1-1")], "2026-08-10");
        // Reconciling one COPR says nothing about the other's.
        let changes = ledger.reconcile("@rust/two", &[], "2026-08-11");
        assert_eq!(changes.departed, vec!["rust-b"]);
        assert!(ledger.packages["rust-a"].staged.is_some());
    }

    #[test]
    fn prune_drops_only_what_is_no_longer_staged() {
        let mut ledger = Ledger::default();
        ledger.reconcile(
            "@rust/uutils",
            &[staged("rust-a", "1-1"), staged("rust-b", "1-1")],
            "2026-08-10",
        );
        ledger.reconcile("@rust/uutils", &[staged("rust-a", "1-1")], "2026-08-11");
        assert_eq!(ledger.prune(), vec!["rust-b"]);
        assert!(ledger.packages.contains_key("rust-a"));
    }

    #[test]
    fn ledger_round_trips_through_toml() {
        let mut ledger = Ledger {
            schema: 1,
            targets: vec!["epel9".to_string()],
            ..Ledger::default()
        };
        ledger.reconcile(
            "@rust/uutils",
            &[staged("rust-fundu", "2.1.1-1")],
            "2026-08-10",
        );
        ledger.packages.get_mut("rust-fundu").unwrap().route = Route::Review;
        ledger.packages.get_mut("rust-fundu").unwrap().review_bug = Some(2498026);
        let text = toml::to_string_pretty(&ledger).unwrap();
        assert_eq!(toml::from_str::<Ledger>(&text).unwrap(), ledger);
        // Readable by a human editing it by hand.
        assert!(text.contains("route = \"review\""), "{text}");
    }

    #[test]
    fn load_starts_empty_when_the_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::load(&dir.path().join("absent.toml")).unwrap();
        assert!(ledger.packages.is_empty());
        assert_eq!(ledger.schema, 1);
    }

    #[test]
    fn render_dates_what_it_shows_and_groups_by_state() {
        let mut ledger = Ledger::default();
        ledger.reconcile(
            "@rust/uutils",
            &[
                staged("rust-a", "1-1"),
                (
                    "rust-b".to_string(),
                    "2-1".to_string(),
                    vec!["fedora-rawhide-x86_64".to_string()],
                ),
            ],
            "2026-08-10",
        );
        ledger.packages.get_mut("rust-a").unwrap().route = Route::Direct;
        let text = render(&ledger);
        // Nothing has been looked up beyond the COPR yet, and the
        // heading says so rather than implying the package is
        // absent from dist-git.
        assert!(text.contains("staged, dist-git not checked (1)"), "{text}");
        assert!(text.contains("staged, not building (1)"), "{text}");
        assert!(text.contains("rust-a 1-1 (as of 2026-08-10)"), "{text}");
        assert!(text.contains("failed in fedora-rawhide-x86_64"), "{text}");
        assert!(text.contains("via direct"), "{text}");
        assert!(text.contains("targets: rawhide"), "{text}");
    }

    fn distgit(exists: bool, branches: &[&str]) -> DistGit {
        DistGit {
            exists,
            branches: branches.iter().map(|b| b.to_string()).collect(),
            seen: "2026-08-10".to_string(),
        }
    }

    #[test]
    fn state_distinguishes_not_looked_from_not_there() {
        // The difference that matters: no dist-git record means
        // nobody asked, which is not the same as the package having
        // no repository.
        let mut package = Package {
            staged: Some(Staged {
                copr: "@rust/uutils".to_string(),
                version: "1-1".to_string(),
                failed_chroots: vec![],
                seen: "2026-08-10".to_string(),
            }),
            ..Package::default()
        };
        assert_eq!(state(&package, &[]), "staged, dist-git not checked");

        package.distgit = Some(distgit(false, &[]));
        assert_eq!(state(&package, &[]), "staged, not yet in dist-git");
    }

    #[test]
    fn state_reports_a_missing_target_branch() {
        let mut package = Package {
            distgit: Some(distgit(true, &["rawhide"])),
            ..Package::default()
        };
        // Built and current, so only the branching question is left.
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("1.0-1.fc46"));
        // Targeting Rawhide only, a rawhide branch is all it needs.
        assert_eq!(state(&package, &[]), "in rawhide");
        assert_eq!(state(&package, &["rawhide".to_string()]), "in rawhide");
        // Targeting epel9 as well, it is not branched yet — and that
        // is reported ahead of anything about the build, since it is
        // what blocks progress.
        let targets = vec!["rawhide".to_string(), "epel9".to_string()];
        assert_eq!(state(&package, &targets), "in dist-git, needs branching");
        package.distgit = Some(distgit(true, &["rawhide", "epel9"]));
        assert_eq!(state(&package, &targets), "in rawhide");
    }

    fn staged_at(version: &str) -> Staged {
        Staged {
            copr: "@rust/uutils".to_string(),
            version: version.to_string(),
            failed_chroots: vec![],
            seen: "2026-08-10".to_string(),
        }
    }

    fn shipped_at(version: &str) -> Shipped {
        Shipped {
            version: version.to_string(),
            seen: "2026-08-10".to_string(),
        }
    }

    #[test]
    fn state_compares_the_staged_build_against_rawhide() {
        let mut package = Package {
            distgit: Some(distgit(true, &["rawhide"])),
            staged: Some(staged_at("0.13.0-1")),
            ..Package::default()
        };
        // Rawhide is behind: the staged work has not landed.
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("0.12.0-7.fc45"));
        assert_eq!(state(&package, &[]), "in dist-git, newer build staged");

        // Rawhide has caught up — the release differs, but the
        // version does not, so nothing is outstanding.
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("0.13.0-1.fc46"));
        assert_eq!(state(&package, &[]), "in rawhide");
    }

    #[test]
    fn state_notices_a_package_never_built_for_rawhide() {
        // Imported but never built: dist-git has it, the repos do
        // not.
        let package = Package {
            distgit: Some(distgit(true, &["rawhide"])),
            staged: Some(staged_at("0.1.0-1")),
            ..Package::default()
        };
        assert_eq!(state(&package, &[]), "in dist-git, not built for rawhide");
    }

    #[test]
    fn render_shows_what_each_branch_ships() {
        let mut ledger = Ledger::default();
        ledger.reconcile(
            "@rust/uutils",
            &[staged("rust-a", "0.13.0-1")],
            "2026-08-10",
        );
        let package = ledger.packages.get_mut("rust-a").unwrap();
        package.distgit = Some(distgit(true, &["rawhide"]));
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("0.12.0-7.fc45"));
        let text = render(&ledger);
        // Both versions are visible, so the comparison the heading
        // makes can be checked by eye.
        assert!(text.contains("rust-a 0.13.0-1"), "{text}");
        assert!(
            text.contains("rawhide: 0.12.0-7.fc45 (as of 2026-08-10)"),
            "{text}"
        );
        assert!(
            text.contains("in dist-git, newer build staged (1)"),
            "{text}"
        );
    }

    #[test]
    fn render_shows_dist_git_state_with_its_date() {
        let mut ledger = Ledger::default();
        ledger.reconcile("@rust/uutils", &[staged("rust-a", "1-1")], "2026-08-10");
        ledger.packages.get_mut("rust-a").unwrap().distgit = Some(distgit(false, &[]));
        let text = render(&ledger);
        assert!(text.contains("staged, not yet in dist-git (1)"), "{text}");
        assert!(
            text.contains("dist-git: no repository (as of 2026-08-10)"),
            "{text}"
        );
    }

    #[test]
    fn render_says_what_to_do_with_an_empty_ledger() {
        let text = render(&Ledger::default());
        assert!(text.contains("--copr"), "{text}");
    }
}
