// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Submitting a checked side tag to Bodhi (`check-update --submit`).
//!
//! After the reverse-dependency check passes (or the reviewer
//! curates / overrides the findings), creates the update from the
//! side tag via `POST /updates/` with `from_tag` — the API behind
//! `bodhi updates new --from-tag`. The plan (packages, type, bugs,
//! karma thresholds, notes) is shown for confirmation first, so an
//! accidentally missing package is visible before anything is
//! published.
//!
//! Authentication reuses the bodhi CLI's cached OIDC session, like
//! the karma flow (see `karma::ensure_session`).

use sandogasa_bodhi::models::{NewUpdateFromTag, UpdateSource};
use sandogasa_bodhi::{BodhiClient, auth};

/// What to submit alongside the side tag.
#[derive(Clone)]
pub struct SubmitOptions {
    /// Update notes/description (from `--notes` / `--notes-file`).
    pub notes: String,
    /// `bugfix` | `enhancement` | `security` | `newpackage`.
    pub update_type: String,
    /// `unspecified` | `low` | `medium` | `high` | `urgent`.
    pub severity: String,
    /// Bug IDs to associate with the update.
    pub bugs: Vec<u64>,
    /// Whether Bodhi closes them when the update goes stable, from
    /// `--close-bugs` / `--no-close-bugs`; `None` decides at submit
    /// time (see [`decide_close`]).
    pub close_bugs: Option<bool>,
    /// Push automatically at the karma thresholds. Forced off when
    /// `rpmfusion` is non-empty — see [`SubmitOptions::autokarma`].
    pub autokarma: bool,
    /// RPM Fusion counterparts of this update's packages, as
    /// (Fedora name, RPM Fusion name). Non-empty means the update
    /// must not reach stable on karma alone: RPM Fusion has to
    /// rebuild its side first, and an autopush would land the pair
    /// broken for anyone with both repositories enabled. Empty
    /// whenever `--rpmfusion-inventory` was not given, so the guard
    /// costs nothing when it is not asked for.
    pub rpmfusion: Vec<(String, String)>,
    pub stable_karma: i32,
    pub unstable_karma: i32,
    /// Skip the submission confirmation.
    pub assume_yes: bool,
    /// Koji profile for reading build changelogs, when not the
    /// default one.
    pub koji_profile: Option<String>,
    /// `-v`: name each lookup the bug proposal makes.
    pub verbose: bool,
}

/// Resolve the update notes from `--notes` (inline) or `--notes-file`
/// (for longer descriptions). Bodhi rejects empty notes, so require
/// one of the two and a non-blank result — checked up front, before
/// the analysis runs for minutes.
pub fn resolve_notes(notes: &sandogasa_cli::NotesArgs) -> Result<String, String> {
    notes.read()?.ok_or_else(|| {
        "Bodhi requires update notes: pass --notes <text> or --notes-file <path>".to_string()
    })
}

/// Why a bug does not belong on this update, if it does not.
///
/// The package check is first because it is the stronger claim: the
/// update ships nothing that bug is about. The release check catches
/// what it cannot — a bug for a package the update *does* build, but
/// for a different release.
fn misfiled_reason(
    bug: &crate::karma::BugFacts,
    packages: &[String],
    branch: &str,
) -> Option<String> {
    if let Some(package) = crate::karma::bug_package(&bug.summary, bug.component.as_deref())
        && !packages.contains(&package)
    {
        return Some(format!("no {package} among the update's packages"));
    }
    crate::karma::other_release_note(&bug.summary, branch)
}

/// A bug the update cannot be fixing, and why.
struct Misfiled {
    bug_id: u64,
    summary: String,
    reason: String,
}

/// Drop bugs this update cannot be fixing.
///
/// A bug listed on an update that does not fix it will be closed
/// when the update goes stable. Catching it here is better than
/// voting -1 on it afterwards: nothing has been published yet, so
/// correcting the list costs nothing.
///
/// Two ways a bug fails to belong. Its package may not be among the
/// update's; only bugs that name their package outright are
/// considered for that (see `karma::bug_package`), since a stale
/// component on a CVE or FTBFS bug would otherwise drop it wrongly.
/// Or it may be a bot-filed report for *another release* — an
/// F46FailsToInstall listed on an F45 update, which the package
/// check cannot see because the package is built. That one is read
/// from the bots' own wording, which names the release outright, so
/// it carries none of the staleness the component does.
///
/// Under `--yes` this warns and keeps them: silently discarding a
/// bug the user passed to `--bug` is not something to do unasked.
pub(crate) async fn screen_bugs(
    bz: &sandogasa_bugzilla::BzClient,
    packages: &[String],
    branch: &str,
    bugs: &[u64],
    assume_yes: bool,
) -> Result<Vec<u64>, String> {
    let fetched = crate::karma::fetch_bugs(bz, bugs).await;
    let misfiled: Vec<Misfiled> = bugs
        .iter()
        .filter_map(|id| {
            let bug = fetched.get(id)?;
            let reason = misfiled_reason(bug, packages, branch)?;
            Some(Misfiled {
                bug_id: *id,
                summary: bug.summary.clone(),
                reason,
            })
        })
        .collect();
    if misfiled.is_empty() {
        return Ok(bugs.to_vec());
    }

    let shown: Vec<String> = misfiled
        .iter()
        .map(|b| format!("  #{} {}\n    {}", b.bug_id, b.summary, b.reason))
        .collect();
    eprintln!("\nThese bugs do not look like this update's to close:");
    for bug in &shown {
        eprintln!("{bug}");
    }
    if assume_yes {
        // `assume_yes` is --yes, or a run with no terminal to ask at;
        // name both, as either can be why nothing was asked.
        eprintln!(
            "kept, as nothing is dropped without asking (--yes, or no terminal); \
             an update that does not fix them could close them"
        );
        return Ok(bugs.to_vec());
    }
    let dropped: Vec<u64> = sandogasa_cli::confirm_each(
        "Leave these bugs off the update?",
        "Leave this bug off?",
        &shown,
        true,
    )
    .map_err(|e| e.to_string())?
    .into_iter()
    .map(|i| misfiled[i].bug_id)
    .collect();
    Ok(bugs
        .iter()
        .copied()
        .filter(|id| !dropped.contains(id))
        .collect())
}

