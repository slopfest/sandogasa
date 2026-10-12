// SPDX-License-Identifier: Apache-2.0 OR MIT

//! RPM Fusion counterparts of the packages in an update.
//!
//! RPM Fusion rebuilds Fedora packages with the codecs Fedora has to
//! strip, so a Fedora update can need a rebuild there to stay in step:
//! Fedora's `libheif` pairs with RPM Fusion's `libheif-freeworld`. An
//! update that reaches stable before that rebuild leaves the pair
//! broken, which is why finding one of these turns autopush off.
//!
//! What a counterpart is called follows no single rule. Measured
//! against `rpmfusion-f43` (263 source packages), three shapes occur:
//! the same name (`ffmpeg`), the name plus `-freeworld` (`libheif` →
//! `libheif-freeworld`), and neither (`vlc` →
//! `vlc-plugins-freeworld`). So a name is *guessed* three ways and
//! each guess is checked against what RPM Fusion actually ships —
//! the generated inventory has the last word, never the guess.

use std::collections::{BTreeMap, HashSet};

/// The suffix RPM Fusion gives a companion package built from the
/// same upstream as a Fedora one.
const FREEWORLD: &str = "-freeworld";

/// What RPM Fusion ships for one release, and the renames that its
/// naming does not make guessable.
#[derive(Clone, Debug)]
pub struct Counterparts {
    /// What RPM Fusion ships, per release. Kept whole rather than
    /// narrowed to the update's release, so a bug can report every
    /// release that needs the same rebuild.
    shipped: BTreeMap<String, HashSet<String>>,
    /// Fedora name → RPM Fusion name, for the cases no rule reaches.
    renames: BTreeMap<String, String>,
    /// The release the update targets, which `of` answers for.
    release: String,
}

/// The `[renames]` table of `configs/rpmfusion/renames.toml`.
#[derive(serde::Deserialize, Default)]
struct RenameFile {
    #[serde(default)]
    renames: BTreeMap<String, String>,
}

impl Counterparts {
    /// Read the generated inventory and the rename map.
    ///
    /// `release` is the inventory workload to read — the question is
    /// per release, because RPM Fusion ships a package for one Fedora
    /// release and not another (`mesa-freeworld` is in f43, not el10).
    /// An unknown workload is an error rather than an empty answer: a
    /// silent "nothing matches" would disable the guard precisely when
    /// the inventory needs regenerating.
    pub fn load(
        inventory_path: &str,
        renames_path: Option<&str>,
        release: &str,
    ) -> Result<Self, String> {
        let inventory = sandogasa_inventory::load(inventory_path)?;
        let workloads = &inventory.inventory.workloads;
        // A workload labelled with the release answers for it. That
        // is how rawhide is reached: RPM Fusion calls it "rawhide" in
        // their repo ids and in their Bugzilla's version list, while
        // the update being checked is for f46, and `sync-rpmfusion`
        // records the correspondence as a label rather than leaving
        // every branched-release update to fail here.
        let release = match workloads.contains_key(release) {
            true => release.to_string(),
            false => workloads
                .iter()
                .find(|(_, m)| m.labels.iter().flatten().any(|l| l == release))
                .map(|(key, _)| key.clone())
                .ok_or_else(|| {
                    format!(
                        "{inventory_path} has no packages for {release} — regenerate it \
                         with `poi-tracker sync-rpmfusion --release {release}` (it has: {})",
                        match workloads.is_empty() {
                            true => "nothing".to_string(),
                            false => workloads.keys().cloned().collect::<Vec<_>>().join(", "),
                        }
                    )
                })?,
        };
        let shipped: BTreeMap<String, HashSet<String>> = inventory
            .inventory
            .workloads
            .iter()
            .map(|(rel, meta)| (rel.clone(), meta.packages.iter().cloned().collect()))
            .collect();
        let renames = match renames_path {
            Some(path) => {
                let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
                toml::from_str::<RenameFile>(&text)
                    .map_err(|e| format!("{path}: {e}"))?
                    .renames
            }
            None => BTreeMap::new(),
        };
        Ok(Counterparts {
            shipped,
            renames,
            release,
        })
    }

    /// The RPM Fusion package that pairs with a Fedora source package,
    /// or `None` when RPM Fusion ships nothing for it.
    ///
    /// Tried in order: the name itself, the rename table, then
    /// `<name>-freeworld`. Each is only accepted if RPM Fusion ships
    /// it, which is what makes the last guess safe to make at all.
    pub fn of(&self, fedora: &str) -> Option<&str> {
        let here = self.shipped.get(&self.release)?;
        let rename = self.renames.get(fedora).map(String::as_str);
        let guess = format!("{fedora}{FREEWORLD}");
        [Some(fedora), rename, Some(guess.as_str())]
            .into_iter()
            .flatten()
            .find_map(|name| here.get(name).map(String::as_str))
    }

    /// Every release whose RPM Fusion repositories carry `rf`, in
    /// inventory order. What the bug lists as needing a rebuild.
    pub fn releases_shipping(&self, rf: &str) -> Vec<&str> {
        self.shipped
            .iter()
            .filter(|(_, pkgs)| pkgs.contains(rf))
            .map(|(rel, _)| rel.as_str())
            .collect()
    }

    /// Every package of an update that has a counterpart, as
    /// (Fedora name, RPM Fusion name), in the order given.
    pub fn in_update<'a>(&'a self, packages: &'a [String]) -> Vec<(&'a str, &'a str)> {
        packages
            .iter()
            .filter_map(|p| self.of(p).map(|rf| (p.as_str(), rf)))
            .collect()
    }

    pub fn release(&self) -> &str {
        &self.release
    }
}

/// The inventory workload a dist-git branch asks about: `f43` stays
/// `f43`, `epel10` becomes `el10` — the spelling
/// `configs/fedrq/rpmfusion.toml` uses, since RPM Fusion builds for
/// EL rather than for EPEL.
///
/// An EPEL minor collapses to its major (`epel10.4` → `el10`): EPEL
/// branches per RHEL minor, RPM Fusion publishes one EL repository per
/// major, and a libheif update really does arrive as EPEL-10.2,
/// EPEL-10.3 and EPEL-10.4 against the single `free-el-10`.
pub fn workload_for_branch(branch: &str) -> Option<String> {
    // Rawhide arrives spelled both ways and both have to land on the
    // same workload: a Bodhi update for it reports the branch as
    // "rawhide", while a side tag for it is f46-build-side-NNN and
    // reports "f46". This handles the first; the second goes through
    // the workload's label, which `sync-rpmfusion` records.
    if branch == "rawhide" {
        return Some(branch.to_string());
    }
    if let Some(rest) = branch.strip_prefix("epel") {
        let major = rest.split_once('.').map_or(rest, |(major, _)| major);
        return (!major.is_empty() && major.chars().all(|c| c.is_ascii_digit()))
            .then(|| format!("el{major}"));
    }
    let digits = branch.strip_prefix('f')?;
    (!digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())).then(|| branch.to_string())
}

