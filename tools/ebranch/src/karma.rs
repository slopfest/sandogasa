// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Karma voting on Bodhi updates (`check-update --give-karma`).
//!
//! Posts a comment with overall karma and per-bug feedback, the
//! way the Bodhi web UI does. Four kinds of bug are judged
//! automatically, each identified by something authoritative rather
//! than by reading its summary: a release-monitoring request by its
//! Bugzilla component and the version the summary asks for, a review
//! request by the package named in its title, and FTBFS and
//! FailsToInstall bugs by the release tracker they block. Anything
//! else is put to the user.
//!
//! Authentication reuses the bodhi CLI's cached OIDC session
//! (see `sandogasa_bodhi::auth`) — authenticate once with any
//! authenticated `bodhi` command and this flow shares the tokens.

use std::cmp::Ordering;

use sandogasa_bodhi::models::BugFeedbackItem;
use sandogasa_bodhi::{BodhiClient, auth};

/// What Bugzilla says about one of an update's bugs. Bodhi carries
/// only an ID and a title, so everything used to judge a bug beyond
/// its summary comes from here.
#[derive(Default)]
pub(crate) struct BugFacts {
    pub summary: String,
    /// The component names the package a release-monitoring bug is
    /// about, which is what an update's builds are matched against.
    pub component: Option<String>,
    /// Bugs this one blocks — how an FTBFS or FailsToInstall bug is
    /// recognized, by the release tracker it blocks.
    pub blocks: Vec<u64>,
}

/// Bug ID to what Bugzilla says about it.
type BugMap = std::collections::HashMap<u64, BugFacts>;

/// What the update and its check say, for judging bugs against.
pub(crate) struct UpdateFacts<'a> {
    /// `(source name, version)` for each build in the update.
    pub builds: &'a [(String, String)],
    /// The release trackers for the branch this update targets, so an
    /// FTBFS bug for a *different* release is not answered here.
    pub trackers: &'a sandogasa_bugclass::bugzilla::BranchTrackers,
    /// The branch the check ran against, for reading the release out
    /// of a bot-filed FTBFS or FailsToInstall summary.
    pub branch: &'a str,
    /// Packages whose Requires did not resolve, from the check.
    pub unsatisfied: &'a [crate::check_update::UnsatisfiedDep],
    /// Whether the check's analysis was complete enough to rely on.
    /// With an incomplete analysis, a clean installability result
    /// means only that nothing was found, not that nothing is wrong.
    pub full_analysis: bool,
}

/// One per-bug feedback decision, with the rationale shown in the
/// confirmation plan.
struct BugDecision {
    bug_id: u64,
    title: Option<String>,
    karma: i32,
    note: String,
}

/// Derive the overall karma from the check outcome: -1 when the
/// update breaks something, 0 when the analysis couldn't fully
/// vouch for it, +1 when it came back clean. Returns the karma
/// and the reason shown in the confirmation plan.
pub fn derive_karma(report: &crate::check_update::CheckUpdateReport) -> (i32, String) {
    let broken: Vec<&str> = report
        .reverse_deps
        .iter()
        .filter(|(_, r)| r.status == "broken")
        .map(|(name, _)| name.as_str())
        .collect();
    if !broken.is_empty() {
        return (-1, format!("broken reverse deps: {}", broken.join(", ")));
    }
    if !report.installability_issues.is_empty() {
        return (-1, "updated packages have unsatisfied deps".to_string());
    }
    if !report.full_analysis {
        return (0, "no full provides analysis was possible".to_string());
    }
    if !report.stale_side_tag.is_empty() {
        return (0, "analysis ran on stale side-tag repodata".to_string());
    }
    (
        1,
        format!(
            "no issues found ({} reverse dependencies checked)",
            report.reverse_deps.len()
        ),
    )
}

/// Format karma with an explicit sign, the way Bodhi displays it.
fn fmt_karma(karma: i32) -> String {
    if karma > 0 {
        format!("+{karma}")
    } else {
        karma.to_string()
    }
}

/// What the automatic check concluded about one bug.
pub(crate) enum Verdict {
    /// Decided from what the update contains.
    Decided { karma: i32, reason: String },
    /// The bug names a package the update builds nothing for, so the
    /// update does not fix it. Suggested rather than applied: only
    /// bugs that name their package outright reach this, but a
    /// component can still be stale, so the user is asked.
    Missing { reason: String },
    /// Not a kind we classify. The user decides, with no suggestion.
    Unknown,
}

/// The package a bug names outright, if it is a kind that does: a
/// review request names the package under review in its title, and
/// an update request's package is its Bugzilla component, confirmed
/// by the summary parsing as `<component>-<version> is available`.
///
/// `None` for anything else. A CVE or FTBFS bug is not classified
/// here and its component can be stale after a package rename, so
/// nothing may be concluded from it not matching a build. This is
/// the same rule [`bug_verdict`] applies before it compares
/// versions.
pub(crate) fn bug_package(title: &str, component: Option<&str>) -> Option<String> {
    if let Some(pkg) = sandogasa_bugclass::bugzilla::review_request_package(title) {
        return Some(pkg);
    }
    let pkg = component?;
    sandogasa_bugclass::bugzilla::extract_new_version(title, pkg).map(|_| pkg.to_string())
}

