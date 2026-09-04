// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `kondo` subcommand.
//!
//! Triage the packages in the inventory that no *essential* inventory
//! needs — the set difference between "packages I maintain" and the
//! union of inventories that justify maintaining something (work
//! inventories, and the dependency inventories `deps` generates).
//! Inventories are the classification mechanism throughout: a package
//! is essential precisely because some inventory contains it.
//!
//! Each candidate is resolved interactively with the shared
//! keep/explain/remove flow, with kondo's reading of the verbs: the
//! finding is "nothing essential needs this package", so **keep**
//! accepts it (the package stays a cull candidate), **explain** files
//! the package into another inventory — the explanation *is* the
//! inventory path, and recording it means adding the package there,
//! which makes the justification durable as membership rather than
//! prose — and **remove** drops a false positive (the analysis missed
//! a real need). A remove is a temporary skip: it is not persisted,
//! and the candidate returns next run until the gap in the essential
//! inputs is fixed. Non-interactive runs keep every candidate.
//!
//! What remains culled is then classified by the user's own dist-git
//! access, because the access level routes the action: a main admin
//! (owner) can orphan a package (`sandogasa-pkg-acl give orphan`), an
//! admin can remove their own ACL (`sandogasa-pkg-acl remove`), and a
//! committer, collaborator or ticket holder has to ask. The report
//! groups candidates accordingly, ready to announce to the mailing
//! list *before* anything acts on it — kondo itself never touches
//! dist-git ACLs.

use std::collections::{BTreeMap, BTreeSet};

use sandogasa_distgit::{AccessLevel, DistGitClient};
use sandogasa_inventory::{Inventory, InventoryMeta, Package};
use sandogasa_review::Resolution;
use serde::Serialize;

use crate::triage_retired::{RETRY_ATTEMPTS, retry};

/// Where a cull candidate lands after the access-level pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Main admin: `sandogasa-pkg-acl give orphan` works.
    Orphan,
    /// Admin: can remove their own ACL.
    SelfRemove,
    /// Commit, collaborator or ticket: has to ask to be removed.
    Ask,
    /// Access could not be determined (lookup failed, no direct
    /// access recorded, or the project is gone).
    Unknown,
}

/// The prompt's vocabulary for the triage: the finding is "nothing
/// essential needs this", so confirming it means culling — `(c)ull`,
/// not `(k)eep`, which reads as the opposite of what happens — and
/// explaining it means naming the inventory that makes the package
/// essential: `(e)ssential [e <inventory>]`; and removing it only
/// leaves the candidate undecided for this run — `(s)kip`.
pub const CULL_VOCABULARY: sandogasa_review::Vocabulary = sandogasa_review::Vocabulary {
    keep: sandogasa_review::Word::new('c', "cull"),
    explain: sandogasa_review::Word::new('e', "essential"),
    remove: sandogasa_review::Word::new('s', "skip"),
    explanation: "inventory",
};

/// One triaged-and-classified cull candidate.
#[derive(Debug, Clone, Serialize)]
pub struct Culled {
    pub name: String,
    /// The user's own access level, as dist-git reports it
    /// (`owner`, `admin`, `commit`, `collaborator`, `ticket`) —
    /// or a note when it could not be determined.
    pub level: String,
    pub action: Action,
    /// Why this one is being culled, in the user's words — typed at
    /// the prompt as `c <note>`; carried into the cull file's
    /// `reason`.
    pub note: Option<String>,
}

/// The whole run's outcome, for `--json` and the report.
#[derive(Debug, Default, Serialize)]
pub struct KondoReport {
    /// Personal packages not found in any essential inventory.
    pub candidates: usize,
    /// Candidates skipped because a previous pass already culled
    /// them (they are in the `-o` inventory).
    pub previously_culled: usize,
    /// Packages removed from the `-o` inventory because they have
    /// become essential since they were culled.
    pub rescued: Vec<String>,
    /// Confirmed cullable, classified by access.
    pub culled: Vec<Culled>,
    /// Filed into another inventory: (package, inventory path).
    pub explained: Vec<(String, String)>,
    /// False positives dropped at the prompt.
    pub removed: Vec<String>,
    pub warnings: Vec<String>,
}