/// What RPM Fusion has for one release, for the bug's table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseState {
    pub release: String,
    /// The version-release RPM Fusion currently ships, when it does.
    pub shipped: Option<String>,
    /// The version-release in their updates-testing, when it holds
    /// something the stable repository does not.
    pub testing: Option<String>,
    /// The `Version:` in their dist-git branch's spec, when there is
    /// a branch. Work committed but not yet built reads here and
    /// nowhere else.
    pub distgit: Option<String>,
}

/// Where RPM Fusion's dist-git serves git over HTTP.
///
/// Git only: their cgit answers 404 for every path, its own index
/// included, so this is a remote and never a link to show anyone.
/// Their Koji needs credentials too, which leaves the ref list as
/// the one thing about their packaging readable from outside —
/// enough to say which branches exist, not what is in them. A
/// `--depth 1` clone does reach the spec, which is how the version
/// in dist-git could be read as well (not done here).
const DISTGIT: &str = "https://pkgs.rpmfusion.org/git";

/// The `Version:` in each dist-git branch's spec, by branch name.
///
/// One bare `--depth 1 --no-single-branch` clone covers every branch
/// at once — 0.25 s for libheif-freeworld — so this costs less than
/// the ref list it replaces while saying far more. A counterpart
/// already committed at the new version is waiting on a build, not on
/// somebody noticing; "branched" could not tell those apart.
///
/// The namespace is not recorded in the inventory (both halves answer
/// the same question), so free is tried first and nonfree second. A
/// failure is not an error: the column simply says nothing, which is
/// better than failing a submit over a git probe.
pub fn distgit_versions(rf: &str) -> BTreeMap<String, String> {
    distgit_versions_at(DISTGIT, rf)
}

/// [`distgit_versions`] against a given base, so a test can point it
/// at repositories on disk: `git clone` takes a local path, and the
/// failure worth guarding is silent — a clone that stops working
/// returns nothing, and every release then reads "no branch" in a bug
/// sent to another project.
fn distgit_versions_at(base: &str, rf: &str) -> BTreeMap<String, String> {
    let Ok(dir) = tempfile::tempdir() else {
        return BTreeMap::new();
    };
    for namespace in ["free", "nonfree"] {
        let url = format!("{base}/{namespace}/{rf}.git");
        let repo = dir.path().join(namespace);
        let cloned = std::process::Command::new("git")
            .args([
                "clone",
                "--quiet",
                "--bare",
                "--depth",
                "1",
                "--no-single-branch",
            ])
            .arg(&url)
            .arg(&repo)
            .output();
        if !matches!(cloned, Ok(ref out) if out.status.success()) {
            continue;
        }
        let versions = versions_in(&repo, rf);
        if !versions.is_empty() {
            return versions;
        }
    }
    BTreeMap::new()
}

/// The `Version:` on every branch of an already-cloned bare repo.
///
/// Split from the clone so it can be tested against a repository made
/// on disk, with no network: the rule worth guarding is which branch
/// maps to which version, not that `git clone` works.
fn versions_in(repo: &std::path::Path, rf: &str) -> BTreeMap<String, String> {
    let Ok(refs) = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(repo)
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads/"])
        .output()
    else {
        return BTreeMap::new();
    };
    String::from_utf8_lossy(&refs.stdout)
        .lines()
        .filter_map(|branch| {
            let branch = branch.trim();
            let show = std::process::Command::new("git")
                .arg("--git-dir")
                .arg(repo)
                .arg("show")
                .arg(format!("{branch}:{rf}.spec"))
                .output()
                .ok()?;
            let version = spec_version(&String::from_utf8_lossy(&show.stdout))?;
            Some((branch.to_string(), version))
        })
        .collect()
}

/// The `Version:` a spec declares, if it declares a plain one.
///
/// A spec whose version is a macro (`%{upstream_version}`) yields
/// nothing rather than the macro text — an unexpanded `%{...}` in a
/// bug sent to another project is worse than an empty column.
fn spec_version(spec: &str) -> Option<String> {
    let value = spec
        .lines()
        .find_map(|l| l.strip_prefix("Version:"))?
        .trim()
        .to_string();
    (!value.is_empty() && !value.contains('%')).then_some(value)
}

/// The dist-git branch name for a release.
///
/// RPM Fusion keeps rawhide on `master`: their ref list for
/// libheif-freeworld on 2026-10-12 ran el8/el9/el10 and f36 through
/// f45, with no f46 and no rawhide, so `master` is what rawhide is
/// developed on. Matching the release name alone reported "no branch"
/// for a package they do in fact carry, which is the kind of wrong
/// claim this bug exists to avoid making.
fn distgit_branch_for(release: &str) -> &str {
    match release {
        "rawhide" => "master",
        other => other,
    }
}

/// The version without its dist tag (`1.23.6-1.fc43` → `1.23.6-1`).
///
/// One rebuild request covers every release, so naming the dist tag
/// of whichever update happened to be checked first would date the
/// whole bug to one of six releases.
fn without_dist(evr: &str) -> &str {
    match evr.rfind(".fc").or_else(|| evr.rfind(".el")) {
        Some(at) => &evr[..at],
        None => evr,
    }
}

/// What RPM Fusion ships of `rf`, per release, and whether their
/// dist-git holds for it.
pub fn survey(
    rf: &str,
    releases: &[&str],
    distgit: &BTreeMap<String, String>,
) -> Vec<ReleaseState> {
    let vr = |release: &str, repo: Option<&str>| {
        sandogasa_fedrq::Fedrq {
            branch: Some(format!("rpmfusion-{release}")),
            repo: repo.map(str::to_string),
        }
        .pkgs_source_vr(&[rf])
        .ok()
        .and_then(|rows| rows.into_iter().next().map(|(_, v)| v))
    };
    releases
        .iter()
        .map(|release| {
            let shipped = vr(release, None);
            // `testing` includes the stable repos, so it answers with
            // whichever is newer. Reported only when it differs, so
            // the column says "there is something in testing" rather
            // than repeating the released version on every row.
            let testing = vr(release, Some("testing")).filter(|t| Some(t) != shipped.as_ref());
            ReleaseState {
                release: (*release).to_string(),
                shipped,
                testing,
                distgit: distgit.get(distgit_branch_for(release)).cloned(),
            }
        })
        .collect()
}

