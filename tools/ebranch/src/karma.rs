// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Karma voting on Bodhi updates (`check-update --give-karma`).
//!
//! Posts a comment with overall karma and per-bug feedback, the
//! way the Bodhi web UI does. Release-monitoring update-request
//! bugs ("<pkg>-<version> is available") are auto-voted by
//! comparing the requested version against what the update's
//! builds deliver; anything else is put to the user.
//!
//! Authentication reuses the bodhi CLI's cached OIDC session
//! (see `sandogasa_bodhi::auth`) — authenticate once with any
//! authenticated `bodhi` command and this flow shares the tokens.

use std::cmp::Ordering;

use sandogasa_bodhi::models::BugFeedbackItem;
use sandogasa_bodhi::{BodhiClient, auth};

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

/// Decide automatic feedback for one bug title against the
/// update's builds (`(source name, version)` pairs).
///
/// A release-monitoring title `"<pkg>-<version> is available"`
/// matching one of the update's packages gets +1 when the build
/// delivers at least the requested version and -1 otherwise.
/// Returns `None` for anything else — the caller should ask the
/// user.
fn auto_bug_karma(title: &str, builds: &[(String, String)]) -> Option<(i32, String)> {
    for (pkg, build_version) in builds {
        let Some(bug_version) = sandogasa_bugclass::bugzilla::extract_new_version(title, pkg)
        else {
            continue;
        };
        let addressed =
            sandogasa_rpmvercmp::rpmvercmp(build_version, &bug_version) != Ordering::Less;
        let note = if addressed {
            format!("update delivers {pkg}-{build_version} >= {bug_version}")
        } else {
            format!("update only delivers {pkg}-{build_version} < {bug_version}")
        };
        return Some((if addressed { 1 } else { -1 }, note));
    }
    None
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
/// auto-decided.
fn prompt_bug_karma(bug_id: u64, title: &str) -> Result<i32, String> {
    use std::io::{BufRead, Write};
    loop {
        eprintln!("bug #{bug_id}: {title}");
        eprintln!("  https://bugzilla.redhat.com/{bug_id}");
        eprint!("  feedback? [+1/-1/0, default 0]: ");
        std::io::stderr().flush().map_err(|e| e.to_string())?;
        let mut line = String::new();
        std::io::stdin()
            .lock()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if let Some(karma) = parse_karma_answer(&line, 0) {
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
    use std::io::{BufRead, Write};
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
    eprint!("Post this comment? [Y/n]: ");
    std::io::stderr().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let answer = line.trim();
    Ok(answer.is_empty() || answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
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
        let http = reqwest::Client::new();
        let cache = auth::cli_cache_path();
        let first_err = match auth::cli_session_token(&http, &cache, auth::FEDORA_IDP).await {
            Ok(_) => return Ok(()),
            Err(e) => e,
        };
        sandogasa_cli::require_tool("bodhi", "sudo dnf install bodhi-client")?;
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

/// Cast karma on a Bodhi update with per-bug feedback. `karma`
/// and `reason` come from [`derive_karma`] on the check report;
/// `report_md` is the rendered report (the posted comment body)
/// and `notes` the `--comment` flag (prompted for interactively
/// when absent).
pub fn run(
    alias: &str,
    karma: i32,
    reason: &str,
    report_md: &str,
    notes: Option<String>,
    assume_yes: bool,
) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(run_async(
        alias, karma, reason, report_md, notes, assume_yes,
    ))
}

async fn run_async(
    alias: &str,
    karma: i32,
    reason: &str,
    report_md: &str,
    notes: Option<String>,
    assume_yes: bool,
) -> Result<(), String> {
    let client = BodhiClient::new();
    let update = client
        .update_by_alias(alias)
        .await
        .map_err(|e| format!("cannot fetch update {alias}: {e}"))?;

    // Bodhi zeroes overall karma from the submitter on their own
    // updates (per-bug feedback still counts), so don't pretend
    // we are casting one. The lookup is best-effort with retries:
    // a transient failure here must not abort a vote the user
    // already spent minutes of analysis on — Bodhi enforces the
    // own-update rule server-side regardless (we'd just echo its
    // caveat instead of pre-empting it).
    let http = reqwest::Client::new();
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

    let mut decisions = Vec::new();
    let mut manual = Vec::new();
    for bug in &update.bugs {
        let auto = bug
            .title
            .as_deref()
            .and_then(|title| auto_bug_karma(title, &builds));
        match auto {
            Some((bug_karma, note)) => decisions.push(BugDecision {
                bug_id: bug.bug_id,
                title: bug.title.clone(),
                karma: bug_karma,
                note,
            }),
            None if assume_yes => decisions.push(BugDecision {
                bug_id: bug.bug_id,
                title: bug.title.clone(),
                karma: 0,
                note: "not an update-request bug; skipped under --yes".to_string(),
            }),
            None => manual.push(bug),
        }
    }
    if !manual.is_empty() {
        // Show what the update says about itself before asking
        // the user to judge its bugs.
        print_update_context(&update);
        for bug in manual {
            let title = bug.title.as_deref().unwrap_or("<no title>");
            let bug_karma = prompt_bug_karma(bug.bug_id, title)?;
            decisions.push(BugDecision {
                bug_id: bug.bug_id,
                title: bug.title.clone(),
                karma: bug_karma,
                note: "manual".to_string(),
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
            full_analysis,
            changed_provides: vec![],
            installability_issues: if installability {
                vec![UnsatisfiedDep {
                    package: "fish".to_string(),
                    dep: "libfoo".to_string(),
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
    fn derive_karma_incomplete_analysis_neutral() {
        // No side tag / @testing: reverse deps listed only.
        assert_eq!(derive_karma(&report(false, false, false, false)).0, 0);
        // Stale side-tag data carried into the analysis.
        assert_eq!(derive_karma(&report(true, false, false, true)).0, 0);
    }

    #[test]
    fn auto_bug_karma_upvotes_exact_version() {
        let (karma, note) =
            auto_bug_karma("rust-quick-xml-0.40.1 is available", &builds()).unwrap();
        assert_eq!(karma, 1);
        assert!(note.contains("0.40.1"), "{note}");
    }

    #[test]
    fn auto_bug_karma_upvotes_newer_build() {
        // The update delivers more than the bug asked for.
        let (karma, _) = auto_bug_karma("fish-3.9.0 is available", &builds()).unwrap();
        assert_eq!(karma, 1);
    }

    #[test]
    fn auto_bug_karma_downvotes_version_mismatch() {
        let (karma, note) =
            auto_bug_karma("rust-quick-xml-0.41.0 is available", &builds()).unwrap();
        assert_eq!(karma, -1);
        assert!(note.contains('<'), "{note}");
    }

    #[test]
    fn auto_bug_karma_ignores_other_bugs() {
        assert!(auto_bug_karma("fish crashes on startup", &builds()).is_none());
        assert!(auto_bug_karma("CVE-2026-1234 fish: overflow", &builds()).is_none());
        // Update-request bug for a package not in this update.
        assert!(auto_bug_karma("zsh-5.9 is available", &builds()).is_none());
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
}
