// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `update-bugs`: revisit the bugs an update carries without running
//! `check-update`'s analysis again (#113).
//!
//! `check-update` keeps the report of each check it runs, keyed by what
//! it checked (a side tag, a Bodhi alias) and by the aliases of the
//! updates it submitted or voted on, under
//! `$XDG_STATE_HOME/ebranch/check-update/`. `update-bugs` reads that
//! report back, refuses it when the update's builds have changed since,
//! and reruns only the bug proposal: what the update looks like it
//! closes, and the bugs on it a build from before it had already met
//! (#112). The edit is previewed in full and saved on confirmation.
//!
//! `check-update --kept` reads the same report back in place of a new
//! analysis, to vote or submit from it while the builds are unchanged.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::check_update::{CheckUpdateReport, Finding};

/// The format a kept check is written in, so a later ebranch can tell a
/// file it does not understand from a damaged one.
const KEPT_FORMAT: u32 = 1;

/// A check `check-update` ran, kept for `update-bugs` and later runs.
#[derive(Debug, Serialize, Deserialize)]
pub struct KeptCheck {
    pub format: u32,
    /// The builds the report judged, as Koji NVRs, sorted: what an
    /// update must still carry for the report to speak for it.
    pub nvrs: Vec<String>,
    /// The report as the analysis left it, before the reviewer's
    /// decisions: so a later run can take a decision back.
    pub report: CheckUpdateReport,
    /// What the reviewer decided about the findings they did not keep.
    /// Absent from a check kept before `--kept` existed.
    #[serde(default)]
    pub decisions: Vec<(Finding, Decision)>,
    /// When the analysis ran, which a `--kept` run carries over rather
    /// than resetting. Absent from a check kept before `--kept`.
    #[serde(default)]
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A reviewer's decision about a finding they did not keep, as kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Accepted, with why: posted in "addressed by the reviewer".
    Explained(String),
    /// A false positive, dropped.
    Removed,
}

impl Decision {
    /// The decision as `sandogasa_review` resolves it, to apply again.
    pub fn resolution(&self) -> sandogasa_review::Resolution {
        match self {
            Decision::Explained(why) => sandogasa_review::Resolution::Explained(why.clone()),
            Decision::Removed => sandogasa_review::Resolution::Removed,
        }
    }

    /// A reviewer's resolution as kept; `None` for one they kept, which
    /// leaves the report as it was.
    pub fn of(resolution: &sandogasa_review::Resolution) -> Option<Self> {
        match resolution {
            sandogasa_review::Resolution::Keep => None,
            sandogasa_review::Resolution::Explained(why) => Some(Decision::Explained(why.clone())),
            sandogasa_review::Resolution::Removed => Some(Decision::Removed),
        }
    }
}

impl KeptCheck {
    /// The report with the reviewer's decisions applied to it.
    pub fn curated(&self) -> CheckUpdateReport {
        let resolutions = self
            .decisions
            .iter()
            .map(|(f, d)| (f.clone(), d.resolution()))
            .collect();
        crate::check_update::apply_resolutions(self.report.clone(), resolutions).0
    }

    pub fn new(
        report: &CheckUpdateReport,
        decisions: &[(Finding, Decision)],
        checked_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        KeptCheck {
            format: KEPT_FORMAT,
            nvrs: report_nvrs(report),
            report: report.clone(),
            decisions: decisions.to_vec(),
            checked_at: Some(checked_at),
        }
    }
}

/// What the reviewer decided, one line each, for showing.
pub fn decisions_text(decisions: &[(Finding, Decision)]) -> String {
    decisions
        .iter()
        .map(|(f, d)| match d {
            Decision::Explained(why) => format!("  explained: {} ({why})\n", f.summary()),
            Decision::Removed => format!("  removed: {}\n", f.summary()),
        })
        .collect()
}

/// Where kept checks live: `$XDG_STATE_HOME/ebranch/check-update/`.
pub fn kept_dir() -> Result<PathBuf, String> {
    dirs::state_dir()
        .map(|d| d.join("ebranch").join("check-update"))
        .ok_or_else(|| {
            "cannot locate the state directory: neither $XDG_STATE_HOME nor $HOME is set"
                .to_string()
        })
}

/// The file a check kept under `key` is in: the key with anything but
/// letters, digits, `.`, `_` and `-` replaced, so a list of NVRs or a
/// COPR name cannot reach outside `dir`.
fn kept_file(dir: &Path, key: &str) -> PathBuf {
    let safe: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{safe}.json"))
}

