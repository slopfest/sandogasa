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
    /// Whether Rawhide carries a `dead.package`. A retired package
    /// keeps its repository, so "the repo exists" says nothing about
    /// whether the package is alive — and a package retired long
    /// enough needs a fresh review before it can come back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retired: bool,
    /// What `dead.package` says, when retired. Usually the sentence
    /// that decides whether the package should come back at all —
    /// "replaced by uutils-coreutils" settles it. The retirement date
    /// would be the commit that added the file, which Pagure's API
    /// does not expose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_reason: Option<String>,
    /// The date retirement happened, `YYYY-MM-DD`, when it can be
    /// established.
    ///
    /// With no commit-log endpoint found on Pagure, this is the
    /// branch's HEAD date — and it is only recorded when that commit's
    /// subject
    /// matches `dead.package`, which is what makes it the retirement
    /// rather than merely the last thing that happened. Without the
    /// match the date is left out: "when the repo was last touched"
    /// would read as a retirement date and be later than the truth,
    /// which is the wrong direction to be wrong in when judging how
    /// long a package has been dead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_on: Option<String>,
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

/// What a package's Review Request bug says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Review {
    pub bug: u64,
    /// Bugzilla status: NEW, ASSIGNED, POST, CLOSED and so on.
    pub status: String,
    /// Whether the review carries `fedora-review+`, which is what
    /// approval actually is — a closed bug without it was abandoned,
    /// not accepted.
    pub approved: bool,
    /// The date the review was filed, `YYYY-MM-DD`. Context for
    /// judging whether a retired package's review is old enough that
    /// coming back needs a new one.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub filed: String,
    /// When this was last observed, `YYYY-MM-DD`.
    pub seen: String,
}

/// The latest build Koji has tagged for a branch.
///
/// Distinct from what the repositories ship: a build is tagged the
/// moment it succeeds, and only reaches repodata once a compose runs
/// (or, on a branched release, once an update is pushed). Without
/// this, "not in the repos" cannot tell a package that was never
/// built from one that is built and waiting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Built {
    /// Name-version-release of the latest tagged build.
    pub nvr: String,
    /// The Koji tag it was found in.
    pub tag: String,
    /// When this was last observed, `YYYY-MM-DD`.
    pub seen: String,
}

/// The Bodhi update carrying a build for a branch.
///
/// On a branched release a build reaches the repositories only via an
/// update, so "built but not shipped" has two very different causes:
/// an update is working its way through, or nobody has submitted one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRef {
    pub alias: String,
    /// `pending`, `testing`, `stable`, and so on.
    pub status: String,
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
    /// What the package's review request says, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<Review>,
    /// The latest build Koji has for each branch, keyed by branch.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub built: BTreeMap<String, Built>,
    /// The Bodhi update carrying each branch's build, keyed by
    /// branch. Rawhide has none — its builds need no update.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub update: BTreeMap<String, UpdateRef>,
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
    /// Koji side tags the effort builds into.
    ///
    /// A build made in a side tag is tagged there and nowhere else
    /// until an update carries it, so without these it is invisible —
    /// which is most of the window in which the next move is to submit
    /// an update. One query per side tag covers every package in it.
    #[serde(default)]
    pub side_tags: Vec<String>,
    #[serde(default)]
    pub packages: BTreeMap<String, Package>,
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
    /// Search for a review request again even where one is recorded,
    /// for a package whose retirement means a new one is needed.
    pub rescan_reviews: bool,
    /// Limit the per-package lookups and the report to these
    /// packages. Empty means all of them.
    pub packages: Vec<String>,
    /// Route assignments to record, as `package=route`. Applied
    /// without contacting anything.
    pub set: Vec<String>,
    /// Packages to track that no COPR staged.
    pub add: Vec<String>,
    /// Side tags to record on the ledger and scan for builds.
    pub side_tags: Vec<String>,
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
    // A package can belong to an effort without ever being staged in
    // a COPR — built straight into a side tag, or already updated
    // before the effort began. Adding it makes the ledger the record
    // of the effort rather than of one COPR's contents.
    for tag in &opts.side_tags {
        // Rejected rather than stored: a name that is not a side tag
        // can never be queried, and keeping it means warning about it
        // on every future run.
        if crate::check_update::branch_from_side_tag(tag).is_none() {
            return Err(format!(
                "{tag} is not a side tag name (<branch>-build-side-<id>)"
            ));
        }
        if !ledger.side_tags.iter().any(|t| t == tag) {
            ledger.side_tags.push(tag.clone());
            eprintln!("tracking side tag {tag}");
            dirty = true;
        }
        // Building into a side tag says that branch is a target, so
        // it is recorded as one rather than having to be given twice.
        // Recording it, rather than inferring it per run, keeps one
        // source of truth for what the report groups on.
        if let Some(branch) = crate::check_update::branch_from_side_tag(tag)
            && branch != "rawhide"
            && !ledger.targets.contains(&branch)
        {
            ledger.targets.push(branch.clone());
            eprintln!("targeting {branch}, from its side tag");
            dirty = true;
        }
    }
    // Drop anything unusable a previous run may have stored.
    let unusable: Vec<String> = ledger
        .side_tags
        .iter()
        .filter(|t| crate::check_update::branch_from_side_tag(t).is_none())
        .cloned()
        .collect();
    if !unusable.is_empty() {
        eprintln!("dropping unusable side tag(s): {}", unusable.join(", "));
        ledger
            .side_tags
            .retain(|t| crate::check_update::branch_from_side_tag(t).is_some());
        dirty = true;
    }

    for name in &opts.add {
        if ledger.packages.contains_key(name) {
            eprintln!("{name} is already tracked");
        } else {
            ledger.packages.insert(name.clone(), Package::default());
            eprintln!("added {name}");
            dirty = true;
        }
    }

    // Assignments are decisions, so they apply before any lookup and
    // need no network of their own.
    for spec in &opts.set {
        eprintln!("set {}", apply_assignment(&mut ledger, spec)?);
        dirty = true;
    }
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
        refresh_distgit(&mut ledger, &today, &opts.packages);
        // Rawhide is where everything lands first, so it is always
        // worth comparing against even when the effort targets
        // branches as well.
        let mut branches = vec!["rawhide".to_string()];
        branches.extend(ledger.targets.iter().filter(|t| *t != "rawhide").cloned());
        // Side-tag branches join the release lookup even though they
        // are not examined in their own right: it is what makes an
        // alias like f45 -> rawhide available on every run, not only on
        // the run whose --side-tag recorded the target.
        let mut lookup = branches.clone();
        for branch in ledger
            .side_tags
            .iter()
            .filter_map(|t| crate::check_update::branch_from_side_tag(t))
        {
            if !lookup.contains(&branch) {
                lookup.push(branch);
            }
        }
        let (mut tags, mut releases) = release_tags(&lookup);
        // Two targets resolving to one Bodhi release are one release
        // under two names, and every fact about it would be reported
        // twice. Fedora names rawhide's side tags after its version, so
        // a rawhide side tag records "f45" as a target — the same
        // release as rawhide, which wins because it is always examined.
        let mut kept: BTreeMap<&String, &String> = BTreeMap::new();
        let mut duplicates: Vec<(String, String, String)> = Vec::new();
        for branch in &lookup {
            let Some(release) = releases.get(branch) else {
                continue;
            };
            match kept.get(release) {
                Some(first) => duplicates.push((branch.clone(), (*first).clone(), release.clone())),
                None => {
                    kept.insert(release, branch);
                }
            }
        }
        let mut aliases: BTreeMap<String, String> = BTreeMap::new();
        for (dup, first, release) in duplicates {
            // Reported only when it was a target, because that is the
            // only case where something is being dropped. A duplicate
            // reached through a side tag's name is just an alias, and
            // saying so every run would be noise about nothing.
            if ledger.targets.contains(&dup) {
                eprintln!("dropping target {dup}: the same Bodhi release as {first} ({release})");
            }
            drop_target(&mut ledger, &dup);
            branches.retain(|b| *b != dup);
            tags.remove(&dup);
            releases.remove(&dup);
            // Kept as an alias rather than only dropped: a side tag
            // named for the duplicate — Fedora's rawhide side tags are
            // named "f45-build-side-*" — holds real builds, and
            // without the alias they belong to no examined branch and
            // go unnoticed.
            aliases.insert(dup, first);
        }
        // Only examined branches are asked about: a side tag's branch
        // that survived as its own name is a target too, and anything
        // else here would collect facts nothing reports on.
        tags.retain(|branch, _| branches.contains(branch));
        // Facts about a branch no longer examined would go on being
        // reported with nothing ever refreshing them — an untracked
        // branch reads exactly like a tracked one in the output.
        let mut stale: Vec<String> = ledger
            .packages
            .values()
            .flat_map(|p| {
                p.shipped
                    .keys()
                    .chain(p.built.keys())
                    .chain(p.update.keys())
                    .cloned()
            })
            .filter(|b| !branches.contains(b))
            .collect();
        stale.sort();
        stale.dedup();
        if !stale.is_empty() {
            eprintln!("forgetting untracked branch(es): {}", stale.join(", "));
            for branch in &stale {
                drop_target(&mut ledger, branch);
            }
        }
        refresh_shipped(&mut ledger, &branches, &today, &opts.packages);
        refresh_reviews_blocking(&mut ledger, &today, opts.rescan_reviews, &opts.packages);
        if sandogasa_koji::is_available() {
            refresh_built(&mut ledger, &tags, &aliases, &today, &opts.packages);
        } else {
            eprintln!("warning: koji is not installed; skipping build lookups");
        }
        refresh_updates_blocking(&mut ledger, &releases, &today, &opts.packages);
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

    for name in &opts.packages {
        if !ledger.packages.contains_key(name) {
            eprintln!(
                "warning: {name} is not tracked in {}",
                opts.ledger.display()
            );
        }
    }

    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ledger).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", render(&ledger, &opts.packages));
    }
    Ok(())
}