/// Decide automatic feedback for one bug.
///
/// Four kinds are judged, and each names the package it concerns, so
/// none of this has to infer one from a summary:
///
/// - a review request (`"Review Request: <pkg> - ..."`) is answered
///   by the update shipping the package under review;
/// - an FTBFS bug — recognized by blocking the target release's FTBFS
///   tracker — is refuted outright by a successful build of that
///   package being in the update. That is the strongest verdict here:
///   the bug says the package does not build, and the build exists;
/// - a FailsToInstall bug is answered by the check's own
///   installability analysis, which resolves exactly what the bug
///   complains about. Weaker than the FTBFS case, since resolving
///   against an assembled repo set predicts what dnf would do rather
///   than observing it;
/// - a release-monitoring request (`"<pkg>-<version> is available"`)
///   is +1 when the update's build of that component delivers at
///   least the requested version, -1 when it delivers less.
///
/// A kind whose package the update builds nothing for gives
/// `Missing`. Anything else is `Unknown`: a CVE or a plain bug report
/// is not classified here, and its component may be stale after a
/// package rename, so a missing match means nothing.
pub(crate) fn bug_verdict(bug: &BugFacts, update: &UpdateFacts<'_>) -> Verdict {
    let title = bug.summary.as_str();
    let builds = update.builds;

    // A package review is answered by shipping the package under
    // review; there is no version in the title to compare.
    if let Some(pkg) = sandogasa_bugclass::bugzilla::review_request_package(title) {
        return match builds.iter().find(|(p, _)| *p == pkg) {
            Some((p, v)) => Verdict::Decided {
                karma: 1,
                reason: format!("update ships {p}-{v}, the package under review"),
            },
            None => Verdict::Missing {
                reason: format!("update builds no {pkg}, the package under review"),
            },
        };
    }

    // FTBFS and FailsToInstall come from two signals, either of which
    // identifies the release as well as the kind — see `tracked_kind`.
    // Both are keyed on the bug's component, which for these is the
    // package that fails.
    match tracked_kind(bug, update) {
        Some(sandogasa_bugclass::BugKind::Ftbfs) => {
            return match bug.component.as_deref() {
                Some(pkg) => match builds.iter().find(|(p, _)| p == pkg) {
                    Some((p, v)) => Verdict::Decided {
                        karma: 1,
                        reason: format!(
                            "{p}-{v} built for this release, so it is not failing to build"
                        ),
                    },
                    None => Verdict::Missing {
                        reason: format!("update builds no {pkg}, which is what fails to build"),
                    },
                },
                None => Verdict::Unknown,
            };
        }
        Some(sandogasa_bugclass::BugKind::Fti) => {
            return fti_verdict(bug.component.as_deref(), update);
        }
        _ => {}
    }

    // The component names the package, so the summary only has to
    // yield its version, and the build is found by an exact name
    // match. Nothing here has to guess where the name ends.
    let Some(pkg) = bug.component.as_deref() else {
        return Verdict::Unknown;
    };
    let Some(bug_version) = sandogasa_bugclass::bugzilla::extract_new_version(title, pkg) else {
        return Verdict::Unknown;
    };
    let Some((_, build_version)) = builds.iter().find(|(p, _)| p == pkg) else {
        return Verdict::Missing {
            reason: format!("update builds no {pkg}"),
        };
    };
    let addressed = sandogasa_rpmvercmp::rpmvercmp(build_version, &bug_version) != Ordering::Less;
    Verdict::Decided {
        karma: if addressed { 1 } else { -1 },
        reason: if addressed {
            format!("update delivers {pkg}-{build_version} >= {bug_version}")
        } else {
            format!("update only delivers {pkg}-{build_version} < {bug_version}")
        },
    }
}

/// Whether a bug is an FTBFS or FailsToInstall report for the release
/// this update targets.
///
/// The tracker it blocks is checked first, since that works for any
/// wording including the human-filed bugs that follow none. Failing
/// that, the bots' fixed summary forms name the release themselves —
/// the only signal available for EPEL, which has no trackers.
fn tracked_kind(bug: &BugFacts, update: &UpdateFacts<'_>) -> Option<sandogasa_bugclass::BugKind> {
    let blocks = |tracker: Option<u64>| tracker.is_some_and(|id| bug.blocks.contains(&id));
    if blocks(update.trackers.ftbfs) {
        return Some(sandogasa_bugclass::BugKind::Ftbfs);
    }
    if blocks(update.trackers.fti) {
        return Some(sandogasa_bugclass::BugKind::Fti);
    }
    sandogasa_bugclass::bugzilla::kind_from_summary(
        &bug.summary,
        bug.component.as_deref()?,
        update.branch,
    )
}

/// Answer a FailsToInstall bug from the check's installability
/// analysis.
///
/// The check resolves the updated packages' Requires against the
/// target, which is the same question the bug asks. An unresolved
/// requirement is quoted in the reason so the user can weigh it: the
/// analysis is a prediction of what dnf would do, and it can
/// over-report when a touched capability is also provided elsewhere.
fn fti_verdict(component: Option<&str>, update: &UpdateFacts<'_>) -> Verdict {
    let Some(pkg) = component else {
        return Verdict::Unknown;
    };
    if !update.builds.iter().any(|(p, _)| p == pkg) {
        return Verdict::Missing {
            reason: format!("update builds no {pkg}, which is what fails to install"),
        };
    }
    if let Some(issue) = update.unsatisfied.iter().find(|d| d.package == pkg) {
        return Verdict::Decided {
            karma: -1,
            reason: format!("{pkg} still has an unsatisfied requirement: {}", issue.dep),
        };
    }
    // Nothing found — but "nothing found" is only meaningful if the
    // analysis was able to look properly.
    if !update.full_analysis {
        return Verdict::Unknown;
    }
    Verdict::Decided {
        karma: 1,
        reason: format!("{pkg}'s requirements all resolve on this target"),
    }
}

/// Interpret a karma answer: `+1`/`1`/`+`, `-1`/`-`, `0`, or
/// empty (the caller-chosen default). `None` means unrecognized —
/// ask again.
fn parse_karma_answer(line: &str, default: i32) -> Option<i32> {
    match line.trim() {
        "" => Some(default),
        "0" => Some(0),
        "+1" | "1" | "+" => Some(1),
        "-1" | "-" => Some(-1),
        _ => None,
    }
}

/// A karma value written the way the prompt accepts it, for showing
/// which answer Enter will pick.
fn fmt_karma_answer(karma: i32) -> &'static str {
    match karma {
        1 => "+1",
        -1 => "-1",
        _ => "0",
    }
}

/// Print the update's description so the user has context for
/// the manual per-bug feedback questions that follow.
fn print_update_context(update: &sandogasa_bodhi::models::Update) {
    if let Some(name) = update.display_name.as_deref().filter(|n| !n.is_empty()) {
        eprintln!("\n{name}");
    }
    if let Some(notes) = update.notes.as_deref().filter(|n| !n.trim().is_empty()) {
        eprintln!("update notes:");
        for line in notes.lines() {
            eprintln!("  {line}");
        }
    }
    eprintln!();
}