/// The set difference: packages of `personal` (skipping ones already
/// marked unshipped — they have nothing left to cull) that appear in
/// no essential inventory.
pub fn cull_candidates(personal: &Inventory, essential: &BTreeSet<String>) -> Vec<String> {
    personal
        .package
        .iter()
        .filter(|p| !p.is_unshipped() && !essential.contains(&p.name))
        .map(|p| p.name.clone())
        .collect()
}

/// Add `name` to the inventory at `path`, creating the file if it
/// does not exist yet (named after its file stem, maintained by
/// `maintainer`). Returns `false` when the package was already there.
pub fn file_into_inventory(path: &str, name: &str, maintainer: &str) -> Result<bool, String> {
    let mut inventory = if std::path::Path::new(path).exists() {
        sandogasa_inventory::load(path)?
    } else {
        Inventory {
            inventory: InventoryMeta {
                name: std::path::Path::new(path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("kondo-kept")
                    .to_string(),
                description: "Packages kept during kondo triage".to_string(),
                maintainer: maintainer.to_string(),
                labels: Vec::new(),
                workloads: BTreeMap::new(),
                private_fields: Vec::new(),
            },
            package: Vec::new(),
        }
    };
    if inventory.package.iter().any(|p| p.name == name) {
        return Ok(false);
    }
    inventory.package.push(Package {
        name: name.to_string(),
        ..Default::default()
    });
    sandogasa_inventory::save(&inventory, path)?;
    Ok(true)
}

/// Fold the prompt's resolutions into the report, filing explained
/// packages into their inventories as we go (immediately, so an
/// interrupted triage loses nothing already decided). Candidates
/// arrive already classified — [`classify`] runs before the prompt
/// so the access level can inform the decision — and the kept ones
/// land in `report.culled` as they are.
pub fn apply_resolutions(
    resolutions: Vec<(Culled, Resolution, Option<String>)>,
    maintainer: &str,
    report: &mut KondoReport,
) {
    for (mut candidate, resolution, note) in resolutions {
        match resolution {
            Resolution::Keep => {
                candidate.note = note;
                report.culled.push(candidate);
            }
            Resolution::Explained(path) => {
                match file_into_inventory(&path, &candidate.name, maintainer) {
                    Ok(true) => report.explained.push((candidate.name, path)),
                    Ok(false) => {
                        report.warnings.push(format!(
                            "{}: already in {path}; nothing added",
                            candidate.name
                        ));
                        report.explained.push((candidate.name, path));
                    }
                    Err(e) => {
                        // Do not lose the decision: an unfilable
                        // package stays a candidate rather than
                        // vanishing.
                        report.warnings.push(format!(
                            "{}: could not add to {path}: {e}; kept",
                            candidate.name
                        ));
                        report.culled.push(candidate);
                    }
                }
            }
            Resolution::Removed => report.removed.push(candidate.name),
        }
    }
}

/// The prompt line for one candidate: the access level gives the
/// decision its context — being a mere committer reads differently
/// from owning the package.
pub fn triage_summary(candidate: &Culled) -> String {
    format!(
        "{} ({}) — nothing essential needs it",
        candidate.name, candidate.level
    )
}

/// The essential paths to actually load: a brand-new `--explain-into`
/// file may legitimately be listed among them — filing into the same
/// inventory the diff subtracts is the natural multi-pass idiom, and
/// the snapshot semantics keep it safe (candidates are computed once,
/// before anything is filed) — but on the first pass the file does
/// not exist yet, and failing to load it would block exactly that
/// flow. A *missing* essential file that is not the explain target
/// stays an error: silently treating a typo as an empty inventory
/// would make every package it names look cullable.
pub fn essential_paths_to_load(
    paths: &[String],
    explain_into: Option<&str>,
) -> (Vec<String>, Option<String>) {
    let mut note = None;
    let kept = paths
        .iter()
        .filter(|p| {
            let is_new_target =
                Some(p.as_str()) == explain_into && !std::path::Path::new(p).exists();
            if is_new_target {
                note = Some(format!(
                    "{p} does not exist yet; treating it as empty (it is the \
                     --explain-into target)"
                ));
            }
            !is_new_target
        })
        .cloned()
        .collect();
    (kept, note)
}

/// Map a direct access level to the action it allows.
fn action_for(level: Option<AccessLevel>) -> Action {
    match level {
        Some(AccessLevel::Owner) => Action::Orphan,
        Some(AccessLevel::Admin) => Action::SelfRemove,
        Some(_) => Action::Ask,
        None => Action::Unknown,
    }
}

/// One remembered ACL answer. Only successful lookups are cached —
/// an error or an absent project retries next run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedLevel {
    pub level: String,
    /// Unix seconds when dist-git answered.
    pub checked: u64,
}