/// The date a package was retired, when it can be established.
///
/// No commit-log endpoint was found on Pagure, so a branch's HEAD is
/// the only commit reachable by hash. For a retired package that is
/// normally the retirement commit,
/// since nothing follows a retirement — and the check that it *is* is
/// its subject matching `dead.package`, which the retirement tooling
/// writes to both. Without that match the date is not claimed: it
/// would be the last time anyone touched the repo, later than the
/// retirement, and too optimistic for judging how long a package has
/// been dead.
async fn retirement_date(
    client: &sandogasa_distgit::DistGitClient,
    package: &str,
    reason: &str,
) -> Option<String> {
    let heads = client.branch_heads(package).await.ok()??;
    let head = heads.get("rawhide")?;
    let (when, subject) = client.commit_info(package, head).await.ok()??;
    if subject.trim() != reason.trim() {
        return None;
    }
    chrono::DateTime::from_timestamp(when, 0).map(|d| d.format("%Y-%m-%d").to_string())
}

/// Builds found in the effort's side tags, as
/// `branch -> package -> (nvr, tag)`.
///
/// One `list-tagged` per side tag rather than one per package: a side
/// tag holds only the effort's builds, so a single call answers for all
/// of them. The branch comes from the tag's name, the same derivation
/// `check-update` uses.
fn side_tag_builds(
    side_tags: &[String],
    aliases: &BTreeMap<String, String>,
) -> BTreeMap<String, BTreeMap<String, (String, String)>> {
    let mut found: BTreeMap<String, BTreeMap<String, (String, String)>> = BTreeMap::new();
    for tag in side_tags {
        let Some(mut branch) = crate::check_update::branch_from_side_tag(tag) else {
            eprintln!("warning: {tag} is not a side tag name; skipping it");
            continue;
        };
        // A branch known under two names is examined under one of
        // them, so the tag's builds are attributed there.
        if let Some(kept) = aliases.get(&branch) {
            branch = kept.clone();
        }
        // Side tags are standalone, so no inheritance is wanted here.
        match sandogasa_koji::list_tagged(tag, None, None) {
            Ok(builds) => {
                let per_branch = found.entry(branch).or_default();
                for build in builds {
                    if let Some((name, _, _)) = sandogasa_koji::parse_nvr(&build.nvr) {
                        per_branch.insert(name.to_string(), (build.nvr.clone(), tag.clone()));
                    }
                }
            }
            Err(e) => eprintln!("warning: koji {tag}: {e}"),
        }
    }
    found
}