/// Ask the user for feedback on a bug that couldn't be
/// auto-decided. `reason`, when present, says why `default` is being
/// suggested — an unexplained -1 is worse than no suggestion.
fn prompt_bug_karma(
    bug_id: u64,
    title: &str,
    default: i32,
    reason: Option<&str>,
) -> Result<i32, String> {
    use std::io::{BufRead, Write};
    loop {
        eprintln!("bug #{bug_id}: {title}");
        eprintln!("  https://bugzilla.redhat.com/{bug_id}");
        if let Some(reason) = reason {
            eprintln!("  {reason}");
        }
        eprint!(
            "  feedback? [+1/-1/0, default {}]: ",
            fmt_karma_answer(default)
        );
        std::io::stderr().flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if let Some(karma) = parse_karma_answer(&line, default) {
            return Ok(karma);
        }
        eprintln!("  unrecognized answer; enter +1, -1, or 0");
    }
}

/// Ask for the overall karma, defaulting to what the automated
/// check derived.
fn prompt_overall_karma(default: i32, reason: &str) -> Result<i32, String> {
    use std::io::{BufRead, Write};
    loop {
        eprint!(
            "overall karma? [+1/-1/0, default {} — {reason}]: ",
            fmt_karma(default)
        );
        std::io::stderr().flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if let Some(karma) = parse_karma_answer(&line, default) {
            return Ok(karma);
        }
        eprintln!("unrecognized answer; enter +1, -1, or 0");
    }
}

/// Ask for free-form reviewer notes to include in the posted
/// comment. Empty input means none.
fn prompt_notes() -> Result<Option<String>, String> {
    use std::io::{BufRead, Write};
    eprint!("additional comments to include? [empty for none]: ");
    std::io::stderr().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let trimmed = line.trim();
    Ok((!trimmed.is_empty()).then(|| trimmed.to_string()))
}

/// Compose the comment to post: the rendered report, with the
/// reviewer's notes as a section right under the title, and a
/// provenance footer recording the ebranch version and the
/// command invocation that produced the analysis.
pub fn compose_comment(report: &str, notes: Option<&str>, invocation: &str) -> String {
    let mut out = String::new();
    match report.split_once('\n') {
        // Keep the `# Checking update: ...` title first.
        Some((title, rest)) if title.starts_with('#') => {
            out.push_str(title);
            out.push('\n');
            if let Some(notes) = notes {
                out.push_str(&format!("\n## Reviewer notes\n\n{notes}\n"));
            }
            out.push_str(rest);
        }
        _ => {
            if let Some(notes) = notes {
                out.push_str(&format!("## Reviewer notes\n\n{notes}\n\n"));
            }
            out.push_str(report);
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!(
        "\n---\n*Generated by ebranch {} — `{invocation}`*\n",
        env!("CARGO_PKG_VERSION")
    ));
    out
}

/// Print the vote plan and confirm (default yes).
fn confirm_plan(
    alias: &str,
    karma: i32,
    reason: &str,
    text: &str,
    decisions: &[BugDecision],
) -> Result<bool, String> {
    eprintln!("\nVote plan for {alias}:");
    eprintln!("  overall karma: {} ({reason})", fmt_karma(karma));
    if !text.is_empty() {
        let mut lines = text.lines();
        let first = lines.next().unwrap_or("");
        let rest = lines.count();
        if rest == 0 {
            eprintln!("  comment: {first}");
        } else {
            eprintln!("  comment: {first} (+{rest} more lines)");
        }
    }
    if !decisions.is_empty() {
        eprintln!("  bug feedback:");
        for d in decisions {
            eprintln!(
                "    {:>2} #{} {} ({})",
                fmt_karma(d.karma),
                d.bug_id,
                d.title.as_deref().unwrap_or("<no title>"),
                d.note
            );
        }
    }
    sandogasa_cli::confirm("Post this comment?", true).map_err(|e| e.to_string())
}

/// Ensure a usable bodhi CLI session exists, driving an
/// interactive login through the bodhi CLI if there is none.
///
/// Called before the (potentially long) update analysis so a
/// missing session is caught up front rather than after minutes
/// of fedrq queries. The login itself is delegated to
/// `bodhi overrides query --mine` — a harmless read-only command
/// whose only relevant effect is making bodhi-client run its
/// OIDC browser flow and cache the tokens we then reuse.
pub fn ensure_session() -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let http = http_client();
        let cache = auth::cli_cache_path();
        let first_err = match auth::cli_session_token(&http, &cache, auth::FEDORA_IDP).await {
            Ok(_) => return Ok(()),
            Err(e) => e,
        };
        sandogasa_cli::require_tools(&[(
            "bodhi",
            "sudo dnf install bodhi-client",
            Some("--version"),
        )])?;
        eprintln!("{first_err}");
        eprintln!("starting a bodhi CLI login (`bodhi overrides query --mine`)...");
        let status = std::process::Command::new("bodhi")
            .args(["overrides", "query", "--mine"])
            .status()
            .map_err(|e| format!("cannot run bodhi: {e}"))?;
        if !status.success() {
            return Err(format!("bodhi CLI login failed ({status})"));
        }
        auth::cli_session_token(&http, &cache, auth::FEDORA_IDP)
            .await
            .map(|_| ())
            .map_err(|e| format!("still no valid bodhi session after login: {e}"))
    })
}

/// Look up the session's username for own-update detection,
/// retrying transient failures and giving up gracefully: returns
/// `None` (with a warning) when the lookup keeps failing, since
/// it only gates a client-side nicety.
async fn session_username_with_retry(http: &reqwest::Client) -> Option<String> {
    const ATTEMPTS: u32 = 3;
    let mut last_err = String::new();
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
        }
        let result = async {
            let token =
                auth::cli_session_token(http, &auth::cli_cache_path(), auth::FEDORA_IDP).await?;
            auth::username(http, auth::FEDORA_IDP, &token).await
        }
        .await;
        match result {
            Ok(user) => return Some(user),
            Err(e) => {
                if attempt + 1 < ATTEMPTS {
                    eprintln!("username lookup failed ({e}); retrying...");
                }
                last_err = e;
            }
        }
    }
    eprintln!(
        "warning: could not determine the session username \
         ({last_err}); assuming this is not your own update \
         (Bodhi enforces the own-update karma rule server-side \
         anyway)"
    );
    None
}