/// ACL answers age on human timescales — ownership changes are rare
/// events — so remembering them for a day is honest and turns each
/// triage sitting's pre-prompt wait into a file read.
pub const ACL_CACHE_TTL_SECS: u64 = 24 * 60 * 60;

/// The per-user ACL answer cache, one JSON file under the XDG cache
/// directory. Disposable by definition: deleting it only costs the
/// next run its lookups, and concurrent kondo sessions may clobber
/// each other's *cache* writes harmlessly (last one wins).
#[derive(Debug, Default)]
pub struct AclCache {
    path: Option<std::path::PathBuf>,
    /// user → package → answer.
    entries: BTreeMap<String, BTreeMap<String, CachedLevel>>,
}

impl AclCache {
    /// Load the cache from `path`, tolerating absence and decay: a
    /// missing or unreadable file is an empty cache, never an error.
    pub fn load(path: Option<std::path::PathBuf>) -> Self {
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        Self { path, entries }
    }

    /// The default location: `~/.cache/poi-tracker/acl-levels.json`
    /// via the XDG rules; `None` (cache disabled) when no cache
    /// directory can be determined.
    pub fn default_path() -> Option<std::path::PathBuf> {
        dirs::cache_dir().map(|d| d.join("poi-tracker").join("acl-levels.json"))
    }

    /// A still-fresh remembered level for (`user`, `package`).
    pub fn fresh(&self, user: &str, package: &str, now: u64) -> Option<&CachedLevel> {
        self.entries
            .get(user)?
            .get(package)
            .filter(|c| now.saturating_sub(c.checked) < ACL_CACHE_TTL_SECS)
    }

    /// Remember a successful lookup.
    pub fn remember(&mut self, user: &str, package: &str, level: String, now: u64) {
        self.entries.entry(user.to_string()).or_default().insert(
            package.to_string(),
            CachedLevel {
                level,
                checked: now,
            },
        );
    }

    /// Write the cache back; failure costs only the next run's
    /// lookups, so it warns rather than errs.
    pub fn save(&self) -> Option<String> {
        let path = self.path.as_ref()?;
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, serde_json::to_vec(&self.entries).unwrap_or_default())
        };
        write()
            .err()
            .map(|e| format!("could not save the ACL cache to {}: {e}", path.display()))
    }
}

/// Rebuild a [`Culled`] from a cached level string.
pub fn culled_from_level(name: &str, level: &str) -> Culled {
    let parsed: Option<AccessLevel> = level.parse().ok();
    Culled {
        name: name.to_string(),
        level: level.to_string(),
        action: match level {
            "owner" => Action::Orphan,
            _ => action_for(parsed),
        },
        note: None,
    }
}

/// How many ACL lookups fly at once. Latency-bound requests against
/// src.fedoraproject.org, so a handful in flight cuts a 900-package
/// pass from minutes to well under one without leaning on the server.
const CLASSIFY_CONCURRENCY: usize = 5;