/// Ask Koji for the latest build tagged for each branch.
///
/// Only packages whose staged version is ahead of what the branch
/// ships are asked about: for anything already current in the repos
/// the question is settled, and Koji is one subprocess per package.
/// The tag comes from Bodhi's release list (`stable_tag`), so no
/// release number is hardcoded and it follows Rawhide as it moves.
fn refresh_built(
    ledger: &mut Ledger,
    tags: &BTreeMap<String, Vec<String>>,
    aliases: &BTreeMap<String, String>,
    today: &str,
    only: &[String],
) {
    let from_side_tags = side_tag_builds(&ledger.side_tags, aliases);
    for (branch, branch_tags) in tags {
        let wanted: Vec<String> = ledger
            .packages
            .iter()
            .filter(|(name, p)| {
                selected(only, name)
                    // Ahead of the repositories, so whether a build
                    // exists is the open question — or nothing is
                    // known of a version at all, which is the case for
                    // a package added by hand with no COPR behind it.
                    // Skipping those would leave nothing to populate
                    // `built`, so nothing ever would.
                    && (ahead_of_repos(p, branch) || wanted_version(p, branch).is_none())
            })
            .map(|(name, _)| name.clone())
            .collect();
        for name in wanted {
            // Both the candidate and testing tags are asked, because
            // neither sees the other: candidate holds a build with no
            // update yet, testing holds one whose update is in
            // flight, and both inherit the stable tag. The newest
            // answer wins.
            //
            // No Koji profile is threaded through: the tags come from
            // Bodhi's release list, which knows only Fedora and EPEL,
            // so a non-default profile would have nothing to query.
            // A side tag build is a candidate like any other, and is
            // often the newest — it is where a build lives before an
            // update exists.
            let mut newest: Option<(String, String)> = from_side_tags
                .get(branch)
                .and_then(|per_branch| per_branch.get(&name))
                .cloned();
            let mut failed = false;
            for tag in branch_tags {
                match sandogasa_koji::latest_tagged(tag, &name, None) {
                    Ok(Some(build)) => {
                        let evr = sandogasa_koji::parse_nvr(&build.nvr)
                            .map(|(_, v, r)| format!("{v}-{r}"))
                            .unwrap_or_default();
                        let better = newest.as_ref().is_none_or(|(prev, _)| {
                            sandogasa_koji::parse_nvr(prev)
                                .map(|(_, v, r)| format!("{v}-{r}"))
                                .is_some_and(|p| {
                                    sandogasa_rpmvercmp::rpmvercmp(&evr, &p)
                                        == std::cmp::Ordering::Greater
                                })
                        });
                        if better {
                            newest = Some((build.nvr, build.tag));
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("warning: koji {tag} {name}: {e}");
                        failed = true;
                    }
                }
            }
            if failed {
                continue;
            }
            if let Some(package) = ledger.packages.get_mut(&name) {
                match newest {
                    Some((nvr, tag)) => {
                        package.built.insert(
                            branch.clone(),
                            Built {
                                nvr,
                                tag,
                                seen: today.to_string(),
                            },
                        );
                    }
                    // Nothing found in either tag. Not proof it was
                    // never built — a side tag build is in neither
                    // until an update exists — so the record goes and
                    // the report says only that none was found.
                    None => {
                        package.built.remove(branch);
                    }
                }
            }
        }
    }
}

/// Ask Bodhi which update carries each branch's build.
///
/// Asked for every branch, Rawhide included: a Rawhide build can be
/// carried by an update too — automatically, or from a side tag — and
/// Bodhi files those under the *release* name for whatever Rawhide
/// currently is, F45 rather than "rawhide". Only packages whose build
/// is ahead of what the branch ships are asked about, which is exactly
/// when "is an update in flight, or does one need submitting?" arises.
async fn refresh_updates(
    ledger: &mut Ledger,
    releases: &BTreeMap<String, String>,
    today: &str,
    only: &[String],
) {
    let client = sandogasa_bodhi::BodhiClient::new();
    for (branch, release) in releases {
        let wanted: Vec<String> = ledger
            .packages
            .iter()
            .filter(|(name, p)| selected(only, name) && ahead_of_repos(p, branch))
            .map(|(name, _)| name.clone())
            .collect();
        for name in wanted {
            // Newest first, so a superseded older update does not
            // mask the one in flight.
            let statuses = ["pending", "testing", "stable"];
            match client.updates_for_package(&name, release, &statuses).await {
                Ok(updates) => {
                    // The version of interest is what is staged, or
                    // once a package has left the COPR, what was built
                    // for this branch — otherwise a graduated package
                    // loses its update line exactly when you want to
                    // watch it reach stable.
                    let want = ledger
                        .packages
                        .get(&name)
                        .and_then(|p| wanted_version(p, branch));
                    // Only an update that carries the build in
                    // question counts. A package's newest update is
                    // often an old one — the uutils stack has a
                    // 120-package update from four months ago — and
                    // showing that beside "needs building" reads as
                    // though the work were done.
                    let carrying = updates.iter().find(|u| {
                        u.builds.iter().any(|b| {
                            sandogasa_koji::parse_nvr(&b.nvr).is_some_and(|(n, v, r)| {
                                n == name
                                    && want.as_deref().is_some_and(|want| {
                                        sandogasa_rpmvercmp::rpmvercmp(&format!("{v}-{r}"), want)
                                            != std::cmp::Ordering::Less
                                    })
                            })
                        })
                    });
                    let package = ledger.packages.get_mut(&name);
                    match (carrying, package) {
                        (Some(update), Some(package)) => {
                            package.update.insert(
                                branch.clone(),
                                UpdateRef {
                                    alias: update.alias.clone(),
                                    status: update.status.clone(),
                                    seen: today.to_string(),
                                },
                            );
                        }
                        // No update: a real answer, so a stale record
                        // goes rather than being left to mislead.
                        (None, Some(package)) => {
                            package.update.remove(branch);
                        }
                        _ => {}
                    }
                }
                Err(e) => eprintln!("warning: bodhi {release} {name}: {e}"),
            }
        }
    }
}

/// The version-release Koji has for `branch`, if any.
fn built_evr(package: &Package, branch: &str) -> Option<String> {
    let built = package.built.get(branch)?;
    sandogasa_koji::parse_nvr(&built.nvr).map(|(_, v, r)| format!("{v}-{r}"))
}

/// Whether Koji has a build for `branch` at least as new as what is
/// staged. `false` when nothing was found, since an absent record is
/// not evidence of a build.
fn built_at_least(package: &Package, branch: &str) -> bool {
    match (built_evr(package, branch), wanted_version(package, branch)) {
        (Some(built), Some(want)) => {
            sandogasa_rpmvercmp::rpmvercmp(&built, &want) != std::cmp::Ordering::Less
        }
        _ => false,
    }
}

/// The version being pushed through for `branch`: what is staged, or
/// once a package has left the COPR, what Koji built.
///
/// One definition, because it is the same question in three places —
/// whether a branch is behind, whether a build exists, and whether an
/// update carries it — and they must not disagree. A graduated package
/// has nothing staged, and answering `None` there stopped it being
/// asked about at all, so its update vanished exactly when it was
/// worth watching.
fn wanted_version(package: &Package, branch: &str) -> Option<String> {
    package
        .staged
        .as_ref()
        .map(|s| s.version.clone())
        .or_else(|| built_evr(package, branch))
}

/// Whether the version being pushed through is ahead of what `branch`
/// ships, which is what makes its build and update state interesting.
fn ahead_of_repos(package: &Package, branch: &str) -> bool {
    match (wanted_version(package, branch), package.shipped.get(branch)) {
        (Some(want), Some(shipped)) => {
            sandogasa_rpmvercmp::rpmvercmp(&want, &shipped.version) == std::cmp::Ordering::Greater
        }
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// How recent a release is, newest first, for ordering branches.
///
/// Rawhide leads because it is always ahead. Then Fedora by version and
/// EPEL by version, Fedora first: alphabetical ordering puts `epel10.3`
/// before `epel9` and both before every Fedora branch, which is neither
/// oldest-first nor newest-first — just the order the strings happen to
/// fall in.
///
/// The tuple sorts ascending, so it is negated at the point of use.
fn release_recency(branch: &str) -> (u8, Vec<u32>) {
    let parts = |v: &str| {
        v.split('.')
            .map(|p| p.parse::<u32>().unwrap_or(0))
            .collect::<Vec<u32>>()
    };
    match branch {
        // Rawhide leads its own alias: dist-git has both, and "rawhide"
        // is the name everything else here uses.
        "rawhide" => (4, Vec::new()),
        "main" => (3, Vec::new()),
        b => match b.strip_prefix("epel").or_else(|| b.strip_prefix("el")) {
            // A bare "epel10" branch builds for whichever minor release
            // is current, so it is ahead of "epel10.2" rather than
            // behind it as a plain numeric comparison would have it.
            Some(v) if !v.contains('.') => (1, vec![parts(v)[0], u32::MAX]),
            Some(v) => (1, parts(v)),
            // fNN, and anything unrecognised sorts last rather than
            // being guessed at.
            None => match b.strip_prefix('f') {
                Some(v) if v.bytes().all(|c| c.is_ascii_digit() || c == b'.') => (2, parts(v)),
                _ => (0, Vec::new()),
            },
        },
    }
}

/// Forget a target and everything recorded per-branch for it, for when
/// it turns out to name a branch already covered under another name.
fn drop_target(ledger: &mut Ledger, target: &str) {
    ledger.targets.retain(|t| t != target);
    for package in ledger.packages.values_mut() {
        package.shipped.remove(target);
        package.built.remove(target);
        package.update.remove(target);
    }
}

/// The Koji tag that carries each branch's content, from Bodhi's
/// release list. Branches Bodhi does not know are skipped rather
/// than guessed at — the tag naming is release-engineering's to
/// change, not ours to infer.
fn release_tags(branches: &[String]) -> (BTreeMap<String, Vec<String>>, BTreeMap<String, String>) {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("warning: Koji lookups skipped ({e})");
            return (BTreeMap::new(), BTreeMap::new());
        }
    };
    let releases = match rt.block_on(sandogasa_bodhi::BodhiClient::new().active_releases()) {
        Ok(releases) => releases,
        Err(e) => {
            eprintln!("warning: could not list Bodhi releases ({e}); skipping Koji lookups");
            return (BTreeMap::new(), BTreeMap::new());
        }
    };
    let mut tags = BTreeMap::new();
    let mut names = BTreeMap::new();
    for branch in branches {
        // Matched by branch or by name, because neither alone
        // suffices: F45's branch is "rawhide" and EPEL-10.3's is
        // "epel10", so a target named for the release finds nothing by
        // branch. And several releases share a branch — F43, F43C and
        // F43F all report "f43" — so the RPM one is picked by its id
        // prefix rather than by whichever Bodhi lists first.
        let rpm_release = |r: &&sandogasa_bodhi::models::BodhiRelease| {
            matches!(r.id_prefix.as_str(), "FEDORA" | "FEDORA-EPEL")
        };
        let as_branch = |name: &str| name.to_ascii_lowercase().replace(['-', '_'], "");
        match releases
            .iter()
            .find(|r| &r.branch == branch && rpm_release(r))
            .or_else(|| {
                releases
                    .iter()
                    .find(|r| as_branch(&r.name) == as_branch(branch) && rpm_release(r))
            }) {
            Some(release) => {
                let branch_tags: Vec<String> = [&release.candidate_tag, &release.testing_tag]
                    .into_iter()
                    .filter(|t| !t.is_empty())
                    .cloned()
                    .collect();
                if branch_tags.is_empty() {
                    eprintln!("warning: no Koji tags known for {branch}; skipping its builds");
                } else {
                    tags.insert(branch.clone(), branch_tags);
                }
                names.insert(branch.clone(), release.name.clone());
            }
            // Says what was looked for, not that the release does not
            // exist: Bodhi has releases this lookup cannot see, and
            // reporting the search keeps the reader pointed at the
            // naming mismatch rather than at a wrong conclusion.
            None => eprintln!(
                "warning: no Bodhi release matches {branch} by branch or name; \
                 skipping its Koji lookup"
            ),
        }
    }
    (tags, names)
}

/// Look up the review request for packages that look like they still
/// need one.
///
/// Only packages dist-git has no repository for are searched, plus
/// any whose review bug the ledger already records. A package that is
/// already imported is past this stage, and searching for all of them
/// would cost a Bugzilla query each to learn nothing.
///
/// The bug found is recorded as an observation. Which route a package
/// takes stays a decision for the user, so `route` is not set from
/// this — but the report can say a review is filed and whether it has
/// been approved.
async fn refresh_reviews(
    bz: &sandogasa_bugzilla::BzClient,
    ledger: &mut Ledger,
    today: &str,
    rescan: bool,
    only: &[String],
) {
    let wanted: Vec<String> = ledger
        .packages
        .iter()
        .filter(|(name, p)| {
            selected(only, name)
                // Retired packages are searched again: coming back may
                // require a fresh review, and the bug to find is a new
                // one rather than the original.
                && (rescan
                || p.review_bug.is_some()
                || p.review.is_some()
                || p.distgit.as_ref().is_some_and(|d| !d.exists || d.retired))
        })
        .map(|(name, _)| name.clone())
        .collect();

    for name in wanted {
        // A bug the ledger already knows is fetched directly; there
        // is no point searching for what we were told.
        // A closed review of an imported package is settled: nothing
        // about it will change, so re-fetching it every run buys
        // nothing. `--rescan-reviews` overrides that, for a package
        // whose retirement means a *new* review is needed.
        if !rescan
            && let Some(p) = ledger.packages.get(&name)
            && p.review.as_ref().is_some_and(|r| r.status == "CLOSED")
            && p.distgit.as_ref().is_some_and(|d| d.exists && !d.retired)
        {
            continue;
        }
        // Searching, not fetching, is the point of a rescan: after a
        // retirement the recorded bug is the old review, and what
        // matters is whether a new one has been filed.
        let known = (!rescan)
            .then(|| {
                ledger
                    .packages
                    .get(&name)
                    .and_then(|p| p.review_bug.or(p.review.as_ref().map(|r| r.bug)))
            })
            .flatten();
        let found = match known {
            Some(id) => match bz.bugs(&[id]).await {
                Ok(bugs) => bugs.first().map(|b| {
                    (
                        b.id,
                        b.status.clone(),
                        is_approved(b),
                        b.creation_time.format("%Y-%m-%d").to_string(),
                    )
                }),
                Err(e) => {
                    eprintln!("warning: {name}: rhbz#{id}: {e}");
                    continue;
                }
            },
            None => match crate::review_deps::find_review_bug(bz, &name).await {
                Ok(Some(bug)) => Some((
                    bug.id,
                    bug.status.clone(),
                    is_approved(&bug),
                    bug.creation_time.format("%Y-%m-%d").to_string(),
                )),
                Ok(None) => None,
                Err(e) => {
                    eprintln!("warning: {name}: {e}");
                    continue;
                }
            },
        };
        if let Some((bug, status, approved, filed)) = found
            && let Some(package) = ledger.packages.get_mut(&name)
        {
            package.review = Some(Review {
                bug,
                status,
                approved,
                filed,
                seen: today.to_string(),
            });
        }
    }
}

/// Run the Bodhi lookups on their own runtime, since `run` is sync.
fn refresh_updates_blocking(
    ledger: &mut Ledger,
    releases: &BTreeMap<String, String>,
    today: &str,
    only: &[String],
) {
    if releases.is_empty() {
        return;
    }
    match tokio::runtime::Runtime::new() {
        Ok(rt) => rt.block_on(refresh_updates(ledger, releases, today, only)),
        Err(e) => eprintln!("warning: Bodhi lookups skipped ({e})"),
    }
}

/// Run the review lookups on their own runtime, since `run` is sync.
fn refresh_reviews_blocking(ledger: &mut Ledger, today: &str, rescan: bool, only: &[String]) {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("warning: review lookups skipped ({e})");
            return;
        }
    };
    let bz = sandogasa_bugzilla::BzClient::new("https://bugzilla.redhat.com");
    rt.block_on(refresh_reviews(&bz, ledger, today, rescan, only));
}

