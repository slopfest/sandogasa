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
    /// Whether their dist-git has a branch for it.
    pub branched: bool,
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

/// The branches RPM Fusion's dist-git has for a package.
///
/// The namespace is not recorded in the inventory (both halves answer
/// the same question), so free is tried first and nonfree second. A
/// failure is not an error: the table simply says nothing about
/// branches, which is better than failing a submit over a git probe.
/// The branch names in `git ls-remote --heads` output.
///
/// Split out so the parsing is tested without a remote: the ref lines
/// are `<sha>\trefs/heads/<branch>`, and anything else (an advertised
/// capability line, a tag) carries no `refs/heads/` and is dropped.
fn heads(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|l| l.rsplit_once("refs/heads/").map(|(_, b)| b.to_string()))
        .collect()
}

pub fn distgit_branches(rf: &str) -> Vec<String> {
    for namespace in ["free", "nonfree"] {
        let url = format!("{DISTGIT}/{namespace}/{rf}.git");
        let Ok(out) = std::process::Command::new("git")
            .args(["ls-remote", "--heads", &url])
            .output()
        else {
            return Vec::new();
        };
        if !out.status.success() {
            continue;
        }
        let branches = heads(&String::from_utf8_lossy(&out.stdout));
        if !branches.is_empty() {
            return branches;
        }
    }
    Vec::new()
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
/// dist-git is branched for it.
pub fn survey(rf: &str, releases: &[&str], branches: &[String]) -> Vec<ReleaseState> {
    releases
        .iter()
        .map(|release| {
            let fedrq = sandogasa_fedrq::Fedrq {
                branch: Some(format!("rpmfusion-{release}")),
                repo: None,
            };
            let shipped = fedrq
                .pkgs_source_vr(&[rf])
                .ok()
                .and_then(|rows| rows.into_iter().next().map(|(_, vr)| vr));
            ReleaseState {
                release: (*release).to_string(),
                shipped,
                branched: branches.iter().any(|b| b == distgit_branch_for(release)),
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
    let _ = writeln!(o, "  {:<width$}  {:<20}  dist-git", "release", "shipped");
    for s in states {
        let _ = writeln!(
            o,
            "  {:<width$}  {:<20}  {}",
            s.release,
            s.shipped.as_deref().unwrap_or("-"),
            match s.branched {
                true => "branched",
                false => "no branch",
            }
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
    let _ = writeln!(o, "Fusion's own repositories, the dist-git column from");
    let _ = writeln!(o, "  git ls-remote {DISTGIT}/{{free,nonfree}}/{rf}.git");
    let _ = writeln!(
        o,
        "Neither shows a build that exists but has not been released, which"
    );
    let _ = writeln!(o, "needs access this tool does not have.");
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

/// File one bug per package that has an RPM Fusion counterpart.
///
/// Returns the bug's URL for each Fedora package it filed for, so the
/// review comment posted on the update can link it. Writes nothing
/// when RPM Fusion's Bugzilla has no configured key: the point of the
/// guard is the held auto-push, and a missing credential should not
/// fail a submit that already succeeded.
pub fn file_bugs(
    counterparts: &Counterparts,
    rebuilds: &[Rebuild],
    assume_yes: bool,
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
        let branches = distgit_branches(rf);
        let states = survey(rf, &releases, &branches);
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
            if !assume_yes
                && !sandogasa_cli::confirm(&format!("add the above as a comment on {url}?"), true)
                    .unwrap_or(false)
            {
                continue;
            }
            let comment = serde_json::json!({ "comment": { "body": said("description") } });
            match rt.block_on(bz.update(open.id, &comment)) {
                Ok(()) => eprintln!("commented on {url}"),
                Err(e) => eprintln!("warning: commenting on {url} failed: {e}"),
            }
            continue;
        }
        if !assume_yes
            && !sandogasa_cli::confirm(&format!("file this with RPM Fusion for {rf}?"), true)
                .unwrap_or(false)
        {
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
                branched: true,
            },
            ReleaseState {
                release: "el10".to_string(),
                shipped: None,
                branched: false,
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
        assert!(body.contains("branched"), "{body}");
        // A release RPM Fusion does not ship reads as absent, not as
        // an empty column.
        assert!(body.contains("no branch"), "{body}");
        assert!(
            body.contains("https://bodhi.fedoraproject.org/updates/FEDORA-2026-e9bb998784"),
            "{body}"
        );
        // Says plainly what it cannot see, so nobody reads the table
        // as "nothing is built".
        assert!(body.contains("has not been released"), "{body}");
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
            branched: false,
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
            branched: true,
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
    fn only_heads_are_read_from_a_ref_listing() {
        // What `git ls-remote --heads` prints, plus the kind of line
        // that is not a branch: neither a tag nor a capability line
        // may be reported as one.
        let out = "\
a1b2c3\trefs/heads/f43\n\
d4e5f6\trefs/heads/el10\n\
99aabb\trefs/tags/v1.0\n";
        assert_eq!(heads(out), ["f43", "el10"]);
        assert!(heads("").is_empty());
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