/// Fill in bug titles Bodhi hasn't cached yet, straight from
/// Bugzilla (batched). Bodhi populates titles from Bugzilla
/// *asynchronously*, so an update fetched right after creation —
/// the `--submit` flow — lists its bugs with `title: null`, which
/// blinds the release-monitoring auto-vote and degrades the manual
/// prompt to `<no title>`. Best-effort: on a fetch failure the
/// affected bugs just keep their missing titles (with a warning).
async fn backfill_bug_titles(
    bz: &sandogasa_bugzilla::BzClient,
    bugs: &mut [sandogasa_bodhi::models::BodhiBug],
) -> BugMap {
    let ids: Vec<u64> = bugs.iter().map(|b| b.bug_id).collect();
    let fetched = fetch_bugs(bz, &ids).await;
    for bug in bugs.iter_mut() {
        if bug.title.is_none() {
            bug.title = fetched.get(&bug.bug_id).map(|f| f.summary.clone());
        }
    }
    fetched
}

/// Fetch `ids` from Bugzilla in one batch, as
/// `id -> (summary, component)`. Best-effort: a failure warns and
/// yields nothing, leaving callers to fall back on asking the user.
pub(crate) async fn fetch_bugs(bz: &sandogasa_bugzilla::BzClient, ids: &[u64]) -> BugMap {
    if ids.is_empty() {
        return BugMap::new();
    }
    match bz.bugs(ids).await {
        Ok(fetched) => fetched
            .into_iter()
            .map(|bug| {
                (
                    bug.id,
                    BugFacts {
                        summary: bug.summary,
                        component: bug.component.first().cloned(),
                        blocks: bug.blocks,
                    },
                )
            })
            .collect(),
        Err(e) => {
            eprintln!(
                "warning: could not fetch bugs from Bugzilla ({e}); \
                 bugs without a Bodhi-cached title, and update requests \
                 whose component is therefore unknown, need manual feedback"
            );
            BugMap::new()
        }
    }
}

/// Cast karma on a Bodhi update with per-bug feedback. `karma`
/// and `reason` come from [`derive_karma`] on the check report;
/// `report_md` is the rendered report (the posted comment body)
/// and `notes` the `--comment` flag (prompted for interactively
/// when absent).
pub fn run(
    alias: &str,
    report: &crate::check_update::CheckUpdateReport,
    report_md: &str,
    notes: Option<String>,
    assume_yes: bool,
) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(run_async(alias, report, report_md, notes, assume_yes))
}