/// Whether a review bug carries `fedora-review+`.
fn is_approved(bug: &sandogasa_bugzilla::models::Bug) -> bool {
    bug.flags
        .iter()
        .any(|f| f.name == "fedora-review" && f.status == "+")
}

/// Ask each targeted branch what version it already ships, in one
/// batched query per branch.
///
/// This is what makes the report answer "has the staged work landed
/// yet" rather than only "does the package exist". A package absent
/// from a branch simply has no entry for it; the whole query failing
/// records nothing at all, leaving the previous reading in place.
fn refresh_shipped(ledger: &mut Ledger, branches: &[String], today: &str, only: &[String]) {
    let names: Vec<String> = ledger
        .packages
        .keys()
        .filter(|name| selected(only, name))
        .cloned()
        .collect();
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
            if !selected(only, name) {
                continue;
            }
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

/// Apply a `package=route` assignment to the ledger.
///
/// The routes are `review:<bug>`, `pr:<id>`, `direct` and `unknown`.
/// A route is a decision, so it is only ever recorded from an explicit
/// instruction like this — never inferred from a lookup, however
/// suggestive the lookup is.
fn apply_assignment(ledger: &mut Ledger, spec: &str) -> Result<String, String> {
    let (name, value) = spec
        .split_once('=')
        .ok_or_else(|| format!("{spec}: expected <package>=<route>"))?;
    let package = ledger
        .packages
        .get_mut(name)
        .ok_or_else(|| format!("{name} is not tracked in this ledger"))?;
    let (route, bug, pr) = match value.split_once(':') {
        Some(("review", id)) => (
            Route::Review,
            Some(
                id.parse()
                    .map_err(|_| format!("{spec}: {id} is not a bug number"))?,
            ),
            None,
        ),
        Some(("pr", id)) => (
            Route::PullRequest,
            None,
            Some(
                id.parse()
                    .map_err(|_| format!("{spec}: {id} is not a pull request number"))?,
            ),
        ),
        Some((other, _)) => return Err(format!("{spec}: unknown route {other}")),
        None => match value {
            "direct" => (Route::Direct, None, None),
            "unknown" => (Route::Unknown, None, None),
            other => {
                return Err(format!(
                    "{spec}: unknown route {other} \
                     (review:<bug>, pr:<id>, direct, unknown)"
                ));
            }
        },
    };
    package.route = route;
    package.review_bug = bug;
    package.pull_request = pr;
    Ok(format!("{name}: {}", route.as_str()))
}

/// Whether a package is one the run was narrowed to. An empty
/// selection means everything.
fn selected(packages: &[String], name: &str) -> bool {
    packages.is_empty() || packages.iter().any(|p| p == name)
}

/// Ask dist-git about every tracked package: whether it has a
/// repository yet, and which branches it has.
///
/// A package with no repository has not been imported — for a new
/// package that means the review has not finished. That is a real
/// answer and is recorded as one; a lookup that fails is not, and
/// leaves whatever the ledger already knew, dated as before.
fn refresh_distgit(ledger: &mut Ledger, today: &str, only: &[String]) {
    let names: Vec<String> = ledger
        .packages
        .keys()
        .filter(|name| selected(only, name))
        .cloned()
        .collect();
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
                    let exists = branches.is_some();
                    // Only worth asking once the repo is known to be
                    // there; a missing repo cannot be retired.
                    // One request answers both questions: the file's
                    // presence is the retirement, its content the
                    // reason.
                    let reason = if exists {
                        client.retired_reason(name, "rawhide").await.unwrap_or(None)
                    } else {
                        None
                    };
                    let retired_on = match &reason {
                        Some(reason) => retirement_date(&client, name, reason).await,
                        None => None,
                    };
                    let record = DistGit {
                        exists,
                        branches: branches.unwrap_or_default(),
                        retired: reason.is_some(),
                        retired_reason: reason.filter(|r| !r.is_empty()),
                        retired_on,
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
fn state(package: &Package, targets: &[String]) -> String {
    let Some(d) = &package.distgit else {
        // Nothing looked up yet: say so rather than implying absence.
        return match &package.staged {
            Some(s) if !s.failed_chroots.is_empty() => "staged, not building",
            Some(_) => "staged, dist-git not checked",
            None => "not checked",
        }
        .to_string();
    };
    let staged = package.staged.as_ref();

    if !d.exists {
        // Not imported yet, so the review is what stands in the way —
        // and an approved one waits on an SCM request rather than on a
        // reviewer.
        return match (&package.review, staged) {
            (_, Some(s)) if !s.failed_chroots.is_empty() => "staged, not building",
            (Some(r), _) if r.approved => "review approved, needs importing",
            (Some(r), _) if r.status != "CLOSED" => "review filed, awaiting approval",
            (Some(_), _) => "review closed, not imported",
            (None, Some(_)) => "staged, no review filed",
            (None, None) => "not in dist-git",
        }
        .to_string();
    }

    // Retirement before anything about branches or builds: a retired
    // package keeps its repository, so those would describe a dead
    // package as work waiting to be done.
    //
    // Whether coming back needs a fresh review depends on how long it
    // has been retired, which is Fedora policy with tooling that has
    // changed recently — so no threshold is encoded. Report what is
    // known and leave the judgement to the reader.
    if d.retired {
        // What is knowable is whether a review is *open*, not whether
        // the closed one found is the first: a package can be retired
        // more than once. The search prefers the newest open review,
        // so finding only a closed one means nothing is in progress.
        return match package.review.as_ref().map(|r| r.status != "CLOSED") {
            Some(true) => "retired, review in progress",
            Some(false) => "retired, no open review",
            None => "retired in rawhide, needs unretiring",
        }
        .to_string();
    }

    // Rawhide is the shared spine: everything lands there first and is
    // branched from it, so until it is current the targets cannot
    // proceed and their state is not what the reader needs.
    if let Some(spine) = rawhide_state(package) {
        return spine;
    }

    // In Rawhide. What is left is per target, and the least advanced
    // one is the effort's real position for this package.
    let mut states: Vec<(u8, &str, &str)> = targets
        .iter()
        .filter(|t| *t != "rawhide")
        .map(|t| {
            let (rank, label) = target_state(package, d, t);
            (rank, label, t.as_str())
        })
        .collect();
    states.sort_by_key(|(rank, _, _)| *rank);
    const SHIPPED: u8 = 5;
    let Some(&(rank, label, _)) = states.first() else {
        return "in rawhide".to_string();
    };
    // Every target at the least advanced state is named, because
    // naming one of several equals reads as though the others were
    // further along — the reason a package sitting in testing for
    // three branches was reported against only one of them.
    let mut at_rank: Vec<&str> = states
        .iter()
        .filter(|(r, _, _)| *r == rank)
        .map(|(_, _, target)| *target)
        .collect();
    at_rank.sort_by_key(|b| std::cmp::Reverse(release_recency(b)));
    if rank == SHIPPED && at_rank.len() == states.len() && states.len() > 1 {
        return "shipped for every target".to_string();
    }
    format!("{label} for {}", at_rank.join(", "))
}

/// One line per branch, gathering what is known about it.
///
/// Shipped, built and in-an-update were three lines each naming a
/// version, which mostly repeated: what Koji has and what the
/// repositories have are usually the same, and the update named an
/// alias without saying which version it carried. So a version appears
/// once, a build only when it differs from what is shipped — that
/// difference being the outstanding work — and the update attaches to
/// the build it carries.
///
/// The date is the oldest of the facts shown, since "as of" must not
/// claim more freshness than the stalest part of the line.
fn branch_lines(package: &Package) -> Vec<String> {
    let branches: BTreeSet<&String> = package
        .shipped
        .keys()
        .chain(package.built.keys())
        .chain(package.update.keys())
        .collect();
    // Ordered by what each release ships, newest version first, and
    // then by how recent the release is. That puts the releases already
    // carrying the new version together at the top and the ones still
    // to do below, which is the question being asked of the line —
    // rather than alphabetically, where the two are interleaved.
    let mut branches: Vec<&String> = branches.into_iter().collect();
    branches.sort_by(|a, b| {
        let version = |branch: &str| package.shipped.get(branch).map(|s| s.version.clone());
        match (version(a), version(b)) {
            (Some(x), Some(y)) => sandogasa_rpmvercmp::rpmvercmp(&y, &x),
            // A release shipping nothing has not started, so it sorts
            // below any that has.
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
        .then_with(|| release_recency(b).cmp(&release_recency(a)))
    });
    branches
        .into_iter()
        .map(|branch| {
            let shipped = package.shipped.get(branch);
            let built = package.built.get(branch);
            let update = package.update.get(branch);
            let built_evr = built.and_then(|b| {
                sandogasa_koji::parse_nvr(&b.nvr).map(|(_, v, r)| format!("{v}-{r}"))
            });

            let mut parts = vec![match shipped {
                Some(s) => s.version.clone(),
                None => "not shipped".to_string(),
            }];
            if let Some(evr) = &built_evr
                && shipped.map(|s| s.version.as_str()) != Some(evr.as_str())
            {
                parts.push(format!("built {evr}"));
            }
            // The update qualifies the version rather than being a
            // separate fact, so it joins with a space.
            let carried = match update {
                Some(u) => format!(" in {} {}", u.alias, u.status),
                None => String::new(),
            };
            let seen = [
                shipped.map(|s| s.seen.as_str()),
                built.map(|b| b.seen.as_str()),
                update.map(|u| u.seen.as_str()),
            ]
            .into_iter()
            .flatten()
            .min()
            .unwrap_or_default();
            format!("{branch}: {}{carried} (as of {seen})", parts.join(", "))
        })
        .collect()
}

/// Where a package stands on the Rawhide spine, or `None` once
/// Rawhide carries the staged version and the targets take over.
fn rawhide_state(package: &Package) -> Option<String> {
    let ahead = ahead_of_repos(package, "rawhide");
    if !ahead {
        return (!package.shipped.contains_key("rawhide"))
            .then(|| "in dist-git, not built for rawhide".to_string());
    }
    // Ahead of the repos. Koji says whether that is work still to do
    // or a build already made and waiting for a compose.
    if !built_at_least(package, "rawhide") {
        return Some("in dist-git, no rawhide build found".to_string());
    }
    // Built. An update may be carrying it — Rawhide builds get one
    // automatically, and a side tag is submitted as one — so say which
    // rather than implying the only wait is a compose.
    Some(
        match package.update.get("rawhide") {
            Some(u) if u.status == "pending" => "built, rawhide update pending",
            Some(u) if u.status == "testing" => "built, rawhide update in testing",
            Some(_) => "built for rawhide, update pushed",
            None => "built for rawhide, not yet in the repos",
        }
        .to_string(),
    )
}

/// A target's state and how far along it is, lowest first — the
/// ranking is what makes "least advanced" meaningful.
fn target_state(package: &Package, distgit: &DistGit, target: &str) -> (u8, &'static str) {
    // Whether the target already carries the version comes first, but
    // only once something has actually been seen in its repositories:
    // branch names do not always match a target — EPEL 10's minor
    // releases all ship from the "epel10" branch — so a package
    // present in the target would otherwise be told it needs
    // branching, which is plainly false. Without a repository fact
    // there is nothing to conclude from, and the branch check below is
    // the more useful answer.
    if package.shipped.contains_key(target) && !ahead_of_repos(package, target) {
        return (5, "shipped");
    }
    if !distgit.branches.iter().any(|b| b == target) {
        return (0, "needs a branch");
    }
    if !built_at_least(package, target) {
        return (1, "needs building");
    }
    match package.update.get(target) {
        None => (2, "needs an update"),
        Some(u) if u.status == "pending" => (3, "update pending"),
        Some(u) if u.status == "testing" => (4, "update in testing"),
        Some(_) => (5, "shipped"),
    }
}

/// Render the report: what is staged, what needs a decision, and
/// what has left staging.
///
/// Every line dates what it shows, so a report served from the
/// ledger without contacting anything is honest about its age rather
/// than presenting an old reading as current.
/// Renders only the named packages, or all of them when none are
/// named.
pub fn render(ledger: &Ledger, only: &[String]) -> String {
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
        // Newest release first. With no package in hand there is no
        // version to order by, so recency is the whole key.
        let mut targets = ledger.targets.clone();
        targets.sort_by_key(|b| std::cmp::Reverse(release_recency(b)));
        targets.join(", ")
    };
    let _ = writeln!(out, "targets: {targets}");

    let mut group: BTreeMap<String, Vec<(&String, &Package)>> = BTreeMap::new();
    for (name, package) in &ledger.packages {
        if !selected(only, name) {
            continue;
        }
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
                    if let Some(reason) = &d.retired_reason {
                        match &d.retired_on {
                            Some(on) => {
                                let _ = writeln!(out, "    retired {on}: {reason}");
                            }
                            None => {
                                let _ = writeln!(out, "    retired: {reason}");
                            }
                        }
                    }
                    let _ = writeln!(
                        out,
                        "    dist-git: {}{} (as of {})",
                        if d.retired { "retired, " } else { "" },
                        // Ordered like every other branch list here,
                        // newest release first.
                        // Every branch the repository has, not the
                        // subset matching a target: the line reads as
                        // the branch list, and showing part of it as
                        // though it were all of it invites the reader
                        // to conclude a package is unbranched when it
                        // is branched under another name.
                        {
                            let mut branches = d.branches.clone();
                            branches.sort_by_key(|b| std::cmp::Reverse(release_recency(b)));
                            branches.join(", ")
                        },
                        d.seen
                    );
                } else {
                    let _ = writeln!(out, "    dist-git: no repository (as of {})", d.seen);
                }
            }
            if let Some(r) = &package.review {
                let state = if r.approved {
                    "fedora-review+".to_string()
                } else {
                    r.status.to_ascii_lowercase()
                };
                let filed = if r.filed.is_empty() {
                    String::new()
                } else {
                    format!(", filed {}", r.filed)
                };
                let _ = writeln!(
                    out,
                    "    review: rhbz#{} {state}{filed} (as of {})",
                    r.bug, r.seen
                );
            }
            for line in branch_lines(package) {
                let _ = writeln!(out, "    {line}");
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
        let text = render(&ledger, &[]);
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
            retired: false,
            retired_reason: None,
            retired_on: None,
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

        // With dist-git answered, the next question is the review —
        // and no review record means none was found.
        package.distgit = Some(distgit(false, &[]));
        assert_eq!(state(&package, &[]), "staged, no review filed");
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
        // Targeting epel9 as well, it is not branched yet — and the
        // heading names the target, since with several the reader
        // needs to know which one is behind.
        let targets = vec!["rawhide".to_string(), "epel9".to_string()];
        assert_eq!(state(&package, &targets), "needs a branch for epel9");
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
        // Rawhide is behind and Koji has nothing newer, so the build
        // is the outstanding work.
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("0.12.0-7.fc45"));
        assert_eq!(state(&package, &[]), "in dist-git, no rawhide build found");

        // Rawhide has caught up — the release differs, but the
        // version does not, so nothing is outstanding.
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("0.13.0-1.fc46"));
        assert_eq!(state(&package, &[]), "in rawhide");
    }

    fn built_at(nvr: &str) -> Built {
        Built {
            nvr: nvr.to_string(),
            tag: "f45".to_string(),
            seen: "2026-08-10".to_string(),
        }
    }

    #[test]
    fn state_separates_needing_a_build_from_awaiting_a_compose() {
        let mut package = Package {
            distgit: Some(distgit(true, &["rawhide"])),
            staged: Some(staged_at("0.13.0-1")),
            ..Package::default()
        };
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("0.12.0-7.fc45"));

        // Koji has only the old build, so this is work to do.
        package
            .built
            .insert("rawhide".to_string(), built_at("rust-a-0.12.0-7.fc45"));
        assert_eq!(state(&package, &[]), "in dist-git, no rawhide build found");

        // Koji has the staged version but the repos do not: built and
        // waiting on a compose, which is not the same as unbuilt.
        package
            .built
            .insert("rawhide".to_string(), built_at("rust-a-0.13.0-1.fc45"));
        assert_eq!(
            state(&package, &[]),
            "built for rawhide, not yet in the repos"
        );
    }

    #[test]
    fn state_reports_an_update_carrying_a_branch_build() {
        // On a branched release a build reaches the repos only via an
        // update, so "built but not shipped" has to say whether one is
        // in flight or still needs submitting. Rawhide has no updates,
        // so this only applies to a target.
        let targets = vec!["epel9".to_string()];
        let mut package = Package {
            distgit: Some(distgit(true, &["rawhide", "epel9"])),
            staged: Some(staged_at("0.13.0-1")),
            ..Package::default()
        };
        // Rawhide is current, so the targets are what is left.
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("0.13.0-1.fc46"));
        package
            .shipped
            .insert("epel9".to_string(), shipped_at("0.12.0-7.el9"));
        package
            .built
            .insert("epel9".to_string(), built_at("rust-a-0.13.0-1.el9"));

        // Built with no update: submitting one is the next step.
        assert_eq!(state(&package, &targets), "needs an update for epel9");
        // Rawhide is not exempt: its builds can be carried by an
        // update too, automatically or from a side tag, so the state
        // reports one when there is one.
        let mut rawhide_only = Package {
            distgit: Some(distgit(true, &["rawhide"])),
            staged: Some(staged_at("0.13.0-1")),
            ..Package::default()
        };
        rawhide_only
            .shipped
            .insert("rawhide".to_string(), shipped_at("0.12.0-7.fc45"));
        rawhide_only
            .built
            .insert("rawhide".to_string(), built_at("rust-a-0.13.0-1.fc45"));
        assert_eq!(
            state(&rawhide_only, &[]),
            "built for rawhide, not yet in the repos"
        );
        rawhide_only.update.insert(
            "rawhide".to_string(),
            UpdateRef {
                alias: "FEDORA-2026-6239fda063".to_string(),
                status: "testing".to_string(),
                seen: "2026-08-11".to_string(),
            },
        );
        assert_eq!(
            state(&rawhide_only, &[]),
            "built, rawhide update in testing"
        );

        for (status, expected) in [
            ("pending", "update pending for epel9"),
            ("testing", "update in testing for epel9"),
            ("stable", "shipped for epel9"),
        ] {
            package.update.insert(
                "epel9".to_string(),
                UpdateRef {
                    alias: "FEDORA-EPEL-2026-abc".to_string(),
                    status: status.to_string(),
                    seen: "2026-08-11".to_string(),
                },
            );
            assert_eq!(state(&package, &targets), expected, "status {status}");
        }
    }

    #[test]
    fn state_reports_the_least_advanced_target() {
        // With several targets the effort's position for a package is
        // whichever is furthest behind — that is where the next move
        // is.
        let targets = vec!["epel9".to_string(), "epel10".to_string()];
        let mut package = Package {
            distgit: Some(distgit(true, &["rawhide", "epel9", "epel10"])),
            staged: Some(staged_at("0.13.0-1")),
            ..Package::default()
        };
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("0.13.0-1.fc46"));
        // epel10 is done; epel9 has not been built.
        package
            .shipped
            .insert("epel10".to_string(), shipped_at("0.13.0-1.el10"));
        package
            .shipped
            .insert("epel9".to_string(), shipped_at("0.12.0-7.el9"));
        assert_eq!(state(&package, &targets), "needs building for epel9");

        // Once epel9 catches up both are done, and neither is named:
        // picking one of two equals would be arbitrary.
        package
            .shipped
            .insert("epel9".to_string(), shipped_at("0.13.0-1.el9"));
        assert_eq!(state(&package, &targets), "shipped for every target");
    }

    #[test]
    fn state_needs_a_build_when_koji_was_not_asked() {
        // No Koji record at all — the honest reading is that the work
        // is outstanding, not that a build is waiting somewhere.
        let mut package = Package {
            distgit: Some(distgit(true, &["rawhide"])),
            staged: Some(staged_at("0.13.0-1")),
            ..Package::default()
        };
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("0.12.0-7.fc45"));
        assert_eq!(state(&package, &[]), "in dist-git, no rawhide build found");
    }

    #[test]
    fn state_notices_a_package_never_built_for_rawhide() {
        // Imported but in neither the repos nor Koji: the build is
        // what is outstanding.
        let package = Package {
            distgit: Some(distgit(true, &["rawhide"])),
            staged: Some(staged_at("0.1.0-1")),
            ..Package::default()
        };
        assert_eq!(state(&package, &[]), "in dist-git, no rawhide build found");

        // Nothing staged either — it has left the COPR — so there is
        // no version to compare and all that can be said is that the
        // repos do not carry it.
        let package = Package {
            distgit: Some(distgit(true, &["rawhide"])),
            ..Package::default()
        };
        assert_eq!(state(&package, &[]), "in dist-git, not built for rawhide");
    }

    fn review(bug: u64, status: &str, approved: bool) -> Review {
        Review {
            bug,
            status: status.to_string(),
            approved,
            filed: "2024-01-13".to_string(),
            seen: "2026-08-10".to_string(),
        }
    }

    #[test]
    fn state_reads_the_review_for_an_unimported_package() {
        let mut package = Package {
            distgit: Some(distgit(false, &[])),
            staged: Some(staged_at("0.1.0-1")),
            ..Package::default()
        };
        // No review found: the first thing to do is file one.
        assert_eq!(state(&package, &[]), "staged, no review filed");

        package.review = Some(review(2498026, "NEW", false));
        assert_eq!(state(&package, &[]), "review filed, awaiting approval");

        // Approval is the fedora-review+ flag, not the status, and
        // it moves the package on to needing an SCM request.
        package.review = Some(review(2498026, "ASSIGNED", true));
        assert_eq!(state(&package, &[]), "review approved, needs importing");
    }

    #[test]
    fn state_says_whether_a_retired_package_has_a_review_open() {
        // The actionable question is whether a review is open, and
        // that needs no dates: the search prefers the newest open
        // review, so finding only a closed one means none is.
        let mut d = distgit(true, &["rawhide"]);
        d.retired = true;
        let mut package = Package {
            distgit: Some(d),
            staged: Some(staged_at("0.9.0-1")),
            review: Some(review(2258203, "CLOSED", true)),
            ..Package::default()
        };
        // Only a closed review: nothing is in progress. Deliberately
        // not "its original review" — a package can be retired more
        // than once, so the newest closed review need not be the
        // first, and the report should not claim otherwise.
        assert_eq!(state(&package, &[]), "retired, no open review");

        // An open one means someone is already reviewing it.
        package.review = Some(review(2500000, "NEW", false));
        assert_eq!(state(&package, &[]), "retired, review in progress");
    }

    #[test]
    fn render_shows_when_a_review_was_filed() {
        // The date is context for judging whether a retired
        // package's review is old enough to need redoing — a
        // judgement the tool deliberately leaves to the reader.
        let mut ledger = Ledger::default();
        ledger.reconcile("@rust/uutils", &[staged("rust-a", "0.9.0-1")], "2026-08-10");
        ledger.packages.get_mut("rust-a").unwrap().review = Some(review(2258203, "CLOSED", true));
        let text = render(&ledger, &[]);
        assert!(text.contains("filed 2024-01-13"), "{text}");
    }

    #[test]
    fn state_notices_a_review_closed_without_approval() {
        // Closed without fedora-review+ is an abandoned review, not
        // an accepted one, and the package is still not imported.
        let package = Package {
            distgit: Some(distgit(false, &[])),
            review: Some(review(2498026, "CLOSED", false)),
            staged: Some(staged_at("0.1.0-1")),
            ..Package::default()
        };
        assert_eq!(state(&package, &[]), "review closed, not imported");
    }

    #[test]
    fn render_shows_the_review_bug_and_its_state() {
        let mut ledger = Ledger::default();
        ledger.reconcile("@rust/uutils", &[staged("rust-a", "0.1.0-1")], "2026-08-10");
        let package = ledger.packages.get_mut("rust-a").unwrap();
        package.distgit = Some(distgit(false, &[]));
        package.review = Some(review(2498026, "NEW", true));
        let text = render(&ledger, &[]);
        assert!(
            text.contains("review approved, needs importing (1)"),
            "{text}"
        );
        assert!(
            text.contains(
                "review: rhbz#2498026 fedora-review+, filed 2024-01-13 (as of 2026-08-10)"
            ),
            "{text}"
        );
    }

    #[test]
    fn state_reports_retirement_rather_than_a_missing_build() {
        // A retired package keeps its repository, so "the repo
        // exists but nothing is built" would read as work waiting to
        // be done when the real obstacle is the retirement.
        let mut d = distgit(true, &["rawhide"]);
        d.retired = true;
        let package = Package {
            distgit: Some(d),
            staged: Some(staged_at("0.1.0-1")),
            ..Package::default()
        };
        assert_eq!(state(&package, &[]), "retired in rawhide, needs unretiring");
    }

    #[test]
    fn render_dates_a_retirement_only_when_it_is_known() {
        let mut ledger = Ledger::default();
        ledger.reconcile("@rust/uutils", &[staged("rust-a", "0.9.0-1")], "2026-08-10");
        let mut d = distgit(true, &["rawhide"]);
        d.retired = true;
        d.retired_reason = Some("replaced by uutils-coreutils".to_string());
        ledger.packages.get_mut("rust-a").unwrap().distgit = Some(d.clone());

        // Without a date, the reason still shows — but no date is
        // invented for it.
        let text = render(&ledger, &[]);
        assert!(
            text.contains("retired: replaced by uutils-coreutils"),
            "{text}"
        );
        assert!(!text.contains("retired 20"), "{text}");

        // With one, it leads, since how long a package has been dead
        // is what the reader is weighing.
        d.retired_on = Some("2026-06-09".to_string());
        ledger.packages.get_mut("rust-a").unwrap().distgit = Some(d);
        let text = render(&ledger, &[]);
        assert!(
            text.contains("retired 2026-06-09: replaced by uutils-coreutils"),
            "{text}"
        );
    }

    #[test]
    fn render_marks_a_retired_repository() {
        let mut ledger = Ledger::default();
        ledger.reconcile("@rust/uutils", &[staged("rust-a", "0.1.0-1")], "2026-08-10");
        let mut d = distgit(true, &["rawhide"]);
        d.retired = true;
        ledger.packages.get_mut("rust-a").unwrap().distgit = Some(d);
        let text = render(&ledger, &[]);
        assert!(text.contains("dist-git: retired, rawhide"), "{text}");
    }

    #[test]
    fn branch_lines_state_a_version_once() {
        let mut package = Package::default();
        // Koji and the repositories agree, which is the usual case —
        // so the version is stated once, not twice.
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("0.7.0-2.fc45"));
        package
            .built
            .insert("rawhide".to_string(), built_at("rust-a-0.7.0-2.fc45"));
        assert_eq!(
            branch_lines(&package),
            vec!["rawhide: 0.7.0-2.fc45 (as of 2026-08-10)"]
        );

        // A build ahead of the repositories is the outstanding work,
        // so that difference does get its own mention.
        package
            .built
            .insert("rawhide".to_string(), built_at("rust-a-0.9.0-1.fc45"));
        assert_eq!(
            branch_lines(&package),
            vec!["rawhide: 0.7.0-2.fc45, built 0.9.0-1.fc45 (as of 2026-08-10)"]
        );

        // The update qualifies the version it carries.
        package.update.insert(
            "rawhide".to_string(),
            UpdateRef {
                alias: "FEDORA-2026-abc".to_string(),
                status: "testing".to_string(),
                seen: "2026-08-11".to_string(),
            },
        );
        let line = &branch_lines(&package)[0];
        assert_eq!(
            line,
            "rawhide: 0.7.0-2.fc45, built 0.9.0-1.fc45 \
             in FEDORA-2026-abc testing (as of 2026-08-10)"
        );
    }

    #[test]
    fn branch_lines_date_the_stalest_fact() {
        // One line, several observations: "as of" must not claim more
        // freshness than its oldest part.
        let mut package = Package::default();
        package.shipped.insert(
            "epel9".to_string(),
            Shipped {
                version: "1.0-1.el9".to_string(),
                seen: "2026-08-01".to_string(),
            },
        );
        package.update.insert(
            "epel9".to_string(),
            UpdateRef {
                alias: "FEDORA-EPEL-2026-x".to_string(),
                status: "testing".to_string(),
                seen: "2026-08-11".to_string(),
            },
        );
        let line = &branch_lines(&package)[0];
        assert!(line.contains("as of 2026-08-01"), "{line}");
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
        let text = render(&ledger, &[]);
        // Both versions are visible, so the comparison the heading
        // makes can be checked by eye.
        assert!(text.contains("rust-a 0.13.0-1"), "{text}");
        assert!(
            text.contains("rawhide: 0.12.0-7.fc45 (as of 2026-08-10)"),
            "{text}"
        );
        assert!(
            text.contains("in dist-git, no rawhide build found (1)"),
            "{text}"
        );
    }

    #[test]
    fn render_shows_dist_git_state_with_its_date() {
        let mut ledger = Ledger::default();
        ledger.reconcile("@rust/uutils", &[staged("rust-a", "1-1")], "2026-08-10");
        ledger.packages.get_mut("rust-a").unwrap().distgit = Some(distgit(false, &[]));
        let text = render(&ledger, &[]);
        assert!(text.contains("staged, no review filed (1)"), "{text}");
        assert!(
            text.contains("dist-git: no repository (as of 2026-08-10)"),
            "{text}"
        );
    }

    #[test]
    fn a_package_that_left_the_copr_is_still_tracked_by_its_build() {
        // No staged record, because it graduated out of the COPR. The
        // version of interest becomes what Koji built, so the package
        // keeps being asked about and its update stays visible — which
        // is exactly when it is worth watching reach stable.
        let mut package = Package {
            distgit: Some(distgit(true, &["rawhide"])),
            ..Package::default()
        };
        package
            .shipped
            .insert("rawhide".to_string(), shipped_at("2.0.1-5.fc45"));
        package
            .built
            .insert("rawhide".to_string(), built_at("rust-a-2.1.1-1.fc45"));

        assert_eq!(
            wanted_version(&package, "rawhide").as_deref(),
            Some("2.1.1-1.fc45")
        );
        // Ahead of the repos even with nothing staged, so it is still
        // queried rather than being treated as settled.
        assert!(ahead_of_repos(&package, "rawhide"));
        assert!(built_at_least(&package, "rawhide"));
        assert_eq!(
            state(&package, &[]),
            "built for rawhide, not yet in the repos"
        );

        package.update.insert(
            "rawhide".to_string(),
            UpdateRef {
                alias: "FEDORA-2026-6239fda063".to_string(),
                status: "stable".to_string(),
                seen: "2026-08-11".to_string(),
            },
        );
        assert_eq!(state(&package, &[]), "built for rawhide, update pushed");
        assert!(branch_lines(&package)[0].contains("in FEDORA-2026-6239fda063 stable"));
    }

    #[test]
    fn built_evr_reads_the_version_out_of_the_nvr() {
        let mut package = Package::default();
        assert_eq!(built_evr(&package, "rawhide"), None);
        package
            .built
            .insert("rawhide".to_string(), built_at("rust-a-0.13.0-1.fc45"));
        // What an update is matched against once nothing is staged.
        assert_eq!(
            built_evr(&package, "rawhide").as_deref(),
            Some("0.13.0-1.fc45")
        );
    }

    #[test]
    fn apply_assignment_records_each_route() {
        let mut ledger = Ledger::default();
        ledger.reconcile("@rust/uutils", &[staged("rust-a", "1-1")], "2026-08-11");

        apply_assignment(&mut ledger, "rust-a=review:2498026").unwrap();
        let p = &ledger.packages["rust-a"];
        assert_eq!(p.route, Route::Review);
        assert_eq!(p.review_bug, Some(2498026));

        // Switching route clears the identifier the old one carried,
        // so a stale bug number cannot outlive the route it belonged
        // to.
        apply_assignment(&mut ledger, "rust-a=pr:1234").unwrap();
        let p = &ledger.packages["rust-a"];
        assert_eq!(p.route, Route::PullRequest);
        assert_eq!(p.pull_request, Some(1234));
        assert_eq!(p.review_bug, None);

        apply_assignment(&mut ledger, "rust-a=direct").unwrap();
        assert_eq!(ledger.packages["rust-a"].route, Route::Direct);
        assert_eq!(ledger.packages["rust-a"].pull_request, None);
    }

    #[test]
    fn apply_assignment_rejects_what_it_cannot_apply() {
        let mut ledger = Ledger::default();
        ledger.reconcile("@rust/uutils", &[staged("rust-a", "1-1")], "2026-08-11");

        // An untracked package is a mistake worth reporting, not a
        // silent no-op.
        let err = apply_assignment(&mut ledger, "rust-nope=direct").unwrap_err();
        assert!(err.contains("not tracked"), "{err}");

        for bad in ["rust-a", "rust-a=review:abc", "rust-a=sideways"] {
            assert!(apply_assignment(&mut ledger, bad).is_err(), "{bad}");
        }
        // A rejected assignment leaves the ledger alone.
        assert_eq!(ledger.packages["rust-a"].route, Route::Unknown);
    }

    #[test]
    fn render_narrows_to_the_named_packages() {
        let mut ledger = Ledger::default();
        ledger.reconcile(
            "@rust/uutils",
            &[staged("rust-a", "1-1"), staged("rust-b", "1-1")],
            "2026-08-10",
        );
        let text = render(&ledger, &["rust-b".to_string()]);
        assert!(text.contains("rust-b"), "{text}");
        assert!(!text.contains("rust-a"), "{text}");
        // The header still describes the whole effort, so the report
        // does not imply the ledger holds only what was asked for.
        assert!(text.contains("2 package(s) tracked"), "{text}");
    }

    #[test]
    fn render_says_what_to_do_with_an_empty_ledger() {
        let text = render(&Ledger::default(), &[]);
        assert!(text.contains("--copr"), "{text}");
    }

    #[test]
    fn side_tag_names_a_target() {
        let mut ledger = Ledger::default();
        for tag in [
            "f43-build-side-146829",
            "epel10.3-build-side-146831",
            "rawhide-build-side-1",
        ] {
            let branch = crate::check_update::branch_from_side_tag(tag).unwrap();
            if branch != "rawhide" && !ledger.targets.contains(&branch) {
                ledger.targets.push(branch);
            }
        }
        // rawhide is always examined, so it is not recorded as a target.
        assert_eq!(ledger.targets, vec!["f43", "epel10.3"]);
    }

    #[test]
    fn a_name_that_is_not_a_side_tag_is_refused() {
        // The comma-joined form a pre-CSV run would have stored.
        for bad in [
            "f44-build-side-1,f43-build-side-2",
            "f43",
            "",
            "f43-build-side-",
        ] {
            assert!(
                crate::check_update::branch_from_side_tag(bad).is_none(),
                "{bad} should not parse as a side tag"
            );
        }
    }

    #[test]
    fn shipped_target_is_not_reported_as_unbranched() {
        let mut package = Package {
            route: Route::Direct,
            ..Package::default()
        };
        // EPEL 10's minor releases build from the "epel10" branch, so a
        // package shipped for epel10.3 has no branch of that name.
        let distgit = DistGit {
            exists: true,
            branches: vec!["epel10".into(), "rawhide".into()],
            retired: false,
            retired_reason: None,
            retired_on: None,
            seen: "2026-08-11".into(),
        };
        package.shipped.insert(
            "epel10.3".into(),
            Shipped {
                version: "0.19.1-1.el10_3".into(),
                seen: "2026-08-11".into(),
            },
        );
        let (_, label) = target_state(&package, &distgit, "epel10.3");
        assert_eq!(label, "shipped");
    }

    #[test]
    fn releases_order_newest_first() {
        let mut branches = vec![
            "epel10.3", "epel9", "f43", "f44", "rawhide", "epel10.2", "main", "epel10",
        ];
        branches.sort_by_key(|b| std::cmp::Reverse(release_recency(b)));
        // Alphabetically this is epel10, epel10.2, epel10.3, epel9,
        // f43, f44, main, rawhide — neither oldest- nor newest-first.
        // The bare epel10 branch tracks the current minor, so it leads
        // the numbered ones.
        assert_eq!(
            branches,
            vec![
                "rawhide", "main", "f44", "f43", "epel10", "epel10.3", "epel10.2", "epel9"
            ]
        );
    }

    #[test]
    fn branch_lines_lead_with_the_newest_version() {
        let mut package = Package::default();
        // Two rollouts at once: 0.19.1 has reached Rawhide and EPEL
        // 10.3, while the older branches are still on 0.18.1 with a
        // build waiting in an update.
        for (branch, version) in [
            ("epel9", "0.18.1-1.el9"),
            ("f43", "0.18.1-1.fc43"),
            ("epel10.3", "0.19.1-1.el10_3"),
            ("f44", "0.18.1-1.fc44"),
            ("rawhide", "0.19.1-1.fc45"),
        ] {
            package
                .shipped
                .insert(branch.to_string(), shipped_at(version));
        }
        let lines = branch_lines(&package);
        let branches: Vec<&str> = lines.iter().map(|l| l.split(':').next().unwrap()).collect();
        // The releases carrying the new version first, newest release
        // first within each group.
        assert_eq!(branches, vec!["rawhide", "epel10.3", "f44", "f43", "epel9"]);
    }
}