/// The builds a report judged, as Koji NVRs (no epoch), sorted.
pub fn report_nvrs(report: &CheckUpdateReport) -> Vec<String> {
    let mut nvrs: Vec<String> = report
        .changes
        .iter()
        .map(|c| {
            let vr = c.new.split_once(':').map_or(c.new.as_str(), |(_, vr)| vr);
            format!("{}-{vr}", c.package)
        })
        .collect();
    nvrs.sort();
    nvrs.dedup();
    nvrs
}

/// Keep `check` under each of `keys` in `dir`. Each file is written
/// whole and then moved into place, so a run interrupted mid-write
/// leaves the previous one.
pub fn keep(dir: &Path, keys: &[String], check: &KeptCheck) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(check).map_err(|e| e.to_string())?;
    for key in keys.iter().collect::<BTreeSet<_>>() {
        let file = kept_file(dir, key);
        let partial = file.with_extension("json.partial");
        std::fs::write(&partial, &text)
            .and_then(|()| std::fs::rename(&partial, &file))
            .map_err(|e| format!("cannot write {}: {e}", file.display()))?;
    }
    Ok(())
}

/// The check kept under `key` in `dir`: `None` when there is none, an
/// error when the file cannot be read as one. A check kept before the
/// time was kept gets the file's, so it keeps it when written again.
pub fn load(dir: &Path, key: &str) -> Option<Result<KeptCheck, String>> {
    let file = kept_file(dir, key);
    let text = std::fs::read_to_string(&file).ok()?;
    let written = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from);
    Some(
        serde_json::from_str::<KeptCheck>(&text)
            .map_err(|e| format!("{} is not a check ebranch can read: {e}", file.display()))
            .and_then(|k| {
                if k.format == KEPT_FORMAT {
                    Ok(k)
                } else {
                    Err(format!(
                        "{} was written in format {}, this ebranch reads {KEPT_FORMAT}",
                        file.display(),
                        k.format
                    ))
                }
            })
            .map(|mut k| {
                k.checked_at = k.checked_at.or(written);
                k
            }),
    )
}

/// What differs between the builds a kept check judged and the ones the
/// update carries now, or `None` when they are the same: a report about
/// other builds cannot say what this update closes.
pub fn builds_changed(judged: &[String], now: &[String]) -> Option<String> {
    let judged: BTreeSet<&str> = judged.iter().map(String::as_str).collect();
    let now: BTreeSet<&str> = now.iter().map(String::as_str).collect();
    if judged == now {
        return None;
    }
    let mut parts = Vec::new();
    let added: Vec<&str> = now.difference(&judged).copied().collect();
    let gone: Vec<&str> = judged.difference(&now).copied().collect();
    if !added.is_empty() {
        parts.push(format!("now also carries {}", added.join(", ")));
    }
    if !gone.is_empty() {
        parts.push(format!("no longer carries {}", gone.join(", ")));
    }
    Some(parts.join("; "))
}

/// The newest check kept under one of `keys` in `dir` that judged the
/// builds `now`: every key is tried, as a check kept under an update's
/// alias can be older than a later one of its side tag, or stale while
/// the tag's is not. The error says what was found instead, when no
/// kept check fits.
fn pick_kept(dir: &Path, keys: &[&str], now: &[String]) -> Result<KeptCheck, String> {
    let mut found = Vec::new();
    let mut fits: Vec<KeptCheck> = Vec::new();
    for key in keys {
        match load(dir, key) {
            None => {}
            Some(Ok(kept)) => match builds_changed(&kept.nvrs, now) {
                None => fits.push(kept),
                Some(what) => found.push(format!(
                    "the check kept as {key} is out of date: the update {what}"
                )),
            },
            Some(Err(e)) => found.push(e),
        }
    }
    if let Some(newest) = fits.into_iter().max_by_key(|k| k.checked_at) {
        return Ok(newest);
    }
    if found.is_empty() {
        Err(format!(
            "no check of {} is kept in {}",
            keys[0],
            dir.display()
        ))
    } else {
        Err(found.join("; "))
    }
}

/// How long ago a check was kept, roughly: "12 min", "3 h", "2 days".
pub fn age_text(age: std::time::Duration) -> String {
    let min = age.as_secs() / 60;
    match min {
        0 => "under a minute".to_string(),
        1..60 => format!("{min} min"),
        60..2880 => format!("{} h", min / 60),
        _ => format!("{} days", min / 1440),
    }
}