/// The body of the bug filed against RPM Fusion.
///
/// Deliberately the opposite of the CVE trackers this workspace
/// spends its time disproving: it says what was updated, what RPM
/// Fusion currently ships for each release, whether a branch exists,
/// and which updates to follow — so the reader can act without
/// looking anything up.
pub fn bug_body(
    fedora: &str,
    rf: &str,
    fedora_evr: Option<&str>,
    states: &[ReleaseState],
    updates: &[String],
    autopush_on: bool,
) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let moving = match fedora_evr {
        Some(evr) => format!("{fedora} {}", without_dist(evr)),
        None => fedora.to_string(),
    };
    // "Counterpart", not "built against": RPM Fusion ships both
    // shapes. libheif-freeworld is an add-on that requires libheif
    // and supplies plugins, while mesa-freeworld is a drop-in
    // replacement that supplies mesa's own libraries. One sentence
    // has to be true of both.
    let _ = writeln!(o, "Fedora is shipping an update for {moving}. RPM Fusion's");
    let _ = writeln!(
        o,
        "{rf} is its counterpart and needs rebuilding to stay in step."
    );
    let _ = writeln!(o);
    match autopush_on {
        false => {
            let _ = writeln!(
                o,
                "The update(s) below have auto-push disabled, so they will not reach"
            );
            let _ = writeln!(o, "stable before {rf} can be rebuilt.");
        }
        // Said rather than glossed: a maintainer deciding how
        // urgently to rebuild needs to know the update is not being
        // held back for them.
        true => {
            let _ = writeln!(
                o,
                "At least one update below still auto-pushes at its karma threshold,"
            );
            let _ = writeln!(o, "so it may reach stable before {rf} is rebuilt.");
        }
    }
    let _ = writeln!(
        o,
        "If RPM Fusion's package is already at the new version, please ignore this."
    );
    let _ = writeln!(o);
    let _ = writeln!(o, "{rf} in RPM Fusion today:");
    let _ = writeln!(o);
    let width = states
        .iter()
        .map(|s| s.release.len())
        .max()
        .unwrap_or(7)
        .max(7);
    let _ = writeln!(
        o,
        "  {:<width$}  {:<20}  {:<20}  dist-git",
        "release", "shipped", "testing"
    );
    for s in states {
        let _ = writeln!(
            o,
            "  {:<width$}  {:<20}  {:<20}  {}",
            s.release,
            s.shipped.as_deref().unwrap_or("-"),
            s.testing.as_deref().unwrap_or("-"),
            s.distgit.as_deref().unwrap_or("no branch"),
        );
    }
    if !updates.is_empty() {
        let _ = writeln!(
            o,
            "
Fedora update(s):
"
        );
        for alias in updates {
            let _ = writeln!(o, "  https://bodhi.fedoraproject.org/updates/{alias}");
        }
    }
    let _ = writeln!(o);
    let _ = writeln!(
        o,
        "Reported by sandogasa ebranch. The shipped column is read from RPM"
    );
    // The remote, not a page: RPM Fusion's cgit answers 404 for
    // every path including its own index, so a bare host URL in a
    // bug sent to them is a broken link. Name the command instead.
    let _ = writeln!(
        o,
        "Fusion's own repositories, updates-testing included; the dist-git"
    );
    let _ = writeln!(o, "column is the Version: in each branch's spec, read with");
    let _ = writeln!(o, "  git clone --bare --depth 1 --no-single-branch \\");
    let _ = writeln!(o, "    {DISTGIT}/{{free,nonfree}}/{rf}.git");
    let _ = writeln!(
        o,
        "A build that exists but has not reached updates-testing appears in"
    );
    let _ = writeln!(o, "none of them, and needs access this tool does not have.");
    o
}

/// The Bugzilla product an update's branch belongs to on RPM
/// Fusion's tracker, whose layout mirrors Red Hat's: `Fedora` and
/// `Fedora EPEL`, component = package name.
pub fn bz_product(branch: &str) -> &'static str {
    match branch.starts_with("epel") {
        true => "Fedora EPEL",
        false => "Fedora",
    }
}

/// The Bugzilla version field, spelled as RPM Fusion's products
/// list it.
///
/// The two products disagree, and both spellings are read from their
/// Bugzilla rather than guessed: `Fedora` carries `f43`, `f44`, `f45`
/// and `rawhide`, while `Fedora EPEL` carries the bare major `9`,
/// `10`. So a Fedora workload passes through unchanged and an `el`
/// one loses its prefix. Sending `43` for Fedora — the release number
/// alone, as this first did — matches no version they offer.
pub fn bz_version(release: &str) -> String {
    release.strip_prefix("el").unwrap_or(release).to_string()
}

/// One package's rebuild request, gathered across a whole batch.
///
/// Three EPEL minor updates are one rebuild for RPM Fusion, so the
/// aliases accumulate and the bug is filed once.
pub struct Rebuild {
    /// The Fedora package whose update prompted this.
    pub fedora: String,
    /// The RPM Fusion package that needs the rebuild.
    pub rf: String,
    /// The version Fedora is moving to, for the summary.
    pub evr: Option<String>,
    /// The branch of the first update that touched it, which picks
    /// the Bugzilla product. The body lists every release anyway.
    pub branch: String,
    /// Every Bodhi update of the batch that touched the package.
    pub aliases: Vec<String>,
    /// Whether any listed update still auto-pushes at its karma
    /// threshold. The bug says what Fedora's side will do, and
    /// promising a hold that is not in place is a promise to
    /// somebody else about our own project.
    pub autopush_on: bool,
}

/// The Bugzilla fields for one rebuild request.
///
/// Separated from the filing so what gets written can be checked
/// without an account: these fields are the part that has to be
/// right, and `file_bugs` around them is a prompt and a POST.
pub fn bug_fields(
    counterparts: &Counterparts,
    rebuild: &Rebuild,
    states: &[ReleaseState],
) -> serde_json::Value {
    let Rebuild {
        fedora,
        rf,
        evr,
        branch,
        aliases,
        autopush_on,
    } = rebuild;
    // A package with no version to name still gets a usable summary
    // rather than one ending in a space.
    let summary = format!(
        "{rf}: rebuild for {fedora} {}",
        evr.as_deref().map(without_dist).unwrap_or_default()
    )
    .trim_end()
    .to_string();
    serde_json::json!({
        "product": bz_product(branch),
        "component": rf,
        "version": bz_version(counterparts.release()),
        "summary": summary,
        "description": bug_body(fedora, rf, evr.as_deref(), states, aliases, *autopush_on),
        // Red Hat's Bugzilla defaults these; RPM Fusion's refuses the
        // create without them ("You must select/enter a OS"). The
        // values are the ones their instance offers, read from
        // `field/bug/op_sys` and `field/bug/rep_platform` rather than
        // guessed: GNU/Linux is the only operating system it lists,
        // and a rebuild request is not about one architecture.
        "op_sys": "GNU/Linux",
        "platform": "All",
    })
}

/// An open rebuild request already on this component, if there is
/// one.
///
/// Filing is not idempotent and nothing else dedupes it, so a second
/// run — or a second person with this tool — would ask RPM Fusion
/// twice for the same work. Their tracker is not ours to litter.
///
/// Matched on the component plus the word "rebuild" in the summary,
/// which finds a request a human wrote as well as one this tool did,
/// while leaving unrelated open bugs on the package alone.
async fn open_rebuild_request(
    bz: &sandogasa_bugzilla::BzClient,
    product: &str,
    rf: &str,
) -> Option<sandogasa_bugzilla::models::Bug> {
    let query = format!(
        "product={}&component={}&status=__open__",
        product.replace(' ', "%20"),
        rf
    );
    bz.search(&query, 50)
        .await
        .ok()?
        .into_iter()
        .find(|b| b.summary.to_lowercase().contains("rebuild"))
}