async fn run_async(
    alias: &str,
    report: &crate::check_update::CheckUpdateReport,
    report_md: &str,
    notes: Option<String>,
    assume_yes: bool,
) -> Result<(), String> {
    let (karma, reason) = derive_karma(report);
    let reason = reason.as_str();
    let client = BodhiClient::new();
    let mut update = client
        .update_by_alias(alias)
        .await
        .map_err(|e| format!("cannot fetch update {alias}: {e}"))?;
    // Bodhi's bug tracker is Red Hat Bugzilla (the same instance
    // the manual prompt links to); public summaries need no auth.
    let bz = sandogasa_bugzilla::BzClient::new("https://bugzilla.redhat.com");
    let bugzilla = backfill_bug_titles(&bz, &mut update.bugs).await;
    // Which release's FTBFS / FailsToInstall trackers to recognize.
    // A bug blocking another release's tracker is not this update's
    // business, and EPEL has no such trackers at all, so those bugs
    // simply reach no verdict here.
    let trackers = sandogasa_bugclass::bugzilla::lookup_branch_trackers(&bz, &report.branch).await;
    let update = update;

    // Bodhi zeroes overall karma from the submitter on their own
    // updates (per-bug feedback still counts), so don't pretend
    // we are casting one. The lookup is best-effort with retries:
    // a transient failure here must not abort a vote the user
    // already spent minutes of analysis on — Bodhi enforces the
    // own-update rule server-side regardless (we'd just echo its
    // caveat instead of pre-empting it).
    let http = http_client();
    let session_user = session_username_with_retry(&http).await;
    let own_update = match (&session_user, &update.user) {
        (Some(session), Some(submitter)) => *session == submitter.name,
        _ => false,
    };
    let (karma, reason) = if own_update && karma != 0 {
        (
            0,
            format!(
                "own update — Bodhi ignores submitter karma; was {}",
                fmt_karma(karma)
            ),
        )
    } else {
        (karma, reason.to_string())
    };
    let reason = reason.as_str();

    // (source package, version) for each build in the update.
    let builds: Vec<(String, String)> = update
        .builds
        .iter()
        .filter_map(|b| sandogasa_koji::parse_nvr(&b.nvr))
        .map(|(n, v, _)| (n.to_string(), v.to_string()))
        .collect();

    let facts = UpdateFacts {
        builds: &builds,
        trackers: &trackers,
        branch: &report.branch,
        unsatisfied: &report.installability_issues,
        full_analysis: report.full_analysis,
    };

    let mut decisions = Vec::new();
    // Bugs to put to the user, each with the answer Enter will take
    // and, when there is one, why it is being suggested.
    let mut manual: Vec<(&sandogasa_bodhi::models::BodhiBug, i32, Option<String>)> = Vec::new();
    for bug in &update.bugs {
        // Bodhi's cached title is used when Bugzilla could not be
        // reached, so a bug still gets its update-request verdict from
        // the summary alone where possible.
        let verdict = match (bugzilla.get(&bug.bug_id), bug.title.as_deref()) {
            (Some(known), _) => bug_verdict(known, &facts),
            (None, Some(title)) => bug_verdict(
                &BugFacts {
                    summary: title.to_string(),
                    ..BugFacts::default()
                },
                &facts,
            ),
            (None, None) => Verdict::Unknown,
        };
        match verdict {
            Verdict::Decided { karma, reason } => decisions.push(BugDecision {
                bug_id: bug.bug_id,
                title: bug.title.clone(),
                karma,
                note: reason,
            }),
            // The update ships nothing for this bug's package, so it
            // does not fix it. Answering 0 would post a claim we have
            // reason to believe is wrong, so -1 is what `--yes` takes
            // and what Enter picks.
            Verdict::Missing { reason } if assume_yes => decisions.push(BugDecision {
                bug_id: bug.bug_id,
                title: bug.title.clone(),
                karma: -1,
                note: reason,
            }),
            Verdict::Missing { reason } => manual.push((bug, -1, Some(reason))),
            Verdict::Unknown if assume_yes => decisions.push(BugDecision {
                bug_id: bug.bug_id,
                title: bug.title.clone(),
                karma: 0,
                note: "no automatic verdict; skipped under --yes".to_string(),
            }),
            Verdict::Unknown => manual.push((bug, 0, None)),
        }
    }
    if !manual.is_empty() {
        // Show what the update says about itself before asking
        // the user to judge its bugs.
        print_update_context(&update);
        for (bug, default, reason) in manual {
            let title = bug.title.as_deref().unwrap_or("<no title>");
            let bug_karma = prompt_bug_karma(bug.bug_id, title, default, reason.as_deref())?;
            decisions.push(BugDecision {
                bug_id: bug.bug_id,
                title: bug.title.clone(),
                karma: bug_karma,
                // Keep the reason when the user took the suggestion,
                // so the plan and the posted comment say why.
                note: match reason {
                    Some(reason) if bug_karma == default => reason,
                    _ => "manual".to_string(),
                },
            });
        }
    }

    // Reviewer notes: the flag wins; otherwise ask (the report
    // is already on stdout for reference). --yes skips the
    // prompt.
    let notes = match notes {
        Some(n) => Some(n),
        None if assume_yes => None,
        None => prompt_notes()?,
    };
    let invocation = std::iter::once("ebranch".to_string())
        .chain(std::env::args().skip(1))
        .collect::<Vec<_>>()
        .join(" ");
    let text = compose_comment(report_md, notes.as_deref(), &invocation);
    let text = text.as_str();

    // Let the user override the derived karma (Enter accepts the
    // suggestion). Pointless on own updates, where Bodhi ignores
    // submitter karma regardless.
    let (karma, reason) = if assume_yes || own_update {
        (karma, reason.to_string())
    } else {
        let chosen = prompt_overall_karma(karma, reason)?;
        if chosen == karma {
            (karma, reason.to_string())
        } else {
            (
                chosen,
                format!("manual override; checks suggested {}", fmt_karma(karma)),
            )
        }
    };
    let reason = reason.as_str();

    if !assume_yes && !confirm_plan(alias, karma, reason, text, &decisions)? {
        return Err("aborted: comment not posted".to_string());
    }

    // Refresh preemptively: the analysis may have run for long
    // enough that a token that was valid at the start is close to
    // (or past) expiry by the time we post.
    let token =
        auth::cli_session_token_refreshed(&http, &auth::cli_cache_path(), auth::FEDORA_IDP).await?;
    let client = client.with_token(token).map_err(|e| e.to_string())?;

    let feedback: Vec<BugFeedbackItem> = decisions
        .iter()
        .map(|d| BugFeedbackItem {
            bug_id: d.bug_id,
            karma: d.karma,
        })
        .collect();
    let resp = client
        .comment(alias, text, karma, &feedback)
        .await
        .map_err(|e| e.to_string())?;
    for caveat in &resp.caveats {
        eprintln!("note from bodhi: {}", caveat.description);
    }
    eprintln!(
        "posted: https://bodhi.fedoraproject.org/updates/{}#comment-{}",
        alias, resp.comment.id
    );
    Ok(())
}