/// The check `check-update --kept` uses in place of a new analysis: the
/// newest one kept under one of `keys` in `dir` that judged the builds
/// `now`, with how long ago the analysis ran. The error says what was
/// found instead and how to get a fresh one.
pub fn reuse(
    dir: &Path,
    keys: &[&str],
    now: &[String],
) -> Result<(KeptCheck, Option<std::time::Duration>), String> {
    let kept = pick_kept(dir, keys, now)
        .map_err(|e| format!("{e}: run it again without --kept for a fresh analysis"))?;
    let age = kept
        .checked_at
        .and_then(|at| (chrono::Utc::now() - at).to_std().ok());
    Ok((kept, age))
}

/// What a kept check of `update` is looked for under, and the builds it
/// must have judged: the update's alias and its side tag, and its NVRs,
/// sorted.
pub fn update_keys_and_builds(
    update: &sandogasa_bodhi::models::Update,
) -> (Vec<String>, Vec<String>) {
    let keys = std::iter::once(update.alias.clone())
        .chain(update.from_tag.clone())
        .collect();
    let mut now: Vec<String> = update.builds.iter().map(|b| b.nvr.clone()).collect();
    now.sort();
    (keys, now)
}

/// The edit as it will be saved: the bug list and whether Bodhi closes
/// the bugs, each with what it was.
pub fn plan_text(
    alias: &str,
    before: &[u64],
    after: &[u64],
    close_before: bool,
    close_after: bool,
) -> String {
    let list = |bugs: &[u64]| {
        if bugs.is_empty() {
            "none".to_string()
        } else {
            bugs.iter()
                .map(|b| format!("#{b}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    let mut out = format!("\nEdit to {alias}:\n");
    if before == after {
        out.push_str(&format!("  bugs: {} (unchanged)\n", list(after)));
    } else {
        out.push_str(&format!("  bugs: {}\n", list(after)));
        out.push_str(&format!("    was: {}\n", list(before)));
    }
    if close_before == close_after {
        out.push_str(&format!(
            "  closed when stable: {} (unchanged)\n",
            yes_no(close_after)
        ));
    } else {
        out.push_str(&format!(
            "  closed when stable: {} (was {})\n",
            yes_no(close_after),
            yes_no(close_before)
        ));
    }
    out
}

/// What `update-bugs` was asked to do.
pub struct Options {
    pub alias: String,
    /// `--close-bugs` / `--no-close-bugs`; `None` asks, or keeps what
    /// the update has under `--yes`.
    pub close_bugs: Option<bool>,
    pub dry_run: bool,
    pub yes: bool,
    pub koji_profile: Option<String>,
    pub verbose: bool,
    /// Whether questions can be asked: stdin is a terminal.
    pub interactive: bool,
    /// Where the update is read and edited, and its bugs read.
    pub bodhi_url: String,
    pub bugzilla_url: String,
    /// Where kept checks are; `None` is [`kept_dir`].
    pub state_dir: Option<PathBuf>,
}

/// Revisit the bugs of the update `opts.alias` from its kept check.
pub fn run(opts: &Options) -> Result<(), String> {
    let interactive = opts.interactive;
    if !interactive && !opts.yes && !opts.dry_run {
        return Err(
            "update-bugs asks before changing an update: run it at a terminal, \
             or pass --dry-run to see the edit, or --yes to save it unasked"
                .to_string(),
        );
    }
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let bodhi = sandogasa_bodhi::BodhiClient::with_base_url(&opts.bodhi_url);
    let update = rt
        .block_on(bodhi.update_by_alias(&opts.alias))
        .map_err(|e| format!("could not read {} from Bodhi: {e}", opts.alias))?;

    let dir = match &opts.state_dir {
        Some(dir) => dir.clone(),
        None => kept_dir()?,
    };
    let (keys, now) = update_keys_and_builds(&update);
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    let kept = pick_kept(&dir, &keys, &now).map_err(|e| {
        format!(
            "{e}: run `ebranch check-update {}`",
            update.from_tag.as_deref().unwrap_or(&opts.alias)
        )
    })?;

    let current: Vec<u64> = update.bugs.iter().map(|b| b.bug_id).collect();
    let bz = sandogasa_bugzilla::BzClient::new(&opts.bugzilla_url);
    // With the reviewer's decisions applied, as check-update voted on it.
    let report = &kept.curated();
    let proposal = rt.block_on(crate::submit::gather_bugs(
        &bz,
        report,
        &[],
        opts.koji_profile.as_deref(),
        opts.verbose,
    ));

    eprintln!(
        "\n{} carries {}",
        update.alias,
        if update.bugs.is_empty() {
            "no bugs".to_string()
        } else {
            format!("{} bug(s):", update.bugs.len())
        }
    );
    for bug in &update.bugs {
        eprintln!(
            "  #{} {}",
            bug.bug_id,
            bug.title.as_deref().unwrap_or("(no title in Bodhi)")
        );
    }

    // Taken only at a terminal: attaching a bug closes it when the
    // update goes stable, and taking one off undoes what someone chose,
    // neither of which `--yes` does unasked.
    let ask_each = |question: &str, each: &str, shown: &[String]| -> Result<Vec<usize>, String> {
        if opts.yes || !interactive {
            eprintln!("(not taken without a terminal; --yes saves only the flags given)");
            return Ok(Vec::new());
        }
        sandogasa_cli::confirm_each(question, each, shown, true).map_err(|e| e.to_string())
    };

    // Bugs on the update a build from before it had already met: theirs
    // to close, offered for removal (#112).
    let met: Vec<&crate::discover::Candidate> = proposal
        .already_met
        .iter()
        .filter(|c| current.contains(&c.bug_id))
        .collect();
    let mut removed: Vec<u64> = Vec::new();
    if !met.is_empty() {
        let shown: Vec<String> = met.iter().map(|c| c.shown()).collect();
        eprintln!("\nOn the update, but already met before it (theirs to close):");
        for line in &shown {
            eprintln!("{line}");
        }
        removed = ask_each(
            "Take these bugs off the update?",
            "Take this bug off?",
            &shown,
        )?
        .into_iter()
        .map(|i| met[i].bug_id)
        .collect();
    }

    let new: Vec<&crate::discover::Candidate> = proposal
        .candidates
        .iter()
        .filter(|c| !current.contains(&c.bug_id))
        .collect();
    let mut added: Vec<u64> = Vec::new();
    if !new.is_empty() {
        let shown: Vec<String> = new.iter().map(|c| c.shown()).collect();
        eprintln!("\nThe update also looks like it closes:");
        for line in &shown {
            eprintln!("{line}");
        }
        added = ask_each("Add these bugs to the update?", "Add this bug?", &shown)?
            .into_iter()
            .map(|i| new[i].bug_id)
            .collect();
    }

    let mut bugs: Vec<u64> = current
        .iter()
        .copied()
        .filter(|b| !removed.contains(b))
        .chain(added)
        .collect();
    bugs.dedup();
    if !bugs.is_empty() {
        bugs = rt.block_on(crate::submit::screen_bugs(
            &bz,
            &report.updated_packages,
            report.dist_branch(),
            &bugs,
            opts.yes || !interactive,
        ))?;
    }

    let close_bugs = match opts.close_bugs {
        Some(c) => c,
        None if opts.yes || !interactive || bugs.is_empty() => update.close_bugs,
        None => sandogasa_cli::confirm(
            "Close these bugs when the update goes stable?",
            update.close_bugs,
        )
        .map_err(|e| e.to_string())?,
    };

    if bugs == current && close_bugs == update.close_bugs {
        eprintln!("\nNothing to change on {}.", update.alias);
        return Ok(());
    }
    eprint!(
        "{}",
        plan_text(
            &update.alias,
            &current,
            &bugs,
            update.close_bugs,
            close_bugs
        )
    );
    if opts.dry_run {
        eprintln!("Dry run: {} is unchanged.", update.alias);
        return Ok(());
    }
    if !opts.yes && !sandogasa_cli::confirm("Save this edit?", true).map_err(|e| e.to_string())? {
        eprintln!("{} is unchanged.", update.alias);
        return Ok(());
    }
    // Bodhi saves a side tag update by its tag and refreshes the builds
    // from it, so a build tagged since would join an update no check
    // judged it in.
    if let Some(tag) = &update.from_tag {
        let tagged = crate::check_update::koji_list_tagged(tag, opts.koji_profile.as_deref())?;
        let extra: Vec<&String> = tagged.iter().filter(|n| !now.contains(n)).collect();
        if !extra.is_empty() {
            return Err(format!(
                "{tag} now also holds {}, which saving {} would add to it; \
                 run `ebranch check-update {tag}` again first",
                extra
                    .iter()
                    .map(|n| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                update.alias
            ));
        }
    }

    rt.block_on(async {
        use sandogasa_bodhi::auth;
        let http = crate::karma::http_client();
        let token =
            auth::cli_session_token_refreshed(&http, &auth::cli_cache_path(), auth::FEDORA_IDP)
                .await?;
        let client = sandogasa_bodhi::BodhiClient::with_base_url(&opts.bodhi_url)
            .with_token(token)
            .map_err(|e| e.to_string())?;
        let resp = client
            .edit_update_bugs(&update.alias, &bugs, close_bugs)
            .await
            .map_err(|e| e.to_string())?;
        for caveat in &resp.caveats {
            eprintln!("note from bodhi: {}", caveat.description);
        }
        eprintln!("edited: {}/updates/{}", opts.bodhi_url, update.alias);
        Ok::<(), String>(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check_update::PackageChange;

    fn report_of(changes: &[(&str, Option<&str>, &str)]) -> CheckUpdateReport {
        let mut report: CheckUpdateReport = serde_json::from_value(serde_json::json!({
            "input": "f46-build-side-158330",
            "branch": "rawhide",
            "updated_packages": [],
            "full_analysis": true,
            "reverse_deps": {}
        }))
        .unwrap();
        report.changes = changes
            .iter()
            .map(|(p, old, new)| PackageChange {
                package: p.to_string(),
                old: old.map(str::to_string),
                new: new.to_string(),
            })
            .collect();
        report
    }

    #[test]
    fn a_check_is_kept_under_each_key_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let report = report_of(&[
            ("libical", Some("3.0.20-1.fc46"), "4.0.7-1.fc46"),
            ("asterisk", Some("24.0.0-3.fc46"), "24.0.0-3.fc46.1"),
            ("perl-Foo", None, "1:2.0-1.fc46"),
        ]);
        let check = KeptCheck::new(&report, &[], chrono::Utc::now());
        // Koji's NVRs carry no epoch, and are kept sorted.
        assert_eq!(
            check.nvrs,
            [
                "asterisk-24.0.0-3.fc46.1",
                "libical-4.0.7-1.fc46",
                "perl-Foo-2.0-1.fc46"
            ]
        );
        let keys = [
            "f46-build-side-158330".to_string(),
            "FEDORA-2026-0b2e1b3c7d".to_string(),
        ];
        keep(dir.path(), &keys, &check).unwrap();
        for key in &keys {
            let back = load(dir.path(), key).unwrap().unwrap();
            assert_eq!(back.nvrs, check.nvrs);
            assert_eq!(back.report.changes.len(), 3);
        }
        assert!(load(dir.path(), "FEDORA-2026-other").is_none());
        // No partial file is left behind.
        let names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| n.ends_with(".json")), "{names:?}");
    }

    #[test]
    fn a_key_cannot_reach_outside_the_directory() {
        let dir = Path::new("/state/ebranch/check-update");
        assert_eq!(
            kept_file(dir, "../../etc/passwd"),
            dir.join(".._.._etc_passwd.json")
        );
        assert_eq!(
            kept_file(dir, "foo-1.0-1.fc46 bar-2.0-1.fc46"),
            dir.join("foo-1.0-1.fc46_bar-2.0-1.fc46.json")
        );
    }

    #[test]
    fn a_kept_check_ebranch_cannot_read_is_an_error_not_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("FEDORA-2026-x.json"), "not json").unwrap();
        let err = load(dir.path(), "FEDORA-2026-x").unwrap().unwrap_err();
        assert!(err.contains("is not a check ebranch can read"), "{err}");

        let mut check = KeptCheck::new(&report_of(&[]), &[], chrono::Utc::now());
        check.format = KEPT_FORMAT + 1;
        keep(dir.path(), &["FEDORA-2026-y".to_string()], &check).unwrap();
        let err = load(dir.path(), "FEDORA-2026-y").unwrap().unwrap_err();
        assert!(err.contains("format 2"), "{err}");
    }

    #[test]
    fn a_report_about_other_builds_is_refused() {
        let judged = ["a-1-1.fc46".to_string(), "b-1-1.fc46".to_string()];
        assert_eq!(builds_changed(&judged, &judged.clone()), None);
        let now = ["a-1-2.fc46".to_string(), "b-1-1.fc46".to_string()];
        assert_eq!(
            builds_changed(&judged, &now).as_deref(),
            Some("now also carries a-1-2.fc46; no longer carries a-1-1.fc46")
        );
    }

    fn opts_at(server: &str, dir: &Path) -> Options {
        Options {
            alias: "FEDORA-2026-c8b6a481c4".to_string(),
            close_bugs: None,
            dry_run: true,
            yes: false,
            koji_profile: None,
            verbose: false,
            interactive: false,
            bodhi_url: server.to_string(),
            bugzilla_url: server.to_string(),
            state_dir: Some(dir.to_path_buf()),
        }
    }

    /// A Bodhi update of `nvrs` from the libical side tag, carrying
    /// `bugs`, closed on stable or not.
    async fn serve_update(server: &wiremock::MockServer, nvrs: &[&str], bugs: &[u64], close: bool) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};
        let builds: Vec<_> = nvrs.iter().map(|n| serde_json::json!({"nvr": n})).collect();
        let bugs: Vec<_> = bugs
            .iter()
            .map(|b| serde_json::json!({"bug_id": b, "title": "libical-4.0.7 is available"}))
            .collect();
        Mock::given(method("GET"))
            .and(path("/updates/FEDORA-2026-c8b6a481c4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "update": {
                    "alias": "FEDORA-2026-c8b6a481c4",
                    "status": "testing",
                    "from_tag": "f46-build-side-158330",
                    "close_bugs": close,
                    "builds": builds,
                    "bugs": bugs
                }
            })))
            .mount(server)
            .await;
    }

    /// A check of the side tag kept in `dir`, judging no package
    /// changes, so the proposal asks nothing of Koji or Bugzilla.
    fn keep_side_tag_check(dir: &Path, nvrs: &[&str]) {
        let mut check = KeptCheck::new(&report_of(&[]), &[], chrono::Utc::now());
        check.nvrs = nvrs.iter().map(|n| n.to_string()).collect();
        keep(dir, &["f46-build-side-158330".to_string()], &check).unwrap();
    }

    #[test]
    fn the_kept_check_that_judged_these_builds_is_the_one_used() {
        let dir = tempfile::tempdir().unwrap();
        let now = vec!["libical-4.0.7-2.fc46".to_string()];
        // Kept at submit under the alias, then the side tag was checked
        // again after a rebuild: the alias's is stale, the tag's fits.
        let mut stale = KeptCheck::new(&report_of(&[]), &[], chrono::Utc::now());
        stale.nvrs = vec!["libical-4.0.7-1.fc46".to_string()];
        keep(dir.path(), &["FEDORA-2026-c8b6a481c4".to_string()], &stale).unwrap();
        let mut fresh = KeptCheck::new(&report_of(&[]), &[], chrono::Utc::now());
        fresh.nvrs = now.clone();
        keep(dir.path(), &["f46-build-side-158330".to_string()], &fresh).unwrap();
        let keys = ["FEDORA-2026-c8b6a481c4", "f46-build-side-158330"];
        assert_eq!(pick_kept(dir.path(), &keys, &now).unwrap().nvrs, now);
        // Neither fits: each says why.
        let err = pick_kept(dir.path(), &keys, &["libical-4.0.7-3.fc46".to_string()]).unwrap_err();
        assert!(
            err.contains("kept as FEDORA-2026-c8b6a481c4 is out of date")
                && err.contains("kept as f46-build-side-158330 is out of date"),
            "{err}"
        );
        // Nothing kept at all.
        let empty = tempfile::tempdir().unwrap();
        let err = pick_kept(empty.path(), &keys, &now).unwrap_err();
        assert!(
            err.contains("no check of FEDORA-2026-c8b6a481c4 is kept"),
            "{err}"
        );
    }

    #[test]
    fn a_change_needs_a_terminal_or_a_dry_run_or_yes() {
        let dir = tempfile::tempdir().unwrap();
        let opts = Options {
            dry_run: false,
            ..opts_at("http://127.0.0.1:9", dir.path())
        };
        let err = run(&opts).unwrap_err();
        assert!(err.contains("run it at a terminal"), "{err}");
    }

    #[test]
    fn an_update_with_no_kept_check_says_how_to_make_one() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(wiremock::MockServer::start());
        rt.block_on(serve_update(&server, &["libical-4.0.7-1.fc46"], &[], false));
        let dir = tempfile::tempdir().unwrap();
        let err = run(&opts_at(&server.uri(), dir.path())).unwrap_err();
        assert!(
            err.contains("no check of FEDORA-2026-c8b6a481c4 is kept")
                && err.contains("run `ebranch check-update f46-build-side-158330`"),
            "{err}"
        );
    }

    #[test]
    fn a_check_of_other_builds_is_refused() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(wiremock::MockServer::start());
        rt.block_on(serve_update(&server, &["libical-4.0.7-2.fc46"], &[], false));
        let dir = tempfile::tempdir().unwrap();
        // Found by the update's side tag, as the check was of that.
        keep_side_tag_check(dir.path(), &["libical-4.0.7-1.fc46"]);
        let err = run(&opts_at(&server.uri(), dir.path())).unwrap_err();
        assert!(
            err.contains("out of date")
                && err.contains("now also carries libical-4.0.7-2.fc46")
                && err.contains("no longer carries libical-4.0.7-1.fc46")
                && err.contains("run `ebranch check-update f46-build-side-158330`"),
            "{err}"
        );
    }

    #[test]
    fn a_dry_run_changes_nothing_on_bodhi() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, ResponseTemplate};

        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(wiremock::MockServer::start());
        rt.block_on(async {
            serve_update(&server, &["libical-4.0.7-1.fc46"], &[2458837], true).await;
            // The bug is screened, and belongs.
            Mock::given(method("GET"))
                .and(path("/rest/bug"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "bugs": [{
                        "id": 2458837,
                        "summary": "libical-4.0.7 is available",
                        "component": ["libical"],
                        "status": "NEW",
                        "product": "Fedora",
                        "version": ["rawhide"]
                    }]
                })))
                .mount(&server)
                .await;
            // Nothing may be saved.
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(500))
                .expect(0)
                .mount(&server)
                .await;
        });
        let dir = tempfile::tempdir().unwrap();
        keep_side_tag_check(dir.path(), &["libical-4.0.7-1.fc46"]);
        // As it is: nothing to change.
        run(&opts_at(&server.uri(), dir.path())).unwrap();
        // Asked to leave the bugs open: shown, and still not saved.
        let opts = Options {
            close_bugs: Some(false),
            ..opts_at(&server.uri(), dir.path())
        };
        run(&opts).unwrap();
    }

    #[test]
    fn the_plan_shows_what_changes_and_what_it_was() {
        let text = plan_text(
            "FEDORA-2026-x",
            &[2458837, 2567306],
            &[2458837],
            false,
            false,
        );
        assert!(text.contains("Edit to FEDORA-2026-x:"), "{text}");
        assert!(
            text.contains("  bugs: #2458837\n    was: #2458837, #2567306"),
            "{text}"
        );
        assert!(
            text.contains("closed when stable: no (unchanged)"),
            "{text}"
        );
        let text = plan_text("FEDORA-2026-x", &[1], &[1], false, true);
        assert!(text.contains("bugs: #1 (unchanged)"), "{text}");
        assert!(text.contains("closed when stable: yes (was no)"), "{text}");
        assert!(plan_text("A", &[1], &[], true, true).contains("bugs: none"));
    }

    /// `check-update --kept` takes a check only for the builds it
    /// judged, and gets back what the reviewer explained with it.
    #[test]
    fn a_kept_check_is_reused_with_its_explanations_for_the_same_builds() {
        let dir = tempfile::tempdir().unwrap();
        let report = report_of(&[("libical", Some("3.0.20-1.fc46"), "4.0.7-1.fc46")]);
        let finding = Finding::NotNewest(crate::check_update::NotNewest {
            nvr: "libical-4.0.7-1.fc46".into(),
            target_nvr: "libical-4.0.7-1.fc46".into(),
            tag: "f46-updates-candidate".into(),
        });
        let decisions = vec![(finding, Decision::Explained("tagged by hand".into()))];
        // Checked two days ago: the age says so, however recently the
        // file was written.
        let at = chrono::Utc::now() - chrono::Duration::hours(49);
        let check = KeptCheck::new(&report, &decisions, at);
        keep(dir.path(), &["f46-build-side-158330".to_string()], &check).unwrap();

        let now = vec!["libical-4.0.7-1.fc46".to_string()];
        let (back, age) = reuse(dir.path(), &["f46-build-side-158330"], &now).unwrap();
        assert_eq!(back.decisions, decisions);
        assert_eq!(age_text(age.unwrap()), "2 days");
        assert!(
            decisions_text(&back.decisions)
                .contains("explained: `libical-4.0.7-1.fc46` is not newer")
                && decisions_text(&back.decisions).contains("(tagged by hand)"),
            "{}",
            decisions_text(&back.decisions)
        );

        let err = reuse(
            dir.path(),
            &["f46-build-side-158330"],
            &["libical-4.0.7-2.fc46".to_string()],
        )
        .unwrap_err();
        assert!(
            err.contains("now also carries libical-4.0.7-2.fc46")
                && err.contains("run it again without --kept"),
            "{err}"
        );
    }

    /// A check kept before decisions and times were kept reads, with no
    /// decisions, and its age is the file's.
    #[test]
    fn a_check_kept_without_decisions_or_time_reads() {
        let dir = tempfile::tempdir().unwrap();
        let mut old =
            serde_json::to_value(KeptCheck::new(&report_of(&[]), &[], chrono::Utc::now())).unwrap();
        old.as_object_mut().unwrap().remove("decisions");
        old.as_object_mut().unwrap().remove("checked_at");
        std::fs::write(dir.path().join("FEDORA-2026-x.json"), old.to_string()).unwrap();
        let (back, age) = reuse(dir.path(), &["FEDORA-2026-x"], &[]).unwrap();
        assert!(back.decisions.is_empty());
        // The file's time, which a rewrite then keeps.
        assert!(back.checked_at.is_some());
        assert!(age.is_some_and(|a| a.as_secs() < 600), "{age:?}");
    }

    #[test]
    fn a_decision_is_kept_as_the_resolution_it_was() {
        use sandogasa_review::Resolution;
        assert_eq!(Decision::of(&Resolution::Keep), None);
        for r in [Resolution::Explained("why".into()), Resolution::Removed] {
            assert_eq!(Decision::of(&r).unwrap().resolution(), r);
        }
    }

    #[test]
    fn a_checks_age_reads_in_the_unit_that_fits() {
        use std::time::Duration;
        assert_eq!(age_text(Duration::from_secs(30)), "under a minute");
        assert_eq!(age_text(Duration::from_secs(12 * 60)), "12 min");
        assert_eq!(age_text(Duration::from_secs(3 * 3600 + 59)), "3 h");
        assert_eq!(age_text(Duration::from_secs(47 * 3600)), "47 h");
        assert_eq!(age_text(Duration::from_secs(3 * 86400)), "3 days");
    }

    /// Two kept checks of the same builds: the newer analysis is used,
    /// whichever key it is under.
    #[test]
    fn the_newest_check_of_the_same_builds_is_used() {
        let dir = tempfile::tempdir().unwrap();
        let report = report_of(&[("libical", None, "4.0.7-1.fc46")]);
        let old = KeptCheck::new(&report, &[], chrono::Utc::now() - chrono::Duration::days(3));
        keep(dir.path(), &["FEDORA-2026-c8b6a481c4".to_string()], &old).unwrap();
        let new = KeptCheck::new(
            &report,
            &[(
                Finding::NotNewest(crate::check_update::NotNewest {
                    nvr: "a".into(),
                    target_nvr: "a".into(),
                    tag: "t".into(),
                }),
                Decision::Removed,
            )],
            chrono::Utc::now(),
        );
        keep(dir.path(), &["f46-build-side-158330".to_string()], &new).unwrap();
        let (kept, age) = reuse(
            dir.path(),
            &["FEDORA-2026-c8b6a481c4", "f46-build-side-158330"],
            &["libical-4.0.7-1.fc46".to_string()],
        )
        .unwrap();
        assert_eq!(kept.decisions.len(), 1);
        assert_eq!(age_text(age.unwrap()), "under a minute");
    }

    /// What update-bugs reads is the report as check-update voted on it.
    #[test]
    fn the_curated_report_has_the_decisions_applied() {
        let mut report = report_of(&[("libical", None, "4.0.7-1.fc46")]);
        let nn = crate::check_update::NotNewest {
            nvr: "libical-4.0.7-1.fc46".into(),
            target_nvr: "libical-4.0.7-1.fc46".into(),
            tag: "f46".into(),
        };
        report.not_newest = vec![nn.clone()];
        let kept = KeptCheck::new(
            &report,
            &[(Finding::NotNewest(nn), Decision::Removed)],
            chrono::Utc::now(),
        );
        assert_eq!(kept.report.not_newest.len(), 1, "kept raw");
        assert!(kept.curated().not_newest.is_empty());
    }
}