/// The note added to the review comment posted on a Bodhi update.
///
/// RPM Fusion users meet a Fedora update as a broken install — their
/// counterpart still matches the old version — and say so on the
/// update, sometimes with a -1. Saying it first, on the update
/// itself, is the point: the reader learns that the pairing is known,
/// that a rebuild is being asked for, and where to follow it.
///
/// `filed` maps a Fedora package to its bug URL. A counterpart with
/// no entry is still worth stating, so a run without an account warns
/// without linking rather than staying silent.
pub fn comment_note(
    counterparts: &[(String, String)],
    filed: &BTreeMap<String, String>,
    autopush: Option<bool>,
) -> String {
    use std::fmt::Write as _;
    if counterparts.is_empty() {
        return String::new();
    }
    let mut o = String::from("\n## RPM Fusion\n\n");
    for (fedora, rf) in counterparts {
        let _ = write!(
            o,
            "- `{rf}` is RPM Fusion's counterpart to `{fedora}` and needs \
             rebuilding before this reaches stable"
        );
        match filed.get(fedora) {
            Some(url) => {
                let _ = writeln!(o, " — tracked at {url}.");
            }
            None => {
                let _ = writeln!(
                    o,
                    ". No rebuild request was filed from this run, so it is \
                     not tracked anywhere yet."
                );
            }
        }
    }
    match autopush {
        // Reported, not corrected: an update that already exists
        // belongs to whoever submitted it, and the reader is better
        // placed to act than a tool reaching into their update.
        Some(true) => {
            let _ = writeln!(
                o,
                "\n**Auto-push is still enabled on this update**, so karma alone \
                 can send it to stable before that rebuild lands. Consider \
                 disabling it until then."
            );
        }
        Some(false) => {
            let _ = writeln!(
                o,
                "\nAuto-push is disabled on this update, so it cannot reach \
                 stable on karma before that rebuild lands."
            );
        }
        None => {}
    }
    o
}

/// Whether the newest comment already says exactly this.
///
/// Re-running on an unchanged update would otherwise repeat the
/// table verbatim under the one above it. A body that differs —
/// their repositories moved, another update joined the batch — is
/// worth saying; the same body twice is noise on somebody else's bug.
///
/// The whole body is compared, so changing the wording of the text
/// around the table counts as a change and costs one extra comment
/// the first time a run follows such an edit. Comparing only the
/// state would avoid that, but it would mean parsing a table back
/// out of a comment somebody may have edited, and being wrong in
/// that direction means staying silent when there was something to
/// say.
///
/// Unreadable comments mean "say it": a failed read is not evidence
/// that the thing was already said.
fn said_already(
    bz: &sandogasa_bugzilla::BzClient,
    rt: &tokio::runtime::Runtime,
    bug: u64,
    body: &str,
) -> bool {
    rt.block_on(bz.comments(bug))
        .ok()
        .and_then(|cs| cs.last().map(|c| c.text.trim() == body.trim()))
        .unwrap_or(false)
}

/// Whether to go ahead with a write, asking only when somebody can
/// answer.
///
/// `confirm` reads a line from stdin and takes the default on an
/// empty one, so at EOF — a pipe, a script, a background run — a
/// prompt defaulting to yes silently becomes a yes. That is how a run
/// meant as a preview posted a comment on another project's tracker.
/// Without a terminal the answer is no, and the run says what it
/// skipped.
fn agreed(question: &str, writes: Writes) -> bool {
    if writes.dry_run {
        eprintln!(
            "would, but for --dry-run: {}",
            question.trim_end_matches('?')
        );
        return false;
    }
    if writes.assume_yes {
        return true;
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!(
            "nothing written: \"{}\" needs a terminal to answer, or -y to proceed",
            question.trim_end_matches('?')
        );
        return false;
    }
    sandogasa_cli::confirm(question, true).unwrap_or(false)
}

/// File one bug per package that has an RPM Fusion counterpart.
///
/// Returns the bug's URL for each Fedora package it filed for, so the
/// review comment posted on the update can link it. Writes nothing
/// when RPM Fusion's Bugzilla has no configured key: the point of the
/// guard is the held auto-push, and a missing credential should not
/// fail a submit that already succeeded.
/// How far a run may go.
#[derive(Clone, Copy, Debug, Default)]
pub struct Writes {
    /// Answer the prompts instead of asking.
    pub assume_yes: bool,
    /// Do everything except send: the duplicate check still runs, the
    /// bug or comment is still rendered, and the run says what it
    /// would have written and where.
    ///
    /// There was no way to see this short of having no account
    /// configured, which stopped being a dry run the moment one was
    /// — and that is how a run meant as a preview put a comment on
    /// another project's tracker.
    pub dry_run: bool,
}