/// Look up the user's own (direct) access level on each culled
/// package, [`CLASSIFY_CONCURRENCY`] requests at a time, preserving
/// input order. Group access is deliberately not consulted: the
/// personal inventory was synced with `--no-groups`, and a group
/// grant is not the user's to walk away from anyway.
pub async fn classify(
    dg: &DistGitClient,
    user: &str,
    packages: &[String],
    verbose: bool,
) -> (Vec<Culled>, Vec<String>) {
    let mut culled = Vec::with_capacity(packages.len());
    let mut warnings = Vec::new();
    for chunk in packages.chunks(CLASSIFY_CONCURRENCY) {
        let mut set = tokio::task::JoinSet::new();
        for (i, name) in chunk.iter().enumerate() {
            let dg = dg.clone();
            let name = name.clone();
            let user = user.to_string();
            set.spawn(async move {
                if verbose {
                    eprintln!("[kondo] {name}: checking dist-git access");
                }
                let looked_up = retry(
                    &format!("ACLs for {name}"),
                    RETRY_ATTEMPTS,
                    // Stringly-typed at the task boundary: the
                    // client's boxed error is not Send, and the
                    // joined task's output must be.
                    || async { dg.get_acls(&name).await.map_err(|e| e.to_string()) },
                    verbose,
                )
                .await;
                let (entry, warning) = match looked_up {
                    Ok(acls) => {
                        let level = if acls.access_users.owner.contains(&user) {
                            Some(AccessLevel::Owner)
                        } else {
                            acls.user_level(&user)
                        };
                        (
                            Culled {
                                name,
                                level: level.map_or_else(|| "none".to_string(), |l| l.to_string()),
                                action: action_for(level),
                                note: None,
                            },
                            None,
                        )
                    }
                    Err(e) => (
                        Culled {
                            name: name.clone(),
                            level: "unknown".to_string(),
                            action: Action::Unknown,
                            note: None,
                        },
                        Some(format!("{name}: could not check access: {e}")),
                    ),
                };
                (i, entry, warning)
            });
        }
        // Completion order is arbitrary; the index restores input
        // order so reports and prompts stay deterministic.
        let mut slots: Vec<Option<(Culled, Option<String>)>> =
            (0..chunk.len()).map(|_| None).collect();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((i, entry, warning)) => slots[i] = Some((entry, warning)),
                Err(e) => warnings.push(format!("access lookup task failed: {e}")),
            }
        }
        for (i, slot) in slots.into_iter().enumerate() {
            match slot {
                Some((entry, warning)) => {
                    culled.push(entry);
                    warnings.extend(warning);
                }
                None => {
                    // The task panicked (reported above); keep the
                    // package rather than losing it.
                    culled.push(Culled {
                        name: chunk[i].clone(),
                        level: "unknown".to_string(),
                        action: Action::Unknown,
                        note: None,
                    });
                }
            }
        }
    }
    (culled, warnings)
}

/// Render the grouped, announcement-ready report.
pub fn format_report(report: &KondoReport, user: &str) -> String {
    let mut out = String::new();
    let group = |action: Action| -> Vec<&Culled> {
        report
            .culled
            .iter()
            .filter(|c| c.action == action)
            .collect()
    };

    let orphan = group(Action::Orphan);
    let self_remove = group(Action::SelfRemove);
    let ask = group(Action::Ask);
    let unknown = group(Action::Unknown);

    out.push_str(&format!(
        "{} cull candidate(s) for {user}: {} orphanable, {} self-removable, \
         {} to ask about, {} unknown\n",
        report.culled.len(),
        orphan.len(),
        self_remove.len(),
        ask.len(),
        unknown.len(),
    ));
    if !orphan.is_empty() {
        out.push_str("\nowner — can be orphaned directly:\n");
        for c in &orphan {
            out.push_str(&format!("  {}\n", c.name));
        }
    }
    if !self_remove.is_empty() {
        out.push_str("\nadmin — can remove own ACL:\n");
        for c in &self_remove {
            out.push_str(&format!("  {}\n", c.name));
        }
    }
    if !ask.is_empty() {
        out.push_str("\ncommit/collaborator/ticket — ask to be removed:\n");
        for c in &ask {
            out.push_str(&format!("  {} ({})\n", c.name, c.level));
        }
    }
    if !unknown.is_empty() {
        out.push_str("\naccess unknown — check by hand:\n");
        for c in &unknown {
            out.push_str(&format!("  {} ({})\n", c.name, c.level));
        }
    }
    out
}