/// A build's Koji NVR: Koji's NVRs carry no epoch.
fn koji_nvr(pkg: &str, evr: &str) -> String {
    let vr = evr.split_once(':').map_or(evr, |(_, vr)| vr);
    format!("{pkg}-{vr}")
}

/// The builds whose changelogs the bug proposal reads, as
/// `(package, Koji NVR)`: only those of packages the target already
/// has, as a package new to it brings a history whose bugs were closed
/// elsewhere (see `discover::changelog_candidates`).
fn changelog_nvrs(
    builds: &[(String, String)],
    old_versions: &std::collections::BTreeMap<String, Option<String>>,
) -> Vec<(String, String)> {
    builds
        .iter()
        .filter(|(pkg, _)| old_versions.get(pkg).is_some_and(Option::is_some))
        .map(|(pkg, evr)| (pkg.clone(), koji_nvr(pkg, evr)))
        .collect()
}

/// How many builds' changelogs one `multiCall` asks for: each comes
/// whole, and a kernel's or a glibc's runs to thousands of entries.
const CHANGELOG_CHUNK: usize = 25;

/// The changelogs of `nvrs` from the Koji hub at `hub_url`, one result
/// per chunk of [`CHANGELOG_CHUNK`] builds: each a changelog per build,
/// or the fault the hub gave for that one build, or why the chunk could
/// not be read, so a failure costs only its own chunk or build. Blocks.
#[allow(clippy::type_complexity)]
fn changelogs_from_hub(
    hub_url: &str,
    nvrs: &[String],
) -> Vec<Result<Vec<Result<String, String>>, String>> {
    let hub = sandogasa_kojihub::HubClient::new(hub_url);
    nvrs.chunks(CHANGELOG_CHUNK)
        .map(|chunk| {
            sandogasa_kojihub::retry(2, || hub.builds_changelogs(chunk))
                .map(|answers| {
                    answers
                        .into_iter()
                        .map(|a| a.map_err(|e| e.to_string()))
                        .collect()
                })
                .map_err(|e| e.to_string())
        })
        .collect()
}