pub fn file_bugs(
    counterparts: &Counterparts,
    rebuilds: &[Rebuild],
    writes: Writes,
) -> BTreeMap<String, String> {
    // Resolved before the loop but *not* returned on: without an
    // account the bug is still worth rendering, because seeing what
    // would be filed is how anyone decides whether to configure one.
    // Reading the repositories and the ref list costs a second, and
    // bailing here would spend it and show nothing.
    let client = match crate::config::rpmfusion_bugzilla() {
        None => {
            eprintln!(
                "note: no [rpmfusion-bugzilla] api_key configured — showing what \
                 would be filed, and filing nothing. `ebranch config` sets one. \
                 Any held auto-push stays held."
            );
            None
        }
        Some(cfg) => match sandogasa_bugzilla::BzClient::new(&cfg.url)
            .with_rest_path(&cfg.rest_path)
            .with_stock_auth()
            .with_api_key(cfg.api_key.clone())
        {
            Ok(bz) => match tokio::runtime::Runtime::new() {
                Ok(rt) => Some((bz, rt, cfg)),
                Err(e) => {
                    eprintln!("warning: no async runtime for filing with RPM Fusion: {e}");
                    None
                }
            },
            Err(e) => {
                eprintln!("warning: RPM Fusion Bugzilla URL is not usable with a key: {e}");
                None
            }
        },
    };
    let mut filed = BTreeMap::new();
    for rebuild in rebuilds {
        let rf = &rebuild.rf;
        let releases = counterparts.releases_shipping(rf);
        let states = survey(rf, &releases, &distgit_versions(rf));
        let fields = bug_fields(counterparts, rebuild, &states);
        let said = |key| fields[key].as_str().unwrap_or_default();
        eprintln!("\n{}\n{}", said("summary"), said("description"));
        let Some((bz, rt, cfg)) = client.as_ref() else {
            continue;
        };
        // Already asked for? Then say what has changed since, rather
        // than ask again. One open request per package is what their
        // tracker wants; a second is noise for them and a split
        // conversation for everyone.
        let product = fields["product"].as_str().unwrap_or("Fedora");
        if let Some(open) = rt.block_on(open_rebuild_request(bz, product, rf)) {
            let url = format!("{}/show_bug.cgi?id={}", cfg.url, open.id);
            eprintln!("already open, not filing again: {url} — {}", open.summary);
            // Linked whether or not the comment goes on: the request
            // exists, and the update should point at it.
            filed.insert(rebuild.fedora.clone(), url.clone());
            // Nothing to add if the last thing said is still true.
            // The duplicate check keeps a second *bug* off their
            // tracker; without this, a re-run would still add a
            // comment identical to the one above it.
            if said_already(bz, rt, open.id, said("description")) {
                eprintln!("no change since the last comment on {url}; saying nothing");
                continue;
            }
            if !agreed(&format!("add the above as a comment on {url}?"), writes) {
                continue;
            }
            let comment = serde_json::json!({ "comment": { "body": said("description") } });
            match rt.block_on(bz.update(open.id, &comment)) {
                Ok(()) => eprintln!("commented on {url}"),
                Err(e) => eprintln!("warning: commenting on {url} failed: {e}"),
            }
            continue;
        }
        if !agreed(&format!("file this with RPM Fusion for {rf}?"), writes) {
            continue;
        }
        match rt.block_on(bz.create(&fields)) {
            Ok(resp) => match resp.id {
                Some(id) => {
                    let url = format!("{}/show_bug.cgi?id={id}", cfg.url);
                    eprintln!("filed {url}");
                    filed.insert(rebuild.fedora.clone(), url);
                }
                None => eprintln!(
                    "warning: RPM Fusion did not create a bug for {rf}: {}",
                    resp.message.unwrap_or_else(|| "unknown error".into())
                ),
            },
            Err(e) => eprintln!("warning: filing for {rf} failed: {e}"),
        }
    }
    filed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counterparts(shipped: &[&str], renames: &[(&str, &str)]) -> Counterparts {
        Counterparts {
            shipped: BTreeMap::from([(
                "f43".to_string(),
                shipped.iter().map(|s| s.to_string()).collect(),
            )]),
            renames: renames
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            release: "f43".to_string(),
        }
    }

    #[test]
    fn a_counterpart_is_found_by_name_by_rename_or_by_suffix() {
        // The three shapes measured in rpmfusion-f43.
        let c = counterparts(
            &["ffmpeg", "libheif-freeworld", "vlc-plugins-freeworld"],
            &[("vlc", "vlc-plugins-freeworld")],
        );
        // Same name: Fedora's ffmpeg is stripped, RPM Fusion's is not.
        assert_eq!(c.of("ffmpeg"), Some("ffmpeg"));
        // The suffix rule, which covers 7 of the 11 -freeworld
        // packages without needing a table entry.
        assert_eq!(c.of("libheif"), Some("libheif-freeworld"));
        // The rename table, for the ones it misses — RPM Fusion names
        // the companion after the plugins, not the package.
        assert_eq!(c.of("vlc"), Some("vlc-plugins-freeworld"));
    }

    #[test]
    fn a_guess_counts_only_if_rpm_fusion_ships_it() {
        // This is what lets the suffix rule be generous: `zlib` is not
        // in RPM Fusion, so `zlib-freeworld` is not invented.
        let c = counterparts(&["libheif-freeworld"], &[]);
        assert_eq!(c.of("zlib"), None);
        // A rename pointing at something unshipped is not a match
        // either — a stale table entry cannot resurrect a package.
        let stale = counterparts(&["libheif-freeworld"], &[("vlc", "vlc-plugins-freeworld")]);
        assert_eq!(stale.of("vlc"), None);
        // And a near-miss stays a miss: Fedora's vlc-plugin-bittorrent
        // is not RPM Fusion's vlc-bittorrent.
        let near = counterparts(&["vlc-bittorrent"], &[]);
        assert_eq!(near.of("vlc-plugin-bittorrent"), None);
    }

    #[test]
    fn an_update_reports_every_package_with_a_counterpart() {
        let c = counterparts(&["ffmpeg", "libheif-freeworld"], &[]);
        let packages: Vec<String> = ["libheif", "zlib", "ffmpeg"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            c.in_update(&packages),
            vec![("libheif", "libheif-freeworld"), ("ffmpeg", "ffmpeg")]
        );
        // An update touching nothing of RPM Fusion's is the common
        // case and must come back empty, leaving autopush alone.
        assert!(c.in_update(&["zlib".to_string()]).is_empty());
    }

    #[test]
    fn the_bug_says_what_rpm_fusion_has_rather_than_just_asking() {
        // The point of the table: a maintainer should not have to look
        // up what their own package is at for four releases.
        let states = vec![
            ReleaseState {
                release: "f43".to_string(),
                shipped: Some("1.23.5-1.fc43".to_string()),
                testing: None,
                distgit: Some("1.23.6".to_string()),
            },
            ReleaseState {
                release: "el10".to_string(),
                shipped: None,

                testing: None,
                distgit: None,
            },
        ];
        let body = bug_body(
            "libheif",
            "libheif-freeworld",
            Some("1.23.6-1.fc43"),
            &states,
            &["FEDORA-2026-e9bb998784".to_string()],
            false,
        );
        // The dist tag is dropped: one request covers every release,
        // so naming .fc43 would date the whole bug to one of them.
        assert!(body.contains("libheif 1.23.6-1."), "{body}");
        // Neutral about the relationship: RPM Fusion ships both
        // add-ons (libheif-freeworld requires libheif and supplies
        // plugins) and drop-in replacements (mesa-freeworld supplies
        // mesa's own libraries), so the bug must not claim either.
        assert!(body.contains("is its"), "{body}");
        assert!(body.contains("counterpart"), "{body}");
        assert!(!body.contains("built against"), "{body}");
        assert!(!body.contains("1.23.6-1.fc43"), "{body}");
        assert!(body.contains("1.23.5-1.fc43"), "{body}");
        // The dist-git column carries the version committed there,
        // not merely that a branch exists: a counterpart already at
        // the new version is waiting on a build, and asking for work
        // that is done reads as not having looked.
        assert!(body.contains("1.23.6"), "{body}");
        assert!(body.contains("testing"), "{body}");
        // A release RPM Fusion does not ship reads as absent, not as
        // an empty column.
        assert!(body.contains("no branch"), "{body}");
        assert!(
            body.contains("https://bodhi.fedoraproject.org/updates/FEDORA-2026-e9bb998784"),
            "{body}"
        );
        // Says plainly what it cannot see, so nobody reads the table
        // as "nothing is built". The blind spot is now narrow — only
        // a build that has not reached updates-testing — because the
        // testing column shows the rest.
        assert!(body.contains("has not reached updates-testing"), "{body}");
        // And the provenance names the method actually used: this
        // said `git ls-remote` for a while after the column started
        // coming from a clone of the spec.
        assert!(body.contains("git clone --bare"), "{body}");
        assert!(!body.contains("ls-remote"), "{body}");
        // And it tells them auto-push is already held, which is the
        // reassurance that makes the bug worth reading.
        assert!(body.contains("auto-push disabled"), "{body}");
    }

    #[test]
    fn the_bug_does_not_promise_a_hold_that_is_not_in_place() {
        // --rpmfusion-file can be pointed at an update nobody held
        // back. Claiming otherwise is a promise to another project
        // about our own, and the maintainer reading it is deciding
        // how urgently to rebuild.
        let held = bug_body("libheif", "libheif-freeworld", None, &[], &[], false);
        assert!(held.contains("have auto-push disabled"), "{held}");
        let loose = bug_body("libheif", "libheif-freeworld", None, &[], &[], true);
        assert!(loose.contains("still auto-pushes"), "{loose}");
        assert!(!loose.contains("auto-push disabled"), "{loose}");
        // Either way the "ignore this if already done" line stays.
        assert!(held.contains("please ignore this"), "{held}");
        assert!(loose.contains("please ignore this"), "{loose}");
    }

    #[test]
    fn the_bug_still_reads_when_little_is_known() {
        // No version to name, nothing shipped anywhere, no updates to
        // link: the body must still say what it wants and why, rather
        // than rendering a half-built sentence or an empty table.
        let states = vec![ReleaseState {
            release: "f43".to_string(),
            shipped: None,

            testing: None,
            distgit: None,
        }];
        let body = bug_body("libheif", "libheif-freeworld", None, &states, &[], false);
        assert!(
            body.contains("Fedora is shipping an update for libheif."),
            "{body}"
        );
        assert!(
            body.contains("libheif-freeworld in RPM Fusion today"),
            "{body}"
        );
        assert!(body.contains("no branch"), "{body}");
        // Nothing to link means no empty "Fedora update(s)" heading.
        assert!(!body.contains("Fedora update(s)"), "{body}");
        // An unshipped release shows a placeholder, not a blank.
        assert!(body.contains(" -  "), "{body}");
        // And with no releases at all it still renders a header row.
        let none = bug_body("libheif", "libheif-freeworld", None, &[], &[], false);
        assert!(none.contains("release"), "{none}");
        assert!(none.contains("dist-git"), "{none}");
    }

    #[test]
    fn every_release_shipping_the_counterpart_is_listed() {
        let mut c = counterparts(&["libheif-freeworld"], &[]);
        c.shipped.insert(
            "el10".to_string(),
            ["libheif-freeworld".to_string()].into_iter().collect(),
        );
        c.shipped.insert("f44".to_string(), HashSet::new());
        assert_eq!(
            c.releases_shipping("libheif-freeworld"),
            vec!["el10", "f43"]
        );
        assert!(c.releases_shipping("zlib").is_empty());
    }

    /// An inventory and renames pair on disk, as sync-rpmfusion and
    /// configs/rpmfusion/ produce them.
    fn written(dir: &std::path::Path) -> (String, String) {
        let inventory = dir.join("inventory.toml");
        std::fs::write(
            &inventory,
            r#"
[inventory]
name = "rpmfusion"
description = "Packages synced from RPM Fusion (rpmfusion)"
maintainer = "rpmfusion"
labels = []
private_fields = []

[inventory.workloads.f43]
description = "RPM Fusion packages for f43"
packages = ["ffmpeg", "libheif-freeworld", "mesa-freeworld", "vlc-plugins-freeworld"]

[inventory.workloads.el10]
description = "RPM Fusion packages for el10"
packages = ["ffmpeg", "libheif-freeworld"]

[[package]]
name = "ffmpeg"

[[package]]
name = "libheif-freeworld"

[[package]]
name = "mesa-freeworld"

[[package]]
name = "vlc-plugins-freeworld"
"#,
        )
        .unwrap();
        let renames = dir.join("renames.toml");
        std::fs::write(
            &renames,
            "[renames]
vlc = \"vlc-plugins-freeworld\"
",
        )
        .unwrap();
        (
            inventory.to_string_lossy().into_owned(),
            renames.to_string_lossy().into_owned(),
        )
    }

    #[test]
    fn loading_reads_the_release_asked_for_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let (inventory, renames) = written(dir.path());

        let f43 = Counterparts::load(&inventory, Some(&renames), "f43").unwrap();
        assert_eq!(f43.of("libheif"), Some("libheif-freeworld"));
        assert_eq!(f43.of("vlc"), Some("vlc-plugins-freeworld"));
        assert_eq!(f43.of("mesa"), Some("mesa-freeworld"));
        assert_eq!(f43.release(), "f43");

        // The question is per release: mesa-freeworld ships for f43
        // and not el10, so the same inventory answers differently.
        let el10 = Counterparts::load(&inventory, Some(&renames), "el10").unwrap();
        assert_eq!(el10.of("mesa"), None);
        assert_eq!(el10.of("libheif"), Some("libheif-freeworld"));
        // Every release is still kept, so a bug can list them all.
        assert_eq!(
            el10.releases_shipping("libheif-freeworld"),
            vec!["el10", "f43"]
        );

        // Without the table, the rules still carry most of it — but
        // not the one they cannot reach.
        let bare = Counterparts::load(&inventory, None, "f43").unwrap();
        assert_eq!(bare.of("libheif"), Some("libheif-freeworld"));
        assert_eq!(bare.of("vlc"), None);
    }

    #[test]
    fn a_labelled_workload_answers_for_the_release_it_names() {
        // RPM Fusion calls rawhide "rawhide" in their repo ids and in
        // their Bugzilla's versions, while the update being checked
        // is for f46. The label carries the correspondence, so the
        // check resolves instead of failing every branched-release
        // update until someone re-pins the inventory.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inventory.toml");
        std::fs::write(
            &path,
            r#"
[inventory]
name = "rpmfusion"
description = ""
maintainer = ""
labels = []
private_fields = []

[inventory.workloads.rawhide]
description = "RPM Fusion packages for rawhide"
labels = ["f46"]
packages = ["libheif-freeworld"]

[[package]]
name = "libheif-freeworld"
"#,
        )
        .unwrap();
        let c = Counterparts::load(&path.to_string_lossy(), None, "f46").unwrap();
        assert_eq!(c.of("libheif"), Some("libheif-freeworld"));
        // The workload keeps its own name, which is what the bug's
        // version field needs: their Fedora product offers "rawhide",
        // never "f46".
        assert_eq!(c.release(), "rawhide");
        assert_eq!(bz_version(c.release()), "rawhide");
        // An unlabelled release is still an error, so a stale
        // inventory cannot quietly disable the guard.
        assert!(Counterparts::load(&path.to_string_lossy(), None, "f47").is_err());
    }

    #[test]
    fn an_inventory_without_the_release_says_how_to_fix_it() {
        let dir = tempfile::tempdir().unwrap();
        let (inventory, renames) = written(dir.path());
        // Silently answering "nothing matches" would disable the
        // guard exactly when the inventory needs regenerating, so
        // this is an error that names the releases it does have.
        let err = Counterparts::load(&inventory, Some(&renames), "f99").unwrap_err();
        assert!(err.contains("no packages for f99"), "{err}");
        assert!(err.contains("sync-rpmfusion"), "{err}");
        assert!(err.contains("el10, f43"), "{err}");

        // A missing renames file is the caller's typo, not an empty
        // table: it must not quietly cost the entries.
        let missing =
            Counterparts::load(&inventory, Some("/nonexistent/renames.toml"), "f43").unwrap_err();
        assert!(missing.contains("renames.toml"), "{missing}");

        // An inventory with no workloads at all is the likeliest
        // version of this: a file written before sync-rpmfusion ever
        // ran. Listing "nothing" beats listing an empty string, which
        // reads as a truncated message.
        let empty = dir.path().join("empty.toml");
        std::fs::write(
            &empty,
            "[inventory]\nname = \"rpmfusion\"\ndescription = \"\"\n\
             maintainer = \"\"\nlabels = []\nprivate_fields = []\n",
        )
        .unwrap();
        let bare = Counterparts::load(&empty.to_string_lossy(), None, "f43").unwrap_err();
        assert!(bare.contains("it has: nothing"), "{bare}");
    }

    #[test]
    fn the_bug_goes_to_the_product_the_branch_belongs_to() {
        // RPM Fusion's tracker mirrors Red Hat's layout, so an EPEL
        // update files against Fedora EPEL.
        assert_eq!(bz_product("f43"), "Fedora");
        assert_eq!(bz_product("epel10.4"), "Fedora EPEL");
        // The versions are each product's own spelling, read from
        // their Bugzilla: Fedora offers f43/f44/f45/rawhide and EPEL
        // offers 8/9/10. A bare "43" matches nothing they have.
        assert_eq!(bz_version("f43"), "f43");
        assert_eq!(bz_version("rawhide"), "rawhide");
        assert_eq!(bz_version("el10"), "10");
        assert_eq!(bz_version("el9"), "9");
    }

    #[test]
    fn the_filed_fields_name_the_component_product_and_version() {
        let c = counterparts(&["libheif-freeworld"], &[]);
        let rebuild = Rebuild {
            fedora: "libheif".to_string(),
            rf: "libheif-freeworld".to_string(),
            evr: Some("1.23.6-1.fc43".to_string()),
            branch: "f43".to_string(),
            aliases: vec!["FEDORA-2026-e9bb998784".to_string()],
            autopush_on: false,
        };
        let states = [ReleaseState {
            release: "f43".to_string(),
            shipped: Some("1.23.5-1.fc43".to_string()),
            testing: None,
            distgit: Some("1.23.6".to_string()),
        }];
        let f = bug_fields(&c, &rebuild, &states);
        // The bug is filed against RPM Fusion's package, not Fedora's
        // — filing it against libheif would land on the wrong
        // maintainer.
        assert_eq!(f["component"], "libheif-freeworld");
        assert_eq!(f["product"], "Fedora");
        assert_eq!(f["version"], "f43");
        // Their Bugzilla refuses a create without these two.
        assert_eq!(f["op_sys"], "GNU/Linux");
        assert_eq!(f["platform"], "All");
        assert_eq!(
            f["summary"],
            "libheif-freeworld: rebuild for libheif 1.23.6-1"
        );
        let body = f["description"].as_str().unwrap();
        assert!(body.contains("1.23.5-1.fc43"), "{body}");
        assert!(body.contains("FEDORA-2026-e9bb998784"), "{body}");
    }

    #[test]
    fn a_rebuild_with_no_version_still_gets_a_clean_summary() {
        // Nothing to name means the summary must not trail a space,
        // which Bugzilla keeps verbatim in the bug title.
        let c = counterparts(&["mesa-freeworld"], &[]);
        let rebuild = Rebuild {
            fedora: "mesa".to_string(),
            rf: "mesa-freeworld".to_string(),
            evr: None,
            branch: "epel10.4".to_string(),
            aliases: vec![],
            autopush_on: false,
        };
        let f = bug_fields(&c, &rebuild, &[]);
        assert_eq!(f["summary"], "mesa-freeworld: rebuild for mesa");
        // An EPEL branch files against the EPEL product, not Fedora.
        assert_eq!(f["product"], "Fedora EPEL");
    }

    #[test]
    fn the_update_comment_names_the_counterpart_and_links_its_bug() {
        let found = [("libheif".to_string(), "libheif-freeworld".to_string())];
        let filed = BTreeMap::from([(
            "libheif".to_string(),
            "https://bugzilla.rpmfusion.org/show_bug.cgi?id=7001".to_string(),
        )]);
        let note = comment_note(&found, &filed, Some(false));
        assert!(note.contains("libheif-freeworld"), "{note}");
        assert!(note.contains("show_bug.cgi?id=7001"), "{note}");
        assert!(note.contains("Auto-push is disabled"), "{note}");
    }

    #[test]
    fn the_comment_still_warns_when_no_bug_could_be_filed() {
        // Without an account nothing is filed, but the warning is
        // the useful half: an RPM Fusion user reading the update
        // learns the pairing is known before hitting it.
        let found = [("vlc".to_string(), "vlc-plugins-freeworld".to_string())];
        let note = comment_note(&found, &BTreeMap::new(), None);
        assert!(note.contains("vlc-plugins-freeworld"), "{note}");
        assert!(note.contains("not tracked anywhere yet"), "{note}");
        // Nothing claims an auto-push this run did not disable.
        assert!(!note.contains("Auto-push"), "{note}");
        // And an update with no counterpart gains no section at all.
        assert!(comment_note(&[], &BTreeMap::new(), None).is_empty());
    }

    #[test]
    fn an_update_that_still_auto_pushes_is_called_out() {
        // The tool reports it and leaves it alone: an update that
        // already exists belongs to whoever submitted it.
        let found = [("libheif".to_string(), "libheif-freeworld".to_string())];
        let note = comment_note(&found, &BTreeMap::new(), Some(true));
        assert!(note.contains("Auto-push is still enabled"), "{note}");
        assert!(note.contains("Consider disabling it"), "{note}");
        // Never phrased as something this run did or will do.
        assert!(!note.to_lowercase().contains("disabled it"), "{note}");
    }

    #[test]
    fn a_comment_that_repeats_the_one_above_it_is_not_posted() {
        // The duplicate check keeps a second *bug* off their
        // tracker; this keeps a second identical *comment* off the
        // first one. Re-running on an unchanged update otherwise
        // repeats the table verbatim.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (server, bz) = rt.block_on(async {
            use wiremock::matchers::{method, path};
            use wiremock::{Mock, MockServer, ResponseTemplate};
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/rest/bug/7564/comment"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "bugs": {"7564": {"comments": [
                        {"id": 1, "text": "an older note", "creator": "a",
                         "creation_time": "2026-10-12T19:13:19Z", "is_private": false},
                        {"id": 2, "text": "the current table\n", "creator": "a",
                         "creation_time": "2026-10-12T20:16:12Z", "is_private": false}
                    ]}}
                })))
                .mount(&server)
                .await;
            let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
            (server, bz)
        });
        let _server = server;
        // Matching the newest comment means there is nothing to add,
        // trailing whitespace aside.
        assert!(said_already(&bz, &rt, 7564, "the current table"));
        // Matching an older one does not: the state moved away and
        // back, which is worth saying.
        assert!(!said_already(&bz, &rt, 7564, "an older note"));
        // And anything new is worth saying.
        assert!(!said_already(&bz, &rt, 7564, "their repos moved"));
    }

    #[test]
    fn nothing_is_written_when_there_is_nobody_to_ask() {
        // Tests have no terminal, which is the case this guards:
        // `confirm` returns its default on an empty line, so at EOF a
        // prompt defaulting to yes used to mean yes. A run meant as a
        // preview put a comment on another project's tracker that
        // way.
        assert!(!agreed("file this?", Writes::default()));
        // -y is the deliberate way to do it unattended, and still
        // works without a terminal.
        let yes = Writes {
            assume_yes: true,
            dry_run: false,
        };
        assert!(agreed("file this?", yes));
        // A dry run declines even under -y: it exists to show what
        // would happen, and the one way to check that safely must not
        // depend on getting another flag right.
        let dry = Writes {
            assume_yes: true,
            dry_run: true,
        };
        assert!(!agreed("file this?", dry));
    }

    /// A bare repo at `<base>/<namespace>/<rf>.git` with one branch
    /// per (branch, version) pair, built on disk.
    fn fake_distgit(base: &std::path::Path, namespace: &str, rf: &str, on: &[(&str, &str)]) {
        let work = base.join("work").join(rf);
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str], dir: &std::path::Path| {
            let out = std::process::Command::new("git")
                .args(["-c", "commit.gpgsign=false"])
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .expect("git");
            assert!(out.status.success(), "git {args:?}");
        };
        let (first, rest) = on.split_first().expect("at least one branch");
        git(&["init", "-q", "-b", first.0], &work);
        for (branch, version) in std::iter::once(first).chain(rest) {
            if *branch != first.0 {
                git(&["checkout", "-q", "-b", branch], &work);
            }
            std::fs::write(
                work.join(format!("{rf}.spec")),
                format!("Version: {version}\n"),
            )
            .unwrap();
            git(&["add", "-A"], &work);
            git(&["commit", "-qm", branch], &work);
        }
        let bare = base.join(namespace).join(format!("{rf}.git"));
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        let out = std::process::Command::new("git")
            .args(["clone", "-q", "--bare"])
            .arg(&work)
            .arg(&bare)
            .output()
            .expect("git clone");
        assert!(out.status.success());
    }

    #[test]
    fn the_dist_git_probe_reads_every_branch_and_falls_back_to_nonfree() {
        let dir = tempfile::tempdir().unwrap();
        let base = format!("file://{}", dir.path().display());
        fake_distgit(
            dir.path(),
            "free",
            "libheif-freeworld",
            &[("f43", "1.23.6"), ("el9", "1.23.5")],
        );

        // Every branch, not just the default one: without
        // --no-single-branch the other releases would silently read
        // as having no branch at all.
        let found = distgit_versions_at(&base, "libheif-freeworld");
        assert_eq!(found.get("f43").map(String::as_str), Some("1.23.6"));
        assert_eq!(found.get("el9").map(String::as_str), Some("1.23.5"));

        // A package only RPM Fusion's nonfree half carries is found
        // by the fallback, not missed.
        fake_distgit(dir.path(), "nonfree", "nvidia-thing", &[("f43", "570.1")]);
        let nonfree = distgit_versions_at(&base, "nvidia-thing");
        assert_eq!(nonfree.get("f43").map(String::as_str), Some("570.1"));

        // And a package neither half has reports nothing rather than
        // failing the run.
        assert!(distgit_versions_at(&base, "not-packaged-here").is_empty());
    }

    #[test]
    fn every_branch_of_a_dist_git_reports_its_own_version() {
        // Built on disk rather than fetched: the rule worth guarding
        // is that each branch reports the version *its* spec carries,
        // which is what tells a rebuild already committed from one
        // nobody has started.
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                // Not the packager's config: a global
                // commit.gpgsign would make this fixture try to sign
                // as a user that does not exist, and the test has to
                // pass in a build sandbox too.
                .args(["-c", "commit.gpgsign=false"])
                .arg("-C")
                .arg(&work)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .expect("git")
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        std::fs::create_dir_all(&work).unwrap();
        let spec = work.join("libheif-freeworld.spec");
        std::process::Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["init", "-q", "-b", "f43"])
            .output()
            .expect("git init");
        std::fs::write(&spec, "Name: libheif-freeworld\nVersion: 1.23.6\n").unwrap();
        git(&["add", "libheif-freeworld.spec"]);
        git(&["commit", "-qm", "f43"]);
        git(&["checkout", "-q", "-b", "el9"]);
        std::fs::write(&spec, "Name: libheif-freeworld\nVersion: 1.23.5\n").unwrap();
        git(&["commit", "-qam", "el9"]);

        let found = versions_in(&work.join(".git"), "libheif-freeworld");
        assert_eq!(found.get("f43").map(String::as_str), Some("1.23.6"));
        assert_eq!(found.get("el9").map(String::as_str), Some("1.23.5"));
        // A package the branch does not carry reports nothing rather
        // than an empty version.
        assert!(versions_in(&work.join(".git"), "something-else").is_empty());
    }

    #[test]
    fn a_spec_version_is_read_only_when_it_is_a_plain_one() {
        assert_eq!(
            spec_version("Name: x\nVersion:        1.23.6\nRelease: 1%{?dist}\n").as_deref(),
            Some("1.23.6")
        );
        // A macro is left unread: an unexpanded %{...} in a bug sent
        // to another project is worse than an empty column.
        assert_eq!(spec_version("Version: %{upstream_version}\n"), None);
        assert_eq!(spec_version("Version:\n"), None);
        assert_eq!(spec_version("Name: x\n"), None);
    }

    #[test]
    fn rawhide_is_branched_as_master_in_their_dist_git() {
        // Their ref list has el8/el9/el10 and f36..f45 and master —
        // no f46, no rawhide. Matching on the release name alone
        // printed "no branch" for a package they carry, which is a
        // false claim to send another project.
        assert_eq!(distgit_branch_for("rawhide"), "master");
        assert_eq!(distgit_branch_for("f45"), "f45");
        assert_eq!(distgit_branch_for("el9"), "el9");
    }

    #[test]
    fn a_version_loses_its_dist_tag_but_keeps_its_release() {
        // One request covers six releases, so the summary must not
        // date itself to whichever update was checked first.
        assert_eq!(without_dist("1.23.6-1.fc43"), "1.23.6-1");
        assert_eq!(without_dist("1.23.6-1.el10_3"), "1.23.6-1");
        // A version that carries no dist tag is left alone rather
        // than truncated at the first dot.
        assert_eq!(without_dist("1.23.6-1"), "1.23.6-1");
    }

    #[test]
    fn rawhide_is_reached_by_either_spelling() {
        // A Bodhi rawhide update says "rawhide"; a side tag for the
        // same thing is f46-build-side-NNN and says "f46". Both must
        // reach the one workload RPM Fusion publishes for rawhide.
        assert_eq!(workload_for_branch("rawhide").as_deref(), Some("rawhide"));
        assert_eq!(workload_for_branch("f46").as_deref(), Some("f46"));
    }

    #[test]
    fn a_branch_names_the_release_rpm_fusion_builds_for() {
        assert_eq!(workload_for_branch("f43").as_deref(), Some("f43"));
        // RPM Fusion builds for EL, not EPEL, and spells it so.
        assert_eq!(workload_for_branch("epel10").as_deref(), Some("el10"));
        assert_eq!(workload_for_branch("epel9").as_deref(), Some("el9"));
        // An EPEL minor collapses to the major RPM Fusion builds for:
        // one libheif update arrives as EPEL-10.2, 10.3 and 10.4, and
        // all three face the same free-el-10.
        assert_eq!(workload_for_branch("epel10.3").as_deref(), Some("el10"));
        assert_eq!(workload_for_branch("epel10.4").as_deref(), Some("el10"));
        // Anything unrecognised declines rather than guessing.
        assert_eq!(workload_for_branch("epel"), None);
        assert_eq!(workload_for_branch("f"), None);
    }
}