/// Remove from the cull inventory at `path` every package that has
/// become essential: regenerated essential inputs — a `deps --build`
/// run justifying a crate stack, say — can overtake a standing
/// verdict, and a stale "cullable" would otherwise survive until
/// someone hand-edited the file. Returns the rescued names; an
/// absent file rescues nothing.
pub fn rescue_culled(path: &str, essential: &BTreeSet<String>) -> Result<Vec<String>, String> {
    if !std::path::Path::new(path).exists() {
        return Ok(Vec::new());
    }
    let mut inventory = sandogasa_inventory::load(path)?;
    let before = inventory.package.len();
    let mut rescued = Vec::new();
    inventory.package.retain(|p| {
        if essential.contains(&p.name) {
            rescued.push(p.name.clone());
            false
        } else {
            true
        }
    });
    if inventory.package.len() != before {
        sandogasa_inventory::save(&inventory, path)?;
    }
    Ok(rescued)
}

/// The names already verdicted into the cull inventory at `path` —
/// empty when the file does not exist yet. `-o` is the accumulated
/// verdict across passes, so a candidate found here was already
/// decided and is not asked about again; without this, every re-run
/// re-prompted for the entire standing cull list.
pub fn prior_culled(path: &str) -> Result<BTreeSet<String>, String> {
    if !std::path::Path::new(path).exists() {
        return Ok(BTreeSet::new());
    }
    Ok(sandogasa_inventory::load(path)?
        .package
        .into_iter()
        .map(|p| p.name)
        .collect())
}

/// Merge the culled set into the inventory at `path`, creating it if
/// absent — `-o` accumulates across passes, because triage happens in
/// sittings (one `--pattern` at a time) and each pass owns only its
/// slice of the verdict. Packages already present are left untouched;
/// new ones carry their access level in `reason`, appended to the
/// per-package note, the run's `--reason`, or the stock wording — in
/// that order. Returns how many were added.
///
/// The same load-modify-save caveat as explain filing applies: two
/// *simultaneously finishing* runs can drop each other's additions,
/// so concurrent sessions should write distinct files and let a later
/// pass merge them.
pub fn merge_culled(
    path: &str,
    report: &KondoReport,
    name: &str,
    maintainer: &str,
    reason: Option<&str>,
) -> Result<usize, String> {
    let mut inventory = if std::path::Path::new(path).exists() {
        sandogasa_inventory::load(path)?
    } else {
        Inventory {
            inventory: to_inventory(report, name, maintainer, reason).inventory,
            package: Vec::new(),
        }
    };
    let existing: BTreeSet<String> = inventory.package.iter().map(|p| p.name.clone()).collect();
    let mut added = 0;
    for pkg in to_inventory(report, name, maintainer, reason).package {
        if !existing.contains(&pkg.name) {
            inventory.package.push(pkg);
            added += 1;
        }
    }
    inventory.package.sort_by(|a, b| a.name.cmp(&b.name));
    sandogasa_inventory::save(&inventory, path)?;
    Ok(added)
}