/// The changelogs of `builds` (`(package, NVR)`), by package: from the
/// hub the Koji profile names, in batches, or through the `koji` CLI one
/// build at a time for a batch, or a build, the hub could not give, or
/// all of them when no hub is configured. Falling back is said, as the
/// CLI takes about a second a build.
async fn read_changelogs(
    builds: &[(String, String)],
    koji_profile: Option<&str>,
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    if builds.is_empty() {
        return out;
    }
    let cli = |chunk: &[(String, String)], out: &mut std::collections::BTreeMap<String, String>| {
        for (pkg, nvr) in chunk {
            if let Ok(text) = sandogasa_koji::build_info_with_changelog(nvr, koji_profile) {
                out.insert(pkg.clone(), text);
            }
        }
    };
    let Some(hub) = sandogasa_kojihub::config::hub_url(koji_profile.unwrap_or("koji")) else {
        cli(builds, &mut out);
        return out;
    };
    // The hub client blocks, so it runs where blocking is allowed.
    let nvrs: Vec<String> = builds.iter().map(|(_, nvr)| nvr.clone()).collect();
    let chunks = tokio::task::spawn_blocking(move || changelogs_from_hub(&hub, &nvrs))
        .await
        .unwrap_or_else(|e| vec![Err(e.to_string()); builds.len().div_ceil(CHANGELOG_CHUNK)]);
    // A build whose own request faulted (a hub hiccup, or a build it
    // does not know) is asked of the CLI too, rather than silently
    // contributing no changelog and so no bug.
    let mut faulted: Vec<(String, String)> = Vec::new();
    let mut fault = String::new();
    for (chunk, result) in builds.chunks(CHANGELOG_CHUNK).zip(chunks) {
        match result {
            Ok(texts) => {
                for ((pkg, nvr), text) in chunk.iter().zip(texts) {
                    match text {
                        Ok(text) => {
                            out.insert(pkg.clone(), text);
                        }
                        Err(e) => {
                            fault = e;
                            faulted.push((pkg.clone(), nvr.clone()));
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "warning: the Koji hub could not give {} changelog(s) ({e}); \
                     asking the koji CLI, one build at a time",
                    chunk.len()
                );
                cli(chunk, &mut out);
            }
        }
    }
    if !faulted.is_empty() {
        eprintln!(
            "warning: the Koji hub could not give the changelog of {} ({fault}); \
             asking the koji CLI",
            faulted
                .iter()
                .map(|(_, nvr)| nvr.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        cli(&faulted, &mut out);
    }
    out
}

/// The bugs a checked update looks like it closes, and the open ones a
/// build from before it had already met, from the report of its check:
/// what [`propose_bugs`] offers, and `update-bugs` revisits (#113).
/// Asks nothing; bugs in `already_listed` are not proposed again.
pub(crate) async fn gather_bugs(
    bz: &sandogasa_bugzilla::BzClient,
    report: &crate::check_update::CheckUpdateReport,
    already_listed: &[u64],
    koji_profile: Option<&str>,
    verbose: bool,
) -> crate::discover::Proposal {
    // The check knows each package's old and new version-release,
    // which is what says how much of a changelog is new.
    let builds: Vec<(String, String)> = report
        .changes
        .iter()
        .map(|c| (c.package.clone(), c.new.clone()))
        .collect();
    if builds.is_empty() {
        return crate::discover::Proposal {
            candidates: Vec::new(),
            already_met: Vec::new(),
            closed: Vec::new(),
        };
    }
    // The changelogs, open bugs and CVE fixes take a while on a big
    // side tag, and nothing else is printed until the proposal.
    eprintln!(
        "\nLooking for the bugs this update closes ({} build(s))...",
        builds.len()
    );
    let old_versions: std::collections::BTreeMap<String, Option<String>> = report
        .changes
        .iter()
        .map(|c| (c.package.clone(), c.old.clone()))
        .collect();
    let started = std::time::Instant::now();
    let changelogs = read_changelogs(&changelog_nvrs(&builds, &old_versions), koji_profile).await;
    if verbose {
        eprintln!(
            "[bugs] read {} changelog(s) from Koji in {:.1}s",
            changelogs.len(),
            started.elapsed().as_secs_f64()
        );
    }

    let started = std::time::Instant::now();
    let trackers =
        sandogasa_bugclass::bugzilla::lookup_branch_trackers(bz, report.dist_branch()).await;
    if verbose {
        eprintln!(
            "[bugs] read {}'s FTBFS and FTI trackers from Bugzilla in {:.1}s",
            report.dist_branch(),
            started.elapsed().as_secs_f64()
        );
    }
    // Koji's NVRs carry no epoch.
    let nvrs: Vec<String> = builds.iter().map(|(pkg, evr)| koji_nvr(pkg, evr)).collect();
    let facts = crate::karma::UpdateFacts {
        builds: &builds,
        nvrs: &nvrs,
        trackers: &trackers,
        branch: report.dist_branch(),
        unsatisfied: &report.installability_issues,
        full_analysis: report.full_analysis,
    };
    crate::discover::candidates(
        bz,
        &changelogs,
        &old_versions,
        &facts,
        already_listed,
        verbose,
    )
    .await
}

/// Propose bugs the update looks like it closes, and return the ones
/// the user accepts.
///
/// Skipped entirely under `--yes`: attaching a bug means closing it
/// when the update goes stable, which is not something to do without
/// being asked. Each candidate is shown with how it was found.
async fn propose_bugs(
    bz: &sandogasa_bugzilla::BzClient,
    report: &crate::check_update::CheckUpdateReport,
    already_listed: &[u64],
    koji_profile: Option<&str>,
    verbose: bool,
) -> Result<Vec<u64>, String> {
    let crate::discover::Proposal {
        candidates,
        already_met,
        closed,
    } = gather_bugs(bz, report, already_listed, koji_profile, verbose).await;
    if !already_met.is_empty() {
        eprintln!("\nLeft off, already met before this update (theirs to close):");
        for c in &already_met {
            eprintln!("  #{} {} ({})", c.bug_id, c.summary, c.source);
        }
    }
    let mut taken = Vec::new();
    if !candidates.is_empty() {
        let shown: Vec<String> = candidates.iter().map(|c| c.shown()).collect();
        eprintln!("\nThis update looks like it closes:");
        for candidate in &shown {
            eprintln!("{candidate}");
        }
        taken.extend(
            sandogasa_cli::confirm_each(
                "Add these bugs to the update?",
                "Add this bug?",
                &shown,
                true,
            )
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|i| candidates[i].bug_id),
        );
    }
    // Closed already, often by another branch's update of the same fix,
    // which may have closed a bug that still affects this branch: offered,
    // but not taken unless asked for.
    if !closed.is_empty() {
        let shown: Vec<String> = closed.iter().map(|c| c.shown()).collect();
        eprintln!("\nAlso fixed by this update, but closed already:");
        for candidate in &shown {
            eprintln!("{candidate}");
        }
        taken.extend(
            sandogasa_cli::confirm_each(
                "Add these closed bugs to the update too?",
                "Add this closed bug?",
                &shown,
                false,
            )
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|i| closed[i].bug_id),
        );
    }
    Ok(taken)
}

/// Whether the update may autopush, given what the operator asked
/// for and what RPM Fusion ships.
///
/// `--disable-autokarma` turns it off; so does a counterpart, and the
/// flag cannot turn it back on. The operator's switch is an override
/// in one direction only, because the other direction is the one that
/// breaks users' systems.
fn effective_autokarma(opts: &SubmitOptions) -> bool {
    opts.autokarma && opts.rpmfusion.is_empty()
}

/// Format the submission plan shown for confirmation. Leads with the
/// package list — the whole point of checking before submitting is
/// spotting a subpackage update that is accidentally missing one.
/// What the plan calls this submission.
///
/// A side tag names itself; a list of builds names the NVRs, since
/// the packages line below shows source names and the dist tag is
/// what says which release each update will be for.
fn source_label(source: &UpdateSource) -> String {
    match source {
        UpdateSource::FromTag(tag) => tag.clone(),
        UpdateSource::Builds(nvrs) => nvrs.join(", "),
    }
}

fn plan_text(
    tag: &str,
    packages: &[String],
    bugs: &[u64],
    close_bugs: bool,
    opts: &SubmitOptions,
) -> String {
    use std::fmt::Write as _;
    let mut o = String::new();
    let _ = writeln!(o, "\nSubmission plan for {tag}:");
    let _ = writeln!(
        o,
        "  packages ({}): {}",
        packages.len(),
        packages.join(", ")
    );
    let _ = writeln!(
        o,
        "  type: {}, severity: {}",
        opts.update_type, opts.severity
    );
    if !bugs.is_empty() {
        let listed = bugs
            .iter()
            .map(|b| format!("#{b}"))
            .collect::<Vec<_>>()
            .join(", ");
        let fate = if close_bugs {
            "closed on stable"
        } else {
            "linked, left open on stable"
        };
        let _ = writeln!(o, "  bugs ({fate}): {listed}");
    }
    if effective_autokarma(opts) {
        let _ = writeln!(
            o,
            "  autopush at karma +{} (unpush at {})",
            opts.stable_karma, opts.unstable_karma
        );
    } else if opts.rpmfusion.is_empty() {
        let _ = writeln!(o, "  autopush: disabled");
    } else {
        // Name the counterparts rather than just disabling: a policy
        // the operator cannot see applied is one they cannot check.
        let pairs = opts
            .rpmfusion
            .iter()
            .map(|(fedora, rf)| format!("{fedora} -> {rf}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            o,
            "  autopush: disabled — RPM Fusion ships a counterpart ({pairs}), \
             which needs rebuilding before this reaches stable"
        );
    }
    let mut lines = opts.notes.lines();
    let first = lines.next().unwrap_or("");
    let rest = lines.count();
    if rest == 0 {
        let _ = writeln!(o, "  notes: {first}");
    } else {
        let _ = writeln!(o, "  notes: {first} (+{rest} more lines)");
    }
    o
}

/// Print the submission plan and confirm (default yes).
/// Bodhi's severities, lowest first; anything else ranks as
/// `unspecified`.
const SEVERITIES: [&str; 5] = ["unspecified", "low", "medium", "high", "urgent"];

fn severity_rank(severity: &str) -> usize {
    SEVERITIES
        .iter()
        .position(|s| s.eq_ignore_ascii_case(severity))
        .unwrap_or(0)
}

/// The severities a security update can be given, each with the letter
/// that picks it at the prompt.
const SEVERITY_CHOICES: [sandogasa_review::Choice; 4] = [
    sandogasa_review::Choice::new('l', "low"),
    sandogasa_review::Choice::new('m', "medium"),
    sandogasa_review::Choice::new('h', "high"),
    sandogasa_review::Choice::new('u', "urgent"),
];

/// The severity the prompt's answer `key` picks.
fn severity_for(key: char) -> Option<&'static str> {
    SEVERITY_CHOICES
        .iter()
        .position(|c| c.word.key == key)
        .map(|i| SEVERITIES[i + 1])
}

/// Ask, at a terminal, for the severity of a security update that still
/// has none once the attached CVE trackers were offered: Bodhi refuses
/// one without, and failing here instead threw away the check that had
/// just run (#135). `None` when the user quits.
fn ask_severity() -> Result<Option<String>, String> {
    eprintln!(
        "\nA security update needs a severity, and no attached CVE tracker gives \
         one (or its severity was declined)."
    );
    let answer = sandogasa_review::ask(
        "Severity of this security update?",
        &sandogasa_review::Menu {
            choices: &SEVERITY_CHOICES,
            default: None,
            all: false,
            quit: true,
            default_arg: None,
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(match answer {
        sandogasa_review::Answer::Pick { key, .. } => severity_for(key).map(str::to_string),
        _ => None,
    })
}

/// The CVE trackers among `ids`, with their Bugzilla severity.
fn cve_trackers(bugs: &crate::karma::BugMap, ids: &[u64]) -> Vec<(u64, String)> {
    ids.iter()
        .filter_map(|id| {
            let b = bugs.get(id)?;
            sandogasa_cve::facts::is_security_tracker(&b.summary, &b.keywords)
                .then(|| (*id, b.severity.to_ascii_lowercase()))
        })
        .collect()
}

/// What the attached CVE trackers suggest for an update's type and
/// severity, each with the bug that suggests it.
#[derive(Debug, PartialEq, Eq)]
struct Suggested {
    /// `security`, when the type is something else and `.0` is a CVE
    /// tracker.
    security: Option<u64>,
    /// The highest tracker severity, when it is above the update's.
    severity: Option<(String, u64)>,
}

/// What `trackers` (bug, severity) suggest for an update of
/// `update_type` at `severity`. Tracker severity is Product Security's
/// impact rating and maps one to one onto Bodhi's levels; an
/// `unspecified` tracker suggests nothing.
fn suggest(update_type: &str, severity: &str, trackers: &[(u64, String)]) -> Suggested {
    let security = (update_type != "security")
        .then(|| trackers.first().map(|(id, _)| *id))
        .flatten();
    // The first tracker at the top severity, to name: `max_by_key`
    // alone would name the last.
    let highest = trackers
        .iter()
        .filter(|(_, s)| severity_rank(s) > 0)
        .rev()
        .max_by_key(|(_, s)| severity_rank(s));
    let severity = highest
        .filter(|(_, s)| severity_rank(s) > severity_rank(severity))
        .map(|(id, s)| (SEVERITIES[severity_rank(s)].to_string(), *id));
    Suggested { security, severity }
}

/// The update's type and severity, after offering what the attached
/// CVE trackers suggest (see [`suggest`]).
///
/// At a terminal each suggestion is asked, defaulting to yes. Under
/// `--yes` or with no terminal it is only reported, and what was
/// passed is kept: an explicit choice is never overridden unasked. A
/// security update still without a severity is refused, since Bodhi
/// refuses it too.
fn settle_type_and_severity(
    opts: &SubmitOptions,
    trackers: &[(u64, String)],
) -> Result<SubmitOptions, String> {
    let mut out = opts.clone();
    let ask = !opts.assume_yes && std::io::IsTerminal::is_terminal(&std::io::stdin());
    let s = suggest(&out.update_type, &out.severity, trackers);
    if let Some(id) = s.security {
        let why = format!(
            "#{id} is a CVE tracker, and the update type is {}",
            out.update_type
        );
        if ask {
            eprintln!("\n{why}.");
            if sandogasa_cli::confirm("Submit it as a security update?", true)
                .map_err(|e| e.to_string())?
            {
                out.update_type = "security".to_string();
            }
        } else {
            eprintln!("note: {why}; pass --type security to submit it as one");
        }
    }
    if out.update_type == "security" {
        // Asked again for the type just settled: a security update
        // without one has every tracker's severity above it.
        let s = suggest(&out.update_type, &out.severity, trackers);
        if let Some((level, id)) = s.severity {
            let why = format!(
                "the attached trackers rate up to {level} (#{id}), and the update's severity is {}",
                out.severity
            );
            if ask {
                eprintln!("\n{}{}.", why[..1].to_uppercase(), &why[1..]);
                if sandogasa_cli::confirm(&format!("Use severity {level}?"), true)
                    .map_err(|e| e.to_string())?
                {
                    out.severity = level;
                }
            } else {
                eprintln!("note: {why}; pass --severity {level} to match");
            }
        }
        // Still none: ask, where someone can answer, rather than fail
        // after a check that may have taken minutes (#135).
        if out.severity == "unspecified" && ask {
            match ask_severity()? {
                Some(level) => out.severity = level,
                None => return Err("no severity given; nothing was submitted".to_string()),
            }
        }
        if out.severity == "unspecified" {
            return Err(
                "a security update needs a severity: pass --severity (low/medium/high/urgent)"
                    .to_string(),
            );
        }
    }
    Ok(out)
}

/// The bugs among `ids` that are a security team tracker for a whole
/// release family, `[fedora-all]` or `[epel-all]`.
fn family_trackers(bugs: &crate::karma::BugMap, ids: &[u64]) -> Vec<u64> {
    ids.iter()
        .copied()
        .filter(|id| {
            bugs.get(id).is_some_and(|b| {
                b.summary.contains("[fedora-all]") || b.summary.contains("[epel-all]")
            })
        })
        .collect()
}

/// Whether the attached bugs close when the update goes stable.
///
/// What `--close-bugs` or `--no-close-bugs` said, when either was
/// given. Otherwise yes, unless one of them is a tracker for a whole
/// release family (`family`): one release's update rarely fixes every
/// release such a tracker covers, and Bodhi has one switch for all of
/// an update's bugs. Asked at a terminal, with that default; under
/// `--yes`, or with no terminal, the default stands, so a family
/// tracker is never closed unasked.
fn decide_close(
    opts: &SubmitOptions,
    bugs: &[u64],
    family: &[u64],
    branch: &str,
) -> Result<bool, String> {
    if let Some(close) = opts.close_bugs {
        return Ok(close);
    }
    let default = family.is_empty();
    if bugs.is_empty() || opts.assume_yes || !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Ok(default);
    }
    if !family.is_empty() {
        let listed = family
            .iter()
            .map(|b| format!("#{b}"))
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!(
            "\n{listed} cover every release of their family, and this update is for {branch} \
             only: closing them when it goes stable would mark the other releases fixed too."
        );
    }
    sandogasa_cli::confirm(
        "Close the attached bugs when the update goes stable?",
        default,
    )
    .map_err(|e| e.to_string())
}

fn confirm_plan(
    tag: &str,
    packages: &[String],
    bugs: &[u64],
    close_bugs: bool,
    opts: &SubmitOptions,
) -> Result<bool, String> {
    eprint!("{}", plan_text(tag, packages, bugs, close_bugs, opts));
    sandogasa_cli::confirm("Submit this update?", true).map_err(|e| e.to_string())
}

/// Submit the side tag to Bodhi. `packages` are the update's source
/// packages (from the check report), shown in the confirmation plan.
/// Returns the created update alias(es) so the caller can post the
/// check report as a follow-up comment.
pub fn run(
    source: &UpdateSource,
    report: &crate::check_update::CheckUpdateReport,
    opts: &SubmitOptions,
) -> Result<Vec<String>, String> {
    let label = source_label(source);
    let packages = &report.updated_packages;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let bz = sandogasa_bugzilla::BzClient::new("https://bugzilla.redhat.com");

    // Settle the bug list before the plan is shown, so what is
    // confirmed is what gets submitted: propose what the update looks
    // like it closes, then drop anything it cannot be closing.
    let mut bugs = opts.bugs.clone();
    if !opts.assume_yes {
        bugs.extend(rt.block_on(propose_bugs(
            &bz,
            report,
            &bugs,
            opts.koji_profile.as_deref(),
            opts.verbose,
        ))?);
    }
    let bugs = if bugs.is_empty() {
        Vec::new()
    } else {
        rt.block_on(screen_bugs(
            &bz,
            packages,
            report.dist_branch(),
            &bugs,
            opts.assume_yes,
        ))?
    };

    // Bodhi closes every attached bug when the update goes stable, or
    // none: a tracker for the whole family is rarely fixed by one
    // release's update, so its presence changes the default (#75).
    let fetched = if bugs.is_empty() {
        crate::karma::BugMap::new()
    } else {
        rt.block_on(crate::karma::fetch_bugs(&bz, &bugs))
    };
    let family = family_trackers(&fetched, &bugs);
    let close_bugs = decide_close(opts, &bugs, &family, report.dist_branch())?;
    // A CVE tracker among the bugs says what kind of update this is
    // and how severe: offered, never imposed (#76).
    let settled = settle_type_and_severity(opts, &cve_trackers(&fetched, &bugs))?;
    let opts = &settled;

    if opts.assume_yes {
        // No prompt, but still show what is being submitted.
        eprint!("{}", plan_text(&label, packages, &bugs, close_bugs, opts));
    } else if !confirm_plan(&label, packages, &bugs, close_bugs, opts)? {
        return Err("aborted: update not submitted".to_string());
    }

    rt.block_on(async {
        // Refresh preemptively: the analysis may have run for long
        // enough that a token valid at the start is close to (or
        // past) expiry by the time we post.
        let http = crate::karma::http_client();
        let token =
            auth::cli_session_token_refreshed(&http, &auth::cli_cache_path(), auth::FEDORA_IDP)
                .await?;
        let client = BodhiClient::new()
            .with_token(token)
            .map_err(|e| e.to_string())?;
        let req = NewUpdateFromTag {
            source: source.clone(),
            notes: opts.notes.clone(),
            update_type: opts.update_type.clone(),
            severity: opts.severity.clone(),
            bugs: bugs.clone(),
            close_bugs,
            autokarma: effective_autokarma(opts),
            stable_karma: opts.stable_karma,
            unstable_karma: opts.unstable_karma,
        };
        let resp = client
            .new_update_from_tag(&req)
            .await
            .map_err(|e| e.to_string())?;
        for caveat in &resp.caveats {
            eprintln!("note from bodhi: {}", caveat.description);
        }
        let aliases: Vec<String> = resp.aliases().iter().map(|a| a.to_string()).collect();
        if aliases.is_empty() {
            eprintln!("submitted (Bodhi returned no update alias)");
        }
        for alias in &aliases {
            eprintln!("submitted: https://bodhi.fedoraproject.org/updates/{alias}");
        }
        Ok(aliases)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> SubmitOptions {
        SubmitOptions {
            notes: "Update uutils to 0.2.\n\nRebuilt against new uucore.".to_string(),
            update_type: "enhancement".to_string(),
            severity: "unspecified".to_string(),
            bugs: vec![2482250],
            close_bugs: None,
            autokarma: true,
            stable_karma: 3,
            unstable_karma: -3,
            assume_yes: false,
            koji_profile: None,
            rpmfusion: vec![],
            verbose: false,
        }
    }

    #[test]
    fn only_packages_the_target_has_have_their_changelogs_read() {
        let builds = vec![
            ("libical".to_string(), "4.0.7-1.fc46".to_string()),
            ("perl-Foo".to_string(), "1:2.0-1.fc46".to_string()),
            ("brand-new".to_string(), "1.0-1.fc46".to_string()),
        ];
        let old: std::collections::BTreeMap<String, Option<String>> = [
            ("libical".to_string(), Some("3.0.20-11.fc46".to_string())),
            ("perl-Foo".to_string(), Some("1:1.9-1.fc46".to_string())),
            ("brand-new".to_string(), None),
        ]
        .into();
        assert_eq!(
            changelog_nvrs(&builds, &old),
            [
                ("libical".to_string(), "libical-4.0.7-1.fc46".to_string()),
                // Koji knows the build without its epoch.
                ("perl-Foo".to_string(), "perl-Foo-2.0-1.fc46".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn changelogs_are_read_from_the_hub_in_one_batch() {
        use wiremock::matchers::{body_string_contains, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let entry = "<value><struct>\
             <member><name>date</name><value><string>2026-11-02 12:00:00</string></value></member>\
             <member><name>author</name><value><string>Milan Crha &lt;mcrha@redhat.com&gt; - 4.0.7-1</string></value></member>\
             <member><name>text</name><value><string>- Resolves: rhbz#2458837</string></value></member>\
             </struct></value>";
        let fault = "<value><struct>\
             <member><name>faultCode</name><value><int>1000</int></value></member>\
             <member><name>faultString</name><value><string>No such build</string></value></member>\
             </struct></value>";
        Mock::given(method("POST"))
            .and(body_string_contains("getChangelogEntries"))
            .and(body_string_contains("libical-4.0.7-1.fc46"))
            .and(body_string_contains("nope-1-1.fc46"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                "<?xml version=\"1.0\"?><methodResponse><params><param><value><array><data>\
                 <value><array><data><value><array><data>{entry}</data></array></value></data></array></value>\
                 {fault}</data></array></value></param></params></methodResponse>"
            )))
            .expect(1)
            .mount(&server)
            .await;
        let nvrs = [
            "libical-4.0.7-1.fc46".to_string(),
            "nope-1-1.fc46".to_string(),
        ];
        let uri = server.uri();
        let mut chunks = tokio::task::spawn_blocking(move || changelogs_from_hub(&uri, &nvrs))
            .await
            .unwrap();
        assert_eq!(chunks.len(), 1, "two builds are one chunk");
        let got = chunks.remove(0).unwrap();
        assert_eq!(got.len(), 2);
        let text = got[0].as_deref().unwrap();
        // The header the changelog parser reads the version from.
        assert!(
            text.starts_with("* 2026-11-02 12:00:00 Milan Crha <mcrha@redhat.com> - 4.0.7-1\n"),
            "{text}"
        );
        assert_eq!(
            crate::discover::new_entry_bug_refs(text, Some("3.0.20-11.fc46")),
            [2458837]
        );
        // The unknown build is its own fault, kept for the CLI to retry.
        assert!(
            got[1].as_ref().is_err_and(|e| e.contains("No such build")),
            "{:?}",
            got[1]
        );
    }

    #[test]
    fn an_rpm_fusion_counterpart_disables_autopush_and_says_why() {
        // libheif pairs with RPM Fusion's libheif-freeworld: letting
        // this reach stable on karma would leave the pair broken for
        // anyone with both repositories enabled.
        let mut o = opts();
        o.rpmfusion = vec![("libheif".to_string(), "libheif-freeworld".to_string())];
        assert!(!effective_autokarma(&o));
        let text = plan_text("f43-build-side-1", &["libheif".to_string()], &[], true, &o);
        assert!(text.contains("autopush: disabled"), "{text}");
        assert!(text.contains("libheif -> libheif-freeworld"), "{text}");
        assert!(text.contains("needs rebuilding"), "{text}");
        // The operator's flag cannot switch it back on: the override
        // runs one way, because the other way is the one that breaks
        // users' systems.
        o.autokarma = true;
        assert!(!effective_autokarma(&o));
    }

    #[test]
    fn an_update_rpm_fusion_does_not_ship_keeps_autopush() {
        // The common case: no counterpart, nothing changes, and the
        // plan says what it always said.
        let o = opts();
        assert!(effective_autokarma(&o));
        let text = plan_text("f43-build-side-1", &["zlib".to_string()], &[], true, &o);
        assert!(text.contains("autopush at karma +3"), "{text}");
        assert!(!text.contains("RPM Fusion"), "{text}");
        // And --disable-autokarma still disables it on its own, with
        // no counterpart to explain.
        let mut off = opts();
        off.autokarma = false;
        let text = plan_text("f43-build-side-1", &["zlib".to_string()], &[], true, &off);
        assert!(text.contains("autopush: disabled"), "{text}");
        assert!(!text.contains("RPM Fusion"), "{text}");
    }

    #[test]
    fn update_notes_are_required() {
        let err = resolve_notes(&sandogasa_cli::NotesArgs::default()).unwrap_err();
        assert!(err.contains("--notes"), "{err}");
        let mut given = sandogasa_cli::NotesArgs::default();
        given.notes = Some("Long description.".into());
        assert_eq!(resolve_notes(&given).unwrap(), "Long description.");
    }

    #[test]
    fn each_severity_is_picked_by_its_letter() {
        assert_eq!(severity_for('l'), Some("low"));
        assert_eq!(severity_for('m'), Some("medium"));
        assert_eq!(severity_for('h'), Some("high"));
        assert_eq!(severity_for('u'), Some("urgent"));
        assert_eq!(severity_for('x'), None);
    }

    #[test]
    fn cve_trackers_suggest_a_security_update_at_their_highest_severity() {
        // Issue #76: the F45 asahi-installer update fixes trackers
        // rated low, medium and high, and goes out as a bugfix unless
        // told otherwise.
        let trackers: Vec<(u64, String)> = [
            (2444701, "low"),
            (2457938, "medium"),
            (2458010, "high"),
            (2546328, "high"),
        ]
        .map(|(id, s)| (id, s.to_string()))
        .to_vec();
        assert_eq!(
            suggest("bugfix", "unspecified", &trackers),
            Suggested {
                security: Some(2444701),
                severity: Some(("high".to_string(), 2458010)),
            }
        );
        // Already a security update at that severity: nothing to say.
        assert_eq!(
            suggest("security", "high", &trackers),
            Suggested {
                security: None,
                severity: None
            }
        );
        // A higher severity than the trackers' is the submitter's call.
        assert_eq!(suggest("security", "urgent", &trackers).severity, None);
        // A lower one is raised to the highest tracker's.
        assert_eq!(
            suggest("security", "medium", &trackers).severity,
            Some(("high".to_string(), 2458010))
        );
        // No trackers, or none rated: nothing.
        assert_eq!(
            suggest("bugfix", "unspecified", &[]),
            Suggested {
                security: None,
                severity: None
            }
        );
        assert_eq!(
            suggest("security", "unspecified", &[(1, "unspecified".to_string())]).severity,
            None
        );

        // Under --yes the suggestions are reported, not applied.
        let mut o = opts();
        o.assume_yes = true;
        let kept = settle_type_and_severity(&o, &trackers).unwrap();
        assert_eq!(kept.update_type, "enhancement");
        // And a security update with nothing to give it a severity is
        // refused, as Bodhi would.
        o.update_type = "security".to_string();
        o.severity = "unspecified".to_string();
        assert!(settle_type_and_severity(&o, &[]).is_err());
        o.severity = "low".to_string();
        assert_eq!(
            settle_type_and_severity(&o, &trackers).unwrap().severity,
            "low"
        );
    }

    #[test]
    fn a_family_tracker_is_left_open_unless_asked() {
        // Issue #75: one release's update rarely fixes every release
        // a [fedora-all] tracker covers, and Bodhi closes all of an
        // update's bugs or none.
        let bug = |summary: &str| crate::karma::BugFacts {
            summary: summary.to_string(),
            ..Default::default()
        };
        let fetched = crate::karma::BugMap::from([
            (
                2525129,
                bug("CVE-2026-7774 asahi-installer: Python tarfile [fedora-all]"),
            ),
            (
                2431623,
                bug("CVE-2025-15366 asahi-installer: IMAP [fedora-43]"),
            ),
            (
                2457936,
                bug("CVE-2026-1502 asahi-installer: Python [epel-all]"),
            ),
        ]);
        assert_eq!(
            family_trackers(&fetched, &[2525129, 2431623, 2457936]),
            [2525129, 2457936]
        );

        // Under --yes the default stands: open with a family tracker,
        // closed without one, and nothing at all to decide without bugs.
        let mut o = opts();
        o.assume_yes = true;
        assert!(!decide_close(&o, &[2525129], &[2525129], "f45").unwrap());
        assert!(decide_close(&o, &[2431623], &[], "f45").unwrap());
        // The flags decide whatever the bugs are.
        o.close_bugs = Some(true);
        assert!(decide_close(&o, &[2525129], &[2525129], "f45").unwrap());
        o.close_bugs = Some(false);
        assert!(!decide_close(&o, &[2431623], &[], "f45").unwrap());

        // The plan says which.
        let text = plan_text(
            "f45-build-side-1",
            &["asahi-installer".into()],
            &[2525129],
            false,
            &o,
        );
        assert!(
            text.contains("bugs (linked, left open on stable): #2525129"),
            "{text}"
        );
    }

    #[test]
    fn plan_text_lists_packages_and_settings() {
        let o = opts();
        let text = plan_text(
            "epel9-build-side-133287",
            &["rust-uucore".to_string(), "uutils-coreutils".to_string()],
            &o.bugs,
            true,
            &o,
        );
        assert!(text.contains("epel9-build-side-133287"), "{text}");
        assert!(
            text.contains("packages (2): rust-uucore, uutils-coreutils"),
            "{text}"
        );
        assert!(
            text.contains("type: enhancement, severity: unspecified"),
            "{text}"
        );
        assert!(text.contains("bugs (closed on stable): #2482250"), "{text}");
        assert!(
            text.contains("autopush at karma +3 (unpush at -3)"),
            "{text}"
        );
        assert!(
            text.contains("notes: Update uutils to 0.2. (+2 more lines)"),
            "{text}"
        );
    }

    #[test]
    fn plan_text_disabled_autokarma_and_single_line_notes() {
        let mut o = opts();
        o.autokarma = false;
        o.bugs.clear();
        o.notes = "One-liner".to_string();
        let text = plan_text("f44-build-side-1", &["fish".to_string()], &o.bugs, true, &o);
        assert!(text.contains("autopush: disabled"), "{text}");
        assert!(!text.contains("bugs"), "{text}");
        assert!(text.contains("notes: One-liner\n"), "{text}");
    }

    /// A Bugzilla stub returning one bug per id.
    async fn bz_with(bugs: serde_json::Value) -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(bugs))
            .mount(&server)
            .await;
        server
    }

    fn bz_bug(id: u64, summary: &str, component: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "summary": summary,
            "status": "NEW",
            "resolution": "",
            "product": "Fedora",
            "component": [component],
            "severity": "",
            "priority": "",
            "assigned_to": "",
            "creator": "",
            "creation_time": "2026-01-01T00:00:00Z",
            "last_change_time": "2026-01-01T00:00:00Z",
            "keywords": [],
            "blocks": [],
        })
    }

    #[test]
    fn a_bug_for_another_release_is_misfiled_though_its_package_is_built() {
        // The real pairing: one dist-git commit named every
        // release's FailsToInstall bug in its Fixes: trailer, so an
        // f46 bug rides along on an f45 update. The package check
        // cannot see it -- the package *is* built -- and submitting
        // it would close the f46 bug on an f45 push.
        let bug = crate::karma::BugFacts {
            summary: "F46FailsToInstall: gnome-shell-extension-frippery-applications-menu"
                .to_string(),
            component: Some("gnome-shell-frippery".to_string()),
            ..Default::default()
        };
        let packages = vec!["gnome-shell-frippery".to_string()];
        let reason = misfiled_reason(&bug, &packages, "f45").expect("f46 does not belong on f45");
        assert!(reason.contains("f46"), "{reason}");
        assert!(reason.contains("f45"), "{reason}");
        // The same bug on its own release belongs there.
        assert_eq!(misfiled_reason(&bug, &packages, "f46"), None);
    }

    #[test]
    fn a_package_the_update_does_not_build_is_named_before_any_release() {
        // Both could be true at once; the package is the stronger
        // claim and is what the user is told.
        let bug = crate::karma::BugFacts {
            summary: "zsh-5.9 is available".to_string(),
            component: Some("zsh".to_string()),
            ..Default::default()
        };
        let reason = misfiled_reason(&bug, &["rust-ctor".to_string()], "f45")
            .expect("zsh is not among the packages");
        assert!(reason.contains("no zsh"), "{reason}");
    }

    #[tokio::test]
    async fn screen_bugs_keeps_bugs_the_update_builds_for() {
        let server = bz_with(serde_json::json!({
            "bugs": [bz_bug(1, "rust-ctor-1.0.8 is available", "rust-ctor")]
        }))
        .await;
        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], "f44", &[1], true)
            .await
            .unwrap();
        assert_eq!(kept, vec![1]);
    }

    #[tokio::test]
    async fn screen_bugs_keeps_unclassified_bugs() {
        // A CVE names no package we can check against the builds, so
        // it is never a candidate for dropping.
        let server = bz_with(serde_json::json!({
            "bugs": [bz_bug(2, "CVE-2026-1234 rust-dtor: overflow", "rust-dtor")]
        }))
        .await;
        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], "f44", &[2], true)
            .await
            .unwrap();
        assert_eq!(kept, vec![2]);
    }

    #[tokio::test]
    async fn screen_bugs_warns_but_keeps_under_assume_yes() {
        // The update builds no rust-dtor, but dropping a bug the user
        // passed to --bug is not something to do without asking.
        let server = bz_with(serde_json::json!({
            "bugs": [bz_bug(3, "rust-dtor-1.0.5 is available", "rust-dtor")]
        }))
        .await;
        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], "f44", &[3], true)
            .await
            .unwrap();
        assert_eq!(kept, vec![3]);
    }

    #[tokio::test]
    async fn screen_bugs_keeps_everything_when_bugzilla_is_down() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let kept = screen_bugs(&bz, &["rust-ctor".to_string()], "f44", &[4], false)
            .await
            .unwrap();
        assert_eq!(kept, vec![4], "a fetch failure must not drop bugs");
    }
}