/// Upper bound on any single Bodhi HTTP request — a hang-catcher rather
/// than a latency cap (reqwest's default client has no timeout).
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Build the karma/submit flows' HTTP client with the standard
/// request timeout. Panics only where `Client::new()` would too.
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .expect("build reqwest client")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builds() -> Vec<(String, String)> {
        vec![
            ("rust-quick-xml".to_string(), "0.40.1".to_string()),
            ("fish".to_string(), "4.0.0".to_string()),
        ]
    }

    fn report(
        full_analysis: bool,
        broken: bool,
        installability: bool,
        stale: bool,
    ) -> crate::check_update::CheckUpdateReport {
        use crate::check_update::*;
        let mut reverse_deps = std::collections::BTreeMap::new();
        reverse_deps.insert(
            "dep-ok".to_string(),
            RevDepResult {
                status: "ok".to_string(),
                issues: vec![],
            },
        );
        if broken {
            reverse_deps.insert(
                "dep-broken".to_string(),
                RevDepResult {
                    status: "broken".to_string(),
                    issues: vec![],
                },
            );
        }
        CheckUpdateReport {
            input: "FEDORA-2026-test".to_string(),
            branch: "f44".to_string(),
            repo: None,
            updated_packages: vec!["fish".to_string()],
            changes: vec![],
            full_analysis,
            changed_provides: vec![],
            installability_issues: if installability {
                vec![UnsatisfiedDep {
                    package: "fish".to_string(),
                    dep: "libfoo".to_string(),
                    unresolved: vec![],
                }]
            } else {
                vec![]
            },
            stale_side_tag: if stale {
                vec![StaleSideTag {
                    package: "fish".to_string(),
                    expected_nvr: "fish-4.0.0-1.fc44".to_string(),
                    actual_vr: None,
                }]
            } else {
                vec![]
            },
            skip_reason: None,
            reverse_deps,
        }
    }

    #[test]
    fn derive_karma_clean_check_upvotes() {
        let (karma, reason) = derive_karma(&report(true, false, false, false));
        assert_eq!(karma, 1);
        assert!(reason.contains("no issues"), "{reason}");
    }

    #[test]
    fn derive_karma_broken_rev_deps_downvote() {
        let (karma, reason) = derive_karma(&report(true, true, false, false));
        assert_eq!(karma, -1);
        assert!(reason.contains("dep-broken"), "{reason}");
    }

    #[test]
    fn derive_karma_installability_downvotes() {
        let (karma, _) = derive_karma(&report(true, false, true, false));
        assert_eq!(karma, -1);
    }

    #[test]
    fn derive_karma_after_curation_lifts_explained_installability() {
        use crate::check_update::{apply_resolutions, blocking_findings};
        use sandogasa_review::Resolution;
        // Raw: one installability issue → -1.
        let raw = report(true, false, true, false);
        assert_eq!(derive_karma(&raw).0, -1);
        // Reviewer explains it away → curated report derives +1.
        let decisions = blocking_findings(&raw)
            .into_iter()
            .map(|f| (f, Resolution::Explained("satisfied at runtime".to_string())))
            .collect();
        let (curated, addressed) = apply_resolutions(raw, decisions);
        assert_eq!(addressed.len(), 1);
        assert_eq!(derive_karma(&curated).0, 1);
    }

    #[test]
    fn derive_karma_incomplete_analysis_neutral() {
        // No side tag / @testing: reverse deps listed only.
        assert_eq!(derive_karma(&report(false, false, false, false)).0, 0);
        // Stale side-tag data carried into the analysis.
        assert_eq!(derive_karma(&report(true, false, false, true)).0, 0);
    }

    /// A bug as Bugzilla would describe it.
    fn bug(summary: &str, component: Option<&str>) -> BugFacts {
        BugFacts {
            summary: summary.to_string(),
            component: component.map(str::to_string),
            blocks: Vec::new(),
        }
    }

    /// A bug that blocks the target release's FTBFS or
    /// FailsToInstall tracker.
    fn tracked_bug(summary: &str, component: &str, tracker: u64) -> BugFacts {
        BugFacts {
            blocks: vec![tracker],
            ..bug(summary, Some(component))
        }
    }

    const FTBFS_TRACKER: u64 = 2339432;
    const FTI_TRACKER: u64 = 2339435;

    fn trackers() -> sandogasa_bugclass::bugzilla::BranchTrackers {
        sandogasa_bugclass::bugzilla::BranchTrackers {
            ftbfs: Some(FTBFS_TRACKER),
            fti: Some(FTI_TRACKER),
        }
    }

    /// The update side of a verdict: the builds, this release's
    /// trackers, and a clean installability result.
    fn update<'a>(
        builds: &'a [(String, String)],
        trackers: &'a sandogasa_bugclass::bugzilla::BranchTrackers,
        unsatisfied: &'a [crate::check_update::UnsatisfiedDep],
    ) -> UpdateFacts<'a> {
        UpdateFacts {
            builds,
            trackers,
            branch: "f44",
            unsatisfied,
            full_analysis: true,
        }
    }

    /// The `(karma, reason)` of a decided verdict, for the tests
    /// that only care about a decided outcome.
    fn decided(verdict: Verdict) -> Option<(i32, String)> {
        match verdict {
            Verdict::Decided { karma, reason } => Some((karma, reason)),
            _ => None,
        }
    }

    /// Whether the verdict is "the update ships nothing for this
    /// bug's package", and its reason.
    fn missing(verdict: Verdict) -> Option<String> {
        match verdict {
            Verdict::Missing { reason } => Some(reason),
            _ => None,
        }
    }

    fn is_unknown(verdict: Verdict) -> bool {
        matches!(verdict, Verdict::Unknown)
    }

    #[test]
    fn bug_verdict_upvotes_exact_version() {
        let (karma, note) = decided(bug_verdict(
            &bug("rust-quick-xml-0.40.1 is available", Some("rust-quick-xml")),
            &update(&builds(), &trackers(), &[]),
        ))
        .unwrap();
        assert_eq!(karma, 1);
        assert!(note.contains("0.40.1"), "{note}");
    }

    #[test]
    fn bug_verdict_upvotes_newer_build() {
        // The update delivers more than the bug asked for.
        let (karma, _) = decided(bug_verdict(
            &bug("fish-3.9.0 is available", Some("fish")),
            &update(&builds(), &trackers(), &[]),
        ))
        .unwrap();
        assert_eq!(karma, 1);
    }

    #[test]
    fn bug_verdict_downvotes_version_mismatch() {
        let (karma, note) = decided(bug_verdict(
            &bug("rust-quick-xml-0.41.0 is available", Some("rust-quick-xml")),
            &update(&builds(), &trackers(), &[]),
        ))
        .unwrap();
        assert_eq!(karma, -1);
        assert!(note.contains('<'), "{note}");
    }

    #[test]
    fn bug_verdict_upvotes_package_review() {
        // A newpackage update answers the review request for the
        // package it ships.
        let (karma, note) = decided(bug_verdict(
            &bug(
                "Review Request: rust-quick-xml - High performance XML reader and writer",
                Some("Package Review"),
            ),
            &update(&builds(), &trackers(), &[]),
        ))
        .unwrap();
        assert_eq!(karma, 1);
        assert!(note.contains("rust-quick-xml-0.40.1"), "{note}");
    }

    #[test]
    fn bug_verdict_flags_a_review_for_a_package_not_built() {
        // The package under review is named in the title, so an
        // update that does not build it cannot be answering the
        // review — suggest -1 rather than saying nothing.
        let reason = missing(bug_verdict(
            &bug(
                "Review Request: rust-macrotest - Testing macros",
                Some("Package Review"),
            ),
            &update(&builds(), &trackers(), &[]),
        ))
        .expect("expected a Missing verdict");
        assert!(reason.contains("rust-macrotest"), "{reason}");
    }

    #[test]
    fn bug_verdict_flags_an_update_request_for_a_package_not_built() {
        let reason = missing(bug_verdict(
            &bug("rust-dtor-1.0.5 is available", Some("rust-dtor")),
            &update(&builds(), &trackers(), &[]),
        ))
        .expect("expected a Missing verdict");
        assert!(reason.contains("rust-dtor"), "{reason}");
    }

    #[test]
    fn bug_verdict_needs_the_component_for_an_update_request() {
        // Bodhi does not carry the component; when the Bugzilla
        // fetch fails there is nothing to anchor the summary on, so
        // the bug goes to the user with no suggestion.
        assert!(is_unknown(bug_verdict(
            &bug("rust-quick-xml-0.40.1 is available", None),
            &update(&builds(), &trackers(), &[]),
        )));
    }

    #[test]
    fn bug_verdict_uses_the_component_not_the_title() {
        // rust-ctor is a prefix of the bug's package but a different
        // component, and it is the component that decides. The
        // update does not build rust-ctor-proc-macro, so this is a
        // Missing, not a wrong +1 against rust-ctor.
        let reason = missing(bug_verdict(
            &bug(
                "rust-ctor-proc-macro-0.0.13 is available",
                Some("rust-ctor-proc-macro"),
            ),
            &update(
                &[("rust-ctor".to_string(), "1.0.8".to_string())],
                &trackers(),
                &[],
            ),
        ))
        .expect("expected a Missing verdict");
        assert!(reason.contains("rust-ctor-proc-macro"), "{reason}");
    }

    #[test]
    fn bug_verdict_refutes_an_ftbfs_bug_with_the_build() {
        // The bug says the package does not build on this release.
        // A build of it in the update is the artifact the bug claims
        // cannot exist, so this is proof rather than inference.
        let (karma, note) = decided(bug_verdict(
            &tracked_bug(
                "rust-quick-xml fails to build with serde 1.0.220",
                "rust-quick-xml",
                FTBFS_TRACKER,
            ),
            &update(&builds(), &trackers(), &[]),
        ))
        .unwrap();
        assert_eq!(karma, 1);
        assert!(note.contains("rust-quick-xml-0.40.1"), "{note}");
    }

    #[test]
    fn bug_verdict_ignores_an_ftbfs_bug_for_another_release() {
        // Blocking a different release's tracker means the bug is not
        // about the release this update targets. The summary says
        // nothing about a release, so the tracker is the only signal
        // and a non-match has to mean silence.
        let other_release_tracker = 999_999;
        assert!(is_unknown(bug_verdict(
            &tracked_bug(
                "rust-quick-xml fails to build",
                "rust-quick-xml",
                other_release_tracker
            ),
            &update(&builds(), &trackers(), &[]),
        )));
    }

    #[test]
    fn bug_verdict_flags_an_ftbfs_bug_for_a_package_not_built() {
        let reason = missing(bug_verdict(
            &tracked_bug("zsh fails to build", "zsh", FTBFS_TRACKER),
            &update(&builds(), &trackers(), &[]),
        ))
        .expect("expected a Missing verdict");
        assert!(reason.contains("zsh"), "{reason}");
    }

    #[test]
    fn bug_verdict_answers_fti_from_the_installability_check() {
        // The check resolves exactly what an FTI bug complains about.
        let (karma, note) = decided(bug_verdict(
            &tracked_bug("fish fails to install", "fish", FTI_TRACKER),
            &update(&builds(), &trackers(), &[]),
        ))
        .unwrap();
        assert_eq!(karma, 1);
        assert!(note.contains("fish"), "{note}");
    }

    #[test]
    fn bug_verdict_downvotes_fti_naming_the_unresolved_requirement() {
        // Still broken, and the reason quotes the requirement so the
        // user can weigh it — the analysis predicts what dnf would
        // do rather than observing it, and can over-report.
        let unsatisfied = [crate::check_update::UnsatisfiedDep {
            package: "fish".to_string(),
            dep: "libpcre2-32.so.0()(64bit)".to_string(),
            unresolved: vec![],
        }];
        let (karma, note) = decided(bug_verdict(
            &tracked_bug("fish fails to install", "fish", FTI_TRACKER),
            &update(&builds(), &trackers(), &unsatisfied),
        ))
        .unwrap();
        assert_eq!(karma, -1);
        assert!(note.contains("libpcre2-32.so.0"), "{note}");
    }

    #[test]
    fn bug_verdict_will_not_clear_fti_on_an_incomplete_analysis() {
        // A clean result only means "nothing found", which is not the
        // same as "nothing wrong" when the analysis could not run
        // fully.
        let builds = builds();
        let facts = UpdateFacts {
            builds: &builds,
            trackers: &trackers(),
            branch: "f44",
            unsatisfied: &[],
            full_analysis: false,
        };
        assert!(is_unknown(bug_verdict(
            &tracked_bug("fish fails to install", "fish", FTI_TRACKER),
            &facts,
        )));
    }

    #[test]
    fn bug_verdict_falls_back_to_the_bots_wording_without_trackers() {
        // EPEL has no trackers and a lookup can fail, but the bots'
        // fixed wording names the release itself. Real summaries from
        // bugs 2433898 and 2437417.
        let none = sandogasa_bugclass::bugzilla::BranchTrackers::default();
        let (karma, _) = decided(bug_verdict(
            &bug(
                "rust-quick-xml: FTBFS in Fedora rawhide/f44",
                Some("rust-quick-xml"),
            ),
            &update(&builds(), &none, &[]),
        ))
        .expect("the bot's FTBFS wording should classify without a tracker");
        assert_eq!(karma, 1);

        let (karma, _) = decided(bug_verdict(
            &bug("F44FailsToInstall: fish", Some("fish")),
            &update(&builds(), &none, &[]),
        ))
        .expect("the bot's FTI wording should classify without a tracker");
        assert_eq!(karma, 1);
    }

    #[test]
    fn bug_verdict_ignores_human_wording_without_a_tracker() {
        // Human-filed FTBFS bugs follow no form and name no release,
        // so with no tracker there is nothing to go on.
        let none = sandogasa_bugclass::bugzilla::BranchTrackers::default();
        assert!(is_unknown(bug_verdict(
            &bug(
                "rust-quick-xml fails to build with serde 1.0.220",
                Some("rust-quick-xml")
            ),
            &update(&builds(), &none, &[]),
        )));
    }

    #[test]
    fn bug_verdict_says_nothing_about_unclassified_bugs() {
        // A CVE or a plain bug report names no package we can check,
        // and its component may be stale after a rename — so no
        // suggestion, in either direction.
        assert!(is_unknown(bug_verdict(
            &bug("fish crashes on startup", Some("fish")),
            &update(&builds(), &trackers(), &[]),
        )));
        assert!(is_unknown(bug_verdict(
            &bug("CVE-2026-1234 fish: overflow", Some("fish")),
            &update(&builds(), &trackers(), &[]),
        )));
    }

    #[test]
    fn bug_package_names_only_the_kinds_we_classify() {
        assert_eq!(
            bug_package("rust-dtor-1.0.5 is available", Some("rust-dtor")).as_deref(),
            Some("rust-dtor")
        );
        assert_eq!(
            bug_package(
                "Review Request: rust-macrotest - Testing",
                Some("Package Review")
            )
            .as_deref(),
            Some("rust-macrotest")
        );
        assert_eq!(
            bug_package("CVE-2026-1234 fish: overflow", Some("fish")),
            None
        );
        assert_eq!(bug_package("fish crashes on startup", Some("fish")), None);
    }

    #[test]
    fn parse_karma_answer_variants() {
        // Empty input takes the caller's default.
        assert_eq!(parse_karma_answer("", 0), Some(0));
        assert_eq!(parse_karma_answer("", 1), Some(1));
        assert_eq!(parse_karma_answer("\n", -1), Some(-1));
        // Explicit answers win over the default.
        assert_eq!(parse_karma_answer("0", 1), Some(0));
        assert_eq!(parse_karma_answer("+1", 0), Some(1));
        assert_eq!(parse_karma_answer("+", 0), Some(1));
        assert_eq!(parse_karma_answer("1", 0), Some(1));
        assert_eq!(parse_karma_answer("-1", 1), Some(-1));
        assert_eq!(parse_karma_answer("-", 1), Some(-1));
        assert_eq!(parse_karma_answer("maybe", 1), None);
    }

    #[test]
    fn compose_comment_inserts_notes_under_title_and_footer() {
        let report = "# Checking update: FEDORA-2026-x\n\n**Branch:** f44\n";
        let out = compose_comment(report, Some("LGTM, smoke-tested"), "ebranch check-update x");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "# Checking update: FEDORA-2026-x");
        assert_eq!(lines[2], "## Reviewer notes");
        assert_eq!(lines[4], "LGTM, smoke-tested");
        assert!(out.contains("**Branch:** f44"));
        let footer = lines.last().unwrap();
        assert!(footer.contains(env!("CARGO_PKG_VERSION")), "{footer}");
        assert!(footer.contains("`ebranch check-update x`"), "{footer}");
    }

    #[test]
    fn compose_comment_without_notes_keeps_report_plus_footer() {
        let report = "# Checking update: FEDORA-2026-x\n\nbody\n";
        let out = compose_comment(report, None, "ebranch check-update x");
        assert!(out.starts_with("# Checking update: FEDORA-2026-x\n\nbody\n"));
        assert!(!out.contains("Reviewer notes"));
        assert!(out.contains("Generated by ebranch"));
    }

    #[test]
    fn fmt_karma_signs() {
        assert_eq!(fmt_karma(1), "+1");
        assert_eq!(fmt_karma(0), "0");
        assert_eq!(fmt_karma(-1), "-1");
    }

    /// BodhiBug is #[non_exhaustive]; construct via serde.
    fn bodhi_bug(bug_id: u64, title: Option<&str>) -> sandogasa_bodhi::models::BodhiBug {
        serde_json::from_value(serde_json::json!({"bug_id": bug_id, "title": title})).unwrap()
    }

    #[tokio::test]
    async fn backfill_bug_titles_keeps_cached_titles() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Every bug is requested — components are never cached by
        // Bodhi — but a title Bodhi already has is not overwritten.
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .and(query_param("id", "100"))
            .and(query_param("id", "200"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [{
                    "id": 100,
                    "summary": "tree-sitter-fsharp-0.3.1 is available",
                    "status": "NEW",
                    "resolution": "",
                    "product": "Fedora",
                    "component": ["rust-tree-sitter-fsharp"],
                    "severity": "unspecified",
                    "priority": "unspecified",
                    "assigned_to": "nobody",
                    "creator": "upstream-release-monitoring",
                    "creation_time": "2026-07-01T10:00:00Z",
                    "last_change_time": "2026-07-01T10:00:00Z"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let mut bugs = vec![bodhi_bug(100, None), bodhi_bug(200, Some("cached title"))];
        let components = backfill_bug_titles(&bz, &mut bugs).await;
        assert_eq!(
            bugs[0].title.as_deref(),
            Some("tree-sitter-fsharp-0.3.1 is available")
        );
        assert_eq!(bugs[1].title.as_deref(), Some("cached title"));
        // The component came back for the bug Bugzilla returned.
        assert_eq!(
            components.get(&100).and_then(|f| f.component.as_deref()),
            Some("rust-tree-sitter-fsharp")
        );
    }

    #[tokio::test]
    async fn backfill_bug_titles_survives_fetch_failure() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Fetch failure: titles stay missing, no components, no panic.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let mut bugs = vec![bodhi_bug(100, None)];
        let components = backfill_bug_titles(&bz, &mut bugs).await;
        assert!(bugs[0].title.is_none());
        assert!(components.is_empty());
    }

    #[tokio::test]
    async fn backfill_bug_titles_fetches_components_for_cached_titles() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // Bodhi caches titles but never components, so a cached
        // title is no reason to skip the fetch: without the
        // component an update request cannot be judged at all.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [{
                    "id": 200,
                    "summary": "cached",
                    "status": "NEW",
                    "resolution": "",
                    "product": "Fedora",
                    "component": ["rust-md-5"],
                    "severity": "",
                    "priority": "",
                    "assigned_to": "",
                    "creator": "",
                    "creation_time": "2026-01-01T00:00:00Z",
                    "last_change_time": "2026-01-01T00:00:00Z",
                    "keywords": [],
                    "blocks": [],
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let bz = sandogasa_bugzilla::BzClient::new(&server.uri());
        let mut bugs = vec![bodhi_bug(200, Some("cached"))];
        let components = backfill_bug_titles(&bz, &mut bugs).await;
        assert_eq!(bugs[0].title.as_deref(), Some("cached"));
        assert_eq!(
            components.get(&200).and_then(|f| f.component.as_deref()),
            Some("rust-md-5")
        );
    }
}