/// The culled set as an inventory: something to keep beside the
/// announcement, and to diff after the next triage. Each entry's
/// `reason` is the package's own prompt note when one was typed,
/// else `reason` (the run's `--reason`), else stock wording — always
/// with the access level appended.
pub fn to_inventory(
    report: &KondoReport,
    name: &str,
    maintainer: &str,
    reason: Option<&str>,
) -> Inventory {
    Inventory {
        inventory: InventoryMeta {
            name: name.to_string(),
            description: "Cull candidates from poi-tracker kondo".to_string(),
            maintainer: maintainer.to_string(),
            labels: Vec::new(),
            workloads: BTreeMap::new(),
            private_fields: Vec::new(),
        },
        package: report
            .culled
            .iter()
            .map(|c| Package {
                name: c.name.clone(),
                reason: Some(format!(
                    "{} ({})",
                    c.note
                        .as_deref()
                        .or(reason)
                        .unwrap_or("kondo cull candidate"),
                    c.level
                )),
                ..Default::default()
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use sandogasa_distgit::DistGitClient;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn inventory(names: &[&str]) -> Inventory {
        let mut toml =
            String::from("[inventory]\nname = \"t\"\ndescription = \"t\"\nmaintainer = \"t\"\n");
        for name in names {
            toml.push_str(&format!("\n[[package]]\nname = \"{name}\"\n"));
        }
        toml::from_str(&toml).unwrap()
    }

    #[test]
    fn candidates_are_the_set_difference() {
        let personal = inventory(&["a", "b", "c"]);
        let essential: BTreeSet<String> = ["b".to_string()].into();
        assert_eq!(cull_candidates(&personal, &essential), ["a", "c"]);
    }

    #[test]
    fn unshipped_packages_are_not_candidates() {
        let mut personal = inventory(&["a", "b"]);
        personal.package[0].unshipped = Some("retired everywhere".to_string());
        assert_eq!(cull_candidates(&personal, &BTreeSet::new()), ["b"]);
    }

    fn culled(name: &str, level: &str) -> Culled {
        Culled {
            name: name.to_string(),
            level: level.to_string(),
            action: Action::Ask,
            note: None,
        }
    }

    #[test]
    fn resolutions_fan_out_and_explained_files_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("favorites.toml");
        let dest = dest.to_str().unwrap();

        let mut report = KondoReport::default();
        apply_resolutions(
            vec![
                (culled("a", "owner"), Resolution::Keep, None),
                (
                    culled("b", "commit"),
                    Resolution::Explained(dest.to_string()),
                    None,
                ),
                (culled("c", "commit"), Resolution::Removed, None),
                (
                    culled("d", "admin"),
                    Resolution::Explained(dest.to_string()),
                    None,
                ),
            ],
            "me",
            &mut report,
        );
        let kept: Vec<&str> = report.culled.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(kept, ["a"]);
        assert_eq!(report.culled[0].level, "owner");
        assert_eq!(report.removed, ["c"]);
        assert_eq!(report.explained.len(), 2);

        let filed = sandogasa_inventory::load(dest).unwrap();
        assert_eq!(filed.inventory.name, "favorites");
        assert_eq!(filed.inventory.maintainer, "me");
        let names: Vec<&str> = filed.package.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["b", "d"]);
    }

    #[test]
    fn triage_summary_carries_the_level() {
        assert_eq!(
            triage_summary(&culled("old-toy", "commit")),
            "old-toy (commit) — nothing essential needs it"
        );
    }

    #[test]
    fn a_new_explain_target_is_tolerated_as_essential() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("new.toml");
        let missing = missing.to_str().unwrap().to_string();
        let other_missing = dir.path().join("typo.toml");
        let other_missing = other_missing.to_str().unwrap().to_string();

        // The nonexistent --explain-into target is dropped with a
        // note; a nonexistent path that is NOT the target stays in
        // the list, so loading it still fails loudly.
        let (kept, note) =
            essential_paths_to_load(&[missing.clone(), other_missing.clone()], Some(&missing));
        assert_eq!(kept, std::slice::from_ref(&other_missing));
        assert!(note.unwrap().contains("does not exist yet"));

        // An existing explain-into target is loaded like any other.
        std::fs::write(&missing, "").unwrap();
        let (kept, note) = essential_paths_to_load(std::slice::from_ref(&missing), Some(&missing));
        assert_eq!(kept, [missing]);
        assert!(note.is_none());
    }

    #[test]
    fn filing_twice_is_reported_not_duplicated() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("kept.toml");
        let dest = dest.to_str().unwrap();
        assert!(file_into_inventory(dest, "a", "me").unwrap());
        assert!(!file_into_inventory(dest, "a", "me").unwrap());
        assert_eq!(sandogasa_inventory::load(dest).unwrap().package.len(), 1);
    }

    fn acls_json(owner: &str, admin: &[&str], commit: &[&str]) -> serde_json::Value {
        serde_json::json!({
            "access_users": {
                "owner": [owner], "admin": admin, "commit": commit,
                "collaborator": [], "ticket": []
            },
            "access_groups": {
                "admin": [], "commit": [], "collaborator": [], "ticket": []
            },
            "name": "x",
            "namespace": "rpms"
        })
    }

    #[tokio::test]
    async fn classification_routes_by_access_level() {
        let server = MockServer::start().await;
        for (pkg, acls) in [
            ("mine", acls_json("me", &[], &[])),
            ("admined", acls_json("boss", &["me"], &[])),
            ("committed", acls_json("boss", &[], &["me"])),
            ("theirs", acls_json("boss", &[], &[])),
        ] {
            Mock::given(method("GET"))
                .and(path(format!("/api/0/rpms/{pkg}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(acls))
                .mount(&server)
                .await;
        }
        let dg = DistGitClient::with_base_url(&server.uri());
        let pkgs: Vec<String> = ["mine", "admined", "committed", "theirs"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (culled, warnings) = classify(&dg, "me", &pkgs, false).await;
        assert!(warnings.is_empty());
        let actions: Vec<Action> = culled.iter().map(|c| c.action).collect();
        assert_eq!(
            actions,
            [
                Action::Orphan,
                Action::SelfRemove,
                Action::Ask,
                Action::Unknown
            ]
        );
        assert_eq!(culled[3].level, "none");
    }

    #[test]
    fn acl_cache_round_trips_respects_ttl_and_survives_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acl-levels.json");

        let mut cache = AclCache::load(Some(path.clone()));
        assert!(cache.fresh("me", "htop", 1000).is_none());
        cache.remember("me", "htop", "commit".to_string(), 1000);
        assert!(cache.save().is_none());

        let cache = AclCache::load(Some(path.clone()));
        // Fresh within the TTL, stale past it, per-user keyed.
        assert_eq!(cache.fresh("me", "htop", 1000).unwrap().level, "commit");
        assert!(
            cache
                .fresh("me", "htop", 1000 + ACL_CACHE_TTL_SECS)
                .is_none()
        );
        assert!(cache.fresh("someone-else", "htop", 1000).is_none());

        // A corrupt or missing file is an empty cache, never an error.
        std::fs::write(&path, b"not json").unwrap();
        assert!(
            AclCache::load(Some(path))
                .fresh("me", "htop", 1000)
                .is_none()
        );
        assert!(AclCache::load(None).fresh("me", "htop", 1000).is_none());
    }

    #[test]
    fn cached_levels_rebuild_the_same_actions() {
        for (level, action) in [
            ("owner", Action::Orphan),
            ("admin", Action::SelfRemove),
            ("commit", Action::Ask),
            ("collaborator", Action::Ask),
            ("ticket", Action::Ask),
            ("none", Action::Unknown),
        ] {
            assert_eq!(culled_from_level("x", level).action, action, "{level}");
        }
    }

    #[test]
    fn newly_essential_packages_are_rescued_from_the_cull_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("cull.toml");
        let dest = dest.to_str().unwrap();
        let pass = KondoReport {
            culled: vec![culled("rust-clap", "owner"), culled("old-toy", "owner")],
            ..Default::default()
        };
        merge_culled(dest, &pass, "cull", "me", None).unwrap();

        // deps --build later justifies rust-clap: it leaves the
        // verdict, old-toy stays condemned.
        let essential: BTreeSet<String> = ["rust-clap".to_string()].into();
        assert_eq!(rescue_culled(dest, &essential).unwrap(), ["rust-clap"]);
        let names: Vec<String> = prior_culled(dest).unwrap().into_iter().collect();
        assert_eq!(names, ["old-toy"]);

        // Nothing left to rescue; the absent-file case is also calm.
        assert!(rescue_culled(dest, &essential).unwrap().is_empty());
        let gone = dir.path().join("never.toml");
        assert!(
            rescue_culled(gone.to_str().unwrap(), &essential)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn prior_culled_is_empty_for_a_new_file_and_full_after_a_pass() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("cull.toml");
        let dest = dest.to_str().unwrap();
        assert!(prior_culled(dest).unwrap().is_empty());

        let pass = KondoReport {
            culled: vec![culled("a-thing", "owner")],
            ..Default::default()
        };
        merge_culled(dest, &pass, "cull", "me", None).unwrap();
        assert!(prior_culled(dest).unwrap().contains("a-thing"));
    }

    #[test]
    fn cull_output_accumulates_across_passes() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("cull.toml");
        let dest = dest.to_str().unwrap();

        let pass1 = KondoReport {
            culled: vec![culled("a-thing", "owner")],
            ..Default::default()
        };
        assert_eq!(merge_culled(dest, &pass1, "cull", "me", None).unwrap(), 1);

        // Second pass: one overlap (untouched), one new.
        let pass2 = KondoReport {
            culled: vec![culled("a-thing", "commit"), culled("rust-old", "owner")],
            ..Default::default()
        };
        assert_eq!(merge_culled(dest, &pass2, "cull", "me", None).unwrap(), 1);

        let merged = sandogasa_inventory::load(dest).unwrap();
        let names: Vec<&str> = merged.package.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a-thing", "rust-old"]);
        // The overlap kept its first-pass reason (owner, not commit).
        assert_eq!(
            merged.package[0].reason.as_deref(),
            Some("kondo cull candidate (owner)")
        );
    }

    #[test]
    fn cull_reasons_prefer_the_note_then_the_flag_then_stock_wording() {
        let noted = Culled {
            note: Some("superseded by uv".to_string()),
            ..culled("python-pipx", "owner")
        };
        let report = KondoReport {
            culled: vec![noted, culled("old-toy", "commit")],
            ..Default::default()
        };
        let with_flag = to_inventory(&report, "cull", "me", Some("2026 spring clean"));
        assert_eq!(
            with_flag.package[0].reason.as_deref(),
            Some("superseded by uv (owner)")
        );
        assert_eq!(
            with_flag.package[1].reason.as_deref(),
            Some("2026 spring clean (commit)")
        );
        let bare = to_inventory(&report, "cull", "me", None);
        assert_eq!(
            bare.package[1].reason.as_deref(),
            Some("kondo cull candidate (commit)")
        );
    }

    #[test]
    fn the_report_carries_no_commands() {
        let report = KondoReport {
            culled: vec![
                Culled {
                    name: "old-toy".into(),
                    level: "owner".into(),
                    action: Action::Orphan,
                    note: None,
                },
                Culled {
                    name: "gone-lib".into(),
                    level: "owner".into(),
                    action: Action::Orphan,
                    note: None,
                },
                Culled {
                    name: "helped-once".into(),
                    level: "admin".into(),
                    action: Action::SelfRemove,
                    note: None,
                },
            ],
            ..Default::default()
        };
        // The report names the packages but carries no commands —
        // enacting is `act`'s job, one confirmed prompt at a time.
        let text = format_report(&report, "me");
        assert!(!text.contains("sandogasa-pkg-acl"));
        assert!(text.contains("old-toy"));
        assert!(text.contains("helped-once"));
    }

    #[test]
    fn report_groups_by_action() {
        let report = KondoReport {
            candidates: 3,
            culled: vec![
                Culled {
                    name: "old-toy".into(),
                    level: "owner".into(),
                    action: Action::Orphan,
                    note: None,
                },
                Culled {
                    name: "helped-once".into(),
                    level: "commit".into(),
                    action: Action::Ask,
                    note: None,
                },
            ],
            ..Default::default()
        };
        let text = format_report(&report, "me");
        assert!(text.contains("owner — can be orphaned directly:\n  old-toy"));
        assert!(text.contains("helped-once (commit)"));
    }
}
